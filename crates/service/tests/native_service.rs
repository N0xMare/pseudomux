#![cfg(unix)]

mod process_support;

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use process_support::CandidateFiles;
use pseudomux_protocol::v1::{
    AuthPolicy, ClaudeLaunchConfig, CompatibilityPolicy, ConfigSource, DisconnectAction,
    EnvironmentSpec, ErrorCode, InputTransport, LifecycleMode, MAX_SAFE_JSON_INTEGER,
    PermissionMode, Request, RetentionPolicy, RunOnceRequest, SessionCell, SessionIdentity,
    StartSessionRequest, SystemPromptPolicy, TerminalProfile, TerminalSpec, TurnLeasePolicy,
    TurnRequest,
};
use pseudomux_service::compatibility::TestedCompatibilityProfile;
use pseudomux_service::native::{NativeService, NativeServiceConfig};
use pseudomux_service::runtime::PrivateRuntimeConfig;
use tempfile::TempDir;
use uuid::Uuid;

#[tokio::test]
#[ignore = "builds companion binaries and starts a real private rmux PTY, but never calls Claude"]
async fn native_run_once_uses_rmux_and_transcript_authority_end_to_end() {
    let candidates = CandidateFiles::discover(&["pmux-rmuxd", "pmux-launcher"]).unwrap();
    let rmuxd = candidates.path("pmux-rmuxd").to_path_buf();
    let launcher = candidates.path("pmux-launcher").to_path_buf();

    let root = TempDir::new().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let cwd = root.path().join("workspace");
    let config_root = root.path().join("claude-config");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(config_root.join("projects/fake")).unwrap();
    let fake_claude = root.path().join("fake-claude");
    let input_proof = config_root.join("fake-input-proof");
    write_fake_claude(&fake_claude);

    let mut service_config = NativeServiceConfig::default();
    service_config
        .tested_claude_profiles
        .insert(TestedCompatibilityProfile {
            claude_version: "9.9.9".to_owned(),
            claude_version_tested_through: None,
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            terminal_profile: TerminalProfile::Transparent,
            input_transport: InputTransport::Sdk,
            transcript_drain_ms: 50,
        })
        .unwrap();
    service_config.readiness_timeout = Duration::from_secs(5);
    service_config.actor.default_turn_timeout_ms = 10_000;
    let service = NativeService::start(
        PrivateRuntimeConfig {
            rmuxd,
            launcher,
            runtime_parent: Some(root.path().to_path_buf()),
            startup_timeout: Duration::from_secs(5),
            operation_timeout: Duration::from_secs(5),
            lease_ttl: Duration::from_secs(5),
        },
        service_config,
    )
    .await
    .unwrap();

    let session_id = Uuid::new_v4();
    let run_result = service
        .run_once(RunOnceRequest {
            session: StartSessionRequest {
                identity: SessionIdentity::New {
                    session_id: Some(session_id),
                },
                cwd: cwd.canonicalize().unwrap().to_string_lossy().into_owned(),
                agent: None,
                claude: Some(ClaudeLaunchConfig {
                    executable: fake_claude
                        .canonicalize()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    model: None,
                    effort: None,
                    permission_mode: Some(PermissionMode::Default),
                    allowed_tools: Vec::new(),
                    denied_tools: Vec::new(),
                    settings: Vec::new(),
                    mcp_configs: Vec::new(),
                    plugin_dirs: Vec::new(),
                    system_prompt: SystemPromptPolicy::Default,
                    extra_args: Vec::new(),
                }),
                environment: EnvironmentSpec {
                    snapshot: BTreeMap::from([
                        (
                            "HOME".to_owned(),
                            root.path().to_string_lossy().into_owned(),
                        ),
                        (
                            "CLAUDE_CONFIG_DIR".to_owned(),
                            config_root.to_string_lossy().into_owned(),
                        ),
                        ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
                    ]),
                    set: BTreeMap::new(),
                    unset: BTreeSet::new(),
                },
                auth_policy: AuthPolicy::Inherit,
                config_isolation: None,
                terminal: TerminalSpec {
                    rows: 24,
                    cols: 120,
                    profile: TerminalProfile::Transparent,
                    input_transport: InputTransport::Sdk,
                },
                lifecycle: LifecycleMode::Transcript,
                retention: RetentionPolicy::OneShot,
                compatibility: CompatibilityPolicy::RequireTested,
                cell: SessionCell::Full,
            },
            turn: TurnRequest {
                turn_id: Uuid::new_v4(),
                prompt: "hello".to_owned(),
                deadline_unix_ms: Some(now_ms() + 10_000),
                lease: TurnLeasePolicy {
                    on_disconnect: DisconnectAction::Continue,
                    heartbeat_timeout_ms: None,
                },
            },
        })
        .await;
    let result = match run_result {
        Ok(result) => result,
        Err(error) => {
            let proof = std::fs::read_to_string(&input_proof)
                .unwrap_or_else(|proof_error| format!("<unavailable: {proof_error}>"));
            let _ = service.shutdown().await;
            panic!("adversarial fake-TUI run failed: {error:?}; proof stages: {proof:?}");
        }
    };

    assert_eq!(result.session_id, session_id);
    assert_eq!(result.text, "world");
    assert_eq!(result.model.as_deref(), Some("fake-model"));
    assert_eq!(result.usage.main.input_tokens, 3);
    assert_eq!(result.usage.main.output_tokens, 1);
    assert!(result.completion.prompt_acknowledged);
    assert!(result.completion.terminal_message_observed);
    assert!(result.completion.terminal_prompt_observed);
    assert!(result.completion.terminal_quiet_observed);
    assert!(result.completion.transcript_drained);
    assert!(result.compatibility.tested);
    assert_eq!(result.compatibility.transcript_drain_ms, 50);
    assert_eq!(
        std::fs::read_to_string(&input_proof).unwrap(),
        concat!(
            "transient_ready_rendered\n",
            "no_input_before_stable_ready\n",
            "stable_ready_rendered\n",
            "bracketed_paste_exact\n",
            "no_enter_before_paste_render\n",
            "pasted_editor_rendered\n",
            "single_enter_exact\n",
            "no_second_input_byte\n",
            "transcript_written\n",
        ),
        "the fake TUI did not prove every input-ordering stage"
    );
    service.shutdown().await.unwrap();
    candidates.assert_unchanged().unwrap();
}

#[tokio::test]
#[ignore = "starts the private rmux sidecar but proves invalid direct Rust DTOs cannot launch Claude"]
async fn direct_native_request_preflight_precedes_files_version_process_and_actor_side_effects() {
    let candidates = CandidateFiles::discover(&["pmux-rmuxd", "pmux-launcher"]).unwrap();
    let rmuxd = candidates.path("pmux-rmuxd").to_path_buf();
    let launcher = candidates.path("pmux-launcher").to_path_buf();

    let root = TempDir::new().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let cwd = root.path().join("workspace");
    let config_root = root.path().join("claude-config");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(&config_root).unwrap();
    let marker = root.path().join("version-invoked");
    let fake_claude = root.path().join("version-probe-claude");
    write_version_probe(&fake_claude);

    let service = NativeService::start(
        PrivateRuntimeConfig {
            rmuxd,
            launcher,
            runtime_parent: Some(root.path().to_path_buf()),
            startup_timeout: Duration::from_secs(5),
            operation_timeout: Duration::from_secs(5),
            lease_ttl: Duration::from_secs(5),
        },
        NativeServiceConfig::default(),
    )
    .await
    .unwrap();
    let baseline = relative_paths(root.path());

    let mut typed_inline = preflight_start_request(&fake_claude, &cwd, &config_root, &marker);
    typed_inline
        .claude
        .as_mut()
        .expect("inline launch")
        .settings = vec![ConfigSource::Inline {
        document: serde_json::json!({"nested": [MAX_SAFE_JSON_INTEGER + 1]}),
    }];
    assert_preflight_rejection(service.start_session(typed_inline).await.unwrap_err());

    let mut dispatched_inline = preflight_start_request(&fake_claude, &cwd, &config_root, &marker);
    dispatched_inline
        .claude
        .as_mut()
        .expect("inline launch")
        .mcp_configs = vec![ConfigSource::Inline {
        document: serde_json::json!({
            "nested": [-(MAX_SAFE_JSON_INTEGER as i64) - 1]
        }),
    }];
    assert_preflight_rejection(
        service
            .dispatch(Request::StartSession(dispatched_inline))
            .await
            .unwrap_err(),
    );

    let mut unsafe_ttl = preflight_start_request(&fake_claude, &cwd, &config_root, &marker);
    unsafe_ttl.retention = RetentionPolicy::Persistent {
        idle_ttl_ms: MAX_SAFE_JSON_INTEGER + 1,
    };
    assert_preflight_rejection(service.start_session(unsafe_ttl).await.unwrap_err());

    let mut typed_once = preflight_run_once(&fake_claude, &cwd, &config_root, &marker);
    typed_once.turn.deadline_unix_ms = Some(u64::MAX);
    assert_preflight_rejection(service.run_once(typed_once).await.unwrap_err());

    let mut dispatched_once = preflight_run_once(&fake_claude, &cwd, &config_root, &marker);
    dispatched_once.turn.deadline_unix_ms = Some(MAX_SAFE_JSON_INTEGER + 1);
    assert_preflight_rejection(
        service
            .dispatch(Request::RunOnce(dispatched_once))
            .await
            .unwrap_err(),
    );

    assert!(
        !marker.exists(),
        "request preflight invoked Claude --version"
    );
    assert_eq!(
        relative_paths(root.path()),
        baseline,
        "request preflight created or removed a runtime/materialized path"
    );
    service.shutdown().await.unwrap();
    candidates.assert_unchanged().unwrap();
}

#[tokio::test]
#[ignore = "starts the private rmux sidecar but proves transcript identity fails before any launch preparation"]
async fn transcript_identity_preflight_precedes_lifecycle_files_version_and_process_side_effects() {
    let candidates = CandidateFiles::discover(&["pmux-rmuxd", "pmux-launcher"]).unwrap();
    let root = TempDir::new().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let cwd = root.path().join("workspace");
    let config_root = root.path().join("claude-config");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(config_root.join("projects")).unwrap();
    let marker = root.path().join("claude-invoked");
    let fake_claude = root.path().join("identity-preflight-claude");
    write_version_probe(&fake_claude);

    let service = NativeService::start(
        PrivateRuntimeConfig {
            rmuxd: candidates.path("pmux-rmuxd").to_path_buf(),
            launcher: candidates.path("pmux-launcher").to_path_buf(),
            runtime_parent: Some(root.path().to_path_buf()),
            startup_timeout: Duration::from_secs(5),
            operation_timeout: Duration::from_secs(5),
            lease_ttl: Duration::from_secs(5),
        },
        NativeServiceConfig::default(),
    )
    .await
    .unwrap();

    let collision_id = Uuid::new_v4();
    write_identity_transcript(
        &config_root,
        "foreign-project",
        collision_id,
        Path::new("/a/foreign/project"),
    );
    let missing_resume_id = Uuid::new_v4();
    let ambiguous_resume_id = Uuid::new_v4();
    let canonical_cwd = cwd.canonicalize().unwrap();
    write_identity_transcript(
        &config_root,
        "resume-a",
        ambiguous_resume_id,
        &canonical_cwd,
    );
    write_identity_transcript(
        &config_root,
        "resume-b",
        ambiguous_resume_id,
        &canonical_cwd,
    );
    let baseline = relative_paths(root.path());

    // The deliberately unavailable Hybrid helper is a mutation sentinel: if
    // lifecycle preparation runs before the foreign-project collision check,
    // this request returns InvalidConfig instead of IdCollision.
    let mut collision = preflight_start_request(&fake_claude, &cwd, &config_root, &marker);
    collision.identity = SessionIdentity::New {
        session_id: Some(collision_id),
    };
    collision.lifecycle = LifecycleMode::Hybrid {
        hook_timeout_ms: 5_000,
    };
    assert_identity_preflight_rejection(
        service.start_session(collision).await.unwrap_err(),
        ErrorCode::IdCollision,
    );

    // An invalid materialized prompt is a second mutation sentinel. Resume
    // lookup must report the absent transcript before creating launch files.
    let mut missing = preflight_start_request(&fake_claude, &cwd, &config_root, &marker);
    missing.identity = SessionIdentity::Resume {
        session_id: missing_resume_id,
    };
    missing
        .claude
        .as_mut()
        .expect("inline launch")
        .system_prompt = SystemPromptPolicy::Append {
        prompt: String::new(),
    };
    assert_identity_preflight_rejection(
        service.start_session(missing).await.unwrap_err(),
        ErrorCode::TranscriptUnavailable,
    );

    let mut ambiguous = preflight_start_request(&fake_claude, &cwd, &config_root, &marker);
    ambiguous.identity = SessionIdentity::Resume {
        session_id: ambiguous_resume_id,
    };
    ambiguous.lifecycle = LifecycleMode::Hybrid {
        hook_timeout_ms: 5_000,
    };
    assert_identity_preflight_rejection(
        service.start_session(ambiguous).await.unwrap_err(),
        ErrorCode::SchemaDrift,
    );

    assert!(
        !marker.exists(),
        "identity preflight invoked the Claude version probe or process"
    );
    assert_eq!(
        relative_paths(root.path()),
        baseline,
        "identity preflight created, removed, or retained a runtime/launch artifact"
    );
    service.shutdown().await.unwrap();
    candidates.assert_unchanged().unwrap();
}

fn write_fake_claude(path: &Path) {
    let script = r###"#!/bin/sh
if [ "$1" = "--version" ]; then
  echo "9.9.9 (Claude Code fake)"
  exit 0
fi
session=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --session-id|--resume) session="$2"; shift 2 ;;
    *) shift ;;
  esac
done
test -n "$session" || exit 41
transcript="$CLAUDE_CONFIG_DIR/projects/fake/$session.jsonl"
proof="$CLAUDE_CONFIG_DIR/fake-input-proof"
: > "$proof"

record_stage() {
  printf '%s\n' "$1" >> "$proof"
}

read_one_hex() {
  dd bs=1 count=1 2>/dev/null | od -An -tx1 | tr -d ' \n'
}

# Own the PTY byte stream exactly. In raw mode neither the line discipline nor
# terminal echo can fabricate render evidence or transform Enter.
stty raw -echo min 0 time 0 || exit 42

# First expose a real cursor-correlated editor, but for less than pmux's quiet
# interval. Replacing it proves that a single ready-shaped snapshot is not an
# admission boundary.
printf '\033[2J\033[22;1H❯ '
record_stage transient_ready_rendered
sleep 0.12
printf '\033[2J\033[12;1HClaude is still starting'

# A pre-patch eager submission would already have queued paste/Enter bytes.
# VMIN=0/VTIME=1 makes this a bounded 100 ms absence assertion on both BSD and
# Linux termios implementations.
stty min 0 time 1 || exit 43
early_start="$(read_one_hex)"
if [ -n "$early_start" ]; then
  record_stage unexpected_input_before_stable_ready
  exit 44
fi
record_stage no_input_before_stable_ready

# This is the first stable empty editor: cursor row and prompt row coincide,
# with the cursor exactly two cells after the prompt glyph.
stty min 1 time 0 || exit 45
printf '\033[2J\033[22;1H❯ '
record_stage stable_ready_rendered

# Validate one—and only one—bracketed paste containing the expected prompt.
# ESC[200~ + "hello" + ESC[201~ is exactly 17 bytes.
paste_hex="$(dd bs=1 count=17 2>/dev/null | od -An -tx1 | tr -d ' \n')"
if [ "$paste_hex" != "1b5b3230307e68656c6c6f1b5b3230317e" ]; then
  record_stage bracketed_paste_mismatch
  exit 46
fi
record_stage bracketed_paste_exact

# Deliberately withhold the editor redraw. Enter must not be sent merely
# because the paste RPC was acknowledged; it may be sent only after pmux sees
# a changed, stable, cursor-correlated editor and fences that observation.
stty min 0 time 3 || exit 47
early_enter="$(read_one_hex)"
if [ -n "$early_enter" ]; then
  record_stage enter_arrived_before_paste_render
  exit 48
fi
record_stage no_enter_before_paste_render

stty min 1 time 0 || exit 49
printf '\033[2J\033[22;1H❯ hello'
record_stage pasted_editor_rendered

enter_hex="$(read_one_hex)"
if [ "$enter_hex" != "0d" ]; then
  record_stage enter_byte_mismatch
  exit 50
fi
record_stage single_enter_exact

# A second byte would prove duplicate Enter or duplicate paste. Keep the
# observation bounded, then publish the only semantic authority: JSONL.
stty min 0 time 2 || exit 51
second_byte="$(read_one_hex)"
if [ -n "$second_byte" ]; then
  record_stage unexpected_second_input_byte
  exit 52
fi
record_stage no_second_input_byte

printf '%s\n' "{\"type\":\"user\",\"uuid\":\"user-1\",\"parentUuid\":null,\"sessionId\":\"$session\",\"cwd\":\"$PWD\",\"promptSource\":\"typed\",\"promptId\":\"prompt-1\",\"message\":{\"content\":\"hello\"}}" >> "$transcript"
printf '%s\n' "{\"type\":\"assistant\",\"uuid\":\"assistant-1\",\"parentUuid\":\"user-1\",\"sessionId\":\"$session\",\"cwd\":\"$PWD\",\"requestId\":\"request-1\",\"message\":{\"id\":\"message-1\",\"model\":\"fake-model\",\"content\":[{\"type\":\"text\",\"text\":\"world\"}],\"stop_reason\":\"end_turn\",\"usage\":{\"input_tokens\":3,\"output_tokens\":1}}}" >> "$transcript"
record_stage transcript_written

printf '\033[2J\033[22;1H❯ '
while :; do sleep 30; done
"###;
    std::fs::write(path, script).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn write_version_probe(path: &Path) {
    let script = r#"#!/bin/sh
printf '%s\n' "$*" > "$PMUX_TEST_VERSION_MARKER"
if [ "$1" = "--version" ]; then
  printf '9.9.9 (Claude Code preflight probe)\n'
  exit 0
fi
exit 97
"#;
    std::fs::write(path, script).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

fn write_identity_transcript(config_root: &Path, project: &str, session_id: Uuid, cwd: &Path) {
    let project = config_root.join("projects").join(project);
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join(format!("{session_id}.jsonl")),
        serde_json::json!({
            "type": "user",
            "uuid": Uuid::new_v4(),
            "sessionId": session_id,
            "cwd": cwd,
            "promptSource": "typed",
            "message": {"content": "historical"}
        })
        .to_string()
            + "\n",
    )
    .unwrap();
}

fn preflight_start_request(
    executable: &Path,
    cwd: &Path,
    config_root: &Path,
    marker: &Path,
) -> StartSessionRequest {
    StartSessionRequest {
        identity: SessionIdentity::New {
            session_id: Some(Uuid::new_v4()),
        },
        cwd: cwd.canonicalize().unwrap().to_string_lossy().into_owned(),
        agent: None,
        claude: Some(ClaudeLaunchConfig {
            executable: executable
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            model: None,
            effort: None,
            permission_mode: Some(PermissionMode::Default),
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            settings: Vec::new(),
            mcp_configs: Vec::new(),
            plugin_dirs: Vec::new(),
            system_prompt: SystemPromptPolicy::Default,
            extra_args: Vec::new(),
        }),
        environment: EnvironmentSpec {
            snapshot: BTreeMap::from([
                (
                    "HOME".to_owned(),
                    config_root.to_string_lossy().into_owned(),
                ),
                (
                    "CLAUDE_CONFIG_DIR".to_owned(),
                    config_root.to_string_lossy().into_owned(),
                ),
                (
                    "PMUX_TEST_VERSION_MARKER".to_owned(),
                    marker.to_string_lossy().into_owned(),
                ),
                ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
            ]),
            set: BTreeMap::new(),
            unset: BTreeSet::new(),
        },
        auth_policy: AuthPolicy::Inherit,
        config_isolation: None,
        terminal: TerminalSpec {
            rows: 24,
            cols: 120,
            profile: TerminalProfile::Transparent,
            input_transport: InputTransport::Sdk,
        },
        lifecycle: LifecycleMode::Transcript,
        retention: RetentionPolicy::Persistent {
            idle_ttl_ms: 60_000,
        },
        compatibility: CompatibilityPolicy::RequireTested,
        cell: SessionCell::Full,
    }
}

fn preflight_run_once(
    executable: &Path,
    cwd: &Path,
    config_root: &Path,
    marker: &Path,
) -> RunOnceRequest {
    RunOnceRequest {
        session: preflight_start_request(executable, cwd, config_root, marker),
        turn: TurnRequest {
            turn_id: Uuid::new_v4(),
            prompt: "preflight".to_owned(),
            deadline_unix_ms: None,
            lease: TurnLeasePolicy {
                on_disconnect: DisconnectAction::Continue,
                heartbeat_timeout_ms: None,
            },
        },
    }
}

fn assert_preflight_rejection(error: pseudomux_protocol::v1::ErrorBody) {
    assert_eq!(error.code, ErrorCode::InvalidConfig);
    assert!(!error.retryable);
    assert!(error.details.is_null());
    serde_json::to_vec(&error).unwrap();
}

fn assert_identity_preflight_rejection(
    error: pseudomux_protocol::v1::ErrorBody,
    expected: ErrorCode,
) {
    assert_eq!(error.code, expected);
    assert!(!error.retryable);
    assert!(error.details.is_null());
    serde_json::to_vec(&error).unwrap();
}

fn relative_paths(root: &Path) -> Vec<std::path::PathBuf> {
    fn visit(root: &Path, directory: &Path, paths: &mut Vec<std::path::PathBuf>) {
        let mut entries = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        entries.sort();
        for path in entries {
            paths.push(path.strip_prefix(root).unwrap().to_path_buf());
            if path.is_dir() {
                visit(root, &path, paths);
            }
        }
    }

    let mut paths = Vec::new();
    visit(root, root, &mut paths);
    paths
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
