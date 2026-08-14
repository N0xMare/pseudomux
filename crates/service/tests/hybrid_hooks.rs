#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use pseudomux_protocol::v1::{ConfigSource, LifecycleMode};
use pseudomux_service::hybrid_hooks::{
    LifecycleEventKind, MAX_HOOK_FRAME_BYTES, PreparedLifecycle, prepare_lifecycle,
    send_hook_payload,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufWriter};
use tokio::net::UnixStream;
use uuid::Uuid;

fn private_runtime() -> TempDir {
    let runtime = tempfile::tempdir().unwrap();
    fs::set_permissions(runtime.path(), fs::Permissions::from_mode(0o700)).unwrap();
    runtime
}

fn hook_client() -> PathBuf {
    std::env::current_exe().unwrap()
}

fn hybrid_mode() -> LifecycleMode {
    LifecycleMode::Hybrid {
        hook_timeout_ms: 5_000,
    }
}

fn read_settings(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn payload(session_id: Uuid, event: &str, transcript: &Path) -> Value {
    json!({
        "session_id": session_id,
        "hook_event_name": event,
        "transcript_path": transcript,
        "assistant_output": "must-not-enter-the-observation",
        "usage": {"input_tokens": 999999},
    })
}

#[tokio::test]
async fn hybrid_composes_hooks_without_replacing_caller_settings() {
    let runtime = private_runtime();
    let session_id = Uuid::new_v4();
    let existing_command = "printf 'unchanged\\bytes\nλ'";
    let original = json!({
        "permissions": {"allow": ["Read", "Glob"]},
        "env": {"SMITHERS_SNAPSHOT_SOCK": "/private/snapshot.sock"},
        "hooks": {
            "Stop": [{
                "matcher": "original-matcher",
                "hooks": [{"type": "command", "command": existing_command}]
            }],
            "PostToolUse": [{
                "matcher": "Write|Edit",
                "hooks": [{"type": "command", "command": "snapshot-existing"}]
            }]
        }
    });
    let original_bytes = serde_json::to_vec_pretty(&original).unwrap();
    let original_path = runtime.path().join("caller-settings.json");
    fs::write(&original_path, &original_bytes).unwrap();

    let caller = vec![ConfigSource::File {
        path: original_path.to_string_lossy().into_owned(),
    }];
    let prepared = prepare_lifecycle(
        &hybrid_mode(),
        runtime.path(),
        session_id,
        &hook_client(),
        &caller,
    )
    .await
    .unwrap();
    let hybrid = prepared.hybrid().unwrap();
    let generated = read_settings(hybrid.settings_path());

    assert_eq!(generated["permissions"], original["permissions"]);
    assert_eq!(generated["env"], original["env"]);
    assert_eq!(
        generated["hooks"]["PostToolUse"],
        original["hooks"]["PostToolUse"]
    );
    assert_eq!(
        generated["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .as_bytes(),
        existing_command.as_bytes()
    );
    assert_eq!(generated["hooks"]["Stop"].as_array().unwrap().len(), 2);
    for event in ["SessionStart", "Stop", "StopFailure"] {
        let entries = generated["hooks"][event].as_array().unwrap();
        let injected = entries.last().unwrap();
        let command = injected["hooks"][0]["command"].as_str().unwrap();
        assert!(command.contains(&format!("--event '{event}'")));
        assert_eq!(injected["hooks"][0]["timeout"], 5);
    }
    assert_eq!(fs::read(&original_path).unwrap(), original_bytes);

    let launch_settings = prepared.launch_settings(&caller);
    assert_eq!(launch_settings.len(), 1);
    assert!(matches!(launch_settings[0], ConfigSource::File { .. }));
    let debug = format!("{prepared:?}");
    assert!(!debug.contains(existing_command));
    assert!(!debug.contains(&runtime.path().to_string_lossy().into_owned()));
}

#[tokio::test]
async fn relay_validates_and_emits_only_lifecycle_observations() {
    let runtime = private_runtime();
    let session_id = Uuid::new_v4();
    let transcript = runtime.path().join("projects/session.jsonl");
    let mut prepared = prepare_lifecycle(
        &hybrid_mode(),
        runtime.path(),
        session_id,
        &hook_client(),
        &[],
    )
    .await
    .unwrap();
    let socket = prepared.hybrid().unwrap().socket_path().to_path_buf();

    for (index, event) in [
        LifecycleEventKind::SessionStart,
        LifecycleEventKind::Stop,
        LifecycleEventKind::StopFailure,
    ]
    .into_iter()
    .enumerate()
    {
        send_hook_payload(
            &socket,
            session_id,
            event,
            payload(session_id, event_name(event), &transcript),
        )
        .await
        .unwrap();
        let observation = tokio::time::timeout(
            Duration::from_secs(1),
            prepared.hybrid_mut().unwrap().recv(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(observation.sequence(), index as u64 + 1);
        assert_eq!(observation.session_id(), session_id);
        assert_eq!(observation.event(), event);
        assert_eq!(observation.transcript_path(), Some(transcript.as_path()));
        assert_eq!(
            observation.failure_observed(),
            event == LifecycleEventKind::StopFailure
        );
        let debug = format!("{observation:?}");
        assert!(!debug.contains("must-not-enter-the-observation"));
        assert!(!debug.contains("999999"));
        assert!(!debug.contains(&transcript.to_string_lossy().into_owned()));
    }

    let rejected = send_hook_payload(
        &socket,
        Uuid::new_v4(),
        LifecycleEventKind::Stop,
        payload(session_id, "Stop", &transcript),
    )
    .await;
    assert!(rejected.is_err());
    assert!(prepared.hybrid_mut().unwrap().try_recv().is_err());
}

#[tokio::test]
async fn relay_rejects_malformed_and_oversized_frames() {
    let runtime = private_runtime();
    let session_id = Uuid::new_v4();
    let mut prepared = prepare_lifecycle(
        &hybrid_mode(),
        runtime.path(),
        session_id,
        &hook_client(),
        &[],
    )
    .await
    .unwrap();
    let socket = prepared.hybrid().unwrap().socket_path().to_path_buf();

    let mut malformed = BufWriter::new(UnixStream::connect(&socket).await.unwrap());
    malformed.write_u32(1).await.unwrap();
    malformed.write_all(b"{").await.unwrap();
    malformed.flush().await.unwrap();
    drop(malformed);

    let mut oversized = BufWriter::new(UnixStream::connect(&socket).await.unwrap());
    oversized
        .write_u32(u32::try_from(MAX_HOOK_FRAME_BYTES + 1).unwrap())
        .await
        .unwrap();
    oversized.flush().await.unwrap();
    drop(oversized);

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(prepared.hybrid_mut().unwrap().try_recv().is_err());
}

#[tokio::test]
async fn cooperative_shutdown_aborts_and_joins_every_inflight_relay_connection() {
    let runtime = private_runtime();
    let session_id = Uuid::new_v4();
    let prepared = prepare_lifecycle(
        &hybrid_mode(),
        runtime.path(),
        session_id,
        &hook_client(),
        &[],
    )
    .await
    .unwrap();
    let PreparedLifecycle::Hybrid(hybrid) = prepared else {
        panic!("hybrid preparation returned transcript lifecycle")
    };
    let socket = hybrid.socket_path().to_path_buf();
    let settings = hybrid.settings_path().to_path_buf();

    // Sixteen partial frames occupy every bounded relay slot. The seventeenth
    // connection is accepted and closed for lack of a permit, which proves the
    // earlier streams reached detached per-connection work before shutdown.
    let mut streams = Vec::new();
    for _ in 0..17 {
        let mut stream = UnixStream::connect(&socket).await.unwrap();
        stream.write_u32(1).await.unwrap();
        streams.push(stream);
    }
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if streams.iter().any(|stream| {
                let mut byte = [0_u8; 1];
                matches!(stream.try_read(&mut byte), Ok(0))
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("relay did not accept enough partial connections to saturate its slots");

    tokio::time::timeout(Duration::from_secs(1), hybrid.shutdown())
        .await
        .expect("cooperative relay shutdown did not join its accepted connections");
    assert!(!socket.exists());
    assert!(!settings.exists());

    for mut stream in streams {
        let mut byte = [0_u8; 1];
        match tokio::time::timeout(Duration::from_secs(1), stream.read(&mut byte)).await {
            Ok(Ok(0)) => {}
            Ok(Err(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::BrokenPipe
                        | std::io::ErrorKind::NotConnected
                ) => {}
            other => panic!("relay connection survived cooperative shutdown: {other:?}"),
        }
    }
}

#[tokio::test]
async fn permissions_collisions_and_drop_cleanup_fail_closed() {
    let runtime = private_runtime();
    let session_id = Uuid::new_v4();
    let prepared = prepare_lifecycle(
        &hybrid_mode(),
        runtime.path(),
        session_id,
        &hook_client(),
        &[],
    )
    .await
    .unwrap();
    let hybrid = prepared.hybrid().unwrap();
    let settings = hybrid.settings_path().to_path_buf();
    let socket = hybrid.socket_path().to_path_buf();
    assert_eq!(
        fs::metadata(&settings).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
        0o600
    );
    drop(prepared);
    assert!(!settings.exists());
    assert!(!socket.exists());

    fs::set_permissions(runtime.path(), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        prepare_lifecycle(
            &hybrid_mode(),
            runtime.path(),
            Uuid::new_v4(),
            &hook_client(),
            &[],
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn startup_failures_preserve_existing_artifacts_and_leave_no_relay() {
    let runtime = private_runtime();
    let session_id = Uuid::new_v4();
    let artifact_key = &session_id.simple().to_string()[..16];
    let collision = runtime.path().join(format!("hh-{artifact_key}.json"));
    fs::write(&collision, b"do-not-replace").unwrap();

    assert!(
        prepare_lifecycle(
            &hybrid_mode(),
            runtime.path(),
            session_id,
            &hook_client(),
            &[],
        )
        .await
        .is_err()
    );
    assert_eq!(fs::read(&collision).unwrap(), b"do-not-replace");
    assert!(
        !runtime
            .path()
            .join(format!("hh-{artifact_key}.sock"))
            .exists()
    );

    let malformed_path = runtime.path().join("malformed.json");
    fs::write(&malformed_path, b"{not-json").unwrap();
    let malformed = [ConfigSource::File {
        path: malformed_path.to_string_lossy().into_owned(),
    }];
    let malformed_session = Uuid::new_v4();
    let malformed_key = &malformed_session.simple().to_string()[..16];
    assert!(
        prepare_lifecycle(
            &hybrid_mode(),
            runtime.path(),
            malformed_session,
            &hook_client(),
            &malformed,
        )
        .await
        .is_err()
    );
    assert!(
        !runtime
            .path()
            .join(format!("hh-{malformed_key}.json"))
            .exists()
    );
    assert!(
        !runtime
            .path()
            .join(format!("hh-{malformed_key}.sock"))
            .exists()
    );
    assert_eq!(fs::read(&malformed_path).unwrap(), b"{not-json");
}

#[tokio::test]
async fn drop_never_removes_same_user_path_replacements() {
    let runtime = private_runtime();
    let prepared = prepare_lifecycle(
        &hybrid_mode(),
        runtime.path(),
        Uuid::new_v4(),
        &hook_client(),
        &[],
    )
    .await
    .unwrap();
    let hybrid = prepared.hybrid().unwrap();
    let settings = hybrid.settings_path().to_path_buf();
    let socket = hybrid.socket_path().to_path_buf();

    fs::remove_file(&settings).unwrap();
    fs::write(&settings, b"replacement-settings").unwrap();
    fs::remove_file(&socket).unwrap();
    fs::write(&socket, b"replacement-socket-path").unwrap();

    drop(prepared);
    assert_eq!(fs::read(&settings).unwrap(), b"replacement-settings");
    assert_eq!(fs::read(&socket).unwrap(), b"replacement-socket-path");
}

#[tokio::test]
async fn transcript_mode_injects_nothing_even_with_invalid_paths() {
    let original = vec![ConfigSource::Inline {
        document: json!({"hooks": {"PostToolUse": []}}),
    }];
    let prepared = prepare_lifecycle(
        &LifecycleMode::Transcript,
        Path::new("/missing/private/runtime"),
        Uuid::new_v4(),
        Path::new("/missing/hook-client"),
        &original,
    )
    .await
    .unwrap();
    assert!(matches!(prepared, PreparedLifecycle::Transcript));
    assert_eq!(prepared.launch_settings(&original), original);
}

const fn event_name(event: LifecycleEventKind) -> &'static str {
    match event {
        LifecycleEventKind::SessionStart => "SessionStart",
        LifecycleEventKind::Stop => "Stop",
        LifecycleEventKind::StopFailure => "StopFailure",
    }
}
