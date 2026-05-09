use super::differ::ScreenChange;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SemanticEvent {
    AssistantDelta {
        text: String,
        seq: u64,
    },
    AssistantTurnStarted {
        seq: u64,
    },
    AssistantTurnCompleted {
        full_text: String,
        seq: u64,
    },
    ThinkingStarted {
        seq: u64,
    },
    ThinkingCompleted {
        seq: u64,
    },
    ToolStarted {
        name: Option<String>,
        seq: u64,
    },
    ToolFinished {
        seq: u64,
    },
    StateChanged {
        from: AgentState,
        to: AgentState,
        seq: u64,
    },
    ConfirmationRequired {
        prompt_text: String,
        seq: u64,
    },
    AuthRequired {
        seq: u64,
    },
    AuthResolved {
        seq: u64,
    },
    ScreenRedraw {
        seq: u64,
    },
    /// Emitted by the session manager (not the classifier) when the PTY process exits.
    SessionExited {
        exit_code: Option<i32>,
        seq: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentState {
    Booting,
    Ready,
    Thinking,
    ToolRunning,
    AuthRequired,
    Error,
    Unknown,
}

/// Pattern sets used by [`RegionClassifier::detect_state`] to map raw VTE text
/// to an [`AgentState`].
///
/// Each field is a list of case-insensitive substrings. Checked in priority order:
/// auth > error > thinking > tool > ready > unknown.
#[derive(Default)]
pub struct StatusPatterns {
    pub thinking_indicators: Vec<String>,
    pub tool_indicators: Vec<String>,
    pub ready_indicators: Vec<String>,
    pub auth_indicators: Vec<String>,
    pub error_indicators: Vec<String>,
    /// When true, also scan content row changes for status patterns.
    /// Needed for TUIs where status appears in the content area.
    pub scan_content_for_status: bool,
}

impl StatusPatterns {
    pub fn opencode_v1_2() -> Self {
        Self {
            thinking_indicators: vec!["esc interrupt".into(), "esc again to interrupt".into()],
            tool_indicators: vec![],
            ready_indicators: vec!["ctrl+p commands".into(), "Ask anything".into()],
            auth_indicators: vec!["Get started /connect".into()],
            error_indicators: vec!["Bad Request".into()],
            scan_content_for_status: true,
        }
    }

    pub fn claude_code() -> Self {
        Self {
            // VTE cell extraction may strip spaces from styled text,
            // so patterns include both spaced and spaceless variants.
            thinking_indicators: vec!["esc to interrupt".into(), "esctointerrupt".into()],
            // Claude Code renders tool invocations in the content region as
            // a parenthesized header like `Read(file)`, `Bash(cmd)`,
            // `Fetch(url)`, `Task(prompt)`. We only match that form because
            // the gerund alternatives ("Reading", "Running", "Searching",
            // "Writing") appear in normal prose far too often to be safe
            // substring indicators — the user's echoed prompt would
            // false-trigger ToolRunning transitions. The parenthesized form
            // does not appear in natural prose and is the first thing Claude
            // Code prints when a tool call starts, so it's a reliable anchor.
            tool_indicators: vec![
                "Bash(".into(),
                "Read(".into(),
                "Write(".into(),
                "Edit(".into(),
                "Grep(".into(),
                "Glob(".into(),
                "Fetch(".into(),
                "Task(".into(),
                "Update(".into(),
                "List(".into(),
                "NotebookEdit(".into(),
                "TodoWrite(".into(),
                // Sub-agent types (Claude Code's Agent tool)
                "Explore(".into(),
                "Plan(".into(),
                "Agent(".into(),
            ],
            ready_indicators: vec![
                "? for shortcuts".into(),
                "?forshortcuts".into(),
                "bypass permissions".into(),
                "bypasspermissions".into(),
            ],
            auth_indicators: vec![
                "Enter to confirm".into(),
                "Entertoconfirm".into(),
                "trust this folder".into(),
                "trustthisfolder".into(),
                "Please run /login".into(),
            ],
            error_indicators: vec![
                "API Error".into(),
                "rate limit".into(),
                "weekly limit".into(),
            ],
            // Claude Code uses ink (React TUI) which re-renders the full screen,
            // so status indicators appear in content rows, not a fixed status region.
            scan_content_for_status: true,
        }
    }
}

/// A function that checks if text is a confirmation prompt.
type ConfirmationChecker = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// Extract a human-readable tool name from the content/status text that
/// triggered a transition into [`AgentState::ToolRunning`]. Matches common
/// TUI patterns across Claude Code and OpenCode:
///
/// - Gerund verbs: `"Reading 2 files..."` → `"Read"`, `"Writing foo.txt"` → `"Write"`
/// - Parenthesized form: `"Bash(ls -la)"` → `"Bash"`, `"Grep(pattern)"` → `"Grep"`
/// - Listing / Searching / Editing variants → `"List"`, `"Search"`, `"Edit"`
///
/// Returns `None` when no known pattern matches, which callers surface as a
/// `null` tool name rather than fabricating one.
pub(crate) fn extract_tool_name(text: &str) -> Option<String> {
    let trimmed = text.trim_start_matches(|c: char| !c.is_alphanumeric() && c != '(');
    // Parenthesized form first: "Bash(", "Grep(", "Read(", "Edit(" ...
    if let Some(paren) = trimmed.find('(') {
        let head = &trimmed[..paren];
        let head = head.trim();
        if !head.is_empty()
            && head.chars().next().is_some_and(|c| c.is_ascii_uppercase())
            && head.chars().all(|c| c.is_alphanumeric() || c == '_')
        {
            return Some(head.to_string());
        }
    }
    // Gerund verb at start: "Reading ...", "Writing ...", "Searching ...", etc.
    let lower = trimmed.to_lowercase();
    let mapping: &[(&str, &str)] = &[
        ("reading", "Read"),
        ("writing", "Write"),
        ("searching", "Search"),
        ("grepping", "Grep"),
        ("editing", "Edit"),
        ("listing", "List"),
        ("running", "Bash"),
        ("executing", "Bash"),
        ("fetching", "WebFetch"),
        ("globbing", "Glob"),
    ];
    for (prefix, name) in mapping {
        if lower.starts_with(prefix) {
            return Some((*name).to_string());
        }
    }
    None
}

/// Classifies [`ScreenChange`] events into higher-level [`SemanticEvent`]s.
///
/// Maintains agent state, a running turn buffer, and quiescence detection.
/// One instance is held per session inside an `Arc<Mutex<_>>`.
pub struct RegionClassifier {
    current_state: AgentState,
    turn_buffer: String,
    pub in_turn: bool,
    pub last_content_change: Option<Instant>,
    status_patterns: StatusPatterns,
    seq: u64,
    confirmation_checker: Option<ConfirmationChecker>,
    /// Name captured from the content row that triggered the most recent
    /// transition into `ToolRunning`. Consumed by `transition_state` when it
    /// emits `ToolStarted`, so the event can carry a real tool name instead
    /// of `None` for TUIs (like Claude Code) that show the tool identifier
    /// in the content area rather than the status bar.
    pending_tool_name: Option<String>,
}

impl RegionClassifier {
    pub fn new(patterns: StatusPatterns) -> Self {
        Self {
            current_state: AgentState::Booting,
            turn_buffer: String::new(),
            in_turn: false,
            last_content_change: None,
            status_patterns: patterns,
            seq: 0,
            confirmation_checker: None,
            pending_tool_name: None,
        }
    }

    pub fn with_confirmation_checker(mut self, checker: ConfirmationChecker) -> Self {
        self.confirmation_checker = Some(checker);
        self
    }

    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    pub fn state(&self) -> AgentState {
        self.current_state
    }

    fn transition_state(&mut self, new_state: AgentState, events: &mut Vec<SemanticEvent>) {
        if new_state == self.current_state {
            return;
        }
        let old_state = self.current_state;
        let seq = self.next_seq();
        events.push(SemanticEvent::StateChanged {
            from: old_state,
            to: new_state,
            seq,
        });

        // Lifecycle events for leaving old state
        match old_state {
            AgentState::Thinking => {
                events.push(SemanticEvent::ThinkingCompleted {
                    seq: self.next_seq(),
                });
            }
            AgentState::ToolRunning => {
                events.push(SemanticEvent::ToolFinished {
                    seq: self.next_seq(),
                });
            }
            AgentState::AuthRequired => {
                events.push(SemanticEvent::AuthResolved {
                    seq: self.next_seq(),
                });
            }
            _ => {}
        }

        // Lifecycle events for entering new state
        match new_state {
            AgentState::Thinking => {
                events.push(SemanticEvent::ThinkingStarted {
                    seq: self.next_seq(),
                });
            }
            AgentState::ToolRunning => {
                let name = self.pending_tool_name.take();
                events.push(SemanticEvent::ToolStarted {
                    name,
                    seq: self.next_seq(),
                });
            }
            AgentState::AuthRequired => {
                events.push(SemanticEvent::AuthRequired {
                    seq: self.next_seq(),
                });
            }
            _ => {}
        }

        self.current_state = new_state;
    }

    /// Infer the current agent state from raw status-bar or content text.
    ///
    /// Returns [`AgentState::Unknown`] if no pattern matches — callers should
    /// treat `Unknown` as "no change" rather than a real state transition.
    pub fn detect_state(&self, status_text: &str) -> AgentState {
        let lower = status_text.to_lowercase();
        // Priority: auth > error > thinking > tool > ready > unknown
        for pat in &self.status_patterns.auth_indicators {
            if lower.contains(&pat.to_lowercase()) {
                return AgentState::AuthRequired;
            }
        }
        for pat in &self.status_patterns.error_indicators {
            if lower.contains(&pat.to_lowercase()) {
                return AgentState::Error;
            }
        }
        for pat in &self.status_patterns.thinking_indicators {
            if lower.contains(&pat.to_lowercase()) {
                return AgentState::Thinking;
            }
        }
        for pat in &self.status_patterns.tool_indicators {
            if lower.contains(&pat.to_lowercase()) {
                return AgentState::ToolRunning;
            }
        }
        for pat in &self.status_patterns.ready_indicators {
            if lower.contains(&pat.to_lowercase()) {
                return AgentState::Ready;
            }
        }
        AgentState::Unknown
    }

    /// Process a batch of screen changes and return the resulting semantic events.
    pub fn classify(&mut self, changes: &[ScreenChange]) -> Vec<SemanticEvent> {
        let mut events = Vec::new();

        for change in changes {
            match change {
                ScreenChange::StatusBarChanged { new, .. } => {
                    let new_state = self.detect_state(new);
                    // Only transition on recognized states — unrecognized status bar
                    // text (Unknown) should not override a known state.
                    if new_state != AgentState::Unknown {
                        if new_state == AgentState::ToolRunning {
                            self.pending_tool_name = extract_tool_name(new);
                        }
                        self.transition_state(new_state, &mut events);
                    }
                }
                ScreenChange::ContentRowChanged { new, .. } => {
                    // Scan content for status patterns if enabled
                    if self.status_patterns.scan_content_for_status && !new.is_empty() {
                        let new_state = self.detect_state(new);
                        if new_state != AgentState::Unknown {
                            // If we're transitioning into ToolRunning, try to
                            // capture a human-readable tool name from the same
                            // content row that triggered the state change.
                            if new_state == AgentState::ToolRunning {
                                self.pending_tool_name = extract_tool_name(new);
                            }
                            self.transition_state(new_state, &mut events);
                            continue;
                        }
                        // Check for confirmation prompts in content area
                        if self.current_state == AgentState::Thinking
                            && self.confirmation_checker.as_ref().is_some_and(|c| c(new))
                        {
                            let seq = self.next_seq();
                            events.push(SemanticEvent::ConfirmationRequired {
                                prompt_text: new.clone(),
                                seq,
                            });
                            continue;
                        }
                    }

                    if !self.in_turn && !new.is_empty() {
                        self.in_turn = true;
                        self.turn_buffer.clear();
                        let seq = self.next_seq();
                        events.push(SemanticEvent::AssistantTurnStarted { seq });
                    }
                    if !new.is_empty() {
                        self.turn_buffer.push_str(new);
                        self.turn_buffer.push('\n');
                        let seq = self.next_seq();
                        events.push(SemanticEvent::AssistantDelta {
                            text: new.clone(),
                            seq,
                        });
                        self.last_content_change = Some(Instant::now());
                    }
                }
                ScreenChange::ScreenCleared => {
                    let seq = self.next_seq();
                    events.push(SemanticEvent::ScreenRedraw { seq });
                }
            }
        }

        events
    }

    /// Fire [`SemanticEvent::AssistantTurnCompleted`] if the agent has been `Ready`
    /// with no new content for longer than `threshold`.
    ///
    /// Called periodically from the quiescence thread.
    pub fn check_quiescence(&mut self, threshold: Duration) -> Option<SemanticEvent> {
        if self.in_turn
            && self.current_state == AgentState::Ready
            && let Some(last) = self.last_content_change
            && last.elapsed() >= threshold
        {
            self.in_turn = false;
            let full_text = self.turn_buffer.trim_end().to_string();
            self.turn_buffer.clear();
            self.last_content_change = None;
            let seq = self.next_seq();
            return Some(SemanticEvent::AssistantTurnCompleted { full_text, seq });
        }
        None
    }
}

#[cfg(test)]
mod extract_tool_name_tests {
    use super::extract_tool_name;

    #[test]
    fn parenthesized_tool() {
        assert_eq!(extract_tool_name("Bash(ls -la)"), Some("Bash".into()));
        assert_eq!(extract_tool_name("Grep(pattern.*)"), Some("Grep".into()));
        assert_eq!(extract_tool_name("Read(README.md)"), Some("Read".into()));
        assert_eq!(
            extract_tool_name("WebFetch(https://example.com)"),
            Some("WebFetch".into())
        );
    }

    #[test]
    fn gerund_verbs() {
        assert_eq!(extract_tool_name("Reading 2 files…"), Some("Read".into()));
        assert_eq!(
            extract_tool_name("Writing to Cargo.toml"),
            Some("Write".into())
        );
        assert_eq!(
            extract_tool_name("Searching for fn main"),
            Some("Search".into())
        );
        assert_eq!(
            extract_tool_name("Listing 1 directory…"),
            Some("List".into())
        );
        assert_eq!(extract_tool_name("Running cargo test"), Some("Bash".into()));
    }

    #[test]
    fn unknown_returns_none() {
        assert_eq!(extract_tool_name("some random text"), None);
        assert_eq!(extract_tool_name(""), None);
        assert_eq!(extract_tool_name("esc to interrupt"), None);
    }

    #[test]
    fn tolerates_leading_chrome() {
        // Leading spinner glyphs, bullet chars, or whitespace shouldn't block
        // the match. "⏺ Bash(ls)" → "Bash".
        assert_eq!(extract_tool_name("⏺ Bash(ls)"), Some("Bash".into()));
        assert_eq!(
            extract_tool_name("  Reading Cargo.toml"),
            Some("Read".into())
        );
    }
}
