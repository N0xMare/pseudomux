use pseudomux_core::vte::{
    AgentState, RegionClassifier, ScreenChange, SemanticEvent, StatusPatterns,
};

#[test]
fn detect_state_thinking() {
    let c = RegionClassifier::new(StatusPatterns::opencode_v1_2());
    assert_eq!(c.detect_state("esc interrupt"), AgentState::Thinking);
    assert_eq!(
        c.detect_state("esc again to interrupt"),
        AgentState::Thinking
    );
}

#[test]
fn detect_state_ready() {
    let c = RegionClassifier::new(StatusPatterns::opencode_v1_2());
    assert_eq!(c.detect_state("ctrl+p commands"), AgentState::Ready);
    assert_eq!(c.detect_state("Ask anything"), AgentState::Ready);
}

#[test]
fn detect_state_auth() {
    let c = RegionClassifier::new(StatusPatterns::opencode_v1_2());
    assert_eq!(
        c.detect_state("Get started /connect"),
        AgentState::AuthRequired
    );
}

#[test]
fn detect_state_error() {
    let c = RegionClassifier::new(StatusPatterns::opencode_v1_2());
    assert_eq!(c.detect_state("Bad Request"), AgentState::Error);
}

#[test]
fn detect_state_unknown() {
    let c = RegionClassifier::new(StatusPatterns::opencode_v1_2());
    assert_eq!(c.detect_state("random text"), AgentState::Unknown);
}

#[test]
fn classify_status_change_emits_state_changed() {
    let mut c = RegionClassifier::new(StatusPatterns::opencode_v1_2());
    let changes = vec![ScreenChange::StatusBarChanged {
        old: String::new(),
        new: "esc interrupt".into(),
    }];
    let events = c.classify(&changes);
    assert!(events.iter().any(|e| matches!(
        e,
        SemanticEvent::StateChanged {
            to: AgentState::Thinking,
            ..
        }
    )));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, SemanticEvent::ThinkingStarted { .. }))
    );
}

#[test]
fn classify_content_change_emits_delta() {
    let mut c = RegionClassifier::new(StatusPatterns::opencode_v1_2());
    let changes = vec![ScreenChange::ContentRowChanged {
        row: 0,
        old: String::new(),
        new: "Hello".into(),
    }];
    let events = c.classify(&changes);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, SemanticEvent::AssistantTurnStarted { .. }))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, SemanticEvent::AssistantDelta { text, .. } if text == "Hello"))
    );
}

#[test]
fn classify_screen_cleared() {
    let mut c = RegionClassifier::new(StatusPatterns::opencode_v1_2());
    let changes = vec![ScreenChange::ScreenCleared];
    let events = c.classify(&changes);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, SemanticEvent::ScreenRedraw { .. }))
    );
}

#[test]
fn turn_lifecycle_with_quiescence() {
    use std::time::{Duration, Instant};
    let mut c = RegionClassifier::new(StatusPatterns::opencode_v1_2());

    // Start turn
    c.classify(&[ScreenChange::ContentRowChanged {
        row: 0,
        old: String::new(),
        new: "Response text".into(),
    }]);
    assert!(c.in_turn);

    // Transition to ready
    c.classify(&[ScreenChange::StatusBarChanged {
        old: String::new(),
        new: "ctrl+p commands".into(),
    }]);

    // Force last_content_change to the past
    c.last_content_change = Some(Instant::now() - Duration::from_secs(5));

    let completed = c.check_quiescence(Duration::from_secs(2));
    assert!(completed.is_some());
    assert!(matches!(
        completed.unwrap(),
        SemanticEvent::AssistantTurnCompleted { .. }
    ));
    assert!(!c.in_turn);
}

#[test]
fn thinking_to_ready_emits_completed() {
    let mut c = RegionClassifier::new(StatusPatterns::opencode_v1_2());
    c.classify(&[ScreenChange::StatusBarChanged {
        old: String::new(),
        new: "esc interrupt".into(),
    }]);
    let events = c.classify(&[ScreenChange::StatusBarChanged {
        old: "esc interrupt".into(),
        new: "ctrl+p commands".into(),
    }]);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, SemanticEvent::ThinkingCompleted { .. }))
    );
    assert!(events.iter().any(|e| matches!(
        e,
        SemanticEvent::StateChanged {
            to: AgentState::Ready,
            ..
        }
    )));
}

#[test]
fn content_row_with_status_pattern_triggers_state_change() {
    let mut c = RegionClassifier::new(StatusPatterns::opencode_v1_2());
    let changes = vec![ScreenChange::ContentRowChanged {
        row: 35,
        old: String::new(),
        new: "esc interrupt".into(),
    }];
    let events = c.classify(&changes);
    assert!(events.iter().any(|e| matches!(
        e,
        SemanticEvent::StateChanged {
            to: AgentState::Thinking,
            ..
        }
    )));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, SemanticEvent::AssistantDelta { .. }))
    );
}

#[test]
fn content_row_with_ready_pattern_triggers_state_change() {
    let mut c = RegionClassifier::new(StatusPatterns::opencode_v1_2());
    let changes = vec![ScreenChange::ContentRowChanged {
        row: 33,
        old: String::new(),
        new: "Ask anything".into(),
    }];
    let events = c.classify(&changes);
    assert!(events.iter().any(|e| matches!(
        e,
        SemanticEvent::StateChanged {
            to: AgentState::Ready,
            ..
        }
    )));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, SemanticEvent::AssistantDelta { .. }))
    );
}

#[test]
fn content_row_normal_text_still_emits_delta() {
    let mut c = RegionClassifier::new(StatusPatterns::opencode_v1_2());
    let changes = vec![ScreenChange::ContentRowChanged {
        row: 5,
        old: String::new(),
        new: "Hello world".into(),
    }];
    let events = c.classify(&changes);
    assert!(
        events.iter().any(
            |e| matches!(e, SemanticEvent::AssistantDelta { text, .. } if text == "Hello world")
        )
    );
}

#[test]
fn claude_code_webfetch_triggers_tool_started() {
    // Claude Code renders WebFetch invocations as "Fetch(url)" in the content
    // region. The classifier should recognize this as a tool transition and
    // emit ToolStarted with name=Some("Fetch") (extracted from the paren form).
    let mut c = RegionClassifier::new(StatusPatterns::claude_code());
    c.classify(&[ScreenChange::ContentRowChanged {
        row: 0,
        old: String::new(),
        new: "esc to interrupt".into(),
    }]);
    assert_eq!(c.state(), AgentState::Thinking);

    let events = c.classify(&[ScreenChange::ContentRowChanged {
        row: 5,
        old: String::new(),
        new: "Fetch(https://doc.rust-lang.org/std/option/enum.Option.html)".into(),
    }]);
    assert_eq!(c.state(), AgentState::ToolRunning);
    let name = events.iter().find_map(|e| match e {
        SemanticEvent::ToolStarted { name, .. } => Some(name.clone()),
        _ => None,
    });
    assert_eq!(name, Some(Some("Fetch".to_string())));
}

#[test]
fn claude_code_tool_started_carries_name() {
    // First enter Thinking state so the next transition into ToolRunning is
    // a real state change (otherwise transition_state no-ops).
    let mut c = RegionClassifier::new(StatusPatterns::claude_code());
    c.classify(&[ScreenChange::ContentRowChanged {
        row: 0,
        old: String::new(),
        new: "esc to interrupt".into(),
    }]);
    assert_eq!(c.state(), AgentState::Thinking);

    // Content row signaling a tool invocation: Claude Code writes lines like
    // "Read(README.md)" into the content region, which matches the Claude
    // Code `tool_indicators`. The classifier should capture "Read" as the name.
    let events = c.classify(&[ScreenChange::ContentRowChanged {
        row: 5,
        old: String::new(),
        new: "Read(README.md)".into(),
    }]);
    assert_eq!(c.state(), AgentState::ToolRunning);
    let tool_started_name = events.iter().find_map(|e| match e {
        SemanticEvent::ToolStarted { name, .. } => Some(name.clone()),
        _ => None,
    });
    assert_eq!(
        tool_started_name,
        Some(Some("Read".to_string())),
        "expected ToolStarted with name=Read"
    );
}

#[test]
fn claude_code_gerund_in_prose_does_not_trigger_tool() {
    // Regression: the prompt text "You are running inside pseudomux" used to
    // false-trigger ToolRunning because "running" matched the gerund tool
    // indicator. Gerunds have been removed from the indicator list — only
    // parenthesized forms (Read(, Fetch(, Bash(, ...) trigger the transition.
    let mut c = RegionClassifier::new(StatusPatterns::claude_code());
    c.classify(&[ScreenChange::ContentRowChanged {
        row: 0,
        old: String::new(),
        new: "esc to interrupt".into(),
    }]);
    assert_eq!(c.state(), AgentState::Thinking);
    let events = c.classify(&[ScreenChange::ContentRowChanged {
        row: 5,
        old: String::new(),
        new: "You are running inside pseudomux as a sub-agent".into(),
    }]);
    assert_eq!(
        c.state(),
        AgentState::Thinking,
        "prompt text containing 'running' must NOT trigger ToolRunning"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, SemanticEvent::ToolStarted { .. })),
        "no ToolStarted event should fire for prose prompt text"
    );
}

#[test]
fn claude_code_detect_confirmation() {
    let mut c = RegionClassifier::new(StatusPatterns::claude_code());
    c.classify(&[ScreenChange::ContentRowChanged {
        row: 0,
        old: String::new(),
        new: "esc to interrupt".into(),
    }]);
    assert_eq!(c.state(), AgentState::Thinking);

    // Confirmation checking is now adapter-provided via with_confirmation_checker.
    // Test the checker function directly.
    let check = |text: &str| -> bool {
        let lower = text.to_lowercase();
        lower.starts_with("allow")
            && (lower.contains("to run")
                || lower.contains("to read")
                || lower.contains("to write")
                || lower.contains("to edit")
                || lower.contains("to execute"))
    };
    assert!(check("Allow Claude to run bash command: ls -la"));
    assert!(check("Allow Claude to read file.txt"));
    assert!(check("Allow Claude to write to output.txt"));
    assert!(check("Allow Claude to edit config.json"));
    assert!(check("Allow Claude to execute script.sh"));
    assert!(!check("Something else entirely"));
    assert!(!check("Allow but no verb match here"));
}

#[test]
fn claude_code_confirmation_emits_event_during_thinking() {
    use std::sync::Arc;
    let checker: Arc<dyn Fn(&str) -> bool + Send + Sync> = Arc::new(|text: &str| {
        let lower = text.to_lowercase();
        lower.starts_with("allow")
            && (lower.contains("to run")
                || lower.contains("to read")
                || lower.contains("to write")
                || lower.contains("to edit")
                || lower.contains("to execute"))
    });
    let mut c =
        RegionClassifier::new(StatusPatterns::claude_code()).with_confirmation_checker(checker);
    c.classify(&[ScreenChange::ContentRowChanged {
        row: 0,
        old: String::new(),
        new: "esc to interrupt".into(),
    }]);
    let events = c.classify(&[ScreenChange::ContentRowChanged {
        row: 1,
        old: String::new(),
        new: "Allow Claude to run bash command: ls -la".into(),
    }]);
    assert!(
        events.iter().any(|e| matches!(
            e,
            SemanticEvent::ConfirmationRequired { prompt_text, .. }
            if prompt_text.contains("Allow Claude to run")
        )),
        "expected ConfirmationRequired event"
    );
}

#[test]
fn claude_code_detect_ready() {
    let c = RegionClassifier::new(StatusPatterns::claude_code());
    assert_eq!(c.detect_state("? for shortcuts"), AgentState::Ready);
    assert_eq!(c.detect_state("?forshortcuts"), AgentState::Ready);
}

#[test]
fn claude_code_detect_thinking() {
    let c = RegionClassifier::new(StatusPatterns::claude_code());
    assert_eq!(c.detect_state("esc to interrupt"), AgentState::Thinking);
    assert_eq!(c.detect_state("esctointerrupt"), AgentState::Thinking);
}

#[test]
fn claude_code_scans_content() {
    let p = StatusPatterns::claude_code();
    assert!(p.scan_content_for_status);
}

#[test]
fn scan_content_disabled_does_not_match_status_in_content() {
    let mut c = RegionClassifier::new(StatusPatterns::default());
    let changes = vec![ScreenChange::ContentRowChanged {
        row: 35,
        old: String::new(),
        new: "esc interrupt".into(),
    }];
    let events = c.classify(&changes);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, SemanticEvent::AssistantDelta { .. }))
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, SemanticEvent::StateChanged { .. }))
    );
}

// --- fixture tests ---

#[test]
fn claude_code_fixture_ready_has_ready_indicator() {
    let fixture = include_str!("../../../fixtures/claude_code_ready.txt");
    assert!(
        fixture.contains("? for shortcuts") || fixture.contains("?forshortcuts"),
        "ready fixture must contain ready indicator"
    );
    assert!(
        !fixture.contains("esc to interrupt"),
        "ready fixture must not contain thinking indicator"
    );
}

#[test]
fn claude_code_fixture_thinking_has_thinking_indicator() {
    let fixture = include_str!("../../../fixtures/claude_code_thinking.txt");
    assert!(
        fixture.contains("esc to interrupt") || fixture.contains("esctointerrupt"),
        "thinking fixture must contain thinking indicator"
    );
}

#[test]
fn claude_code_fixture_tool_use_has_thinking_indicator() {
    let fixture = include_str!("../../../fixtures/claude_code_tool_use.txt");
    assert!(
        fixture.contains("esc to interrupt") || fixture.contains("esctointerrupt"),
        "tool_use fixture must contain esc to interrupt"
    );
}

#[test]
fn claude_code_fixture_response_has_ready_indicator() {
    let fixture = include_str!("../../../fixtures/claude_code_response.txt");
    assert!(
        fixture.contains("? for shortcuts") || fixture.contains("?forshortcuts"),
        "response fixture must contain ready indicator"
    );
}
