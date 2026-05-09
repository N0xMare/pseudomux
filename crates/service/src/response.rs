//! Post-processing for TUI agent responses.
//!
//! The VTE content filter strips most chrome at ingest time, but some chrome
//! is only recognizable in context of the whole response — specifically the
//! user's prompt echo (which wraps across multiple rows but only the first
//! carries a `❯` marker) and tool-invocation chrome that can reappear mid-
//! response when ink re-renders.
//!
//! This module consolidates those post-processing passes so the same logic is
//! applied whether the consumer is the `pmux` CLI, the HTTP API, or an
//! embedded library user.

/// Clean up a raw filtered response by removing prompt echo and tool-use chrome.
///
/// Two passes:
///
///   1. **Leading-only prompt echo**: drop leading lines whose word-normalized
///      text is a contiguous substring of the word-normalized prompt. Catches
///      wrap-continuation rows of the user prompt that the content filter's
///      `[user]` tag only marks on the first row. Stops at the first
///      non-matching line so legitimate prose isn't trimmed.
///
///   2. **Anywhere tool chrome**: drop any line (not just leading) that
///      matches Claude Code tool-use chrome — `[context] ...`, gerund progress
///      (`Reading N files…`), past-tense summaries (`Read 2 files, listed 1
///      directory`), effort/model indicators (`◐ medium · /effort`),
///      collapsible headers (`(ctrl+o to expand)`), parenthesized tool
///      headers (`Fetch(url)`, `Bash(cmd)`, `Task(prompt)`, …), or welcome-
///      box residue (block-element glyphs). These patterns can reappear
///      mid-response when ink re-renders, so a full-pass filter is needed.
pub fn strip_prompt_echo(response: String, prompt: &str) -> String {
    let prompt_normalized: String = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    let initial_lines: Vec<&str> = response.lines().collect();
    let mut cut = 0;
    for (idx, line) in initial_lines.iter().enumerate() {
        let normalized: String = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if normalized.is_empty() {
            cut = idx + 1;
            continue;
        }
        if !prompt_normalized.is_empty()
            && normalized.len() >= 8
            && prompt_normalized.contains(&normalized)
        {
            cut = idx + 1;
        } else {
            break;
        }
    }
    let mut kept: Vec<&str> = initial_lines[cut..]
        .iter()
        .copied()
        .filter(|line| {
            let normalized: String = line.split_whitespace().collect::<Vec<_>>().join(" ");
            if normalized.is_empty() {
                return true;
            }
            !is_tool_chrome(&normalized)
        })
        .collect();

    // ── Structural bookend detection ────────────────────────────────────────
    // If a short line appears as BOTH the first and last non-empty line of
    // the response AND the response has real content between them, it's
    // almost certainly a Claude Code UI-label fragment (a footer/header hint
    // that ink rendered twice into the content region). Examples caught:
    // "later", "point in time", "or ~/.claude/skills/ for skills that work...".
    // Bookending is rare in legitimate prose; the cost of false-drop is low.
    while kept.len() >= 4 {
        let first_idx = kept.iter().position(|l| !l.trim().is_empty());
        let last_idx = kept.iter().rposition(|l| !l.trim().is_empty());
        match (first_idx, last_idx) {
            (Some(f), Some(l)) if f < l => {
                let first = kept[f].trim();
                let last = kept[l].trim();
                if first == last
                    && first.len() <= 100
                    // must be genuinely short — longer bookending is plausibly prose
                    && first.split_whitespace().count() <= 12
                {
                    kept.remove(l);
                    kept.remove(f);
                    continue;
                }
            }
            _ => {}
        }
        break;
    }

    kept.join("\n")
}

/// Return `true` if the line looks like Claude Code tool-use chrome that the
/// VTE filter didn't catch (typically because it's context-dependent).
pub fn is_tool_chrome(line: &str) -> bool {
    if line.starts_with("[context] ") {
        return true;
    }
    if is_claude_terminal_status(line) {
        return true;
    }
    if line.ends_with("(ctrl+o to expand)") {
        return true;
    }
    const PAREN_TOOL_HEADS: &[&str] = &[
        "Read(",
        "Write(",
        "Edit(",
        "Bash(",
        "Grep(",
        "Glob(",
        "Fetch(",
        "WebFetch(",
        "WebSearch(",
        "Task(",
        "Update(",
        "List(",
        "NotebookEdit(",
        "NotebookRead(",
        "TodoWrite(",
        "Search(",
        "Explore(",
        "Plan(",
        "Agent(",
    ];
    if PAREN_TOOL_HEADS.iter().any(|h| line.starts_with(h)) {
        return true;
    }
    let first_word = line.split_whitespace().next().unwrap_or("");
    let gerunds = [
        "Reading",
        "Writing",
        "Searching",
        "Listing",
        "Running",
        "Executing",
        "Editing",
        "Fetching",
        "Grepping",
        "Globbing",
    ];
    if gerunds.contains(&first_word) && line.contains('…') {
        return true;
    }
    let past_tense_tools = [
        "Read ",
        "Wrote ",
        "Edited ",
        "Searched ",
        "Listed ",
        "Ran ",
        "Fetched ",
        "Grepped ",
    ];
    if past_tense_tools.iter().any(|p| line.starts_with(p))
        && line.chars().any(|c| c.is_ascii_digit())
    {
        return true;
    }
    if let Some(first) = line.chars().next()
        && matches!(first, '◐' | '◯' | '●' | '◉')
        && line.contains('/')
    {
        return true;
    }
    if line.contains("· /effort") || line.contains("· /model") || line.ends_with("/effort") {
        return true;
    }
    if let Some(c) = line.trim_start().chars().next()
        && matches!(c, '\u{2580}'..='\u{259F}')
    {
        return true;
    }
    let non_ws_chars: Vec<char> = line.chars().filter(|c| !c.is_whitespace()).collect();
    if !non_ws_chars.is_empty() {
        let box_count = non_ws_chars
            .iter()
            .filter(|c| {
                matches!(**c,
                    '\u{2580}'..='\u{259F}'
                    | '\u{2500}'..='\u{257F}'
                )
            })
            .count();
        if box_count * 2 >= non_ws_chars.len() {
            return true;
        }
    }
    false
}

fn is_claude_terminal_status(line: &str) -> bool {
    let trimmed = line.trim();
    let Some(first_char) = trimmed.chars().next() else {
        return false;
    };
    if !matches!(first_char, '·' | '✢' | '✶' | '✻' | '✽' | '⏺') {
        return false;
    }
    let rest = trimmed[first_char.len_utf8()..].trim().to_lowercase();
    let is_terminal_status = rest.starts_with("brewed for ") || rest.starts_with("cooked for ");
    is_terminal_status && rest.ends_with('s') && rest.split_whitespace().count() == 3
}

#[cfg(test)]
mod tests {
    use super::strip_prompt_echo;

    #[test]
    fn strips_wrapped_prompt_echo() {
        let prompt = "You are running inside pseudomux as a sub-agent. Please review this \
                      codebase and report what you find in under 300 words.";
        let response = [
            "You are running inside pseudomux as a sub-agent. Please review this",
            "codebase and report what you find in under 300 words.",
            "Pseudomux Review",
            "Pseudomux is an SDK-grade PTY multiplexer.",
        ]
        .join("\n");
        assert_eq!(
            strip_prompt_echo(response, prompt),
            "Pseudomux Review\nPseudomux is an SDK-grade PTY multiplexer."
        );
    }

    #[test]
    fn preserves_response_when_no_echo() {
        let prompt = "What is 2+2?";
        let response = "4. It's a simple addition.".to_string();
        assert_eq!(strip_prompt_echo(response.clone(), prompt), response);
    }

    #[test]
    fn strips_parenthesized_tool_headers() {
        let prompt = "Fetch a URL and summarize.";
        let response = [
            "Fetch(https://example.com)",
            "The page covers topic X.",
            "Bash(ls)",
            "Summary complete.",
        ]
        .join("\n");
        assert_eq!(
            strip_prompt_echo(response, prompt),
            "The page covers topic X.\nSummary complete."
        );
    }

    #[test]
    fn strips_claude_terminal_status_lines() {
        let prompt = "Summarize.";
        let response = [
            "Summary complete.",
            "✻ Brewed for 33s",
            "Another line.",
            "✻ Cooked for 39s",
        ]
        .join("\n");
        assert_eq!(
            strip_prompt_echo(response, prompt),
            "Summary complete.\nAnother line."
        );
    }

    #[test]
    fn strips_bookending_short_ui_fragments() {
        // A short label appearing as both first AND last non-empty line of
        // the response is almost certainly a Claude Code UI-hint fragment
        // ink rendered twice into the content region. Catches unknown labels
        // without requiring a pattern match.
        let prompt = "Summarize.";
        let response = [
            "later",
            "This is the real summary prose.",
            "It spans multiple lines.",
            "later",
        ]
        .join("\n");
        assert_eq!(
            strip_prompt_echo(response, prompt),
            "This is the real summary prose.\nIt spans multiple lines."
        );
    }

    #[test]
    fn preserves_bookending_prose() {
        // If the bookending text is longer (> 12 words), it's more likely
        // legitimate prose using repetition as a stylistic device. Preserve.
        let prompt = "Write a paragraph.";
        let first_and_last =
            "The city at dawn moves with deliberate grace across the waking streets.";
        let response = [first_and_last, "Middle sentence.", first_and_last].join("\n");
        let out = strip_prompt_echo(response.clone(), prompt);
        // Not bookend-stripped because first_and_last is > 12 words.
        assert!(out.contains(first_and_last));
    }
}
