use pseudomux_core::adapter::{ContentFilterTrait, TuiAdapter};
use pseudomux_core::vte::{ContentFilter, ScreenRegions, StatusPatterns};

/// OpenCode TUI adapter.
pub struct OpenCodeAdapter;

impl TuiAdapter for OpenCodeAdapter {
    fn name(&self) -> &str {
        "opencode"
    }

    fn screen_regions(&self, rows: u16, cols: u16) -> ScreenRegions {
        ScreenRegions::opencode(rows, cols)
    }

    fn status_patterns(&self) -> StatusPatterns {
        StatusPatterns::opencode_v1_2()
    }

    fn content_filter(&self) -> Box<dyn ContentFilterTrait> {
        Box::new(ContentFilter::new())
    }

    fn program(&self) -> &str {
        "opencode"
    }
}
