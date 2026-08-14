use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParseMode {
    Strict,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceLocation {
    pub line: u64,
    pub byte_offset: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RowScope {
    Main,
    Sidechain,
    Team,
    Meta,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommonFields {
    pub uuid: Option<String>,
    pub parent_uuid: Option<String>,
    pub session_id: Option<String>,
    pub scope: RowScope,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParsedRow {
    pub source: SourceLocation,
    pub common: CommonFields,
    pub kind: RowKind,
    /// The complete original object. Unknown fields are never discarded.
    pub raw: Value,
}

impl ParsedRow {
    /// Whether this row is one the `turn_duration` marker measurement counts as
    /// capable of changing a committed [`TranscriptAnalysis`].
    ///
    /// Observation only: no parse, ingest, or completion decision reads it. It
    /// backs `TurnTimings::post_turn_duration_row_observed_at_ms`, whose job is
    /// to say whether anything the analysis reads arrived after the marker was
    /// observed.
    ///
    /// Structural and deliberately conservative. It asks whether the engine
    /// admits rows of this kind and scope *anywhere* — not whether this
    /// particular row altered this particular analysis — because a false
    /// "something followed" only declines to justify a faster completion path,
    /// while a false "nothing followed" would justify an unsound one.
    ///
    /// Only the enumerated metadata records are excluded, and only because the
    /// engine provably drops them before analysis: they are filtered out of the
    /// graph in `strict_active_indices` and raise no warning. Unknown row types
    /// count in every scope, because they are rendered as a warning wherever
    /// they appear and are fatal on the active chain. Everything else counts
    /// while it is on the main chain or a sidechain, the two scopes the engine
    /// reads.
    #[must_use]
    pub fn is_analysis_changing(&self) -> bool {
        match &self.kind {
            RowKind::Metadata { .. } => false,
            RowKind::Unknown { .. } => true,
            _ => matches!(self.common.scope, RowScope::Main | RowScope::Sidechain),
        }
    }

    /// Whether this row is the main-chain `turn_duration` marker.
    ///
    /// Observation only, and it must agree with
    /// [`TranscriptAnalysis::turn_duration_seen`], which the engine derives from
    /// the same subtype on the active parent chain. This predicate looks at one
    /// row instead of a chain, so it can also fire for a main-scope
    /// `turn_duration` row that is not reachable from the acknowledged prompt.
    /// That is the safe direction: it opens the "did anything follow?" window
    /// earlier and stamps the marker earlier, both of which can only make the
    /// drain look more necessary than it is.
    #[must_use]
    pub fn is_turn_duration_marker(&self) -> bool {
        self.common.scope == RowScope::Main
            && matches!(
                &self.kind,
                RowKind::System(system) if system.subtype.as_deref() == Some("turn_duration")
            )
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RowKind {
    TypedUser {
        prompt: String,
        prompt_id: Option<String>,
    },
    UserToolResults {
        results: Vec<ToolResultBlock>,
    },
    UserOther,
    Assistant(AssistantFragment),
    /// Claude-injected context that participates in the active parent chain
    /// but is never trusted as a prompt, result, tool call, or usage source.
    Attachment {
        attachment_type: String,
    },
    System(SystemRow),
    Metadata {
        record_type: String,
    },
    Unknown {
        declared_type: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SystemRow {
    pub subtype: Option<String>,
}

impl SystemRow {
    /// The system subtypes whose own payload proves the turn is *over*.
    /// Membership is *earned* in `JsonlParser::parse_system`, which admits each
    /// subtype only after the row's payload proves it carries no semantics and
    /// that no further model output can follow it. Every member therefore opens
    /// a trailing zone on the chain: any semantic row appearing after one is
    /// schema drift.
    #[must_use]
    pub fn is_proven_inert_marker(&self) -> bool {
        matches!(
            self.subtype.as_deref(),
            Some("turn_duration" | "stop_hook_summary")
        )
    }

    /// `api_error` is the one admitted subtype whose payload proves the
    /// *opposite* of inertness. Claude writes one row per transport retry and
    /// then usually succeeds, so the row is ordinary and must not fail the turn
    /// -- but it means a retry is in flight, so it opens no trailing zone (a
    /// semantic row after it is the retry succeeding, which is normal) and it
    /// can never be a terminal leaf.
    #[must_use]
    pub fn is_retry_in_flight_marker(&self) -> bool {
        matches!(self.subtype.as_deref(), Some("api_error"))
    }

    /// The strict allowlist of system subtypes admitted onto the active parent
    /// chain, for either reason above. Everything not admitted here stays
    /// drift. The remaining wild subtype population is dominated by rows a
    /// completion authority must *not* ignore (`compact_boundary` deliberately
    /// breaks the chain, `model_refusal_fallback` records a mid-turn model
    /// substitution), so reject-by-default is the invariant, not an oversight.
    #[must_use]
    pub fn is_admitted_on_active_chain(&self) -> bool {
        self.is_proven_inert_marker() || self.is_retry_in_flight_marker()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AssistantFragment {
    pub message_id: Option<String>,
    pub request_id: Option<String>,
    pub model: Option<String>,
    pub blocks: Vec<ContentBlock>,
    pub stop_reason: Option<String>,
    pub usage: Option<UsageSnapshot>,
    pub is_api_error: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    Unknown {
        declared_type: Option<String>,
        raw: Value,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolResultBlock {
    pub tool_use_id: String,
    pub content: Value,
    pub is_error: Option<bool>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UsageSnapshot {
    pub tokens: TokenUsage,
    /// Full usage payload for compatibility diagnostics and future schema support.
    pub raw: Value,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct UsageTotals {
    pub tokens: TokenUsage,
    pub model_calls_with_usage: u64,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(tag = "source", content = "value", rename_all = "snake_case")]
pub enum LogicalMessageKey {
    MessageId(String),
    RequestId(String),
    RowUuid(String),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    StopSequence,
    ToolUse,
    PauseTurn,
    Refusal,
    Unknown(String),
}

impl StopReason {
    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value {
            "end_turn" => Self::EndTurn,
            "max_tokens" => Self::MaxTokens,
            "stop_sequence" => Self::StopSequence,
            "tool_use" => Self::ToolUse,
            "pause_turn" => Self::PauseTurn,
            "refusal" => Self::Refusal,
            other => Self::Unknown(other.to_owned()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LogicalAssistantMessage {
    pub key: LogicalMessageKey,
    pub row_uuids: Vec<String>,
    pub model: Option<String>,
    pub blocks: Vec<ContentBlock>,
    pub stop_reason: Option<StopReason>,
    pub usage: Option<UsageSnapshot>,
    pub is_api_error: bool,
    pub first_ordinal: u64,
    pub last_ordinal: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PromptAcknowledgement {
    pub row_uuid: String,
    pub prompt_id: Option<String>,
    pub ordinal: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolRecord {
    pub tool_use_id: String,
    pub name: String,
    pub input: Value,
    pub result: Option<ToolResultBlock>,
    pub order: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalOutcome {
    Completed,
    MaxTokens,
    Refused,
    ApiError,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FinalTurn {
    pub outcome: TerminalOutcome,
    pub message_key: LogicalMessageKey,
    pub stop_reason: Option<StopReason>,
    pub final_text: String,
    pub final_text_blocks: Vec<String>,
    pub model: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TurnStatus {
    AwaitingPromptAcknowledgement,
    Running {
        latest_stop_reason: Option<StopReason>,
    },
    Terminal(FinalTurn),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EngineWarning {
    UnknownRow {
        ordinal: u64,
        declared_type: Option<String>,
    },
    UnknownContentBlock {
        message: LogicalMessageKey,
        declared_type: Option<String>,
    },
    ConflictingUsage {
        message: LogicalMessageKey,
    },
    OrphanToolResult {
        tool_use_id: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TranscriptAnalysis {
    pub status: TurnStatus,
    pub acknowledgement: Option<PromptAcknowledgement>,
    pub active_chain: Vec<String>,
    pub messages: Vec<LogicalAssistantMessage>,
    pub tools: Vec<ToolRecord>,
    pub usage: UsageTotals,
    pub sidechain_usage: UsageTotals,
    pub combined_usage: UsageTotals,
    pub turn_duration_seen: bool,
    /// True when the active chain carries a `stop_hook_summary` row, which the
    /// parser admitted only after proving `preventedContinuation:false`. It is
    /// reported rather than merely tolerated: in transcript mode it is the only
    /// evidence that a caller-installed Stop hook ran inside a pmux turn.
    pub stop_hook_summary_seen: bool,
    /// How many `api_error` rows sit on the active chain, i.e. how many
    /// transport retries Claude logged inside this turn. Counted rather than
    /// flagged because one network incident emits a ladder of rows: the count is
    /// what lets an operator tell "this turn was slow because Claude retried
    /// eight times" from "pmux was slow". It is rendered in default output, so
    /// the retry is never merely detected.
    pub api_error_retries_seen: u64,
    /// How many rows this turn appended on a sidechain, of ANY kind.
    ///
    /// Counted rather than inferred from [`Self::sidechain_usage`], because a
    /// sidechain row is not obliged to carry usage: the `user` row that opens a
    /// `Task`, a fragment whose uuid is not yet flushed and a row whose parent
    /// chain is not yet reconstructible all contribute zero tokens and are all
    /// evidence that a subagent ran. On a cell launched with its tool surface
    /// denied a `Task` is unreachable, so a non-zero count there means the
    /// launch bundle did not take effect and the isolation claim is false --
    /// which is a refusal, not a warning. See `pool::Pool::commit`.
    pub sidechain_rows: u64,
    pub warnings: Vec<EngineWarning>,
}
