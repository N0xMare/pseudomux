pub mod claude_code;
pub mod opencode;
pub mod shell;

pub mod input_profile;

pub use input_profile::{CapabilityPolicy, InputProfile, KeyEvent, KeyboardPolicy};

use pseudomux_core::session::StartSpec;
use pseudomux_core::session::state::{LoggingMode, ScrollbackConfig, TerminalSize};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub enum AgentKind {
    Shell,
    OpenCode,
    ClaudeCode,
    Custom { program: String, args: Vec<String> },
}

pub struct LaunchConfig {
    pub profile: Option<String>,
    pub agent_name: Option<String>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub size: TerminalSize,
    pub scrollback: ScrollbackConfig,
    pub logging: LoggingMode,
    pub args: Vec<String>,
    pub record_path: Option<PathBuf>,
}

impl Default for LaunchConfig {
    fn default() -> Self {
        Self {
            profile: None,
            agent_name: None,
            cwd: None,
            env: vec![],
            size: TerminalSize { rows: 24, cols: 80 },
            scrollback: ScrollbackConfig::default(),
            logging: LoggingMode::Metadata,
            args: vec![],
            record_path: None,
        }
    }
}

pub fn to_start_spec(kind: AgentKind, cfg: LaunchConfig) -> StartSpec {
    let input_profile = match &kind {
        AgentKind::Shell => Some(InputProfile::shell()),
        AgentKind::OpenCode => Some(InputProfile::opencode()),
        AgentKind::ClaudeCode => Some(InputProfile::claude_code()),
        AgentKind::Custom { .. } => None,
    };

    let capability_policy_keyboard =
        input_profile
            .as_ref()
            .map(|p| match p.capability_policy.keyboard {
                KeyboardPolicy::Deny => "deny".to_string(),
                KeyboardPolicy::Accept => "accept".to_string(),
                KeyboardPolicy::PassThrough => "passthrough".to_string(),
            });

    let input_profile_name = match &kind {
        AgentKind::Shell => Some("shell".to_string()),
        AgentKind::OpenCode => Some("opencode".to_string()),
        AgentKind::ClaudeCode => Some("claude_code".to_string()),
        AgentKind::Custom { .. } => None,
    };

    let agent = cfg.agent_name.unwrap_or_else(|| match &kind {
        AgentKind::Shell => "shell".to_string(),
        AgentKind::OpenCode => "opencode".to_string(),
        AgentKind::ClaudeCode => "claude-code".to_string(),
        AgentKind::Custom { program, .. } => program.clone(),
    });

    let agent_kind_str = match &kind {
        AgentKind::OpenCode => Some("opencode".to_string()),
        AgentKind::ClaudeCode => Some("claude-code".to_string()),
        _ => None,
    };

    // Merge input profile env into cfg env (profile fills in unset keys)
    let mut env = cfg.env;
    if let Some(ref profile) = input_profile {
        for (k, v) in &profile.env {
            if !env.iter().any(|(ek, _)| ek == k) {
                env.push((k.clone(), v.clone()));
            }
        }
    }

    let env_remove = match &kind {
        AgentKind::ClaudeCode => vec![
            "CLAUDECODE".to_string(),
            "CLAUDE_CODE_ENTRYPOINT".to_string(),
        ],
        _ => vec![],
    };

    let mut base = StartSpec {
        profile: cfg.profile,
        agent,
        program: default_shell(),
        args: cfg.args,
        env,
        cwd: cfg.cwd,
        size: cfg.size,
        scrollback: cfg.scrollback,
        logging: cfg.logging,
        log_dir_base: None,
        agent_kind: agent_kind_str.clone(),
        capability_policy_keyboard,
        input_profile_name,
        env_remove,
        adapter: adapter_for(agent_kind_str.as_deref().unwrap_or("shell")).map(Arc::from),
        record_path: cfg.record_path,
        name: None,
    };

    match kind {
        AgentKind::Shell => {}
        AgentKind::OpenCode => {
            base.program = "opencode".to_string();
        }
        AgentKind::ClaudeCode => {
            base.program = "claude".to_string();
        }
        AgentKind::Custom { program, args } => {
            base.program = program;
            base.args = args;
        }
    }
    base
}

fn default_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string())
}

impl std::str::FromStr for AgentKind {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.to_lowercase().as_str() {
            "shell" => AgentKind::Shell,
            "opencode" => AgentKind::OpenCode,
            "claude-code" | "claude" | "claudecode" => AgentKind::ClaudeCode,
            other => AgentKind::Custom {
                program: other.to_string(),
                args: vec![],
            },
        })
    }
}

/// Claude Code configuration options that map to CLI flags.
#[derive(Default, Clone, Debug)]
pub struct ClaudeCodeOpts {
    pub model: Option<String>,
    pub permission_mode: Option<String>,
    pub allowed_tools: Option<String>,
    pub disallowed_tools: Option<String>,
    pub system_prompt: Option<String>,
    pub append_system_prompt: Option<String>,
    pub effort: Option<String>,
    pub max_budget: Option<f64>,
    pub settings_json: Option<String>,
}

impl ClaudeCodeOpts {
    /// Convert to CLI argument vector for Claude Code.
    pub fn to_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if let Some(m) = &self.model {
            args.push("--model".to_string());
            args.push(m.clone());
        }
        if let Some(pm) = &self.permission_mode {
            args.push("--permission-mode".to_string());
            args.push(pm.clone());
        }
        if let Some(at) = &self.allowed_tools {
            args.push("--allowedTools".to_string());
            args.push(at.clone());
        }
        if let Some(dt) = &self.disallowed_tools {
            args.push("--disallowedTools".to_string());
            args.push(dt.clone());
        }
        if let Some(sp) = &self.system_prompt {
            args.push("--system-prompt".to_string());
            args.push(sp.clone());
        }
        if let Some(asp) = &self.append_system_prompt {
            args.push("--append-system-prompt".to_string());
            args.push(asp.clone());
        }
        if let Some(e) = &self.effort {
            args.push("--effort".to_string());
            args.push(e.clone());
        }
        if let Some(mb) = &self.max_budget {
            args.push("--max-budget-usd".to_string());
            args.push(mb.to_string());
        }
        if let Some(sj) = &self.settings_json {
            args.push("--settings".to_string());
            args.push(sj.clone());
        }
        args
    }
}

pub use pseudomux_core::adapter::TuiAdapter;

/// Look up a TUI adapter by name.
pub fn adapter_for(name: &str) -> Option<Box<dyn TuiAdapter>> {
    match name.to_lowercase().as_str() {
        "claude-code" | "claude" | "claudecode" => Some(Box::new(claude_code::ClaudeCodeAdapter)),
        "opencode" => Some(Box::new(opencode::OpenCodeAdapter)),
        "shell" | "bash" | "zsh" | "fish" => Some(Box::new(shell::ShellAdapter)),
        _ => None,
    }
}
