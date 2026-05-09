use pseudomux_core::vte::{ContentBuffer, ContentTag};

#[test]
fn new_buffer_is_empty() {
    let buf = ContentBuffer::new(1024);
    assert!(buf.is_empty());
    assert_eq!(buf.len(), 0);
    assert_eq!(buf.current_seq(), 0);
}

#[test]
fn append_increments_seq() {
    let mut buf = ContentBuffer::new(1024);
    assert_eq!(buf.append("hello".into(), ContentTag::Unknown), 1);
    assert_eq!(buf.append("world".into(), ContentTag::Unknown), 2);
    assert_eq!(buf.append("foo".into(), ContentTag::Unknown), 3);
    assert_eq!(buf.current_seq(), 3);
    assert_eq!(buf.len(), 3);
}

#[test]
fn since_seq_returns_newer() {
    let mut buf = ContentBuffer::new(1024);
    buf.append("a".into(), ContentTag::Unknown);
    buf.append("b".into(), ContentTag::Unknown);
    buf.append("c".into(), ContentTag::Unknown);
    let entries = buf.since_seq(1);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].text, "b");
    assert_eq!(entries[1].text, "c");
}

#[test]
fn since_seq_empty_when_current() {
    let mut buf = ContentBuffer::new(1024);
    buf.append("a".into(), ContentTag::Unknown);
    let entries = buf.since_seq(buf.current_seq());
    assert!(entries.is_empty());
}

#[test]
fn mark_input_boundary() {
    let mut buf = ContentBuffer::new(1024);
    let seq = buf.mark_input_boundary();
    assert_eq!(seq, 1);
    assert_eq!(buf.len(), 1);
    let entry = &buf.entries[0];
    assert_eq!(entry.tag, ContentTag::InputBoundary);
    assert!(entry.text.is_empty());
}

#[test]
fn since_last_input() {
    let mut buf = ContentBuffer::new(1024);
    buf.append("before".into(), ContentTag::AssistantOutput);
    buf.mark_input_boundary();
    buf.append("after1".into(), ContentTag::AssistantOutput);
    buf.append("after2".into(), ContentTag::ToolOutput);
    let entries = buf.since_last_input();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].text, "after1");
    assert_eq!(entries[1].text, "after2");
}

#[test]
fn since_last_input_no_boundary() {
    let mut buf = ContentBuffer::new(1024);
    buf.append("a".into(), ContentTag::Unknown);
    buf.append("b".into(), ContentTag::Unknown);
    let entries = buf.since_last_input();
    assert_eq!(entries.len(), 2);
}

#[test]
fn text_since_seq_joins_with_newlines() {
    let mut buf = ContentBuffer::new(1024);
    buf.append("hello".into(), ContentTag::Unknown);
    buf.append("world".into(), ContentTag::Unknown);
    buf.append("foo".into(), ContentTag::Unknown);
    let text = buf.text_since_seq(1);
    assert_eq!(text, "world\nfoo");
}

#[test]
fn capacity_eviction() {
    let mut buf = ContentBuffer::new(10);
    buf.append("aaaaa".into(), ContentTag::Unknown);
    buf.append("bbbbb".into(), ContentTag::Unknown);
    assert_eq!(buf.len(), 2);
    buf.append("ccccc".into(), ContentTag::Unknown);
    assert!(buf.current_bytes <= 10);
    assert_eq!(buf.entries.front().unwrap().text, "bbbbb");
}

#[test]
fn last_n_excludes_boundaries() {
    let mut buf = ContentBuffer::new(1024);
    buf.append("a".into(), ContentTag::Unknown);
    buf.mark_input_boundary();
    buf.append("b".into(), ContentTag::Unknown);
    buf.mark_input_boundary();
    buf.append("c".into(), ContentTag::Unknown);
    let entries = buf.last_n(2);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].text, "b");
    assert_eq!(entries[1].text, "c");
}

#[test]
fn text_since_last_input() {
    let mut buf = ContentBuffer::new(1024);
    buf.append("old".into(), ContentTag::Unknown);
    buf.mark_input_boundary();
    buf.append("new1".into(), ContentTag::AssistantOutput);
    buf.append("new2".into(), ContentTag::ToolOutput);
    let text = buf.text_since_last_input();
    assert_eq!(text, "new1\nnew2");
}

#[test]
fn multiple_boundaries() {
    let mut buf = ContentBuffer::new(1024);
    buf.append("round1".into(), ContentTag::Unknown);
    buf.mark_input_boundary();
    buf.append("round2".into(), ContentTag::Unknown);
    buf.mark_input_boundary();
    buf.append("round3a".into(), ContentTag::Unknown);
    buf.append("round3b".into(), ContentTag::Unknown);
    let entries = buf.since_last_input();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].text, "round3a");
    assert_eq!(entries[1].text, "round3b");
}

#[test]
fn row_aware_collapses_ink_rerenders() {
    // Simulates ink rewriting the same screen rows on every streamed token:
    // row 5 goes through progressive values, row 6 too, then ink re-renders
    // the whole content region producing the SAME row values again.
    // The row-aware snapshot should return only the final per-row values.
    use pseudomux_core::vte::ContentFilter;
    let mut buf = ContentBuffer::new(16_384);
    buf.mark_input_boundary();
    // First render burst
    buf.append_with_row("Pseud".into(), ContentTag::AssistantOutput, 5);
    buf.append_with_row("Pseudomux Review".into(), ContentTag::AssistantOutput, 5);
    buf.append_with_row("1.".into(), ContentTag::AssistantOutput, 6);
    buf.append_with_row("1. What is it?".into(), ContentTag::AssistantOutput, 6);
    // ink re-renders the whole region (same values, extra writes)
    buf.append_with_row("Pseudomux Review".into(), ContentTag::AssistantOutput, 5);
    buf.append_with_row("1. What is it?".into(), ContentTag::AssistantOutput, 6);
    // New content appears on a later row
    buf.append_with_row(
        "Pseudomux is an SDK-grade PTY multiplexer.".into(),
        ContentTag::AssistantOutput,
        7,
    );
    // Another full re-render
    buf.append_with_row("Pseudomux Review".into(), ContentTag::AssistantOutput, 5);
    buf.append_with_row("1. What is it?".into(), ContentTag::AssistantOutput, 6);
    buf.append_with_row(
        "Pseudomux is an SDK-grade PTY multiplexer.".into(),
        ContentTag::AssistantOutput,
        7,
    );

    let text = buf.filtered_text_latest_per_row_since_last_input(&ContentFilter::default());
    assert_eq!(
        text,
        "Pseudomux Review\n1. What is it?\nPseudomux is an SDK-grade PTY multiplexer."
    );
}

#[test]
fn row_aware_passes_through_rowless_entries() {
    // Entries without a row (synthetic, or from legacy callers) should be
    // kept in arrival order and not collapsed.
    use pseudomux_core::vte::ContentFilter;
    let mut buf = ContentBuffer::new(1024);
    buf.mark_input_boundary();
    buf.append("untagged line one".into(), ContentTag::AssistantOutput);
    buf.append("untagged line two".into(), ContentTag::AssistantOutput);
    let text = buf.filtered_text_latest_per_row_since_last_input(&ContentFilter::default());
    assert_eq!(text, "untagged line one\nuntagged line two");
}

#[test]
fn row_aware_handles_scroll_up_repeats() {
    // Simulates a vt100 scroll: content that occupied rows 2-4 shifts up
    // to rows 1-3, and a new row 4 appears. Each line should be emitted
    // once, in its original (pre-scroll) position, plus the newly appeared
    // content at the scrolled-in row.
    use pseudomux_core::vte::ContentFilter;
    let mut buf = ContentBuffer::new(16_384);
    buf.mark_input_boundary();
    // Initial write
    buf.append_with_row("header".into(), ContentTag::AssistantOutput, 1);
    buf.append_with_row("line A".into(), ContentTag::AssistantOutput, 2);
    buf.append_with_row("line B".into(), ContentTag::AssistantOutput, 3);
    buf.append_with_row("line C".into(), ContentTag::AssistantOutput, 4);
    // Scroll: every row shifts up by one, new content at bottom
    buf.append_with_row("header".into(), ContentTag::AssistantOutput, 0);
    buf.append_with_row("line A".into(), ContentTag::AssistantOutput, 1);
    buf.append_with_row("line B".into(), ContentTag::AssistantOutput, 2);
    buf.append_with_row("line C".into(), ContentTag::AssistantOutput, 3);
    buf.append_with_row("line D".into(), ContentTag::AssistantOutput, 4);

    let text = buf.filtered_text_latest_per_row_since_last_input(&ContentFilter::default());
    assert_eq!(text, "header\nline A\nline B\nline C\nline D");
}

#[test]
fn row_aware_combined_scroll_and_rerender() {
    // Mix of progressive streaming, full re-render, and scroll — the three
    // kinds of duplication must all be collapsed correctly in one pass.
    use pseudomux_core::vte::ContentFilter;
    let mut buf = ContentBuffer::new(16_384);
    buf.mark_input_boundary();
    // Progressive streaming on row 3
    buf.append_with_row("Pseu".into(), ContentTag::AssistantOutput, 3);
    buf.append_with_row("Pseudomux".into(), ContentTag::AssistantOutput, 3);
    buf.append_with_row("Pseudomux Review".into(), ContentTag::AssistantOutput, 3);
    // Full re-render (same values written again)
    buf.append_with_row("Pseudomux Review".into(), ContentTag::AssistantOutput, 3);
    // New line streams in on row 4
    buf.append_with_row("1. Overview".into(), ContentTag::AssistantOutput, 4);
    buf.append_with_row("1. Overview".into(), ContentTag::AssistantOutput, 4);
    // Scroll: everything shifts up
    buf.append_with_row("Pseudomux Review".into(), ContentTag::AssistantOutput, 2);
    buf.append_with_row("1. Overview".into(), ContentTag::AssistantOutput, 3);
    buf.append_with_row("Body paragraph".into(), ContentTag::AssistantOutput, 4);

    let text = buf.filtered_text_latest_per_row_since_last_input(&ContentFilter::default());
    assert_eq!(text, "Pseudomux Review\n1. Overview\nBody paragraph");
}
