use pseudomux_adapters::{AgentKind, LaunchConfig};
use pseudomux_core::session::state::SessionStatus;
use pseudomux_service::Service;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::thread::sleep;
use std::time::{Duration, Instant};

fn write_mock_opencode(dir: &Path) -> anyhow::Result<()> {
    let path = dir.join("opencode");
    let script = r#"#!/usr/bin/env bash
set -euo pipefail
stty raw -echo
trap 'stty sane' EXIT
printf 'Ask anything\n'
buf=''
esc_buf=''
while true; do
  byte="$(dd bs=1 count=1 2>/dev/null | od -An -t u1 | tr -d '[:space:]')"
  if [[ -z "$byte" ]]; then
    break
  fi
  if [[ "$byte" == "27" ]]; then
    esc_buf=$'\x1b'
    continue
  fi
  if [[ -n "$esc_buf" ]]; then
    printf -v oct '%03o' "$byte"
    printf -v ch "\\$oct"
    esc_buf+="$ch"
    if [[ "$esc_buf" == $'\x1b[13u' ]]; then
      esc_buf=''
      if [[ -n "$buf" ]]; then
        printf 'PROMPT:%s\n' "$buf"
        if [[ "$buf" == *"SENTINEL_TEST"* ]]; then
          printf '__PMUX_DONE__\n'
        fi
        buf=''
      fi
      continue
    fi
    if [[ ${#esc_buf} -ge 5 ]]; then
      esc_buf=''
    fi
    continue
  fi
  if [[ "$byte" == "20" ]]; then
    printf 'KEY:CTRL_T\n'
    continue
  fi
  if [[ "$byte" == "13" || "$byte" == "10" ]]; then
    if [[ -n "$buf" ]]; then
      printf 'PROMPT:%s\n' "$buf"
      if [[ "$buf" == *"SENTINEL_TEST"* ]]; then
        printf '__PMUX_DONE__\n'
      fi
      buf=''
    fi
    continue
  fi
  printf -v oct '%03o' "$byte"
  printf -v ch "\\$oct"
  buf+="$ch"
done
"#;
    fs::write(&path, script)?;
    let mut perms = fs::metadata(&path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms)?;
    Ok(())
}

fn read_until(
    service: &Service,
    id: pseudomux_core::session::state::SessionId,
    pattern: &str,
    timeout: Duration,
) -> String {
    let start = Instant::now();
    let mut seq = 0u64;
    let mut out = String::new();
    while start.elapsed() < timeout {
        if let Ok((chunks, next_seq)) = service.read_since(id, seq) {
            for chunk in chunks {
                out.push_str(&String::from_utf8_lossy(&chunk.bytes));
            }
            if out.contains(pattern) {
                return out;
            }
            seq = next_seq.saturating_sub(1);
        }
        sleep(Duration::from_millis(50));
    }
    out
}

#[test]
fn opencode_adapter_contract_start_ready_key_prompt_teardown() {
    let temp = tempfile::tempdir().expect("tempdir");
    write_mock_opencode(temp.path()).expect("mock opencode");
    let original_path = std::env::var("PATH").unwrap_or_default();
    let path_override = format!("{}:{original_path}", temp.path().display());

    let service = Service::new().unwrap();
    let mut cfg = LaunchConfig::default();
    cfg.env.push(("PATH".to_string(), path_override));
    cfg.logging = pseudomux_core::session::state::LoggingMode::Metadata;

    let sid = service
        .start(AgentKind::OpenCode, cfg, None)
        .expect("start session");

    let ready = read_until(&service, sid, "Ask anything", Duration::from_secs(3));
    assert!(
        ready.contains("Ask anything"),
        "did not observe readiness output: {ready:?}"
    );

    service
        .send_bytes(sid, &[0x14])
        .expect("send ctrl-t key to mock opencode");
    let key_echo = read_until(&service, sid, "KEY:CTRL_T", Duration::from_secs(2));
    assert!(
        key_echo.contains("KEY:CTRL_T"),
        "did not observe key injection output: {key_echo:?}"
    );

    service
        .send_text(sid, "SENTINEL_TEST")
        .expect("send prompt");
    service.send_enter(sid).expect("send enter");
    let prompt_echo = read_until(
        &service,
        sid,
        "PROMPT:SENTINEL_TEST",
        Duration::from_secs(2),
    );
    assert!(
        prompt_echo.contains("PROMPT:SENTINEL_TEST"),
        "did not observe prompt echo: {prompt_echo:?}"
    );
    let sentinel = read_until(&service, sid, "__PMUX_DONE__", Duration::from_secs(2));
    assert!(
        sentinel.contains("__PMUX_DONE__"),
        "did not observe sentinel output: {sentinel:?}"
    );

    service.terminate(sid).expect("terminate session");
    let state = service.state(sid).expect("session state available");
    assert_eq!(state.status, SessionStatus::Exited);
}
