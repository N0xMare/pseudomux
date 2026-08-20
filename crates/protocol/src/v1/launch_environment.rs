//! The v1 launch-environment policy: which caller-supplied environment names
//! reach an interactive Claude child, and which are dropped before launch.
//!
//! # Why this is protocol and not service implementation detail
//!
//! The daemon is what *enforces* this policy, so the obvious home for it is
//! `crates/service`. It lives here instead, and the reason is a property of the
//! v1 contract rather than a convenience:
//!
//! **If a client must be able to predict which inherited variables the daemon
//! will drop, then that prediction is part of the observable v1 contract, not a
//! service implementation detail.**
//!
//! The daemon is the remaining reader. `pmux probe` and
//! `agent_profile::verify_required_environment` used to need the same
//! prediction without contacting a daemon; both are gone from the product.
//! The table stays here so a second reader cannot grow a private copy.
//!
//! Protocol v1 carries no field for the daemon's own
//! `removed_environment_keys`, so those two callers compute the answer locally.
//! Before this module existed they did it from hand-copied tables kept honest by
//! source-text-parsing drift fences. One definition, reachable from every crate
//! that already depends on `pseudomux-protocol`, removes that whole class of
//! defect: `crates/client`, `crates/service` and `bin/pmux` all depend on this
//! crate, and it is the only crate all three can reach.
//!
//! Nothing here is a wire type, so nothing here appears in
//! `tests/conformance/v1/manifest.json`. It is a *behavioral* clause of the same
//! contract, in the same spirit as [`admit_native_frame_header`], which also
//! lives here so that every transport agrees on one boundary.
//!
//! [`admit_native_frame_header`]: super::admit_native_frame_header
//!
//! # The shape of the policy
//!
//! `crates/service/src/claude_launch.rs::build_environment` applies these tables
//! in one fixed order:
//!
//! ```text
//! effective = allowlist(snapshot) - unset + set - policy_removals + profile_changes
//! ```
//!
//! 1. **allowlist(snapshot)** — [`inherits`] filters the inherited term.
//!    Unknown means denied.
//! 2. **- unset**, 3. **+ set** — the caller's explicit patch. `set` bypasses
//!    the allowlist entirely; it is the supported escape hatch.
//! 4. **- policy_removals** — [`subscription_policy_removes`].
//! 5. **+ profile_changes** — [`transparent_profile_removes`], the tmux-shim
//!    `PATH` prune, and `TERM=xterm-256color`.
//!
//! Steps 4 and 5 run *after* `set`, which is why a name they remove cannot be
//! restored through [`super::EnvironmentSpec::set`] while a name step 1 drops can.
//!
//! # Matching is case-sensitive, deliberately
//!
//! In both the exact and the prefix form, under every table here. POSIX
//! environment names are case-sensitive; a case-insensitive allowlist would
//! admit `path`, `Term`, or a lowercase spelling of a future marker as if it
//! were the reviewed name. Folding would also create a second, divergent notion
//! of "the same variable" — the exact defect consolidating these tables into one
//! module exists to prevent. The lowercase proxy spellings in
//! [`INHERITED_EXACT_KEYS`] are therefore listed individually rather than
//! folded, because both cases are genuinely in use by real tooling.

use super::AuthPolicy;

/// Credential and provider-selection names removed under
/// [`AuthPolicy::Subscription`], however they arrived.
///
/// Applied at step 4, after the caller's explicit `set`, so a subscription
/// session cannot be handed an API key by any channel.
pub const SUBSCRIPTION_AUTH_KEYS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_BEDROCK_BASE_URL",
    "ANTHROPIC_VERTEX_BASE_URL",
    "ANTHROPIC_FOUNDRY_BASE_URL",
    "AWS_BEARER_TOKEN_BEDROCK",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
];

/// Names the transparent terminal profile removes at step 5.
pub const TRANSPARENT_EXACT_KEYS: &[&str] = &[
    "RMUX",
    "TMUX",
    "TMUX_PANE",
    "TMUX_PROGRAM",
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS",
    // Behavioral markers from a parent Claude/remote process are never part
    // of a transparent interactive child, regardless of its auth source.
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_REMOTE",
    // A parent Claude Code session exports this to mark a nested invocation.
    // Inheriting it makes the child behave as somebody else's subordinate
    // session: it still renders a composer, still accepts the bracketed paste
    // and the Enter, and then never writes a transcript of its own -- so the
    // post-arm typed-user row can never appear and every turn dies at
    // `awaiting_prompt_ack` with `TurnTimeout`. Observed 2026-07-27 against
    // Claude 2.1.215 and 2.1.220; isolated to this one variable, which alone
    // reproduces the hang and whose removal alone fixes it. `spec.md` already
    // promised nested-marker removal before launch; this delivers it.
    "CLAUDE_CODE_CHILD_SESSION",
];

/// Prefixes the transparent terminal profile removes at step 5.
pub const TRANSPARENT_PREFIXES: &[&str] = &[
    "RMUX",
    "TMUX",
    // Conductor/Codex and other programmatic parents may carry Claude SDK
    // markers. An interactive compatibility cell must not inherit SDK mode or
    // SDK-only MCP/built-in-agent controls from its orchestrating process.
    "CLAUDE_AGENT_SDK_",
    "CLAUDE_CODE_SDK_",
];

// ---------------------------------------------------------------------------
// Inheritance allowlist.
//
// A denylist can never be completed. `CLAUDECODE`, `CLAUDE_CODE_ENTRYPOINT`,
// `CLAUDE_CODE_REMOTE` and `CLAUDE_CODE_CHILD_SESSION` were each added to
// `TRANSPARENT_EXACT_KEYS` only after a live failure, and each was invisible to
// the whole deterministic suite because the fake Claude does not read them.
// So the inherited `snapshot` is filtered by an allowlist first: an unknown
// name is denied *by construction*, and the next nested-session marker Claude
// invents is dead on arrival without anyone noticing it exists.
//
// **Stated precisely, because "unknown means denied" is easy to overclaim.**
// It holds *unconditionally* under [`AuthPolicy::Subscription`], which is the
// default. Under [`AuthPolicy::Inherit`] it holds for everything except the
// provider-routing families enumerated in [`PROVIDER_ROUTING_PREFIXES`] and
// [`PROVIDER_ROUTING_EXACT_KEYS`], which stay open by an explicit, justified
// decision recorded on those constants — `Inherit` *is* the caller saying
// "keep my ambient credential environment". Every marker Claude has ever
// invented lives in the `CLAUDE*` namespace, which no branch of this allowlist
// admits under any policy, so the dead-on-arrival property above survives the
// exception.
//
// The allowlist governs the inherited `snapshot` term only. `EnvironmentSpec`'s
// `set` is the caller's explicit, deliberate channel (an MCP server's API token,
// for instance) and bypasses it entirely. That is the supported escape hatch for
// any name this list does not admit.
//
// Matching is case-sensitive in both forms; see the module note above.

/// Names inherited from the caller snapshot under every auth policy.
///
/// Deliberately generous with genuinely-needed infrastructure: a too-tight list
/// breaks Claude in environments this repository cannot test, and that failure
/// mode is worse than the leak it would prevent.
pub const INHERITED_EXACT_KEYS: &[&str] = &[
    // Process basics. Without PATH Claude cannot resolve `node`, `git`, or any
    // Bash-tool command; without HOME it cannot find `~/.claude` (spec.md:243).
    "PATH",
    "HOME",
    "SHELL",
    "USER",
    "LOGNAME",
    "TMPDIR",
    "PWD",
    "TZ",
    // Terminal identity and geometry. The TUI is the liveness gate: a terminal
    // that renders differently than pmux measured is a hang, not a wrong answer.
    // `TERM` is overwritten by the transparent profile; the rest are inherited.
    "TERM",
    "COLORTERM",
    "TERMINFO",
    "TERMINFO_DIRS",
    "LINES",
    "COLUMNS",
    // Locale. `LANG` plus the `LC_` prefix below; `LANGUAGE` is the GNU
    // gettext spelling and is inert everywhere else.
    "LANG",
    "LANGUAGE",
    // Claude's own configuration root. `native.rs:1753` resolves the effective
    // config root from this or HOME; denying it would break every caller that
    // does not use `$HOME/.claude`.
    "CLAUDE_CONFIG_DIR",
    // TLS trust and proxying. Denying these breaks Claude *completely* behind a
    // corporate proxy or a custom CA, with no local reproduction available.
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    // Nix and NixOS export `NIX_SSL_CERT_FILE` and frequently do **not** export
    // `SSL_CERT_FILE`. Measured on a live macOS dev host 2026-07-27: the box
    // exports `NIX_SSL_CERT_FILE` and no other CA-bundle variable, so without
    // this entry the allowlist drops the only trust root present and takes TLS
    // with it. A Nix user would see every request fail with a certificate error
    // that reproduces nowhere else.
    "NIX_SSL_CERT_FILE",
    "NODE_EXTRA_CA_CERTS",
    "REQUESTS_CA_BUNDLE",
    "CURL_CA_BUNDLE",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
    // XDG base directories. Config/cache/data are what callers actually
    // redirect; state and runtime are included because denying half of a
    // standard set is how a tool ends up writing to two different roots.
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "XDG_RUNTIME_DIR",
    // Node runtime. Claude Code is a Node program launched through a
    // `#!/usr/bin/env node` shim.
    "NODE_OPTIONS",
    "NODE_PATH",
    // Agent-forwarded SSH credentials. Claude's Bash tool routinely runs
    // `git fetch`/`git push`; denying this turns every SSH remote into a
    // hang-at-the-passphrase-prompt with no diagnosable cause.
    "SSH_AUTH_SOCK",
    // Git. `SSH_AUTH_SOCK` alone does not cover ordinary repository work: a
    // deploy key is delivered through `GIT_SSH_COMMAND`, and a caller that
    // injects configuration does it through the `GIT_CONFIG_*` family.
    //
    // **Named individually rather than as a blanket `GIT_` prefix, and that is
    // the load-bearing decision here.** `GIT_DIR`, `GIT_WORK_TREE`,
    // `GIT_INDEX_FILE`, `GIT_OBJECT_DIRECTORY` and `GIT_COMMON_DIR` leak from
    // any parent that is itself inside a git operation -- a hook, a
    // `rebase --exec`, an editor spawned by `git commit` -- and each one
    // silently redirects every Bash-tool git command at a *different repository
    // than `cwd`*. That is a wrong-repo write with no diagnostic, which is
    // exactly the ambient resolution this product refuses everywhere else. A
    // blanket prefix would admit all five, plus `GIT_EDITOR`/`GIT_PAGER`, which
    // can hang a non-interactive child on a pager or editor that never exits.
    "GIT_SSH_COMMAND",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    // `GIT_CONFIG_COUNT` and the `GIT_CONFIG_KEY_`/`GIT_CONFIG_VALUE_` prefixes
    // below are one indivisible mechanism: git aborts with
    // `missing config key GIT_CONFIG_KEY_0` when the count arrives without its
    // pairs, so admitting the count alone would break git outright rather than
    // merely dropping configuration. All three move together or none do.
    "GIT_CONFIG_COUNT",
];

/// Prefixes inherited from the caller snapshot under every auth policy.
pub const INHERITED_PREFIXES: &[&str] = &[
    // POSIX locale categories: `LC_ALL`, `LC_CTYPE`, `LC_TIME`, ... The set is
    // open-ended per platform, so it must be a prefix rather than a list.
    "LC_",
    // pmux's own namespace. `pmux-test-claude` -- a shipped release binary and
    // the entire hermetic full-stack lane -- is driven through `PMUX_TEST_*`
    // names delivered in the snapshot (`crates/e2e/tests/full_stack.rs:3568`,
    // `crates/service/tests/native_service.rs:538`,
    // `crates/service/tests/private_runtime.rs:76`). Real Claude reads nothing
    // in this namespace, so allowing it costs nothing and denying it would make
    // Gate A structurally unpassable.
    "PMUX_",
    // The two open halves of the `GIT_CONFIG_COUNT` mechanism above. Both are
    // open-ended by construction -- the count names how many `_0.._n-1` pairs
    // follow -- so they cannot be enumerated. Neither can redirect the
    // repository the way `GIT_DIR` can: they set config keys, and the ones that
    // touch the worktree (`core.worktree`, `core.bare`) are already reachable
    // through the admitted `GIT_CONFIG_GLOBAL` file.
    "GIT_CONFIG_KEY_",
    "GIT_CONFIG_VALUE_",
];

/// Names inherited **only** under [`AuthPolicy::Inherit`], in addition to
/// [`SUBSCRIPTION_AUTH_KEYS`].
///
/// `Inherit` is an explicit caller decision to keep the ambient credential
/// environment (`spec.md` §4). Provider routing is not one variable: Bedrock
/// resolves credentials through the AWS SDK's own environment, Vertex through
/// Google ADC, Foundry through Azure. Allowing the ten selector keys while
/// denying the credentials they select would leave `Inherit` broken in a way
/// that looks like an auth outage.
///
/// **Why these stay open namespaces rather than enumerated names.** This is the
/// one place the allowlist is deliberately not closed, so the reasoning is
/// recorded rather than assumed:
///
/// * They are **not pmux's to enumerate, and not Claude's to invent in**.
///   `AWS_`, `GOOGLE_`, `GCLOUD_`, `CLOUDSDK_` and `AZURE_` belong to the AWS,
///   Google and Azure SDKs. `AWS_` alone runs to access key, secret, session
///   token, profile, region, default region, config file, shared credentials
///   file, role ARN, web-identity token file, container credential URIs, CA
///   bundle, endpoint override and more, and the set changes on those vendors'
///   release schedules, not ours. Decisively: **the failure mode this allowlist
///   exists to stop is a *Claude-owned* name like `CLAUDE_CODE_CHILD_SESSION`
///   arriving unreviewed, and no `AWS_*` name can ever be one.**
/// * `ANTHROPIC_` is Claude's own namespace, so it is the one that deserves
///   scrutiny — and it is kept open anyway, on evidence. It carries at least
///   five separately-versioned families that `Inherit` exists to preserve:
///   credentials and base URLs (already in [`SUBSCRIPTION_AUTH_KEYS`]), model
///   routing (`ANTHROPIC_MODEL`, `ANTHROPIC_SMALL_FAST_MODEL`,
///   `ANTHROPIC_DEFAULT_{OPUS,SONNET,HAIKU}_MODEL`), request shaping
///   (`ANTHROPIC_CUSTOM_HEADERS`, `ANTHROPIC_VERTEX_PROJECT_ID`), profile
///   selection (`ANTHROPIC_PROFILE`, `ANTHROPIC_CONFIG_DIR`), and workload
///   identity federation (`ANTHROPIC_FEDERATION_RULE_ID`,
///   `ANTHROPIC_ORGANIZATION_ID`, `ANTHROPIC_SERVICE_ACCOUNT_ID`,
///   `ANTHROPIC_IDENTITY_TOKEN{,_FILE}`, `ANTHROPIC_WORKSPACE_ID`). The last two
///   families are recent additions; enumerating the namespace freezes a list
///   this repository has no way to test and converts every future addition into
///   a silent auth outage under the one policy whose entire purpose is "keep my
///   ambient credential environment". That is exactly the failure the note above
///   [`INHERITED_EXACT_KEYS`] calls worse than the leak it would prevent.
///   Meanwhile the marker risk is empirically nil: **all four names added to
///   [`TRANSPARENT_EXACT_KEYS`] after a live failure are `CLAUDE*`, none is
///   `ANTHROPIC_*`**, and `CLAUDE_CODE_*` is denied by this allowlist under
///   every policy.
///
/// The bound on all of this is that the whole list is `Inherit`-only. Under the
/// default `Subscription` every name here is denied at step 1 *and* removed at
/// step 4, and `unknown means denied` is unconditional.
pub const PROVIDER_ROUTING_PREFIXES: &[&str] = &[
    "ANTHROPIC_", // base URLs, auth tokens, model routing, headers, WIF, profiles
    "AWS_",       // Bedrock: keys, session token, profile, region, role, config paths
    "GOOGLE_",    // Vertex: GOOGLE_APPLICATION_CREDENTIALS, GOOGLE_CLOUD_PROJECT
    "GCLOUD_",
    "CLOUDSDK_", // gcloud SDK configuration
    "AZURE_",    // Foundry
    // Per-model Vertex region overrides (`VERTEX_REGION_CLAUDE_3_5_HAIKU`).
    // Narrowed from the bare `VERTEX_` namespace: every documented member of
    // this family is a region string for one model, the set is open-ended only
    // in the model suffix, and the values are inert routing hints rather than
    // credentials. A bare `VERTEX_` admitted any present or future `VERTEX_*`
    // name for no stated need.
    "VERTEX_REGION_",
];

/// Exact names inherited only under [`AuthPolicy::Inherit`], beyond the
/// prefixes above and [`SUBSCRIPTION_AUTH_KEYS`].
pub const PROVIDER_ROUTING_EXACT_KEYS: &[&str] = &[
    "CLOUD_ML_REGION", // Vertex default region; not under any admitted prefix
];

/// Whether one inherited snapshot name survives the allowlist (step 1).
///
/// This is the allowlist half of the policy and the primary defense: a name this
/// returns `false` for never reaches the child through the inherited snapshot,
/// however new or unreviewed it is. It says nothing about
/// [`EnvironmentSpec::set`], which bypasses step 1 entirely, and nothing about
/// the two denylist passes that run afterwards — see
/// [`subscription_policy_removes`] and [`transparent_profile_removes`].
///
/// Case-sensitive by construction; see the module note.
///
/// [`EnvironmentSpec::set`]: super::EnvironmentSpec::set
#[must_use]
pub fn inherits(name: &str, auth_policy: AuthPolicy) -> bool {
    if INHERITED_EXACT_KEYS.contains(&name)
        || INHERITED_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
    {
        return true;
    }
    // Under `Subscription` these remain denied here *and* removed by the
    // policy pass, which is the point of keeping both mechanisms.
    auth_policy == AuthPolicy::Inherit
        && (SUBSCRIPTION_AUTH_KEYS.contains(&name)
            || PROVIDER_ROUTING_EXACT_KEYS.contains(&name)
            || PROVIDER_ROUTING_PREFIXES
                .iter()
                .any(|prefix| name.starts_with(prefix)))
}

/// Whether the auth policy removes one name at step 4, after the caller's
/// explicit `set`.
///
/// Belt and braces with [`inherits`]: a name that is both allowed and explicitly
/// forbidden is still removed, and a name restored through `set` is still
/// stripped. Always `false` under [`AuthPolicy::Inherit`].
#[must_use]
pub fn subscription_policy_removes(name: &str, auth_policy: AuthPolicy) -> bool {
    auth_policy == AuthPolicy::Subscription && SUBSCRIPTION_AUTH_KEYS.contains(&name)
}

/// Whether the transparent terminal profile removes one name at step 5.
///
/// Runs after the caller's explicit `set`, so [`super::EnvironmentSpec::set`]
/// cannot restore anything this returns `true` for. Case-sensitive in both forms.
#[must_use]
pub fn transparent_profile_removes(name: &str) -> bool {
    TRANSPARENT_EXACT_KEYS.contains(&name)
        || TRANSPARENT_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
}
