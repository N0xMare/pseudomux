//! Registry-level backpressure. Live Path A `client.start_session` lanes were
//! removed; pool concurrency lives in `crates/e2e/tests/pool_concurrency.rs`.

mod support;

use std::sync::Arc;

use pseudomux_protocol::v1::{
    CancelOutcome, CancelTurnRequest, ErrorCode, EventPayload, InspectSessionRequest,
    ResponseEnvelope, ResponseResult, SessionState, SubscribeEventsRequest,
};
use tokio::sync::Barrier;
use tokio::task::JoinSet;

use support::{
    Probe, TestTerminal, TestTranscript, actor_config, close_and_unregister, generation, id,
    register, registry_with_config, turn, wait_for_resources_released,
};

#[tokio::test]
async fn cancellation_distinguishes_terminal_wrong_and_missing_active_turns() {
    let registry = registry_with_config(actor_config());
    let probe = Arc::new(Probe::default());
    let terminal = Arc::new(TestTerminal::new(Arc::clone(&probe)));
    let workspace = tempfile::tempdir().unwrap();
    let session_id = id(0xc001);
    let active_turn = id(0xc011);
    let wrong_turn = id(0xc012);
    register(
        &registry,
        session_id,
        Arc::clone(&terminal),
        Arc::new(TestTranscript::pending(Arc::clone(&probe))),
        workspace.path(),
    )
    .await;

    let missing = registry
        .cancel_turn(CancelTurnRequest {
            session_id,
            generation_id: generation(session_id),
            turn_id: wrong_turn,
        })
        .await
        .unwrap_err();
    assert_eq!(missing.code, ErrorCode::IdConflict);

    registry
        .run_turn(turn(session_id, active_turn, "cancel exactly"))
        .await
        .unwrap();
    terminal.wait_for_submission(session_id, active_turn).await;
    let wrong = registry
        .cancel_turn(CancelTurnRequest {
            session_id,
            generation_id: generation(session_id),
            turn_id: wrong_turn,
        })
        .await
        .unwrap_err();
    assert_eq!(wrong.code, ErrorCode::IdConflict);
    let snapshot = registry
        .inspect(InspectSessionRequest {
            session_id,
            generation_id: generation(session_id),
        })
        .await
        .unwrap();
    assert_eq!(snapshot.active_turn_id, Some(active_turn));

    let cancelled = registry
        .cancel_turn(CancelTurnRequest {
            session_id,
            generation_id: generation(session_id),
            turn_id: active_turn,
        })
        .await
        .unwrap();
    assert_eq!(cancelled.outcome, CancelOutcome::Cancelled);
    assert_eq!(cancelled.session_state, SessionState::Ready);

    let terminal_cancel = registry
        .cancel_turn(CancelTurnRequest {
            session_id,
            generation_id: generation(session_id),
            turn_id: active_turn,
        })
        .await
        .unwrap();
    assert_eq!(terminal_cancel.outcome, CancelOutcome::AlreadyTerminal);
    assert_eq!(terminal_cancel.session_state, SessionState::Ready);

    let no_active = registry
        .cancel_turn(CancelTurnRequest {
            session_id,
            generation_id: generation(session_id),
            turn_id: wrong_turn,
        })
        .await
        .unwrap_err();
    assert_eq!(no_active.code, ErrorCode::IdConflict);

    close_and_unregister(&registry, session_id).await;
    drop(terminal);
    wait_for_resources_released(&probe).await;
}

#[tokio::test]
async fn replay_byte_saturation_preserves_frame_paging_and_gap_exclusivity() {
    const FRAME_LIMIT: usize = 1_400;
    let mut config = actor_config();
    config.replay_capacity = 1_024;
    config.replay_byte_capacity = 3_000;
    config.max_frame_bytes = FRAME_LIMIT;
    let registry = registry_with_config(config);
    let probe = Arc::new(Probe::default());
    let terminal = Arc::new(TestTerminal::new(Arc::clone(&probe)));
    let workspace = tempfile::tempdir().unwrap();
    let session_id = id(0xc100);
    register(
        &registry,
        session_id,
        Arc::clone(&terminal),
        Arc::new(TestTranscript::pending(Arc::clone(&probe))),
        workspace.path(),
    )
    .await;

    for index in 0..10_u128 {
        let turn_id = id(0xc110 + index);
        registry
            .run_turn(turn(
                session_id,
                turn_id,
                format!("bounded replay {index} {}", "x".repeat(80)),
            ))
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
    }

    let snapshot = registry
        .inspect(InspectSessionRequest {
            session_id,
            generation_id: generation(session_id),
        })
        .await
        .unwrap();
    assert!(
        snapshot.last_sequence < 1_024,
        "count capacity was not saturated"
    );
    let gap_batch = registry
        .events(SubscribeEventsRequest {
            session_id,
            generation_id: generation(session_id),
            after_sequence: 0,
            wait_ms: 0,
            max_events: 512,
        })
        .await
        .unwrap();
    assert!(gap_batch.events.is_empty());
    let gap = gap_batch
        .replay_gap
        .as_ref()
        .expect("the byte budget must evict old events");
    assert!(gap.oldest_available > 1);
    assert_eq!(gap.requested_after, 0);
    assert_eq!(gap.next_sequence, gap.snapshot.last_sequence + 1);
    assert_eq!(gap_batch.next_sequence, gap.next_sequence);
    assert!(
        serde_json::to_vec(&ResponseEnvelope::success(
            id(0xc1ff),
            ResponseResult::Events(gap_batch.clone()),
        ))
        .unwrap()
        .len()
            <= FRAME_LIMIT
    );

    let mut after_sequence = gap.oldest_available - 1;
    let final_sequence = snapshot.last_sequence;
    let mut pages = 0;
    let mut observed = Vec::new();
    while after_sequence < final_sequence {
        let batch = registry
            .events(SubscribeEventsRequest {
                session_id,
                generation_id: generation(session_id),
                after_sequence,
                wait_ms: 0,
                max_events: 512,
            })
            .await
            .unwrap();
        assert!(batch.replay_gap.is_none());
        assert!(!batch.events.is_empty(), "paging must make progress");
        assert!(
            serde_json::to_vec(&ResponseEnvelope::success(
                id(0xc200 + pages),
                ResponseResult::Events(batch.clone()),
            ))
            .unwrap()
            .len()
                <= FRAME_LIMIT
        );
        assert_eq!(batch.events[0].sequence, after_sequence + 1);
        assert!(
            batch
                .events
                .windows(2)
                .all(|events| events[1].sequence == events[0].sequence + 1)
        );
        observed.extend(batch.events.iter().map(|event| event.sequence));
        after_sequence = batch.next_sequence - 1;
        pages += 1;
    }
    assert!(pages >= 2, "configured frame limit must require paging");
    assert_eq!(observed.last().copied(), Some(final_sequence));

    close_and_unregister(&registry, session_id).await;
    drop(terminal);
    wait_for_resources_released(&probe).await;
}

#[tokio::test]
async fn every_concurrent_long_poll_subscriber_wakes_for_one_actor_event() {
    const SUBSCRIBERS: usize = 16;
    let registry = Arc::new(registry_with_config(actor_config()));
    let probe = Arc::new(Probe::default());
    let terminal = Arc::new(TestTerminal::new(Arc::clone(&probe)));
    let workspace = tempfile::tempdir().unwrap();
    let session_id = id(0xc300);
    let turn_id = id(0xc301);
    register(
        &registry,
        session_id,
        Arc::clone(&terminal),
        Arc::new(TestTranscript::pending(Arc::clone(&probe))),
        workspace.path(),
    )
    .await;
    let after_sequence = registry
        .inspect(InspectSessionRequest {
            session_id,
            generation_id: generation(session_id),
        })
        .await
        .unwrap()
        .last_sequence;
    let barrier = Arc::new(Barrier::new(SUBSCRIBERS + 1));
    let mut subscribers = JoinSet::new();
    for _ in 0..SUBSCRIBERS {
        let registry = Arc::clone(&registry);
        let barrier = Arc::clone(&barrier);
        subscribers.spawn(async move {
            barrier.wait().await;
            registry
                .events(SubscribeEventsRequest {
                    session_id,
                    generation_id: generation(session_id),
                    after_sequence,
                    wait_ms: 30_000,
                    max_events: 32,
                })
                .await
        });
    }
    barrier.wait().await;
    registry
        .run_turn(turn(session_id, turn_id, "wake every subscriber"))
        .await
        .unwrap();

    let mut completed = 0;
    while let Some(result) = subscribers.join_next().await {
        let batch = result.unwrap().unwrap();
        assert!(batch.replay_gap.is_none());
        assert!(!batch.events.is_empty());
        assert!(batch.events.iter().all(|event| {
            event.session_id == session_id
                && event.generation_id == generation(session_id)
                && event.sequence > after_sequence
        }));
        completed += 1;
    }
    assert_eq!(completed, SUBSCRIBERS);

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
}

#[tokio::test]
async fn thirty_two_session_actors_remain_isolated_under_concurrent_load() {
    const SESSIONS: usize = 32;
    let registry = Arc::new(registry_with_config(actor_config()));
    let probe = Arc::new(Probe::default());
    let workspace = tempfile::tempdir().unwrap();
    let mut terminals = Vec::new();
    for index in 0..SESSIONS {
        let terminal = Arc::new(TestTerminal::new(Arc::clone(&probe)));
        register(
            &registry,
            id(0xd000 + index as u128),
            Arc::clone(&terminal),
            Arc::new(TestTranscript::pending(Arc::clone(&probe))),
            workspace.path(),
        )
        .await;
        terminals.push(terminal);
    }

    let mut submissions = JoinSet::new();
    for index in 0..SESSIONS {
        let registry = Arc::clone(&registry);
        submissions.spawn(async move {
            let session_id = id(0xd000 + index as u128);
            let turn_id = id(0xe000 + index as u128);
            registry
                .run_turn(turn(session_id, turn_id, format!("session-{index}")))
                .await
        });
    }
    while let Some(result) = submissions.join_next().await {
        let accepted = result.unwrap().unwrap();
        assert!(!accepted.replayed);
        assert_eq!(accepted.generation_id, generation(accepted.session_id));
    }

    for (index, terminal) in terminals.iter().enumerate() {
        let session_id = id(0xd000 + index as u128);
        let turn_id = id(0xe000 + index as u128);
        terminal.wait_for_submission(session_id, turn_id).await;
        let snapshot = registry
            .inspect(InspectSessionRequest {
                session_id,
                generation_id: generation(session_id),
            })
            .await
            .unwrap();
        assert_eq!(snapshot.active_turn_id, Some(turn_id));
        let events = registry
            .events(SubscribeEventsRequest {
                session_id,
                generation_id: generation(session_id),
                after_sequence: 0,
                wait_ms: 0,
                max_events: 64,
            })
            .await
            .unwrap();
        assert!(events.events.iter().all(|event| {
            event.session_id == session_id
                && event.generation_id == generation(session_id)
                && event.turn_id.is_none_or(|event_turn| event_turn == turn_id)
        }));
        assert!(
            events
                .events
                .windows(2)
                .all(|pair| pair[1].sequence == pair[0].sequence + 1)
        );
        assert!(
            events
                .events
                .iter()
                .any(|event| matches!(event.event, EventPayload::SessionStateChanged(_)))
        );
    }

    let mut cancellations = JoinSet::new();
    for index in 0..SESSIONS {
        let registry = Arc::clone(&registry);
        cancellations.spawn(async move {
            let session_id = id(0xd000 + index as u128);
            registry
                .cancel_turn(CancelTurnRequest {
                    session_id,
                    generation_id: generation(session_id),
                    turn_id: id(0xe000 + index as u128),
                })
                .await
        });
    }
    while let Some(result) = cancellations.join_next().await {
        let cancelled = result.unwrap().unwrap();
        assert_eq!(cancelled.outcome, CancelOutcome::Cancelled);
        assert_eq!(cancelled.session_state, SessionState::Ready);
    }

    let mut cleanup = JoinSet::new();
    for index in 0..SESSIONS {
        let registry = Arc::clone(&registry);
        cleanup.spawn(async move {
            close_and_unregister(&registry, id(0xd000 + index as u128)).await;
        });
    }
    while let Some(result) = cleanup.join_next().await {
        result.unwrap();
    }
    drop(terminals);
    wait_for_resources_released(&probe).await;

    let submissions = probe.submissions();
    assert_eq!(submissions.len(), SESSIONS);
    for index in 0..SESSIONS {
        assert!(submissions.iter().any(|submission| {
            submission.session_id == id(0xd000 + index as u128)
                && submission.turn_id == id(0xe000 + index as u128)
                && submission.prompt == format!("session-{index}")
        }));
    }
    assert_eq!(probe.interrupts(), SESSIONS);
    assert_eq!(probe.closes(), SESSIONS);
}
