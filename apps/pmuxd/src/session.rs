use crate::util::parse_logging_mode;
use anyhow::{Context, Result};
use pseudomux_adapters::{AgentKind, ClaudeCodeOpts, LaunchConfig};
use pseudomux_core::session::state::TerminalSize;
use pseudomux_protocol::{SessionId, StartSessionParams};
use pseudomux_service::Service;
use pseudomux_service::profile::{ProfileSpec, load_profile, toml_to_json};
use std::path::PathBuf;

pub(crate) fn start_session(service: &Service, params: StartSessionParams) -> Result<SessionId> {
    let profile = match params.profile.as_deref() {
        Some(name) => load_profile(name)
            .with_context(|| format!("failed loading profile '{name}'"))?
            .ok_or_else(|| anyhow::anyhow!("profile '{name}' not found"))?,
        None => ProfileSpec {
            agent: None,
            cwd: None,
            rows: None,
            cols: None,
            logging_mode: None,
            args: Vec::new(),
            env: Vec::new(),
            model: None,
            permission_mode: None,
            allowed_tools: None,
            disallowed_tools: None,
            system_prompt: None,
            append_system_prompt: None,
            effort: None,
            max_budget: None,
            extra_args: Vec::new(),
            settings: None,
        },
    };

    let mut cfg = LaunchConfig::default();
    if let Some(cwd) = profile.cwd {
        cfg.cwd = Some(cwd);
    }
    if let Some(cwd) = params.cwd {
        cfg.cwd = Some(PathBuf::from(cwd));
    }
    cfg.size = TerminalSize {
        rows: params.rows.or(profile.rows).unwrap_or(24),
        cols: params.cols.or(profile.cols).unwrap_or(80),
    };
    cfg.env = merge_env(profile.env, params.env);
    if let Some(mode) = params.logging_mode {
        cfg.logging = parse_logging_mode(&mode);
    } else if let Some(mode) = profile.logging_mode {
        cfg.logging = parse_logging_mode(&mode);
    }
    if let Some(ref rp) = params.record_path {
        cfg.record_path = Some(PathBuf::from(rp));
    }
    let profile_name = params.profile.clone();

    let effective_agent = params
        .agent
        .as_deref()
        .or(profile.agent.as_deref())
        .map(str::to_string);
    let is_claude = effective_agent
        .as_deref()
        .map(|a| {
            matches!(
                a.to_lowercase().as_str(),
                "claude-code" | "claude" | "claudecode"
            )
        })
        .unwrap_or(false);

    let mut profile_convenience_args: Vec<String> = Vec::new();
    if is_claude {
        let settings_json = if let Some(settings) = &profile.settings {
            Some(
                serde_json::to_string(&toml_to_json(settings))
                    .context("failed to serialize profile settings to JSON")?,
            )
        } else {
            None
        };
        let claude_opts = ClaudeCodeOpts {
            model: profile.model.clone(),
            permission_mode: profile.permission_mode.clone(),
            allowed_tools: profile.allowed_tools.clone(),
            disallowed_tools: profile.disallowed_tools.clone(),
            system_prompt: profile.system_prompt.clone(),
            append_system_prompt: profile.append_system_prompt.clone(),
            effort: profile.effort.clone(),
            max_budget: profile.max_budget,
            settings_json,
        };
        profile_convenience_args.extend(claude_opts.to_args());
    }

    let args = if params.args.is_empty()
        && profile_convenience_args.is_empty()
        && profile.extra_args.is_empty()
    {
        profile.args
    } else {
        let mut merged = profile_convenience_args;
        merged.extend(profile.extra_args);
        merged.extend(params.args);
        merged
    };

    let (agent_name, kind, cfg_args) = build_agent(params.agent, profile.agent, args);
    cfg.profile = profile_name;
    cfg.agent_name = Some(agent_name);
    cfg.args = cfg_args;
    service.start(kind, cfg, params.name)
}

pub(crate) fn merge_env(
    defaults: Vec<(String, String)>,
    overrides: Vec<(String, String)>,
) -> Vec<(String, String)> {
    let mut map = std::collections::BTreeMap::<String, String>::new();
    for (k, v) in defaults {
        map.insert(k, v);
    }
    for (k, v) in overrides {
        map.insert(k, v);
    }
    map.into_iter().collect()
}

pub(crate) fn build_agent(
    request_agent: Option<String>,
    profile_agent: Option<String>,
    args: Vec<String>,
) -> (String, AgentKind, Vec<String>) {
    let agent = request_agent
        .or(profile_agent)
        .unwrap_or_else(|| "shell".to_string());
    let kind: AgentKind = agent.parse().unwrap();
    let cfg_args = match &kind {
        AgentKind::Custom { .. } => Vec::new(),
        _ => args,
    };
    (agent, kind, cfg_args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_env_overrides_by_key() {
        let merged = merge_env(
            vec![("A".to_string(), "1".to_string())],
            vec![
                ("A".to_string(), "2".to_string()),
                ("B".to_string(), "3".to_string()),
            ],
        );
        assert_eq!(
            merged,
            vec![
                ("A".to_string(), "2".to_string()),
                ("B".to_string(), "3".to_string())
            ]
        );
    }

    #[test]
    fn build_agent_uses_profile_when_request_missing() {
        let (agent, kind, args) =
            build_agent(None, Some("opencode".to_string()), vec!["--help".into()]);
        assert_eq!(agent, "opencode");
        assert!(matches!(kind, AgentKind::OpenCode));
        assert_eq!(args, vec!["--help".to_string()]);
    }
}
