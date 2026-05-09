use pseudomux_core::adapter::{ContentFilterTrait, TuiAdapter};
use pseudomux_core::vte::{ContentFilter, ScreenRegions, StatusPatterns};

/// Claude Code TUI adapter.
pub struct ClaudeCodeAdapter;

/// Content filter that wraps the default filter plus Claude Code chrome detection.
struct ClaudeCodeContentFilter {
    inner: ContentFilter,
}

impl ContentFilterTrait for ClaudeCodeContentFilter {
    fn filter_line(&self, line: &str) -> Option<String> {
        // The default ContentFilter already handles Claude Code chrome,
        // so just delegate to it.
        self.inner.filter_line(line)
    }
}

impl TuiAdapter for ClaudeCodeAdapter {
    fn name(&self) -> &str {
        "claude-code"
    }

    fn screen_regions(&self, rows: u16, cols: u16) -> ScreenRegions {
        ScreenRegions::claude_code(rows, cols)
    }

    fn status_patterns(&self) -> StatusPatterns {
        StatusPatterns::claude_code()
    }

    fn content_filter(&self) -> Box<dyn ContentFilterTrait> {
        Box::new(ClaudeCodeContentFilter {
            inner: ContentFilter::new(),
        })
    }

    fn is_confirmation(&self, text: &str) -> bool {
        let lower = text.to_lowercase();
        lower.starts_with("allow")
            && (lower.contains("to run")
                || lower.contains("to read")
                || lower.contains("to write")
                || lower.contains("to edit")
                || lower.contains("to execute"))
    }

    fn program(&self) -> &str {
        "claude"
    }

    fn env_remove(&self) -> Vec<String> {
        vec![
            "CLAUDECODE".to_string(),
            "CLAUDE_CODE_ENTRYPOINT".to_string(),
        ]
    }

    fn post_paste_delay_ms(&self) -> u64 {
        500
    }
}
