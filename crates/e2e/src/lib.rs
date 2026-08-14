//! Shared support for tracked, publish-disabled shipped-binary validation.
//!
//! This crate is never part of pmux's runtime. Its tests launch exact candidate
//! binaries in owner-only sandboxes and use a deterministic interactive test
//! double for the external Claude process.

/// Version emitted by the deterministic external-process test double.
pub const TEST_CLAUDE_VERSION: &str = "9.9.9";

/// Version of the launch/environment evidence schema emitted by the test double.
pub const TEST_ATTESTATION_VERSION: u64 = 1;

/// Synthetic values used to prove environment patch ordering at the exec boundary.
pub const TEST_ENV_ATTESTATION_MARKER: &str = "pmux-e2e-environment-v1";
pub const TEST_ENV_PATCHED_VALUE: &str = "set-wins-after-unset";
pub const TEST_ENV_SET_ONLY_VALUE: &str = "set-only-value";
pub const TEST_ENV_SAFE_CONFIG_VALUE: &str = "caller-config-preserved";

/// Synthetic secrets are deliberately recognizable but never recorded by the child.
pub const TEST_ANTHROPIC_SECRET: &str = "pmux-e2e-anthropic-secret-do-not-log";
pub const TEST_PROVIDER_SECRET: &str = "pmux-e2e-provider-secret-do-not-log";
pub const TEST_LAUNCH_SECRET: &str = "pmux-e2e-inline-launch-secret-do-not-log";

/// Environment names that subscription policy must remove after snapshot patching.
pub const TEST_SUBSCRIPTION_KEYS: &[&str] = &[
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

/// Exact parent/terminal identity names that the transparent profile removes.
pub const TEST_TRANSPARENT_EXACT_KEYS: &[&str] = &[
    "RMUX",
    "TMUX",
    "TMUX_PANE",
    "TMUX_PROGRAM",
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_REMOTE",
    // A parent Claude Code session exports this to mark a nested invocation.
    // Inheriting it made the real child render a composer, accept the bracketed
    // paste and the Enter, and then never write a transcript of its own, so
    // every turn died at `awaiting_prompt_ack` against Claude 2.1.215 and
    // 2.1.220. This list is deliberately an INDEPENDENT oracle rather than an
    // import of the daemon's policy, so the full-stack lane proves the shipped
    // child never sees the name. Omitting it meant the lane never exercised
    // that fix end to end.
    "CLAUDE_CODE_CHILD_SESSION",
];

/// Parent/SDK identity prefixes that the transparent profile removes.
pub const TEST_TRANSPARENT_PREFIXES: &[&str] =
    &["RMUX", "TMUX", "CLAUDE_AGENT_SDK_", "CLAUDE_CODE_SDK_"];
