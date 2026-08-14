//! The agent resource, held to the invariant it exists to enforce.
//!
//! > An agent may narrow what a session may name. It may never name a resource
//! > on the session's behalf.
//!
//! Every test here is one instance of that sentence, and every list any of them
//! walks is derived from a type rather than written out, because a hand-written
//! set of things-to-check is the defect this repository has now found
//! thirty-three times. This counter said twenty-eight while `v1.rs` said
//! twenty-nine and `current-state.md` §9.25 was already written: a count
//! restated in three files is a count that is wrong in at least one of them,
//! which is the same defect the sentence above is about.
//!
//! And it stayed hand-kept for three more instances after saying that, so it
//! went wrong again: the brief for instance thirty-two asserted all of these
//! sites read "thirty-two" while every one read "thirty-one".
//! `test_every_statement_of_the_bug_class_counter_spells_the_same_ordinal` in
//! `tools/gate-a/tests/test_run_gate.py` is the derivation the sentence above
//! should have carried from the start: it reads the ordinal out of the last
//! `THE BUG CLASS, instance …` heading in `docs/current-state.md` and requires
//! every statement of it in `crates/` and `bin/` to spell that same word.
//!
//! DELIBERATELY NOT A RUST TEST, and the reason is this file. Every test target
//! of `pseudomux-service` runs once per mutant inside
//! `scripts/gate-a-mutants.sh`, in a COPY of the tree that `cargo-mutants`
//! makes; whether that copy carries `docs/` is a claim about a tool nobody
//! here had checked, and a wrong guess costs the 88-minute
//! `gate_b/mutation_score_agent_launch_pool_protocol` cell an aborted baseline.
//! `tools/gate-a/tests` never runs under mutation, already scans this whole
//! repository for a second defect of exactly this shape
//! (`test_every_reader_of_the_workspace_tool_root_derives_the_same_path`), and
//! reaches the Markdown as easily as the Rust.

#![cfg(unix)]

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use pseudomux_protocol::v1::{
    AgentContainment, AgentEnvironmentSpec, AgentRef, AgentSpec, AgentVersion, AuthPolicy,
    ClaudeLaunchConfig, CompatibilityPolicy, ConfigIsolation, ConfigSource, EnvironmentSpec,
    ErrorCode, InputTransport, LifecycleMode, RetentionPolicy, SessionCell, SessionIdentity,
    StartSessionRequest, SystemPromptPolicy, TerminalProfile, TerminalSpec,
    agent_supplied_start_paths,
};
use pseudomux_service::agent::{
    AgentStore, admit_agent_containment, config_digest, redact_agent_spec, resolve_agent_start,
    validate_agent_spec,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

const NOW: u64 = 1_700_000_000_000;

fn claude() -> ClaudeLaunchConfig {
    ClaudeLaunchConfig {
        executable: "/bin/sh".into(),
        model: Some("claude-sonnet-5".into()),
        effort: None,
        permission_mode: None,
        allowed_tools: vec!["Read".into()],
        denied_tools: Vec::new(),
        settings: Vec::new(),
        mcp_configs: Vec::new(),
        plugin_dirs: Vec::new(),
        system_prompt: SystemPromptPolicy::Append {
            prompt: "Be exact.".into(),
        },
        extra_args: Vec::new(),
    }
}

/// One admissible agent, with every field set to something distinguishable.
///
/// The struct literal has no `..Default::default()`, so a field added to
/// `AgentSpec` is a compile error here and has to be given a value that the
/// tests below then carry through resolution.
fn spec() -> AgentSpec {
    AgentSpec {
        name: "reviewer".into(),
        description: Some("reads and reports".into()),
        claude: claude(),
        environment: AgentEnvironmentSpec {
            set: BTreeMap::from([("REVIEW_MODE".into(), "strict".into())]),
            unset: BTreeSet::from(["ANTHROPIC_API_KEY".into()]),
        },
        auth_policy: AuthPolicy::Inherit,
        terminal: TerminalSpec {
            rows: 40,
            cols: 132,
            profile: TerminalProfile::Transparent,
            input_transport: InputTransport::Sdk,
        },
        lifecycle: LifecycleMode::Hybrid {
            hook_timeout_ms: 5_000,
        },
        retention: RetentionPolicy::Persistent {
            idle_ttl_ms: 900_000,
        },
        compatibility: CompatibilityPolicy::AllowUntested,
        cell: SessionCell::Full,
        containment: AgentContainment::default(),
    }
}

fn agent_start(agent_id: Uuid, version: AgentVersion, cwd: &str) -> StartSessionRequest {
    StartSessionRequest {
        identity: SessionIdentity::New { session_id: None },
        cwd: cwd.into(),
        claude: None,
        agent: Some(AgentRef { agent_id, version }),
        environment: EnvironmentSpec {
            snapshot: BTreeMap::from([("PATH".into(), "/usr/bin".into())]),
            set: BTreeMap::new(),
            unset: BTreeSet::new(),
        },
        auth_policy: AuthPolicy::default(),
        config_isolation: None,
        terminal: TerminalSpec::default(),
        lifecycle: LifecycleMode::default(),
        retention: RetentionPolicy::default(),
        compatibility: CompatibilityPolicy::default(),
        cell: SessionCell::default(),
    }
}

fn store(root: &Path) -> AgentStore {
    AgentStore::open(root).expect("a store pmux creates is a store pmux may use")
}

/// Resolution produces EXACTLY the DTO a caller would have typed inline.
///
/// This is the whole argv-purity argument, as an equality rather than a
/// description: everything downstream of this line receives a request it cannot
/// distinguish from an inline one, which is what keeps `docs/spec.md` Sec. 4.4
/// literally true. Compared as `serde_json::Value` so it is the WIRE forms that
/// are equal and not merely two Rust values that happen to `PartialEq`.
#[test]
fn agent_resolution_is_a_pure_function_of_the_spec_and_the_session_fields() {
    let spec = spec();
    let agent_id = Uuid::from_u128(7);
    let reference = AgentRef {
        agent_id,
        version: AgentVersion::FIRST,
    };
    let digest = config_digest(&spec).expect("digest");

    let (resolved, pin) = resolve_agent_start(
        &spec,
        &digest,
        reference,
        agent_start(agent_id, AgentVersion::FIRST, "/work/project"),
    );

    let inline = StartSessionRequest {
        identity: SessionIdentity::New { session_id: None },
        cwd: "/work/project".into(),
        claude: Some(claude()),
        agent: None,
        environment: EnvironmentSpec {
            snapshot: BTreeMap::from([("PATH".into(), "/usr/bin".into())]),
            set: BTreeMap::from([("REVIEW_MODE".into(), "strict".into())]),
            unset: BTreeSet::from(["ANTHROPIC_API_KEY".into()]),
        },
        auth_policy: AuthPolicy::Inherit,
        config_isolation: None,
        terminal: TerminalSpec {
            rows: 40,
            cols: 132,
            profile: TerminalProfile::Transparent,
            input_transport: InputTransport::Sdk,
        },
        lifecycle: LifecycleMode::Hybrid {
            hook_timeout_ms: 5_000,
        },
        retention: RetentionPolicy::Persistent {
            idle_ttl_ms: 900_000,
        },
        compatibility: CompatibilityPolicy::AllowUntested,
        cell: SessionCell::Full,
    };
    assert_eq!(
        serde_json::to_value(&resolved).expect("resolved serializes"),
        serde_json::to_value(&inline).expect("inline serializes"),
        "a resolved start must be indistinguishable from one a caller typed"
    );
    assert!(
        resolved.agent.is_none(),
        "the resolved DTO is not `a start that named an agent`; it is the start that agent MEANS"
    );
    assert_eq!(pin.config_digest, digest);
    assert_eq!(pin.version, AgentVersion::FIRST);

    // ...and it is a FUNCTION: two calls with the same inputs agree, and the
    // only thing that varies with the caller is what the caller supplied.
    let (again, _) = resolve_agent_start(
        &spec,
        &digest,
        reference,
        agent_start(agent_id, AgentVersion::FIRST, "/work/project"),
    );
    assert_eq!(again, resolved);
    let (elsewhere, _) = resolve_agent_start(
        &spec,
        &digest,
        reference,
        agent_start(agent_id, AgentVersion::FIRST, "/work/other"),
    );
    assert_eq!(elsewhere.cwd, "/work/other");
    assert_eq!(elsewhere.claude, resolved.claude);
}

/// A stored version is IMMUTABLE, byte for byte, across any number of updates.
#[test]
fn an_update_mints_a_new_version_and_never_rewrites_an_old_one() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("agents");
    let store = store(&root);

    let first = store.create(spec(), NOW).expect("create");
    let file = root
        .join(first.agent_id.hyphenated().to_string())
        .join("1.json");
    let before = std::fs::read(&file).expect("version 1 exists");

    let mut second_spec = spec();
    second_spec.claude.model = Some("claude-opus-5".into());
    let second = store
        .update(first.agent_id, first.version, second_spec, NOW + 1_000)
        .expect("update");
    assert_eq!(second.version.get(), 2);
    assert_ne!(second.config_digest, first.config_digest);
    assert_eq!(
        second.created_at_ms, first.created_at_ms,
        "an update mints a version, it does not create a new agent"
    );

    assert_eq!(
        std::fs::read(&file).expect("version 1 still exists"),
        before,
        "a version a caller pinned is never rewritten"
    );
    let read_back = store
        .get(first.agent_id, Some(AgentVersion::FIRST))
        .expect("v1 is still readable");
    assert_eq!(read_back, first);
}

/// The update fence is REQUIRED and never soft.
#[test]
fn a_stale_update_fence_is_a_conflict_and_is_never_answered_as_already_applied() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = store(&temp.path().join("agents"));
    let first = store.create(spec(), NOW).expect("create");
    store
        .update(first.agent_id, first.version, spec(), NOW + 1)
        .expect("the first update is on the head");

    // Stale by exactly one revision.
    let error = store
        .update(first.agent_id, AgentVersion::FIRST, spec(), NOW + 2)
        .expect_err("a fence one behind is still a stale fence");
    assert_eq!(error.code, ErrorCode::IdConflict);
    assert!(error.message.contains("version 2"), "{}", error.message);

    // ...INCLUDING when the submitted spec is byte-identical to the head. An
    // arm that answered "your update already landed" here would be answering a
    // caller whose view of the agent is a revision behind, which is exactly the
    // shape `ClearSessionResult` retired for the same reason.
    let head = store.get(first.agent_id, None).expect("head");
    assert_eq!(head.version.get(), 2);
    let error = store
        .update(first.agent_id, AgentVersion::FIRST, spec(), NOW + 3)
        .expect_err("an identical spec on a stale fence is still a conflict");
    assert_eq!(error.code, ErrorCode::IdConflict);
    assert!(
        !error.message.to_lowercase().contains("already"),
        "no update is ever answered as already applied: {}",
        error.message
    );
}

/// Every level and every file the store creates is owner-only FROM BIRTH.
#[test]
fn the_store_is_private_at_every_level_and_in_every_file_it_creates() {
    let temp = tempfile::tempdir().expect("tempdir");
    // Two missing levels, so the assertion is about the TREE and not only about
    // the leaf: `create_dir_all` + one `chmod` seals only the last component,
    // which is the exact defect `private_dir` exists for.
    let root = temp.path().join("deep/nested/agents");
    let store = store(&root);
    let descriptor = store.create(spec(), NOW).expect("create");

    // Walked from the filesystem rather than from a list written here.
    let mut checked = 0;
    for directory in [
        temp.path().join("deep"),
        temp.path().join("deep/nested"),
        root.clone(),
        root.join(descriptor.agent_id.hyphenated().to_string()),
    ] {
        let mode = std::fs::metadata(&directory)
            .expect("directory")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "{} is not owner-only", directory.display());
        checked += 1;
    }
    assert_eq!(checked, 4);

    let agent_dir = root.join(descriptor.agent_id.hyphenated().to_string());
    let mut files = 0;
    for entry in std::fs::read_dir(&agent_dir).expect("agent directory") {
        let path = entry.expect("entry").path();
        let mode = std::fs::metadata(&path).expect("file").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{} is not owner-only", path.display());
        files += 1;
    }
    assert_eq!(files, 2, "one version file and one head file");
}

/// A widened store is REFUSED at boot, and is never re-permissioned.
#[test]
fn a_widened_store_is_refused_and_is_left_exactly_as_the_operator_left_it() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("agents");
    std::fs::create_dir(&root).expect("operator directory");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).expect("mode");

    let error = AgentStore::open(&root).expect_err("a store readable by others is refused");
    assert_eq!(error.code, ErrorCode::InvalidConfig);
    assert!(error.message.contains("owner-only"), "{}", error.message);
    // The refusal names what would be RIGHT, not only what is wrong.
    assert!(
        error.details["recommendation"]
            .as_str()
            .is_some_and(|text| text.contains("chmod 700")),
        "the refusal must name the fix: {}",
        error.details
    );
    assert_eq!(
        std::fs::metadata(&root)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777,
        0o755,
        "pmuxd never re-permissions a directory it did not create"
    );
}

/// A file widened AFTER boot is refused AT READ TIME.
#[test]
fn a_widened_agent_file_is_refused_when_it_is_read_and_not_only_at_boot() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("agents");
    let store = store(&root);
    let descriptor = store.create(spec(), NOW).expect("create");
    let file = root
        .join(descriptor.agent_id.hyphenated().to_string())
        .join("1.json");

    // The store was admissible at boot; the file is widened afterwards, which
    // is the whole point of checking on every read.
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).expect("widen");
    let error = store
        .load_for_launch(AgentRef {
            agent_id: descriptor.agent_id,
            version: AgentVersion::FIRST,
        })
        .expect_err("a file readable by others holds values pmux must not trust");
    assert_eq!(error.code, ErrorCode::InvalidConfig);
    assert!(error.message.contains("644"), "{}", error.message);
    assert_eq!(
        std::fs::metadata(&file)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777,
        0o644,
        "refuse, never re-permission"
    );
}

/// The path component is ALWAYS a canonical UUID, and `name` never reaches one.
///
/// `agent_profile::validate_agent_name` admits `..`, `.` and `a..b`; it is a
/// perfectly good validator for a JSON map key and a directory traversal the
/// moment the name becomes a path component. Minting a UUID makes the traversal
/// unconstructible rather than filtered.
#[test]
fn a_stored_agent_is_named_by_a_minted_uuid_and_never_by_its_label() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("agents");
    let store = store(&root);

    for hostile in ["..", ".", "...", "a..b", "-", "../../etc"] {
        let mut spec = spec();
        spec.name = hostile.to_owned();
        let descriptor = store.create(spec, NOW).expect("a label is just a label");
        let entries: Vec<String> = std::fs::read_dir(&root)
            .expect("store")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        for entry in &entries {
            assert_eq!(
                Uuid::parse_str(entry)
                    .expect("every store entry is a UUID")
                    .hyphenated()
                    .to_string(),
                *entry,
                "{entry} is not a canonical hyphenated UUID"
            );
        }
        assert!(
            entries.contains(&descriptor.agent_id.hyphenated().to_string()),
            "the minted id must be the directory name"
        );
        assert!(
            !entries.iter().any(|entry| entry == hostile),
            "the label {hostile:?} reached the filesystem"
        );
    }
}

/// Containment NARROWS, and can never admit what the existing rules refuse.
///
/// The direction is the whole rule, so it is asserted in both: an inside cwd is
/// admitted, and every outside one is refused -- INCLUDING one that CONTAINS
/// the root, which the repository's symmetric containment predicate would have
/// admitted and which this field's own documentation promises it does not.
#[test]
fn containment_bounds_a_cwd_and_never_supplies_or_widens_one() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let inside = workspace.join("project");
    let outside = temp.path().join("elsewhere");
    for directory in [&workspace, &inside, &outside] {
        std::fs::create_dir_all(directory).expect("directory");
    }
    let agent_id = Uuid::from_u128(9);
    let containment = AgentContainment {
        workspace_root: Some(workspace.to_string_lossy().into_owned()),
        require_config_isolation: false,
    };

    admit_agent_containment(&containment, agent_id, &inside, None)
        .expect("a cwd inside the bound is admitted");
    admit_agent_containment(&containment, agent_id, &workspace, None)
        .expect("the root itself is inside the bound");

    for refused in [outside.as_path(), temp.path()] {
        let error = admit_agent_containment(&containment, agent_id, refused, None)
            .expect_err("a cwd outside the bound is refused");
        assert_eq!(error.code, ErrorCode::InvalidConfig);
        assert!(
            error.message.contains("does not resolve inside"),
            "{}",
            error.message
        );
    }
    // `temp.path()` is the parent of `workspace`, so it CONTAINS the root. The
    // symmetric predicate `one_directory_contains_the_other` answers `true` for
    // that pair; this rule must not, or the field's promise is false.

    // ...and a symlink into the bound is admitted, because containment is
    // decided on the resource rather than on the spelling.
    let link = temp.path().join("link-to-project");
    std::os::unix::fs::symlink(&inside, &link).expect("symlink");
    admit_agent_containment(&containment, agent_id, &link, None)
        .expect("an alias of a contained directory is the same directory");

    // ...and one that only TEXTUALLY prefixes is not.
    let sibling = temp.path().join("workspace-other");
    std::fs::create_dir_all(&sibling).expect("sibling");
    admit_agent_containment(&containment, agent_id, &sibling, None)
        .expect_err("a name prefix is not containment");
}

/// `require_config_isolation` refuses a start that names no root, and NEVER
/// names one itself.
#[test]
fn require_config_isolation_refuses_a_start_and_never_supplies_a_root() {
    let agent_id = Uuid::from_u128(11);
    let containment = AgentContainment {
        workspace_root: None,
        require_config_isolation: true,
    };
    let error = admit_agent_containment(&containment, agent_id, Path::new("/work"), None)
        .expect_err("the agent requires a root and this start named none");
    assert_eq!(error.code, ErrorCode::InvalidConfig);
    assert!(
        error.details["recommendation"]
            .as_str()
            .is_some_and(|text| text.contains("--config-isolation-root")),
        "the refusal must name the flag: {}",
        error.details
    );

    let isolation = ConfigIsolation {
        root: "/private/root".into(),
    };
    admit_agent_containment(&containment, agent_id, Path::new("/work"), Some(&isolation))
        .expect("a start that names its own root satisfies the requirement");

    // The agent itself has no field that could carry one: the only way to check
    // this is that `AgentContainment` is exhaustively destructured by
    // `admit_agent_containment`, and that `AgentSpec` has no `config_isolation`
    // leaf at all.
    let serialized = serde_json::to_value(spec()).expect("spec serializes");
    assert!(
        serialized.get("config_isolation").is_none(),
        "an agent must have no field through which it could name a configuration root"
    );
    assert!(
        serialized.get("cwd").is_none() && serialized.get("identity").is_none(),
        "an agent must have no field through which it could name a session's resources"
    );
    assert!(
        serialized["environment"].get("snapshot").is_none(),
        "an agent must have no field through which it could store a caller snapshot"
    );
}

/// `require_config_isolation: false` beside `cell: minified` is REFUSED, not
/// silently overridden.
#[test]
fn a_minified_agent_that_does_not_require_its_own_root_is_refused_at_create() {
    let mut spec = spec();
    spec.cell = SessionCell::Minified;
    spec.containment.require_config_isolation = false;
    let error = validate_agent_spec(&spec).expect_err("a value the agent could never honour");
    assert_eq!(error.code, ErrorCode::InvalidConfig);
    assert!(
        error.message.contains("require_config_isolation"),
        "{}",
        error.message
    );

    spec.containment.require_config_isolation = true;
    validate_agent_spec(&spec).expect("the pair the minified cell actually needs");
}

/// An agent may not NAME a configuration root through the one environment
/// channel the launch allowlist does not filter.
#[test]
fn an_agent_may_not_set_a_variable_that_moves_the_childs_configuration_root() {
    // WALKED OVER THE SERVICE'S OWN TABLE, so a door added to
    // `CONFIG_ROOT_ENV_DOORS` is covered here with no second edit.
    let doors = pseudomux_service::claude_launch::CONFIG_ROOT_ENV_DOORS;
    assert!(!doors.is_empty());
    for door in doors {
        let mut spec = spec();
        spec.environment
            .set
            .insert((*door).to_owned(), "/tmp/anywhere".into());
        let error =
            validate_agent_spec(&spec).expect_err("a door an agent may not open must be refused");
        assert_eq!(error.code, ErrorCode::InvalidConfig);
        assert!(error.message.contains(door), "{}", error.message);
        assert!(
            error.details["recommendation"]
                .as_str()
                .is_some_and(|text| text.contains("require_config_isolation")),
            "the refusal must name what an agent MAY do instead: {}",
            error.details
        );
    }
}

/// A stored agent is one that can START.
///
/// The checks are the service's own, called rather than restated, so a spec
/// that every `start_session` would refuse is refused where the caller can
/// still fix it instead of at a launch that never happens.
#[test]
fn a_spec_a_start_would_refuse_is_refused_at_create() {
    for (label, mutate) in [
        (
            "one_shot retention",
            Box::new(|spec: &mut AgentSpec| spec.retention = RetentionPolicy::OneShot)
                as Box<dyn Fn(&mut AgentSpec)>,
        ),
        (
            "the reserved rmux-standard terminal identity",
            Box::new(|spec: &mut AgentSpec| spec.terminal.profile = TerminalProfile::RmuxStandard),
        ),
        (
            "the reserved attached-stream transport",
            Box::new(|spec: &mut AgentSpec| {
                spec.terminal.input_transport = InputTransport::AttachedStream;
            }),
        ),
        (
            "a relative Claude executable",
            Box::new(|spec: &mut AgentSpec| spec.claude.executable = "claude".into()),
        ),
        (
            "a driver-owned flag",
            Box::new(|spec: &mut AgentSpec| spec.claude.extra_args = vec!["--resume".into()]),
        ),
        (
            "a relative workspace root",
            Box::new(|spec: &mut AgentSpec| {
                spec.containment.workspace_root = Some("relative/path".into());
            }),
        ),
        (
            "a workspace root spelled with a parent component",
            Box::new(|spec: &mut AgentSpec| {
                spec.containment.workspace_root = Some("/work/../etc".into());
            }),
        ),
        (
            "an empty name",
            Box::new(|spec: &mut AgentSpec| spec.name = "   ".into()),
        ),
    ] {
        let mut spec = spec();
        mutate(&mut spec);
        assert!(
            validate_agent_spec(&spec).is_err(),
            "{label} must be refused before it is stored, not at a launch that never happens"
        );
    }
    // ...and the unmutated spec is admitted, so the assertions above are not
    // all passing for one reason that has nothing to do with what they name.
    validate_agent_spec(&spec()).expect("the baseline spec is admissible");
}

/// Redaction hides every VALUE and still distinguishes two configurations that
/// differ only in one.
#[test]
fn redaction_hides_values_while_the_digest_still_identifies_the_configuration() {
    let mut spec = spec();
    spec.claude.settings = vec![ConfigSource::Inline {
        document: json!({"theme": "dark"}),
    }];
    spec.claude.mcp_configs = vec![ConfigSource::File {
        path: "/work/mcp.json".into(),
    }];

    let redacted = redact_agent_spec(&spec);
    let frame = serde_json::to_string(&redacted).expect("serializes");
    assert!(
        !frame.contains("strict"),
        "an environment value reached the frame: {frame}"
    );
    assert!(
        !frame.contains("dark"),
        "an inline settings document reached the frame: {frame}"
    );
    assert!(
        frame.contains("REVIEW_MODE"),
        "the NAME is not a secret and is what makes the frame useful"
    );
    assert!(
        frame.contains("/work/mcp.json"),
        "a file path is not a secret; `probe` prints paths for the same reason"
    );
    assert!(
        frame.contains("Be exact."),
        "the system prompt is deliberately NOT redacted: it is the most \
         important thing about an agent"
    );

    // THE DIGEST IS THE ONE A CALLER CAN RECOMPUTE FROM WHAT IT SUBMITTED.
    //
    // The first version of this assertion said "two agents differing only in a
    // hidden value are still distinguishable" and claimed that a digest taken
    // over the REDACTED spec would collide here. MEASURED by deleting the check
    // and taking the digest over `redact_agent_spec(spec)`: it does not
    // collide, because two different values digest to two different digests, so
    // the assertion passed over the very defect its message named. That is this
    // repository's own bug class, in a check written to catch it.
    //
    // The property that is both true and load-bearing is the one the design
    // actually rests on: the digest a caller computes over the spec it SENT is
    // the digest the store reports, which is what lets a caller check what it
    // launched rather than trust that resolution did what it said. A digest
    // taken over the redacted form breaks exactly that.
    // COMPUTED HERE, NOT BY THE PRODUCTION FUNCTION. Asking `config_digest` for
    // the expected value is what let the previous version of this assertion
    // pass over its own defect: a digest taken over the redacted spec changes
    // BOTH sides of such a comparison identically. This is the caller's own
    // computation -- sha256 over the canonical serialization -- which is what a
    // caller in any of the three languages would do.
    let expected = {
        let mut hasher = Sha256::new();
        hasher.update(serde_json::to_vec(&spec).expect("spec serializes"));
        format!("{:x}", hasher.finalize())
    };
    let temp = tempfile::tempdir().expect("tempdir");
    let store = AgentStore::open(&temp.path().join("agents")).expect("store");
    let stored = store.create(spec.clone(), NOW).expect("create");
    assert_eq!(
        stored.config_digest, expected,
        "a caller must be able to recompute the digest from the spec it SUBMITTED; a digest \
         taken over the redacted form is not that digest"
    );
    let redacted_digest = {
        let mut hasher = Sha256::new();
        hasher.update(serde_json::to_vec(&redacted).expect("redacted serializes"));
        format!("{:x}", hasher.finalize())
    };
    assert_ne!(
        stored.config_digest, redacted_digest,
        "the reported digest must not be one a reader could compute from the redacted frame"
    );

    // ...and it still separates two agents that differ only in a hidden value.
    let mut other = spec.clone();
    other
        .environment
        .set
        .insert("REVIEW_MODE".into(), "lenient".into());
    assert_ne!(
        config_digest(&spec).expect("digest"),
        config_digest(&other).expect("digest")
    );
    // ...while their redacted frames differ only in a digest, never in a value.
    let other_frame = serde_json::to_string(&redact_agent_spec(&other)).expect("serializes");
    assert!(!other_frame.contains("lenient"), "{other_frame}");
}

/// The store refuses an id it never minted, and names the way to find one.
#[test]
fn a_missing_agent_or_version_is_refused_with_the_recovery_named() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = store(&temp.path().join("agents"));
    let created = store.create(spec(), NOW).expect("create");

    let missing = store
        .get(Uuid::from_u128(4242), None)
        .expect_err("no such agent");
    assert_eq!(missing.code, ErrorCode::InvalidConfig);
    assert!(
        missing.details["recommendation"]
            .as_str()
            .is_some_and(|text| text.contains("pmux agent list")),
        "{}",
        missing.details
    );

    let version = AgentVersion::new(9).expect("9 is a version");
    let missing_version = store
        .get(created.agent_id, Some(version))
        .expect_err("no such version");
    assert_eq!(missing_version.code, ErrorCode::InvalidConfig);
    assert!(
        missing_version.message.contains("version 9"),
        "{}",
        missing_version.message
    );
}

/// `list_agents` reports every stored agent's head and NEVER a full spec.
#[test]
fn listing_reports_heads_and_never_a_stored_environment_value() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = store(&temp.path().join("agents"));
    let first = store.create(spec(), NOW).expect("create");
    store
        .update(first.agent_id, first.version, spec(), NOW + 1)
        .expect("update");
    let mut second_spec = spec();
    second_spec.name = "other".into();
    let second = store.create(second_spec, NOW + 2).expect("create");

    let list = store.list().expect("list");
    assert_eq!(list.agents.len(), 2);
    let heads: BTreeMap<Uuid, u64> = list
        .agents
        .iter()
        .map(|summary| (summary.agent_id, summary.version.get()))
        .collect();
    assert_eq!(heads[&first.agent_id], 2, "a list reports the HEAD");
    assert_eq!(heads[&second.agent_id], 1);

    let frame = serde_json::to_string(&list).expect("serializes");
    assert!(
        !frame.contains("strict") && !frame.contains("Be exact."),
        "a list must not spray stored configuration across one frame: {frame}"
    );
}

/// The both-modes refusal names EVERY path an agent supplies, and the list it
/// walks is the production one.
#[test]
fn a_start_that_names_an_agent_and_a_launch_field_is_refused_for_every_path() {
    // Reuses the protocol's own derived list rather than restating it, so a
    // path added there is exercised here with no second edit.
    assert!(!agent_supplied_start_paths().is_empty());
    for path in agent_supplied_start_paths() {
        let mut request = json!({
            "identity": {"mode": "new"},
            "cwd": "/work",
            "agent": {"agent_id": Uuid::from_u128(3).hyphenated().to_string(), "version": 1},
        });
        let key = path.split('.').next().expect("a first component");
        request[key] = match *path {
            "claude" => json!({"executable": "/bin/sh"}),
            "environment.set" => json!({"set": {"TERM": "dumb"}}),
            "environment.unset" => json!({"unset": ["TERM"]}),
            "auth_policy" => json!("inherit"),
            "terminal" => json!({"rows": 1, "cols": 1}),
            "lifecycle" => json!({"mode": "transcript"}),
            "retention" => json!({"mode": "persistent"}),
            "compatibility" => json!("allow_untested"),
            "cell" => json!("minified"),
            other => panic!("agent_supplied_start_paths gained {other:?}"),
        };
        let error = serde_json::from_value::<StartSessionRequest>(request)
            .expect_err("a start may not name an agent and a launch field");
        assert!(error.to_string().contains(path), "{error}");
    }
}

/// **TWO CONCURRENT UPDATES ON ONE FENCE PUBLISH ONE VERSION, OR NEITHER.**
///
/// The fence comparison in `update` cannot decide this: `bin/pmuxd/src/handler.rs`
/// serves 64 connections at once and there is no lock between reading `head`
/// and writing the next version, so two callers holding the same
/// `expected_version` both pass it. What decides is the `link(2)` in
/// `publish_version_exclusively`, where naming the file and refusing to
/// overwrite one are the same syscall.
///
/// MEASURED over exactly this harness before that change, 25 rounds: 7 left
/// `head` naming a file that no longer parsed -- `trailing characters at line 1
/// column 1497`, which also took `list` down for the whole store -- and 13 more
/// answered the winner a `config_digest` the store did not hold. The two racing
/// specs differ in serialized LENGTH deliberately: equal-length specs tear onto
/// each other byte-for-byte and read back clean.
///
/// Twenty-five rounds rather than one, because a race observed once is not a
/// race tested.
#[test]
fn concurrent_updates_on_one_fence_publish_one_version_and_never_a_torn_one() {
    use std::sync::{Arc, Barrier};

    for round in 0..25 {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(store(&temp.path().join("agents")));
        let created = store.create(spec(), NOW).expect("create");
        let agent_id = created.agent_id;

        let barrier = Arc::new(Barrier::new(2));
        let outcomes: Vec<_> = ["b", "c"]
            .into_iter()
            .map(|marker| {
                let store = Arc::clone(&store);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let mut racing = spec();
                    racing.name = format!("reviewer-{marker}");
                    racing.claude.system_prompt = SystemPromptPolicy::Append {
                        // Length differs by marker, so a torn write is visible.
                        prompt: marker.repeat(400 + usize::from(marker.as_bytes()[0])),
                    };
                    barrier.wait();
                    store.update(agent_id, AgentVersion::FIRST, racing, NOW + 1)
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|handle| handle.join().expect("no writer panics"))
            .collect();

        let winners: Vec<_> = outcomes
            .iter()
            .filter_map(|result| result.as_ref().ok())
            .collect();
        assert_eq!(
            winners.len(),
            1,
            "round {round}: exactly one writer may publish version 2"
        );
        let loser = outcomes
            .iter()
            .find_map(|result| result.as_ref().err())
            .expect("the other writer is refused");
        assert_eq!(
            loser.code,
            ErrorCode::IdConflict,
            "round {round}: a lost race is a fence conflict, not a filesystem error: {}",
            loser.message
        );
        assert!(
            loser.message.contains("version 2"),
            "round {round}: the refusal names the version that now exists: {}",
            loser.message
        );

        // THE STORE IS READABLE, at head and at the pinned older version.
        let head = store
            .get(agent_id, None)
            .unwrap_or_else(|error| panic!("round {round}: head unreadable: {}", error.message));
        store
            .get(agent_id, Some(AgentVersion::FIRST))
            .unwrap_or_else(|error| panic!("round {round}: v1 unreadable: {}", error.message));
        let listed = store
            .list()
            .unwrap_or_else(|error| panic!("round {round}: list failed: {}", error.message));
        assert!(listed.unreadable.is_empty(), "round {round}: {listed:?}");

        // ...and the winner was answered the digest the store actually holds.
        assert_eq!(
            winners[0].config_digest, head.config_digest,
            "round {round}: the descriptor a caller received names a configuration the store does \
             not hold"
        );
        assert_eq!(winners[0].version, head.version);
    }
}

/// One unreadable record loses itself and nothing else, and says so.
///
/// `list_agents` used to propagate `?` per entry, so one bad record answered
/// the whole listing with that record's refusal -- and `no agent <id>`
/// recommends this exact command, which made the recommendation unreachable in
/// precisely the state it was offered. Three ways in, all of them states the
/// store can genuinely be in.
#[test]
fn one_unreadable_record_is_reported_by_id_and_never_takes_the_listing_down() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("agents");
    let store = store(&root);
    let good = store.create(spec(), NOW).expect("create");
    let mut other_spec = spec();
    other_spec.name = "other".into();
    let bad = store.create(other_spec, NOW + 1).expect("create");
    let bad_dir = root.join(bad.agent_id.hyphenated().to_string());

    let widen = |path: &Path| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).expect("mode");
    };
    type BreakIt = Box<dyn Fn()>;
    let cases: Vec<(&str, BreakIt)> = vec![
        (
            "a widened head",
            Box::new({
                let head = bad_dir.join("head");
                move || widen(&head)
            }),
        ),
        (
            "a torn version file",
            Box::new({
                let version = bad_dir.join("1.json");
                move || {
                    let mut bytes = std::fs::read(&version).expect("read");
                    bytes.extend_from_slice(b"{trailing}");
                    std::fs::write(&version, &bytes).expect("write");
                }
            }),
        ),
        (
            "a head naming a version that was never minted",
            Box::new({
                let head = bad_dir.join("head");
                move || std::fs::write(&head, b"9\n").expect("write")
            }),
        ),
    ];

    for (label, break_it) in cases {
        let restore = (
            std::fs::read(bad_dir.join("head")).expect("head"),
            std::fs::read(bad_dir.join("1.json")).expect("version"),
        );
        break_it();
        let list = store
            .list()
            .unwrap_or_else(|error| panic!("{label} took the listing down: {}", error.message));
        assert_eq!(
            list.agents.iter().map(|a| a.agent_id).collect::<Vec<_>>(),
            vec![good.agent_id],
            "{label}: the readable record must survive"
        );
        assert_eq!(
            list.unreadable
                .iter()
                .map(|failure| failure.agent_id)
                .collect::<Vec<_>>(),
            vec![bad.agent_id],
            "{label}: the unreadable record must be reported by id"
        );
        assert!(
            !list.unreadable[0].reason.is_empty(),
            "{label}: a reported failure carries the reason"
        );
        // ...and `get_agent` answers the same sentence for it, so the two
        // surfaces cannot drift.
        let direct = store
            .get(bad.agent_id, None)
            .expect_err("the record is unreadable through get too");
        assert_eq!(direct.message, list.unreadable[0].reason, "{label}");

        if bad_dir.join("head").exists() {
            std::fs::set_permissions(bad_dir.join("head"), std::fs::Permissions::from_mode(0o600))
                .expect("mode");
        }
        std::fs::write(bad_dir.join("head"), &restore.0).expect("restore head");
        std::fs::set_permissions(bad_dir.join("head"), std::fs::Permissions::from_mode(0o600))
            .expect("mode");
        std::fs::write(bad_dir.join("1.json"), &restore.1).expect("restore version");
        assert!(store.list().expect("list").unreadable.is_empty());
    }
}

/// `create` publishes an agent WHOLE or not at all, OBSERVED FROM OUTSIDE.
///
/// The old shape created the UUID directory and then wrote into it, so between
/// `create_private_dir_all` and the first `write_version` there was a window in
/// which the store held a UUID-named record with no `head` -- a record `list`
/// then had to read, and one unreadable record used to take the whole listing
/// down. It is also the state the adversary reproduced directly.
///
/// This asserts the window is GONE rather than small: a reader walking the root
/// while creates run must never see a UUID-named directory that is not already
/// complete. The staging name is not a UUID, and the UUID name comes into
/// existence at a `rename(2)` with the whole agent already behind it, so the
/// green direction is deterministic -- there is no interleaving in which the
/// observation is possible. The red direction is probabilistic, which is the
/// right way round for a race: this test cannot flake green.
#[test]
fn a_created_agent_is_never_observable_half_made() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("agents");
    let store = Arc::new(store(&root));
    let creating = Arc::new(AtomicBool::new(true));

    let watcher = std::thread::spawn({
        let root = root.clone();
        let creating = Arc::clone(&creating);
        move || {
            let mut half_made: Vec<String> = Vec::new();
            let mut observations = 0_u64;
            while creating.load(Ordering::Relaxed) {
                let Ok(entries) = std::fs::read_dir(&root) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let Ok(parsed) = Uuid::parse_str(&name) else {
                        continue;
                    };
                    if parsed.hyphenated().to_string() != name {
                        continue;
                    }
                    observations += 1;
                    let path = entry.path();
                    if !path.join("head").exists() || !path.join("1.json").exists() {
                        half_made.push(name);
                    }
                }
            }
            (half_made, observations)
        }
    });

    let mut created = Vec::new();
    for round in 0..200 {
        created.push(store.create(spec(), NOW + round).expect("create").agent_id);
    }
    creating.store(false, Ordering::Relaxed);
    let (half_made, observations) = watcher.join().expect("the watcher does not panic");

    assert!(
        half_made.is_empty(),
        "a UUID-named record was visible before it was complete: {half_made:?}"
    );
    // The watcher must actually have looked, or the assertion above is vacuous.
    assert!(
        observations > 0,
        "the watcher never observed a published record, so it proves nothing"
    );
    assert_eq!(created.len(), 200);

    // ...and every published record is complete and readable.
    let list = store.list().expect("list");
    assert_eq!(list.agents.len(), 200);
    assert!(list.unreadable.is_empty(), "{list:?}");

    // A directory this store never published is not a record, and a listing
    // says nothing about it in either column.
    std::fs::create_dir(root.join(format!(".pending-{}", Uuid::from_u128(77).hyphenated())))
        .expect("staging");
    let list = store.list().expect("list");
    assert_eq!(list.agents.len(), 200);
    assert!(list.unreadable.is_empty(), "{list:?}");
}

/// The per-read owner-only guard reads THE BYTES, not the name in front of
/// them.
///
/// `symlink_metadata` describes the LINK and `std::fs::read` follows it, and a
/// symlink's own mode is `umask`-dependent: under `umask 077` it is born
/// `0700`. MEASURED against that shape, a `1.json` that was a symlink to a
/// `0666` file outside the store was accepted and launched; under the default
/// `umask 022` the same file was refused for the wrong reason ("has mode 755",
/// which was the link's).
#[test]
fn a_version_file_that_is_not_a_regular_file_this_store_wrote_is_refused() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("agents");
    let store = store(&root);
    let created = store.create(spec(), NOW).expect("create");
    let version = root
        .join(created.agent_id.hyphenated().to_string())
        .join("1.json");

    let outside = temp.path().join("outside.json");
    std::fs::copy(&version, &outside).expect("copy");
    std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o666)).expect("mode");
    std::fs::remove_file(&version).expect("remove");
    std::os::unix::fs::symlink(&outside, &version).expect("symlink");

    let error = store
        .get(created.agent_id, Some(AgentVersion::FIRST))
        .expect_err("a symlinked version file is not a stored version");
    assert_eq!(error.code, ErrorCode::InvalidConfig);
    assert!(
        error.message.contains("symbolic link"),
        "the refusal names what is actually wrong: {}",
        error.message
    );
    // ...and so is anything else that is not a regular file. `O_NOFOLLOW`
    // answers the symlink; a DIRECTORY at this name opens perfectly well and
    // only `is_file()` refuses it, so without this case that guard is never
    // exercised.
    std::fs::remove_file(&version).expect("remove the link");
    std::fs::create_dir(&version).expect("a directory where a version belongs");
    let error = store
        .get(created.agent_id, Some(AgentVersion::FIRST))
        .expect_err("a directory is not a stored agent version");
    assert!(
        error.message.contains("not a regular file"),
        "the refusal names what is actually wrong: {}",
        error.message
    );
    // ...and it is refused whatever the LINK's own mode happens to be, which is
    // the property the old guard did not have.
    assert!(
        !error.message.contains("mode 700") && !error.message.contains("mode 755"),
        "the link's mode is not the file's mode: {}",
        error.message
    );
}

/// The per-agent DIRECTORY is held to the tree's bar on every read too.
///
/// Only files used to be re-checked, which left the directory holding them
/// unexamined between the boot that opened the store and the `start_session`
/// that reads a version out of it.
#[test]
fn a_widened_agent_directory_is_refused_when_it_is_read_and_not_only_at_boot() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("agents");
    let store = store(&root);
    let created = store.create(spec(), NOW).expect("create");
    let agent_dir = root.join(created.agent_id.hyphenated().to_string());
    std::fs::set_permissions(&agent_dir, std::fs::Permissions::from_mode(0o755)).expect("mode");

    for read in [
        store.get(created.agent_id, None),
        store.get(created.agent_id, Some(AgentVersion::FIRST)),
    ] {
        let error = read.expect_err("a widened agent directory is refused");
        assert_eq!(error.code, ErrorCode::InvalidConfig);
        assert!(error.message.contains("owner-only"), "{}", error.message);
    }
    // ...and it is REPORTED rather than dropped, and never re-permissioned.
    let list = store.list().expect("list");
    assert!(list.agents.is_empty());
    assert_eq!(list.unreadable.len(), 1);
    assert_eq!(
        std::fs::metadata(&agent_dir)
            .expect("directory")
            .permissions()
            .mode()
            & 0o777,
        0o755,
        "pmuxd never re-permissions a directory it did not create"
    );
}

/// Puts the head pointer back to `version`, which is the on-disk state a crash
/// between `publish_version_exclusively` and `advance_head` leaves.
///
/// The state is not hypothetical and this is not a hypothetical about it.
/// MEASURED with a SIGKILL harness -- a child updating one agent in a loop,
/// killed at an offset jittered uniformly across one measured update cycle --
/// **19 of 50 trials landed in exactly this state**, and before the fix **15 of
/// 40** of them left the agent refusing every fence in both directions forever.
/// Constructing it directly is what makes the assertions below deterministic
/// rather than a race a test has to win; the harness is what establishes it is
/// reachable.
fn rewind_head(agent_dir: &Path, version: u64) {
    let head = agent_dir.join("head");
    std::fs::write(&head, format!("{version}\n")).expect("rewind head");
    std::fs::set_permissions(&head, std::fs::Permissions::from_mode(0o600)).expect("mode");
}

fn version(value: u64) -> AgentVersion {
    AgentVersion::new(value).expect("a version starts at 1")
}

/// A version published before the pointer reached it is ADOPTED, and the agent
/// stays usable.
///
/// The comment this replaces claimed such a crash "reads as 'the update did not
/// land' -- the safe direction". It read as *this agent can never be updated
/// again*: `update` always recomputed `head.next()`, so it always targeted the
/// number already on disk, and `link(2)` always refused it. Both fences were
/// answered `id_conflict` naming the other one, and `list` reported the record
/// healthy at the older version with nothing unreadable, so no surface said a
/// word.
#[test]
fn a_version_published_before_the_pointer_moved_is_adopted_and_never_wedges_the_agent() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("agents");
    let store = store(&root);
    let created = store.create(spec(), NOW).expect("create");
    let agent_id = created.agent_id;
    let agent_dir = root.join(agent_id.hyphenated().to_string());

    let mut second = spec();
    second.name = "reviewer-two".into();
    let published = store
        .update(agent_id, AgentVersion::FIRST, second, NOW + 1)
        .expect("update");
    assert_eq!(published.version, version(2));
    rewind_head(&agent_dir, 1);

    // ADOPTED, and it is the published bytes that are adopted rather than a
    // rebuild of them: the digest is the one the interrupted update minted.
    let head = store.get(agent_id, None).expect("the head is readable");
    assert_eq!(head.version, version(2));
    assert_eq!(head.config_digest, published.config_digest);

    // ...and a listing says so. It used to report this record healthy at
    // version 1 with zero unreadable, which is why nothing surfaced the wedge.
    let listed = store.list().expect("list");
    assert!(listed.unreadable.is_empty(), "{listed:?}");
    assert_eq!(
        listed.agents.iter().map(|a| a.version).collect::<Vec<_>>(),
        vec![version(2)],
        "a listing must never report the version the pointer stopped at as the record's version"
    );

    // Adoption REMOVES nothing, which is the half of the adopt/discard choice a
    // pinned session depends on: `missing_version` promises "a version is never
    // removed", and a start that pinned version 1 still resolves it.
    store
        .get(agent_id, Some(AgentVersion::FIRST))
        .expect("an older version is still pinnable");

    // THE FENCE IS STILL MEANINGFUL, which is the half that was broken. Two
    // consecutive attempts on one fence are answered against one head, so a
    // caller is never told it is stale in one direction and then in the other.
    let stale = store
        .update(agent_id, AgentVersion::FIRST, spec(), NOW + 2)
        .expect_err("the fence the caller held is stale");
    assert_eq!(stale.code, ErrorCode::IdConflict);
    assert!(
        stale
            .message
            .contains("is at version 2, not the expected version 1"),
        "{}",
        stale.message
    );
    let repeated = store
        .update(agent_id, AgentVersion::FIRST, spec(), NOW + 3)
        .expect_err("and it is still stale");
    assert_eq!(
        repeated.message, stale.message,
        "consecutive attempts on one fence must be answered identically, and never in opposite \
         directions"
    );

    // ...and the fence that refusal RECOMMENDS is accepted. That is what
    // "usable" means, and it is the assertion the measured store failed: no
    // fence existed that it would accept, ever.
    assert!(
        stale
            .details
            .get("recommendation")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|text| text.contains("--expected-version 2")),
        "the refusal names the fence that works: {:?}",
        stale.details
    );
    let mut third = spec();
    third.name = "reviewer-three".into();
    let recovered = store
        .update(agent_id, version(2), third, NOW + 4)
        .expect("the agent is still updatable");
    assert_eq!(recovered.version, version(3));
    assert_eq!(
        store.get(agent_id, None).expect("get").version,
        recovered.version
    );
}

/// The pointer is a LOWER BOUND, and resolution walks forward over EVERY
/// published version rather than looking ahead by one.
///
/// A one-step lookahead would be enough for a single crashed writer and is not
/// enough for the store this daemon actually runs: `advance_head` writes an
/// absolute value with no lock behind it, so a writer descheduled between its
/// `link(2)` and its pointer write can land that write after two later writers
/// have already moved the pointer past it, and the pointer REGRESSES. This
/// constructs the end state of that interleaving directly.
#[test]
fn a_head_pointer_that_regressed_resolves_forward_over_every_published_version() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("agents");
    let store = store(&root);
    let created = store.create(spec(), NOW).expect("create");
    let agent_id = created.agent_id;
    let agent_dir = root.join(agent_id.hyphenated().to_string());

    for step in 1..=3 {
        let mut next = spec();
        next.name = format!("reviewer-{step}");
        store
            .update(agent_id, version(step), next, NOW + step)
            .expect("update");
    }
    rewind_head(&agent_dir, 1);

    assert_eq!(
        store.get(agent_id, None).expect("get").version,
        version(4),
        "a pointer three versions behind still resolves to the newest published one"
    );
    assert_eq!(
        store
            .list()
            .expect("list")
            .agents
            .iter()
            .map(|a| a.version)
            .collect::<Vec<_>>(),
        vec![version(4)]
    );
    let mut fifth = spec();
    fifth.name = "reviewer-five".into();
    assert_eq!(
        store
            .update(agent_id, version(4), fifth, NOW + 9)
            .expect("update at the resolved head")
            .version,
        version(5),
        "and the next version minted is one no name is taken for"
    );
}

/// A version NAME that is taken by something that is not a readable version is
/// REPORTED, and is never the name the next update tries to publish at.
///
/// The step predicate is `link(2)`'s and deliberately no narrower: any name at
/// all, not "a regular file", not "one that parses", not "one whose digest
/// checks". Every narrower predicate steps around a name `link(2)` will refuse,
/// which puts `update` back to minting a number it can never publish and
/// answering both fences with the other one -- the wedge, reached by a second
/// road. So the record earns its own refusal instead, from `get_agent` and in
/// `AgentList::unreadable`, and the bytes at that name are left exactly as they
/// were found.
#[test]
fn a_taken_version_name_that_is_not_a_readable_version_is_reported_and_never_published_over() {
    type BreakIt = Box<dyn Fn(&Path)>;
    let cases: Vec<(&str, BreakIt, &str)> = vec![
        (
            "a torn version file",
            Box::new(|path: &Path| {
                let mut bytes = std::fs::read(path).expect("read");
                bytes.extend_from_slice(b"{trailing}");
                std::fs::write(path, &bytes).expect("write");
            }),
            "is not a readable agent version",
        ),
        (
            "a symlink where a version belongs",
            Box::new(|path: &Path| {
                let outside = path.with_file_name("outside.json");
                std::fs::copy(path, &outside).expect("copy");
                std::fs::remove_file(path).expect("remove");
                std::os::unix::fs::symlink(&outside, path).expect("symlink");
            }),
            "symbolic link",
        ),
        (
            "a directory where a version belongs",
            Box::new(|path: &Path| {
                std::fs::remove_file(path).expect("remove");
                std::fs::create_dir(path).expect("directory");
            }),
            "not a regular file",
        ),
    ];

    for (label, break_it, expected) in cases {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("agents");
        let store = store(&root);
        let created = store.create(spec(), NOW).expect("create");
        let agent_id = created.agent_id;
        let agent_dir = root.join(agent_id.hyphenated().to_string());
        let mut second = spec();
        second.name = "reviewer-two".into();
        store
            .update(agent_id, AgentVersion::FIRST, second, NOW + 1)
            .expect("update");
        rewind_head(&agent_dir, 1);
        let taken = agent_dir.join("2.json");
        break_it(&taken);
        let before = std::fs::symlink_metadata(&taken).expect("the name is taken");

        // REPORTED, and never summarized at version 1 as though nothing were
        // wrong: the newest published NAME is what a listing is about.
        let listed = store
            .list()
            .expect("a broken record never takes the listing down");
        assert!(
            listed.agents.is_empty(),
            "{label}: a record whose newest version is unreadable is not a healthy record: \
             {listed:?}"
        );
        assert_eq!(listed.unreadable.len(), 1, "{label}: {listed:?}");
        assert!(
            listed.unreadable[0].reason.contains(expected),
            "{label}: the report names what is actually wrong: {}",
            listed.unreadable[0].reason
        );
        let refused = store
            .get(agent_id, None)
            .expect_err("the head is not readable");
        assert_eq!(
            refused.message, listed.unreadable[0].reason,
            "{label}: the two surfaces must not drift"
        );

        // NEVER PUBLISHED OVER. Both fences are answered, neither is answered
        // with the other, and the bytes at the taken name are untouched.
        let at_one = store
            .update(agent_id, AgentVersion::FIRST, spec(), NOW + 2)
            .expect_err("the fence is stale");
        assert_eq!(at_one.code, ErrorCode::IdConflict, "{label}");
        assert!(
            at_one
                .message
                .contains("is at version 2, not the expected version 1"),
            "{label}: {}",
            at_one.message
        );
        let at_two = store
            .update(agent_id, version(2), spec(), NOW + 3)
            .expect_err("and the version it names cannot be read");
        assert_eq!(
            at_two.code,
            ErrorCode::InvalidConfig,
            "{label}: a broken file is not a fence conflict: {}",
            at_two.message
        );
        assert!(
            at_two.message.contains(expected) && at_two.message.contains("2.json"),
            "{label}: the refusal names the file and what is wrong with it: {}",
            at_two.message
        );
        let after = std::fs::symlink_metadata(&taken).expect("the name is still taken");
        assert_eq!(
            (before.file_type().is_file(), before.len()),
            (after.file_type().is_file(), after.len()),
            "{label}: a taken version name is never written through"
        );
    }
}

/// MANY writers on one fence publish ONE version, and never two.
///
/// The two-writer case is `concurrent_updates_on_one_fence_publish_one_version_
/// and_never_a_torn_one`. This is the same claim at the width the daemon
/// actually serves, and it is here because resolving the head forward gave the
/// losers a SECOND way to lose -- a writer that reads after the winner's
/// `link(2)` now resolves the newer head and fails the courtesy comparison,
/// where before it would have reached `link(2)` and failed there. Both roads
/// have to end in one published version, or the change traded a wedge for a
/// fork.
#[test]
fn many_writers_on_one_fence_publish_exactly_one_version_and_never_two() {
    use std::sync::{Arc, Barrier};

    const WRITERS: usize = 8;
    for round in 0..30 {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(store(&temp.path().join("agents")));
        let agent_id = store.create(spec(), NOW).expect("create").agent_id;
        let barrier = Arc::new(Barrier::new(WRITERS));
        let outcomes: Vec<_> = (0..WRITERS)
            .map(|writer| {
                let store = Arc::clone(&store);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let mut racing = spec();
                    racing.name = format!("reviewer-{writer}");
                    racing.claude.system_prompt = SystemPromptPolicy::Append {
                        prompt: "z".repeat(400 + writer * 37),
                    };
                    barrier.wait();
                    store.update(agent_id, AgentVersion::FIRST, racing, NOW + 1)
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|handle| handle.join().expect("no writer panics"))
            .collect();

        let winners: Vec<_> = outcomes.iter().filter_map(|r| r.as_ref().ok()).collect();
        assert_eq!(
            winners.len(),
            1,
            "round {round}: exactly one of {WRITERS} writers may publish version 2"
        );
        for loser in outcomes.iter().filter_map(|r| r.as_ref().err()) {
            assert_eq!(
                loser.code,
                ErrorCode::IdConflict,
                "round {round}: a lost race is a fence conflict: {}",
                loser.message
            );
            assert!(
                loser.message.contains("is at version 2"),
                "round {round}: every loser is pointed at the version that now exists, whichever \
                 way it lost: {}",
                loser.message
            );
        }
        // ...and the store holds exactly versions 1 and 2. A THIRD file would
        // mean two writers each minted a number, which is the fork.
        let mut published: Vec<u64> = std::fs::read_dir(
            temp.path()
                .join("agents")
                .join(agent_id.hyphenated().to_string()),
        )
        .expect("read dir")
        .flatten()
        .filter_map(|entry| {
            entry
                .file_name()
                .to_str()
                .and_then(|name| name.strip_suffix(".json"))
                .and_then(|number| number.parse::<u64>().ok())
        })
        .collect();
        published.sort_unstable();
        assert_eq!(published, vec![1, 2], "round {round}");
        assert_eq!(
            store.get(agent_id, None).expect("get").config_digest,
            winners[0].config_digest,
            "round {round}: the head is the winner's version"
        );
    }
}

/// A reader racing a writer never observes a head it cannot read.
///
/// Resolving the head forward is a `stat` followed by a read, which is a
/// check-then-act, and the reason it is a safe one has to be measured rather
/// than argued: `link(2)` publishes a name whose inode is already whole and
/// `fsync`ed, and a published version file is never removed, so between the
/// `stat` that finds `N+1` and the read that opens it there is no state
/// transition that can take it away or leave it partial. The reader loop below
/// runs against a writer loop and asserts every read succeeds -- not that most
/// do.
///
/// **NOT DELETION-OBSERVABLE on the head-resolution change itself**, and said
/// here rather than implied: reverting `get` to the raw pointer leaves this
/// green, because a pointer that lags is still a pointer to a readable version.
/// It is a guard against a future scan that reads what it did not `stat`, and
/// the wedge measurements are what prove the change it accompanies.
#[test]
fn a_reader_resolving_the_head_beside_a_live_writer_never_observes_one_it_cannot_read() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let temp = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(store(&temp.path().join("agents")));
    let agent_id = store.create(spec(), NOW).expect("create").agent_id;
    let writing = Arc::new(AtomicBool::new(true));

    let reader = std::thread::spawn({
        let store = Arc::clone(&store);
        let writing = Arc::clone(&writing);
        move || {
            let mut reads = 0u64;
            let mut versions = Vec::new();
            while writing.load(Ordering::Relaxed) {
                let head = store.get(agent_id, None).unwrap_or_else(|error| {
                    panic!("a resolved head must be readable: {}", error.message)
                });
                let listed = store.list().expect("list");
                assert!(
                    listed.unreadable.is_empty(),
                    "a live update is not an unreadable record: {listed:?}"
                );
                // ...and every version at or below the one just observed, which
                // is what a pinned session resolves.
                for value in 1..=head.version.get() {
                    store
                        .get(agent_id, Some(AgentVersion::new(value).expect("version")))
                        .unwrap_or_else(|error| {
                            panic!(
                                "v{value} was published and must stay readable: {}",
                                error.message
                            )
                        });
                }
                versions.push(head.version.get());
                reads += 1;
            }
            (reads, versions)
        }
    });

    let mut fence = AgentVersion::FIRST;
    for step in 0..60 {
        let mut next = spec();
        next.name = format!("reviewer-{step}");
        fence = store
            .update(agent_id, fence, next, NOW + step)
            .expect("update")
            .version;
    }
    writing.store(false, Ordering::Relaxed);
    let (reads, versions) = reader.join().expect("the reader never panics");
    assert!(reads > 0, "the reader must have observed something");
    // The heads a reader saw are NON-DECREASING. A resolved head that went
    // backwards would mean the walk trusted a pointer that regressed.
    assert!(
        versions.windows(2).all(|pair| pair[0] <= pair[1]),
        "a resolved head must never go backwards: {versions:?}"
    );
    assert_eq!(
        store.get(agent_id, None).expect("get").version,
        fence,
        "and it lands on the last version published"
    );
}

// ---------------------------------------------------------------------------
// The bounds and the guards, found by cargo-mutants rather than by reading
//
// Every test below closes a mutant that survived the first full mutation run of
// `crates/service/src/agent.rs` (100 mutants). They are grouped here because
// they share one shape: a predicate whose message states an exact rule that no
// test ever exercised at the point where the rule bites. That is the same bug
// class the counter in `crates/protocol/src/v1.rs` tracks, arrived at by
// enumeration instead of by suspicion.
// ---------------------------------------------------------------------------

/// The byte bound in the refusal is the byte bound the check applies.
///
/// SURVIVING MUTANTS CLOSED: `agent.rs:302 > -> >=`, `agent.rs:309 > -> >=`,
/// `agent.rs:309 > -> ==`. Every test of these limits used a value far past
/// them, which cannot distinguish `>` from `>=` -- so the refusal said "at
/// most N bytes" while nothing had ever established that N itself is admitted.
///
/// The bound is READ OUT OF THE REFUSAL rather than restated here, because
/// `MAX_AGENT_LABEL_BYTES` is private and a copy of a private constant in a
/// test is the same defect one layer down: it would keep passing after the
/// constant moved.
#[test]
fn the_label_bounds_admit_exactly_the_length_the_refusal_names_and_refuse_one_past() {
    fn bound_from(message: &str) -> usize {
        let digits: String = message
            .chars()
            .skip_while(|value| !value.is_ascii_digit())
            .take_while(char::is_ascii_digit)
            .collect();
        digits
            .parse()
            .unwrap_or_else(|_| panic!("the refusal must name its bound: {message}"))
    }

    // The name.
    let mut over = spec();
    over.name = "n".repeat(100_000);
    let refusal = validate_agent_spec(&over).expect_err("a 100,000-byte name is refused");
    assert_eq!(refusal.code, ErrorCode::InvalidConfig);
    let limit = bound_from(&refusal.message);
    assert!(limit > 0 && limit < 100_000, "{}", refusal.message);

    let mut exact = spec();
    exact.name = "n".repeat(limit);
    if let Err(error) = validate_agent_spec(&exact) {
        panic!(
            "a name of exactly {limit} bytes must be admitted: {}",
            error.message
        );
    }
    let mut past = spec();
    past.name = "n".repeat(limit + 1);
    assert_eq!(
        validate_agent_spec(&past)
            .expect_err("one byte past the bound is refused")
            .code,
        ErrorCode::InvalidConfig
    );

    // The description, whose bound is a different number and whose check is a
    // different line.
    let mut over = spec();
    over.description = Some("d".repeat(100_000));
    let refusal = validate_agent_spec(&over).expect_err("a 100,000-byte description is refused");
    let limit = bound_from(&refusal.message);
    assert!(limit > 0 && limit < 100_000, "{}", refusal.message);

    let mut exact = spec();
    exact.description = Some("d".repeat(limit));
    if let Err(error) = validate_agent_spec(&exact) {
        panic!(
            "a description of exactly {limit} bytes must be admitted: {}",
            error.message
        );
    }
    let mut past = spec();
    past.description = Some("d".repeat(limit + 1));
    assert_eq!(
        validate_agent_spec(&past)
            .expect_err("one byte past the bound is refused")
            .code,
        ErrorCode::InvalidConfig
    );
}

/// A zero in EITHER terminal dimension is refused, not only in both.
///
/// SURVIVING MUTANT CLOSED: `agent.rs:327 || -> &&`. Every existing case set
/// both dimensions to zero at once, which is the one input on which `||` and
/// `&&` agree.
#[test]
fn a_zero_terminal_dimension_is_refused_whichever_one_of_the_two_it_is() {
    for (rows, cols) in [(0, 132), (40, 0), (0, 0)] {
        let error = {
            let mut candidate = spec();
            candidate.terminal.rows = rows;
            candidate.terminal.cols = cols;
            validate_agent_spec(&candidate)
                .expect_err("a zero dimension is not a terminal an agent could launch")
        };
        assert_eq!(error.code, ErrorCode::InvalidConfig, "{rows}x{cols}");
    }
    // ...and a terminal with neither dimension zero is admitted.
    validate_agent_spec(&spec()).expect("an ordinary terminal is admitted");
}

/// Each forbidden byte in an environment NAME is refused on its own.
///
/// SURVIVING MUTANT CLOSED: `agent.rs:419 || -> &&`. A name carrying both `=`
/// and NUL cannot tell the two operators apart.
#[test]
fn an_environment_name_is_refused_for_either_forbidden_byte_on_its_own() {
    for name in ["HAS=EQUALS", "HAS\0NUL", "HAS=BOTH\0"] {
        let mut candidate = spec();
        candidate.environment.set = BTreeMap::from([(name.to_owned(), "v".to_owned())]);
        let error = validate_agent_spec(&candidate)
            .expect_err("a forbidden byte in an environment name is refused");
        assert_eq!(error.code, ErrorCode::InvalidConfig, "{name:?}");

        let mut candidate = spec();
        candidate.environment.unset = BTreeSet::from([name.to_owned()]);
        validate_agent_spec(&candidate).expect_err("and it is refused in `unset` too");
    }
    // ...and a name carrying neither is admitted, so this cannot pass by
    // refusing every name.
    let mut candidate = spec();
    candidate.environment.set = BTreeMap::from([("PLAIN_NAME".to_owned(), "v".to_owned())]);
    validate_agent_spec(&candidate).expect("an ordinary environment name is admitted");
}

/// The redaction is the SHA-256 of the bytes it replaced, and not a constant.
///
/// SURVIVING MUTANTS CLOSED: `agent.rs:483 value_digest -> String::new()` and
/// `-> "xyzzy".into()`. Every existing assertion checked that the plaintext was
/// GONE, which a constant satisfies exactly as well as a digest -- and a
/// constant would make two different secrets indistinguishable on an inspection
/// surface whose whole claim is that it identifies the configuration exactly.
#[test]
fn every_redacted_value_is_the_sha256_of_the_exact_bytes_it_replaced() {
    fn expected(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        format!("sha256:{:x}", hasher.finalize())
    }

    let mut candidate = spec();
    candidate.environment.set = BTreeMap::from([
        ("FIRST".to_owned(), "one".to_owned()),
        ("SECOND".to_owned(), "two".to_owned()),
    ]);
    let document = json!({"permissions": {"allow": ["Read"]}});
    candidate.claude.settings = vec![ConfigSource::Inline {
        document: document.clone(),
    }];

    let redacted = redact_agent_spec(&candidate);
    assert_eq!(redacted.environment.set["FIRST"], expected(b"one"));
    assert_eq!(redacted.environment.set["SECOND"], expected(b"two"));
    // Two different values must redact to two different strings: that is the
    // whole difference between a digest and a placeholder.
    assert_ne!(
        redacted.environment.set["FIRST"],
        redacted.environment.set["SECOND"]
    );
    let ConfigSource::Inline { document: carried } = &redacted.claude.settings[0] else {
        panic!("an inline settings document stays inline once redacted")
    };
    assert_eq!(
        carried,
        &serde_json::Value::String(expected(&serde_json::to_vec(&document).unwrap()))
    );
}

/// A stored agent file that is a symbolic link is refused, and never followed.
///
/// SURVIVING MUTANTS CLOSED: `agent.rs:1481` -- the `ELOOP` match guard, in all
/// three of its mutations (`-> true`, `-> false`, `== -> !=`). `O_NOFOLLOW` was
/// opened with, the refusal was written, its message names the hazard exactly,
/// and nothing in the tree had ever put a symlink in the store.
///
/// The link points at a file that is a PERFECTLY VALID stored version, so a
/// reader that followed it would succeed and report the linked agent's
/// configuration under this agent's id. That is what makes the refusal
/// necessary rather than tidy.
#[test]
fn a_stored_version_that_is_a_symbolic_link_is_refused_rather_than_followed() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("agents");
    let store = store(&root);
    let decoy = store.create(spec(), NOW).expect("the linked-to agent");

    let mut victim_spec = spec();
    victim_spec.name = "victim".into();
    let victim = store
        .create(victim_spec, NOW)
        .expect("the agent under attack");
    assert_ne!(decoy.agent_id, victim.agent_id);

    let target = root
        .join(decoy.agent_id.hyphenated().to_string())
        .join("1.json");
    let planted = root
        .join(victim.agent_id.hyphenated().to_string())
        .join("2.json");
    assert!(target.is_file(), "the fixture must link at a real version");
    std::os::unix::fs::symlink(&target, &planted).unwrap();
    // The premise: following the link WOULD succeed and WOULD hand back the
    // other agent's configuration.
    assert!(
        std::fs::read(&planted).is_ok(),
        "a reader that followed this link would have been served"
    );

    let error = store
        .get(victim.agent_id, Some(AgentVersion::new(2).unwrap()))
        .expect_err("a version file that is a symlink is not a stored version");
    assert_eq!(error.code, ErrorCode::InvalidConfig);
    // Asserted on a clause PMUX wrote, never on the word "symbolic link".
    //
    // The first version of this assertion tested `contains("symbolic link")`
    // and was satisfied by the kernel's own `Too many levels of symbolic links
    // (os error 62)` coming back through the generic arm -- so it stayed green
    // with the guard replaced by `false`, and `cargo-mutants` said so. An
    // assertion a mutant satisfies is the bug class this whole file counts.
    assert!(
        error
            .message
            .contains("a stored agent file is a regular file this store wrote"),
        "the refusal must be the one pmux wrote and not the kernel's errno text: {}",
        error.message
    );
    assert!(
        error.details["recommendation"]
            .as_str()
            .is_some_and(|text| text.contains("never follows a link out of its own store")),
        "and it must carry the recommendation the refusal promises: {:?}",
        error.details
    );

    // The OTHER direction, which is the half a guard replaced by `true`
    // satisfies: a version file that fails to open for any OTHER reason must
    // NOT be reported as a symbolic link. A regular file with no permissions at
    // all fails `open(2)` with `EACCES`, never `ELOOP`.
    let unreadable = root
        .join(decoy.agent_id.hyphenated().to_string())
        .join("1.json");
    std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).expect("close");
    let refused = store.get(decoy.agent_id, Some(AgentVersion::FIRST));
    std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o600)).expect("reopen");
    match refused {
        // Running as a user the mode bits do not apply to; unreachable.
        Ok(_) => {}
        Err(error) => assert!(
            !error.message.contains("is a symbolic link"),
            "a file pmux merely could not open is not a symbolic link: {}",
            error.message
        ),
    }
    store
        .get(decoy.agent_id, Some(AgentVersion::FIRST))
        .expect("and it reads again once the mode is restored");
}

/// The start-path list this crate re-exports is the protocol's own array.
///
/// SURVIVING MUTANTS CLOSED: `agent.rs:1591 supplied_start_paths` replaced with
/// an empty list, with `[""]`, and with `["xyzzy"]`. Its doc says it is
/// "re-exported so callers that must name one in a message read the protocol's
/// list and never a copy" -- and nothing compared the two, so every one of
/// those three replacements was a refusal message naming a path that does not
/// exist, with the whole suite green.
#[test]
fn the_start_paths_this_crate_re_exports_are_the_protocols_own_list() {
    assert_eq!(
        pseudomux_service::agent::supplied_start_paths(),
        agent_supplied_start_paths(),
        "the re-export must be the protocol's array and never a copy of it"
    );
    assert!(
        !agent_supplied_start_paths().is_empty(),
        "an empty list would make this comparison vacuous"
    );
}

/// An agent directory that cannot be inspected is a store failure, not a
/// missing agent.
///
/// SURVIVING MUTANT CLOSED: `agent.rs:969` -- the `NotFound` match guard
/// replaced with `true`, which reports every `stat` failure as
/// `agent_not_found`. The two answers are the same words to a reader and
/// different facts: "there is no such agent" is a caller error, and "pmux could
/// not look" is an operator one, and telling the second as the first sends an
/// operator to create an agent that already exists.
#[test]
fn an_agent_directory_that_cannot_be_inspected_is_not_reported_as_a_missing_agent() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("agents");
    let store = store(&root);
    let created = store.create(spec(), NOW).expect("create");

    // Absent: the "no agent <id>" refusal, which is the branch the guard
    // selects. Both refusals carry `InvalidConfig` -- `missing_agent` mints no
    // new code on purpose -- so the two are told apart by the MESSAGE, which is
    // the only thing an operator reads.
    let absent = store
        .get(Uuid::from_u128(0xDEAD), None)
        .expect_err("an id nothing was ever stored under");
    assert!(
        absent.message.starts_with("no agent "),
        "an absent agent is reported as absent: {}",
        absent.message
    );

    // Present but unreachable: the parent is closed, so `symlink_metadata`
    // fails with `EACCES` rather than `ENOENT`. The agent EXISTS.
    //
    // `0o600` AND NOT `0o300`, and the difference is the whole test. `stat(2)`
    // needs SEARCH permission on each parent directory -- the execute bit -- and
    // not read. `0o300` is `-wx`, which GRANTS search, so `symlink_metadata`
    // succeeded, `get` returned `Ok`, and this test took the escape hatch below
    // and asserted nothing. It passed that way with the guard deleted, which is
    // how `agent.rs:969` stayed on the survivor list under a doc comment
    // claiming this test closed it. MEASURED: with the parent at `0o300` and
    // `0o100` the `lstat` succeeds; at `0o600` and `0o400` it is `EACCES`.
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o600)).expect("close");
    // THE FIXTURE'S PREMISE, ASSERTED BEFORE IT IS USED, in the idiom the rest
    // of this tree uses for alias fixtures. A closed parent that `stat` walks
    // anyway makes every assertion below vacuous, and vacuous is exactly what
    // this test was: it must fail as a broken fixture rather than pass as a
    // rule that held. Skipping on `Ok` without checking this is what let the
    // guard's mutant survive under a comment saying it had been closed.
    let premise = std::fs::symlink_metadata(root.join(created.agent_id.to_string())).err();
    let blocked = store.get(created.agent_id, None);
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).expect("reopen");
    assert_eq!(
        premise.map(|error| error.kind()),
        Some(std::io::ErrorKind::PermissionDenied),
        "a parent with no search bit must make `stat` fail with EACCES, or this \
         test proves nothing about the arm that reports it"
    );

    let blocked = blocked.expect_err("an unreadable store is a refusal, not an agent");
    assert!(
        !blocked.message.starts_with("no agent "),
        "an unreadable store must not be reported as an absent agent: {}",
        blocked.message
    );
    assert!(
        blocked.message.contains("could not inspect it"),
        "and it must say what actually happened: {}",
        blocked.message
    );
    // ...and the agent really is still there once the directory reopens.
    store
        .get(created.agent_id, None)
        .expect("the agent was never missing");
}

/// A stored file under the wrong NAME is refused, and one field disagreeing is
/// enough.
///
/// SURVIVING MUTANT CLOSED: `agent.rs:1167 || -> &&`, which requires BOTH the
/// agent id and the version to disagree before a version file is refused. The
/// realistic corruption is exactly one of them: a copy of `1.json` left at
/// `2.json` by an operator, or a directory restored from a backup taken at a
/// different version. Under `&&` the store hands that file back as version 2,
/// and every caller that pinned version 2 launches version 1's configuration
/// with version 2's digest echoed back at it.
#[test]
fn a_version_file_whose_own_fields_disagree_with_its_name_is_refused() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("agents");
    let store = store(&root);
    let created = store.create(spec(), NOW).expect("create");
    let directory = root.join(created.agent_id.hyphenated().to_string());
    let first = std::fs::read(directory.join("1.json")).expect("version 1 exists");

    // ONE field disagrees: the id is this agent's, the version is not.
    std::fs::write(directory.join("2.json"), &first).expect("plant the copy");
    std::fs::set_permissions(
        directory.join("2.json"),
        std::fs::Permissions::from_mode(0o600),
    )
    .expect("mode");
    let error = store
        .get(created.agent_id, Some(AgentVersion::new(2).unwrap()))
        .expect_err("a file that names version 1 is not version 2");
    assert_eq!(error.code, ErrorCode::InvalidConfig);
    assert!(
        error.message.contains("version 1, not agent"),
        "the refusal must name what the file actually says: {}",
        error.message
    );

    // The OTHER field alone: the version is right, the id is another agent's.
    let mut other = spec();
    other.name = "other".into();
    let sibling = store.create(other, NOW).expect("a second agent");
    let sibling_first = std::fs::read(
        root.join(sibling.agent_id.hyphenated().to_string())
            .join("1.json"),
    )
    .expect("the sibling's version 1");
    std::fs::write(directory.join("1.json"), &sibling_first).expect("overwrite");
    let error = store
        .get(created.agent_id, Some(AgentVersion::FIRST))
        .expect_err("a file that names another agent is not this agent's version");
    assert_eq!(error.code, ErrorCode::InvalidConfig);
    assert!(
        error.message.contains("names agent"),
        "the refusal must name the agent the file actually claims: {}",
        error.message
    );
}
