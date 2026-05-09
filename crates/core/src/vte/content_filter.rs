//! Filters TUI chrome from content buffer text entries.
//!
//! Handles chrome from multiple TUI agents (Claude Code, OpenCode, etc.):
//! box-drawing borders, status bars, spinner lines, sidebar leakage, and
//! progressive-rendering duplicates. The main entry point is
//! [`ContentFilter::filter_line`] via the [`ContentFilterTrait`] adapter.

use crate::adapter::ContentFilterTrait;

/// Filters TUI chrome from content buffer text entries.
#[derive(Clone, Debug, Default)]
pub struct ContentFilter {}

impl ContentFilterTrait for ContentFilter {
    fn filter_line(&self, line: &str) -> Option<String> {
        ContentFilter::filter_line(self, line)
    }
}

/// Check if a character is a box-drawing or border character.
pub fn is_box_drawing(c: char) -> bool {
    matches!(
        c,
        '▀' | '▁'
            | '▂'
            | '▃'
            | '▄'
            | '▅'
            | '▆'
            | '▇'
            | '█'
            | '═'
            | '─'
            | '━'
            | '╸'
            | '╺'
            | '╹'
            | '╻'
            | '┃'
            | '┗'
            | '┣'
            | '┛'
            | '┏'
            | '┓'
            | '┫'
            | '┻'
            | '┳'
            | '╋'
            | '│'
            | '┤'
            | '├'
            | '┐'
            | '┘'
            | '┌'
            | '└'
            | '┬'
            | '┴'
            | '┼'
            | '╔'
            | '╗'
            | '╚'
            | '╝'
            | '╠'
            | '╣'
            | '╦'
            | '╩'
            | '╬'
            | '▐'
            | '▌'
            | '▊'
            | '▋'
            | '▍'
            | '▎'
            | '▏'
            | '▕'
            | '▪'
            | '╭'
            | '╮'
            | '╰'
            | '╯'
    )
}

/// Check if a character is a left-side chrome prefix char (stripped from content).
fn is_left_chrome(c: char) -> bool {
    matches!(c, '┃' | '╹' | '╺' | '┗' | '┣' | '│' | '├')
}

/// Check if a line is entirely border characters and whitespace.
pub fn is_border_line(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_whitespace() || is_box_drawing(c))
}

/// Check if a line is an `OpenCode` or Claude Code footer/chrome.
pub fn is_footer(s: &str) -> bool {
    let lower = s.to_lowercase();

    // Claude Code prompt indicator
    if lower.trim() == ">" {
        return true;
    }

    (lower.contains("ctrl+t") && lower.contains("variants"))
        || (lower.contains("ctrl+p") && lower.contains("commands"))
        || (lower.contains("ctrl+t") && lower.contains("tab"))
}

/// Check if a line is Claude Code UI chrome that should be dropped.
pub fn is_claude_code_chrome(s: &str) -> bool {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return false;
    }
    let lower = trimmed.to_lowercase();

    // Status bar lines
    if lower == "esc to interrupt" || lower == "esctointerrupt" {
        return true;
    }
    // Bypass permissions status line
    if lower.contains("bypass permissions") || lower.contains("bypasspermissions") {
        return true;
    }
    // Play/transport control chars used in status bar (⏵ U+23F5)
    if trimmed.starts_with('⏵') {
        return true;
    }
    // "? for shortcuts" and variants (may have appended notice)
    if lower.starts_with("? for shortcuts") || lower.starts_with("?forshortcuts") {
        return true;
    }

    // Thinking spinner lines contain "(thinking)"
    if lower.contains("(thinking)") {
        return true;
    }

    // Claude Code thinking/progress spinner frames. Shape:
    //   <spinner_char> <Verb>…
    //   <spinner_char> <Verb>… (thinking)
    //   <spinner_char> <Verb>… (23s · ↓ 529 tokens)
    //   <spinner_char> <Verb>… (running stop hook · 23s · ↓ 529 tokens)
    // Spinner glyphs: · ✢ * ✶ ✻ ✽ ⏺. We match on first-char-is-glyph AND
    // the line contains an ellipsis, which is permissive enough to catch
    // post-turn "stop hook" frames while rare enough that legitimate list
    // bullets (" · foo", " * bar") don't accidentally carry an ellipsis.
    {
        let first_char = trimmed.chars().next().unwrap_or(' ');
        if matches!(first_char, '·' | '✢' | '*' | '✶' | '✻' | '✽' | '⏺') && trimmed.contains('…')
        {
            return true;
        }
        if matches!(first_char, '·' | '✢' | '✶' | '✻' | '✽' | '⏺') {
            let rest = trimmed[first_char.len_utf8()..].trim().to_lowercase();
            let is_terminal_status =
                rest.starts_with("brewed for ") || rest.starts_with("cooked for ");
            if is_terminal_status && rest.ends_with('s') && rest.split_whitespace().count() == 3 {
                return true;
            }
        }
    }

    // "Pasting text…" echo from bracketed paste
    if lower == "pasting text…" || lower == "pastingtext…" || lower.starts_with("pasting text")
    {
        return true;
    }

    // Claude Code welcome box: line starts with ╭ or ╰ and ends with ╮ or ╯
    // e.g. "╭─── Claude Code v2.1.70 ───╮"
    if (trimmed.starts_with('╭') && trimmed.ends_with('╮'))
        || (trimmed.starts_with('╰') && trimmed.ends_with('╯'))
    {
        return true;
    }

    // Tip lines: "\u{23BF} Tip: ..."
    if trimmed.starts_with('\u{23BF}') && lower.contains("tip:") {
        return true;
    }

    // "\u{276F} Tool loaded." variants
    if trimmed.starts_with('\u{276F}') {
        let after = trimmed.trim_start_matches('\u{276F}').trim();
        let after_nospace = after.replace(' ', "");
        if after.is_empty() || after == "Tool loaded." || after_nospace == "Toolloaded." {
            return true;
        }
    }

    // ── "(ctrl+o to expand)" / "(ctrl+b to run in background)" are blanket
    // chrome signals, regardless of where they appear in the line. Any line
    // carrying these is Claude Code's expandable tool-output box, not prose.
    if lower.contains("(ctrl+o to expand)") || lower.contains("(ctrl+b to run in background)") {
        return true;
    }

    // ── Task tool tree-glyph progress indicators ────────────────────────────
    // When the agent spawns a sub-agent via the Task tool, Claude Code renders
    // a tree-structured progress view:
    //   ─ Ocean poem · 0 tool uses
    //   └─ Silent forest poem · 0 tool uses
    //   ├─ Distant star poem · 1 tool use
    //   │  ⎿  Initializing…                (tree-glyph + embedded ⎿)
    //   │  ⎿  Done                         (tree-glyph + embedded ⎿)
    // These start with a tree-drawing box glyph and include a chrome signal:
    // dot-separator + tool-use counter, embedded ⎿ continuation, or …/ctrl+.
    {
        let first_char = trimmed.chars().next().unwrap_or(' ');
        if matches!(
            first_char,
            '─' | '└' | '├' | '│' | '┌' | '┐' | '┘' | '┤' | '┬' | '┴' | '┼'
        ) && (trimmed.contains(" · ")
            || trimmed.contains("tool use")
            || trimmed.contains('\u{23BF}')
            || trimmed.contains('…'))
        {
            return true;
        }
    }

    // ── Task batch summary ──────────────────────────────────────────────────
    // "3 agents finished", "2 agents finished (5 tool uses)". Also caught
    // when bullet-prefixed (see ● check below).
    if is_agents_finished(trimmed) {
        return true;
    }

    // ── Bullet-prefixed Claude Code chrome ──────────────────────────────────
    // The `●` marker is used for both prose ("● Pseudomux is …") and chrome
    // ("● 2 agents finished", "● Running 2 agents…"). Recognize the chrome
    // forms here so filter_line's generic bullet stripping doesn't let them
    // through.
    if trimmed.starts_with('\u{25CF}') {
        let stripped = trimmed.trim_start_matches('\u{25CF}').trim();
        let stripped_lower = stripped.to_lowercase();
        if is_agents_finished(stripped) {
            return true;
        }
        // "Running N agents…" / "Running N agent…" progress lines.
        if stripped_lower.starts_with("running ")
            && (stripped_lower.contains(" agents…")
                || stripped_lower.contains(" agent…")
                || stripped_lower.contains(" agents...")
                || stripped_lower.contains(" agent..."))
        {
            return true;
        }
    }

    // ── Keybinding hint lines (standalone) ──────────────────────────────────
    // "(ctrl+b to run in background)", "Shift + Enter for new line and more".
    // Most are caught by the blanket contains-check above; this catches the
    // long-form variants that don't include ctrl+o/ctrl+b.
    if lower.starts_with("(ctrl+") && lower.contains(" to ") && trimmed.ends_with(')') {
        return true;
    }
    if lower.starts_with("shift + enter") || lower.starts_with("shift+enter") {
        return true;
    }

    // ── Effort / model status indicators without the leading bullet glyph ──
    // The existing rule handles "◐ medium · /effort"; this catches the
    // glyphless variant "high · /effort" or just "/effort"-terminated strings.
    if trimmed.ends_with("/effort")
        || trimmed.ends_with("/model")
        || (lower.contains(" · /") && (lower.contains("/effort") || lower.contains("/model")))
    {
        return true;
    }

    // ── Bare "Running…" tool-in-progress line (Bash or similar) ─────────────
    // Claude Code renders just "Running…" on its own line while a shell
    // command is active. No glyph, no other context.
    if lower == "running…" || lower == "running..." {
        return true;
    }

    // ── Short-response UI-hint fragments ────────────────────────────────────
    // Claude Code renders footer labels like "point in time" or "later" into
    // the content region when the response is short and spare rows remain.
    // We match these as EXACT short lines only (not substring), so legitimate
    // prose that happens to mention the phrase ("see you later") isn't
    // stripped. Any fragment seen in real Claude Code output goes in the list.
    {
        const UI_HINT_EXACT_LINES: &[&str] = &[
            "later",
            "point in time",
            "beneath the input box",
            "shift+tab to cycle",
            "ctrl+c to exit",
            "ctrl+d to exit",
            "for new line and more",
            "mode)",
            "(plan mode)",
            "or ~/.claude/skills/ for skills that work in any project",
        ];
        if trimmed.len() < 80 && UI_HINT_EXACT_LINES.contains(&lower.as_str()) {
            return true;
        }
    }

    false
}

/// Match "N agents finished" with an optional "(M tool uses)" suffix.
/// Used by `is_claude_code_chrome` to drop Task batch-completion summaries.
fn is_agents_finished(line: &str) -> bool {
    // Extract leading digits
    let mut chars = line.chars();
    let first = chars.next().unwrap_or(' ');
    if !first.is_ascii_digit() {
        return false;
    }
    let mut digit_end = 1;
    for c in chars {
        if c.is_ascii_digit() {
            digit_end += 1;
        } else {
            break;
        }
    }
    let rest = &line[digit_end..];
    let rest_lower = rest.to_lowercase();
    let rest_trim = rest_lower.trim();
    rest_trim == "agent finished"
        || rest_trim == "agents finished"
        || rest_trim.starts_with("agent finished (")
        || rest_trim.starts_with("agents finished (")
}

impl ContentFilter {
    /// Create a new `ContentFilter` with default noise patterns.
    pub fn new() -> Self {
        Self::default()
    }

    /// Filter a single raw content buffer line.
    /// Returns `Some(clean_text)` or `None` (drop the line).
    pub fn filter_line(&self, line: &str) -> Option<String> {
        // Step 0: Drop footer lines early
        if is_footer(line) {
            return None;
        }

        // Drop Claude Code chrome
        if is_claude_code_chrome(line) {
            return None;
        }

        // Claude Code bullet marker: "\u{25CF} text"
        let trimmed_input = line.trim();
        if trimmed_input.starts_with('\u{25CF}') {
            let content_str = trimmed_input.trim_start_matches('\u{25CF}').trim();
            // Strip "(ctrl+o to expand)" suffix
            let content_str = if let Some(pos) = content_str.rfind("(ctrl+o to expand)") {
                content_str[..pos].trim()
            } else {
                content_str
            };
            // Strip "\u{2026} (ctrl+o to expand)" suffix
            let content_str = if let Some(pos) = content_str.rfind("\u{2026} (ctrl+o to expand)") {
                content_str[..pos].trim()
            } else {
                content_str
            };
            if content_str.is_empty() {
                return None;
            }
            return Some(content_str.to_string());
        }

        // Claude Code user turn: "\u{276F} text" (non-empty, non-chrome already handled above)
        if trimmed_input.starts_with('\u{276F}') {
            let content_str = trimmed_input.trim_start_matches('\u{276F}').trim();
            if content_str.is_empty() {
                return None;
            }
            return Some(format!("[user] {content_str}"));
        }

        // Claude Code continuation: "\u{23BF} text" (non-tip lines already dropped above)
        if trimmed_input.starts_with('\u{23BF}') {
            let content_str = trimmed_input.trim_start_matches('\u{23BF}').trim();
            if content_str.is_empty() {
                return None;
            }
            return Some(format!("[context] {content_str}"));
        }

        // Step 1: Strip sidebar — find LAST occurrence of █
        let content = if let Some(pos) = line.rfind('█') {
            &line[..pos]
        } else {
            line
        };

        // Step 2: Strip left chrome
        let trimmed_left = content.trim_start();
        let stripped = strip_left_chrome(trimmed_left);

        // Step 3: Trim both ends
        let result = stripped.trim();

        // Step 4: Drop empty lines
        if result.is_empty() {
            return None;
        }

        // Step 5: Drop border lines
        if is_border_line(result) {
            return None;
        }

        // Special handling: Thinking lines
        if trimmed_left.starts_with("┃") {
            let after_pipe = trimmed_left.trim_start_matches('┃').trim();
            if let Some(text) = after_pipe.strip_prefix("Thinking:") {
                return Some(format!("[thinking] {}", text.trim()));
            }
            // Drop Build status lines
            if after_pipe.starts_with("Build") {
                return None;
            }
        }

        // Special handling: Build completion
        if result.starts_with("▣") {
            let after = result.trim_start_matches('▣').trim();
            if let Some(rest) = after.strip_prefix("Build ·") {
                return Some(format!("[completed] {}", rest.trim()));
            }
            if let Some(rest) = after.strip_prefix("Build·") {
                return Some(format!("[completed] {}", rest.trim()));
            }
        }

        // Special handling: Tool activity indicators
        if result.starts_with("→ ") || result.starts_with("→") {
            let action = result.trim_start_matches('→').trim();
            if !action.is_empty() {
                return Some(format!("[tool] {action}"));
            }
        }
        if result.starts_with("✱ ") || result.starts_with("✱") {
            let action = result.trim_start_matches('✱').trim();
            if !action.is_empty() {
                return Some(format!("[tool] {action}"));
            }
        }
        if result.starts_with("~ ") {
            return Some(format!("[tool] {result}"));
        }
        if result.starts_with("⠋ ")
            || result.starts_with("⠙ ")
            || result.starts_with("⠹ ")
            || result.starts_with("⠸ ")
            || result.starts_with("⠼ ")
            || result.starts_with("⠴ ")
            || result.starts_with("⠦ ")
            || result.starts_with("⠧ ")
            || result.starts_with("⠇ ")
            || result.starts_with("⠏ ")
        {
            let action = result.chars().skip(1).collect::<String>();
            let action = action.trim();
            if !action.is_empty() {
                return Some(format!("[tool] {action}"));
            }
        }

        Some(result.to_string())
    }
}

/// Strip leading box-drawing chrome characters, preserving tool indicators.
pub fn strip_left_chrome(s: &str) -> &str {
    let mut chars = s.char_indices();
    let mut last_chrome_end = 0;
    loop {
        match chars.next() {
            Some((i, c)) if is_left_chrome(c) || c.is_whitespace() => {
                last_chrome_end = i + c.len_utf8();
            }
            _ => break,
        }
    }
    &s[last_chrome_end..]
}

/// Check if a line looks like sidebar leakage.
pub fn is_sidebar_leakage(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    // "New session - 2026-03-05..."
    if trimmed.starts_with("New session - ") {
        return true;
    }
    // "Ask anything..."
    if trimmed.starts_with("Ask anything") {
        return true;
    }
    // "Context", "LSP", "LSPs will activate..."
    if trimmed == "Context" || trimmed == "LSP" || trimmed.starts_with("LSPs will activate") {
        return true;
    }
    // "123 tokens", "45% used", "$0.00 spent"
    if trimmed.ends_with(" tokens") {
        let prefix = trimmed.trim_end_matches(" tokens");
        if prefix.chars().all(|c| c.is_ascii_digit() || c == ',') {
            return true;
        }
    }
    if trimmed.ends_with("% used") {
        let prefix = trimmed.trim_end_matches("% used");
        if prefix.chars().all(|c| c.is_ascii_digit()) {
            return true;
        }
    }
    if trimmed.starts_with('$') && trimmed.ends_with(" spent") {
        let mid = &trimmed[1..trimmed.len() - 6];
        if mid.chars().all(|c| c.is_ascii_digit() || c == '.') {
            return true;
        }
    }
    // Isolated timestamp fragments like "618Z", "123Z"
    if trimmed.len() <= 6 && trimmed.ends_with('Z') {
        let prefix = &trimmed[..trimmed.len() - 1];
        if prefix.chars().all(char::is_alphanumeric) && prefix.len() >= 2 {
            return true;
        }
    }
    false
}

/// Check if a line is a tagged line ([completed], [thinking], [tool]).
pub fn is_tagged_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with("[completed]")
        || trimmed.starts_with("[thinking]")
        || trimmed.starts_with("[tool]")
}

/// Post-process filtered lines to remove progressive rendering duplicates.
///
/// Rules:
/// 1. If line N+1 starts with line N's text, line N is a progressive fragment — drop it
/// 2. Consecutive identical lines are collapsed to one
/// 3. Consecutive identical tagged lines are collapsed to one
/// 4. Lines that look like sidebar leakage are dropped
pub fn deduplicate_lines(lines: Vec<String>) -> Vec<String> {
    // First: remove sidebar leakage
    let lines: Vec<String> = lines
        .into_iter()
        .filter(|l| !is_sidebar_leakage(l))
        .collect();

    if lines.is_empty() {
        return lines;
    }

    // Collapse duplicate [user] lines (same content after removing spaces, keep last)
    // VTE progressive rendering may produce versions with/without spaces
    let lines = {
        let mut seen_user: Vec<(usize, String)> = Vec::new();
        let mut to_remove: Vec<usize> = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("[user] ") {
                let normalized = trimmed.replace(' ', "");
                if let Some(slot) = seen_user.iter_mut().find(|(_, n)| n == &normalized) {
                    to_remove.push(slot.0);
                    slot.0 = i;
                } else {
                    seen_user.push((i, normalized));
                }
            }
        }
        lines
            .into_iter()
            .enumerate()
            .filter(|(i, _)| !to_remove.contains(i))
            .map(|(_, l)| l)
            .collect::<Vec<_>>()
    };

    // Phase 1: Remove interspersed [completed] between content/thinking fragments.
    // Pattern: content_A, [completed]*, content_B where B starts_with A -> drop A + [completed]s
    let mut phase1: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let current_trimmed = lines[i].trim();

        if current_trimmed.is_empty() {
            i += 1;
            continue;
        }

        // For content lines: look past [completed] for progressive successor
        if !is_tagged_line(&lines[i]) {
            let mut j = i + 1;
            while j < lines.len() && lines[j].trim().starts_with("[completed]") {
                j += 1;
            }
            if j < lines.len() && j > i + 1 && !is_tagged_line(&lines[j]) {
                let next_trimmed = lines[j].trim();
                if next_trimmed.starts_with(current_trimmed) && next_trimmed != current_trimmed {
                    // Skip current + all intervening [completed] lines
                    i = j;
                    continue;
                }
            }
        }

        // For [thinking] lines: look past [completed] for progressive thinking successor
        if current_trimmed.starts_with("[thinking]") {
            let ct = current_trimmed
                .strip_prefix("[thinking]")
                .unwrap_or("")
                .trim();
            let mut best_j = i;
            let mut j = i + 1;
            let mut best_text = ct;
            while j < lines.len() {
                let jt = lines[j].trim();
                if jt.starts_with("[completed]") || jt.is_empty() {
                    j += 1;
                    continue;
                }
                if jt.starts_with("[thinking]") {
                    let nt = jt.strip_prefix("[thinking]").unwrap_or("").trim();
                    if nt.starts_with(best_text) && nt != best_text {
                        best_j = j;
                        best_text = nt;
                        j += 1;
                        continue; // Keep looking for even longer version
                    }
                }
                break;
            }
            if best_j > i {
                i = best_j; // Skip to the longest thinking fragment
            }
        }

        phase1.push(lines[i].clone());
        i += 1;
    }

    // Phase 2: Collapse consecutive identical + simple progressive fragments
    let mut result: Vec<String> = Vec::with_capacity(phase1.len());
    for i in 0..phase1.len() {
        let current_trimmed = phase1[i].trim();

        if current_trimmed.is_empty() {
            continue;
        }

        // Consecutive identical
        if let Some(prev) = result.last()
            && prev.trim() == current_trimmed
        {
            continue;
        }

        // Simple progressive fragment (adjacent, no gap)
        if !is_tagged_line(&phase1[i])
            && let Some(next) = phase1.get(i + 1)
        {
            let next_trimmed = next.trim();
            if !is_tagged_line(next)
                && next_trimmed.starts_with(current_trimmed)
                && next_trimmed != current_trimmed
            {
                continue;
            }
        }

        // Progressive thinking (adjacent)
        if current_trimmed.starts_with("[thinking]")
            && let Some(next) = phase1.get(i + 1)
            && next.trim().starts_with("[thinking]")
        {
            let ct = current_trimmed
                .strip_prefix("[thinking]")
                .unwrap_or("")
                .trim();
            let nt = next.trim().strip_prefix("[thinking]").unwrap_or("").trim();
            if nt.starts_with(ct) && nt != ct {
                continue;
            }
        }

        result.push(phase1[i].clone());
    }

    // Phase 3: Non-adjacent prefix dedup for streaming TUI re-renders where
    // ink/React re-renders multiple rows in one batch, interleaving progressive
    // fragments of different rows so Phase 2's adjacent check can't see them.
    // Drop any non-tagged non-empty line that is a *strict* prefix (longer, not
    // equal) of some later non-tagged non-empty line. Equal-but-non-adjacent
    // duplicates are preserved — those aren't progressive fragments.
    let phase3: Vec<String> = (0..result.len())
        .filter(|&i| {
            let current_trimmed = result[i].trim();
            if current_trimmed.is_empty() || is_tagged_line(&result[i]) {
                return true;
            }
            let superseded = result.iter().skip(i + 1).any(|later| {
                if is_tagged_line(later) {
                    return false;
                }
                let lt = later.trim();
                lt.len() > current_trimmed.len() && lt.starts_with(current_trimmed)
            });
            !superseded
        })
        .map(|i| result[i].clone())
        .collect();

    phase3
}

/// Extract only the assistant response text from a list of filtered lines.
///
/// 1. Runs `deduplicate_lines()`
/// 2. Finds the LAST `[user]` line
/// 3. Returns only lines AFTER that last `[user]` line
/// 4. Strips trailing empty lines
pub fn extract_response_text(lines: Vec<String>) -> Vec<String> {
    let deduped = deduplicate_lines(lines);
    // Find last [user] line index
    let last_user = deduped
        .iter()
        .rposition(|l| l.trim().starts_with("[user] "));
    let after: Vec<String> = match last_user {
        Some(idx) => deduped.into_iter().skip(idx + 1).collect(),
        None => deduped,
    };
    // Strip trailing empty lines
    let mut result = after;
    while result
        .last()
        .map(|l: &String| l.trim().is_empty())
        .unwrap_or(false)
    {
        result.pop();
    }
    result
}
