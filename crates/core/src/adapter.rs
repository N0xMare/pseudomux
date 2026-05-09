//! Adapter traits for TUI-specific behaviour.
//!
//! [`TuiAdapter`] is the primary extension point: implement it to teach
//! pseudomux how to talk to a new TUI agent without touching core internals.
//! [`ContentFilterTrait`] is a narrower trait used when only line-level
//! filtering is needed.

use crate::vte::{ScreenRegions, StatusPatterns};

/// Trait for filtering content lines from TUI output.
pub trait ContentFilterTrait: Send + Sync {
    /// Filter a single raw content buffer line.
    /// Returns `Some(clean_text)` or `None` (drop the line).
    fn filter_line(&self, line: &str) -> Option<String>;
}

/// Trait for TUI-specific behavior. Each supported TUI implements this.
pub trait TuiAdapter: Send + Sync {
    /// Unique name identifier (e.g., "claude-code", "opencode", "shell")
    fn name(&self) -> &str;

    /// Screen region layout for this TUI at given terminal size
    fn screen_regions(&self, rows: u16, cols: u16) -> ScreenRegions;

    /// Status patterns for VTE state detection
    fn status_patterns(&self) -> StatusPatterns;

    /// Content filter for stripping TUI chrome
    fn content_filter(&self) -> Box<dyn ContentFilterTrait>;

    /// Check if a content line is a confirmation prompt
    fn is_confirmation(&self, _text: &str) -> bool {
        false
    }

    /// Program binary name
    fn program(&self) -> &str;

    /// Env vars to remove from PTY
    fn env_remove(&self) -> Vec<String> {
        vec![]
    }

    /// Post-paste delay in ms
    fn post_paste_delay_ms(&self) -> u64 {
        0
    }
}
