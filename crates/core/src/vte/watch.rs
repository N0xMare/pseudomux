use serde::{Deserialize, Serialize};
use std::time::SystemTime;

use super::classifier::{AgentState, SemanticEvent};

/// Pilot-friendly structured events for efficient monitoring.
/// These are derived from `SemanticEvent` but batched and enriched.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum WatchEvent {
    StateChange {
        from: String,
        to: String,
        ts: u64,
        seq: u64,
    },
    ContentDelta {
        lines: usize,
        chars: usize,
        preview: String,
        tag: String,
        ts: u64,
        seq: u64,
    },
    TurnComplete {
        total_lines: usize,
        total_chars: usize,
        duration_ms: u64,
        ts: u64,
        seq: u64,
    },
    InputRequired {
        kind: String,
        prompt_text: String,
        ts: u64,
        seq: u64,
    },
    ErrorDetected {
        text: String,
        ts: u64,
        seq: u64,
    },
    InputSent {
        preview: String,
        ts: u64,
        seq: u64,
    },
    SessionExited {
        exit_code: Option<i32>,
        ts: u64,
        seq: u64,
    },
    /// A tool invocation began. Surfaces the tool name when the classifier
    /// could infer it (e.g. "Read", "Bash", "Grep"). Consumers use this to
    /// show progress or to build a tool-call summary in the final response.
    ToolStarted {
        name: Option<String>,
        ts: u64,
        seq: u64,
    },
    /// A tool invocation finished (state transitioned out of ToolRunning).
    ToolFinished {
        ts: u64,
        seq: u64,
    },
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn truncate_preview(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}

/// Converts [`SemanticEvent`]s into [`WatchEvent`]s with batching and enrichment.
///
/// Accumulates [`SemanticEvent::AssistantDelta`] entries into a pending delta
/// buffer that is flushed on the next state change or turn boundary, reducing
/// the number of events a pilot agent needs to process.
pub struct WatchEventBuilder {
    seq: u64,
    turn_start_time: Option<SystemTime>,
    turn_line_count: usize,
    turn_char_count: usize,
    pending_delta_lines: usize,
    pending_delta_chars: usize,
    pending_delta_preview: String,
    pending_delta_tag: String,
    /// Guards against duplicate TurnComplete events when both AssistantTurnCompleted
    /// and StateChanged { to: Ready } fire in the same turn.
    turn_complete_emitted: bool,
}

impl WatchEventBuilder {
    pub fn new() -> Self {
        Self {
            seq: 0,
            turn_start_time: None,
            turn_line_count: 0,
            turn_char_count: 0,
            pending_delta_lines: 0,
            pending_delta_chars: 0,
            pending_delta_preview: String::new(),
            pending_delta_tag: String::new(),
            turn_complete_emitted: false,
        }
    }

    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    pub fn process(&mut self, event: &SemanticEvent) -> Vec<WatchEvent> {
        let mut out = Vec::new();

        match event {
            SemanticEvent::StateChanged { from, to, seq: _ } => {
                if let Some(delta) = self.flush_pending() {
                    out.push(delta);
                }
                let seq = self.next_seq();
                out.push(WatchEvent::StateChange {
                    from: format!("{from:?}"),
                    to: format!("{to:?}"),
                    ts: now_millis(),
                    seq,
                });
                if *to == AgentState::Thinking {
                    self.turn_start_time = Some(SystemTime::now());
                    self.turn_line_count = 0;
                    self.turn_char_count = 0;
                    self.turn_complete_emitted = false;
                }
                if (*from == AgentState::Thinking || *from == AgentState::ToolRunning)
                    && *to == AgentState::Ready
                    && !self.turn_complete_emitted
                {
                    let duration_ms = self
                        .turn_start_time
                        .and_then(|t| t.elapsed().ok())
                        .map_or(0, |d| d.as_millis() as u64);
                    let seq = self.next_seq();
                    out.push(WatchEvent::TurnComplete {
                        total_lines: self.turn_line_count,
                        total_chars: self.turn_char_count,
                        duration_ms,
                        ts: now_millis(),
                        seq,
                    });
                    self.turn_start_time = None;
                    self.turn_complete_emitted = true;
                }
            }
            SemanticEvent::AssistantDelta { text, seq: _ } => {
                let line_count = text.lines().count().max(1);
                let char_count = text.len();
                self.pending_delta_lines += line_count;
                self.pending_delta_chars += char_count;
                self.turn_line_count += line_count;
                self.turn_char_count += char_count;
                if self.pending_delta_preview.is_empty() {
                    self.pending_delta_preview = truncate_preview(text, 80);
                }
                if self.pending_delta_tag.is_empty() {
                    self.pending_delta_tag = "assistant".to_string();
                }
            }
            SemanticEvent::ThinkingStarted { seq: _ } => {
                self.turn_start_time = Some(SystemTime::now());
                self.turn_line_count = 0;
                self.turn_char_count = 0;
                self.turn_complete_emitted = false;
            }
            SemanticEvent::AssistantTurnCompleted { full_text, seq: _ } => {
                if let Some(delta) = self.flush_pending() {
                    out.push(delta);
                }
                if !self.turn_complete_emitted {
                    let duration_ms = self
                        .turn_start_time
                        .and_then(|t| t.elapsed().ok())
                        .map_or(0, |d| d.as_millis() as u64);
                    let lines = full_text.lines().count().max(1);
                    let chars = full_text.len();
                    let seq = self.next_seq();
                    out.push(WatchEvent::TurnComplete {
                        total_lines: lines.max(self.turn_line_count),
                        total_chars: chars.max(self.turn_char_count),
                        duration_ms,
                        ts: now_millis(),
                        seq,
                    });
                    self.turn_complete_emitted = true;
                }
                self.turn_start_time = None;
                self.turn_line_count = 0;
                self.turn_char_count = 0;
            }
            SemanticEvent::ConfirmationRequired {
                prompt_text,
                seq: _,
            } => {
                if let Some(delta) = self.flush_pending() {
                    out.push(delta);
                }
                let seq = self.next_seq();
                out.push(WatchEvent::InputRequired {
                    kind: "confirmation".to_string(),
                    prompt_text: prompt_text.clone(),
                    ts: now_millis(),
                    seq,
                });
            }
            SemanticEvent::AuthRequired { seq: _ } => {
                let seq = self.next_seq();
                out.push(WatchEvent::InputRequired {
                    kind: "auth".to_string(),
                    prompt_text: "Authentication required".to_string(),
                    ts: now_millis(),
                    seq,
                });
            }
            SemanticEvent::ToolStarted { name, seq: _ } => {
                if let Some(delta) = self.flush_pending() {
                    out.push(delta);
                }
                self.pending_delta_tag = "tool".to_string();
                let seq = self.next_seq();
                out.push(WatchEvent::ToolStarted {
                    name: name.clone(),
                    ts: now_millis(),
                    seq,
                });
            }
            SemanticEvent::ToolFinished { seq: _ } => {
                if let Some(delta) = self.flush_pending() {
                    out.push(delta);
                }
                let seq = self.next_seq();
                out.push(WatchEvent::ToolFinished {
                    ts: now_millis(),
                    seq,
                });
            }
            SemanticEvent::SessionExited { exit_code, seq: _ } => {
                if let Some(delta) = self.flush_pending() {
                    out.push(delta);
                }
                let seq = self.next_seq();
                out.push(WatchEvent::SessionExited {
                    exit_code: *exit_code,
                    ts: now_millis(),
                    seq,
                });
            }
            // ThinkingStarted handled above. ThinkingCompleted, AuthResolved,
            // ScreenRedraw, and AssistantTurnStarted are not forwarded —
            // consumers rely on StateChange and TurnComplete instead.
            _ => {}
        }

        out
    }

    pub fn flush_pending(&mut self) -> Option<WatchEvent> {
        if self.pending_delta_chars == 0 {
            return None;
        }
        let seq = self.next_seq();
        let event = WatchEvent::ContentDelta {
            lines: self.pending_delta_lines,
            chars: self.pending_delta_chars,
            preview: std::mem::take(&mut self.pending_delta_preview),
            tag: if self.pending_delta_tag.is_empty() {
                "unknown".to_string()
            } else {
                std::mem::take(&mut self.pending_delta_tag)
            },
            ts: now_millis(),
            seq,
        };
        self.pending_delta_lines = 0;
        self.pending_delta_chars = 0;
        Some(event)
    }

    pub fn notify_input_sent(&mut self, text: &str) -> WatchEvent {
        let seq = self.next_seq();
        WatchEvent::InputSent {
            preview: truncate_preview(text, 80),
            ts: now_millis(),
            seq,
        }
    }

    pub fn notify_session_exited(&mut self, exit_code: Option<i32>) -> WatchEvent {
        let seq = self.next_seq();
        WatchEvent::SessionExited {
            exit_code,
            ts: now_millis(),
            seq,
        }
    }
}

impl Default for WatchEventBuilder {
    fn default() -> Self {
        Self::new()
    }
}
