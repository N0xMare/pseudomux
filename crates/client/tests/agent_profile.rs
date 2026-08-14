#![cfg(unix)]

//! Composition, fail-closed parsing, and the two safety checks the daemon does
//! not perform, for client-side agent profiles.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use pseudomux_client::agent_profile::{expand, load_agent_profile, verify_required_environment};
use pseudomux_protocol::v1::{
    AuthPolicy, CompatibilityPolicy, ConfigSource, EffortLevel, EnvironmentSpec, InputTransport,
    LifecycleMode, PermissionMode, RetentionPolicy, TerminalProfile,
};

const OWNER_ONLY: u32 = 0o600;
const WORLD_READABLE: u32 = 0o644;

fn error_of(text: &str, agent: &str) -> String {
    expand(text, OWNER_ONLY, "/profiles.json", agent)
        .expect_err("document was accepted")
        .to_string()
}

fn write_profile(directory: &Path, name: &str, text: &str, mode: u32) -> std::path::PathBuf {
    let path = directory.join(name);
    fs::write(&path, text).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
    path
}

const CHAIN: &str = r#"{
  "version": 1,
  "agents": {
    "base": {
      "claude": {
        "model": "sonnet",
        "effort": "high",
        "allowed_tools": ["Read", "Glob"],
        "plugin_dirs": ["/plugins/base"]
      },
      "terminal": { "rows": 48, "cols": 160 },
      "auth_policy": "subscription"
    },
    "middle": {
      "extends": "base",
      "claude": { "effort": "max", "allowed_tools": ["Bash(git:*)"] },
      "compatibility": "allow_untested"
    },
    "yolo": {
      "extends": "middle",
      "claude": {
        "permission_mode": "dangerously_skip_permissions",
        "allowed_tools": ["Write"],
        "plugin_dirs": ["/plugins/yolo"],
        "extra_args": ["--debug"]
      },
      "require_env": ["MY_TOKEN"]
    }
  }
}"#;

#[test]
fn extends_chain_replaces_scalars_and_appends_lists_parent_first() {
    let profile = expand(CHAIN, OWNER_ONLY, "/profiles.json", "yolo").unwrap();

    // Scalars: nearest definition in the chain wins, absent inherits.
    assert_eq!(profile.model.as_deref(), Some("sonnet"));
    assert_eq!(profile.effort, Some(EffortLevel::Max));
    assert_eq!(
        profile.permission_mode,
        Some(PermissionMode::DangerouslySkipPermissions)
    );
    assert_eq!(profile.rows, Some(48));
    assert_eq!(profile.cols, Some(160));
    assert_eq!(profile.auth_policy, Some(AuthPolicy::Subscription));
    assert_eq!(
        profile.compatibility,
        Some(CompatibilityPolicy::AllowUntested)
    );

    // Lists: appended in chain order, because argv repeats one flag per element.
    assert_eq!(
        profile.allowed_tools,
        ["Read", "Glob", "Bash(git:*)", "Write"]
    );
    assert_eq!(profile.plugin_dirs, ["/plugins/base", "/plugins/yolo"]);
    assert_eq!(profile.extra_args, ["--debug"]);
    assert_eq!(profile.require_env, ["MY_TOKEN"]);

    // A shorter request through the same document stops where it is asked to.
    let base = expand(CHAIN, OWNER_ONLY, "/profiles.json", "base").unwrap();
    assert_eq!(base.effort, Some(EffortLevel::High));
    assert_eq!(base.permission_mode, None);
    assert_eq!(base.allowed_tools, ["Read", "Glob"]);
}

#[test]
fn cyclic_extends_chains_are_named_and_rejected() {
    let error = error_of(
        r#"{"version":1,"agents":{
             "a":{"extends":"b"},
             "b":{"extends":"a"}}}"#,
        "a",
    );
    assert!(error.contains("cyclic"), "{error}");
    assert!(error.contains("a -> b -> a"), "{error}");

    let selfish = error_of(r#"{"version":1,"agents":{"a":{"extends":"a"}}}"#, "a");
    assert!(selfish.contains("cyclic"), "{selfish}");
}

#[test]
fn extends_depth_is_bounded_at_four_and_never_silently_truncated() {
    let four = r#"{"version":1,"agents":{
        "a":{"claude":{"model":"a"}},
        "b":{"extends":"a"},
        "c":{"extends":"b"},
        "d":{"extends":"c"}}}"#;
    assert_eq!(
        expand(four, OWNER_ONLY, "/profiles.json", "d")
            .unwrap()
            .model
            .as_deref(),
        Some("a")
    );

    let five = r#"{"version":1,"agents":{
        "a":{"claude":{"model":"a"}},
        "b":{"extends":"a"},
        "c":{"extends":"b"},
        "d":{"extends":"c"},
        "e":{"extends":"d"}}}"#;
    let error = error_of(five, "e");
    assert!(error.contains("deeper than 4"), "{error}");
    assert!(error.contains("e -> d -> c -> b -> a"), "{error}");
}

#[test]
fn a_literal_null_is_a_parse_error_rather_than_an_unset_operator() {
    let error = error_of(
        r#"{"version":1,"agents":{"a":{"claude":{"model":null}}}}"#,
        "a",
    );
    assert!(error.contains("is null"), "{error}");
    assert!(error.contains("no unset operator"), "{error}");
}

#[test]
fn inline_documents_keep_their_own_nulls() {
    let profile = expand(
        r#"{"version":1,"agents":{"a":{"claude":{"mcp_configs":[
             {"source":"inline","document":{"mcpServers":{"x":{"env":null}}}}]}}}}"#,
        OWNER_ONLY,
        "/profiles.json",
        "a",
    )
    .unwrap();
    assert_eq!(profile.mcp_configs.len(), 1);
}

#[test]
fn unknown_keys_are_rejected_at_every_level() {
    for (document, expected) in [
        (
            r#"{"version":1,"agents":{"a":{}},"defaults":{}}"#,
            "unknown document key `defaults`",
        ),
        (
            r#"{"version":1,"agents":{"a":{"claud":{}}}}"#,
            "unknown agent key `claud`",
        ),
        (
            r#"{"version":1,"agents":{"a":{"claude":{"modle":"x"}}}}"#,
            "unknown claude key `modle`",
        ),
        (
            r#"{"version":1,"agents":{"a":{"terminal":{"row":1}}}}"#,
            "unknown terminal key `row`",
        ),
    ] {
        let error = error_of(document, "a");
        assert!(error.contains(expected), "{error}");
    }
}

#[test]
fn per_invocation_fields_are_rejected_by_name() {
    for key in [
        "cwd",
        "session_id",
        "resume",
        "identity",
        "prompt",
        "deadline_unix_ms",
        "environment",
    ] {
        let document = format!(r#"{{"version":1,"agents":{{"a":{{"{key}":"x"}}}}}}"#);
        let error = error_of(&document, "a");
        assert!(
            error.contains(&format!("`{key}` is per-invocation")),
            "{error}"
        );
    }

    let executable = error_of(
        r#"{"version":1,"agents":{"a":{"claude":{"executable":"/bin/claude"}}}}"#,
        "a",
    );
    assert!(
        executable.contains("`executable` is per-invocation"),
        "{executable}"
    );
}

#[test]
fn reserved_but_unimplemented_values_fail_at_expansion_not_at_launch() {
    for (document, expected) in [
        (
            r#"{"version":1,"agents":{"a":{"terminal":{"profile":"rmux_standard"}}}}"#,
            "rmux_standard",
        ),
        (
            r#"{"version":1,"agents":{"a":{"terminal":{"input_transport":"attached_stream"}}}}"#,
            "attached_stream",
        ),
        (
            r#"{"version":1,"agents":{"a":{"retention":{"mode":"one_shot"}}}}"#,
            "one_shot",
        ),
    ] {
        let error = error_of(document, "a");
        assert!(error.contains(expected), "{error}");
        assert!(error.contains("reserved"), "{error}");
    }
}

#[test]
fn supported_tagged_policies_deserialize_into_the_protocol_types() {
    let profile = expand(
        r#"{"version":1,"agents":{"a":{
             "lifecycle":{"mode":"hybrid","hook_timeout_ms":7000},
             "retention":{"mode":"persistent","idle_ttl_ms":60000},
             "terminal":{"input_transport":"sdk"},
             "claude":{"system_prompt":{"mode":"append","prompt":"stay concise"}}}}}"#,
        OWNER_ONLY,
        "/profiles.json",
        "a",
    )
    .unwrap();
    assert_eq!(
        profile.lifecycle,
        Some(LifecycleMode::Hybrid {
            hook_timeout_ms: 7_000
        })
    );
    assert_eq!(
        profile.retention,
        Some(RetentionPolicy::Persistent {
            idle_ttl_ms: 60_000
        })
    );
    assert_eq!(profile.input_transport, Some(InputTransport::Sdk));
    assert!(profile.system_prompt.is_some());
}

#[test]
fn duplicate_agent_names_are_rejected_instead_of_last_one_winning() {
    let error = error_of(
        r#"{"version":1,"agents":{
             "yolo":{"claude":{"model":"opus"}},
             "yolo":{"claude":{"model":"haiku"}}}}"#,
        "yolo",
    );
    assert!(error.contains("duplicate key \"yolo\""), "{error}");
}

#[test]
fn a_document_carrying_an_inline_secret_must_be_owner_only() {
    let directory = tempfile::tempdir().unwrap();
    let document = r#"{"version":1,"agents":{"a":{"claude":{"mcp_configs":[
        {"source":"inline","document":{"token":"inline-secret"}}]}}}}"#;

    let exposed = write_profile(directory.path(), "world.json", document, WORLD_READABLE);
    let error = load_agent_profile(&exposed, "a").unwrap_err().to_string();
    assert!(error.contains("inline settings or MCP document"), "{error}");
    assert!(error.contains("owner-only"), "{error}");
    assert!(!error.contains("inline-secret"), "{error}");

    let private = write_profile(directory.path(), "private.json", document, OWNER_ONLY);
    assert_eq!(
        load_agent_profile(&private, "a").unwrap().mcp_configs.len(),
        1
    );
}

#[test]
fn referenced_config_files_must_be_owner_only_and_absolute() {
    let directory = tempfile::tempdir().unwrap();
    let mcp = write_profile(directory.path(), "mcp.json", "{}", WORLD_READABLE);
    let document = format!(
        r#"{{"version":1,"agents":{{"a":{{"claude":{{"mcp_configs":[
             {{"source":"file","path":"{}"}}]}}}}}}}}"#,
        mcp.display()
    );
    let error = error_of(&document, "a");
    assert!(error.contains("must be owner-only"), "{error}");

    fs::set_permissions(&mcp, fs::Permissions::from_mode(OWNER_ONLY)).unwrap();
    let profile = expand(&document, OWNER_ONLY, "/profiles.json", "a").unwrap();
    assert_eq!(
        profile.mcp_configs,
        [ConfigSource::File {
            path: mcp.display().to_string()
        }]
    );

    let relative = error_of(
        r#"{"version":1,"agents":{"a":{"claude":{"settings":[
             {"source":"file","path":"relative/settings.json"}]}}}}"#,
        "a",
    );
    assert!(relative.contains("must be absolute"), "{relative}");

    let plugin = error_of(
        r#"{"version":1,"agents":{"a":{"claude":{"plugin_dirs":["plugins/x"]}}}}"#,
        "a",
    );
    assert!(plugin.contains("must be absolute"), "{plugin}");
}

#[test]
fn profile_paths_are_explicit_and_documents_are_versioned() {
    let relative = load_agent_profile(Path::new("agents.json"), "a")
        .unwrap_err()
        .to_string();
    assert!(relative.contains("must be absolute"), "{relative}");

    let missing_version = error_of(r#"{"agents":{"a":{}}}"#, "a");
    assert!(missing_version.contains("missing required `version`"));

    let wrong_version = error_of(r#"{"version":2,"agents":{"a":{}}}"#, "a");
    assert!(wrong_version.contains("unsupported profile version 2"));

    let undefined = error_of(r#"{"version":1,"agents":{"a":{}}}"#, "b");
    assert!(
        undefined.contains("agent `b` is not defined"),
        "{undefined}"
    );
    assert!(undefined.contains("known agents: a"), "{undefined}");
}

fn environment(pairs: &[(&str, &str)]) -> EnvironmentSpec {
    EnvironmentSpec {
        snapshot: pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect(),
        ..EnvironmentSpec::default()
    }
}

#[test]
fn require_env_asserts_presence_without_ever_reading_the_value() {
    let names = vec!["MY_TOKEN".to_owned()];

    let missing = verify_required_environment(
        &names,
        &environment(&[]),
        AuthPolicy::Subscription,
        TerminalProfile::Transparent,
    )
    .unwrap_err()
    .to_string();
    assert!(missing.contains("MY_TOKEN is not set"), "{missing}");

    let empty = verify_required_environment(
        &names,
        &environment(&[("MY_TOKEN", "")]),
        AuthPolicy::Subscription,
        TerminalProfile::Transparent,
    )
    .unwrap_err()
    .to_string();
    assert!(empty.contains("set but empty"), "{empty}");

    // Present and non-empty: the presence check passes. `MY_TOKEN` is not on the
    // launch allowlist, so this warns — and the warning must not carry the value
    // it just proved was there, which is the whole point of this test.
    let warnings = verify_required_environment(
        &names,
        &environment(&[("MY_TOKEN", "super-secret-value")]),
        AuthPolicy::Subscription,
        TerminalProfile::Transparent,
    )
    .unwrap();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(!warnings[0].contains("super-secret-value"), "{warnings:?}");

    // An allowlisted name is silent, so the warning above is a signal and not
    // background noise on every launch.
    let allowlisted = verify_required_environment(
        &["CLAUDE_CONFIG_DIR".to_owned()],
        &environment(&[("CLAUDE_CONFIG_DIR", "/home/user/.claude")]),
        AuthPolicy::Subscription,
        TerminalProfile::Transparent,
    )
    .unwrap();
    assert!(allowlisted.is_empty(), "{allowlisted:?}");
}

/// BLOCKER 1: the guard the product ships for exactly this failure was blind to
/// the allowlist, which is now the dominant reason a `require_env` name never
/// reaches the child. `GITHUB_TOKEN` is the reviewer's own example.
#[test]
fn require_env_warns_when_the_inheritance_allowlist_would_drop_the_name() {
    let warnings = verify_required_environment(
        &["GITHUB_TOKEN".to_owned()],
        &environment(&[("GITHUB_TOKEN", "ghp_secret")]),
        AuthPolicy::Inherit,
        TerminalProfile::Transparent,
    )
    .unwrap();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("GITHUB_TOKEN"), "{warnings:?}");
    assert!(
        warnings[0].contains("allowlist does not admit it"),
        "{warnings:?}"
    );
    // The escape hatch must be named, and named as the fix.
    assert!(
        warnings[0].contains("--env-passthrough GITHUB_TOKEN"),
        "{warnings:?}"
    );
    assert!(!warnings[0].contains("ghp_secret"), "{warnings:?}");

    // Neither denylist applies, so neither is claimed as a reason.
    assert!(
        !warnings[0].contains("auth_policy=subscription"),
        "{warnings:?}"
    );
    assert!(
        !warnings[0].contains("terminal profile=transparent"),
        "{warnings:?}"
    );
}

/// The allowlist filters the inherited snapshot only. `--env-passthrough` lands
/// the name in `EnvironmentSpec::set`, which bypasses it — so once the caller
/// has used the escape hatch the warning must stop, or it trains them to ignore
/// it.
#[test]
fn a_name_delivered_through_the_explicit_set_is_not_reported_as_allowlist_dropped() {
    let spec = EnvironmentSpec {
        set: [("GITHUB_TOKEN".to_owned(), "ghp_secret".to_owned())]
            .into_iter()
            .collect(),
        ..EnvironmentSpec::default()
    };
    let warnings = verify_required_environment(
        &["GITHUB_TOKEN".to_owned()],
        &spec,
        AuthPolicy::Inherit,
        TerminalProfile::Transparent,
    )
    .unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");

    // But `set` does not defeat the denylists: they run *after* it. The remedy
    // sentence must therefore say the opposite of the allowlist-only case, or it
    // sends the caller back to a flag that cannot help them.
    let spec = EnvironmentSpec {
        set: [("ANTHROPIC_API_KEY".to_owned(), "sk-secret".to_owned())]
            .into_iter()
            .collect(),
        ..EnvironmentSpec::default()
    };
    let still_stripped = verify_required_environment(
        &["ANTHROPIC_API_KEY".to_owned()],
        &spec,
        AuthPolicy::Subscription,
        TerminalProfile::Transparent,
    )
    .unwrap();
    assert_eq!(still_stripped.len(), 1, "{still_stripped:?}");
    assert!(
        still_stripped[0].contains("auth_policy=subscription removes it"),
        "{still_stripped:?}"
    );
    assert!(
        !still_stripped[0].contains("allowlist does not admit it"),
        "an explicitly-set name is not dropped by the allowlist: {still_stripped:?}"
    );
    assert!(
        still_stripped[0].contains("cannot restore it"),
        "{still_stripped:?}"
    );
    assert!(
        !still_stripped[0].contains("sk-secret"),
        "{still_stripped:?}"
    );
}

/// The infrastructure names HIGH 5 and HIGH 6 added, asserted through the same
/// mirror the warning uses. A regression here means the client would start
/// warning about names the daemon does deliver.
#[test]
fn newly_admitted_tls_and_git_infrastructure_names_warn_about_nothing() {
    for name in [
        "NIX_SSL_CERT_FILE",
        "GIT_SSH_COMMAND",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_SYSTEM",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_KEY_0",
        "GIT_CONFIG_VALUE_0",
    ] {
        let warnings = verify_required_environment(
            &[name.to_owned()],
            &environment(&[(name, "value")]),
            AuthPolicy::Subscription,
            TerminalProfile::Transparent,
        )
        .unwrap();
        assert!(warnings.is_empty(), "{name} warned: {warnings:?}");
    }

    // The blanket `GIT_` prefix was rejected on purpose: these redirect the
    // Bash tool at a repository other than `cwd`, so they stay denied and the
    // caller is told so.
    for name in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"] {
        let warnings = verify_required_environment(
            &[name.to_owned()],
            &environment(&[(name, "/some/other/repo")]),
            AuthPolicy::Subscription,
            TerminalProfile::Transparent,
        )
        .unwrap();
        assert_eq!(warnings.len(), 1, "{name} was silently admitted");
        assert!(
            warnings[0].contains("allowlist does not admit it"),
            "{warnings:?}"
        );
    }
}

/// HIGH 7: `VERTEX_` was narrowed to `VERTEX_REGION_`, and the provider
/// namespaces are `Inherit`-only in both directions.
#[test]
fn provider_routing_is_inherit_only_and_the_vertex_namespace_is_narrowed() {
    let inherit_admits = |name: &str| {
        verify_required_environment(
            &[name.to_owned()],
            &environment(&[(name, "value")]),
            AuthPolicy::Inherit,
            TerminalProfile::Transparent,
        )
        .unwrap()
        .is_empty()
    };

    assert!(inherit_admits("VERTEX_REGION_CLAUDE_3_5_HAIKU"));
    assert!(inherit_admits("AWS_PROFILE"));
    assert!(inherit_admits("ANTHROPIC_CUSTOM_HEADERS"));
    assert!(inherit_admits("CLOUD_ML_REGION"));
    assert!(
        !inherit_admits("VERTEX_SOMETHING_ELSE"),
        "the bare VERTEX_ namespace is no longer open"
    );

    // Under the default policy the same names are denied at the allowlist, and
    // a credential name is denied twice — once by each mechanism.
    let subscription = verify_required_environment(
        &["AWS_PROFILE".to_owned()],
        &environment(&[("AWS_PROFILE", "default")]),
        AuthPolicy::Subscription,
        TerminalProfile::Transparent,
    )
    .unwrap();
    assert_eq!(subscription.len(), 1, "{subscription:?}");
    assert!(
        subscription[0].contains("allowlist does not admit it"),
        "{subscription:?}"
    );
}

#[test]
fn require_env_warns_when_the_resolved_policies_would_strip_the_name() {
    let subscription = verify_required_environment(
        &["ANTHROPIC_API_KEY".to_owned()],
        &environment(&[("ANTHROPIC_API_KEY", "secret")]),
        AuthPolicy::Subscription,
        TerminalProfile::Transparent,
    )
    .unwrap();
    assert_eq!(subscription.len(), 1);
    assert!(
        subscription[0].contains("auth_policy=subscription"),
        "{subscription:?}"
    );
    // A name can fail for both reasons at once, and the message says which is
    // which: under `subscription` the credential names are outside the
    // allowlist *and* stripped by the policy pass. One warning, two reasons.
    assert!(
        subscription[0].contains("allowlist does not admit it"),
        "{subscription:?}"
    );
    assert!(subscription[0].contains(" and "), "{subscription:?}");
    assert!(!subscription[0].contains("secret"), "{subscription:?}");

    // The MCP-token hazard the warning exists for: a name that matches a
    // TRANSPARENT_PREFIXES entry vanishes without ever being mentioned.
    let transparent = verify_required_environment(
        &["CLAUDE_CODE_SDK_TOKEN".to_owned()],
        &environment(&[("CLAUDE_CODE_SDK_TOKEN", "secret")]),
        AuthPolicy::Inherit,
        TerminalProfile::Transparent,
    )
    .unwrap();
    assert_eq!(transparent.len(), 1);
    assert!(
        transparent[0].contains("terminal profile=transparent"),
        "{transparent:?}"
    );
    // The denylist reason survives the allowlist addition, and because a
    // denylist pass applies the escape hatch is *not* offered as the fix.
    assert!(
        transparent[0].contains("cannot restore it"),
        "{transparent:?}"
    );

    // Under inherit auth the same name survives, so no warning is emitted.
    let quiet = verify_required_environment(
        &["ANTHROPIC_API_KEY".to_owned()],
        &environment(&[("ANTHROPIC_API_KEY", "secret")]),
        AuthPolicy::Inherit,
        TerminalProfile::Transparent,
    )
    .unwrap();
    assert!(quiet.is_empty(), "{quiet:?}");
}

#[test]
fn require_env_follows_the_effective_environment_patch_order() {
    let spec = EnvironmentSpec {
        snapshot: [("MY_TOKEN".to_owned(), "from-snapshot".to_owned())]
            .into_iter()
            .collect(),
        unset: ["MY_TOKEN".to_owned()].into_iter().collect(),
        ..EnvironmentSpec::default()
    };
    let error = verify_required_environment(
        &["MY_TOKEN".to_owned()],
        &spec,
        AuthPolicy::Inherit,
        TerminalProfile::Transparent,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("is not set"), "{error}");
}
