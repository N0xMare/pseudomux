use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type SessionId = Uuid;

pub use pseudomux_core::output::chunk::OutputChunk;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StartSessionParams {
    pub agent: Option<String>,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: Vec<(String, String)>,
    pub cwd: Option<String>,
    pub rows: Option<u16>,
    pub cols: Option<u16>,
    pub logging_mode: Option<String>,
    #[serde(default)]
    pub record_path: Option<String>,
    /// Human-readable session name for identification in `pmux list` and JSON output.
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SendTextParams {
    pub session: SessionId,
    pub text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SendBytesParams {
    pub session: SessionId,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SendKeyParams {
    pub session: SessionId,
    pub key: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SendActionParams {
    pub session: SessionId,
    pub action: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SendPromptParams {
    pub session: SessionId,
    pub text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReadSinceParams {
    pub session: SessionId,
    pub seq: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResizeParams {
    pub session: SessionId,
    pub rows: u16,
    pub cols: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InterruptParams {
    pub session: SessionId,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TerminateParams {
    pub session: SessionId,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionStateParams {
    pub session: SessionId,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubscribeEventsParams {
    pub session: SessionId,
    /// Maximum duration to stream events (milliseconds). 0 = no limit.
    #[serde(default)]
    pub timeout_ms: u64,
    /// Maximum number of events to return. 0 = no limit.
    #[serde(default)]
    pub max_events: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContentSinceParams {
    pub session: SessionId,
    pub seq: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContentEntryDto {
    pub seq: u64,
    pub timestamp_ms: u64,
    pub tag: String,
    pub text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Request {
    Ping,
    StartSession(StartSessionParams),
    SendText(SendTextParams),
    SendBytes(SendBytesParams),
    SendEnter(InterruptParams),
    SendKey(SendKeyParams),
    SendAction(SendActionParams),
    SendPrompt(SendPromptParams),
    ReadSince(ReadSinceParams),
    Resize(ResizeParams),
    Interrupt(InterruptParams),
    Terminate(TerminateParams),
    GetState(SessionStateParams),
    GetTerminalState(SessionStateParams),
    ListSessions,
    /// Subscribe to semantic events for a session (streaming).
    SubscribeEvents(SubscribeEventsParams),
    /// Get the current VTE-inferred agent state.
    GetAgentState(SessionStateParams),
    /// Get the current content region text from VTE screen model.
    GetContentText(SessionStateParams),
    /// Get the current status bar text from VTE screen model.
    GetStatusText(SessionStateParams),
    /// Get content buffer entries since a sequence number.
    GetContentSince(ContentSinceParams),
    /// Get content buffer entries since last user input.
    GetContentSinceLastInput(SessionStateParams),
    /// Get current content buffer sequence number.
    GetContentCurrentSeq(SessionStateParams),
    /// Subscribe to watch events for a session.
    SubscribeWatchEvents(SubscribeEventsParams),
    /// Get filtered content since a sequence number (TUI chrome stripped).
    GetFilteredContent(ContentSinceParams),
    /// Get filtered content since last user input (TUI chrome stripped).
    GetFilteredContentSinceLastInput(SessionStateParams),
    /// Snapshot the current visible content region with TUI chrome stripped —
    /// a final-state read that cannot return duplicated progressive fragments.
    /// Useful for "what's on screen right now?" but limited to the visible
    /// content region; scrolled-off content is not included.
    GetFilteredScreenContent(SessionStateParams),
    /// Row-aware assistant response since the last user input: walks the
    /// content buffer, collapses same-row entries to their latest value, and
    /// strips TUI chrome. This is the preferred primitive for `pmux prompt`
    /// because it survives ink/React re-renders (which rewrite every row on
    /// every token) AND captures rows that have scrolled off the visible area.
    GetFilteredResponseSinceLastInput(SessionStateParams),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionSummary {
    pub session: SessionId,
    pub status: String,
    pub rows: u16,
    pub cols: u16,
    pub pid: Option<u32>,
    pub profile: Option<String>,
    pub agent: String,
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Response {
    Pong,
    Ack,
    StartSession {
        session: SessionId,
    },
    Output {
        chunks: Vec<OutputChunk>,
        next_seq: u64,
    },
    SessionState {
        summary: SessionSummary,
    },
    Sessions {
        sessions: Vec<SessionSummary>,
    },
    Error {
        code: String,
        message: String,
    },
    /// Current agent state from VTE classifier.
    AgentState {
        state: String,
    },
    /// Content region text from VTE screen model.
    ContentText {
        text: String,
    },
    /// Status bar text from VTE screen model.
    StatusText {
        text: String,
    },
    /// A semantic event from the VTE classifier (pushed during `SubscribeEvents`).
    SemanticEvent {
        event: serde_json::Value,
    },
    /// Current negotiated terminal state.
    TerminalState {
        keyboard_mode: String,
        bracketed_paste: bool,
        focus_events: bool,
    },
    /// Content buffer entries.
    Content {
        entries: Vec<ContentEntryDto>,
        next_seq: u64,
    },
    /// Current content buffer sequence number.
    ContentSeq {
        seq: u64,
    },
    /// A watch event from the VTE watch system.
    /// Filtered content text (TUI chrome stripped).
    FilteredContent {
        text: String,
        next_seq: u64,
    },
    WatchEvent {
        event: serde_json::Value,
    },
}

impl Response {
    pub fn ok() -> Self {
        Response::Ack
    }

    pub fn error<C: Into<String>, M: Into<String>>(code: C, message: M) -> Self {
        Response::Error {
            code: code.into(),
            message: message.into(),
        }
    }
}
