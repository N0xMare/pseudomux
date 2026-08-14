mod process_support;
mod support;

use std::sync::Arc;
use std::time::Duration;

use pseudomux_protocol::v1::{
    CancelOutcome, CancelTurnRequest, ErrorCode, InspectSessionRequest, MAX_NATIVE_FRAME_BYTES,
    Request, RequestEnvelope, ResponseEnvelope, ResponsePayload, TurnOutcome,
};
use pseudomux_service::driver_io::MAX_PROMPT_BYTES;
use pseudomux_service::v1::{DriverFailure, StoredTurnTerminal};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use uuid::Uuid;

use process_support::actual_daemon::ActualDaemon;
use support::{
    Probe, TestTerminal, TestTranscript, actor_config, close_and_unregister, generation, id,
    register, registry_with_config, turn, wait_for_resources_released, wait_for_stored_turn,
};

#[tokio::test]
async fn bounded_fault_soak_releases_every_actor_backend_and_temp_resource() {
    const ITERATIONS: usize = 128;
    let registry = registry_with_config(actor_config());
    let probe = Arc::new(Probe::default());
    let workspace = tempfile::tempdir().unwrap();

    for index in 0..ITERATIONS {
        let session_id = id(0xf000 + index as u128);
        let turn_id = id(0xf800 + index as u128);
        let terminal = if index % 4 == 1 {
            Arc::new(TestTerminal::failing_submit(
                Arc::clone(&probe),
                ErrorCode::PermissionDenied,
            ))
        } else {
            Arc::new(TestTerminal::new(Arc::clone(&probe)))
        };
        let owned_temp = tempfile::Builder::new()
            .prefix("pmux-service-soak-")
            .tempdir_in(workspace.path())
            .unwrap();
        let owned_path = owned_temp.path().to_path_buf();
        std::fs::write(
            owned_path.join("owned.marker"),
            b"owned by transcript boundary",
        )
        .unwrap();
        let arm_failure = (index % 4 == 2).then(|| {
            DriverFailure::new(
                ErrorCode::TranscriptUnavailable,
                "injected transcript arm failure",
            )
        });
        let transcript = Arc::new(TestTranscript::with_tempdir(
            Arc::clone(&probe),
            arm_failure,
            owned_temp,
        ));
        register(
            &registry,
            session_id,
            Arc::clone(&terminal),
            transcript,
            workspace.path(),
        )
        .await;

        match index % 4 {
            0 => {}
            1 => {
                registry
                    .run_turn(turn(session_id, turn_id, "submit fault"))
                    .await
                    .unwrap();
                let StoredTurnTerminal::Failed(error) =
                    wait_for_stored_turn(&registry, session_id, turn_id).await
                else {
                    panic!("submit fault must store one immutable failure")
                };
                assert_eq!(error.code, ErrorCode::PermissionDenied);
            }
            2 => {
                registry
                    .run_turn(turn(session_id, turn_id, "transcript fault"))
                    .await
                    .unwrap();
                let StoredTurnTerminal::Failed(error) =
                    wait_for_stored_turn(&registry, session_id, turn_id).await
                else {
                    panic!("transcript fault must store one immutable failure")
                };
                assert_eq!(error.code, ErrorCode::TranscriptUnavailable);
            }
            3 => {
                registry
                    .run_turn(turn(session_id, turn_id, "cancel cleanly"))
                    .await
                    .unwrap();
                terminal.wait_for_submission(session_id, turn_id).await;
                let cancelled = registry
                    .cancel_turn(CancelTurnRequest {
                        session_id,
                        generation_id: generation(session_id),
                        turn_id,
                    })
                    .await
                    .unwrap();
                assert_eq!(cancelled.outcome, CancelOutcome::Cancelled);
                let StoredTurnTerminal::Result(result) =
                    wait_for_stored_turn(&registry, session_id, turn_id).await
                else {
                    panic!("successful cancellation must remain replayable")
                };
                assert_eq!(result.outcome, TurnOutcome::Cancelled);
            }
            _ => unreachable!(),
        }

        close_and_unregister(&registry, session_id).await;
        drop(terminal);
        wait_for_resources_released(&probe).await;
        assert!(
            !owned_path.exists(),
            "actor-owned temporary transcript directory leaked: {}",
            owned_path.display()
        );
    }

    assert_eq!(probe.live_terminals(), 0);
    assert_eq!(probe.live_transcripts(), 0);
    assert_eq!(probe.closes(), ITERATIONS);
    assert_eq!(probe.interrupts(), ITERATIONS / 4);
    assert_eq!(probe.submissions().len(), ITERATIONS / 2);
    assert_eq!(std::fs::read_dir(workspace.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn exact_prompt_maximum_and_one_past_are_decided_before_actor_or_terminal_mutation() {
    let registry = registry_with_config(actor_config());
    let probe = Arc::new(Probe::default());
    let terminal = Arc::new(TestTerminal::new(Arc::clone(&probe)));
    let workspace = tempfile::tempdir().unwrap();
    let session_id = id(0x30_001);
    let maximum_turn = id(0x30_002);
    let rejected_turn = id(0x30_003);
    register(
        &registry,
        session_id,
        Arc::clone(&terminal),
        Arc::new(TestTranscript::pending(Arc::clone(&probe))),
        workspace.path(),
    )
    .await;

    registry
        .run_turn(turn(session_id, maximum_turn, "m".repeat(MAX_PROMPT_BYTES)))
        .await
        .unwrap();
    terminal.wait_for_submission(session_id, maximum_turn).await;
    let cancelled = registry
        .cancel_turn(CancelTurnRequest {
            session_id,
            generation_id: generation(session_id),
            turn_id: maximum_turn,
        })
        .await
        .unwrap();
    assert_eq!(cancelled.outcome, CancelOutcome::Cancelled);

    let before = registry
        .inspect(InspectSessionRequest {
            session_id,
            generation_id: generation(session_id),
        })
        .await
        .unwrap();
    let error = registry
        .run_turn(turn(
            session_id,
            rejected_turn,
            "x".repeat(MAX_PROMPT_BYTES + 1),
        ))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidConfig);
    assert!(error.message.contains("1048576-byte service limit"));
    let after = registry
        .inspect(InspectSessionRequest {
            session_id,
            generation_id: generation(session_id),
        })
        .await
        .unwrap();
    assert_eq!(after.state, before.state);
    assert_eq!(after.active_turn_id, before.active_turn_id);
    assert_eq!(after.last_sequence, before.last_sequence);
    assert!(
        registry
            .stored_turn(session_id, generation(session_id), rejected_turn)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(probe.submissions().len(), 1);

    close_and_unregister(&registry, session_id).await;
    drop(terminal);
    wait_for_resources_released(&probe).await;
}

#[tokio::test]
async fn turn_history_byte_reservation_accepts_the_exact_boundary_and_rejects_one_past() {
    let prompt = "history-boundary";
    let measured = actor_history_rejection(0, id(0x31_001), id(0x31_002), prompt).await;
    let required_bytes = measured.details["additional_bytes"]
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .expect("capacity error exposes an exact bounded reservation");
    assert!(required_bytes > prompt.len());

    let registry = registry_with_config({
        let mut config = actor_config();
        config.turn_history_byte_capacity = required_bytes;
        config
    });
    let probe = Arc::new(Probe::default());
    let terminal = Arc::new(TestTerminal::new(Arc::clone(&probe)));
    let workspace = tempfile::tempdir().unwrap();
    let session_id = id(0x31_101);
    let turn_id = id(0x31_102);
    register(
        &registry,
        session_id,
        Arc::clone(&terminal),
        Arc::new(TestTranscript::pending(Arc::clone(&probe))),
        workspace.path(),
    )
    .await;
    registry
        .run_turn(turn(session_id, turn_id, prompt))
        .await
        .unwrap();
    terminal.wait_for_submission(session_id, turn_id).await;
    registry
        .cancel_turn(CancelTurnRequest {
            session_id,
            generation_id: generation(session_id),
            turn_id,
        })
        .await
        .unwrap();
    close_and_unregister(&registry, session_id).await;
    drop(terminal);
    wait_for_resources_released(&probe).await;

    let one_past =
        actor_history_rejection(required_bytes - 1, id(0x31_201), id(0x31_202), prompt).await;
    assert_eq!(one_past.details["additional_bytes"], required_bytes);
    assert_eq!(one_past.details["maximum_bytes"], required_bytes - 1);
}

#[tokio::test]
#[ignore = "launches exact candidate pmuxd/private-rmux binaries; credential-free"]
async fn actual_daemon_accepts_exact_native_frame_and_rejects_one_past_without_body_allocation() {
    let mut daemon = ActualDaemon::start().await.unwrap();
    let request_id = id(0x32_001);
    let prefix = format!(
        "{{\"version\":1,\"request_id\":\"{request_id}\",\"method\":\"ping\",\"padding\":\""
    );
    let suffix = "\"}";
    let padding_bytes = MAX_NATIVE_FRAME_BYTES - prefix.len() - suffix.len();
    let mut exact_payload = Vec::with_capacity(MAX_NATIVE_FRAME_BYTES);
    exact_payload.extend_from_slice(prefix.as_bytes());
    exact_payload.resize(exact_payload.len() + padding_bytes, b'x');
    exact_payload.extend_from_slice(suffix.as_bytes());
    assert_eq!(exact_payload.len(), MAX_NATIVE_FRAME_BYTES);

    let mut exact = UnixStream::connect(daemon.socket()).await.unwrap();
    write_raw_frame(&mut exact, &exact_payload).await;
    let response = read_raw_response(&mut exact).await;
    assert_eq!(response.request_id, request_id);
    assert!(matches!(
        response.payload,
        ResponsePayload::Failure(ref error) if error.code == ErrorCode::InvalidConfig
    ));

    let recovery_id = id(0x32_002);
    let recovery = serde_json::to_vec(&RequestEnvelope::new(recovery_id, Request::Ping)).unwrap();
    write_raw_frame(&mut exact, &recovery).await;
    let response = read_raw_response(&mut exact).await;
    assert_eq!(response.request_id, recovery_id);
    assert!(matches!(response.payload, ResponsePayload::Success(_)));
    drop(exact);

    let mut one_past = UnixStream::connect(daemon.socket()).await.unwrap();
    one_past
        .write_all(&((MAX_NATIVE_FRAME_BYTES as u32) + 1).to_be_bytes())
        .await
        .unwrap();
    let response = read_raw_response(&mut one_past).await;
    assert_eq!(response.request_id, Uuid::nil());
    assert!(matches!(
        response.payload,
        ResponsePayload::Failure(ref error) if error.code == ErrorCode::InvalidConfig
    ));
    let mut trailing = [0_u8; 1];
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), one_past.read(&mut trailing))
            .await
            .unwrap()
            .unwrap(),
        0
    );

    daemon.stop().await.unwrap();
}

async fn actor_history_rejection(
    byte_capacity: usize,
    session_id: Uuid,
    turn_id: Uuid,
    prompt: &str,
) -> pseudomux_protocol::v1::ErrorBody {
    let registry = registry_with_config({
        let mut config = actor_config();
        config.turn_history_byte_capacity = byte_capacity;
        config
    });
    let probe = Arc::new(Probe::default());
    let terminal = Arc::new(TestTerminal::new(Arc::clone(&probe)));
    let workspace = tempfile::tempdir().unwrap();
    register(
        &registry,
        session_id,
        Arc::clone(&terminal),
        Arc::new(TestTranscript::pending(Arc::clone(&probe))),
        workspace.path(),
    )
    .await;
    let before = registry
        .inspect(InspectSessionRequest {
            session_id,
            generation_id: generation(session_id),
        })
        .await
        .unwrap();
    let error = registry
        .run_turn(turn(session_id, turn_id, prompt))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::TurnHistoryCapacityExceeded);
    let after = registry
        .inspect(InspectSessionRequest {
            session_id,
            generation_id: generation(session_id),
        })
        .await
        .unwrap();
    assert_eq!(after.state, before.state);
    assert_eq!(after.active_turn_id, before.active_turn_id);
    assert_eq!(after.last_sequence, before.last_sequence);
    assert!(probe.submissions().is_empty());
    assert!(
        registry
            .stored_turn(session_id, generation(session_id), turn_id)
            .await
            .unwrap()
            .is_none()
    );
    close_and_unregister(&registry, session_id).await;
    drop(terminal);
    wait_for_resources_released(&probe).await;
    error
}

async fn write_raw_frame(stream: &mut UnixStream, payload: &[u8]) {
    stream
        .write_all(&(payload.len() as u32).to_be_bytes())
        .await
        .unwrap();
    stream.write_all(payload).await.unwrap();
}

async fn read_raw_response(stream: &mut UnixStream) -> ResponseEnvelope {
    let mut header = [0_u8; 4];
    tokio::time::timeout(Duration::from_secs(12), stream.read_exact(&mut header))
        .await
        .unwrap()
        .unwrap();
    let length = u32::from_be_bytes(header) as usize;
    assert!(length <= MAX_NATIVE_FRAME_BYTES);
    let mut payload = vec![0_u8; length];
    tokio::time::timeout(Duration::from_secs(12), stream.read_exact(&mut payload))
        .await
        .unwrap()
        .unwrap();
    serde_json::from_slice(&payload).unwrap()
}
