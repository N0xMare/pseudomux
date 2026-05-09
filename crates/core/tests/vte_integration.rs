//! Full VTE pipeline integration tests for Claude Code fixtures.

use pseudomux_core::vte::{
    AgentState, ContentFilter, RegionClassifier, ScreenModel, ScreenRegions, SemanticEvent,
    StatusPatterns, WatchEvent, WatchEventBuilder,
};

fn to_pty_bytes(s: &str) -> Vec<u8> {
    s.replace('\n', "\r\n").into_bytes()
}

fn claude_code_model() -> ScreenModel {
    let regions = ScreenRegions::claude_code(50, 120);
    ScreenModel::new(50, 120, 0, regions)
}

fn filtered_lines(text: &str) -> Vec<String> {
    let filter = ContentFilter::new();
    text.lines()
        .filter_map(|line| filter.filter_line(line))
        .collect()
}

#[test]
fn claude_code_ready_state_detection() {
    let mut model = claude_code_model();
    let fixture = include_str!("../../../fixtures/claude_code_ready.txt");
    let changes = model.process(&to_pty_bytes(fixture));

    let mut classifier = RegionClassifier::new(StatusPatterns::claude_code());
    let _events = classifier.classify(&changes);

    assert_eq!(
        classifier.state(),
        AgentState::Ready,
        "Ready fixture must produce Ready state"
    );

    let content = model.content_text();
    let lines = filtered_lines(&content);

    assert!(
        !lines.iter().any(|l| l.contains("? for shortcuts")),
        "Status indicator must not appear in filtered content"
    );
    assert!(
        !lines.iter().any(|l| l.contains("esc to interrupt")),
        "Thinking indicator must not appear in filtered content"
    );
}

#[test]
fn claude_code_thinking_state_detection() {
    let mut model = claude_code_model();
    let fixture = include_str!("../../../fixtures/claude_code_thinking.txt");
    let changes = model.process(&to_pty_bytes(fixture));

    let mut classifier = RegionClassifier::new(StatusPatterns::claude_code());
    let _events = classifier.classify(&changes);

    assert_eq!(
        classifier.state(),
        AgentState::Thinking,
        "Thinking fixture must produce Thinking state"
    );

    let content = model.content_text();
    let lines = filtered_lines(&content);

    assert!(
        !lines.iter().any(|l| l.contains("(thinking)")),
        "Filtered content must not contain spinner (thinking) lines"
    );
    assert!(
        !lines.iter().any(|l| l.to_lowercase().contains("tip:")),
        "Filtered content must not contain Tip: lines"
    );
}

#[test]
fn claude_code_tool_use_state_detection() {
    let mut model = claude_code_model();
    let fixture = include_str!("../../../fixtures/claude_code_tool_use.txt");
    let changes = model.process(&to_pty_bytes(fixture));

    let mut classifier = RegionClassifier::new(StatusPatterns::claude_code());
    let events = classifier.classify(&changes);

    assert_eq!(
        classifier.state(),
        AgentState::Thinking,
        "Tool-use fixture must produce Thinking state (esc to interrupt)"
    );

    let has_state_change = events
        .iter()
        .any(|e| matches!(e, SemanticEvent::StateChanged { .. }));
    assert!(
        has_state_change,
        "Must emit at least one StateChanged event"
    );
}

#[test]
fn claude_code_response_state_detection() {
    let mut model = claude_code_model();
    let fixture = include_str!("../../../fixtures/claude_code_response.txt");
    let changes = model.process(&to_pty_bytes(fixture));

    let mut classifier = RegionClassifier::new(StatusPatterns::claude_code());
    let _events = classifier.classify(&changes);

    assert_eq!(
        classifier.state(),
        AgentState::Ready,
        "Response fixture must produce Ready state"
    );

    let content = model.content_text();
    let lines = filtered_lines(&content);

    assert!(
        lines.iter().any(|l| l.trim() == "4"),
        "Filtered content must contain the clean response '4'; got: {lines:?}"
    );

    assert!(
        lines.iter().any(|l| l.starts_with("[user]")),
        "Filtered content must tag user turns with [user]; got: {lines:?}"
    );

    assert!(
        !lines.iter().any(|l| l.starts_with('●')),
        "Bullet markers must be stripped from filtered content"
    );
}

#[test]
fn claude_code_error_state_detection() {
    let mut model = claude_code_model();
    let fixture = include_str!("../../../fixtures/claude_code_error.txt");
    let changes = model.process(&to_pty_bytes(fixture));

    let mut classifier = RegionClassifier::new(StatusPatterns::claude_code());
    let events = classifier.classify(&changes);

    let has_error_transition = events.iter().any(|e| {
        matches!(
            e,
            SemanticEvent::StateChanged {
                to: AgentState::Error,
                ..
            }
        )
    });
    assert!(
        has_error_transition,
        "Error fixture must emit a StateChanged-to-Error event"
    );

    let content = model.content_text();
    let lines = filtered_lines(&content);
    let full = lines.join(" ");
    assert!(
        full.contains("API Error") || full.contains("weekly limit"),
        "Filtered content must preserve error message; got: {lines:?}"
    );
}

#[test]
fn claude_code_thinking_to_ready_lifecycle() {
    let mut model = claude_code_model();
    let mut classifier = RegionClassifier::new(StatusPatterns::claude_code());

    let thinking = include_str!("../../../fixtures/claude_code_thinking.txt");
    let changes1 = model.process(&to_pty_bytes(thinking));
    let events1 = classifier.classify(&changes1);
    assert_eq!(classifier.state(), AgentState::Thinking);
    assert!(
        events1
            .iter()
            .any(|e| matches!(e, SemanticEvent::ThinkingStarted { .. })),
        "Must emit ThinkingStarted after thinking fixture"
    );

    let changes2 = model.process(b"\x1b[2J\x1b[H");
    let _events2 = classifier.classify(&changes2);

    let response = include_str!("../../../fixtures/claude_code_response.txt");
    let changes3 = model.process(&to_pty_bytes(response));
    let events3 = classifier.classify(&changes3);

    assert_eq!(
        classifier.state(),
        AgentState::Ready,
        "Must reach Ready state after response fixture"
    );

    let has_thinking_completed = events3
        .iter()
        .any(|e| matches!(e, SemanticEvent::ThinkingCompleted { .. }));
    let has_state_to_ready = events3.iter().any(|e| {
        matches!(
            e,
            SemanticEvent::StateChanged {
                to: AgentState::Ready,
                ..
            }
        )
    });
    assert!(
        has_thinking_completed,
        "Must emit ThinkingCompleted when transitioning from Thinking to Ready"
    );
    assert!(
        has_state_to_ready,
        "Must emit StateChanged(→Ready) when response fixture processed"
    );
}

#[test]
fn claude_code_content_filter_no_chrome_leakage() {
    let mut model = claude_code_model();
    let fixture = include_str!("../../../fixtures/claude_code_response.txt");
    model.process(&to_pty_bytes(fixture));

    let content = model.content_text();
    let lines = filtered_lines(&content);

    for line in &lines {
        assert!(
            !line.contains("? for shortcuts"),
            "Status indicator leaked: {line:?}"
        );
        assert!(
            !line.contains("esc to interrupt"),
            "Thinking indicator leaked: {line:?}"
        );
        assert!(
            !(line.contains('\u{276F}') && line.contains("Tool loaded")),
            "Tool loaded chrome leaked: {line:?}"
        );
        assert!(
            !line.contains("(ctrl+o to expand)"),
            "(ctrl+o to expand) leaked: {line:?}"
        );
    }
}

#[test]
fn claude_code_watch_events_sequence() {
    let mut model = claude_code_model();
    let fixture = include_str!("../../../fixtures/claude_code_thinking.txt");
    let changes = model.process(&to_pty_bytes(fixture));

    let mut classifier = RegionClassifier::new(StatusPatterns::claude_code());
    let semantic_events = classifier.classify(&changes);

    let mut builder = WatchEventBuilder::new();
    let mut watch_events: Vec<WatchEvent> = Vec::new();
    for evt in &semantic_events {
        watch_events.extend(builder.process(evt));
    }
    if let Some(flushed) = builder.flush_pending() {
        watch_events.push(flushed);
    }

    let has_thinking_state = watch_events
        .iter()
        .any(|e| matches!(e, WatchEvent::StateChange { to, .. } if to == "Thinking"));
    assert!(
        has_thinking_state,
        "WatchEvents must include StateChange to Thinking; got: {watch_events:?}"
    );

    let has_content_delta = watch_events
        .iter()
        .any(|e| matches!(e, WatchEvent::ContentDelta { .. }));
    assert!(
        has_content_delta,
        "WatchEvents must include ContentDelta events; got: {watch_events:?}"
    );

    assert_eq!(classifier.state(), AgentState::Thinking);
}
