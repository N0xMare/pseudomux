mod support;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use proptest::{
    prelude::*,
    test_runner::{Config as ProptestConfig, RngAlgorithm, RngSeed},
};
use pseudomux_claude::{CompleteLine, JsonlParser, ParseMode, ParsedRow, SourceLocation};
use pseudomux_protocol::v1::{
    CancelOutcome, CancelTurnRequest, ClosePolicy, CloseSessionRequest, CompatibilityReport,
    ErrorCode, EventBatch, EventPayload, InputTransport, InspectSessionRequest, NeedsInput,
    NeedsInputKind, ResponseEnvelope, ResponseResult, RunTurnRequest, SessionState,
    SubscribeEventsRequest, TerminalProfile, TurnRequest,
};
use pseudomux_rmux::{TerminalCursor, TerminalSnapshot};
use pseudomux_service::driver_io::{TerminalScreenState, classify_terminal_snapshot};
use pseudomux_service::v1::{
    DriverFailure, DriverResult, SessionCell, SessionRegistration, StoredTurnTerminal,
    TerminalEvidence, TerminalScreenObservation, TranscriptArm, TranscriptBatch,
    TranscriptDrainEvidence, TranscriptPosition, TranscriptSource, WritableAttachCompletion,
    is_valid_session_transition,
};

use support::{
    Probe, TestTerminal, TestTranscript, actor_config, close_and_unregister, generation, id,
    register, registry_with_config, turn, unregister_after_exit, wait_for_resources_released,
};

const SESSION: u128 = 0xa001;
const TURN_BASE: u128 = 0xb000;
const TURN_CAPACITY: usize = 4;
const LIFECYCLE_SESSION: u128 = 0xa101;
const LIFECYCLE_TURN_BASE: u128 = 0xc000;
const LIFECYCLE_ATTACH_BASE: u128 = 0xd000;
const REPLAY_SESSION: u128 = 0xa201;
const REPLAY_TURN_BASE: u128 = 0xe000;

#[derive(Clone, Debug)]
enum ModelCommand {
    Submit { slot: u8, prompt: u8 },
    Replay { selector: u8 },
    Conflict { selector: u8 },
    Inspect,
    Subscribe { lag: u8, maximum: u8 },
    Cancel { slot: u8 },
    Close,
}

fn command_strategy() -> impl Strategy<Value = ModelCommand> {
    prop_oneof![
        6 => (0_u8..8, 0_u8..6).prop_map(|(slot, prompt)| ModelCommand::Submit { slot, prompt }),
        2 => (0_u8..16).prop_map(|selector| ModelCommand::Replay { selector }),
        2 => (0_u8..16).prop_map(|selector| ModelCommand::Conflict { selector }),
        2 => Just(ModelCommand::Inspect),
        2 => (0_u8..16, 0_u8..32).prop_map(|(lag, maximum)| ModelCommand::Subscribe { lag, maximum }),
        3 => (0_u8..8).prop_map(|slot| ModelCommand::Cancel { slot }),
        1 => Just(ModelCommand::Close),
    ]
}

fn deterministic_config() -> ProptestConfig {
    ProptestConfig {
        max_shrink_iters: 10_000,
        failure_persistence: None,
        rng_algorithm: RngAlgorithm::ChaCha,
        rng_seed: RngSeed::Fixed(0x504d_5558_4143_544f),
        ..ProptestConfig::default()
    }
}

#[test]
fn deterministic_config_preserves_the_gate_requested_case_count() {
    assert_eq!(
        deterministic_config().cases,
        ProptestConfig::default().cases
    );
}

#[derive(Clone, Debug)]
struct ModelTurn {
    prompt: String,
    terminal: bool,
}

#[derive(Default)]
struct ActorModel {
    active: Option<u8>,
    turns: BTreeMap<u8, ModelTurn>,
    closed: bool,
}

impl ActorModel {
    fn selected(&self, selector: u8) -> Option<(u8, &ModelTurn)> {
        let index = usize::from(selector) % self.turns.len().max(1);
        self.turns
            .iter()
            .nth(index)
            .map(|(slot, turn)| (*slot, turn))
    }
}

proptest! {
    #![proptest_config(deterministic_config())]

    #[test]
    fn command_sequence_matches_single_owner_actor_model(
        commands in prop::collection::vec(command_strategy(), 1..64),
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(run_model_sequence(commands));
    }
}

async fn run_model_sequence(commands: Vec<ModelCommand>) {
    let mut config = actor_config();
    config.turn_history_capacity = TURN_CAPACITY;
    let registry = registry_with_config(config);
    let probe = Arc::new(Probe::default());
    let terminal = Arc::new(TestTerminal::new(Arc::clone(&probe)));
    let workspace = tempfile::tempdir().unwrap();
    register(
        &registry,
        id(SESSION),
        Arc::clone(&terminal),
        Arc::new(TestTranscript::pending(Arc::clone(&probe))),
        workspace.path(),
    )
    .await;
    let mut model = ActorModel::default();

    for command in commands {
        match command {
            ModelCommand::Submit { slot, prompt } => {
                let prompt = format!("prompt-{prompt}");
                let turn_id = id(TURN_BASE + u128::from(slot));
                let before = if !model.closed && model.active.is_none() {
                    Some(
                        registry
                            .inspect(InspectSessionRequest {
                                session_id: id(SESSION),
                                generation_id: generation(id(SESSION)),
                            })
                            .await
                            .unwrap(),
                    )
                } else {
                    None
                };
                let before_submissions = probe.submissions().len();
                let result = registry
                    .run_turn(turn(id(SESSION), turn_id, prompt.clone()))
                    .await;

                if model.closed {
                    assert_eq!(result.unwrap_err().code, ErrorCode::DaemonLost);
                } else if let Some(existing) = model.turns.get(&slot) {
                    if existing.prompt == prompt {
                        let accepted = result.unwrap();
                        assert!(accepted.replayed);
                        assert_eq!(accepted.turn_id, turn_id);
                        assert_eq!(accepted.generation_id, generation(id(SESSION)));
                    } else {
                        assert_eq!(result.unwrap_err().code, ErrorCode::IdConflict);
                    }
                } else if model.active.is_some() {
                    assert_eq!(result.unwrap_err().code, ErrorCode::SessionBusy);
                } else if model.turns.len() == TURN_CAPACITY {
                    assert_eq!(
                        result.unwrap_err().code,
                        ErrorCode::TurnHistoryCapacityExceeded
                    );
                    let after = registry
                        .inspect(InspectSessionRequest {
                            session_id: id(SESSION),
                            generation_id: generation(id(SESSION)),
                        })
                        .await
                        .unwrap();
                    assert_eq!(Some(&after), before.as_ref());
                    assert_eq!(probe.submissions().len(), before_submissions);
                } else {
                    let accepted = result.unwrap();
                    assert!(!accepted.replayed);
                    assert_eq!(accepted.state, SessionState::Submitting);
                    assert_eq!(accepted.generation_id, generation(id(SESSION)));
                    model.turns.insert(
                        slot,
                        ModelTurn {
                            prompt,
                            terminal: false,
                        },
                    );
                    model.active = Some(slot);
                }
            }
            ModelCommand::Replay { selector } => {
                if let Some((slot, stored)) = model.selected(selector) {
                    let before_submissions = probe.submissions().len();
                    let result = registry
                        .run_turn(turn(
                            id(SESSION),
                            id(TURN_BASE + u128::from(slot)),
                            stored.prompt.clone(),
                        ))
                        .await;
                    if model.closed {
                        assert_eq!(result.unwrap_err().code, ErrorCode::DaemonLost);
                    } else {
                        let accepted = result.unwrap();
                        assert!(accepted.replayed);
                        assert_eq!(accepted.generation_id, generation(id(SESSION)));
                        assert_eq!(probe.submissions().len(), before_submissions);
                    }
                }
            }
            ModelCommand::Conflict { selector } => {
                if let Some((slot, _)) = model.selected(selector) {
                    let result = registry
                        .run_turn(turn(
                            id(SESSION),
                            id(TURN_BASE + u128::from(slot)),
                            format!("conflicting-{selector}"),
                        ))
                        .await;
                    if model.closed {
                        assert_eq!(result.unwrap_err().code, ErrorCode::DaemonLost);
                    } else {
                        assert_eq!(result.unwrap_err().code, ErrorCode::IdConflict);
                    }
                }
            }
            ModelCommand::Inspect => {
                let result = registry
                    .inspect(InspectSessionRequest {
                        session_id: id(SESSION),
                        generation_id: generation(id(SESSION)),
                    })
                    .await;
                if model.closed {
                    assert_eq!(result.unwrap_err().code, ErrorCode::DaemonLost);
                } else {
                    let snapshot = result.unwrap();
                    assert_eq!(snapshot.session_id, id(SESSION));
                    assert_eq!(snapshot.generation_id, generation(id(SESSION)));
                    assert_eq!(
                        snapshot.active_turn_id,
                        model.active.map(|slot| id(TURN_BASE + u128::from(slot)))
                    );
                    if model.active.is_none() {
                        assert_eq!(snapshot.state, SessionState::Ready);
                    }
                }
            }
            ModelCommand::Subscribe { lag, maximum } => {
                if model.closed {
                    let error = registry
                        .events(SubscribeEventsRequest {
                            session_id: id(SESSION),
                            generation_id: generation(id(SESSION)),
                            after_sequence: 0,
                            wait_ms: 0,
                            max_events: u32::from(maximum),
                        })
                        .await
                        .unwrap_err();
                    assert_eq!(error.code, ErrorCode::DaemonLost);
                } else {
                    let snapshot = registry
                        .inspect(InspectSessionRequest {
                            session_id: id(SESSION),
                            generation_id: generation(id(SESSION)),
                        })
                        .await
                        .unwrap();
                    let after_sequence = snapshot.last_sequence.saturating_sub(u64::from(lag));
                    let batch = registry
                        .events(SubscribeEventsRequest {
                            session_id: id(SESSION),
                            generation_id: generation(id(SESSION)),
                            after_sequence,
                            wait_ms: 0,
                            max_events: u32::from(maximum),
                        })
                        .await
                        .unwrap();
                    assert!(batch.replay_gap.is_none());
                    assert!(batch.events.iter().all(|event| {
                        event.session_id == id(SESSION)
                            && event.generation_id == generation(id(SESSION))
                            && event.sequence > after_sequence
                    }));
                    assert!(
                        batch
                            .events
                            .windows(2)
                            .all(|events| events[1].sequence == events[0].sequence + 1)
                    );
                }
            }
            ModelCommand::Cancel { slot } => {
                let result = registry
                    .cancel_turn(CancelTurnRequest {
                        session_id: id(SESSION),
                        generation_id: generation(id(SESSION)),
                        turn_id: id(TURN_BASE + u128::from(slot)),
                    })
                    .await;
                if model.closed {
                    assert_eq!(result.unwrap_err().code, ErrorCode::DaemonLost);
                } else if model.turns.get(&slot).is_some_and(|turn| turn.terminal) {
                    let cancelled = result.unwrap();
                    assert_eq!(cancelled.outcome, CancelOutcome::AlreadyTerminal);
                    let snapshot = registry
                        .inspect(InspectSessionRequest {
                            session_id: id(SESSION),
                            generation_id: generation(id(SESSION)),
                        })
                        .await
                        .unwrap();
                    assert_eq!(cancelled.session_state, snapshot.state);
                    assert_eq!(
                        snapshot.active_turn_id,
                        model
                            .active
                            .map(|active| id(TURN_BASE + u128::from(active)))
                    );
                } else if model.active == Some(slot) {
                    let cancelled = result.unwrap();
                    assert_eq!(cancelled.outcome, CancelOutcome::Cancelled);
                    assert_eq!(cancelled.session_state, SessionState::Ready);
                    model.active = None;
                    model.turns.get_mut(&slot).unwrap().terminal = true;
                } else {
                    assert_eq!(result.unwrap_err().code, ErrorCode::IdConflict);
                }
            }
            ModelCommand::Close => {
                let result = registry
                    .close(CloseSessionRequest {
                        session_id: id(SESSION),
                        generation_id: generation(id(SESSION)),
                        policy: ClosePolicy::Force,
                    })
                    .await;
                if model.closed {
                    assert_eq!(result.unwrap_err().code, ErrorCode::DaemonLost);
                } else {
                    let closed = result.unwrap();
                    assert!(closed.process_reaped);
                    assert_eq!(closed.generation_id, generation(id(SESSION)));
                    model.closed = true;
                    model.active = None;
                }
            }
        }

        if !model.closed {
            assert_actor_invariants(&registry, &model).await;
        }
    }

    if model.closed {
        unregister_after_exit(&registry, id(SESSION)).await;
    } else {
        close_and_unregister(&registry, id(SESSION)).await;
    }
    drop(terminal);
    wait_for_resources_released(&probe).await;
}

async fn assert_actor_invariants(
    registry: &pseudomux_service::v1::SessionRegistry,
    model: &ActorModel,
) {
    let snapshot = registry
        .inspect(InspectSessionRequest {
            session_id: id(SESSION),
            generation_id: generation(id(SESSION)),
        })
        .await
        .unwrap();
    assert_eq!(
        snapshot.active_turn_id,
        model.active.map(|slot| id(TURN_BASE + u128::from(slot)))
    );

    let events = registry
        .events(SubscribeEventsRequest {
            session_id: id(SESSION),
            generation_id: generation(id(SESSION)),
            after_sequence: 0,
            wait_ms: 0,
            max_events: 512,
        })
        .await
        .unwrap();
    assert!(events.replay_gap.is_none());
    assert_eq!(events.events.first().map(|event| event.sequence), Some(1));
    assert!(
        events
            .events
            .windows(2)
            .all(|events| events[1].sequence == events[0].sequence + 1)
    );
    let mut state = SessionState::Creating;
    for event in &events.events {
        assert_eq!(event.session_id, id(SESSION));
        assert_eq!(event.generation_id, generation(id(SESSION)));
        if let EventPayload::SessionStateChanged(change) = &event.event {
            assert_eq!(change.previous, state);
            assert!(is_valid_session_transition(change.previous, change.current));
            state = change.current;
        }
    }
}

#[derive(Clone, Debug)]
struct ReplayScenario {
    replay_capacity: usize,
    replay_byte_capacity: usize,
    frame_limit: usize,
    default_page_events: usize,
    turns: u8,
    requested_page_events: u8,
    selected_cursor: u16,
    exercise_immediate_wait: bool,
}

fn replay_scenario_strategy() -> impl Strategy<Value = ReplayScenario> {
    (
        1_usize..65,
        2_048_usize..16_385,
        2_048_usize..6_145,
        1_usize..17,
        1_u8..13,
        0_u8..17,
        any::<u16>(),
        any::<bool>(),
    )
        .prop_map(
            |(
                replay_capacity,
                replay_byte_capacity,
                frame_limit,
                default_page_events,
                turns,
                requested_page_events,
                selected_cursor,
                exercise_immediate_wait,
            )| ReplayScenario {
                replay_capacity,
                replay_byte_capacity,
                frame_limit,
                default_page_events,
                turns,
                requested_page_events,
                selected_cursor,
                exercise_immediate_wait,
            },
        )
}

proptest! {
    #![proptest_config(deterministic_config())]

    #[test]
    fn production_actor_subscription_pages_match_cursor_gap_wait_and_frame_invariants(
        scenario in replay_scenario_strategy(),
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(run_replay_scenario(scenario));
    }
}

async fn run_replay_scenario(scenario: ReplayScenario) {
    let mut config = actor_config();
    config.replay_capacity = scenario.replay_capacity;
    config.replay_byte_capacity = scenario.replay_byte_capacity;
    config.max_frame_bytes = scenario.frame_limit;
    config.default_event_batch_size = scenario.default_page_events;
    config.turn_history_capacity = usize::from(scenario.turns) + 1;
    let registry = registry_with_config(config);
    let probe = Arc::new(Probe::default());
    let terminal = Arc::new(TestTerminal::new(Arc::clone(&probe)));
    let workspace = tempfile::tempdir().unwrap();
    register(
        &registry,
        id(REPLAY_SESSION),
        Arc::clone(&terminal),
        Arc::new(TestTranscript::pending(Arc::clone(&probe))),
        workspace.path(),
    )
    .await;

    for index in 0..scenario.turns {
        let turn_id = id(REPLAY_TURN_BASE + u128::from(index));
        registry
            .run_turn(turn(
                id(REPLAY_SESSION),
                turn_id,
                format!("replay-model-{index}"),
            ))
            .await
            .unwrap();
        terminal
            .wait_for_submission(id(REPLAY_SESSION), turn_id)
            .await;
        registry
            .cancel_turn(CancelTurnRequest {
                session_id: id(REPLAY_SESSION),
                generation_id: generation(id(REPLAY_SESSION)),
                turn_id,
            })
            .await
            .unwrap();
    }

    let snapshot = registry
        .inspect(InspectSessionRequest {
            session_id: id(REPLAY_SESSION),
            generation_id: generation(id(REPLAY_SESSION)),
        })
        .await
        .unwrap();
    let last_sequence = snapshot.last_sequence;
    let next_sequence = last_sequence + 1;
    let max_events = u32::from(scenario.requested_page_events);

    let first = registry
        .events(SubscribeEventsRequest {
            session_id: id(REPLAY_SESSION),
            generation_id: generation(id(REPLAY_SESSION)),
            after_sequence: 0,
            wait_ms: u64::from(scenario.exercise_immediate_wait) * 30_000,
            max_events,
        })
        .await
        .unwrap();
    assert_batch_fits(&first, scenario.frame_limit);
    let oldest_available = first
        .replay_gap
        .as_ref()
        .map_or(1, |gap| gap.oldest_available);
    if let Some(gap) = &first.replay_gap {
        assert!(first.events.is_empty());
        assert_eq!(gap.requested_after, 0);
        assert!(gap.oldest_available > 1);
        assert_eq!(gap.next_sequence, next_sequence);
        assert_eq!(gap.snapshot.last_sequence, last_sequence);
        assert_eq!(first.next_sequence, next_sequence);
    }

    // Walk the entire retained suffix using the production actor. This is a
    // cursor invariant model, not a duplicate of replay retention or paging:
    // the actor itself decides the retained boundary and page composition.
    let mut cursor = oldest_available - 1;
    let effective_count_limit = if max_events == 0 {
        scenario.default_page_events
    } else {
        usize::try_from(max_events).unwrap()
    };
    let mut observed = Vec::new();
    let mut observed_event_bytes = 0_usize;
    while cursor < last_sequence {
        let batch = registry
            .events(SubscribeEventsRequest {
                session_id: id(REPLAY_SESSION),
                generation_id: generation(id(REPLAY_SESSION)),
                after_sequence: cursor,
                wait_ms: u64::from(scenario.exercise_immediate_wait) * 30_000,
                max_events,
            })
            .await
            .unwrap();
        assert!(batch.replay_gap.is_none());
        assert!(
            !batch.events.is_empty(),
            "a retained page must make progress"
        );
        assert!(batch.events.len() <= effective_count_limit);
        assert_eq!(batch.events[0].sequence, cursor + 1);
        assert!(
            batch
                .events
                .windows(2)
                .all(|pair| pair[1].sequence == pair[0].sequence + 1)
        );
        assert!(batch.events.iter().all(|event| {
            event.session_id == id(REPLAY_SESSION)
                && event.generation_id == generation(id(REPLAY_SESSION))
        }));
        assert_eq!(
            batch.next_sequence,
            batch.events.last().unwrap().sequence + 1
        );
        assert_batch_fits(&batch, scenario.frame_limit);
        let page_end_cursor = batch.next_sequence - 1;
        if batch.events.len() < effective_count_limit && page_end_cursor < last_sequence {
            let lookahead = registry
                .events(SubscribeEventsRequest {
                    session_id: id(REPLAY_SESSION),
                    generation_id: generation(id(REPLAY_SESSION)),
                    after_sequence: page_end_cursor,
                    wait_ms: 0,
                    max_events: 1,
                })
                .await
                .unwrap();
            assert!(lookahead.replay_gap.is_none());
            let next = lookahead
                .events
                .first()
                .expect("the next retained event must remain observable");
            assert_eq!(next.sequence, page_end_cursor + 1);
            let mut candidate_events = batch.events.clone();
            candidate_events.push(next.clone());
            let candidate = EventBatch {
                events: candidate_events,
                next_sequence: next.sequence + 1,
                replay_gap: None,
            };
            let candidate_bytes = encoded_batch_bytes(&candidate);
            assert!(
                candidate_bytes > scenario.frame_limit,
                "a byte-limited page stopped even though its next retained event made a {candidate_bytes}-byte response within the {}-byte frame",
                scenario.frame_limit
            );
        }
        observed_event_bytes += batch
            .events
            .iter()
            .map(|event| serde_json::to_vec(event).unwrap().len())
            .sum::<usize>();
        observed.extend(batch.events.iter().map(|event| event.sequence));
        cursor = page_end_cursor;
    }
    if oldest_available <= last_sequence {
        assert_eq!(observed.first().copied(), Some(oldest_available));
        assert_eq!(observed.last().copied(), Some(last_sequence));
        assert_eq!(observed.len() as u64, last_sequence - oldest_available + 1);
        assert!(observed.len() <= scenario.replay_capacity);
        assert!(observed_event_bytes <= scenario.replay_byte_capacity);
    } else {
        assert!(observed.is_empty());
    }

    let selected = u64::from(scenario.selected_cursor) % (next_sequence + 2);
    let selected_result = tokio::time::timeout(
        Duration::from_millis(100),
        registry.events(SubscribeEventsRequest {
            session_id: id(REPLAY_SESSION),
            generation_id: generation(id(REPLAY_SESSION)),
            after_sequence: selected,
            wait_ms: if selected < last_sequence { 30_000 } else { 0 },
            max_events,
        }),
    )
    .await
    .expect("available events and replay gaps must bypass the long-poll wait");
    if selected >= next_sequence {
        assert_eq!(selected_result.unwrap_err().code, ErrorCode::InvalidConfig);
    } else {
        let batch = selected_result.unwrap();
        assert_batch_fits(&batch, scenario.frame_limit);
        if selected + 1 < oldest_available {
            let gap = batch
                .replay_gap
                .expect("an evicted cursor must report a gap");
            assert!(batch.events.is_empty());
            assert_eq!(gap.requested_after, selected);
            assert_eq!(gap.oldest_available, oldest_available);
            assert_eq!(gap.next_sequence, next_sequence);
        } else if selected < last_sequence {
            assert!(batch.replay_gap.is_none());
            assert_eq!(batch.events.first().unwrap().sequence, selected + 1);
        } else {
            assert!(batch.replay_gap.is_none());
            assert!(batch.events.is_empty());
            assert_eq!(batch.next_sequence, next_sequence);
        }
    }

    // The exact current cursor is the only no-data long-poll cursor. A bounded
    // timeout returns a coherent empty batch without manufacturing progress.
    let empty = registry
        .events(SubscribeEventsRequest {
            session_id: id(REPLAY_SESSION),
            generation_id: generation(id(REPLAY_SESSION)),
            after_sequence: last_sequence,
            wait_ms: 1,
            max_events,
        })
        .await
        .unwrap();
    assert!(empty.events.is_empty());
    assert!(empty.replay_gap.is_none());
    assert_eq!(empty.next_sequence, next_sequence);
    assert_batch_fits(&empty, scenario.frame_limit);

    // A cursor claiming an event that has not been published fails before any
    // cursor can be advanced or history can be skipped.
    let future = registry
        .events(SubscribeEventsRequest {
            session_id: id(REPLAY_SESSION),
            generation_id: generation(id(REPLAY_SESSION)),
            after_sequence: next_sequence,
            wait_ms: 30_000,
            max_events,
        })
        .await
        .unwrap_err();
    assert_eq!(future.code, ErrorCode::InvalidConfig);
    assert_eq!(future.details["requested_after"], next_sequence);
    assert_eq!(future.details["last_sequence"], last_sequence);

    close_and_unregister(&registry, id(REPLAY_SESSION)).await;
    drop(terminal);
    wait_for_resources_released(&probe).await;
}

fn assert_batch_fits(batch: &pseudomux_protocol::v1::EventBatch, frame_limit: usize) {
    let encoded = encoded_batch_bytes(batch);
    assert!(
        encoded <= frame_limit,
        "{encoded}-byte event batch exceeded {frame_limit}-byte frame limit",
    );
}

fn encoded_batch_bytes(batch: &pseudomux_protocol::v1::EventBatch) -> usize {
    serde_json::to_vec(&ResponseEnvelope::success(
        id(0),
        ResponseResult::Events(batch.clone()),
    ))
    .unwrap()
    .len()
}

#[derive(Clone, Copy, Debug)]
enum LifecycleCommand {
    Submit { slot: u8, prompt: u8, mode: u8 },
    CompleteActive,
    Cancel { slot: u8 },
    SetModal { present: bool },
    ReserveAttach { slot: u8 },
    ReleaseAttach { slot: u8 },
    AdvanceIdleClock { past_deadline: bool },
    Close { process_reaped: bool },
    Inspect,
}

fn lifecycle_command_strategy() -> impl Strategy<Value = LifecycleCommand> {
    prop_oneof![
        6 => (0_u8..8, 0_u8..8, 0_u8..3).prop_map(|(slot, prompt, mode)| {
            LifecycleCommand::Submit { slot, prompt, mode }
        }),
        2 => Just(LifecycleCommand::CompleteActive),
        3 => (0_u8..8).prop_map(|slot| LifecycleCommand::Cancel { slot }),
        2 => any::<bool>().prop_map(|present| LifecycleCommand::SetModal { present }),
        3 => (0_u8..8).prop_map(|slot| LifecycleCommand::ReserveAttach { slot }),
        2 => (0_u8..8).prop_map(|slot| LifecycleCommand::ReleaseAttach { slot }),
        2 => any::<bool>()
            .prop_map(|past_deadline| LifecycleCommand::AdvanceIdleClock { past_deadline }),
        2 => any::<bool>()
            .prop_map(|process_reaped| LifecycleCommand::Close { process_reaped }),
        2 => Just(LifecycleCommand::Inspect),
    ]
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IdleBaseState {
    Ready,
    NeedsInput,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LifecyclePhase {
    Ready,
    NeedsInput,
    Active {
        slot: u8,
        modal: bool,
        attach: Option<u8>,
    },
    Attached {
        slot: u8,
        base: IdleBaseState,
    },
    Failed,
    Closing,
    Closed,
}

struct LifecycleModel {
    phase: LifecyclePhase,
    turns: BTreeMap<u8, ModelTurn>,
    process_reaped: bool,
    terminal_close_reaped: bool,
}

struct MutableTranscript {
    offset: AtomicU64,
    batch: Mutex<Option<(u64, TranscriptBatch)>>,
}

impl MutableTranscript {
    fn new() -> Self {
        Self {
            offset: AtomicU64::new(0),
            batch: Mutex::new(None),
        }
    }

    fn publish_completion(&self, slot: u8, prompt: &str) {
        let start = self.offset.load(Ordering::SeqCst);
        let prompt_uuid = format!("model-prompt-{slot}");
        let answer_uuid = format!("model-answer-{slot}");
        let message_id = format!("model-message-{slot}");
        let user = format!(
            r#"{{"parentUuid":null,"sessionId":"test","type":"user","message":{{"content":{prompt:?}}},"uuid":{prompt_uuid:?},"promptSource":"typed","promptId":"model-prompt-id-{slot}"}}"#
        );
        let assistant = format!(
            r#"{{"parentUuid":{prompt_uuid:?},"sessionId":"test","type":"assistant","uuid":{answer_uuid:?},"message":{{"id":{message_id:?},"model":"claude-test","content":[{{"type":"text","text":"completed-{slot}"}}],"stop_reason":"end_turn","usage":{{"input_tokens":3,"output_tokens":2}}}}}}"#
        );
        let rows = [user, assistant]
            .into_iter()
            .enumerate()
            .map(|(index, json)| parse_model_row(start, index, json.as_bytes()))
            .collect::<Vec<_>>();
        let end = start + rows.len() as u64;
        let batch = TranscriptBatch {
            position: TranscriptPosition {
                generation: 0,
                offset: end,
            },
            rows,
            drain: stable_drain(),
        };
        *self.batch.lock().unwrap() = Some((start, batch));
        self.offset.store(end, Ordering::SeqCst);
    }
}

#[async_trait]
impl TranscriptSource for MutableTranscript {
    async fn arm_at_eof(&self, _session_id: uuid::Uuid) -> DriverResult<TranscriptArm> {
        Ok(TranscriptArm {
            position: TranscriptPosition {
                generation: 0,
                offset: self.offset.load(Ordering::SeqCst),
            },
            historical_rows: Vec::new(),
        })
    }

    async fn poll(
        &self,
        _session_id: uuid::Uuid,
        position: &TranscriptPosition,
    ) -> DriverResult<TranscriptBatch> {
        let current = self.offset.load(Ordering::SeqCst);
        if position.generation != 0 || position.offset > current {
            return Err(DriverFailure::new(
                ErrorCode::SchemaDrift,
                "model transcript cursor is invalid",
            ));
        }
        if position.offset == current {
            return Ok(TranscriptBatch {
                position: position.clone(),
                rows: Vec::new(),
                drain: stable_drain(),
            });
        }
        let guard = self.batch.lock().unwrap();
        let Some((start, batch)) = guard.as_ref() else {
            return Err(DriverFailure::new(
                ErrorCode::SchemaDrift,
                "model transcript lost its published batch",
            ));
        };
        if position.offset != *start {
            return Err(DriverFailure::new(
                ErrorCode::SchemaDrift,
                "model transcript observed a discontinuous cursor",
            ));
        }
        Ok(batch.clone())
    }
}

fn stable_drain() -> TranscriptDrainEvidence {
    TranscriptDrainEvidence {
        at_eof: true,
        has_partial_line: false,
        stable_for_ms: 100,
    }
}

fn parse_model_row(start: u64, index: usize, bytes: &[u8]) -> ParsedRow {
    JsonlParser::new(ParseMode::Strict)
        .parse(&CompleteLine {
            location: SourceLocation {
                line: start + index as u64 + 1,
                byte_offset: (start + index as u64) * 1_000,
            },
            bytes: bytes.to_vec(),
        })
        .unwrap()
}

proptest! {
    #![proptest_config(deterministic_config())]

    #[test]
    fn lifecycle_commands_match_deadline_attach_modal_expiry_and_close_model(
        initial_modal in any::<bool>(),
        commands in prop::collection::vec(lifecycle_command_strategy(), 1..48),
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(run_lifecycle_sequence(initial_modal, commands));
    }
}

async fn run_lifecycle_sequence(initial_modal: bool, commands: Vec<LifecycleCommand>) {
    let mut config = actor_config();
    config.turn_history_capacity = 8;
    let registry = registry_with_config(config);
    let probe = Arc::new(Probe::default());
    let terminal = Arc::new(TestTerminal::new(Arc::clone(&probe)));
    terminal.set_evidence(TerminalEvidence {
        ready_prompt: true,
        quiet: true,
        lifecycle_expected: false,
        lifecycle_hook_observed: false,
        lifecycle_hook_at_ms: None,
    });
    let transcript = Arc::new(MutableTranscript::new());
    let workspace = tempfile::tempdir().unwrap();
    let initial_needs_input = initial_modal.then(|| NeedsInput {
        kind: NeedsInputKind::Trust,
        message: "model trust input required".to_owned(),
        details: serde_json::Value::Null,
    });
    if let Some(needs_input) = initial_needs_input.clone() {
        terminal.set_screen(TerminalScreenObservation::NeedsInput(needs_input));
    }
    registry
        .register(SessionRegistration {
            agent: None,
            owner: pseudomux_service::v1::SessionOwner::Caller,
            session_id: id(LIFECYCLE_SESSION),
            generation_id: generation(id(LIFECYCLE_SESSION)),
            cwd: workspace.path().to_string_lossy().into_owned(),
            compatibility: CompatibilityReport {
                claude_version: "test".to_owned(),
                os: std::env::consts::OS.to_owned(),
                arch: std::env::consts::ARCH.to_owned(),
                terminal_profile: TerminalProfile::Transparent,
                input_transport: InputTransport::Sdk,
                tested: true,
                transcript_drain_ms: 1,
            },
            dangerous_permission_bypass: false,
            resumable: true,
            cell: SessionCell::Full,
            idle_ttl_ms: Some(100),
            initial_needs_input,
            terminal: terminal.clone(),
            transcript: transcript.clone(),
        })
        .await
        .unwrap();
    let mut model = LifecycleModel {
        phase: if initial_modal {
            LifecyclePhase::NeedsInput
        } else {
            LifecyclePhase::Ready
        },
        turns: BTreeMap::new(),
        process_reaped: false,
        terminal_close_reaped: true,
    };

    for command in commands {
        match command {
            LifecycleCommand::Submit { slot, prompt, mode } => {
                lifecycle_submit(&registry, &terminal, &probe, &mut model, slot, prompt, mode)
                    .await;
            }
            LifecycleCommand::CompleteActive => {
                if let LifecyclePhase::Active {
                    slot,
                    modal: false,
                    attach: None,
                } = model.phase
                {
                    let stored = model.turns.get(&slot).unwrap().prompt.clone();
                    transcript.publish_completion(slot, &stored);
                    let terminal_result = support::wait_for_stored_turn(
                        &registry,
                        id(LIFECYCLE_SESSION),
                        id(LIFECYCLE_TURN_BASE + u128::from(slot)),
                    )
                    .await;
                    let StoredTurnTerminal::Result(result) = terminal_result else {
                        panic!("model completion must store a result: {terminal_result:?}")
                    };
                    assert_eq!(result.text, format!("completed-{slot}"));
                    model.turns.get_mut(&slot).unwrap().terminal = true;
                    model.phase = LifecyclePhase::Ready;
                }
            }
            LifecycleCommand::Cancel { slot } => {
                lifecycle_cancel(&registry, &terminal, &mut model, slot).await;
            }
            LifecycleCommand::SetModal { present } => {
                if let LifecyclePhase::Active { slot, attach, .. } = model.phase {
                    terminal.set_screen(if present {
                        TerminalScreenObservation::NeedsInput(NeedsInput {
                            kind: NeedsInputKind::Permission,
                            message: "model permission input required".to_owned(),
                            details: serde_json::Value::Null,
                        })
                    } else {
                        TerminalScreenObservation::Ready
                    });
                    wait_for_modal_state(&registry, present).await;
                    model.phase = LifecyclePhase::Active {
                        slot,
                        modal: present,
                        attach,
                    };
                } else if model.phase == LifecyclePhase::NeedsInput {
                    if present {
                        terminal.set_screen(TerminalScreenObservation::NeedsInput(NeedsInput {
                            kind: NeedsInputKind::Trust,
                            message: "model trust input required".to_owned(),
                            details: serde_json::Value::Null,
                        }));
                    } else {
                        terminal.set_screen(TerminalScreenObservation::Ready);
                        wait_for_modal_state(&registry, false).await;
                        model.phase = LifecyclePhase::Ready;
                    }
                } else if !matches!(model.phase, LifecyclePhase::Attached { .. }) {
                    terminal.set_screen(TerminalScreenObservation::Ready);
                }
            }
            LifecycleCommand::ReserveAttach { slot } => {
                lifecycle_reserve_attach(&registry, &mut model, slot).await;
            }
            LifecycleCommand::ReleaseAttach { slot } => {
                lifecycle_release_attach(&registry, &mut model, slot).await;
            }
            LifecycleCommand::AdvanceIdleClock { past_deadline } => {
                lifecycle_expire(&registry, &mut model, past_deadline).await;
            }
            LifecycleCommand::Close { process_reaped } => {
                terminal.set_close_reaped(process_reaped);
                model.terminal_close_reaped = process_reaped;
                let result = registry
                    .close(CloseSessionRequest {
                        session_id: id(LIFECYCLE_SESSION),
                        generation_id: generation(id(LIFECYCLE_SESSION)),
                        policy: ClosePolicy::Force,
                    })
                    .await;
                if model.phase == LifecyclePhase::Closed {
                    assert_eq!(result.unwrap_err().code, ErrorCode::DaemonLost);
                } else {
                    let closed = result.unwrap();
                    let reaped = model.process_reaped || process_reaped;
                    assert_eq!(closed.process_reaped, reaped);
                    model.process_reaped = reaped;
                    model.phase = if reaped {
                        LifecyclePhase::Closed
                    } else {
                        LifecyclePhase::Closing
                    };
                }
            }
            LifecycleCommand::Inspect => {
                assert_lifecycle_snapshot(&registry, &model).await;
            }
        }
        if model.phase != LifecyclePhase::Closed {
            assert_lifecycle_snapshot(&registry, &model).await;
        }
    }

    if model.phase == LifecyclePhase::Closed {
        unregister_after_exit(&registry, id(LIFECYCLE_SESSION)).await;
    } else {
        terminal.set_close_reaped(true);
        close_and_unregister(&registry, id(LIFECYCLE_SESSION)).await;
    }
    drop(transcript);
    drop(terminal);
    wait_for_resources_released(&probe).await;
}

async fn lifecycle_submit(
    registry: &pseudomux_service::v1::SessionRegistry,
    terminal: &Arc<TestTerminal>,
    probe: &Arc<Probe>,
    model: &mut LifecycleModel,
    slot: u8,
    prompt_selector: u8,
    mode: u8,
) {
    let prompt = format!("lifecycle-prompt-{slot}-{prompt_selector}");
    let turn_id = id(LIFECYCLE_TURN_BASE + u128::from(slot));
    let mut request = RunTurnRequest {
        session_id: id(LIFECYCLE_SESSION),
        generation_id: generation(id(LIFECYCLE_SESSION)),
        turn: TurnRequest {
            turn_id,
            prompt: prompt.clone(),
            deadline_unix_ms: (mode == 1).then_some(0),
            lease: Default::default(),
        },
    };
    let before_submissions = probe.submissions().len();
    if mode == 2 {
        terminal.set_submit_failure(Some(DriverFailure::new(
            ErrorCode::RecoveryFailed,
            "model injected terminal ambiguity",
        )));
    }
    let result = registry.run_turn(request.clone()).await;
    if model.phase == LifecyclePhase::Closed {
        assert_eq!(result.unwrap_err().code, ErrorCode::DaemonLost);
    } else if let Some(existing) = model.turns.get(&slot) {
        if existing.prompt == prompt {
            assert!(result.unwrap().replayed);
        } else {
            assert_eq!(result.unwrap_err().code, ErrorCode::IdConflict);
        }
    } else if mode == 1 {
        assert_eq!(result.unwrap_err().code, ErrorCode::TurnTimeout);
        assert_eq!(probe.submissions().len(), before_submissions);
    } else if model.phase != LifecyclePhase::Ready {
        let expected = match model.phase {
            LifecyclePhase::NeedsInput => ErrorCode::NeedsTrust,
            LifecyclePhase::Active { .. } | LifecyclePhase::Attached { .. } => {
                ErrorCode::SessionBusy
            }
            LifecyclePhase::Failed => ErrorCode::TranscriptUnavailable,
            LifecyclePhase::Closing => ErrorCode::SessionNotFound,
            LifecyclePhase::Ready | LifecyclePhase::Closed => unreachable!(),
        };
        assert_eq!(result.unwrap_err().code, expected);
        assert_eq!(probe.submissions().len(), before_submissions);
    } else {
        let accepted = result.unwrap();
        assert!(!accepted.replayed);
        model.turns.insert(
            slot,
            ModelTurn {
                prompt,
                terminal: false,
            },
        );
        terminal
            .wait_for_submission(id(LIFECYCLE_SESSION), turn_id)
            .await;
        if mode == 2 {
            let StoredTurnTerminal::Failed(error) =
                support::wait_for_stored_turn(registry, id(LIFECYCLE_SESSION), turn_id).await
            else {
                panic!("model submit failure must be stored")
            };
            assert_eq!(error.code, ErrorCode::RecoveryFailed);
            model.turns.get_mut(&slot).unwrap().terminal = true;
            model.process_reaped = true;
            model.phase = LifecyclePhase::Failed;
        } else {
            model.phase = LifecyclePhase::Active {
                slot,
                modal: false,
                attach: None,
            };
        }
    }
    terminal.set_submit_failure(None);
    request.turn.deadline_unix_ms = None;
}

async fn lifecycle_cancel(
    registry: &pseudomux_service::v1::SessionRegistry,
    terminal: &Arc<TestTerminal>,
    model: &mut LifecycleModel,
    slot: u8,
) {
    let active_matches =
        matches!(model.phase, LifecyclePhase::Active { slot: active, .. } if active == slot);
    if active_matches {
        terminal.set_screen(TerminalScreenObservation::Ready);
    }
    let result = registry
        .cancel_turn(CancelTurnRequest {
            session_id: id(LIFECYCLE_SESSION),
            generation_id: generation(id(LIFECYCLE_SESSION)),
            turn_id: id(LIFECYCLE_TURN_BASE + u128::from(slot)),
        })
        .await;
    if model.phase == LifecyclePhase::Closed {
        assert_eq!(result.unwrap_err().code, ErrorCode::DaemonLost);
    } else if model.turns.get(&slot).is_some_and(|turn| turn.terminal) {
        assert_eq!(result.unwrap().outcome, CancelOutcome::AlreadyTerminal);
    } else if active_matches {
        let attach = match model.phase {
            LifecyclePhase::Active { attach, .. } => attach,
            _ => unreachable!(),
        };
        let cancelled = result.unwrap();
        assert_eq!(cancelled.outcome, CancelOutcome::Cancelled);
        model.turns.get_mut(&slot).unwrap().terminal = true;
        model.phase = attach.map_or(LifecyclePhase::Ready, |attach_slot| {
            LifecyclePhase::Attached {
                slot: attach_slot,
                base: IdleBaseState::Ready,
            }
        });
    } else {
        assert_eq!(result.unwrap_err().code, ErrorCode::IdConflict);
    }
}

async fn lifecycle_reserve_attach(
    registry: &pseudomux_service::v1::SessionRegistry,
    model: &mut LifecycleModel,
    slot: u8,
) {
    let result = registry
        .reserve_writable_attach(
            id(LIFECYCLE_SESSION),
            generation(id(LIFECYCLE_SESSION)),
            id(LIFECYCLE_ATTACH_BASE + u128::from(slot)),
        )
        .await;
    match model.phase {
        LifecyclePhase::Ready => {
            result.unwrap();
            model.phase = LifecyclePhase::Attached {
                slot,
                base: IdleBaseState::Ready,
            };
        }
        LifecyclePhase::NeedsInput => {
            result.unwrap();
            model.phase = LifecyclePhase::Attached {
                slot,
                base: IdleBaseState::NeedsInput,
            };
        }
        LifecyclePhase::Active {
            slot: active,
            modal: true,
            attach: None,
        } => {
            result.unwrap();
            model.phase = LifecyclePhase::Active {
                slot: active,
                modal: true,
                attach: Some(slot),
            };
        }
        LifecyclePhase::Closed => assert_eq!(result.unwrap_err().code, ErrorCode::DaemonLost),
        _ => assert_eq!(result.unwrap_err().code, ErrorCode::SessionBusy),
    }
}

async fn lifecycle_release_attach(
    registry: &pseudomux_service::v1::SessionRegistry,
    model: &mut LifecycleModel,
    slot: u8,
) {
    let result = registry
        .release_writable_attach(
            id(LIFECYCLE_SESSION),
            generation(id(LIFECYCLE_SESSION)),
            id(LIFECYCLE_ATTACH_BASE + u128::from(slot)),
            WritableAttachCompletion::Unused,
        )
        .await;
    match model.phase {
        LifecyclePhase::Active {
            slot: active,
            modal,
            attach: Some(current),
        } if current == slot => {
            result.unwrap();
            model.phase = LifecyclePhase::Active {
                slot: active,
                modal,
                attach: None,
            };
        }
        LifecyclePhase::Active {
            attach: Some(_), ..
        } => assert_eq!(result.unwrap_err().code, ErrorCode::IdConflict),
        LifecyclePhase::Attached {
            slot: current,
            base,
        } if current == slot => {
            result.unwrap();
            model.phase = match base {
                IdleBaseState::Ready => LifecyclePhase::Ready,
                IdleBaseState::NeedsInput => LifecyclePhase::NeedsInput,
            };
        }
        LifecyclePhase::Attached { .. } => {
            assert_eq!(result.unwrap_err().code, ErrorCode::IdConflict)
        }
        LifecyclePhase::Closed => assert_eq!(result.unwrap_err().code, ErrorCode::DaemonLost),
        _ => result.unwrap(),
    }
}

async fn lifecycle_expire(
    registry: &pseudomux_service::v1::SessionRegistry,
    model: &mut LifecycleModel,
    past_deadline: bool,
) {
    let result = registry
        .expire_idle(
            id(LIFECYCLE_SESSION),
            generation(id(LIFECYCLE_SESSION)),
            if past_deadline { u64::MAX } else { 0 },
        )
        .await;
    if model.phase == LifecyclePhase::Closed {
        assert_eq!(result.unwrap_err().code, ErrorCode::DaemonLost);
        return;
    }
    if past_deadline
        && matches!(
            model.phase,
            LifecyclePhase::Ready | LifecyclePhase::NeedsInput
        )
    {
        let closed = result.unwrap().expect("eligible idle session must expire");
        let reaped = model.process_reaped || model.terminal_close_reaped;
        assert_eq!(closed.process_reaped, reaped);
        model.process_reaped = reaped;
        model.phase = if reaped {
            LifecyclePhase::Closed
        } else {
            LifecyclePhase::Closing
        };
    } else {
        assert!(result.unwrap().is_none());
    }
}

async fn wait_for_modal_state(registry: &pseudomux_service::v1::SessionRegistry, present: bool) {
    for _ in 0..2_048 {
        let snapshot = registry
            .inspect(InspectSessionRequest {
                session_id: id(LIFECYCLE_SESSION),
                generation_id: generation(id(LIFECYCLE_SESSION)),
            })
            .await
            .unwrap();
        if (snapshot.state == SessionState::NeedsInput) == present {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("actor did not reconcile the modeled modal state");
}

async fn assert_lifecycle_snapshot(
    registry: &pseudomux_service::v1::SessionRegistry,
    model: &LifecycleModel,
) {
    let result = registry
        .inspect(InspectSessionRequest {
            session_id: id(LIFECYCLE_SESSION),
            generation_id: generation(id(LIFECYCLE_SESSION)),
        })
        .await;
    if model.phase == LifecyclePhase::Closed {
        assert_eq!(result.unwrap_err().code, ErrorCode::DaemonLost);
        return;
    }
    let snapshot = result.unwrap();
    match model.phase {
        LifecyclePhase::Ready => assert_eq!(snapshot.state, SessionState::Ready),
        LifecyclePhase::NeedsInput => assert_eq!(snapshot.state, SessionState::NeedsInput),
        LifecyclePhase::Active { slot, modal, .. } => {
            assert_eq!(
                snapshot.active_turn_id,
                Some(id(LIFECYCLE_TURN_BASE + u128::from(slot)))
            );
            if modal {
                assert_eq!(snapshot.state, SessionState::NeedsInput);
            } else {
                assert!(matches!(
                    snapshot.state,
                    SessionState::Submitting
                        | SessionState::AwaitingPromptAck
                        | SessionState::Running
                        | SessionState::TerminalCandidate
                        | SessionState::Draining
                ));
            }
        }
        LifecyclePhase::Attached { base, .. } => match base {
            IdleBaseState::Ready => assert_eq!(snapshot.state, SessionState::Ready),
            IdleBaseState::NeedsInput => assert_eq!(snapshot.state, SessionState::NeedsInput),
        },
        LifecyclePhase::Failed => assert_eq!(snapshot.state, SessionState::Failed),
        LifecyclePhase::Closing => assert_eq!(snapshot.state, SessionState::Closing),
        LifecyclePhase::Closed => unreachable!(),
    }
}

#[tokio::test]
async fn turn_capacity_is_checked_before_any_actor_or_backend_mutation() {
    let mut config = actor_config();
    config.turn_history_capacity = 1;
    let registry = registry_with_config(config);
    let probe = Arc::new(Probe::default());
    let terminal = Arc::new(TestTerminal::new(Arc::clone(&probe)));
    let workspace = tempfile::tempdir().unwrap();
    register(
        &registry,
        id(SESSION + 1),
        Arc::clone(&terminal),
        Arc::new(TestTranscript::pending(Arc::clone(&probe))),
        workspace.path(),
    )
    .await;
    let first = id(TURN_BASE + 100);
    registry
        .run_turn(turn(id(SESSION + 1), first, "immutable"))
        .await
        .unwrap();
    terminal.wait_for_submission(id(SESSION + 1), first).await;
    registry
        .cancel_turn(CancelTurnRequest {
            session_id: id(SESSION + 1),
            generation_id: generation(id(SESSION + 1)),
            turn_id: first,
        })
        .await
        .unwrap();

    let before = registry
        .inspect(InspectSessionRequest {
            session_id: id(SESSION + 1),
            generation_id: generation(id(SESSION + 1)),
        })
        .await
        .unwrap();
    let submissions = probe.submissions();
    let error = registry
        .run_turn(turn(id(SESSION + 1), id(TURN_BASE + 101), "new"))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::TurnHistoryCapacityExceeded);
    let after = registry
        .inspect(InspectSessionRequest {
            session_id: id(SESSION + 1),
            generation_id: generation(id(SESSION + 1)),
        })
        .await
        .unwrap();
    assert_eq!(after, before);
    assert_eq!(probe.submissions(), submissions);

    let replay = registry
        .run_turn(turn(id(SESSION + 1), first, "immutable"))
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(probe.submissions(), submissions);
    assert_eq!(
        registry
            .run_turn(turn(id(SESSION + 1), first, "changed"))
            .await
            .unwrap_err()
            .code,
        ErrorCode::IdConflict
    );

    close_and_unregister(&registry, id(SESSION + 1)).await;
    drop(terminal);
    wait_for_resources_released(&probe).await;
}

// ---------------------------------------------------------------------------
// Session-state adjacency
// ---------------------------------------------------------------------------

/// Every `SessionState` variant, in protocol declaration order
/// (`crates/protocol/src/v1.rs:1357-1372`).
const ALL_SESSION_STATES: [SessionState; 14] = [
    SessionState::Creating,
    SessionState::Booting,
    SessionState::Ready,
    SessionState::Submitting,
    SessionState::AwaitingPromptAck,
    SessionState::Running,
    SessionState::NeedsInput,
    SessionState::TerminalCandidate,
    SessionState::Draining,
    SessionState::Cancelling,
    SessionState::Tainted,
    SessionState::Closing,
    SessionState::Closed,
    SessionState::Failed,
];

/// Wildcard-free projection of `SessionState` onto [`ALL_SESSION_STATES`].
///
/// This exists only as an anti-drift guard. Adding, removing, or renaming a
/// protocol variant stops this file compiling until both [`ALL_SESSION_STATES`]
/// and the golden adjacency below are updated, so the enumeration cannot
/// silently fall behind the wire enum.
fn session_state_index(state: SessionState) -> usize {
    match state {
        SessionState::Creating => 0,
        SessionState::Booting => 1,
        SessionState::Ready => 2,
        SessionState::Submitting => 3,
        SessionState::AwaitingPromptAck => 4,
        SessionState::Running => 5,
        SessionState::NeedsInput => 6,
        SessionState::TerminalCandidate => 7,
        SessionState::Draining => 8,
        SessionState::Cancelling => 9,
        SessionState::Tainted => 10,
        SessionState::Closing => 11,
        SessionState::Closed => 12,
        SessionState::Failed => 13,
    }
}

/// Pins the production transition table against a golden adjacency written from
/// the specification.
///
/// `command_sequence_matches_single_owner_actor_model` also calls
/// `is_valid_session_transition`, but it can never fail:
/// `SessionActor::transition` (`crates/service/src/v1/actor.rs`) runs
/// that exact predicate as a guard and returns `Err` *before* `emit`, so every
/// `SessionStateChanged` event on the stream is valid by construction. That
/// makes the 13-arm table inside `is_valid_session_transition`
/// (`crates/service/src/v1/actor.rs`) self-certifying. This test is the only thing that pins it: it walks the
/// complete `SessionState` x `SessionState` product and compares the predicate
/// against an adjacency set written out by hand, so adding or removing an arm
/// fails here.
///
/// Scope: the predicate governs *event-observable* transitions only.
/// `poison_after_unpublishable_terminal`
/// (`crates/service/src/v1/actor.rs`) assigns `Tainted` directly and
/// deliberately emits nothing, exactly as `docs/spec.md:531-538` requires, so the
/// reachable state graph is a superset of this adjacency. Do not read a green
/// run here as "these are all the states the actor can reach".
///
/// The expected set is derived from the following rules. Nothing below is read
/// off `actor.rs`; each rule cites the document it comes from.
///
/// * R1 normal turn path (`docs/spec.md:511-517`,
///   `.context/final-pmux-plan.md:1196`, `.context/final-pmux-plan.md:1198-1199`):
///   `creating -> booting -> ready -> submitting -> awaiting_prompt_ack ->
///   running -> terminal_candidate -> draining -> ready`.
/// * R2 modal blocking (`docs/spec.md:541-546`,
///   `.context/final-pmux-plan.md:1197`): startup and every in-turn phase can
///   enter `needs_input`, and the actor "retains the underlying phase and
///   resumes it", so `needs_input` returns to `ready`,
///   `awaiting_prompt_ack`, `running`, `terminal_candidate`, or `draining`.
///   `submitting` is not a resume target: a modal seen inside a
///   prompt-admission gate "returns a typed `Needs*` terminal failure and the
///   worker force-reaps", which is not a resumable block
///   (`docs/spec.md:494-497`, `docs/spec.md:548-549`).
/// * R3 cancellation (`docs/spec.md:555-563`,
///   `.context/final-pmux-plan.md:1200`): any phase with an active turn can
///   enter `cancelling`, including a blocked `needs_input` because
///   "turn deadlines, cancellation, and close remain effective while blocked"
///   (`docs/spec.md:1196`). Recovery returns to `ready`; failure taints. `ready`
///   has no active turn, so it has no `cancelling` arm.
/// * R4 writable attach reconciliation (`docs/spec.md:1731`): a reservation is
///   allowed "only from `ready` or `needs_input`"; on detach "recognized modals
///   remain `needs_input`, and ambiguity taints the session". That is exactly
///   `ready -> tainted` and `needs_input -> tainted`.
/// * R5 unrecoverable failure (`.context/final-pmux-plan.md:1202`, "any
///   unrecoverable path -> failed or closing with retryable cleanup"): every
///   phase that runs an unrecoverable step reaches `failed`. `ready` is idle
///   and `cancelling` is defined to taint rather than fail (`docs/spec.md:561-562`),
///   so neither has a `failed` arm.
/// * R6 close (`.context/final-pmux-plan.md:1201`, `docs/spec.md:565-566`): every
///   state except `creating` (which the actor leaves before any request can
///   reach it), `closing`, and `closed` can move to `closing`.
/// * R7 close completion (`docs/spec.md:579-582`): "an unconfirmed reap leaves the
///   actor in `closing` ... only a confirmed reap moves the actor to `closed`",
///   so `closing -> closed`, plus `closing -> failed` for unrecoverable
///   teardown (R5). `closed` is final.
#[test]
fn session_transition_table_matches_the_spec_adjacency_exactly() {
    use SessionState as S;

    /// Golden adjacency. Each row is annotated with the rules above that
    /// produce it; the row is the expectation, not a re-derivation.
    const SPEC_ADJACENCY: &[(SessionState, &[SessionState])] = &[
        // R1.
        (S::Creating, &[S::Booting]),
        // R1 | R2 | R5 | R6.
        (
            S::Booting,
            &[S::Ready, S::NeedsInput, S::Failed, S::Closing],
        ),
        // R1 | R4 (both arms) | R6.
        (
            S::Ready,
            &[S::Submitting, S::NeedsInput, S::Tainted, S::Closing],
        ),
        // R1 | R2 | R3 | R5 | R6.
        (
            S::Submitting,
            &[
                S::AwaitingPromptAck,
                S::NeedsInput,
                S::Cancelling,
                S::Failed,
                S::Closing,
            ],
        ),
        // R1 | R2 | R3 | R5 | R6.
        (
            S::AwaitingPromptAck,
            &[
                S::Running,
                S::NeedsInput,
                S::Cancelling,
                S::Failed,
                S::Closing,
            ],
        ),
        // R1 | R2 | R3 | R5 | R6.
        (
            S::Running,
            &[
                S::NeedsInput,
                S::TerminalCandidate,
                S::Cancelling,
                S::Failed,
                S::Closing,
            ],
        ),
        // R2 resume targets | R3 | R4 | R5 | R6.
        (
            S::NeedsInput,
            &[
                S::Ready,
                S::AwaitingPromptAck,
                S::Running,
                S::TerminalCandidate,
                S::Draining,
                S::Cancelling,
                S::Tainted,
                S::Failed,
                S::Closing,
            ],
        ),
        // R1 | R2 | R3 | R5 | R6.
        (
            S::TerminalCandidate,
            &[
                S::NeedsInput,
                S::Draining,
                S::Cancelling,
                S::Failed,
                S::Closing,
            ],
        ),
        // R1 | R2 | R3 | R5 | R6.
        (
            S::Draining,
            &[
                S::Ready,
                S::NeedsInput,
                S::Cancelling,
                S::Failed,
                S::Closing,
            ],
        ),
        // R3 (recovered | recovery_failed) | R6.
        (S::Cancelling, &[S::Ready, S::Tainted, S::Closing]),
        // R6. A tainted session "cannot accept another turn and SHOULD be
        // closed" (docs/spec.md:1211).
        (S::Tainted, &[S::Closing]),
        // R6.
        (S::Failed, &[S::Closing]),
        // R7.
        (S::Closing, &[S::Closed, S::Failed]),
        // R7: terminal.
        (S::Closed, &[]),
    ];

    // The golden table must cover every variant exactly once, so a new protocol
    // state can never be added without an explicit expectation.
    let mut covered = [false; ALL_SESSION_STATES.len()];
    for (previous, _) in SPEC_ADJACENCY {
        let index = session_state_index(*previous);
        assert!(
            !covered[index],
            "SPEC_ADJACENCY lists {previous:?} more than once"
        );
        covered[index] = true;
    }
    for state in ALL_SESSION_STATES {
        assert!(
            covered[session_state_index(state)],
            "SPEC_ADJACENCY has no row for {state:?}"
        );
    }

    let mut expected_edges = 0_usize;
    let mut missing = Vec::new();
    let mut unexpected = Vec::new();
    for previous in ALL_SESSION_STATES {
        let successors = SPEC_ADJACENCY
            .iter()
            .find(|(state, _)| *state == previous)
            .map(|(_, successors)| *successors)
            .expect("row coverage was just asserted");
        for current in ALL_SESSION_STATES {
            let expected = successors.contains(&current);
            let actual = is_valid_session_transition(previous, current);
            if expected {
                expected_edges += 1;
            }
            match (expected, actual) {
                (true, false) => missing.push(format!("{previous:?} -> {current:?}")),
                (false, true) => unexpected.push(format!("{previous:?} -> {current:?}")),
                _ => {}
            }
        }
    }

    assert!(
        missing.is_empty() && unexpected.is_empty(),
        "is_valid_session_transition (crates/service/src/v1/actor.rs) no longer \
         matches the specification adjacency.\n  \
         missing (the spec allows it, production refuses): {missing:?}\n  \
         unexpected (production allows it, the spec does not): {unexpected:?}"
    );
    // Guards the golden table itself against being emptied or duplicated.
    assert_eq!(
        expected_edges,
        50,
        "the golden adjacency should contain exactly 50 legal edges out of the \
         {} ordered pairs",
        ALL_SESSION_STATES.len() * ALL_SESSION_STATES.len()
    );
}

// ---------------------------------------------------------------------------
// Real Claude Code v2.1.70 terminal captures
// ---------------------------------------------------------------------------

const CLAUDE_2_1_70_READY: &str = include_str!("fixtures/claude_2_1_70_ready.txt");

/// The five recovered captures, byte-for-byte as they were taken.
const CLAUDE_2_1_70_CAPTURES: &[(&str, &str)] = &[
    ("claude_2_1_70_ready.txt", CLAUDE_2_1_70_READY),
    (
        "claude_2_1_70_response.txt",
        include_str!("fixtures/claude_2_1_70_response.txt"),
    ),
    (
        "claude_2_1_70_thinking.txt",
        include_str!("fixtures/claude_2_1_70_thinking.txt"),
    ),
    (
        "claude_2_1_70_tool_use.txt",
        include_str!("fixtures/claude_2_1_70_tool_use.txt"),
    ),
    (
        "claude_2_1_70_error.txt",
        include_str!("fixtures/claude_2_1_70_error.txt"),
    ),
];

/// Splits a capture into the rendered rows an rmux snapshot would carry. The
/// files end with one trailing newline from the capture tool; that terminator
/// is not a row.
fn capture_rows(capture: &str) -> Vec<String> {
    capture
        .strip_suffix('\n')
        .unwrap_or(capture)
        .split('\n')
        .map(str::to_owned)
        .collect()
}

/// Wraps rendered rows in the snapshot shape `classify_terminal_snapshot`
/// consumes. `cursor` is always fabricated: these captures have no cursor.
fn capture_snapshot(rows: &[String], cursor: Option<TerminalCursor>) -> TerminalSnapshot {
    let cols = rows
        .iter()
        .map(|row| row.chars().count())
        .max()
        .unwrap_or(0)
        .max(1);
    TerminalSnapshot {
        revision: 1,
        rows: u16::try_from(rows.len()).expect("capture row count fits u16"),
        cols: u16::try_from(cols).expect("capture width fits u16"),
        cursor,
        visible_text: rows.join("\n"),
    }
}

fn fabricated_cursor(row: usize, col: u16) -> Option<TerminalCursor> {
    Some(TerminalCursor {
        row: u16::try_from(row).expect("cursor row fits u16"),
        col,
        visible: true,
        style: 0,
    })
}

/// The composer row: the last row carrying the `❯` glyph. Earlier `❯` rows in
/// these captures are echoed user prompts in the scrollback.
fn composer_row(rows: &[String]) -> usize {
    rows.iter()
        .rposition(|row| row.contains('❯'))
        .expect("every capture renders a composer prompt")
}

fn rows_with_blank_footer(rows: &[String], extra: usize) -> Vec<String> {
    let mut padded = rows.to_vec();
    padded.extend(std::iter::repeat_n(String::new(), extra));
    padded
}

/// Padding that moves the END OF THE FRAME, which is what the bound is measured
/// against. `rows_with_blank_footer` does not: blank rows below the composer are
/// exactly the shape Claude leaves after a `/clear`.
fn rows_with_rendered_footer(rows: &[String], extra: usize) -> Vec<String> {
    let mut padded = rows.to_vec();
    padded.extend(std::iter::repeat_n("rendered".to_owned(), extra));
    padded
}

/// Pins the two Claude-TUI geometry constants the recovered captures can
/// actually establish.
///
/// `crates/service/tests/fixtures/claude_2_1_70_*.txt` are real terminal
/// captures of Claude Code **v2.1.70**, recovered verbatim from the deleted
/// architecture (`git show origin/main:fixtures/claude_code_*.txt`). They are
/// **cursor-less, whitespace-stripped text**. That limits what they can prove
/// about the four geometry constants in `crates/service/src/driver_io.rs`:
///
/// SETTLED BY THESE FILES
/// 1. The `❯` composer glyph is rendered with an all-whitespace prefix — column
///    0 in all five captures — so there is no Ink box border in front of it and
///    `prompt_glyph_col` (`crates/service/src/driver_io.rs`) can locate it.
/// 2. The composer sits within `MAX_RENDERED_ROWS_BELOW_ACTIVE_CURSOR == 4`
///    RENDERED rows of the end of the frame (that constant, applied in
///    `active_editor`) — two rows, in all five captures, which is also what 85
///    of 85 live 2.1.220 empty-composer screens render. These captures paint to
///    the last row, so they cannot distinguish "four rows off the end of the
///    frame" from "four rows off the bottom of the grid"; live 2.1.220 can, and
///    does — see `driver_io::tests::post_clear_lines`.
///
/// NOT SETTLED BY THESE FILES — FABRICATED BELOW
/// 3. `cursor_col_from_prompt == 2` on an empty composer
///    (`ActiveEditor::empty_cursor_position`).
/// 4. The editor box growing upward with an invariant cursor row (the anchor
///    scan in `active_editor`).
///
/// Constants 3 and 4 need a cursor and these captures have none, so every
/// cursor in this test and in
/// `fabricated_composer_cursors_exercise_the_geometry_the_captures_cannot_establish`
/// is **invented by the test**, not observed from Claude. Constants 3 and 4
/// remain pinned only by the live v2.1.215 evidence recorded in
/// `.context/plans/review/48-coordinator-correction-tui-constants.md`. A green
/// run of this file is not evidence for them. (Two earlier readers of this
/// project drew the opposite conclusion from a similarly named green test.)
#[test]
fn claude_2_1_70_captures_pin_the_prompt_glyph_prefix_and_bottom_offset() {
    for (name, capture) in CLAUDE_2_1_70_CAPTURES {
        let rows = capture_rows(capture);
        let composer = composer_row(&rows);

        // Constant 1, read straight off the captured bytes.
        let glyph_offset = rows[composer]
            .find('❯')
            .expect("composer_row selected a glyph row");
        let prefix = &rows[composer][..glyph_offset];
        assert!(
            prefix.chars().all(char::is_whitespace),
            "{name}: the composer glyph has a non-whitespace prefix {prefix:?}; \
             prompt_glyph_col would reject the row"
        );
        let prompt_col = u16::try_from(prefix.chars().count()).expect("prompt column fits u16");
        assert_eq!(
            prompt_col, 0,
            "{name}: v2.1.70 renders the composer glyph flush at column 0"
        );

        // Constant 2, read straight off the captured bytes.
        let last_rendered = rows
            .iter()
            .rposition(|row| !row.trim().is_empty())
            .expect("every capture renders something");
        assert_eq!(
            last_rendered,
            rows.len() - 1,
            "{name}: v2.1.70 paints to the last row of the capture"
        );
        let rows_below = last_rendered - composer;
        assert_eq!(
            rows_below, 2,
            "{name}: v2.1.70 renders exactly two footer rows below the composer"
        );
        assert!(
            rows_below <= 4,
            "{name}: within MAX_RENDERED_ROWS_BELOW_ACTIVE_CURSOR"
        );

        // No capture trips a modal phrase, so every classification below is
        // decided by the editor geometry rather than by the phrase scanner.
        assert_eq!(
            classify_terminal_snapshot(&capture_snapshot(&rows, None)).label(),
            "unrecognised",
            "{name}: a cursor-less capture is not classifiable and must not \
             accidentally match a blocking-screen phrase"
        );

        // Replay through the production classifier. The rows are real; the
        // cursor is fabricated (see the doc comment).
        assert_eq!(
            classify_terminal_snapshot(&capture_snapshot(
                &rows,
                fabricated_cursor(composer, prompt_col + 2)
            )),
            TerminalScreenState::Ready,
            "{name}: production must resolve the captured composer row as an \
             empty active editor"
        );

        // Negative control for constant 1: put an Ink box border in front of the
        // glyph and the same geometry stops resolving.
        let mut bordered = rows.clone();
        bordered[composer] = format!("\u{2502} {}", rows[composer]);
        assert_ne!(
            classify_terminal_snapshot(&capture_snapshot(
                &bordered,
                fabricated_cursor(composer, prompt_col + 4)
            )),
            TerminalScreenState::Ready,
            "{name}: a bordered composer must not resolve, or the column-0 \
             assertion above is vacuous"
        );

        // Negative control for constant 2: four RENDERED footer rows below the
        // composer is still an active editor, five is not.
        assert_eq!(
            classify_terminal_snapshot(&capture_snapshot(
                &rows_with_rendered_footer(&rows, 4 - rows_below),
                fabricated_cursor(composer, prompt_col + 2)
            )),
            TerminalScreenState::Ready,
            "{name}: MAX_RENDERED_ROWS_BELOW_ACTIVE_CURSOR is inclusive at 4"
        );
        assert_ne!(
            classify_terminal_snapshot(&capture_snapshot(
                &rows_with_rendered_footer(&rows, 5 - rows_below),
                fabricated_cursor(composer, prompt_col + 2)
            )),
            TerminalScreenState::Ready,
            "{name}: a composer five rendered rows off the end of the frame must \
             not resolve, or the offset assertion above is vacuous"
        );

        // Positive control for the same constant, and the shape the bound is
        // NOT about: Claude leaves the grid below a post-`/clear` frame blank,
        // and blank rows are not frame. Padding this capture with sixteen of
        // them reproduces the live 2.1.220 geometry that made the first turn
        // after every successful clear unsubmittable, and it must still resolve.
        for blank in [1_usize, 5, 16] {
            assert_eq!(
                classify_terminal_snapshot(&capture_snapshot(
                    &rows_with_blank_footer(&rows, blank),
                    fabricated_cursor(composer, prompt_col + 2)
                )),
                TerminalScreenState::Ready,
                "{name}: {blank} blank rows below the frame do not move the composer"
            );
        }
    }
}

/// Exercises the two geometry constants the v2.1.70 captures CANNOT establish.
///
/// Read the doc comment on
/// `claude_2_1_70_captures_pin_the_prompt_glyph_prefix_and_bottom_offset`
/// first. Every cursor position and every composer row in this test is
/// **fabricated**. This test pins the production code against the constants as
/// written, which is useful for refactoring; it is **not** evidence that live
/// Claude renders them this way. That evidence lives only in
/// `.context/plans/review/48-coordinator-correction-tui-constants.md` (24/24
/// real turns at v2.1.215).
#[test]
fn fabricated_composer_cursors_exercise_the_geometry_the_captures_cannot_establish() {
    let rows = capture_rows(CLAUDE_2_1_70_READY);
    let composer = composer_row(&rows);

    // Constant 3: exactly two cells after the glyph means "empty composer".
    assert_eq!(
        classify_terminal_snapshot(&capture_snapshot(&rows, fabricated_cursor(composer, 2))),
        TerminalScreenState::Ready
    );
    for col in [0_u16, 1, 3, 4, 12] {
        assert_ne!(
            classify_terminal_snapshot(&capture_snapshot(&rows, fabricated_cursor(composer, col))),
            TerminalScreenState::Ready,
            "a cursor {col} cells after the glyph is not an empty composer"
        );
    }
    // A hidden cursor is never an active editor, whatever the column says.
    assert_ne!(
        classify_terminal_snapshot(&capture_snapshot(
            &rows,
            Some(TerminalCursor {
                row: u16::try_from(composer).expect("cursor row fits u16"),
                col: 2,
                visible: false,
                style: 0,
            })
        )),
        TerminalScreenState::Ready
    );

    // Constant 4: Claude's Ink editor grows UPWARD. A two-row composer moves
    // the `❯` anchor one row up while the cursor row — and therefore its
    // distance from the bottom — is unchanged.
    let mut grown = rows.clone();
    grown[composer - 1] = "\u{276f} describe the permission model".to_owned();
    grown[composer] = "  and do you want to proceed yes or no".to_owned();
    assert_eq!(grown.len(), rows.len(), "the screen height is unchanged");
    // The anchor resolves one row above the cursor, so this is a POPULATED
    // editor -- a screen pmux RECOGNIZES, and specifically not NeedsInput,
    // because production must not scan the operator's own composer text for
    // modal phrases (`classify_terminal_snapshot`).
    //
    // `composer_holding_text` and not `unrecognised` is the whole of what the
    // split bought here: this assertion used to read `Unknown`, which was also
    // what a "trust this directory" screen pmux had never been taught read as.
    assert_eq!(
        classify_terminal_snapshot(&capture_snapshot(&grown, fabricated_cursor(composer, 38)))
            .label(),
        "composer_holding_text"
    );

    // Control: delete only the anchor glyph. The identical text now does trip
    // the modal scanner, which proves the assertion above is about the
    // upward-grown editor and not about the phrase being unrecognized.
    let mut anchorless = grown.clone();
    anchorless[composer - 1] = "  describe the permission model".to_owned();
    assert!(matches!(
        classify_terminal_snapshot(&capture_snapshot(
            &anchorless,
            fabricated_cursor(composer, 38)
        )),
        TerminalScreenState::NeedsInput(_)
    ));
}

/// A pool instance is unreachable from every session-addressed method, and the
/// refusal is byte-identical to the one a session that never existed gets.
///
/// The indistinguishability is the point rather than a nicety. A refusal that
/// said "this exists but is not yours" -- `permission_denied`, `session_busy`,
/// anything distinguishable -- is an ORACLE: it lets a caller enumerate the
/// pool's session ids by asking, and the whole product statement of Path B is
/// that the caller names no resource. A caller who can learn a resource's name
/// is one step from aliasing one, and nine leaks in this codebase were each
/// reachable exactly that way.
///
/// The owner check runs BEFORE the generation fence, and the third assertion is
/// what pins that: a stale-generation body names the session and therefore
/// confirms it exists, so answering it for a pool instance would rebuild the
/// oracle the owner check removes.
#[tokio::test]
async fn a_pool_instance_is_refused_to_every_session_addressed_method_as_if_absent() {
    use pseudomux_protocol::v1::{
        CloseSessionRequest, ErrorCode, InspectSessionRequest, SubscribeEventsRequest,
    };
    use pseudomux_service::v1::SessionOwner;

    let registry = registry_with_config(actor_config());
    let workspace = tempfile::tempdir().unwrap();
    let pool_id = id(0xf001);
    let probe = Arc::new(Probe::default());
    let pool_terminal = Arc::new(TestTerminal::new(Arc::clone(&probe)));
    let pool_transcript = Arc::new(TestTranscript::pending(Arc::clone(&probe)));
    let pool = support::register_owned(
        &registry,
        SessionOwner::Pool,
        pool_id,
        pool_terminal,
        pool_transcript,
        workspace.path(),
    )
    .await;

    // A session id that was never registered at all. Its refusal is the
    // reference every assertion below is compared against.
    let absent_id = id(0xf002);
    let absent = registry
        .actor(absent_id, generation(absent_id))
        .await
        .err()
        .expect("a session that does not exist is not resolvable");

    let refused = registry
        .actor(pool_id, pool.generation_id)
        .await
        .err()
        .expect("a pool instance is not reachable from the caller resolver");
    assert_eq!(refused.code, absent.code);
    assert_eq!(refused.code, ErrorCode::SessionNotFound);
    assert_eq!(
        refused.retryable, absent.retryable,
        "a distinguishable retry hint is an oracle too"
    );
    assert_eq!(refused.details, absent.details);

    // The message differs only in the id it echoes, which is the id the CALLER
    // supplied and already knows.
    assert_eq!(
        refused.message.replace(&pool_id.to_string(), "ID"),
        absent.message.replace(&absent_id.to_string(), "ID"),
    );

    // A WRONG generation for the pool instance must not answer
    // `stale_session_generation`: that body names the session and so confirms
    // it exists.
    let wrong_generation = registry
        .actor(pool_id, generation(absent_id))
        .await
        .err()
        .expect("a pool instance is refused whatever generation is presented");
    assert_eq!(
        wrong_generation.code,
        ErrorCode::SessionNotFound,
        "the owner check must run before the generation fence, or it leaks existence"
    );

    // Every session-addressed registry method, not just the resolver. Each one
    // goes through `actor`, and the point of checking them individually is that
    // a future method that resolves some other way fails here.
    assert_eq!(
        registry
            .inspect(InspectSessionRequest {
                session_id: pool_id,
                generation_id: pool.generation_id,
            })
            .await
            .expect_err("inspect")
            .code,
        ErrorCode::SessionNotFound
    );
    assert_eq!(
        registry
            .run_turn(turn(pool_id, id(0xf003), "hello"))
            .await
            .expect_err("run_turn")
            .code,
        ErrorCode::SessionNotFound
    );
    assert_eq!(
        registry
            .close(CloseSessionRequest {
                session_id: pool_id,
                generation_id: pool.generation_id,
                policy: pseudomux_protocol::v1::ClosePolicy::Force,
            })
            .await
            .expect_err("close")
            .code,
        ErrorCode::SessionNotFound
    );
    assert_eq!(
        registry
            .events(SubscribeEventsRequest {
                session_id: pool_id,
                generation_id: pool.generation_id,
                after_sequence: 0,
                wait_ms: 0,
                max_events: 1,
            })
            .await
            .expect_err("subscribe_events")
            .code,
        ErrorCode::SessionNotFound
    );

    // The generic idle reaper's entry point, which is what stops a second
    // reaper from racing the pool's own teardown.
    assert_eq!(
        registry
            .expire_idle(pool_id, pool.generation_id, u64::MAX)
            .await
            .expect_err("expire_idle")
            .code,
        ErrorCode::SessionNotFound
    );

    // Control: a CALLER session registered the same way is reachable by every
    // one of those. Without this the assertions above would also pass if
    // `register` had simply failed.
    let caller_id = id(0xf004);
    let caller = support::register_owned(
        &registry,
        SessionOwner::Caller,
        caller_id,
        Arc::new(TestTerminal::new(Arc::clone(&probe))),
        Arc::new(TestTranscript::pending(Arc::clone(&probe))),
        workspace.path(),
    )
    .await;
    assert!(
        registry
            .actor(caller_id, caller.generation_id)
            .await
            .is_ok()
    );
    assert!(
        registry
            .inspect(InspectSessionRequest {
                session_id: caller_id,
                generation_id: caller.generation_id,
            })
            .await
            .is_ok()
    );
    // And a wrong generation on a CALLER session DOES say stale, which is what
    // makes the pool instance's `session_not_found` above a decision and not
    // the only answer this resolver has.
    assert_eq!(
        registry
            .actor(caller_id, generation(absent_id))
            .await
            .err()
            .expect("a caller session with a wrong generation")
            .code,
        ErrorCode::StaleSessionGeneration
    );
}
