mod process_support;
mod support;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pseudomux_protocol::v1::{
    CancelOutcome, CancelTurnRequest, ClosePolicy, DisconnectAction, ErrorCode, EventPayload,
    InspectSessionRequest, Request, RequestEnvelope, ResponseEnvelope, ResponseResult,
    SessionHandle, SessionState, SubscribeEventsRequest, TurnAccepted, TurnLeasePolicy,
    TurnOutcome, TurnRequest,
};
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::sync::Barrier;
use tokio::task::JoinSet;
use tokio::time::Instant;
use uuid::Uuid;

use process_support::actual_daemon::ActualDaemon;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "launches exact candidate pmuxd/private-rmux/fake-Claude binaries; credential-free"]
async fn actual_daemon_concurrent_private_ptys_never_cross_session_input_or_transcripts() {
    const SESSIONS: usize = 4;
    let mut daemon = ActualDaemon::start().await.unwrap();
    let client = daemon.client();

    let mut starts = JoinSet::new();
    for index in 0..SESSIONS {
        let client = client.clone();
        let request = daemon.start_request(id(0x10_000 + index as u128));
        starts.spawn(async move { client.start_session(request).await });
    }
    let mut handles = BTreeMap::new();
    while let Some(result) = starts.join_next().await {
        let handle = result.unwrap().unwrap();
        assert_eq!(handle.state, SessionState::Ready);
        assert!(handles.insert(handle.session_id, handle).is_none());
    }
    assert_eq!(handles.len(), SESSIONS);
    let launched = daemon.launched_processes(SESSIONS).await.unwrap();
    assert_eq!(launched.len(), SESSIONS);

    let mut turns = JoinSet::new();
    for (index, handle) in handles.values().cloned().enumerate() {
        let client = client.clone();
        let prompt = format!("PMUX_TEST_ECHO:isolated-session-{index}");
        turns.spawn(async move {
            let accepted = client
                .run_turn(
                    handle.session_id,
                    handle.generation_id,
                    ActualDaemon::turn(prompt.clone()),
                )
                .await?;
            Ok::<_, pseudomux_client::ClientError>((handle, accepted, prompt))
        });
    }
    let mut accepted_turns = Vec::new();
    while let Some(result) = turns.join_next().await {
        accepted_turns.push(result.unwrap().unwrap());
    }

    let mut completions = JoinSet::new();
    for (handle, accepted, prompt) in accepted_turns {
        let client = client.clone();
        completions.spawn(async move {
            let result = wait_for_public_result(&client, &handle, &accepted).await;
            (handle, accepted, prompt, result)
        });
    }
    let mut completed = BTreeMap::new();
    while let Some(joined) = completions.join_next().await {
        let (handle, accepted, prompt, result) = joined.unwrap();
        let result = result.unwrap();
        let expected_suffix = prompt.strip_prefix("PMUX_TEST_ECHO:").unwrap();
        assert_eq!(result.text, format!("pmux-test-echo:{expected_suffix}"));
        assert_eq!(result.session_id, handle.session_id);
        assert_eq!(result.generation_id, handle.generation_id);
        assert_eq!(result.turn_id, accepted.turn_id);
        assert_eq!(result.outcome, TurnOutcome::Completed);
        assert!(
            completed
                .insert(handle.session_id, (handle, prompt))
                .is_none()
        );
    }
    assert_eq!(completed.len(), SESSIONS);

    let all_prompts = completed
        .values()
        .map(|(_, prompt)| prompt.clone())
        .collect::<Vec<_>>();
    for (session_id, (_, own_prompt)) in &completed {
        let transcript = std::fs::read_to_string(daemon.transcript_path(*session_id)).unwrap();
        let rows = transcript
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        let typed_prompts = rows
            .iter()
            .filter(|row| row.get("promptSource").and_then(|value| value.as_str()) == Some("typed"))
            .filter_map(|row| {
                row.pointer("/message/content")
                    .and_then(|value| value.as_str())
            })
            .collect::<Vec<_>>();
        assert_eq!(typed_prompts, vec![own_prompt.as_str()]);
        for other_prompt in &all_prompts {
            assert_eq!(
                transcript.contains(other_prompt),
                other_prompt == own_prompt
            );
        }
    }

    let mut closes = JoinSet::new();
    for (handle, _) in completed.into_values() {
        let client = client.clone();
        closes.spawn(async move {
            client
                .close_session(handle.session_id, handle.generation_id, ClosePolicy::Force)
                .await
        });
    }
    while let Some(result) = closes.join_next().await {
        assert!(result.unwrap().unwrap().process_reaped);
    }
    daemon.assert_processes_absent(&launched).await.unwrap();
    daemon.stop().await.unwrap();
}

/// Twenty turns cut short mid-terminal-operation must leave the daemon whole.
///
/// This is the daemon-level statement of the transport-cancellation fix, and
/// the only one of its regressions that goes through a real `pmuxd` rather than
/// through `PrivateRuntime` directly. Everything between the client and the leaf
/// participates: the session actor, `TurnWorker::await_turn_step`, the driver's
/// input gate, and the private rmux backend they share.
///
/// A turn deadline expiring is not an exotic fault. It is the ordinary outcome
/// of any caller-supplied `deadline_unix_ms`, and when it fires `await_turn_step`
/// drops whatever terminal call was in flight -- `submit_prompt`'s gate loop is
/// polling snapshots roughly half the wall clock -- so on the unfixed leaf a
/// routine timeout permanently kills the one transport the whole daemon shares.
///
/// Two shapes of evidence, because they fail differently:
///
/// * A **bystander** session, started before any cancellation and never
///   cancelled itself, must complete an ordinary turn after every wave. This is
///   the head-of-line statement: one caller's timeout must not reach another
///   caller's session.
/// * A **new** session must still start and complete a turn at the end. This is
///   the admission statement, and it is the one that failed first in three of
///   the four runs measured on the unfixed leaf: `start_session` began failing
///   with `RmuxUnavailable` or `DaemonLost` inside the first or second wave and
///   never recovered. The fourth failed on the bystander instead.
///
/// The deadlines sweep from 8 ms upward rather than sitting at one value. The
/// interesting drop is one that lands while a request is outstanding, and the
/// fraction of a turn spent that way is a ratio of two numbers this test does
/// not control -- a snapshot round trip against the driver's 25 ms poll gap --
/// so a single fixed deadline would sample one phase of that cycle twenty
/// times. Spreading them is what makes the unfixed leaf reliably red; a version
/// of this test that fired every deadline into the inter-poll sleep would be
/// green on both sides and worse than nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "launches exact candidate pmuxd/private-rmux/fake-Claude binaries; credential-free"]
async fn actual_daemon_survives_turns_cancelled_while_terminal_operations_are_in_flight() {
    const WAVES: usize = 5;
    const CANCELLED_PER_WAVE: usize = 4;
    let mut daemon = ActualDaemon::start().await.unwrap();
    let client = daemon.client();

    let bystander = client
        .start_session(daemon.start_request(id(0xca00)))
        .await
        .expect("the bystander session must start before any turn is cancelled");
    assert_eq!(bystander.state, SessionState::Ready);
    complete_bystander_turn(&daemon, &bystander, "baseline").await;

    for wave in 0..WAVES {
        let mut cancelled = JoinSet::new();
        for index in 0..CANCELLED_PER_WAVE {
            let ordinal = wave * CANCELLED_PER_WAVE + index;
            let client = client.clone();
            let request = daemon.start_request(id(0xcb00 + ordinal as u128));
            // 50, 60, ... 240 ms, measured from the moment the request is
            // built rather than from the moment the actor starts the turn. The
            // sweep spans several of the driver's 25 ms poll gaps, so the
            // deadlines land at many different points of the gate loop's
            // request/sleep cycle instead of sampling one phase of it twenty
            // times. The floor is not decorative: a deadline that expires
            // before the daemon admits the turn is rejected outright with
            // `turn deadline already elapsed`, and a rejected turn never had
            // anything in flight. An 8 ms floor was measured doing exactly that
            // on two runs out of ten.
            let budget = Duration::from_millis(50 + 10 * ordinal as u64);
            cancelled.spawn(async move {
                let handle = client.start_session(request).await.unwrap_or_else(|error| {
                    panic!(
                        "session {ordinal} could not start while earlier turns were being cancelled: {error:?}"
                    )
                });
                let accepted = client
                    .run_turn(
                        handle.session_id,
                        handle.generation_id,
                        TurnRequest {
                            turn_id: Uuid::new_v4(),
                            prompt: format!("PMUX_TEST_ECHO:cancelled-{ordinal}"),
                            deadline_unix_ms: Some(deadline_after(budget)),
                            lease: TurnLeasePolicy {
                                on_disconnect: DisconnectAction::Continue,
                                heartbeat_timeout_ms: None,
                            },
                        },
                    )
                    .await
                    .unwrap_or_else(|error| {
                        panic!("turn {ordinal} was refused rather than accepted: {error:?}")
                    });
                let outcome = wait_for_public_outcome(&client, &handle, &accepted).await;
                let _ = client
                    .close_session(handle.session_id, handle.generation_id, ClosePolicy::Force)
                    .await;
                (ordinal, budget, outcome)
            });
        }
        while let Some(joined) = cancelled.join_next().await {
            let (ordinal, budget, outcome) = joined.unwrap();
            // The precondition, not the point: a turn that *completed* inside
            // its budget never had anything in flight to abandon, and would
            // make the health assertions below vacuous.
            assert_eq!(
                outcome,
                Err(ErrorCode::TurnTimeout),
                "turn {ordinal} with a {budget:?} budget did not time out"
            );
        }

        daemon
            .wait_for_ready(&bystander)
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "the bystander session was not ready after cancellation wave {wave}: {error:?}"
                )
            });
        complete_bystander_turn(&daemon, &bystander, &format!("after-wave-{wave}")).await;
    }

    let fresh = client
        .start_session(daemon.start_request(id(0xcc00)))
        .await
        .expect("a new session must still start after twenty cancelled turns");
    let accepted = client
        .run_turn(
            fresh.session_id,
            fresh.generation_id,
            ActualDaemon::turn("PMUX_TEST_ECHO:after-cancellation"),
        )
        .await
        .expect("a new session must still accept a turn after twenty cancelled turns");
    let result = daemon.wait_for_result(&fresh, &accepted).await.unwrap();
    assert_eq!(result.text, "pmux-test-echo:after-cancellation");

    for handle in [&bystander, &fresh] {
        assert!(
            client
                .close_session(handle.session_id, handle.generation_id, ClosePolicy::Force)
                .await
                .unwrap()
                .process_reaped
        );
    }
    daemon.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "launches exact candidate pmuxd/private-rmux/fake-Claude binaries; credential-free"]
async fn actual_daemon_slow_and_disconnected_event_subscribers_leave_one_of_64_slots_live() {
    const DISCONNECTED_SUBSCRIBERS: usize = 8;
    const SLOW_SUBSCRIBERS: usize = 16;
    const OTHER_HELD_CONNECTIONS: usize = 47;
    const LARGE_RESULT_BYTES: usize = 2 * 1024 * 1024;

    let mut daemon = ActualDaemon::start().await.unwrap();
    let client = daemon.client();
    let first = client
        .start_session(daemon.start_request(id(0x20_001)))
        .await
        .unwrap();
    let unrelated = client
        .start_session(daemon.start_request(id(0x20_002)))
        .await
        .unwrap();
    let launched = daemon.launched_processes(2).await.unwrap();

    let before = client
        .inspect_session(first.session_id, first.generation_id)
        .await
        .unwrap()
        .last_sequence;
    let large_turn = ActualDaemon::turn(format!("PMUX_TEST_LARGE_RESULT:{LARGE_RESULT_BYTES}"));
    let large_accepted = client
        .run_turn(first.session_id, first.generation_id, large_turn)
        .await
        .unwrap();
    daemon.wait_for_ready(&first).await.unwrap();
    let baseline_fds = daemon.daemon_resources().unwrap().open_fds;

    for _ in 0..DISCONNECTED_SUBSCRIBERS {
        let stream = open_unread_subscription(daemon.socket(), &first, before).await;
        drop(stream);
    }
    wait_for_daemon_fd_ceiling(&daemon, baseline_fds).await;

    let mut slow_subscribers = Vec::new();
    for _ in 0..SLOW_SUBSCRIBERS {
        slow_subscribers.push(open_unread_subscription(daemon.socket(), &first, before).await);
    }
    let mut other_connections = Vec::new();
    for index in 0..OTHER_HELD_CONNECTIONS {
        let mut stream = UnixStream::connect(daemon.socket()).await.unwrap();
        if index % 2 == 0 {
            stream.write_all(&[0]).await.unwrap();
        }
        other_connections.push(stream);
    }
    assert_eq!(SLOW_SUBSCRIBERS + OTHER_HELD_CONNECTIONS, 63);
    wait_for_daemon_fd_floor(&daemon, baseline_fds + 63).await;

    let mut unrelated_turn = ActualDaemon::turn("PMUX_TEST_ECHO:unrelated-under-backpressure");
    unrelated_turn.deadline_unix_ms = Some(deadline_after(Duration::from_secs(7)));
    let unrelated_accepted = client
        .run_turn(
            unrelated.session_id,
            unrelated.generation_id,
            unrelated_turn,
        )
        .await
        .unwrap();
    let unrelated_result = wait_for_public_result(&client, &unrelated, &unrelated_accepted)
        .await
        .unwrap();
    assert_eq!(
        unrelated_result.text,
        "pmux-test-echo:unrelated-under-backpressure"
    );

    let mut sixty_fourth = UnixStream::connect(daemon.socket()).await.unwrap();
    sixty_fourth.write_all(&[0]).await.unwrap();
    wait_for_daemon_fd_floor(&daemon, baseline_fds + 64).await;
    let mut sixty_fifth = tokio::spawn({
        let client = client.clone();
        async move { client.ping().await }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(250), &mut sixty_fifth)
            .await
            .is_err(),
        "a 65th public request bypassed the exact 64-connection service bound"
    );
    drop(sixty_fourth);
    tokio::time::timeout(Duration::from_secs(2), sixty_fifth)
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    drop(other_connections);
    drop(slow_subscribers);
    wait_for_daemon_fd_ceiling(&daemon, baseline_fds).await;
    client.ping().await.unwrap();

    let large_result = wait_for_public_result(&client, &first, &large_accepted)
        .await
        .unwrap();
    assert_eq!(large_result.text.len(), LARGE_RESULT_BYTES);
    assert!(large_result.text.bytes().all(|byte| byte == b'r'));

    for handle in [&first, &unrelated] {
        let closed = client
            .close_session(handle.session_id, handle.generation_id, ClosePolicy::Force)
            .await
            .unwrap();
        assert!(closed.process_reaped);
    }
    daemon.assert_processes_absent(&launched).await.unwrap();
    daemon.stop().await.unwrap();
}

/// Runs one ordinary turn on the bystander session and proves it produced the
/// exact echo it asked for.
///
/// A turn that merely *completes* is not enough evidence here: the point is
/// that this session's own input still reached its own pane and came back, so
/// the returned text is compared rather than only the outcome.
async fn complete_bystander_turn(daemon: &ActualDaemon, handle: &SessionHandle, tag: &str) {
    let client = daemon.client();
    let accepted = client
        .run_turn(
            handle.session_id,
            handle.generation_id,
            ActualDaemon::turn(format!("PMUX_TEST_ECHO:bystander-{tag}")),
        )
        .await
        .unwrap_or_else(|error| panic!("bystander turn {tag} was refused: {error:?}"));
    let result = daemon
        .wait_for_result(handle, &accepted)
        .await
        .unwrap_or_else(|error| panic!("bystander turn {tag} did not complete: {error:?}"));
    assert_eq!(result.text, format!("pmux-test-echo:bystander-{tag}"));
}

/// Waits for whichever terminal event a turn produced, without deciding in
/// advance which one it should have been: `Ok` is a `TurnCompleted` outcome and
/// `Err` is a `TurnFailed` code.
///
/// [`wait_for_public_result`] panics on `TurnFailed`, which is right for turns
/// that are supposed to succeed and useless for turns that are supposed to be
/// cut short.
async fn wait_for_public_outcome(
    client: &pseudomux_client::PmuxClient,
    handle: &SessionHandle,
    accepted: &TurnAccepted,
) -> Result<TurnOutcome, ErrorCode> {
    let mut after_sequence = accepted.next_sequence.saturating_sub(1);
    for _ in 0..60 {
        let batch = client
            .subscribe_events(SubscribeEventsRequest {
                session_id: handle.session_id,
                generation_id: handle.generation_id,
                after_sequence,
                wait_ms: 1_000,
                max_events: 128,
            })
            .await
            .expect("event subscription failed while awaiting a cancelled turn");
        assert!(batch.replay_gap.is_none());
        for event in batch.events {
            after_sequence = event.sequence;
            match event.event {
                EventPayload::TurnCompleted(result) if result.turn_id == accepted.turn_id => {
                    return Ok(result.outcome);
                }
                EventPayload::TurnFailed(error) if event.turn_id == Some(accepted.turn_id) => {
                    return Err(error.code);
                }
                _ => {}
            }
        }
    }
    panic!("turn {} produced no terminal event", accepted.turn_id)
}

async fn wait_for_public_result(
    client: &pseudomux_client::PmuxClient,
    handle: &SessionHandle,
    accepted: &TurnAccepted,
) -> Result<pseudomux_protocol::v1::TurnResult, pseudomux_client::ClientError> {
    let mut after_sequence = accepted.next_sequence.saturating_sub(1);
    for _ in 0..60 {
        let batch = client
            .subscribe_events(SubscribeEventsRequest {
                session_id: handle.session_id,
                generation_id: handle.generation_id,
                after_sequence,
                wait_ms: 1_000,
                max_events: 128,
            })
            .await?;
        assert!(batch.replay_gap.is_none());
        for event in batch.events {
            after_sequence = event.sequence;
            match event.event {
                EventPayload::TurnCompleted(result) if result.turn_id == accepted.turn_id => {
                    return Ok(*result);
                }
                EventPayload::TurnFailed(error) if event.turn_id == Some(accepted.turn_id) => {
                    panic!("turn failed unexpectedly with {:?}", error.code);
                }
                _ => {}
            }
        }
    }
    panic!("turn {} did not complete", accepted.turn_id)
}

async fn open_unread_subscription(
    socket: &std::path::Path,
    handle: &SessionHandle,
    after_sequence: u64,
) -> UnixStream {
    let request = RequestEnvelope::new(
        Uuid::new_v4(),
        Request::SubscribeEvents(SubscribeEventsRequest {
            session_id: handle.session_id,
            generation_id: handle.generation_id,
            after_sequence,
            wait_ms: 30_000,
            max_events: 512,
        }),
    );
    let payload = serde_json::to_vec(&request).unwrap();
    let mut stream = UnixStream::connect(socket).await.unwrap();
    stream
        .write_all(&(payload.len() as u32).to_be_bytes())
        .await
        .unwrap();
    stream.write_all(&payload).await.unwrap();
    stream
}

async fn wait_for_daemon_fd_floor(daemon: &ActualDaemon, minimum: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let observed = daemon.daemon_resources().unwrap().open_fds;
        if observed >= minimum {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "daemon opened only {observed} descriptors, expected at least {minimum}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_for_daemon_fd_ceiling(daemon: &ActualDaemon, maximum: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let observed = daemon.daemon_resources().unwrap().open_fds;
        if observed <= maximum {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "daemon retained {observed} descriptors, expected at most {maximum}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn deadline_after(duration: Duration) -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let delta = duration.as_millis();
    (now + delta).try_into().unwrap()
}
