use pseudomux_core::vte::ContentFilter;

fn f(line: &str) -> Option<String> {
    ContentFilter::new().filter_line(line)
}

#[test]
fn strips_sidebar() {
    assert_eq!(
        f("  text content                         █    sidebar info"),
        Some("text content".into())
    );
}

#[test]
fn strips_left_chrome() {
    assert_eq!(f("  ┃  Hello world"), Some("Hello world".into()));
}

#[test]
fn drops_empty_chrome() {
    assert_eq!(
        f(
            "                                                                                               █"
        ),
        None
    );
}

#[test]
fn drops_border_lines() {
    assert_eq!(
        f("  ╹▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀"),
        None
    );
}

#[test]
fn preserves_tool_activity() {
    assert_eq!(
        f(
            "     → Read README.md                                                                          █"
        ),
        Some("[tool] Read README.md".into())
    );
}

#[test]
fn preserves_thinking() {
    assert_eq!(
        f(
            "  ┃  Thinking: Checking files                                                █    2% used"
        ),
        Some("[thinking] Checking files".into())
    );
}

#[test]
fn preserves_plain_text() {
    assert_eq!(
        f(
            "     Hello world                                                              █    sidebar"
        ),
        Some("Hello world".into())
    );
}

#[test]
fn drops_footer() {
    assert_eq!(f("ctrl+t variants  tab agents  ctrl+p commands"), None);
}

#[test]
fn drops_build_status() {
    assert_eq!(f("  ┃  Build  GPT-5.1 Codex mini"), None);
}

#[test]
fn preserves_build_complete() {
    assert_eq!(
        f(
            "     ▣  Build · gpt-5.1-codex-mini · 8.6s                                                     █"
        ),
        Some("[completed] gpt-5.1-codex-mini · 8.6s".into())
    );
}

#[test]
fn drops_claude_code_prompt_indicator() {
    assert_eq!(f(">"), None);
    assert_eq!(f("  >  "), None);
}

#[test]
fn handles_no_sidebar() {
    assert_eq!(
        f("plain text without sidebar"),
        Some("plain text without sidebar".into())
    );
}

#[test]
fn drops_pure_whitespace() {
    assert_eq!(f("        "), None);
}

#[test]
fn spinner_tool_activity() {
    assert_eq!(
        f(
            "     ⠋ Read README.md                                                                          █"
        ),
        Some("[tool] Read README.md".into())
    );
}

#[test]
fn tilde_finding() {
    assert_eq!(
        f(
            "     ~ Finding files...                                                                        █    $0.00 spent"
        ),
        Some("[tool] ~ Finding files...".into())
    );
}

#[test]
fn drops_bypass_permissions() {
    assert_eq!(f("⏵⏵ bypass permissions on (shift+tab to cycle)"), None);
    assert_eq!(f("⏵⏵bypasspermissionson (shift+tabtocycle)"), None);
    assert_eq!(
        f("⏵⏵ bypass permissions on (shift+tab to cycle) · esc to interrupt"),
        None
    );
    assert_eq!(
        f("⏵⏵bypasspermissionson (shift+tabtocycle)·esctointerrupt"),
        None
    );
}

#[test]
fn drops_esc_to_interrupt() {
    assert_eq!(f("esc to interrupt"), None);
    assert_eq!(f("  esc to interrupt  "), None);
}

#[test]
fn drops_shortcuts_hint() {
    assert_eq!(f("? for shortcuts"), None);
    assert_eq!(f("? for shortcuts  2 connectors need auth · /mcp"), None);
}

#[test]
fn drops_thinking_spinner() {
    assert_eq!(f("✻ Composing… (thinking)"), None);
    assert_eq!(f("· Composing… (thinking)"), None);
    assert_eq!(f("✶ Osmosing… (thinking)"), None);
}

#[test]
fn drops_tip_line() {
    assert_eq!(
        f("⎿  Tip: Run /terminal-setup to enable convenient terminal integration"),
        None
    );
}

#[test]
fn drops_tool_loaded() {
    assert_eq!(f("❯ Tool loaded."), None);
}

#[test]
fn drops_empty_prompt_arrow() {
    assert_eq!(f("❯"), None);
    assert_eq!(f("  ❯  "), None);
}

#[test]
fn tags_user_turn() {
    assert_eq!(f("❯ What is 2+2?"), Some("[user] What is 2+2?".into()));
}

#[test]
fn tags_bullet_response() {
    assert_eq!(f("● The answer is four"), Some("The answer is four".into()));
    assert_eq!(f("● 4"), Some("4".into()));
}

#[test]
fn drops_ctrl_o_expandable_lines() {
    // "(ctrl+o to expand)" is always Claude Code chrome — the inner text
    // (e.g. "Read 1 file", "Searched for 2 patterns") is itself a tool
    // summary, so the whole line is dropped rather than partially cleaned.
    assert_eq!(f("● Read 1 file (ctrl+o to expand)"), None);
    assert_eq!(f("● Searched for 2 patterns (ctrl+o to expand)"), None);
}

#[test]
fn tags_context_continuation() {
    assert_eq!(
        f("⎿  crates/core/src/vte/classifier.rs"),
        Some("[context] crates/core/src/vte/classifier.rs".into())
    );
}

// --- dedup tests ---

use pseudomux_core::vte::content_filter::deduplicate_lines;

#[test]
fn dedup_progressive_fragments() {
    let lines = vec![
        "Pseudomux is the repository".into(),
        "Pseudomux is the repository you're".into(),
        "Pseudomux is the repository you're currently inside".into(),
    ];
    let result = deduplicate_lines(lines);
    assert_eq!(
        result,
        vec!["Pseudomux is the repository you're currently inside"]
    );
}

#[test]
fn dedup_consecutive_identical() {
    let lines = vec![
        "hello".into(),
        "hello".into(),
        "hello".into(),
        "world".into(),
    ];
    let result = deduplicate_lines(lines);
    assert_eq!(result, vec!["hello", "world"]);
}

#[test]
fn dedup_consecutive_tagged() {
    let lines = vec![
        "[completed] gpt-5.1-codex-mini".into(),
        "[completed] gpt-5.1-codex-mini".into(),
        "[completed] gpt-5.1-codex-mini".into(),
        "[thinking] Preparing response".into(),
    ];
    let result = deduplicate_lines(lines);
    assert_eq!(
        result,
        vec![
            "[completed] gpt-5.1-codex-mini",
            "[thinking] Preparing response"
        ]
    );
}

#[test]
fn dedup_sidebar_leakage() {
    let lines = vec![
        "real content".into(),
        "0 tokens".into(),
        "Context".into(),
        "LSP".into(),
        "42% used".into(),
        "$0.00 spent".into(),
        "more real content".into(),
    ];
    let result = deduplicate_lines(lines);
    assert_eq!(result, vec!["real content", "more real content"]);
}

#[test]
fn dedup_timestamp_leakage() {
    let lines = vec![
        "real content".into(),
        "618Z".into(),
        "New session - 2026-03-05T20:56:52.".into(),
        "more content".into(),
    ];
    let result = deduplicate_lines(lines);
    assert_eq!(result, vec!["real content", "more content"]);
}

#[test]
fn dedup_ask_anything() {
    let lines = vec![
        "Ask anything...".into(),
        "Ask anything... \"Fix broken tests\"".into(),
        "real content".into(),
    ];
    let result = deduplicate_lines(lines);
    assert_eq!(result, vec!["real content"]);
}

#[test]
fn dedup_preserves_real_content() {
    let lines = vec![
        "First sentence.".into(),
        "Second sentence.".into(),
        "Third sentence.".into(),
    ];
    let result = deduplicate_lines(lines);
    assert_eq!(
        result,
        vec!["First sentence.", "Second sentence.", "Third sentence."]
    );
}

#[test]
fn dedup_mixed_scenario() {
    let lines = vec![
        "What is pseudomux? Answer in exactly 3 sentences.".into(),
        "Ask anything... \"Fix broken tests\"".into(),
        "What is pseudomux? Answer in exactly 3 sentences.".into(),
        "New session - 2026-03-05T20:56:52.".into(),
        "618Z".into(),
        "[completed] gpt-5.1-codex-mini".into(),
        "[completed] gpt-5.1-codex-mini".into(),
        "[completed] gpt-5.1-codex-mini".into(),
        "[thinking] Preparing concise response".into(),
        "Pseudomux is the repository you're currently inside, likely a".into(),
        "Pseudomux is the repository you're currently inside, likely a project".into(),
        "Pseudomux is the repository you're currently inside, likely a project or tool named"
            .into(),
        "[completed] gpt-5.1-codex-mini".into(),
        "its functionality beyond its name. You can share more context if you want a deeper".into(),
        "explanation".into(),
        "[completed] gpt-5.1-codex-mini · 2.5s".into(),
    ];
    let result = deduplicate_lines(lines);
    assert_eq!(
        result,
        vec![
            "What is pseudomux? Answer in exactly 3 sentences.",
            "[completed] gpt-5.1-codex-mini",
            "[thinking] Preparing concise response",
            "Pseudomux is the repository you're currently inside, likely a project or tool named",
            "[completed] gpt-5.1-codex-mini",
            "its functionality beyond its name. You can share more context if you want a deeper",
            "explanation",
            "[completed] gpt-5.1-codex-mini · 2.5s",
        ]
    );
}

#[test]
fn dedup_doesnt_merge_unrelated() {
    let lines = vec![
        "Pseudomux is great".into(),
        "Something else entirely".into(),
        "Pseudomux is great".into(),
    ];
    let result = deduplicate_lines(lines);
    assert_eq!(
        result,
        vec![
            "Pseudomux is great",
            "Something else entirely",
            "Pseudomux is great"
        ]
    );
}

#[test]
fn dedup_non_adjacent_progressive_streaming() {
    // Simulates ink/React re-rendering multiple rows in one batch: progressive
    // fragments of "row A" and "row B" get interleaved in the content buffer.
    // Phase 2's adjacent-only check can't see these pairs; Phase 3 should.
    let lines = vec![
        "Hello".into(),
        "Row".into(),
        "Hello wor".into(),
        "Row X".into(),
        "Hello world!".into(),
        "Row Xyz done".into(),
    ];
    let result = deduplicate_lines(lines);
    assert_eq!(result, vec!["Hello world!", "Row Xyz done"]);
}

#[test]
fn dedup_non_adjacent_preserves_equal_repeats() {
    // Phase 3 only drops *strict* prefixes (longer, not equal). Equal non-
    // adjacent duplicates are unrelated repeats, not streaming fragments.
    let lines = vec![
        "checkpoint".into(),
        "something in between".into(),
        "checkpoint".into(),
    ];
    let result = deduplicate_lines(lines);
    assert_eq!(
        result,
        vec!["checkpoint", "something in between", "checkpoint"]
    );
}

// --- extract tests ---

use pseudomux_core::vte::content_filter::extract_response_text;

#[test]
fn drops_whisking_spinner() {
    assert_eq!(f("✢ Whisking…"), None);
    assert_eq!(f("* Whisking…"), None);
    assert_eq!(f("· Boogieing…"), None);
    assert_eq!(f("✶ Osmosing…"), None);
    assert_eq!(f("✻ Composing…"), None);
    assert_eq!(f("✽ Pondering…"), None);
}

#[test]
fn drops_spinner_with_progress_info() {
    // Claude Code shows progress after the ellipsis during a turn, and after
    // turn-end while stop hooks are running. Both should be filtered out.
    assert_eq!(f("✻ Osmosing… (22s · ↓ 529 tokens)"), None);
    assert_eq!(f("✽ Osmosing… (22s · ↓ 529 tokens)"), None);
    assert_eq!(
        f("✻ Osmosing… (running stop hook · 22s · ↓ 529 tokens)"),
        None
    );
    assert_eq!(f("* Beaming… (20s · ↓ 488 tokens)"), None);
    assert_eq!(
        f("✶ Beaming… (running stop hook · 21s · ↓ 488 tokens)"),
        None
    );
    assert_eq!(
        f("⏺ Wibbling… (running stop hook · 16s · ↓ 518 tokens)"),
        None
    );
    assert_eq!(f("✻ Brewed for 33s"), None);
    assert_eq!(f("✻ Cooked for 39s"), None);
}

#[test]
fn drops_spinner_thought_for_variant() {
    // Newer Claude Code spinner variant: "(thought for Ns)"
    assert_eq!(f("✶ Scurrying… (thought for 1s)"), None);
    assert_eq!(f("* Scurrying… (thought for 12s)"), None);
    assert_eq!(f("✻ Scurrying… (running stop hook · thought for 3s)"), None);
}

#[test]
fn drops_task_tree_progress_indicators() {
    // Claude Code's Task tool renders a tree-structured progress view.
    // Each sub-agent gets a row with a tree glyph + description + tool count.
    assert_eq!(f("─ Ocean poem · 0 tool uses"), None);
    assert_eq!(f("└─ Silent forest poem · 0 tool uses"), None);
    assert_eq!(f("├─ Distant star poem · 1 tool use"), None);
    // Intermediate branch markers with tool-use counter
    assert_eq!(f("┌─ root · 0 tool uses"), None);
    assert_eq!(f("┤ joining · 2 tool uses"), None);
    // But preserve ordinary prose that happens to start with a tree glyph
    // without tool-use signal or dot-separator (don't care about exact
    // leading-char trimming — just assert the line survives).
    assert!(f("─── this is a divider-like header for a section").is_some());
}

#[test]
fn drops_task_batch_summary() {
    // Task batch completion: "N agents finished", optionally with tool count.
    assert_eq!(f("3 agents finished"), None);
    assert_eq!(f("1 agent finished"), None);
    assert_eq!(f("5 agents finished (12 tool uses)"), None);
    assert_eq!(f("2 agents finished (3 tool uses · 15.2k tokens)"), None);
    // But NOT: a sentence that happens to start with a digit
    assert_eq!(
        f("3 apples were eaten"),
        Some("3 apples were eaten".to_string())
    );
}

#[test]
fn drops_keybinding_hint_lines() {
    // "(ctrl+X to Y)" and "Shift + Enter ..." hints Claude Code shows.
    assert_eq!(f("(ctrl+b to run in background)"), None);
    assert_eq!(f("(ctrl+o to expand)"), None);
    assert_eq!(f("(ctrl+c to exit)"), None);
    assert_eq!(f("Shift + Enter for new line and more"), None);
    assert_eq!(f("shift+enter for newline"), None);
    // But not: prose that happens to mention ctrl+b
    assert_eq!(
        f("Press ctrl+b in tmux to switch panes."),
        Some("Press ctrl+b in tmux to switch panes.".to_string())
    );
}

#[test]
fn drops_effort_and_model_status() {
    // Effort/model indicator without bullet glyph, various forms.
    assert_eq!(f("high · /effort"), None);
    assert_eq!(f("medium · /effort"), None);
    assert_eq!(f("low · /effort"), None);
    assert_eq!(f("sonnet · /model"), None);
    // The line ends with /effort or /model
    assert_eq!(f("something trailing /effort"), None);
    // But NOT: prose that mentions /effort as part of a path
    assert_eq!(
        f("Check ./scripts/run.sh --effort for details."),
        Some("Check ./scripts/run.sh --effort for details.".to_string())
    );
}

#[test]
fn drops_bare_running_tool_progress() {
    assert_eq!(f("Running…"), None);
    assert_eq!(f("Running..."), None);
    assert_eq!(f("  Running…  "), None);
    // But not: prose starting with "Running"
    assert_eq!(
        f("Running integration tests requires a local daemon."),
        Some("Running integration tests requires a local daemon.".to_string())
    );
}

#[test]
fn drops_short_ui_hint_fragments() {
    // Claude Code UI labels that leak into short responses. Matched as EXACT
    // short lines, so common words like "later" don't false-drop when they
    // appear in real prose.
    assert_eq!(f("later"), None);
    assert_eq!(f("point in time"), None);
    assert_eq!(f("beneath the input box"), None);
    assert_eq!(f("shift+tab to cycle"), None);
    assert_eq!(f("mode)"), None);
    // But NOT: prose containing these phrases
    assert_eq!(
        f("See you later when we meet again."),
        Some("See you later when we meet again.".to_string())
    );
    assert_eq!(
        f("A specific point in time is when the event occurred."),
        Some("A specific point in time is when the event occurred.".to_string())
    );
}

#[test]
fn drops_lines_with_ctrl_o_to_expand() {
    // "(ctrl+o to expand)" is a blanket chrome signal — any line with it is a
    // Claude Code expandable tool-output box, never prose.
    assert_eq!(f("Reading 1 file… (ctrl+o to expand)"), None);
    assert_eq!(f("Read 2 files (ctrl+o to expand)"), None);
    assert_eq!(f("● 2 agents finished (ctrl+o to expand)"), None);
    assert_eq!(f("● Running 2 agents… (ctrl+o to expand)"), None);
    assert_eq!(
        f("  ⎿  Done (1 tool use · 31.5k tokens · 4s) (ctrl+o to expand)"),
        None
    );
}

#[test]
fn drops_tree_glyph_with_embedded_continuation() {
    // Tree-glyph lines embed ⎿ inside the tree indentation:
    //    │  ⎿  Initializing…
    //       ⎿  Done
    // We need to catch these even when the tree glyph is the first non-space
    // char (not the ⎿).
    assert_eq!(f("   │  ⎿  Initializing…"), None);
    assert_eq!(f("│  ⎿  Done"), None);
    assert_eq!(f("├─ Ocean 3-line poem · 0 tool uses"), None);
    assert_eq!(f("└─ Forest 3-line poem · 0 tool uses"), None);
}

#[test]
fn drops_bulleted_agent_chrome() {
    // "●" bullet + chrome phrase: batch summaries, running-agents progress.
    // The bullet is stripped later in filter_line, but we catch it here.
    assert_eq!(f("● 2 agents finished"), None);
    assert_eq!(f("● 3 agents finished (5 tool uses)"), None);
    assert_eq!(f("● Running 2 agents…"), None);
    assert_eq!(f("● Running 1 agent…"), None);
    // But NOT: bullet + genuine prose
    assert_eq!(
        f("● Pseudomux is a PTY multiplexer written in Rust."),
        Some("Pseudomux is a PTY multiplexer written in Rust.".to_string())
    );
}

#[test]
fn drops_pasting_echo() {
    assert_eq!(f("Pasting text…"), None);
}

#[test]
fn drops_dot_separator_line() {
    assert_eq!(f("────────────────── ▪▪▪ ─"), None);
    assert_eq!(f("──────────────────▪▪▪─"), None);
}

#[test]
fn drops_welcome_box_chrome() {
    assert_eq!(f("╭───ClaudeCodev2.1.70───╮"), None);
    assert_eq!(f("╰─────────╯"), None);
}

#[test]
fn extract_response_after_user_line() {
    let lines = vec![
        "[user] List 3 benefits of Rust.".into(),
        "1. Memory safety".into(),
        "2. Zero-cost abstractions".into(),
        "3. Fearless concurrency".into(),
    ];
    let result = extract_response_text(lines);
    assert_eq!(
        result,
        vec![
            "1. Memory safety",
            "2. Zero-cost abstractions",
            "3. Fearless concurrency",
        ]
    );
}

#[test]
fn extract_deduplicates_user_lines() {
    let lines = vec![
        "[user] List3benefitsofRust.".into(),
        "[user] List 3 benefits of Rust.".into(),
        "1. Memory safety".into(),
    ];
    let result = extract_response_text(lines);
    assert_eq!(result, vec!["1. Memory safety"]);
}

#[test]
fn extract_strips_trailing_empty() {
    let lines = vec!["[user] question".into(), "answer".into(), "".into()];
    let result = extract_response_text(lines);
    assert_eq!(result, vec!["answer"]);
}
