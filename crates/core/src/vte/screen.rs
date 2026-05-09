use super::differ::ScreenChange;
use super::regions::ScreenRegions;

pub struct ScreenModel {
    parser: vt100::Parser,
    regions: ScreenRegions,
    pub prev_content_rows: Vec<String>,
    prev_status_text: String,
}

fn extract_row(screen: &vt100::Screen, row: u16, cols: u16) -> String {
    // Unwritten cells — and cells whose contents are empty because ink/React-
    // style TUIs use cursor-positioning to skip over gaps instead of writing
    // space characters — must render as " " so that adjacent styled words
    // retain their separators. Without this, "Hello World" stored as two
    // styled runs with a positional gap comes back as "HelloWorld".
    (0..cols)
        .map(
            |col| match screen.cell(row, col).map(vt100::Cell::contents) {
                Some(s) if !s.is_empty() => s,
                _ => " ",
            },
        )
        .collect::<String>()
        .trim_end()
        .to_string()
}

impl ScreenModel {
    pub fn new(rows: u16, cols: u16, scrollback_rows: usize, regions: ScreenRegions) -> Self {
        let parser = vt100::Parser::new(rows, cols, scrollback_rows);
        let content_row_count = regions.content_end.saturating_sub(regions.content_start) as usize;
        Self {
            parser,
            regions,
            prev_content_rows: vec![String::new(); content_row_count],
            prev_status_text: String::new(),
        }
    }

    pub fn process(&mut self, bytes: &[u8]) -> Vec<ScreenChange> {
        self.parser.process(bytes);
        let mut changes = Vec::new();
        let screen = self.parser.screen();

        // Diff content rows
        let mut had_content = false;
        let mut all_empty_now = true;
        for row in self.regions.content_start..self.regions.content_end {
            let idx = (row - self.regions.content_start) as usize;
            let new_text = extract_row(screen, row, self.regions.cols);
            if !self.prev_content_rows[idx].is_empty() {
                had_content = true;
            }
            if !new_text.is_empty() {
                all_empty_now = false;
            }
            if self.prev_content_rows[idx] != new_text {
                changes.push(ScreenChange::ContentRowChanged {
                    row,
                    old: self.prev_content_rows[idx].clone(),
                    new: new_text.clone(),
                });
                self.prev_content_rows[idx] = new_text;
            }
        }

        // Diff status bar
        let new_status = self.status_text_inner(screen);
        if new_status != self.prev_status_text {
            changes.push(ScreenChange::StatusBarChanged {
                old: self.prev_status_text.clone(),
                new: new_status.clone(),
            });
            self.prev_status_text = new_status;
        }

        // Screen cleared detection
        if had_content && all_empty_now {
            changes.push(ScreenChange::ScreenCleared);
        }

        changes
    }

    fn status_text_inner(&self, screen: &vt100::Screen) -> String {
        let mut parts = Vec::new();
        for row in self.regions.status_start..self.regions.status_end {
            parts.push(extract_row(screen, row, self.regions.cols));
        }
        parts.join("\n").trim_end().to_string()
    }

    pub fn content_text(&self) -> String {
        self.prev_content_rows.join("\n").trim_end().to_string()
    }

    pub fn status_text(&self) -> String {
        self.prev_status_text.clone()
    }

    pub fn cursor_position(&self) -> (u16, u16) {
        self.parser.screen().cursor_position()
    }

    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    pub fn set_regions(&mut self, regions: ScreenRegions) {
        let content_row_count = regions.content_end.saturating_sub(regions.content_start) as usize;
        self.prev_content_rows = vec![String::new(); content_row_count];
        self.prev_status_text = String::new();
        self.regions = regions;
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.parser.screen_mut().set_size(rows, cols);
        self.regions.resize(rows, cols);
        let content_row_count = self
            .regions
            .content_end
            .saturating_sub(self.regions.content_start) as usize;
        self.prev_content_rows = vec![String::new(); content_row_count];
        self.prev_status_text = String::new();
    }
}
