use pseudomux_core::adapter::{ContentFilterTrait, TuiAdapter};
use pseudomux_core::vte::{ContentFilter, ScreenRegions, StatusPatterns};

/// Shell (bash/zsh/fish) adapter.
pub struct ShellAdapter;

impl TuiAdapter for ShellAdapter {
    fn name(&self) -> &str {
        "shell"
    }

    fn screen_regions(&self, rows: u16, cols: u16) -> ScreenRegions {
        ScreenRegions::full_screen(rows, cols)
    }

    fn status_patterns(&self) -> StatusPatterns {
        StatusPatterns::default()
    }

    fn content_filter(&self) -> Box<dyn ContentFilterTrait> {
        Box::new(ContentFilter::new())
    }

    fn program(&self) -> &str {
        "bash"
    }
}
