use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::time::SystemTime;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ContentTag {
    AssistantOutput,
    ToolOutput,
    UserInput,
    StatusChange,
    InputBoundary,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContentEntry {
    pub seq: u64,
    pub timestamp: SystemTime,
    pub tag: ContentTag,
    pub text: String,
    /// Screen row this entry came from, when sourced from a VTE content-row
    /// change. `None` for synthetic entries (e.g. InputBoundary) or entries
    /// appended by tests. Used by `filtered_text_latest_per_row_since_last_input`
    /// to dedupe ink/React re-renders that rewrite the same row many times.
    #[serde(default)]
    pub row: Option<u16>,
}

pub struct ContentBuffer {
    pub entries: VecDeque<ContentEntry>,
    max_bytes: usize,
    pub current_bytes: usize,
    seq: u64,
}

impl ContentBuffer {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            max_bytes,
            current_bytes: 0,
            seq: 0,
        }
    }

    pub fn append(&mut self, text: String, tag: ContentTag) -> u64 {
        self.append_inner(text, tag, None)
    }

    pub fn append_with_row(&mut self, text: String, tag: ContentTag, row: u16) -> u64 {
        self.append_inner(text, tag, Some(row))
    }

    fn append_inner(&mut self, text: String, tag: ContentTag, row: Option<u16>) -> u64 {
        let text_len = text.len();
        while self.current_bytes + text_len > self.max_bytes && !self.entries.is_empty() {
            if let Some(evicted) = self.entries.pop_front() {
                self.current_bytes -= evicted.text.len();
            }
        }
        // Guard: a single entry larger than max_bytes would permanently break the
        // capacity invariant. Skip it rather than corrupt the buffer.
        if text_len > self.max_bytes {
            self.seq += 1;
            return self.seq;
        }
        self.seq += 1;
        self.current_bytes += text_len;
        self.entries.push_back(ContentEntry {
            seq: self.seq,
            timestamp: SystemTime::now(),
            tag,
            text,
            row,
        });
        self.seq
    }

    pub fn mark_input_boundary(&mut self) -> u64 {
        self.append(String::new(), ContentTag::InputBoundary)
    }

    pub fn since_seq(&self, seq: u64) -> Vec<&ContentEntry> {
        self.entries.iter().filter(|e| e.seq > seq).collect()
    }

    pub fn since_last_input(&self) -> Vec<&ContentEntry> {
        let last_boundary_idx = self
            .entries
            .iter()
            .rposition(|e| e.tag == ContentTag::InputBoundary);
        match last_boundary_idx {
            Some(idx) => self.entries.iter().skip(idx + 1).collect(),
            None => self.entries.iter().collect(),
        }
    }

    pub fn last_n(&self, n: usize) -> Vec<&ContentEntry> {
        self.entries
            .iter()
            .filter(|e| e.tag != ContentTag::InputBoundary)
            .rev()
            .take(n)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }

    pub fn text_since_seq(&self, seq: u64) -> String {
        self.since_seq(seq)
            .iter()
            .filter(|e| e.tag != ContentTag::InputBoundary)
            .map(|e| e.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn text_since_last_input(&self) -> String {
        self.since_last_input()
            .iter()
            .filter(|e| e.tag != ContentTag::InputBoundary)
            .map(|e| e.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn current_seq(&self) -> u64 {
        self.seq
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for ContentBuffer {
    fn default() -> Self {
        Self::new(1_048_576)
    }
}

use crate::adapter::ContentFilterTrait;

impl ContentBuffer {
    /// Returns filtered text since last input boundary, with TUI chrome stripped.
    pub fn filtered_text_since_last_input(&self, filter: &dyn ContentFilterTrait) -> String {
        use super::content_filter::extract_response_text;
        let lines: Vec<String> = self
            .since_last_input()
            .iter()
            .filter(|e| e.tag != ContentTag::InputBoundary)
            .filter_map(|e| filter.filter_line(&e.text))
            .collect();
        extract_response_text(lines).join("\n")
    }

    /// Returns filtered text since a sequence number.
    pub fn filtered_text_since_seq(&self, seq: u64, filter: &dyn ContentFilterTrait) -> String {
        use super::content_filter::extract_response_text;
        let lines: Vec<String> = self
            .since_seq(seq)
            .iter()
            .filter(|e| e.tag != ContentTag::InputBoundary)
            .filter_map(|e| filter.filter_line(&e.text))
            .collect();
        extract_response_text(lines).join("\n")
    }

    /// Row-aware filtered text: collapses the three kinds of duplication that
    /// ink/React TUIs produce in the content buffer.
    ///
    /// For each entry `E` at row `R` with text `T`:
    ///
    ///   1. **Progressive streaming** — drop `E` if a later entry at row `R`
    ///      has text that is a strict prefix extension of `T` (`starts_with`
    ///      and longer). Collapses `"Pse"` → `"Pseudomux"` → `"Pseudomux Review"`.
    ///
    ///   2. **Full-region re-render** — drop `E` if a later entry at row `R`
    ///      has identical text. Collapses the clear-then-rewrite cycles that
    ///      ink uses on every token when it redraws the whole content region.
    ///
    ///   3. **Scroll-up repeat** — drop `E` if an earlier entry at a *higher*
    ///      row has identical text. When a vt100 scroll shifts line L from
    ///      row 5 to row 4 and then row 3, all three positions appear in the
    ///      buffer; we keep only the original (highest-row) occurrence so the
    ///      line is emitted once at its chronological position.
    ///
    /// Surviving entries are emitted in insertion order, preserving the
    /// chronological flow of the response even across scroll events — pre-
    /// scroll content stays in the output and isn't overwritten by the
    /// post-scroll state of the same row.
    ///
    /// Entries without a `row` (legacy `append`, synthetic boundaries) are
    /// passed through in arrival order without dedup.
    pub fn filtered_text_latest_per_row_since_last_input(
        &self,
        filter: &dyn ContentFilterTrait,
    ) -> String {
        use super::content_filter::extract_response_text;

        let window = self.since_last_input();
        let kept: Vec<&ContentEntry> = window
            .iter()
            .enumerate()
            .filter(|(i, e)| {
                if e.tag == ContentTag::InputBoundary {
                    return false;
                }
                let Some(row) = e.row else {
                    return true;
                };
                // Rules 1 + 2: later entry at same row supersedes this one.
                let superseded_later = window.iter().skip(i + 1).any(|later| {
                    later.row == Some(row)
                        && (later.text == e.text
                            || (later.text.len() > e.text.len() && later.text.starts_with(&e.text)))
                });
                if superseded_later {
                    return false;
                }
                // Rule 3: earlier entry at higher row with identical text
                // means this entry is a scroll-up repeat.
                let scroll_up_repeat = window.iter().take(*i).any(|earlier| {
                    matches!(earlier.row, Some(er) if er > row) && earlier.text == e.text
                });
                !scroll_up_repeat
            })
            .map(|(_, e)| *e)
            .collect();
        let lines: Vec<String> = kept
            .iter()
            .filter_map(|e| filter.filter_line(&e.text))
            .collect();
        extract_response_text(lines).join("\n")
    }
}
