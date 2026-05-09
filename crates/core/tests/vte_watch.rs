use pseudomux_core::vte::{AgentState, SemanticEvent, WatchEvent, WatchEventBuilder};

#[test]
fn state_change_emits_watch_event() {
    let mut b = WatchEventBuilder::new();
    let evts = b.process(&SemanticEvent::StateChanged {
        from: AgentState::Booting,
        to: AgentState::Ready,
        seq: 1,
    });
    assert_eq!(evts.len(), 1);
    match &evts[0] {
        WatchEvent::StateChange { from, to, .. } => {
            assert_eq!(from, "Booting");
            assert_eq!(to, "Ready");
        }
        _ => panic!("expected StateChange"),
    }
}

#[test]
fn content_delta_batching() {
    let mut b = WatchEventBuilder::new();
    let evts = b.process(&SemanticEvent::AssistantDelta {
        text: "hello ".to_string(),
        seq: 1,
    });
    assert!(evts.is_empty());
    let evts = b.process(&SemanticEvent::AssistantDelta {
        text: "world".to_string(),
        seq: 2,
    });
    assert!(evts.is_empty());
    let flushed = b.flush_pending().unwrap();
    match flushed {
        WatchEvent::ContentDelta {
            lines,
            chars,
            preview,
            tag,
            ..
        } => {
            assert_eq!(chars, 11);
            assert_eq!(lines, 2);
            assert_eq!(preview, "hello ");
            assert_eq!(tag, "assistant");
        }
        _ => panic!("expected ContentDelta"),
    }
}

#[test]
fn turn_complete_on_thinking_to_ready() {
    let mut b = WatchEventBuilder::new();
    b.process(&SemanticEvent::ThinkingStarted { seq: 1 });
    b.process(&SemanticEvent::AssistantDelta {
        text: "output".to_string(),
        seq: 2,
    });
    let evts = b.process(&SemanticEvent::StateChanged {
        from: AgentState::Thinking,
        to: AgentState::Ready,
        seq: 3,
    });
    assert_eq!(evts.len(), 3);
    assert!(matches!(&evts[0], WatchEvent::ContentDelta { .. }));
    assert!(matches!(&evts[1], WatchEvent::StateChange { .. }));
    match &evts[2] {
        WatchEvent::TurnComplete {
            total_lines,
            total_chars,
            ..
        } => {
            assert_eq!(*total_lines, 1);
            assert_eq!(*total_chars, 6);
        }
        _ => panic!("expected TurnComplete"),
    }
}

#[test]
fn confirmation_emits_input_required() {
    let mut b = WatchEventBuilder::new();
    let evts = b.process(&SemanticEvent::ConfirmationRequired {
        prompt_text: "Allow file write?".to_string(),
        seq: 1,
    });
    assert_eq!(evts.len(), 1);
    match &evts[0] {
        WatchEvent::InputRequired {
            kind, prompt_text, ..
        } => {
            assert_eq!(kind, "confirmation");
            assert_eq!(prompt_text, "Allow file write?");
        }
        _ => panic!("expected InputRequired"),
    }
}

#[test]
fn auth_emits_input_required() {
    let mut b = WatchEventBuilder::new();
    let evts = b.process(&SemanticEvent::AuthRequired { seq: 1 });
    assert_eq!(evts.len(), 1);
    match &evts[0] {
        WatchEvent::InputRequired { kind, .. } => {
            assert_eq!(kind, "auth");
        }
        _ => panic!("expected InputRequired"),
    }
}

#[test]
fn flush_pending_returns_none_when_empty() {
    let mut b = WatchEventBuilder::new();
    assert!(b.flush_pending().is_none());
}

#[test]
fn state_change_flushes_pending_delta() {
    let mut b = WatchEventBuilder::new();
    b.process(&SemanticEvent::AssistantDelta {
        text: "data".to_string(),
        seq: 1,
    });
    let evts = b.process(&SemanticEvent::StateChanged {
        from: AgentState::Thinking,
        to: AgentState::ToolRunning,
        seq: 2,
    });
    assert_eq!(evts.len(), 2);
    assert!(matches!(&evts[0], WatchEvent::ContentDelta { .. }));
    assert!(matches!(&evts[1], WatchEvent::StateChange { .. }));
}

#[test]
fn session_exited_event() {
    let mut b = WatchEventBuilder::new();
    let evt = b.notify_session_exited(Some(0));
    match evt {
        WatchEvent::SessionExited { exit_code, .. } => {
            assert_eq!(exit_code, Some(0));
        }
        _ => panic!("expected SessionExited"),
    }
}

#[test]
fn session_exited_via_semantic_event() {
    let mut b = WatchEventBuilder::new();
    let evts = b.process(&SemanticEvent::SessionExited {
        exit_code: Some(1),
        seq: 1,
    });
    assert_eq!(evts.len(), 1);
    match &evts[0] {
        WatchEvent::SessionExited { exit_code, .. } => {
            assert_eq!(*exit_code, Some(1));
        }
        _ => panic!("expected SessionExited"),
    }
}

#[test]
fn input_sent_generates_event() {
    let mut b = WatchEventBuilder::new();
    let evt = b.notify_input_sent("yes");
    match evt {
        WatchEvent::InputSent { preview, .. } => {
            assert_eq!(preview, "yes");
        }
        _ => panic!("expected InputSent"),
    }
}

#[test]
fn preview_truncated_to_80_chars() {
    let long = "a".repeat(200);
    let mut b = WatchEventBuilder::new();
    b.process(&SemanticEvent::AssistantDelta { text: long, seq: 1 });
    let flushed = b.flush_pending().unwrap();
    match flushed {
        WatchEvent::ContentDelta { preview, .. } => {
            assert!(preview.len() <= 84);
            assert!(preview.ends_with('…'));
        }
        _ => panic!("expected ContentDelta"),
    }
}

#[test]
fn turn_metrics_accumulate() {
    let mut b = WatchEventBuilder::new();
    b.process(&SemanticEvent::ThinkingStarted { seq: 1 });
    b.process(&SemanticEvent::AssistantDelta {
        text: "line1\nline2".to_string(),
        seq: 2,
    });
    b.process(&SemanticEvent::AssistantDelta {
        text: "more".to_string(),
        seq: 3,
    });
    let evts = b.process(&SemanticEvent::AssistantTurnCompleted {
        full_text: "line1\nline2more".to_string(),
        seq: 4,
    });
    assert_eq!(evts.len(), 2);
    match &evts[1] {
        WatchEvent::TurnComplete {
            total_lines,
            total_chars,
            ..
        } => {
            assert_eq!(*total_chars, 15);
            assert_eq!(*total_lines, 3);
        }
        _ => panic!("expected TurnComplete"),
    }
}
