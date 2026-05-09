use pseudomux_adapters::KeyEvent;
use pseudomux_adapters::input_profile::{InputProfile, KeyboardPolicy};

#[test]
fn test_shell_profile_has_submit() {
    let p = InputProfile::shell();
    assert_eq!(p.action_keys("submit"), Some(&vec![KeyEvent::Enter]));
}

#[test]
fn test_opencode_profile_deny_kitty() {
    let p = InputProfile::opencode();
    assert_eq!(p.capability_policy.keyboard, KeyboardPolicy::Deny);
}

#[test]
fn test_opencode_profile_actions() {
    let p = InputProfile::opencode();
    assert!(p.action_keys("submit").is_some());
    assert!(p.action_keys("interrupt").is_some());
    assert!(p.action_keys("command_palette").is_some());
    assert!(p.action_keys("variants").is_some());
    assert!(p.action_keys("agents").is_some());
}

#[test]
fn test_opencode_profile_fallback() {
    let p = InputProfile::opencode();
    assert_eq!(p.fallback_keys("submit"), Some(&vec![KeyEvent::Ctrl('s')]));
}

#[test]
fn test_claude_code_profile_actions() {
    let p = InputProfile::claude_code();
    assert!(p.action_keys("submit").is_some());
    assert!(p.action_keys("interrupt").is_some());
    assert_eq!(p.action_keys("submit"), Some(&vec![KeyEvent::Enter]));
    assert_eq!(p.action_keys("interrupt"), Some(&vec![KeyEvent::Escape]));
    assert!(p.action_keys("hard_interrupt").is_some());
    assert_eq!(
        p.action_keys("hard_interrupt"),
        Some(&vec![KeyEvent::Ctrl('c')])
    );
}

#[test]
fn test_claude_code_profile_deny_kitty() {
    let p = InputProfile::claude_code();
    assert_eq!(p.capability_policy.keyboard, KeyboardPolicy::Deny);
}

#[test]
fn test_bubbletea_profile_defaults() {
    let p = InputProfile::bubbletea_generic();
    assert_eq!(p.capability_policy.keyboard, KeyboardPolicy::Deny);
    assert!(p.action_keys("submit").is_some());
    assert!(p.action_keys("interrupt").is_some());
}

#[test]
fn test_action_keys_lookup() {
    let p = InputProfile::shell();
    assert!(p.action_keys("submit").is_some());
    assert!(p.action_keys("nonexistent").is_none());
}

#[test]
fn test_env_defaults() {
    for p in [
        InputProfile::shell(),
        InputProfile::opencode(),
        InputProfile::claude_code(),
        InputProfile::bubbletea_generic(),
    ] {
        let term = p
            .env
            .iter()
            .find(|(k, _)| k == "TERM")
            .map(|(_, v)| v.as_str());
        let color = p
            .env
            .iter()
            .find(|(k, _)| k == "COLORTERM")
            .map(|(_, v)| v.as_str());
        assert_eq!(term, Some("xterm-256color"));
        assert_eq!(color, Some("truecolor"));
    }
}
