use pseudomux_service::profile::{load_profile, profile_candidates, toml_to_json};
use std::fs;
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[allow(unsafe_code)]
fn with_profile_file<T>(path: &std::path::Path, f: impl FnOnce() -> T) -> T {
    let _guard = ENV_LOCK.lock().expect("env lock");
    unsafe { std::env::set_var("PSEUDOMUX_PROFILE_FILE", path.as_os_str()) };
    let result = f();
    unsafe { std::env::remove_var("PSEUDOMUX_PROFILE_FILE") };
    result
}

#[test]
fn parse_profile_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let profile_path = dir.path().join("profiles.toml");
    fs::write(
        &profile_path,
        r#"
[profiles.opencode_default]
agent = "opencode"
cwd = "/workspace"
rows = 40
cols = 120
logging_mode = "metadata"
args = ["--model", "openai/gpt-5.1-codex-mini"]

[profiles.opencode_default.env]
FOO = "bar"
"#,
    )
    .expect("write profile");

    let spec = with_profile_file(&profile_path, || {
        load_profile("opencode_default")
            .expect("load ok")
            .expect("profile found")
    });

    assert_eq!(spec.agent.as_deref(), Some("opencode"));
    assert_eq!(spec.rows, Some(40));
    assert_eq!(spec.cols, Some(120));
    let foo = spec
        .env
        .iter()
        .find(|(k, _)| k == "FOO")
        .map(|(_, v)| v.as_str());
    assert_eq!(foo, Some("bar"));
}

#[test]
fn parse_claude_code_profile() {
    let dir = tempfile::tempdir().expect("tempdir");
    let profile_path = dir.path().join("profiles.toml");
    fs::write(
        &profile_path,
        r#"
[profiles.reviewer]
agent = "claude-code"
model = "opus"
permission_mode = "default"
system_prompt = "You are a code reviewer."
effort = "high"
allowed_tools = "Read Bash(git:*)"
extra_args = ["--betas", "interleaved-thinking"]

[profiles.reviewer.env]
CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC = "1"

[profiles.reviewer.settings]
model = "claude-opus-4-6"

[profiles.reviewer.settings.permissions]
allow = ["Read", "Bash(git:*)"]
deny = ["Edit", "Write"]
"#,
    )
    .expect("write profile");

    let spec = with_profile_file(&profile_path, || {
        load_profile("reviewer")
            .expect("load ok")
            .expect("profile found")
    });

    assert_eq!(spec.agent.as_deref(), Some("claude-code"));
    assert_eq!(spec.model.as_deref(), Some("opus"));
    assert_eq!(spec.permission_mode.as_deref(), Some("default"));
    assert_eq!(spec.effort.as_deref(), Some("high"));
    assert!(spec.settings.is_some());
    assert_eq!(spec.extra_args.len(), 2);
}

#[test]
fn toml_to_json_converts_table() {
    let toml_val: toml::Value = toml::from_str(
        r#"
model = "claude-opus-4-6"
[permissions]
allow = ["Read"]
deny = ["Edit"]
"#,
    )
    .unwrap();
    let json = toml_to_json(&toml_val);
    assert_eq!(json["model"], serde_json::json!("claude-opus-4-6"));
    assert_eq!(json["permissions"]["allow"][0], serde_json::json!("Read"));
}

#[test]
fn profile_candidates_returns_paths() {
    let candidates = profile_candidates();
    // Just verify the function runs without panic
    let _ = candidates;
}
