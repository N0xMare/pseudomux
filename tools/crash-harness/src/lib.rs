//! Shared fixture and kill primitive for the SIGKILL harness.

use pseudomux_protocol::v1::{
    AgentContainment, AgentEnvironmentSpec, AgentSpec, AuthPolicy, ClaudeLaunchConfig,
    CompatibilityPolicy, InputTransport, LifecycleMode, RetentionPolicy, SessionCell,
    SystemPromptPolicy, TerminalProfile, TerminalSpec,
};

/// One admissible agent, with a system prompt long enough that publishing it is
/// several pages of I/O and a kill can land inside the write rather than only
/// between calls.
#[must_use]
pub fn spec(marker: u64) -> AgentSpec {
    AgentSpec {
        name: format!("reviewer-{marker}"),
        description: Some("reads and reports".into()),
        claude: ClaudeLaunchConfig {
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
                prompt: "x".repeat(8_000 + usize::try_from(marker % 512).unwrap_or(0)),
            },
            extra_args: Vec::new(),
        },
        environment: AgentEnvironmentSpec::default(),
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

pub const NOW: u64 = 1_700_000_000_000;

/// SIGKILL, and deliberately not `Child::kill`'s SIGKILL-then-reap: nothing may
/// run in the child after this, not a destructor and not a signal handler. That
/// is the whole point -- a `SIGTERM` the child could clean up after would test
/// the cleanup rather than the crash.
#[allow(unsafe_code)]
pub fn kill9(child: &std::process::Child) {
    // SAFETY: `kill` takes two integers and dereferences nothing.
    unsafe {
        libc::kill(i32::try_from(child.id()).expect("pid fits"), libc::SIGKILL);
    }
}

/// A xorshift64 stream, so the kill offset is spread across a whole update
/// cycle instead of clustering.
///
/// The first version of this harness took the offset from
/// `SystemTime::subsec_nanos() % 4000`, which on this host put every kill inside
/// the first few milliseconds of the loop and found ZERO wedges in 40 trials
/// against a store that wedges 37% of the time. A harness that samples one
/// phase of a cycle is a harness that confirms whatever that phase does.
pub struct Jitter(u64);

impl Jitter {
    #[must_use]
    pub fn from_clock() -> Self {
        Self(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0x2545_F491_4F6C_DD1D, |elapsed| {
                    u64::from(elapsed.subsec_nanos()) | 1
                }),
        )
    }

    pub fn next(&mut self, modulus: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % modulus.max(1)
    }
}
