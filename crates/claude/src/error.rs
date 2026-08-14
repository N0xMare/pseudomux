use thiserror::Error;

use crate::{FileIdentity, LogicalMessageKey, SourceLocation};

/// Failures that prevent a transcript from being an authoritative turn source.
#[derive(Debug, Error)]
pub enum TranscriptError {
    #[error("read offset {actual} does not match cursor offset {expected}")]
    CursorOffsetMismatch { expected: u64, actual: u64 },

    #[error("read ending at {read_end} exceeds observed file length {file_len}")]
    ReadBeyondObservedFile { read_end: u64, file_len: u64 },

    #[error(
        "transcript JSONL record at byte {byte_offset} is {bytes} bytes, exceeding limit {limit}"
    )]
    LineTooLong {
        bytes: usize,
        limit: usize,
        byte_offset: u64,
    },

    #[error("invalid UTF-8 at line {location:?}: {message}")]
    InvalidUtf8 {
        location: SourceLocation,
        message: String,
    },

    #[error("malformed JSON at line {location:?}: {message}")]
    MalformedJson {
        location: SourceLocation,
        message: String,
    },

    #[error("Claude transcript schema drift at {path}{row_suffix}: {message}", row_suffix = row_suffix(.row_uuid))]
    SchemaDrift {
        row_uuid: Option<String>,
        path: String,
        message: String,
    },

    #[error("row UUID {uuid} was observed with different content")]
    ConflictingDuplicateRow { uuid: String },

    #[error("a turn is already armed")]
    TurnAlreadyArmed,

    #[error("no turn is armed")]
    NoTurnArmed,

    // Prompt bodies are intentionally retained for in-process correlation and
    // tests, but must never be rendered through `Display`: transcript errors
    // cross the daemon boundary and may be printed by CLI/MCP clients.
    #[error("a different typed prompt appeared while awaiting the active turn")]
    UnexpectedTypedPrompt { expected: String, actual: String },

    #[error("multiple typed prompt acknowledgements appeared for the active turn")]
    MultiplePromptAcknowledgements,

    #[error("active turn has {leaf_count} ambiguous main-chain branches")]
    AmbiguousActiveBranches { leaf_count: usize },

    #[error("active main-chain row at ordinal {ordinal} has no graph UUID")]
    ActiveRowMissingUuid { ordinal: u64 },

    #[error("post-prompt semantic row {row_uuid} is disconnected from the active graph")]
    DisconnectedActiveRow { row_uuid: String },

    #[error("active row {row_uuid} was appended before its parent {parent_uuid}")]
    ParentAppendOrder {
        row_uuid: String,
        parent_uuid: String,
    },

    #[error("parentUuid graph contains a cycle at {uuid}")]
    ParentCycle { uuid: String },

    #[error("logical assistant message {key:?} reappeared after another active graph node")]
    InterleavedLogicalMessage { key: LogicalMessageKey },

    #[error("logical assistant message {key:?} has conflicting {field} values")]
    LogicalMessageConflict {
        key: LogicalMessageKey,
        field: &'static str,
    },

    #[error("token usage overflow while aggregating {field}")]
    UsageOverflow { field: &'static str },

    #[error("duplicate tool call id {tool_use_id}")]
    DuplicateToolCall { tool_use_id: String },

    #[error("duplicate tool result id {tool_use_id}")]
    DuplicateToolResult { tool_use_id: String },

    #[error("tool result {tool_use_id} has no tool call on the active chain")]
    OrphanToolResult { tool_use_id: String },

    #[error("terminal logical assistant message {key:?} has no text block")]
    TerminalMessageMissingText { key: LogicalMessageKey },

    #[error("file identity changed while applying a read: expected {expected:?}, got {actual:?}")]
    FileIdentityMismatch {
        expected: FileIdentity,
        actual: FileIdentity,
    },
}

fn row_suffix(row_uuid: &Option<String>) -> String {
    row_uuid
        .as_ref()
        .map(|uuid| format!(" (row {uuid})"))
        .unwrap_or_default()
}
