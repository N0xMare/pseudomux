use pseudomux_adapters::{AgentKind, LaunchConfig, to_start_spec};

#[test]
fn test_to_start_spec_injects_env() {
    let spec = to_start_spec(AgentKind::Shell, LaunchConfig::default());
    let term = spec
        .env
        .iter()
        .find(|(k, _)| k == "TERM")
        .map(|(_, v)| v.as_str());
    let color = spec
        .env
        .iter()
        .find(|(k, _)| k == "COLORTERM")
        .map(|(_, v)| v.as_str());
    assert_eq!(term, Some("xterm-256color"));
    assert_eq!(color, Some("truecolor"));
}

#[test]
fn test_to_start_spec_user_env_precedence() {
    let cfg = LaunchConfig {
        env: vec![("TERM".into(), "dumb".into())],
        ..Default::default()
    };
    let spec = to_start_spec(AgentKind::Shell, cfg);
    let term = spec
        .env
        .iter()
        .find(|(k, _)| k == "TERM")
        .map(|(_, v)| v.as_str());
    assert_eq!(term, Some("dumb"));
}

#[test]
fn test_to_start_spec_sets_profile_name() {
    let spec = to_start_spec(AgentKind::OpenCode, LaunchConfig::default());
    assert_eq!(spec.input_profile_name.as_deref(), Some("opencode"));
}

#[test]
fn test_to_start_spec_claude_code() {
    let spec = to_start_spec(AgentKind::ClaudeCode, LaunchConfig::default());
    assert_eq!(spec.input_profile_name.as_deref(), Some("claude_code"));
    assert_eq!(spec.program, "claude");
}
