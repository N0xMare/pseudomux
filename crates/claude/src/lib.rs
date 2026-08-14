//! Pure transcript semantics for an interactive Claude Code driver.
//!
//! The crate deliberately has no PTY, filesystem watcher, or async runtime
//! dependency. [`TranscriptCursor`] turns offset-addressed file reads into complete
//! JSONL records. [`TranscriptEngine`] correlates those records to a prompt armed
//! at the caller's pre-injection EOF and derives logical messages, tools, usage,
//! and a conservative terminal candidate.

mod composer;
mod cursor;
mod engine;
mod error;
mod locator;
mod model;
mod parser;

pub use composer::{
    COMPOSER_LINE_CONTINUATION, COMPOSER_MODE_PREFIXES, COMPOSER_REWRITTEN_CHARACTERS,
    ComposerRefusal, ComposerRenderProof, composer_refusal, composer_render_proof,
    composer_submitted_text, is_ignorable_prompt_prefix, is_refused_wherever_it_stands,
    is_trimmed_from_the_end,
};
pub use cursor::{
    CompleteLine, CursorChange, CursorObservation, CursorUpdate, FileIdentity, FileMetadata,
    MAX_TRANSCRIPT_LINE_BYTES, TranscriptCursor,
};
pub use engine::{IngestOutcome, TranscriptAnalysisWork, TranscriptEngine, normalize_prompt};
pub use error::TranscriptError;
pub use locator::{LocatedTranscript, TranscriptLocationError, TranscriptLocator};
pub use model::{
    AssistantFragment, CommonFields, ContentBlock, EngineWarning, FinalTurn,
    LogicalAssistantMessage, LogicalMessageKey, ParseMode, ParsedRow, PromptAcknowledgement,
    RowKind, RowScope, SourceLocation, StopReason, SystemRow, TerminalOutcome, TokenUsage,
    ToolRecord, ToolResultBlock, TranscriptAnalysis, TurnStatus, UsageSnapshot, UsageTotals,
};
pub use parser::JsonlParser;
