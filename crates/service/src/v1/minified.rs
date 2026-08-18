//! The per-turn admissibility check for the minified (Path B) cell's fast path.
//!
//! Path B runs the Claude Code TUI with no tool surface and clears context
//! between turns by typing `/clear`. Its faster completion proof is calibrated
//! against that shape and *only* that shape. This module is the guard that
//! decides, for one turn, whether the turn pmux actually observed is the turn
//! the calibration assumed.
//!
//! # THE LAUNCH BUNDLE
//!
//! Every argument a Path B mint's child receives, and the whole of it:
//!
//! BUNDLE: `--session-id`, `--model`, `--effort`, `--permission-mode`,
//! `--disallowedTools`, `--strict-mcp-config`, `--system-prompt-file`
//!
//! That list is not prose. `stateless::tests::the_documented_minified_launch_bundle_is_the_argv_a_mint_emits`
//! parses it out of this file and compares it, spelling for spelling, against
//! the argv [`crate::stateless::launch_request_for`] actually produces -- so a
//! flag added to or removed from the launch turns this paragraph red rather
//! than leaving it quietly wrong.
//!
//! It says so because it was quietly wrong. This paragraph used to name
//! `--strict-mcp-config` and `--safe-mode` as shipped. Neither was ever
//! emitted: the first was retracted in `docs/path-b.md` §2.2 on a
//! descendant-process inventory that cannot observe an HTTP endpoint, and a
//! 2.1.226 cell was MEASURED reaching the operator's account connector list at
//! `https://api.anthropic.com/v1/mcp_servers` because of it. The flag is now
//! passed (see [`crate::claude_launch::MINIFIED_CELL_FLAGS`]); `--safe-mode` is
//! not, and the same constant records why.
//!
//! The governing asymmetry: returning before the work is done is unacceptable,
//! refusing to return is merely bad. So every check here fails towards the
//! slower proof. A refusal costs one turn the ~200ms the fast path would have
//! saved; a missed refusal would commit a truncated turn. The two are not
//! comparable, and nothing in this module trades the second for the first.
//!
//! Scope, stated precisely. This is a **pure predicate over already-published
//! data**: the committed [`TranscriptAnalysis`], the [`TurnTimings`] the actor
//! is about to publish, and one sticky terminal observation. It reads no files,
//! takes no clock, and decides nothing about whether the turn *succeeded* --
//! only about which proof is allowed to finish it. A refusal must never fail a
//! turn.
//!
//! Zero silent detection. Every check that refuses is reported, not just the
//! first, and each carries the observation that refused it. A fast path that
//! declined for an unattributable reason is indistinguishable from one that
//! declined for a reason nobody has noticed yet, and this project treats an
//! undiagnosable detection as a defect in its own right.

use pseudomux_claude::{ContentBlock, StopReason, TranscriptAnalysis, TurnStatus, UsageTotals};
use pseudomux_protocol::v1::{TimestampMs, TurnTimings};

use super::backend::LINUX_MINIFIED_POST_ANSWER_ARRIVAL_MAX_MS;

/// How long the transcript must have been unchanged before a minified-cell turn
/// that passed every check may commit.
///
/// This is the only quantity Path B's fast path actually spends. It is CHOSEN,
/// not measured, and it is the one number here that a live run must confirm
/// before Path B is worth selecting for latency. It is stated with its own
/// counter-evidence, because a constant whose risk is recorded somewhere else is
/// a constant that gets tuned by someone who never reads the somewhere else.
///
/// Against it: `TURN_DURATION_DRAIN_FLOOR_MS` (250ms) is deliberately an order
/// of magnitude above the only near post-marker gap ever observed (25ms), and
/// this floor is only 2x it. For it: every observed post-marker row in that
/// corpus was a harness-injected `<task-notification>` user row, and the
/// minified cell has no harness to inject one -- no tools, no MCP, no hooks, no
/// skills, no CLAUDE.md. That is an argument from the launch bundle, not a
/// measurement, which is exactly why check 10 exists: a turn where a row did
/// arrive after the marker refuses on its own evidence rather than on this
/// constant being right.
///
/// Linux/x86_64 minified ground truth sits just under this floor: the
/// assistant-to-`turn_duration` gap maxed at 46ms over 47 unique marked turns
/// (`LINUX_MINIFIED_POST_ANSWER_ARRIVAL_MAX_MS`), and no row arrived after the
/// marker. 50ms therefore covers the measured linux arrival; it is still
/// CHOSEN as a fast-path drain, not re-derived from that 46.
///
/// The protections that do not depend on it: the cell must be explicitly
/// selected on a calibrated host, ready-prompt and terminal-quiet stay in the
/// conjunction unconditionally, the confirming re-poll still has to come back
/// with an unmoved cursor and no rows, and any one of the ten checks failing
/// returns the turn to the Full cell's drain.
pub const MINIFIED_FAST_PATH_DRAIN_FLOOR_MS: u64 = 50;

/// The linux minified arrival max sits inside this floor. Lowering the fast
/// path to or below 46ms would commit at the measured marker on this host.
const _: () = assert!(
    LINUX_MINIFIED_POST_ANSWER_ARRIVAL_MAX_MS < MINIFIED_FAST_PATH_DRAIN_FLOOR_MS,
    "the minified fast-path drain must still cover the linux minified post-answer max"
);

/// The drain one turn owes, given what the Full cell would have required and
/// whether this turn earned the shorter proof.
///
/// `min` rather than a bare substitution, for the same reason
/// `graduated_drain_ms` uses one: an operator who configured a drain below this
/// floor has already asked for something shorter for every turn, and passing
/// ten checks must never be the reason pmux waits longer than it was told to.
/// A refused turn owes exactly what the Full cell owed -- not more. The
/// fallback is a return to the ordinary proof, not a penalty.
#[must_use]
pub const fn minified_drain_ms(full_drain_ms: u64, admissible: bool) -> u64 {
    if admissible && MINIFIED_FAST_PATH_DRAIN_FLOOR_MS < full_drain_ms {
        MINIFIED_FAST_PATH_DRAIN_FLOOR_MS
    } else {
        full_drain_ms
    }
}

/// Everything the fast-path decision is allowed to read for one turn.
///
/// Borrowed rather than owned: the actor already holds all of it at commit
/// time, and copying it would invite the checks to drift from the values that
/// were actually published.
#[derive(Clone, Copy, Debug)]
pub struct MinifiedTurnObservations<'a> {
    /// The committed analysis for this turn -- the same value that builds the
    /// `TurnResult`, never a mid-turn snapshot.
    pub analysis: &'a TranscriptAnalysis,
    /// The timings about to be published for this turn. Only the arrival-order
    /// pair is read; it is taken from the published struct so the check can
    /// never disagree with what the operator sees.
    pub timings: &'a TurnTimings,
    /// Whether the terminal reported *any* `NeedsInput` screen at *any* point
    /// in this turn.
    ///
    /// STICKY for the whole turn, deliberately. The actor's `active_needs_input`
    /// is a live state that clears when the modal goes away, so reading it at
    /// commit time would answer "is a modal on screen now?" -- a question whose
    /// answer is almost always no, including on the turn where a permission
    /// prompt appeared and was dismissed. The calibrated Path B turn never
    /// shows a modal at all, so the sticky form is the one that matches the
    /// assumption.
    ///
    /// This is also the only detector for the one Path B property that was
    /// never measured: that `--permission-mode dontAsk` survives `/clear`. If
    /// it does not, the escaped permission prompt shows up here. Detect, do not
    /// assume.
    pub needs_input_observed: bool,
}

/// One reason the fast path was refused for one turn.
///
/// Each variant carries the observation that refused it, because "the fast path
/// declined" is not an operator-actionable statement and "a `Task` subagent ran,
/// so a sidechain appeared" is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FastPathRefusal {
    /// Check 1. A tool ran. Path B is launched with no tool surface, so a
    /// correlated tool record means the launch bundle did not take effect.
    ToolRecordsPresent { tool_count: usize },
    /// Check 2. A raw `ToolUse` content block exists on some message.
    ///
    /// Deliberately redundant with [`Self::ToolRecordsPresent`]: that check
    /// reads the *correlated* record the engine built, this one reads the raw
    /// block the parser emitted. A parser or correlation change that silently
    /// emptied `tools` cannot also empty the blocks, so the redundancy is what
    /// keeps a schema change from quietly widening the fast path.
    ToolUseBlockPresent { message_index: usize, name: String },
    /// Check 3. A sidechain carried model usage, which means a `Task` subagent
    /// ran -- a shape the fast path was never calibrated against.
    SidechainActivity { usage: UsageTotals },
    /// Check 4. The turn took a transport-retry path. `api_error` rows are
    /// ordinary and must not fail a turn, but the retry ladder's timing was
    /// never calibrated, so it is not eligible for the shorter proof.
    ApiErrorRetriesSeen { retries: u64 },
    /// Check 5. The engine raised warnings, i.e. schema drift. Whatever the
    /// analysis missed is exactly what the fast path would be trusting.
    EngineWarningsPresent { warning_count: usize },
    /// Check 6. A `Stop` hook ran inside the turn.
    ///
    /// A Path B mint passes no `--settings` and its private root has no
    /// `settings.json`, so nothing the cell was launched with declares a Stop
    /// hook. This variant used to say the observation meant "`--safe-mode`
    /// leaked" -- a flag pmux has never passed and so cannot have failed to
    /// hold. The predicate was right and only the sentence was wrong, which is
    /// why the sentence now names the settings chain the cell actually has.
    ///
    /// It stays a refusal, and the reachable source is the reason: managed
    /// (policy) settings live at `/Library/Application Support/ClaudeCode/`,
    /// OUTSIDE the private root, and pmux neither reads nor suppresses them.
    /// So a Stop hook is not structurally unreachable here; it is merely not
    /// something the launch asked for, and a turn that saw one is a turn the
    /// calibration did not cover.
    StopHookObserved,
    /// Check 7. No `turn_duration` marker on the active chain. The marker *is*
    /// the fast path's proof; without it there is nothing to complete on.
    TurnDurationMarkerAbsent,
    /// Check 8. The final message did not stop with `end_turn`. `tool_use` and
    /// `pause_turn` mean the turn is not over at all; `max_tokens` and
    /// `refusal` mean it ended through a path the calibration never covered.
    StopReasonNotEndTurn { observed: Option<StopReason> },
    /// Check 9. A `NeedsInput` screen was observed at some point this turn.
    NeedsInputObserved,
    /// Check 10. An analysis-changing row arrived strictly *after* the batch
    /// carrying the `turn_duration` marker. On this turn, completing at the
    /// marker would have dropped that row.
    ///
    /// This is what makes Path B self-falsifying in production: the fast path
    /// carries its own counter-evidence detector, and the turn that would have
    /// been wrong is the turn that refuses.
    LateAnalysisChangingRow { observed_at_ms: TimestampMs },
    /// Not one of the ten. A totality guard: the checks describe a *finished*
    /// turn, and asking them about an unfinished one has no defined answer. It
    /// refuses rather than defaulting to admissible, because the only unsafe
    /// direction here is silently admitting.
    TurnNotTerminal,
}

impl FastPathRefusal {
    /// A stable slug for logs, metrics, and diagnostics.
    ///
    /// Stable across payload changes: an operator correlating "why did Path B
    /// stop being fast last Tuesday" is grouping by this, so it must not move
    /// when a variant gains a field.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::ToolRecordsPresent { .. } => "tool_records_present",
            Self::ToolUseBlockPresent { .. } => "tool_use_block_present",
            Self::SidechainActivity { .. } => "sidechain_activity",
            Self::ApiErrorRetriesSeen { .. } => "api_error_retries_seen",
            Self::EngineWarningsPresent { .. } => "engine_warnings_present",
            Self::StopHookObserved => "stop_hook_observed",
            Self::TurnDurationMarkerAbsent => "turn_duration_marker_absent",
            Self::StopReasonNotEndTurn { .. } => "stop_reason_not_end_turn",
            Self::NeedsInputObserved => "needs_input_observed",
            Self::LateAnalysisChangingRow { .. } => "late_analysis_changing_row",
            Self::TurnNotTerminal => "turn_not_terminal",
        }
    }

    /// A one-line operator-facing account of what was observed.
    ///
    /// Says what pmux saw, not what pmux did about it: the caller already knows
    /// the fast path was refused, and the thing it cannot reconstruct is the
    /// observation.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::ToolRecordsPresent { tool_count } => {
                format!("{tool_count} tool record(s) on a cell launched with no tool surface")
            }
            Self::ToolUseBlockPresent {
                message_index,
                name,
            } => format!("message {message_index} carries a tool_use block for `{name}`"),
            Self::SidechainActivity { usage } => format!(
                "sidechain carried {} model call(s), so a Task subagent ran",
                usage.model_calls_with_usage
            ),
            Self::ApiErrorRetriesSeen { retries } => {
                format!("{retries} api_error retry row(s) on the active chain")
            }
            Self::EngineWarningsPresent { warning_count } => {
                format!("{warning_count} engine warning(s), i.e. schema drift")
            }
            Self::StopHookObserved => {
                "a stop_hook_summary row is on the active chain, which this cell's launch \
                 declared no settings source for"
                    .to_owned()
            }
            Self::TurnDurationMarkerAbsent => {
                "no turn_duration marker on the active chain".to_owned()
            }
            Self::StopReasonNotEndTurn { observed } => observed.as_ref().map_or_else(
                || "the final message reported no stop_reason".to_owned(),
                |reason| format!("the final message stopped with {reason:?}, not EndTurn"),
            ),
            Self::NeedsInputObserved => {
                "a NeedsInput screen was observed during this turn".to_owned()
            }
            Self::LateAnalysisChangingRow { observed_at_ms } => format!(
                "an analysis-changing row arrived at {observed_at_ms} ms, \
                 strictly after the turn_duration marker"
            ),
            Self::TurnNotTerminal => {
                "the turn had not reached a terminal analysis when the fast path was evaluated"
                    .to_owned()
            }
        }
    }
}

/// The outcome of the ten checks for one turn.
///
/// A list, not a first-failure: several checks fire together on exactly the
/// interesting turns (a leaked tool surface trips 1, 2, and 8 at once), and an
/// operator who is shown only the first has to re-run the incident to see the
/// rest.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FastPathVerdict {
    refusals: Vec<FastPathRefusal>,
}

impl FastPathVerdict {
    /// Whether the shorter proof may finish this turn.
    ///
    /// Admissible means every check passed. There is no partial credit: the
    /// calibration is a conjunction of assumptions, and any one of them being
    /// false is enough to make the measured timing inapplicable.
    #[must_use]
    pub fn is_admissible(&self) -> bool {
        self.refusals.is_empty()
    }

    /// Every check that refused, in check order.
    #[must_use]
    pub fn refusals(&self) -> &[FastPathRefusal] {
        &self.refusals
    }

    /// The refusal codes, for a log line or a metric label set.
    pub fn codes(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.refusals.iter().map(FastPathRefusal::code)
    }
}

/// Evaluates the ten per-turn constraint checks for the minified (Path B) cell.
///
/// Pure: same observations, same verdict, no side effects, no I/O, no clock. It
/// is safe to call on any turn, including turns from the Full cell -- those
/// simply refuse, which is the correct answer for them.
///
/// The caller's obligation is the one thing this function cannot enforce: a
/// refusal must fall back to the Full cell's slower proof **for that turn
/// only**, and must never fail the turn or disable the fast path for the
/// session. A refusal is a statement about one turn's observations, not about
/// the cell's health.
#[must_use]
pub fn evaluate_minified_fast_path(observations: MinifiedTurnObservations<'_>) -> FastPathVerdict {
    let MinifiedTurnObservations {
        analysis,
        timings,
        needs_input_observed,
    } = observations;
    let mut refusals = Vec::new();

    // 1. The correlated view: did the engine record a tool actually running?
    if !analysis.tools.is_empty() {
        refusals.push(FastPathRefusal::ToolRecordsPresent {
            tool_count: analysis.tools.len(),
        });
    }

    // 2. The raw view of the same fact. Kept separate on purpose; see
    //    `FastPathRefusal::ToolUseBlockPresent`.
    if let Some((message_index, name)) = analysis
        .messages
        .iter()
        .enumerate()
        .flat_map(|(index, message)| {
            message.blocks.iter().filter_map(move |block| match block {
                ContentBlock::ToolUse { name, .. } => Some((index, name.clone())),
                _ => None,
            })
        })
        .next()
    {
        refusals.push(FastPathRefusal::ToolUseBlockPresent {
            message_index,
            name,
        });
    }

    // 3. Any sidechain usage at all means a Task subagent ran. Compared against
    //    the default rather than against a call count, so a future field that
    //    records sidechain activity without model calls still refuses.
    if analysis.sidechain_usage != UsageTotals::default() {
        refusals.push(FastPathRefusal::SidechainActivity {
            usage: analysis.sidechain_usage.clone(),
        });
    }

    // 4. Retries are ordinary and never fail a turn -- but the turn took a
    //    timing path the calibration never saw.
    if analysis.api_error_retries_seen != 0 {
        refusals.push(FastPathRefusal::ApiErrorRetriesSeen {
            retries: analysis.api_error_retries_seen,
        });
    }

    // 5. Schema drift. The fast path trusts the analysis; a warning says the
    //    analysis is the thing in question.
    if !analysis.warnings.is_empty() {
        refusals.push(FastPathRefusal::EngineWarningsPresent {
            warning_count: analysis.warnings.len(),
        });
    }

    // 6. A Stop hook ran, and the launch named no settings source that declares
    //    one -- so this is not the cell the calibration covered, whatever it was
    //    launched as.
    if analysis.stop_hook_summary_seen {
        refusals.push(FastPathRefusal::StopHookObserved);
    }

    // 7. The proof itself.
    if !analysis.turn_duration_seen {
        refusals.push(FastPathRefusal::TurnDurationMarkerAbsent);
    }

    // 8. Only `end_turn` is calibrated. `tool_use`/`pause_turn` mean the turn is
    //    not over; `max_tokens`/`refusal`/anything unknown mean it ended some
    //    other way.
    match &analysis.status {
        TurnStatus::Terminal(final_turn) => {
            if final_turn.stop_reason != Some(StopReason::EndTurn) {
                refusals.push(FastPathRefusal::StopReasonNotEndTurn {
                    observed: final_turn.stop_reason.clone(),
                });
            }
        }
        TurnStatus::AwaitingPromptAcknowledgement | TurnStatus::Running { .. } => {
            refusals.push(FastPathRefusal::TurnNotTerminal);
        }
    }

    // 9. Sticky for the whole turn; see `needs_input_observed`.
    if needs_input_observed {
        refusals.push(FastPathRefusal::NeedsInputObserved);
    }

    // 10. The self-falsifying check. Read straight off the published pair, so
    //     the refusal and the operator's evidence for it are the same number.
    if let Some(observed_at_ms) = timings.post_turn_duration_row_observed_at_ms {
        refusals.push(FastPathRefusal::LateAnalysisChangingRow { observed_at_ms });
    }

    FastPathVerdict { refusals }
}

#[cfg(test)]
mod tests {
    use pseudomux_claude::{
        AssistantFragment, EngineWarning, FinalTurn, LogicalAssistantMessage, LogicalMessageKey,
        PromptAcknowledgement, TerminalOutcome, TokenUsage, ToolRecord, ToolResultBlock,
    };
    use serde_json::json;

    use super::*;

    /// The calibrated Path B turn: one text-only assistant message that stopped
    /// with `end_turn`, a `turn_duration` marker, nothing after it, no tools, no
    /// sidechain, no retries, no warnings, no hook, no modal.
    ///
    /// Every test below mutates exactly one fact away from this, so a test that
    /// fires proves the check it names and nothing else.
    fn calibrated_analysis() -> TranscriptAnalysis {
        TranscriptAnalysis {
            status: TurnStatus::Terminal(FinalTurn {
                outcome: TerminalOutcome::Completed,
                message_key: LogicalMessageKey::MessageId("msg_1".to_owned()),
                stop_reason: Some(StopReason::EndTurn),
                final_text: "done".to_owned(),
                final_text_blocks: vec!["done".to_owned()],
                model: Some("claude-test".to_owned()),
            }),
            acknowledgement: Some(PromptAcknowledgement {
                row_uuid: "row-1".to_owned(),
                prompt_id: None,
                ordinal: 1,
            }),
            active_chain: vec!["row-1".to_owned(), "row-2".to_owned()],
            messages: vec![LogicalAssistantMessage {
                key: LogicalMessageKey::MessageId("msg_1".to_owned()),
                row_uuids: vec!["row-2".to_owned()],
                model: Some("claude-test".to_owned()),
                blocks: vec![ContentBlock::Text {
                    text: "done".to_owned(),
                }],
                stop_reason: Some(StopReason::EndTurn),
                usage: None,
                is_api_error: false,
                first_ordinal: 2,
                last_ordinal: 2,
            }],
            tools: Vec::new(),
            usage: UsageTotals::default(),
            sidechain_usage: UsageTotals::default(),
            combined_usage: UsageTotals::default(),
            turn_duration_seen: true,
            stop_hook_summary_seen: false,
            api_error_retries_seen: 0,
            sidechain_rows: 0,
            warnings: Vec::new(),
        }
    }

    /// Timings for a turn where the marker was observed and nothing followed it.
    fn calibrated_timings() -> TurnTimings {
        TurnTimings {
            submitted_at_ms: 1_000,
            prompt_acknowledged_at_ms: Some(1_050),
            terminal_candidate_at_ms: Some(1_400),
            completed_at_ms: 1_500,
            drain_ms: Some(100),
            last_transcript_activity_at_ms: Some(1_400),
            stop_hook_at_ms: None,
            turn_duration_observed_at_ms: Some(1_400),
            post_turn_duration_row_observed_at_ms: None,
        }
    }

    fn verdict(
        analysis: &TranscriptAnalysis,
        timings: &TurnTimings,
        needs_input: bool,
    ) -> FastPathVerdict {
        evaluate_minified_fast_path(MinifiedTurnObservations {
            analysis,
            timings,
            needs_input_observed: needs_input,
        })
    }

    /// The one shape the fast path exists for. If this ever refuses, Path B is
    /// silently permanently slow, which is the failure this case guards.
    #[test]
    fn the_calibrated_turn_is_admissible() {
        let verdict = verdict(&calibrated_analysis(), &calibrated_timings(), false);
        assert!(verdict.is_admissible(), "{:?}", verdict.refusals());
        assert!(verdict.refusals().is_empty());
    }

    /// Every check below asserts *exactly one* refusal, which is what makes it a
    /// test of that check in isolation rather than of the conjunction.
    fn sole_refusal(verdict: &FastPathVerdict) -> &FastPathRefusal {
        assert_eq!(verdict.refusals().len(), 1, "{:?}", verdict.refusals());
        assert!(!verdict.is_admissible());
        &verdict.refusals()[0]
    }

    #[test]
    fn check_1_a_correlated_tool_record_refuses() {
        let mut analysis = calibrated_analysis();
        analysis.tools.push(ToolRecord {
            tool_use_id: "toolu_1".to_owned(),
            name: "Read".to_owned(),
            input: json!({}),
            result: Some(ToolResultBlock {
                tool_use_id: "toolu_1".to_owned(),
                content: json!("ok"),
                is_error: None,
            }),
            order: 0,
        });

        let verdict = verdict(&analysis, &calibrated_timings(), false);
        assert_eq!(
            sole_refusal(&verdict),
            &FastPathRefusal::ToolRecordsPresent { tool_count: 1 }
        );
        assert_eq!(sole_refusal(&verdict).code(), "tool_records_present");
    }

    /// Check 2 must fire on the raw block even when `tools` is empty -- that
    /// combination is precisely the correlation break the redundancy exists to
    /// catch, and it is why this test does not also populate `tools`.
    #[test]
    fn check_2_a_raw_tool_use_block_refuses_even_with_no_correlated_record() {
        let mut analysis = calibrated_analysis();
        analysis.messages[0].blocks.push(ContentBlock::ToolUse {
            id: "toolu_1".to_owned(),
            name: "Bash".to_owned(),
            input: json!({ "command": "ls" }),
        });
        assert!(analysis.tools.is_empty());

        let verdict = verdict(&analysis, &calibrated_timings(), false);
        assert_eq!(
            sole_refusal(&verdict),
            &FastPathRefusal::ToolUseBlockPresent {
                message_index: 0,
                name: "Bash".to_owned(),
            }
        );
        assert_eq!(sole_refusal(&verdict).code(), "tool_use_block_present");
    }

    #[test]
    fn check_3_sidechain_usage_refuses() {
        let mut analysis = calibrated_analysis();
        analysis.sidechain_usage = UsageTotals {
            tokens: TokenUsage {
                input_tokens: 12,
                output_tokens: 3,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
            model_calls_with_usage: 1,
        };

        let verdict = verdict(&analysis, &calibrated_timings(), false);
        let FastPathRefusal::SidechainActivity { usage } = sole_refusal(&verdict) else {
            panic!("expected a sidechain refusal, got {:?}", verdict.refusals());
        };
        assert_eq!(usage.model_calls_with_usage, 1);
        assert_eq!(sole_refusal(&verdict).code(), "sidechain_activity");
    }

    /// A sidechain that recorded no model calls but did record tokens still
    /// refuses: the check is "is this the default?", not "did it bill?".
    #[test]
    fn check_3_refuses_on_any_departure_from_the_default_not_only_on_call_count() {
        let mut analysis = calibrated_analysis();
        analysis.sidechain_usage = UsageTotals {
            tokens: TokenUsage {
                input_tokens: 1,
                ..TokenUsage::default()
            },
            model_calls_with_usage: 0,
        };

        let verdict = verdict(&analysis, &calibrated_timings(), false);
        assert_eq!(sole_refusal(&verdict).code(), "sidechain_activity");
    }

    #[test]
    fn check_4_an_api_error_retry_refuses() {
        let mut analysis = calibrated_analysis();
        analysis.api_error_retries_seen = 3;

        let verdict = verdict(&analysis, &calibrated_timings(), false);
        assert_eq!(
            sole_refusal(&verdict),
            &FastPathRefusal::ApiErrorRetriesSeen { retries: 3 }
        );
        assert_eq!(sole_refusal(&verdict).code(), "api_error_retries_seen");
    }

    #[test]
    fn check_5_an_engine_warning_refuses() {
        let mut analysis = calibrated_analysis();
        analysis.warnings.push(EngineWarning::UnknownRow {
            ordinal: 7,
            declared_type: Some("brand_new_row".to_owned()),
        });

        let verdict = verdict(&analysis, &calibrated_timings(), false);
        assert_eq!(
            sole_refusal(&verdict),
            &FastPathRefusal::EngineWarningsPresent { warning_count: 1 }
        );
        assert_eq!(sole_refusal(&verdict).code(), "engine_warnings_present");
    }

    #[test]
    fn check_6_a_stop_hook_summary_refuses() {
        let mut analysis = calibrated_analysis();
        analysis.stop_hook_summary_seen = true;

        let verdict = verdict(&analysis, &calibrated_timings(), false);
        assert_eq!(sole_refusal(&verdict), &FastPathRefusal::StopHookObserved);
        assert_eq!(sole_refusal(&verdict).code(), "stop_hook_observed");
    }

    #[test]
    fn check_7_a_missing_turn_duration_marker_refuses() {
        let mut analysis = calibrated_analysis();
        analysis.turn_duration_seen = false;

        let verdict = verdict(&analysis, &calibrated_timings(), false);
        assert_eq!(
            sole_refusal(&verdict),
            &FastPathRefusal::TurnDurationMarkerAbsent
        );
        assert_eq!(sole_refusal(&verdict).code(), "turn_duration_marker_absent");
    }

    #[test]
    fn check_8_a_stop_reason_other_than_end_turn_refuses() {
        for reason in [
            StopReason::ToolUse,
            StopReason::PauseTurn,
            StopReason::MaxTokens,
            StopReason::Refusal,
            StopReason::StopSequence,
            StopReason::Unknown("brand_new_reason".to_owned()),
        ] {
            let mut analysis = calibrated_analysis();
            let TurnStatus::Terminal(final_turn) = &mut analysis.status else {
                panic!("the calibrated analysis is terminal by construction");
            };
            final_turn.stop_reason = Some(reason.clone());

            let verdict = verdict(&analysis, &calibrated_timings(), false);
            assert_eq!(
                sole_refusal(&verdict),
                &FastPathRefusal::StopReasonNotEndTurn {
                    observed: Some(reason.clone()),
                },
                "stop_reason {reason:?} must refuse"
            );
            assert_eq!(sole_refusal(&verdict).code(), "stop_reason_not_end_turn");
        }
    }

    /// A terminal message with no `stop_reason` at all is not `end_turn` either.
    /// Absent evidence is not evidence of the calibrated shape.
    #[test]
    fn check_8_an_absent_stop_reason_refuses() {
        let mut analysis = calibrated_analysis();
        let TurnStatus::Terminal(final_turn) = &mut analysis.status else {
            panic!("the calibrated analysis is terminal by construction");
        };
        final_turn.stop_reason = None;

        let verdict = verdict(&analysis, &calibrated_timings(), false);
        assert_eq!(
            sole_refusal(&verdict),
            &FastPathRefusal::StopReasonNotEndTurn { observed: None }
        );
    }

    #[test]
    fn check_9_a_needs_input_screen_anywhere_in_the_turn_refuses() {
        let verdict = verdict(&calibrated_analysis(), &calibrated_timings(), true);
        assert_eq!(sole_refusal(&verdict), &FastPathRefusal::NeedsInputObserved);
        assert_eq!(sole_refusal(&verdict).code(), "needs_input_observed");
    }

    #[test]
    fn check_10_a_row_arriving_after_the_marker_refuses() {
        let mut timings = calibrated_timings();
        timings.post_turn_duration_row_observed_at_ms = Some(1_450);

        let verdict = verdict(&calibrated_analysis(), &timings, false);
        assert_eq!(
            sole_refusal(&verdict),
            &FastPathRefusal::LateAnalysisChangingRow {
                observed_at_ms: 1_450
            }
        );
        assert_eq!(sole_refusal(&verdict).code(), "late_analysis_changing_row");
    }

    /// The totality guard. Asking the ten checks about an unfinished turn has no
    /// defined answer, and the only unsafe default is "admissible".
    #[test]
    fn a_non_terminal_analysis_refuses_rather_than_defaulting_to_admissible() {
        for status in [
            TurnStatus::AwaitingPromptAcknowledgement,
            TurnStatus::Running {
                latest_stop_reason: None,
            },
        ] {
            let mut analysis = calibrated_analysis();
            analysis.status = status;

            let verdict = verdict(&analysis, &calibrated_timings(), false);
            assert_eq!(sole_refusal(&verdict), &FastPathRefusal::TurnNotTerminal);
        }
    }

    /// Several checks firing at once must all be reported. Showing only the
    /// first would make the operator re-run the incident to learn the rest,
    /// which is the silent-detection defect this module is built to avoid.
    #[test]
    fn concurrent_refusals_are_all_reported_and_all_attributed() {
        let mut analysis = calibrated_analysis();
        analysis.tools.push(ToolRecord {
            tool_use_id: "toolu_1".to_owned(),
            name: "Read".to_owned(),
            input: json!({}),
            result: None,
            order: 0,
        });
        analysis.messages[0].blocks.push(ContentBlock::ToolUse {
            id: "toolu_1".to_owned(),
            name: "Read".to_owned(),
            input: json!({}),
        });
        analysis.api_error_retries_seen = 1;
        analysis.turn_duration_seen = false;
        let mut timings = calibrated_timings();
        timings.post_turn_duration_row_observed_at_ms = Some(1_450);

        let verdict = verdict(&analysis, &timings, true);
        assert!(!verdict.is_admissible());
        assert_eq!(
            verdict.codes().collect::<Vec<_>>(),
            vec![
                "tool_records_present",
                "tool_use_block_present",
                "api_error_retries_seen",
                "turn_duration_marker_absent",
                "needs_input_observed",
                "late_analysis_changing_row",
            ]
        );
        for refusal in verdict.refusals() {
            assert!(!refusal.describe().is_empty());
        }
    }

    /// Zero silent detection, stated as a test: every variant has a distinct
    /// slug and a non-empty description, so no refusal can reach an operator as
    /// an unexplained "not fast today".
    #[test]
    fn every_refusal_variant_is_distinctly_attributable() {
        let all = [
            FastPathRefusal::ToolRecordsPresent { tool_count: 1 },
            FastPathRefusal::ToolUseBlockPresent {
                message_index: 0,
                name: "Read".to_owned(),
            },
            FastPathRefusal::SidechainActivity {
                usage: UsageTotals::default(),
            },
            FastPathRefusal::ApiErrorRetriesSeen { retries: 1 },
            FastPathRefusal::EngineWarningsPresent { warning_count: 1 },
            FastPathRefusal::StopHookObserved,
            FastPathRefusal::TurnDurationMarkerAbsent,
            FastPathRefusal::StopReasonNotEndTurn {
                observed: Some(StopReason::MaxTokens),
            },
            FastPathRefusal::NeedsInputObserved,
            FastPathRefusal::LateAnalysisChangingRow {
                observed_at_ms: 1_450,
            },
            FastPathRefusal::TurnNotTerminal,
        ];

        let mut codes: Vec<_> = all.iter().map(FastPathRefusal::code).collect();
        let total = codes.len();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), total, "refusal codes must be distinct");
        for refusal in &all {
            assert!(!refusal.describe().is_empty(), "{refusal:?} has no account");
        }
    }

    /// The shorter proof may only ever be shorter. A refused turn owes exactly
    /// what the Full cell owed, and an operator who configured a drain below
    /// the floor is never made to wait longer for having passed the checks.
    #[test]
    fn the_fast_path_floor_only_ever_lowers_the_required_drain() {
        assert_eq!(
            minified_drain_ms(250, true),
            MINIFIED_FAST_PATH_DRAIN_FLOOR_MS
        );
        assert_eq!(minified_drain_ms(250, false), 250);
        // At and below the floor the Full cell's requirement is returned
        // unchanged, in both directions.
        assert_eq!(
            minified_drain_ms(MINIFIED_FAST_PATH_DRAIN_FLOOR_MS, true),
            MINIFIED_FAST_PATH_DRAIN_FLOOR_MS
        );
        assert_eq!(minified_drain_ms(10, true), 10);
        assert_eq!(minified_drain_ms(10, false), 10);
        assert_eq!(minified_drain_ms(1, true), 1);
    }

    /// `AssistantFragment` is the raw parser shape behind
    /// `LogicalAssistantMessage`. Referenced here so a rename of the block enum
    /// on the parser side cannot leave check 2 compiling against a stale idea of
    /// what a tool call looks like.
    #[test]
    fn a_raw_assistant_fragment_carries_the_same_block_enum_check_2_reads() {
        let fragment = AssistantFragment {
            message_id: Some("msg_1".to_owned()),
            request_id: None,
            model: None,
            blocks: vec![ContentBlock::ToolUse {
                id: "toolu_1".to_owned(),
                name: "Read".to_owned(),
                input: json!({}),
            }],
            stop_reason: Some("tool_use".to_owned()),
            usage: None,
            is_api_error: false,
        };
        let mut analysis = calibrated_analysis();
        analysis.messages[0].blocks = fragment.blocks;

        let verdict = verdict(&analysis, &calibrated_timings(), false);
        assert_eq!(sole_refusal(&verdict).code(), "tool_use_block_present");
    }
}
