/// Defines the screen layout for a specific TUI profile.
/// Row indices are 0-based, matching vt100's coordinate system.
#[derive(Clone, Debug)]
pub struct ScreenRegions {
    pub content_start: u16,
    pub content_end: u16,
    pub status_start: u16,
    pub status_end: u16,
    pub rows: u16,
    pub cols: u16,
}

impl ScreenRegions {
    /// `OpenCode` layout: content is 0..rows-2, status is the last 2 rows
    /// (footer line + shortcuts/spinner bar).
    pub fn opencode(rows: u16, cols: u16) -> Self {
        let status_rows = 2u16.min(rows);
        Self {
            content_start: 0,
            content_end: rows.saturating_sub(status_rows),
            status_start: rows.saturating_sub(status_rows),
            status_end: rows,
            rows,
            cols,
        }
    }

    /// Claude Code layout: content is 0..rows-4, bottom 4 rows are chrome:
    ///   row rows-4: separator with status dots
    ///   row rows-3: input prompt
    ///   row rows-2: separator line
    ///   row rows-1: status bar (? for shortcuts / esc to interrupt)
    pub fn claude_code(rows: u16, cols: u16) -> Self {
        let status_rows = 4u16.min(rows);
        Self {
            content_start: 0,
            content_end: rows.saturating_sub(status_rows),
            status_start: rows.saturating_sub(status_rows),
            status_end: rows,
            rows,
            cols,
        }
    }

    /// Full screen layout: all rows are content, no status bar.
    pub fn full_screen(rows: u16, cols: u16) -> Self {
        Self {
            content_start: 0,
            content_end: rows,
            status_start: rows,
            status_end: rows,
            rows,
            cols,
        }
    }

    /// Resize the layout to `new_rows × new_cols`, preserving the same status-row count.
    pub fn resize(&mut self, new_rows: u16, new_cols: u16) {
        let status_row_count = self.status_end.saturating_sub(self.status_start);
        self.rows = new_rows;
        self.cols = new_cols;
        if status_row_count > 0 {
            let status_rows = status_row_count.min(new_rows);
            self.content_start = 0;
            self.content_end = new_rows.saturating_sub(status_rows);
            self.status_start = new_rows.saturating_sub(status_rows);
            self.status_end = new_rows;
        } else {
            self.content_start = 0;
            self.content_end = new_rows;
            self.status_start = new_rows;
            self.status_end = new_rows;
        }
    }

    /// Returns `true` if `row` falls within the content region.
    pub fn is_content(&self, row: u16) -> bool {
        row >= self.content_start && row < self.content_end
    }

    /// Returns `true` if `row` falls within the status-bar region.
    pub fn is_status(&self, row: u16) -> bool {
        row >= self.status_start && row < self.status_end
    }
}
