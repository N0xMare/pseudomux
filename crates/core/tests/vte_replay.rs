//! Replay test framework for PTY byte stream fixtures.
//!
//! Feeds recorded (or synthetic) byte streams through the full VTE pipeline
//! and asserts on the resulting semantic events, watch events, and content.

use pseudomux_core::vte::{
    AgentState, ContentBuffer, ContentFilter, ContentTag, RegionClassifier, ScreenChange,
    ScreenModel, ScreenRegions, SemanticEvent, StatusPatterns, WatchEvent, WatchEventBuilder,
};

/// Result of replaying a byte stream through the VTE pipeline.
#[derive(Debug)]
struct ReplayResult {
    semantic_events: Vec<SemanticEvent>,
    watch_events: Vec<WatchEvent>,
    content_text: String,
    final_state: AgentState,
}

/// Replay raw bytes through the full VTE pipeline.
fn replay_bytes(
    bytes: &[u8],
    rows: u16,
    cols: u16,
    regions: ScreenRegions,
    patterns: StatusPatterns,
) -> ReplayResult {
    let mut model = ScreenModel::new(rows, cols, 1000, regions);
    let mut classifier = RegionClassifier::new(patterns);
    let mut content_buffer = ContentBuffer::default();
    let mut watch_builder = WatchEventBuilder::new();

    let mut all_semantic = Vec::new();
    let mut all_watch = Vec::new();

    // Feed bytes in chunks to simulate real PTY reads
    let chunk_size = 4096;
    for chunk in bytes.chunks(chunk_size) {
        let screen_changes = model.process(chunk);

        let semantic_events = if screen_changes.is_empty() {
            vec![]
        } else {
            classifier.classify(&screen_changes)
        };

        // Content buffer: capture content row changes
        for change in &screen_changes {
            if let ScreenChange::ContentRowChanged { new, .. } = change {
                if !new.is_empty() {
                    let tag = match classifier.state() {
                        AgentState::Thinking => ContentTag::AssistantOutput,
                        AgentState::ToolRunning => ContentTag::ToolOutput,
                        _ => ContentTag::Unknown,
                    };
                    content_buffer.append(new.clone(), tag);
                }
            }
        }

        // Watch events
        for event in &semantic_events {
            let watch_events = watch_builder.process(event);
            all_watch.extend(watch_events);
        }

        all_semantic.extend(semantic_events);
    }

    let content_text = model.content_text();
    let final_state = classifier.state();

    ReplayResult {
        semantic_events: all_semantic,
        watch_events: all_watch,
        content_text,
        final_state,
    }
}

/// Convert a text fixture to synthetic PTY bytes (for testing with existing fixtures).
fn text_to_pty_bytes(s: &str) -> Vec<u8> {
    s.replace("\n", "\r\n").into_bytes()
}

// --- Tests using existing text fixtures ---

#[test]
fn replay_claude_code_ready() {
    let fixture = include_str!("../../../fixtures/claude_code_ready.txt");
    let bytes = text_to_pty_bytes(fixture);
    let regions = ScreenRegions::claude_code(50, 120);
    let patterns = StatusPatterns::claude_code();

    let result = replay_bytes(&bytes, 50, 120, regions, patterns);

    assert_eq!(
        result.final_state,
        AgentState::Ready,
        "Expected Ready state"
    );

    // Content should not contain status bar text
    let filter = ContentFilter::new();
    let filtered: Vec<String> = result
        .content_text
        .lines()
        .filter_map(|line| filter.filter_line(line))
        .collect();
    assert!(
        !filtered.iter().any(|l| l.contains("? for shortcuts")),
        "Status indicator leaked into content"
    );
}

#[test]
fn replay_claude_code_thinking() {
    let fixture = include_str!("../../../fixtures/claude_code_thinking.txt");
    let bytes = text_to_pty_bytes(fixture);
    let regions = ScreenRegions::claude_code(50, 120);
    let patterns = StatusPatterns::claude_code();

    let result = replay_bytes(&bytes, 50, 120, regions, patterns);

    assert!(
        matches!(
            result.final_state,
            AgentState::Thinking | AgentState::Booting
        ),
        "Expected Thinking or Booting from text fixture, got {:?}",
        result.final_state
    );
}

#[test]
fn replay_claude_code_tool_use() {
    let fixture = include_str!("../../../fixtures/claude_code_tool_use.txt");
    let bytes = text_to_pty_bytes(fixture);
    let regions = ScreenRegions::claude_code(50, 120);
    let patterns = StatusPatterns::claude_code();

    let result = replay_bytes(&bytes, 50, 120, regions, patterns);

    assert!(
        matches!(
            result.final_state,
            AgentState::ToolRunning | AgentState::Thinking
        ),
        "Expected ToolRunning or Thinking from text fixture, got {:?}",
        result.final_state
    );
}

#[test]
fn replay_opencode_ready() {
    let fixture = include_str!("../../../fixtures/opencode_v1_2_ready.txt");
    let bytes = text_to_pty_bytes(fixture);
    let regions = ScreenRegions::opencode(50, 120);
    let patterns = StatusPatterns::opencode_v1_2();

    let result = replay_bytes(&bytes, 50, 120, regions, patterns);

    assert_eq!(
        result.final_state,
        AgentState::Ready,
        "Expected Ready state"
    );
}

#[test]
fn replay_opencode_thinking() {
    let fixture = include_str!("../../../fixtures/opencode_v1_2_thinking.txt");
    let bytes = text_to_pty_bytes(fixture);
    let regions = ScreenRegions::opencode(50, 120);
    let patterns = StatusPatterns::opencode_v1_2();

    let result = replay_bytes(&bytes, 50, 120, regions, patterns);

    assert!(
        matches!(
            result.final_state,
            AgentState::Thinking | AgentState::Booting
        ),
        "Expected Thinking or Booting from text fixture, got {:?}",
        result.final_state
    );
}

#[test]
fn replay_produces_semantic_events() {
    let fixture = include_str!("../../../fixtures/claude_code_thinking.txt");
    let bytes = text_to_pty_bytes(fixture);
    let regions = ScreenRegions::claude_code(50, 120);
    let patterns = StatusPatterns::claude_code();

    let result = replay_bytes(&bytes, 50, 120, regions, patterns);

    // Should produce at least some semantic events from processing a non-trivial fixture
    assert!(
        !result.semantic_events.is_empty(),
        "Expected semantic events from thinking fixture, got none"
    );
    assert!(
        !result.watch_events.is_empty(),
        "Expected watch events from thinking fixture, got none"
    );
}

#[test]
fn replay_chunked_vs_whole() {
    // Verify that chunked replay produces the same final state as whole-buffer replay
    let fixture = include_str!("../../../fixtures/claude_code_response.txt");
    let bytes = text_to_pty_bytes(fixture);
    let regions = ScreenRegions::claude_code(50, 120);
    let patterns = StatusPatterns::claude_code();

    let regions1 = ScreenRegions::claude_code(50, 120);
    let patterns1 = StatusPatterns::claude_code();
    let result_chunked = replay_bytes(&bytes, 50, 120, regions, patterns);

    // Also process as single chunk
    let mut model = ScreenModel::new(50, 120, 1000, regions1);
    let changes = model.process(&bytes);
    let mut classifier = RegionClassifier::new(patterns1);
    let _events = classifier.classify(&changes);
    let single_state = classifier.state();

    assert_eq!(
        result_chunked.final_state, single_state,
        "Chunked and single-pass replay should produce same final state"
    );
}
