//! Production I/O adapters for the v1 session actor.
//!
//! These adapters deliberately keep terminal observations and transcript
//! semantics separate. The terminal can corroborate readiness and quiet, but
//! only parsed Claude JSONL rows can authorize a result.

use std::collections::{BTreeSet, VecDeque};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use pseudomux_claude::{
    CursorChange, FileIdentity, FileMetadata, JsonlParser, ParseMode, TranscriptCursor,
    TranscriptLocationError, TranscriptLocator,
};
use pseudomux_protocol::v1::{
    ClosePolicy, ErrorCode, MAX_SAFE_JSON_INTEGER, NeedsInput, NeedsInputKind, RECOMMENDATION_KEY,
    SessionId, TurnId,
};
use pseudomux_rmux::{
    CellColor, StyledCell, StyledScreen, TerminalBackendError, TerminalSession, TerminalSnapshot,
};
use serde_json::json;
use tokio::sync::Mutex;
use unicode_normalization::UnicodeNormalization;

use crate::v1::{
    DriverFailure, DriverResult, InterruptRecovery, MINIFIED_FAST_PATH_DRAIN_FLOOR_MS,
    POST_MARKER_CATCH_WINDOW_FLOOR_MS, TURN_DURATION_DRAIN_FLOOR_MS, TerminalControl,
    TerminalEvidence, TerminalScreenObservation, TranscriptArm, TranscriptBatch,
    TranscriptDrainEvidence, TranscriptPosition, TranscriptSource, post_marker_catch_window_ms,
};

const MAX_TRANSCRIPT_READ_BYTES: u64 = 64 * 1024 * 1024;
/// Maximum normalized prompt size accepted by the native service.
pub const MAX_PROMPT_BYTES: usize = 1024 * 1024;
// Local input admission only: this is bounded separately from (and by) the
// immutable turn deadline. It is not a model execution or billing timeout.
const INPUT_GATE_MAX_DURATION: Duration = Duration::from_secs(15);
/// The sampling grain of every screen proof in this file.
const TERMINAL_POLL_INTERVAL_MS: u64 = 25;
const TERMINAL_POLL_INTERVAL: Duration = Duration::from_millis(TERMINAL_POLL_INTERVAL_MS);

/// The corpus site label of each read this file classifies, named once here so
/// the recorder and the sweep that checks every read is recorded agree on the
/// spelling.
///
/// The module documentation on [`crate::screen_corpus`] argues for recording
/// from "every 25 ms for the length of every turn", and for a long time only the
/// input gate recorded anything at all: the per-turn polls -- the ones the modal
/// hang happened underneath -- were taken and discarded, which is what the
/// paragraph promising to keep them means when it outruns its own call sites.
/// [`tests::every_classified_read_is_recorded_to_the_screen_corpus`] derives the
/// set of reads from this file's own source and fails when one is added without
/// a recording.
const INPUT_GATE_PRE_PASTE_SITE: &str = "input_gate.pre_paste";
const INPUT_GATE_POST_PASTE_SITE: &str = "input_gate.post_paste";
const CONTROL_CHANNEL_SELECTION_SITE: &str = "control_channel.selection";
const COMPLETION_GATE_EVIDENCE_SITE: &str = "completion_gate.evidence";
const SCREEN_STABILITY_SITE: &str = "screen_stability.poll";
const TURN_MONITOR_SITE: &str = "turn_monitor.observe";
/// The recovery loop after an interrupt, which reads frames until one is
/// `Ready` or the recovery timeout runs out.
///
/// FOUND BY THE SWEEP, not by reading: this loop classified every frame it took
/// and recorded none of them, so the one recording a failed recovery most needs
/// -- what the pane was showing while it refused to come back -- was the one
/// nothing kept.
const INTERRUPT_RECOVERY_SITE: &str = "interrupt.recovery";
/// `native.rs`'s startup readiness poll, named here with the other six so one
/// module owns every site label.
pub(crate) const STARTUP_READINESS_SITE: &str = "startup.wait_until_ready";
/// How long a screen proof requires its subject to hold still.
///
/// ONE constant with FOUR meanings, which is the entire reason
/// [`POST_MARKER_CATCH_WINDOW_FLOOR_MS`] had to be written down: Gate 1's
/// stable-empty-editor window, Gate 2's post-paste render window, the commit
/// gate's screen-liveness window, and -- multiplied out with
/// [`TERMINAL_POLL_INTERVAL`] -- the commit loop's sampling period, which is
/// what decides how late a transcript row may arrive and still be read.
///
/// The first three are properties of a SCREEN. The fourth is a property of a
/// TRANSCRIPT and has no reason to be the same number; it is the same number by
/// accident. [`SCREEN_QUIET_FOR`] below is where that accident is checked, and
/// it is checked there rather than in a free-standing assertion so that the
/// value and its consequence cannot be edited apart.
///
/// It has been 250ms since `405fccd`, the initial commit, and no measurement
/// anywhere in this tree sizes the first three. What IS measured is the fourth.
const SCREEN_QUIET_FOR_MS: u64 = 250;

/// [`SCREEN_QUIET_FOR_MS`] as a [`Duration`], and the refusal
/// [`POST_MARKER_CATCH_WINDOW_FLOOR_MS`] exists to make possible: a screen
/// configuration that narrows the post-marker catch window below the largest
/// post-answer transcript arrival ever measured **does not compile**.
///
/// The assertions live inside this constant, and not beside it, for one reason:
/// this is the value they are about. A free-standing `const _` can be deleted on
/// its own, and rustc's dead-code pass does not even traverse one -- it reported
/// [`COMMIT_LOOP_SAMPLING_PERIOD_MS`] as never used while two assertions were
/// reading it. Here the guarantee is discharged by the definition of the number
/// that decides it.
///
/// BOTH shipped requirements are checked, not just the graduated floor, and the
/// minified one is the binding one. Below one period the window is twice the
/// period whatever the requirement is, so a minified-cell turn -- which owes
/// only [`MINIFIED_FAST_PATH_DRAIN_FLOOR_MS`] -- loses its margin first.
/// MEASURED at the boundary: at `SCREEN_QUIET_FOR_MS` = 194 the minified window
/// is exactly 438ms; at 193 it is 436 and this refuses; at **125** -- a value
/// this repository has actually measured for latency, and found to save 245ms on
/// the input gate with no test in `pseudomux-service` noticing -- the minified
/// window is **300ms**, below the 352ms row that really arrived on ordinal 70.
/// Asserting only the graduated floor would have admitted that: it passes at 125
/// with a 450ms window.
///
/// WHAT IT DOES NOT SEE, stated so it is not read as covering more than it does:
/// a change to WHERE the drain is evaluated rather than to how long the loop
/// takes. Deciding the drain from the confirming re-poll collapses the period
/// without touching either constant, and the six graduated-band tests in
/// `crates/service/tests/v1_actor.rs` are what refuse that.
const SCREEN_QUIET_FOR: Duration = {
    assert!(
        post_marker_catch_window_ms(TURN_DURATION_DRAIN_FLOOR_MS, COMMIT_LOOP_SAMPLING_PERIOD_MS)
            >= POST_MARKER_CATCH_WINDOW_FLOOR_MS,
        "the shipped screen constants narrow a marked turn's post-marker catch window below the \
         largest post-answer transcript arrival ever measured; see \
         POST_MARKER_CATCH_WINDOW_FLOOR_MS"
    );
    assert!(
        post_marker_catch_window_ms(
            MINIFIED_FAST_PATH_DRAIN_FLOOR_MS,
            COMMIT_LOOP_SAMPLING_PERIOD_MS
        ) >= POST_MARKER_CATCH_WINDOW_FLOOR_MS,
        "the shipped screen constants narrow a minified-cell turn's post-marker catch window below \
         the largest post-answer transcript arrival ever measured; see \
         POST_MARKER_CATCH_WINDOW_FLOOR_MS"
    );
    Duration::from_millis(SCREEN_QUIET_FOR_MS)
};
/// One iteration of the actor's commit loop while the turn is a terminal
/// candidate.
///
/// The iteration is: one screen observation (~1ms), one transcript poll (~1ms),
/// and one `completion_evidence` call, which reads and then re-proves
/// [`SCREEN_QUIET_FOR`] of screen quiet FROM SCRATCH at
/// [`TERMINAL_POLL_INTERVAL`] grain -- `wait_for_snapshot_stability` restarts
/// `stable_since` on entry, every iteration. The two ~1ms terms are dropped; the
/// quiet window plus one poll of overshoot is the period.
///
/// NOMINAL: it omits per-read overhead. That is the safe direction here, because
/// for a period at or above the drain requirement the catch window is exactly
/// twice the period and therefore increases with it -- so [`SCREEN_QUIET_FOR`]'s
/// assertions are evaluated against a period no larger than the real one.
/// MEASURED: this nominal 275ms predicts a 550ms window and the observed
/// `drain_ms` median over n=30 is 550.0.
const COMMIT_LOOP_SAMPLING_PERIOD_MS: u64 = SCREEN_QUIET_FOR_MS + TERMINAL_POLL_INTERVAL_MS;
/// How many RENDERED rows may sit below the cursor's row before the cursor
/// stops being correlatable with a composer.
///
/// MEASURED as two on both reviewed versions: all five
/// `crates/service/tests/fixtures/claude_2_1_70_*.txt` captures render exactly
/// two rows below the composer (a rule and the footer), and so did 85/85 live
/// empty-composer screens on Claude Code 2.1.220. Four is 2x that.
///
/// Measured from the last RENDERED row, not from the bottom of the grid. Ink
/// repaints from where the previous frame ended and leaves the remainder of the
/// grid literally blank, so the frame after a `/clear` is four rows tall at the
/// TOP of a 24-row screen -- MEASURED on 2.1.220: rule/composer/rule/footer at
/// rows 4-7, cursor at (5,2), rows 8-23 of length zero, byte-identical for 285s
/// across ~4,250 samples. Its distance to the grid bottom is 18, so measuring
/// there made a provably empty composer unfindable and refused the first turn
/// after every successful `/clear` with `PromptNotAcknowledged`.
///
/// This enforces the same measured fact against the same constant. What it drops
/// is the unstated assumption the old expression silently carried -- that Ink
/// always paints to the bottom of the grid -- and what it adds is one directly
/// observed assumption: rows past the frame are blank.
const MAX_RENDERED_ROWS_BELOW_ACTIVE_CURSOR: u16 = 4;
/// How long pmux waits for the transcript `/clear` opens before it refuses to
/// rebind.
///
/// MEASURED on Claude Code 2.1.220: the new file appears +39ms after Enter, and
/// it appears immediately rather than lazily. 2000ms is ~50x that, which absorbs
/// contention and a loaded filesystem without ever being near a real bound. It
/// is a refusal deadline, not a latency budget: a rebind runs between turns, so
/// waiting longer costs capacity and never turn latency.
const CLEAR_REBIND_TIMEOUT: Duration = Duration::from_millis(2_000);
const ROTATION_POLL_INTERVAL: Duration = Duration::from_millis(10);
/// Bound on one project directory's transcript listing. The retention policy for
/// a cleared cell keeps at most a few hundred abandoned transcripts per cwd, so
/// exceeding this means the directory is not the one this rebind believes it is
/// watching, and guessing which of 20,000 files is the successor is exactly the
/// move a completion authority must not make.
const MAX_ROTATION_DIRECTORY_ENTRIES: usize = 20_000;
/// Bound on the bytes read while looking for a candidate transcript's row 0.
const MAX_ROTATION_ANCHOR_BYTES: usize = 64 * 1024;
/// How many abandoned session ids one source remembers.
///
/// A cleared cell rotates once per turn, so this ledger has to be bounded. What
/// is lost when the oldest entry is dropped is only the named diagnostic: an arm
/// under a forgotten id re-locates that still-existing abandoned file and times
/// out exactly as it does today. No guarantee is stored here, only an
/// explanation.
const MAX_REMEMBERED_ROTATIONS: usize = 8;
/// Row 0 of the transcript `/clear` opens. MEASURED: `{"type":"mode",
/// "sessionId":"<NEW-UUID>"}`, written immediately. It is the rebind anchor.
const ROTATION_ANCHOR_ROW_TYPE: &str = "mode";
/// Row budget for a transcript that is claimed to have served no work.
///
/// MEASURED on Claude Code 2.1.220 over 61 post-`/clear` transcripts: the
/// preamble is 5 rows (`mode`, `file-history-snapshot`, the caveat `user` row,
/// the `/clear` command-echo `user` row, `system`/`local_command`), plus a
/// `last-prompt` row that lands afterwards. 16 is 2.7x that, which absorbs a
/// preamble that grows without ever admitting a served turn.
const MAX_ASSERT_EMPTY_ROWS: usize = 16;
/// Byte budget for the same claim, checked before any parse so a large leaked
/// transcript is refused cheaply rather than parsed to prove it is dirty.
/// MEASURED: the 5-row preamble is 1051-1890 bytes.
const MAX_ASSERT_EMPTY_BYTES: u64 = 64 * 1024;
/// How many `RowKind::UserOther` rows a clean preamble may carry. MEASURED: two,
/// the caveat and the command echo.
const MAX_ASSERT_EMPTY_USER_ROWS: usize = 2;
const LOCAL_COMMAND_CAVEAT_OPEN: &str = "<local-command-caveat>";
const COMMAND_NAME_OPEN: &str = "<command-name>";
const COMMAND_NAME_CLOSE: &str = "</command-name>";
/// The one slash command pmux ever types. Claude's own record of what the
/// composer executed is this row, and it is the only evidence anywhere in the
/// system about which command Enter actually selected.
const CLEAR_COMMAND_NAME: &str = "/clear";
const LOCAL_COMMAND_SUBTYPE: &str = "local_command";
/// The trailing preamble row, and the one allowlisted metadata record that has a
/// field able to carry a caller's prompt.
const LAST_PROMPT_RECORD_TYPE: &str = "last-prompt";
const LAST_PROMPT_FIELD: &str = "lastPrompt";
/// Bound on a Claude schema token reproduced in a diagnostic.
const MAX_DIAGNOSTIC_TOKEN_BYTES: usize = 40;
/// How long a rebound transcript's byte length must hold still before the clear
/// path will bind it.
///
/// MEASURED: the five preamble rows carry timestamps spanning 3ms, and the file
/// first appears 39ms after Enter. 50ms is ~16x that spread, and it is spent
/// between turns inside the existing 2000ms rebind deadline -- so it costs
/// availability, never turn latency, and is never the binding constraint.
const ASSERT_EMPTY_QUIET_FOR: Duration = Duration::from_millis(50);

/// What pmux is willing to say about one captured frame.
///
/// # Why there is no `Unknown`
///
/// There was, and it carried two situations that have nothing in common:
/// a screen pmux POSITIVELY RECOGNIZES as neither ready nor blocking -- a
/// composer holding a caller's own text -- and a screen that matched NO RULE
/// pmux owns. Every consumer read the first, which is ordinary, and inherited
/// the second, which is not.
///
/// MEASURED, and the reason this enum has four arms: `blocking_screen`
/// recognizes 24 screen shapes, and a real "trust this directory", "not logged
/// in", "please update claude code" or "quota exceeded" screen it had not been
/// taught fell into `Unknown` -- indistinguishable, at every call site, from
/// "the caller's prompt is sitting in the composer". `Unknown` meant PROCEED, so
/// the turn ran to its 600,000 ms deadline sitting on a modal, and no refusal
/// anywhere named the screen.
///
/// Splitting them is the structural half of that fix: "matched nothing" is now a
/// value of its own that carries [`ScreenShape`], and because this enum is
/// matched exhaustively, a classifier added tomorrow cannot answer it by
/// accident. `tests::every_rendering_decision_site_is_registered` is the other
/// half -- it reads this crate's own source and fails when a new function turns
/// a rendering into a decision without saying what its unrecognized arm does.
#[derive(Clone, Debug, PartialEq)]
pub enum TerminalScreenState {
    Ready,
    NeedsInput(NeedsInput),
    /// Neither ready nor blocking, and pmux knows which screen this is.
    Recognised(RecognisedScreen),
    /// No rule pmux owns matched this frame. The default outcome, and the one
    /// that refuses.
    Unrecognised(ScreenShape),
}

impl TerminalScreenState {
    /// The stable token this verdict is reported and counted by.
    ///
    /// Every consumer that has to name a verdict in text -- the census in
    /// `crates/service/examples/screen_census.rs`, the corpus invariants, this
    /// file's own assertions -- reads it from here. It was four `match` arms
    /// inside the census, which is a second statement of this enum's vocabulary
    /// that a fifth variant would not have updated: the census would have kept
    /// compiling and kept printing a verdict set with a hole in it.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::NeedsInput(_) => "needs_input",
            Self::Recognised(recognised) => recognised.label(),
            Self::Unrecognised(_) => "unrecognised",
        }
    }
}

/// A rendering pmux positively recognizes as neither ready nor blocking.
///
/// Each arm is a screen some rule in this file ADMITS, which is what separates
/// it from [`TerminalScreenState::Unrecognised`]. A new arm is a new thing pmux
/// claims to understand, and it has to be earned by a rule.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecognisedScreen {
    /// The pane has painted no frame at all: `revision == 0`. Not a screen, and
    /// specifically not an unrecognized one -- there is nothing on it to fail to
    /// recognize.
    NoFrameYet,
    /// A cursor-correlated composer holding text. Ordinary between a paste and
    /// its Enter, and never scanned for modal phrases: the caller's own prompt
    /// may legitimately contain "permission", "allow" or "do you want to
    /// proceed".
    ComposerHoldingText,
}

impl RecognisedScreen {
    /// The stable token this screen is reported and counted by.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::NoFrameYet => "no_frame_yet",
            Self::ComposerHoldingText => "composer_holding_text",
        }
    }
}

/// The STRUCTURAL shape of one captured frame.
///
/// This is what "naming what was on screen" is allowed to mean here. Raw screen
/// text never leaves the terminal adapter -- it carries the caller's prompt, the
/// account name and the working directory -- so an unrecognized screen is
/// reported by the facts that distinguish a changed Claude TUI from a slow one
/// and by nothing else.
///
/// [`crate::native`]'s `startup_screen_diagnostics` is this same shape plus the
/// launch-bundle facts only a startup refusal has any use for, and it BUILDS on
/// [`ScreenShape::to_json`] rather than restating these eight keys.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScreenShape {
    pub revision_nonzero: bool,
    pub rows: u16,
    pub cols: u16,
    pub cursor_present: bool,
    pub cursor_visible: bool,
    pub line_count: usize,
    pub non_empty_line_count: usize,
    pub contains_prompt_glyph: bool,
}

impl ScreenShape {
    /// The shape of one frame, computed from the frame alone.
    #[must_use]
    pub fn of(snapshot: &TerminalSnapshot) -> Self {
        let lines: Vec<&str> = snapshot.visible_text.split('\n').collect();
        Self {
            revision_nonzero: snapshot.revision != 0,
            rows: snapshot.rows,
            cols: snapshot.cols,
            cursor_present: snapshot.cursor.is_some(),
            cursor_visible: snapshot.cursor.is_some_and(|cursor| cursor.visible),
            line_count: lines.len(),
            non_empty_line_count: lines.iter().filter(|line| !line.trim().is_empty()).count(),
            contains_prompt_glyph: snapshot.visible_text.contains(PROMPT_GLYPH),
        }
    }

    /// The same facts as a JSON object, for a refusal's `details`.
    ///
    /// The counts go through `diagnostic_u64` for the same reason every other
    /// count published on the wire does: protocol-v1 has a safe integer domain
    /// and a diagnostic must never be the thing that leaves it.
    ///
    /// The destructuring is the point, not a style: a field added to
    /// [`ScreenShape`] and not published here would leave every unrecognized
    /// screen reported by a shape missing the fact that was added to describe
    /// it, silently. Binding every field by name makes that a COMPILE ERROR in
    /// this function rather than a test somebody has to think to write.
    #[must_use]
    pub fn to_json(self) -> serde_json::Value {
        let Self {
            revision_nonzero,
            rows,
            cols,
            cursor_present,
            cursor_visible,
            line_count,
            non_empty_line_count,
            contains_prompt_glyph,
        } = self;
        json!({
            "revision_nonzero": revision_nonzero,
            "rows": rows,
            "cols": cols,
            "cursor_present": cursor_present,
            "cursor_visible": cursor_visible,
            "line_count": diagnostic_u64(line_count as u64),
            "non_empty_line_count": diagnostic_u64(non_empty_line_count as u64),
            "contains_prompt_glyph": contains_prompt_glyph,
        })
    }
}

#[cfg(test)]
#[must_use]
fn classify_terminal_screen(visible_text: &str) -> TerminalScreenState {
    if let Some(needs_input) = blocking_screen(visible_text) {
        TerminalScreenState::NeedsInput(needs_input)
    } else if has_ready_prompt(visible_text) {
        TerminalScreenState::Ready
    } else {
        TerminalScreenState::Unrecognised(ScreenShape::of(&TerminalSnapshot {
            revision: 1,
            rows: 0,
            cols: 0,
            cursor: None,
            visible_text: visible_text.to_owned(),
        }))
    }
}

/// Classifies a captured terminal using the real cursor when one is present.
/// Cursor-less snapshots retain the narrow legacy text fallback solely for
/// deterministic fakes. For a structured snapshot, the active editor is
/// recognized before scanning screen-wide modal phrases because the user's
/// prompt itself may legitimately contain words such as "permission" or
/// "allow".
///
/// **Every arm that is not a positive recognition returns
/// [`TerminalScreenState::Unrecognised`].** That is the whole of the structural
/// rule: this function may only ever say `Ready`, `NeedsInput` or one of
/// [`RecognisedScreen`]'s named screens by MATCHING something, and anything else
/// falls through to a value the caller cannot mistake for a negative.
#[must_use]
pub fn classify_terminal_snapshot(snapshot: &TerminalSnapshot) -> TerminalScreenState {
    if snapshot.revision == 0 {
        return TerminalScreenState::Recognised(RecognisedScreen::NoFrameYet);
    }
    if snapshot.cursor.is_none() {
        if let Some(needs_input) = blocking_screen(&snapshot.visible_text) {
            return TerminalScreenState::NeedsInput(needs_input);
        }
        #[cfg(test)]
        if has_ready_prompt(&snapshot.visible_text) {
            return TerminalScreenState::Ready;
        }
        return TerminalScreenState::Unrecognised(ScreenShape::of(snapshot));
    }
    if let Some(editor) = active_editor(snapshot) {
        if editor.empty_cursor_position {
            TerminalScreenState::Ready
        } else {
            // A populated active editor is neither ready for another prompt nor
            // a modal. In particular, do not scan the user's pasted text for
            // modal keywords.
            TerminalScreenState::Recognised(RecognisedScreen::ComposerHoldingText)
        }
    } else if let Some(needs_input) = blocking_screen(&snapshot.visible_text) {
        TerminalScreenState::NeedsInput(needs_input)
    } else {
        TerminalScreenState::Unrecognised(ScreenShape::of(snapshot))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct EditorSignature {
    /// Rendered input rows from the nearest prompt anchor through the cursor.
    /// This intentionally excludes history, banners, and footer animation.
    rendered_rows: Vec<String>,
    cursor_row_from_anchor: u16,
    cursor_col_from_prompt: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ActiveEditor {
    signature: EditorSignature,
    anchor_row: u16,
    cursor_row: u16,
    prompt_col: u16,
    empty_cursor_position: bool,
}

fn active_editor(snapshot: &TerminalSnapshot) -> Option<ActiveEditor> {
    let cursor = snapshot.cursor?;
    // A grid with no rows, or none with any columns, needs no clause of its
    // own: a cursor position is unsigned, so EVERY cursor is outside a
    // zero-sized grid and the two bounds below already refuse it. The two
    // clauses that used to say so again could each be disabled by a full-scope
    // mutation run with nothing in the workspace to notice, which is what a
    // clause no input can reach looks like from outside.
    if snapshot.revision == 0
        || !cursor.visible
        || cursor.row >= snapshot.rows
        || cursor.col >= snapshot.cols
    {
        return None;
    }

    // rmux visible_text is produced by joining exactly `rows` rendered rows.
    // Reject malformed or cursor-less fake shapes rather than correlating a
    // cursor against the wrong textual row.
    let lines: Vec<_> = snapshot.visible_text.split('\n').collect();
    if lines.len() != usize::from(snapshot.rows) {
        return None;
    }

    let cursor_row = usize::from(cursor.row);
    // The composer sits at the END OF THE FRAME, which is only the end of the
    // grid while Ink happens to be painting that far down. A screen with no
    // rendered row at all carries no frame to be at the end of, and a cursor
    // parked below every rendered row is not in one.
    let last_rendered_row = lines.iter().rposition(|line| !line.trim().is_empty())?;
    if cursor_row > last_rendered_row
        || last_rendered_row - cursor_row > usize::from(MAX_RENDERED_ROWS_BELOW_ACTIVE_CURSOR)
    {
        return None;
    }
    let (anchor_row, prompt_col) = (0..=cursor_row)
        .rev()
        .find_map(|row| prompt_glyph_col(lines[row]).map(|prompt_col| (row, prompt_col)))?;
    let cursor_row_from_anchor = u16::try_from(cursor_row.saturating_sub(anchor_row)).ok()?;
    let cursor_col_from_prompt = if cursor_row == anchor_row {
        cursor.col.checked_sub(prompt_col)?
    } else {
        cursor.col
    };
    if cursor_row == anchor_row && cursor_col_from_prompt < 2 {
        return None;
    }

    Some(ActiveEditor {
        signature: EditorSignature {
            rendered_rows: lines[anchor_row..=cursor_row]
                .iter()
                .map(|line| (*line).to_owned())
                .collect(),
            cursor_row_from_anchor,
            cursor_col_from_prompt,
        },
        anchor_row: anchor_row.try_into().ok()?,
        cursor_row: cursor.row,
        prompt_col,
        // Live Claude's empty/placeholder editor leaves the cursor two cells
        // after the prompt glyph. Placeholder text may still be rendered to
        // the right, so cursor position—not line emptiness—is authoritative.
        empty_cursor_position: cursor_row_from_anchor == 0 && cursor_col_from_prompt == 2,
    })
}

/// The measured geometry of one resolved composer, for offline analysis.
///
/// Every field is read straight off the same `active_editor` (private, so not
/// an intra-doc link from public documentation) that production
/// classifies with -- this type recomputes nothing. That is the point: a corpus
/// invariant asserting "a Ready frame renders exactly two rows below the cursor"
/// is only evidence about production if it is reading production's own numbers.
/// A parallel reimplementation here would drift, and would then agree with
/// itself while disagreeing with the gate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScreenGeometry {
    /// Row of the `❯` glyph the cursor was correlated to.
    pub anchor_row: u16,
    pub cursor_row: u16,
    /// Column the `❯` glyph sits at.
    pub prompt_col: u16,
    pub cursor_row_from_anchor: u16,
    pub cursor_col_from_prompt: u16,
    /// Last row carrying any non-whitespace. This is the end of Ink's FRAME,
    /// which is the bottom of the grid only when Ink happens to paint that far.
    pub last_rendered_row: u16,
    /// `last_rendered_row - cursor_row`: the quantity the composer gate bounds,
    /// and the one the old bottom-of-grid expression got wrong.
    pub rendered_rows_below_cursor: u16,
    pub empty_cursor_position: bool,
}

/// The geometry of the composer on `snapshot`, or `None` when no editor
/// resolves.
///
/// Read-only and side-effect free. Exposed for the screen corpus and the
/// synthetic-screen property suite; production classifies through
/// [`classify_terminal_snapshot`] and never calls this.
#[must_use]
pub fn screen_geometry(snapshot: &TerminalSnapshot) -> Option<ScreenGeometry> {
    let editor = active_editor(snapshot)?;
    let last_rendered_row = u16::try_from(
        snapshot
            .visible_text
            .split('\n')
            .collect::<Vec<_>>()
            .iter()
            .rposition(|line| !line.trim().is_empty())?,
    )
    .ok()?;
    Some(ScreenGeometry {
        anchor_row: editor.anchor_row,
        cursor_row: editor.cursor_row,
        prompt_col: editor.prompt_col,
        cursor_row_from_anchor: editor.signature.cursor_row_from_anchor,
        cursor_col_from_prompt: editor.signature.cursor_col_from_prompt,
        last_rendered_row,
        rendered_rows_below_cursor: last_rendered_row.saturating_sub(editor.cursor_row),
        empty_cursor_position: editor.empty_cursor_position,
    })
}

/// The one glyph that opens a Claude composer row. Named once so
/// [`prompt_glyph_col`] and [`composer_head`] cannot come to disagree about
/// which character they are stepping over.
const PROMPT_GLYPH: char = '❯';

/// The composer's column on one rendered row, and the buffer text that begins
/// after it: leading whitespace, the glyph, then AT MOST ONE whitespace cell.
///
/// One statement of that rule, consumed by both readers of it. It used to be
/// two -- [`prompt_glyph_col`] decided which cells were the composer's opening
/// and [`composer_head`] decided again which one to remove -- and the second
/// decision was unfalsifiable: this function admits a row only when the cell
/// after the glyph is whitespace or absent, so `composer_head`'s own test for
/// whitespace was true on every row that could reach it. A full-scope mutation
/// run replaced that test with `true` and no test in the workspace noticed.
/// The head is now what this rule's own iterator has left over, so the removal
/// cannot come to disagree with the admission.
fn prompt_glyph_split(line: &str) -> Option<(u16, &str)> {
    let glyph_offset = line.find(PROMPT_GLYPH)?;
    let prefix = &line[..glyph_offset];
    if !prefix.chars().all(char::is_whitespace) {
        return None;
    }
    let mut suffix = line[glyph_offset..].chars();
    let glyph = suffix.next();
    let separator = suffix.next();
    if glyph != Some(PROMPT_GLYPH) || !separator.is_none_or(char::is_whitespace) {
        return None;
    }
    Some((u16::try_from(prefix.chars().count()).ok()?, suffix.as_str()))
}

fn prompt_glyph_col(line: &str) -> Option<u16> {
    prompt_glyph_split(line).map(|(column, _)| column)
}

/// The composer's first rendered row with its indent, its `❯` and the single
/// cell after the glyph removed: the beginning of whatever the composer is
/// actually holding.
///
/// The removal IS [`prompt_glyph_split`]'s own step over those cells and not a
/// second statement of its rule, so this text begins exactly where that
/// function decided the buffer begins. MEASURED at 2.1.226 the separator is
/// U+00A0 and at 2.1.70 it is absent entirely on an empty composer, which is
/// why one optional whitespace is the rule rather than a literal `"❯ "`.
///
/// `None` when the anchor row does not open a composer at the column
/// [`active_editor`] found one at. That is unreachable for an editor this
/// module produced, and it is an `Option` rather than an `expect` because the
/// caller's use of `None` is to refuse, which is the safe direction anyway.
fn composer_head(editor: &ActiveEditor) -> Option<&str> {
    let row = editor.signature.rendered_rows.first()?;
    prompt_glyph_split(row)
        .filter(|(column, _)| *column == editor.prompt_col)
        .map(|(_, head)| head)
}

/// One continuation row with the composer's gutter removed.
///
/// `gutter` is a count of CELLS and the gutter is spaces, so it is also a count
/// of characters. MEASURED at 2.1.226: every row below the first is indented by
/// exactly two, which is where [`composer_head`] decided the buffer's text
/// begins — the `❯` and its one separating cell. The prompt's OWN leading
/// whitespace survives that removal and is compared: a prompt line reading
/// `    this third line begins with four spaces` rendered as six leading
/// spaces, two of them the gutter.
///
/// `None` when the gutter is not blank, which is how a row that is not a
/// continuation of this composer — a horizontal rule, a footer, another
/// editor's text — refuses instead of being silently sliced.
fn continuation_content(row: &str, gutter: usize) -> Option<&str> {
    // A row the terminal right-trimmed to nothing is a blank composer line, not
    // a row with a missing gutter.
    let boundary = row
        .char_indices()
        .nth(gutter)
        .map_or(row.len(), |(offset, _)| offset);
    let (blank, content) = row.split_at(boundary);
    blank.chars().all(char::is_whitespace).then_some(content)
}

/// Everything the composer is holding, row by row, with its glyph and gutters
/// removed.
///
/// The rows are `active_editor`'s own, which run from the `❯` anchor through
/// the cursor row. That range is the WHOLE buffer rather than a window on it:
/// the anchor is the buffer's first row and the cursor sits at its last
/// character, so a composer whose text ran past the pane would have lost its
/// anchor and resolved no editor at all.
fn composer_rows(editor: &ActiveEditor) -> Option<Vec<&str>> {
    let head = composer_head(editor)?;
    // Derived from where the head began rather than from a literal 2, so the
    // gutter and the glyph removal cannot come to disagree.
    let gutter = editor.signature.rendered_rows[0].chars().count() - head.chars().count();
    let mut rows = Vec::with_capacity(editor.signature.rendered_rows.len());
    rows.push(head);
    for row in &editor.signature.rendered_rows[1..] {
        rows.push(continuation_content(row, gutter)?);
    }
    Some(rows)
}

/// Normalizes and validates prompt text before any terminal mutation occurs.
///
/// The composer rule is [`pseudomux_claude::composer_refusal`] and not a test
/// stated here. It used to be `starts_with('/')`, which named one of the two
/// characters the composer MEASURABLY treats as a mode switch: a prompt whose
/// first character is `!` switched a real 2.1.226 cell into bash mode and ran
/// the rest as a shell command on the host, six times out of six. The rule now
/// lives in the crate `bin/pmux`'s CLI copy also links, so the guard a caller
/// meets early and the guard the daemon enforces are one function rather than
/// two statements of one intention.
///
/// The emptiness test below runs AFTER normalization and that is the whole of
/// pmux's rule against an unsubmittable buffer. `normalize_prompt` applies the
/// composer's own trailing trim, so `"   "`, `"\u{a0}"` and `"\n"` arrive here
/// as the empty string. Each of them was MEASURED at 2.1.226 as a buffer Enter
/// never submits -- pmux typed it, waited for an acknowledgement that cannot
/// arrive, and destroyed the instance at the caller's deadline
/// (`docs/path-b-adversarial.md` sec. 11) -- and none of them needs a rule of
/// its own now that the normalization states the composer's.
pub fn validate_prompt(prompt: &str) -> DriverResult<String> {
    let normalized = pseudomux_claude::normalize_prompt(prompt);
    if normalized.is_empty() {
        return Err(DriverFailure::new(
            ErrorCode::InvalidConfig,
            "prompt must not be empty; a prompt of nothing but whitespace is empty here, \
             because the composer removes its trailing whitespace and Enter never submits \
             what is left",
        ));
    }
    if normalized.len() > MAX_PROMPT_BYTES {
        return Err(DriverFailure::new(
            ErrorCode::InvalidConfig,
            format!("prompt exceeds the {MAX_PROMPT_BYTES}-byte service limit"),
        ));
    }
    if let Some(refusal) = pseudomux_claude::composer_refusal(&normalized) {
        return Err(DriverFailure::new(
            // A mode prefix is a control surface pmux has no typed API for; a
            // rewritten character and a trailing line continuation are both
            // malformed requests -- the caller changes the prompt and retries,
            // and no pmux feature would make either of them work. Two codes
            // because a caller retries one of them differently from the other.
            match refusal {
                pseudomux_claude::ComposerRefusal::ModePrefix(_) => ErrorCode::UnsupportedFeature,
                pseudomux_claude::ComposerRefusal::RewrittenCharacter(_)
                | pseudomux_claude::ComposerRefusal::LineContinuation => ErrorCode::InvalidConfig,
            },
            // The remedy travels in the advice channel rather than only inside
            // this message: `bin/pmux-mcp` redacts a daemon message and renders
            // that field, so `unsupported_feature` there otherwise says the
            // same nothing for a `/`-prefixed prompt as for a daemon with no
            // pool. The CLI prints message then advice, which reads as the one
            // sentence `ComposerRefusal::describe` has always produced.
            refusal.explain(),
        )
        .with_details(json!({
            "violation": refusal.code(),
            RECOMMENDATION_KEY: refusal.remedy(),
        })));
    }
    // `\t` is deliberately absent from this exception where it once stood:
    // `composer_refusal` above has already refused it, with a message that says
    // what the composer does to it instead of calling it unsafe.
    //
    // NUL and ESC were named in front of this clause and are both Cc, so the
    // clause already refused both and the two comparisons could never be the
    // ones that refused. A full-scope mutation run disabled them and no test in
    // the workspace could tell -- while the clause behind them, which exempts
    // `\n` and refuses the 62 control characters neither literal names (65 in
    // C0, DEL and C1, less those two and the newline), had no test at all.
    // `every_control_character_but_a_newline_is_refused_from_a_prompt` now
    // states the rule over the whole domain instead of over two of its members.
    if normalized
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n'))
    {
        return Err(DriverFailure::new(
            ErrorCode::InvalidConfig,
            "prompt contains an unsafe control character",
        ));
    }
    Ok(normalized)
}

/// A privileged command pmux types into a Claude TUI it owns.
///
/// The caller-facing refusal above is a filter over caller bytes, and a filter
/// invites argument: leading whitespace, a unicode lookalike, an embedded
/// newline before the slash. This channel does not answer that argument, it
/// removes it. No caller byte can become the text typed here, because the text
/// is not data — it is selected by the variant, at compile time, from this file.
/// So `validate_prompt` never has to be relaxed for pmux to clear a cell, and no
/// future relaxation of it can reach this text either.
///
/// The variant carries no payload for the same reason. A `ControlCommand(String)`
/// would reintroduce exactly the injection question this type exists to close.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlCommand {
    /// `/clear`: abandons the current transcript, rotates the session id, and
    /// opens a new transcript in the same project directory. Statelessness for a
    /// minified cell is bought with this command rather than with a relaunch,
    /// which MEASURED ~4.4s against its ~30ms.
    Clear,
}

impl ControlCommand {
    /// The command a recorded screen-corpus frame names in `expect_selection`,
    /// or `None` for anything pmux does not type. The mapping is closed over
    /// the variants for the same reason the enum carries no payload.
    #[must_use]
    pub fn from_literal(literal: &str) -> Option<Self> {
        (literal == Self::Clear.literal()).then_some(Self::Clear)
    }

    #[must_use]
    pub const fn literal(self) -> &'static str {
        match self {
            Self::Clear => "/clear",
        }
    }
}

fn ensure_before_turn_deadline(deadline_unix_ms: u64) -> DriverResult<()> {
    validate_turn_deadline_domain(deadline_unix_ms)?;
    if now_unix_ms()? >= deadline_unix_ms {
        return Err(turn_deadline_failure());
    }
    Ok(())
}

fn validate_turn_deadline_domain(deadline_unix_ms: u64) -> DriverResult<()> {
    if deadline_unix_ms > MAX_SAFE_JSON_INTEGER {
        return Err(DriverFailure::new(
            ErrorCode::InvalidConfig,
            "turn deadline is outside protocol-v1's safe timestamp domain",
        ));
    }
    Ok(())
}

fn now_unix_ms() -> DriverResult<u64> {
    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| {
        DriverFailure::new(
            ErrorCode::RecoveryFailed,
            "current time precedes the Unix epoch",
        )
    })?;
    protocol_milliseconds(elapsed.as_millis(), "current time")
}

fn protocol_milliseconds(milliseconds: u128, field: &'static str) -> DriverResult<u64> {
    u64::try_from(milliseconds)
        .ok()
        .filter(|value| *value <= MAX_SAFE_JSON_INTEGER)
        .ok_or_else(|| {
            DriverFailure::new(
                ErrorCode::RecoveryFailed,
                format!("{field} is outside protocol-v1's safe integer domain"),
            )
        })
}

fn turn_deadline_failure() -> DriverFailure {
    DriverFailure::new(
        ErrorCode::TurnTimeout,
        "turn deadline elapsed before prompt submission",
    )
}

fn input_gate_failure() -> DriverFailure {
    DriverFailure::new(
        ErrorCode::PromptNotAcknowledged,
        "terminal editor state could not be proven before prompt submission",
    )
}

/// The message names what [`rendered_prompt_is_proven`] tests, and it has now
/// been wrong in both directions. It first said "the terminal input render was
/// not proven", over a predicate that read cursor geometry and never looked at
/// a character. It then said "this prompt's HEAD", over a predicate that
/// accepted any non-empty prefix of it -- true, and an understatement of what
/// the operator needed to know, which is that ONE character on the row was
/// enough. It now names the rows, which is what is compared.
fn input_render_failure() -> DriverFailure {
    DriverFailure::new(
        ErrorCode::PromptNotAcknowledged,
        "the composer's rendered rows were not proven to hold this prompt before Enter",
    )
}

fn paste_ambiguity_failure() -> DriverFailure {
    DriverFailure::new(
        ErrorCode::PromptNotAcknowledged,
        "terminal paste acknowledgement was ambiguous; Enter was not attempted",
    )
}

fn enter_ambiguity_failure() -> DriverFailure {
    mark_enter_attempted(DriverFailure::new(
        ErrorCode::RecoveryFailed,
        "terminal Enter acknowledgement was ambiguous; submission was not retried",
    ))
}

/// The detail key a failure carries when pmux's single irreversible write has
/// already gone to the terminal.
const ENTER_ATTEMPTED: &str = "enter_attempted";

/// Marks a failure raised at or after that write.
///
/// One statement about "Enter may have landed", set here and read only by
/// [`enter_was_attempted`], because more than one failure has to carry it:
/// [`enter_ambiguity_failure`] is built from it, and so is the turn-deadline
/// answer `enter_once` gives when the budget expires with the Enter still in
/// flight. Written as a mark applied to a failure rather than as a detail
/// literal at each site so the two can never come to disagree -- and a bare
/// deadline answer out of `enter_once` would be exactly that disagreement:
/// [`clear_and_rebind`] would read "nothing was typed", mark the refusal
/// `clear_not_submitted`, and leave the actor bound to a transcript `/clear`
/// may already have abandoned.
fn mark_enter_attempted(mut failure: DriverFailure) -> DriverFailure {
    match &mut failure.details {
        serde_json::Value::Object(details) => {
            details.insert(ENTER_ATTEMPTED.to_owned(), true.into());
        }
        details => *details = json!({ ENTER_ATTEMPTED: true }),
    }
    failure
}

struct InputGateBudget {
    cap: Instant,
    turn_deadline_unix_ms: u64,
}

impl InputGateBudget {
    fn new(turn_deadline_unix_ms: u64, maximum: Duration) -> DriverResult<Self> {
        validate_turn_deadline_domain(turn_deadline_unix_ms)?;
        let now_ms = now_unix_ms()?;
        let remaining_ms = turn_deadline_unix_ms
            .checked_sub(now_ms)
            .filter(|remaining| *remaining > 0)
            .ok_or_else(turn_deadline_failure)?;
        let remaining_turn = Duration::from_millis(remaining_ms);
        let cap = Instant::now()
            .checked_add(maximum.min(remaining_turn))
            .ok_or_else(|| {
                DriverFailure::new(
                    ErrorCode::InvalidConfig,
                    "terminal input gate duration overflows the monotonic clock domain",
                )
            })?;
        Ok(Self {
            cap,
            turn_deadline_unix_ms,
        })
    }

    fn remaining(&self, after_paste: bool) -> DriverResult<Duration> {
        ensure_before_turn_deadline(self.turn_deadline_unix_ms)?;
        let now = Instant::now();
        if now >= self.cap {
            return Err(if after_paste {
                input_render_failure()
            } else {
                input_gate_failure()
            });
        }
        Ok(self.cap - now)
    }

    /// Which of this budget's two clocks a `tokio::time::timeout` built from
    /// [`Self::remaining`] just ran out of.
    ///
    /// [`Self::cap`] is `min(gate maximum, remaining turn)`, so a fired timeout
    /// means one of two materially different things — the turn is over, or pmux
    /// could not prove this one operation inside the gate's own bound — and the
    /// `Elapsed` the caller is handed says NOTHING about which. That is the
    /// whole reason somebody has to ask the budget. Only the first is a
    /// `TurnTimeout`, and only the second is the caller's terminal being
    /// ambiguous.
    ///
    /// It lives on the budget rather than at each site because the sites had
    /// drifted. `gated_snapshot` and `gated_styled_screen` asked this question;
    /// `paste_once` and `enter_once` did not, so ONE physical event -- this
    /// turn ran out of time -- reached callers under two different codes
    /// depending only on whether the clock happened to expire during a read or
    /// during a write, which is not a distinction any caller can observe or act
    /// on. On the `/clear` path it was not even a race: the clear deadline and
    /// the gate maximum are both 15,000 ms and the deadline is computed first,
    /// so the remaining turn is the binding term on every clear, and every
    /// write expiry there was a deadline wearing another code.
    ///
    /// `ambiguity` stays at the site because only the site knows what it failed
    /// to prove.
    fn expiry(&self, ambiguity: impl FnOnce() -> DriverFailure) -> DriverFailure {
        match now_unix_ms() {
            Ok(now) if now >= self.turn_deadline_unix_ms => turn_deadline_failure(),
            Ok(_) => ambiguity(),
            // The clock left protocol-v1's domain while this operation was in
            // flight. That is the stronger fact and it is exactly what
            // `remaining` would have reported one call earlier.
            Err(clock) => clock,
        }
    }
}

async fn gated_snapshot(
    terminal: &mut dyn TerminalSession,
    budget: &InputGateBudget,
    after_paste: bool,
) -> DriverResult<TerminalSnapshot> {
    if terminal.lease_lost() {
        return Err(DriverFailure::new(
            ErrorCode::DaemonLost,
            "private rmux lease was lost during prompt submission",
        )
        .retryable(true));
    }
    let remaining = budget.remaining(after_paste)?;
    // Bound as its own statement so the read's borrow of `terminal` ends before
    // the mapper takes its own borrow to consult the lease.
    let read = tokio::time::timeout(remaining, terminal.snapshot()).await;
    let snapshot = match read {
        Ok(snapshot) => snapshot.map_err(|error| map_terminal_error(terminal, error))?,
        Err(_) => {
            return Err(budget.expiry(|| {
                if after_paste {
                    input_render_failure()
                } else {
                    input_gate_failure()
                }
            }));
        }
    };
    if terminal.lease_lost() {
        return Err(DriverFailure::new(
            ErrorCode::DaemonLost,
            "private rmux lease was lost during prompt submission",
        )
        .retryable(true));
    }
    budget.remaining(after_paste)?;
    // Opt-in corpus recording. Off unless PMUX_SCREEN_CORPUS_DIR is set, in
    // which case this is one bounded non-blocking send to a dedicated writer
    // thread; see `crate::screen_corpus`. Placed after the last fallible check
    // so a recorded frame is exactly a frame the gate went on to classify.
    crate::screen_corpus::record_snapshot(
        if after_paste {
            INPUT_GATE_POST_PASTE_SITE
        } else {
            INPUT_GATE_PRE_PASTE_SITE
        },
        &snapshot,
    );
    Ok(snapshot)
}

/// [`gated_snapshot`] for the one read that needs cell colours.
///
/// Always the post-paste half of the budget: the only caller is the control
/// channel's render gate, which by construction runs after the command has been
/// pasted.
async fn gated_styled_screen(
    terminal: &mut dyn TerminalSession,
    budget: &InputGateBudget,
) -> DriverResult<StyledScreen> {
    if terminal.lease_lost() {
        return Err(DriverFailure::new(
            ErrorCode::DaemonLost,
            "private rmux lease was lost during prompt submission",
        )
        .retryable(true));
    }
    let remaining = budget.remaining(true)?;
    // Bound as its own statement so the read's borrow of `terminal` ends before
    // the mapper takes its own borrow to consult the lease.
    let read = tokio::time::timeout(remaining, terminal.styled_screen()).await;
    let screen = match read {
        Ok(screen) => screen.map_err(|error| map_terminal_error(terminal, error))?,
        Err(_) => return Err(budget.expiry(input_render_failure)),
    };
    if terminal.lease_lost() {
        return Err(DriverFailure::new(
            ErrorCode::DaemonLost,
            "private rmux lease was lost during prompt submission",
        )
        .retryable(true));
    }
    budget.remaining(true)?;
    // The only read that carries cell colours, and therefore the only frames a
    // corpus can ever use to check the `/clear` selection proof.
    crate::screen_corpus::record_styled(CONTROL_CHANNEL_SELECTION_SITE, &screen);
    Ok(screen)
}

async fn sleep_for_input_poll(
    budget: &InputGateBudget,
    poll_interval: Duration,
    after_paste: bool,
) -> DriverResult<()> {
    let remaining = budget.remaining(after_paste)?;
    tokio::time::sleep(poll_interval.min(remaining)).await;
    budget.remaining(after_paste).map(|_| ())
}

fn needs_input_failure(needs_input: NeedsInput) -> DriverFailure {
    let code = match needs_input.kind {
        NeedsInputKind::Trust => ErrorCode::NeedsTrust,
        NeedsInputKind::Login => ErrorCode::NeedsLogin,
        NeedsInputKind::Permission => ErrorCode::NeedsPermission,
        NeedsInputKind::Update => ErrorCode::NeedsUpdate,
        NeedsInputKind::Quota => ErrorCode::RateLimited,
        NeedsInputKind::UnknownModal => ErrorCode::NeedsInput,
    };
    DriverFailure::new(code, needs_input.message)
}

async fn wait_for_stable_empty_editor(
    terminal: &mut dyn TerminalSession,
    budget: &InputGateBudget,
    stable_for: Duration,
    poll_interval: Duration,
) -> DriverResult<TerminalSnapshot> {
    let mut candidate: Option<(EditorSignature, Instant)> = None;
    loop {
        let snapshot = gated_snapshot(terminal, budget, false).await?;
        match classify_terminal_snapshot(&snapshot) {
            TerminalScreenState::Ready => {
                let editor = active_editor(&snapshot)
                    .filter(|editor| editor.empty_cursor_position)
                    .ok_or_else(input_gate_failure)?;
                let same_candidate = candidate
                    .as_ref()
                    .is_some_and(|(observed, _)| observed == &editor.signature);
                if !same_candidate {
                    candidate = Some((editor.signature, Instant::now()));
                }
                if candidate
                    .as_ref()
                    .is_some_and(|(_, since)| since.elapsed() >= stable_for)
                {
                    return Ok(snapshot);
                }
            }
            TerminalScreenState::NeedsInput(needs_input) => {
                return Err(needs_input_failure(needs_input));
            }
            // Both arms restart the window, and they are written apart rather
            // than folded because they are not the same fact: a composer
            // holding text is a screen this gate is WAITING OUT, and an
            // unrecognized one is a screen it has nothing to say about. Neither
            // refuses here -- Gate 1 refuses by running out of
            // [`INPUT_GATE_MAX_DURATION`], which is a bounded refusal that
            // already names the gate.
            TerminalScreenState::Recognised(_) | TerminalScreenState::Unrecognised(_) => {
                candidate = None;
            }
        }
        sleep_for_input_poll(budget, poll_interval, false).await?;
    }
}

/// Gate 2's whole question, in two independent halves: is the composer this
/// gate fenced now holding THIS PROMPT, rather than merely holding something?
///
/// # What is proven, exactly
///
/// **Geometry**, which answers *is this the same composer, and did it change*.
/// It cannot answer *with what*, and until 2026-08-09 it was the entire test:
/// the function was called `rendered_prompt_is_proven` and the failure it raises
/// said the render "was not proven", and neither statement ever compared one
/// character. A composer holding `! echo … > /tmp/…` satisfied every
/// geometric clause of a prompt that said `What is 2 plus 2?` and Enter was
/// pressed on it. `docs/path-b-adversarial.md` sec. 4.3(b) filed that as
/// reported and not fixed; this is the fix.
///
/// **The text**, which answers *with what*. [`composer_rows`] takes every row
/// the composer is rendering and [`pseudomux_claude::composer_render_proof`]
/// requires them to spell this prompt from its first character to its last, or
/// to be the single placeholder row the composer MEASURABLY substitutes for a
/// paste it collapsed, carrying this prompt's own line-break count.
///
/// # Why this is the whole buffer and not a head
///
/// It was a head until 2026-08-10, and a head with no lower bound: the clause
/// was `prompt.starts_with(head)` over the FIRST ROW ONLY. PROBED at `8c3d387`,
/// a composer showing `W` proved the prompt `What is 2 plus 2?` — every
/// geometric clause satisfied, `(pastes, enters) = (1, 1)` — and the post-Enter
/// equality then refused the turn and destroyed the pooled instance. One
/// delivered character of seventeen satisfied the clause this gate is named
/// for.
///
/// The rows are the whole buffer because [`active_editor`] takes them from the
/// `❯` anchor through the cursor row: the anchor is the buffer's first row and
/// the cursor sits at its last character. MEASURED on all twelve 2.1.226
/// renders this session recorded, including a 600-character prompt across six
/// rows. `MAX_PROMPT_BYTES` is 1 MiB and a pane is 24 rows, so most prompts
/// still cannot be compared here at all — but they are not *partly* compared
/// either: MEASURED, a single line of 1000 characters or more collapses to a
/// placeholder, and a buffer too tall for the pane loses its anchor and
/// resolves no editor.
///
/// The full POST-Enter equality is unchanged and is still the stronger one:
/// `TranscriptEngine::ingest` refuses the turn with `UnexpectedTypedPrompt`
/// when the row Claude recorded differs from the text pmux typed. This gate is
/// what stands between the paste and the irreversible write, which is the one
/// place that equality can never be.
///
/// A bounded hash over the visible region was weighed and lost: it needs the
/// same reconstruction to know what to hash, and then reports a mismatch with
/// no diagnostic.
///
/// # The clause that used to be here, and why it is not
///
/// `cursor_moved || rendered_rows_changed` — "the editor is not what the fence
/// saw" — was one of three clauses a mutation run could disable while the whole
/// `pseudomux-service --lib` suite stayed at 415 passed. The other two are
/// load-bearing and now have tests that fail without them. This one is
/// **redundant, and provably so rather than in the reviewer's opinion**:
/// `EditorSignature` holds exactly `rendered_rows`, `cursor_row_from_anchor`
/// and `cursor_col_from_prompt`, and [`ActiveEditor::empty_cursor_position`] is
/// computed out of the last two alone. So two editors with equal signatures
/// have equal `empty_cursor_position`, and this function's baseline is the
/// FENCE, which [`prove_stable_empty_editor`] has already filtered on. An
/// editor that is not at its empty position therefore cannot have the fence's
/// signature, and the clause could never be the one that refused.
///
/// It said `cursor_moved || rendered_rows_changed` where it meant
/// `editor.signature != baseline.signature`, which is the same three fields
/// hand-written — a fourth field on the signature would have quietly stopped
/// being covered. Deleted rather than rewritten, because rewriting it correctly
/// would still leave a clause no input can reach.
///
/// `baseline.empty_cursor_position` is the clause that replaces it, and it is
/// not the same statement: it CHECKS the fence invariant the deletion relies on
/// instead of assuming it, so this function is now right for any baseline
/// rather than only for the two call sites that pass a fenced one.
fn rendered_prompt_is_proven(
    snapshot: &TerminalSnapshot,
    baseline_revision: u64,
    baseline: &ActiveEditor,
    prompt: &str,
) -> bool {
    let Some(editor) = active_editor(snapshot) else {
        return false;
    };
    let rows_are_this_prompt = composer_rows(&editor)
        .and_then(|rows| pseudomux_claude::composer_render_proof(&rows, prompt))
        .is_some();
    // One composer, grown in whichever direction its frame is anchored.
    //
    // BOTTOM-anchored (MEASURED on 2.1.70 and on 2.1.220 mid-session): the box
    // is pinned to the footer, so a wrapping prompt pushes the `❯` anchor UP and
    // the cursor's row does not move.
    //
    // TOP-anchored (MEASURED on 2.1.220 after a `/clear`, three sessions): the
    // frame begins where the previous one ended and the blank grid below it
    // absorbs the growth, so the anchor stays PINNED and the cursor walks DOWN.
    // A zero-model bracketed-paste probe into a post-clear composer moved the
    // cursor from (5,2) to (7,x) with the anchor still at row 5.
    //
    // Both are the same statement -- the pasted text rendered into the composer
    // this gate fenced -- and neither admits a second editor: the prompt column
    // is identical and one of the two rows is invariant in each.
    let same_editor_geometry = editor.prompt_col == baseline.prompt_col
        && ((editor.cursor_row == baseline.cursor_row && editor.anchor_row <= baseline.anchor_row)
            || (editor.anchor_row == baseline.anchor_row
                && editor.cursor_row >= baseline.cursor_row));
    snapshot.revision != 0
        && snapshot.revision != baseline_revision
        && baseline.empty_cursor_position
        && !editor.empty_cursor_position
        && same_editor_geometry
        && rows_are_this_prompt
}

async fn wait_for_stable_prompt_render(
    terminal: &mut dyn TerminalSession,
    budget: &InputGateBudget,
    baseline: &TerminalSnapshot,
    baseline_editor: &ActiveEditor,
    prompt: &str,
    stable_for: Duration,
    poll_interval: Duration,
) -> DriverResult<TerminalSnapshot> {
    let mut candidate: Option<(EditorSignature, Instant)> = None;
    loop {
        let snapshot = gated_snapshot(terminal, budget, true).await?;
        if rendered_prompt_is_proven(&snapshot, baseline.revision, baseline_editor, prompt) {
            let signature = active_editor(&snapshot)
                .expect("render proof requires an active editor")
                .signature;
            let same_candidate = candidate
                .as_ref()
                .is_some_and(|(observed, _)| observed == &signature);
            if !same_candidate {
                candidate = Some((signature, Instant::now()));
            }
            if candidate
                .as_ref()
                .is_some_and(|(_, since)| since.elapsed() >= stable_for)
            {
                return Ok(snapshot);
            }
        } else {
            // An active populated editor can contain modal-like words from the
            // user's own prompt; only scan for a modal when there is no active
            // cursor-correlated editor at all.
            if active_editor(&snapshot).is_none()
                && let Some(needs_input) = blocking_screen(&snapshot.visible_text)
            {
                return Err(needs_input_failure(needs_input));
            }
            candidate = None;
        }
        sleep_for_input_poll(budget, poll_interval, true).await?;
    }
}

/// Gate 1: observe one stable, empty, cursor-anchored Claude editor and
/// immediately fence it. Any mutation at the fence restarts the gate, so the
/// caller only ever reaches its first terminal write against a proven editor.
///
/// Shared by prompt submission and the control channel deliberately. `/clear`
/// typed into a populated composer would concatenate with whatever is already
/// there, and a `/clear` typed while Claude is mid-turn is a command the pool's
/// model of the instance does not cover. Both are excluded by this gate, which
/// is also why the control channel needs no separate mutual exclusion against
/// turns: a busy TUI has no stable empty editor to prove.
async fn prove_stable_empty_editor(
    terminal: &mut dyn TerminalSession,
    budget: &InputGateBudget,
    stable_for: Duration,
    poll_interval: Duration,
) -> DriverResult<(TerminalSnapshot, ActiveEditor)> {
    loop {
        let baseline =
            wait_for_stable_empty_editor(terminal, budget, stable_for, poll_interval).await?;
        let baseline_editor = active_editor(&baseline)
            .filter(|editor| editor.empty_cursor_position)
            .ok_or_else(input_gate_failure)?;
        let fence = gated_snapshot(terminal, budget, false).await?;
        // Snapshot equality is the whole of the fence. `active_editor` is a
        // function of the snapshot alone, so a fence equal to the baseline
        // resolves the baseline's own editor -- already filtered to an empty
        // cursor position two statements above. The re-derivation that used to
        // stand here could not refuse a fence this line admitted, and a mutation
        // run proved it: replacing its `&&` with `||` changed no answer.
        if fence == baseline {
            return Ok((fence, baseline_editor));
        }
        if active_editor(&fence).is_none()
            && let Some(needs_input) = blocking_screen(&fence.visible_text)
        {
            return Err(needs_input_failure(needs_input));
        }
        // Nothing has been written yet, so a benign editor mutation can safely
        // restart Gate 1 within the same bounded budget.
    }
}

/// Exactly one acknowledged bracketed paste. An ambiguous or failed paste is
/// never repeated and never followed by Enter.
///
/// The two failing arms are deliberately not the same arm. A terminal that
/// answered with an error is ambiguous; a budget that ran out may instead be
/// the turn ending, which is [`InputGateBudget::expiry`]'s question and not
/// this function's.
async fn paste_once(
    terminal: &mut dyn TerminalSession,
    budget: &InputGateBudget,
    text: &str,
) -> DriverResult<()> {
    let remaining = budget.remaining(false)?;
    match tokio::time::timeout(remaining, terminal.paste(text)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(paste_ambiguity_failure()),
        Err(_) => Err(budget.expiry(paste_ambiguity_failure)),
    }
}

/// The sole Enter attempt, after a final recheck of both revocable authorities.
/// There is deliberately no retry: a lost response is ambiguous, so the caller
/// fails closed rather than submitting twice.
///
/// Every failure from here carries [`mark_enter_attempted`], including the
/// turn-deadline one. Which clock ran out changes what the caller should
/// *report*; it changes nothing about the byte that already left, and that byte
/// is the only thing [`clear_and_rebind`] is asking about.
async fn enter_once(
    terminal: &mut dyn TerminalSession,
    budget: &InputGateBudget,
) -> DriverResult<()> {
    if terminal.lease_lost() {
        return Err(DriverFailure::new(
            ErrorCode::DaemonLost,
            "private rmux lease was lost before Enter",
        )
        .retryable(true));
    }
    let remaining = budget.remaining(true)?;
    match tokio::time::timeout(remaining, terminal.enter()).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(enter_ambiguity_failure()),
        Err(_) => Err(mark_enter_attempted(budget.expiry(enter_ambiguity_failure))),
    }
}

/// The control channel's post-paste gate: prove the screen changed from the
/// pre-paste fence and then held still, and return the frame it settled on.
///
/// This is deliberately weaker than [`wait_for_stable_prompt_render`], which
/// additionally requires the cursor-correlated editor geometry to be preserved.
/// A slash command opens Claude's command menu. On 2.1.220/2.1.227 the composer
/// jumps UP and candidates paint below it; on 2.1.238 (macos) and 2.1.257
/// (linux) in a 24-row pane the composer stays at the bottom and candidates
/// paint ABOVE it. Either way the editor signature a prompt is proven by does
/// not exist on this screen.
///
/// What replaces it is not nothing. The frame this returns is handed to
/// [`prove_control_command_selection`], which proves from that same frame that
/// the entry Enter is about to select is the command that was typed. The
/// transcript proof afterwards — exactly one new transcript carrying exactly one
/// new session id — is still the authority on what happened, but it is a
/// detector, not a guard: MEASURED, the commands that sort next to `/clear`
/// mostly do not rotate anything, so a mis-selection reaches that authority as a
/// rebind timeout blamed on a missing file rather than as a named wrong command.
///
/// The read is [`TerminalSession::styled_screen`] rather than
/// [`TerminalSession::snapshot`] so that the frame proven stable and the frame
/// the selection is proven from are ONE capture. Two reads would leave a window
/// between them and the proof would describe a screen that had already been
/// replaced. The stability predicate itself is unchanged: it still compares the
/// plain-text view, so what "the screen held still" means here is the same
/// statement it is at every other gate.
///
/// This gate asserts NOTHING about where the menu is. It does not look at the
/// rows above or below the cursor, only at whether the plain-text frame
/// differs from the pre-paste fence and then holds still for `stable_for`. So
/// the menu-below (2.1.220) and menu-above (2.1.238/2.1.257) layouts are the
/// same thing to it — MEASURED, it settled on the 2.1.257 frame and the
/// refusal came from [`prove_control_command_selection`] alone — and the
/// geometry is decided in exactly one place, there.
async fn wait_for_stable_control_render(
    terminal: &mut dyn TerminalSession,
    budget: &InputGateBudget,
    baseline: &TerminalSnapshot,
    stable_for: Duration,
    poll_interval: Duration,
) -> DriverResult<StyledScreen> {
    let mut candidate: Option<(StyledScreen, TerminalSnapshot, Instant)> = None;
    loop {
        let screen = gated_styled_screen(terminal, budget).await?;
        let snapshot = screen.to_terminal_snapshot();
        if snapshot.revision != 0 && snapshot.revision != baseline.revision && &snapshot != baseline
        {
            let same_candidate = candidate
                .as_ref()
                .is_some_and(|(_, observed, _)| observed == &snapshot);
            if !same_candidate {
                candidate = Some((screen, snapshot, Instant::now()));
            }
            if let Some((settled, _, since)) = candidate.as_ref()
                && since.elapsed() >= stable_for
            {
                return Ok(settled.clone());
            }
        } else {
            if active_editor(&snapshot).is_none()
                && let Some(needs_input) = blocking_screen(&snapshot.visible_text)
            {
                return Err(needs_input_failure(needs_input));
            }
            candidate = None;
        }
        sleep_for_input_poll(budget, poll_interval, true).await?;
    }
}

/// MEASURED 2.1.220/2.1.227: the menu's lower rule sits on the row directly
/// below the composer. 2.1.238 (macos) and 2.1.257 (linux) still draw that
/// lower rule (and an upper one, the row directly above the composer);
/// candidates may sit below the lower rule or above the upper one.
const MENU_RULE_ROWS_BELOW_COMPOSER: u16 = 1;

/// The glyph Claude Code rules the command menu off with (U+2500).
const MENU_RULE_GLYPH: char = '─';

/// Cells between the prompt glyph and the first typed character. The composer
/// renders `❯ `, so the caret of an n-character command sits at
/// `prompt_col + 2 + n` — MEASURED at column 8 for `❯ /clear`. It is the same
/// offset [`active_editor`] calls an empty cursor position.
const COMPOSER_TEXT_OFFSET: u16 = 2;

/// MEASURED: a candidate token starts within this many leading ASCII spaces.
/// 2.1.220/2.1.227 start at column 0; 2.1.238 and 2.1.257 indent two spaces.
/// Wrapped description lines start around column 30 (2.1.220) or exactly at
/// column 32 (2.1.257, `  /clear` + 30-column description field) and must not
/// count as candidates even when they contain a solidus (` /resume)`).
const MAX_MENU_TOKEN_INDENT: usize = 2;

/// One rendered menu row that offers a command.
struct MenuCandidate {
    row: u16,
    /// The command token, without leading indent.
    token: String,
}

/// The pre-Enter proof: the entry the menu will select is the command pmux
/// typed, or nothing is submitted.
///
/// # What was measured
///
/// Claude Code 2.1.220 in a 24x80 private rmux pane, `--cell minified`,
/// `--disallowedTools '*'`, read through the same `PaneSnapshot` this crate's
/// backend reads. Typing or pasting a slash command opens a menu. On
/// 2.1.220/2.1.227 the composer jumps UP and candidates paint directly under
/// it, tokens at column 0:
///
/// ```text
/// r09 ❯ /clear                                  <- composer, cursor at col 8
/// r10 ────────────────────────────────────      <- rule, full pane width
/// r11 /clear        Start a new session with…    <- candidate 0, SELECTED
/// r13 /code-review  Review the current diff…     <- candidate 1
/// r15 /simplify     Review the changed code…     <- candidate 2
/// ```
///
/// On 2.1.238 (macos) in a 24x120 pane the composer stays at the bottom, the
/// same U+2500 box still surrounds it, and candidates paint ABOVE the upper
/// rule with a two-space indent. Unselected rows are also a uniform colour
/// (they were mixed on 2.1.227), so "this row is uniform" is no longer the
/// selection. The selected row is the unique candidate whose body colour
/// equals the composer's typed-command colour — compared, never named.
///
/// MEASURED again on linux/x86_64 at 2.1.257, 24x120, recorded by this crate's
/// own screen-corpus recorder at site `control_channel.selection`
/// (`tests/corpus/claude-2.1.257-clear-menu.ndjson`, replayed through this
/// function by `tests/screen_corpus_replay.rs`). The rule is the row directly
/// above the composer; the candidates are `  /clear` (selected) and
/// `  /code-review`, each wrapping its description onto a continuation row
/// indented 32 spaces:
///
/// ```text
/// r16   /clear                        Start a new session with empty context; …   SELECTED
/// r17                                 /resume)                                     (continuation)
/// r18   /code-review                  Review the current diff, or a PR number/…
/// r19                                 reuse/simplification/efficiency cleanups …   (continuation)
/// r20 ──────────────────────────────────────────── (full 120 cols)
/// r21 ❯ /clear                                      <- composer, cursor at col 8
/// r22 ──────────────────────────────────────────── (full 120 cols)
/// r23   ⏵⏵ don't ask on (shift+tab to cycle)                              /rc active
/// ```
///
/// The selected row and the composer's typed `/clear` share one opaque
/// foreground encoding (`45201913`, as rmux reports it); the unselected
/// `/code-review` row is uniform in another (`43620761`). The continuation of
/// the selected entry (row 17) carries the selection colour too and is
/// excluded by its indent, not by its colour. 2.1.236 — the linux ceiling
/// promoted before either menu-above measurement — was observed on 2026-09-01
/// to refuse the below-only proof with `menu_not_rendered`, so the move
/// happened somewhere in 2.1.228..=2.1.236 and that promotion never recycled
/// a cell through `/clear`: the pool fell back to a per-turn relaunch after
/// the answer was already delivered.
///
/// Enter selects the HIGHLIGHTED ENTRY, not the composer text. That is
/// submitted evidence, not inference: with `/c` in the composer and `/cd`
/// highlighted, Enter ran `/cd` and printed `Usage: /cd <path>`. Because `/cd`
/// does not rotate the transcript, the caller would have seen that as a rebind
/// timeout blamed on a file that never appeared. Even the fully typed `/clear`
/// still leaves five candidates — `/code-review`, `/simplify`, `/doctor` and
/// `/run-skill-generator` alongside it — three of which are prompt-expanding
/// skills that would spend a model turn on selection.
///
/// # How the selection is visible, and why this reads cells
///
/// The selected row is marked by a FOREGROUND COLOUR and nothing else: no
/// reverse video, no background, no attribute bit, no `❯`/`>`/`*` marker, and
/// no change of indentation. In `visible_text` a selected row and an unselected
/// one are indistinguishable in kind, so a proof built on the plain-text
/// snapshot cannot see the selection at all — the evidence is not hard to read
/// there, it is absent. Hence [`StyledScreen`].
///
/// The discriminator is NOT "this row contains the highlight colour": unselected
/// rows contain it too, on the characters the filter matched (`/copy` at prefix
/// `/c` renders its `c` in the same colour). On 2.1.220/2.1.227 the selected
/// row is one colour from column 0 through the last glyph, blanks between token
/// and description included, while an unselected row leaves those blanks at the
/// terminal default. On 2.1.238 and 2.1.257 unselected rows are also uniform
/// (in a different colour) and tokens indent two unstyled spaces, so uniformity alone is not
/// the selection: the selected candidate is the unique one whose body colour
/// equals the composer's typed-command colour. Compared, never named. A theme
/// change degrades this to a refusal, never to a wrong answer.
///
/// # What this does NOT rule out
///
/// **Which `/clear`.** MEASURED: a project command at `.claude/commands/clear.md`
/// is offered as a second entry also named `/clear`, and it sorts ABOVE the
/// built-in. Its row is the highlighted one, its token is exactly `/clear`, and
/// pressing Enter served a real model turn instead of rotating the transcript.
/// Every check below passes on that screen. The only thing that distinguishes
/// the two entries anywhere on the screen is description prose, which is not
/// something to pin an assertion to. Bounding it is a LAUNCH-side job — no user,
/// project or plugin command may shadow `/clear` — and the shipped launch bundle
/// does not do it today. The test
/// `a_project_command_that_shadows_clear_is_not_ruled_out_by_this_proof` states
/// that out loud against the captured screen.
///
/// **That the corpus is unchanged between this frame and Enter.** This proves
/// one settled capture. It cannot prove the next write lands on the same one.
/// The gap is bounded by Enter following immediately, not eliminated.
///
/// **That a menu-less composer would have been wrong.** MEASURED: for roughly
/// 14–32 ms after a paste the composer holds a complete `/clear` and no menu has
/// painted yet, and Enter in that window still executes `/clear` correctly —
/// the filter is computed on input and only the paint lags. This refuses that
/// frame anyway, because an absent menu is not evidence about the selection.
pub fn prove_control_command_selection(
    screen: &StyledScreen,
    command: ControlCommand,
) -> DriverResult<()> {
    let literal = command.literal();
    let composer_row = proven_composer_row(screen, literal)?;
    let candidates = menu_candidates(screen, composer_row);
    if candidates.is_empty() {
        return Err(control_selection_refusal("menu_not_rendered"));
    }
    let Some(command_colour) = composer_command_colour(screen, composer_row) else {
        return Err(control_selection_refusal_with(
            "menu_selection_not_unique",
            "highlighted_rows",
            0.into(),
        ));
    };
    let highlighted: Vec<&MenuCandidate> = candidates
        .iter()
        .filter(|candidate| {
            candidate_body_colour(screen.row(candidate.row)) == Some(command_colour)
        })
        .collect();
    let [selected] = highlighted.as_slice() else {
        return Err(control_selection_refusal_with(
            "menu_selection_not_unique",
            "highlighted_rows",
            highlighted.len().into(),
        ));
    };
    if selected.token != literal {
        return Err(control_selection_refusal_with(
            "menu_selects_a_different_command",
            "selected_command",
            redacted_schema_token(&selected.token).into(),
        ));
    }
    Ok(())
}

/// Locates the composer by the cursor and proves it holds exactly `literal`.
///
/// The cursor is the anchor rather than a text search, and the caret column is
/// checked rather than only the text, because `row_text` right-trims: `❯ /clear`
/// and `❯ /clear` followed by spaces render identically, and a text-only check
/// would accept a composer holding content the trim removed. The caret is the
/// part that cannot be trimmed away.
fn proven_composer_row(screen: &StyledScreen, literal: &str) -> DriverResult<u16> {
    let cursor = screen
        .cursor
        .filter(|cursor| cursor.visible && cursor.row < screen.rows)
        .ok_or_else(|| control_selection_refusal("composer_not_rendered"))?;
    let rendered = screen.row_text(cursor.row);
    let prompt_col = prompt_glyph_col(&rendered)
        .ok_or_else(|| control_selection_refusal("composer_not_rendered"))?;
    let caret = u16::try_from(literal.chars().count())
        .ok()
        .and_then(|typed| {
            prompt_col
                .checked_add(COMPOSER_TEXT_OFFSET)?
                .checked_add(typed)
        })
        .ok_or_else(|| control_selection_refusal("composer_text_unproven"))?;
    let typed: String = rendered
        .chars()
        .skip(usize::from(prompt_col) + usize::from(COMPOSER_TEXT_OFFSET))
        .collect();
    if typed != literal || cursor.col != caret {
        return Err(control_selection_refusal("composer_text_unproven"));
    }
    Ok(cursor.row)
}

/// MEASURED: the menu is ruled off from the composer by a row of U+2500 the full
/// width of the pane. The idle composer is boxed by the same rule, which is why
/// a rule alone proves nothing and at least one candidate row is also required.
fn is_menu_rule(rendered: &str) -> bool {
    !rendered.is_empty() && rendered.chars().all(|glyph| glyph == MENU_RULE_GLYPH)
}

/// Candidate rows on either side of the composer box.
///
/// The idle composer is boxed by the same U+2500 rule, so a rule is necessary
/// and not sufficient: candidates are the rows BELOW the lower rule (2.1.220/
/// 2.1.227) and/or ABOVE the upper rule (2.1.238, 2.1.257). Without an adjacent rule,
/// leftover slash-prefixed text is not a menu. Wrapped description lines are
/// indented past [`MAX_MENU_TOKEN_INDENT`] and are not candidates, so a
/// highlighted wrap does not count as a second selected entry.
fn menu_candidates(screen: &StyledScreen, composer_row: u16) -> Vec<MenuCandidate> {
    let mut rows = Vec::new();
    if let Some(below) = composer_row.checked_add(MENU_RULE_ROWS_BELOW_COMPOSER)
        && below < screen.rows
        && is_menu_rule(&screen.row_text(below))
        && let Some(first) = below.checked_add(1)
    {
        rows.extend(first..screen.rows);
    }
    if let Some(above) = composer_row.checked_sub(1)
        && is_menu_rule(&screen.row_text(above))
    {
        rows.extend(0..above);
    }
    rows.sort_unstable();
    rows.dedup();
    rows.into_iter()
        .filter_map(|row| menu_candidate_at(screen, row))
        .collect()
}

fn menu_candidate_at(screen: &StyledScreen, row: u16) -> Option<MenuCandidate> {
    let rendered = screen.row_text(row);
    let indent = rendered.chars().take_while(|glyph| *glyph == ' ').count();
    if indent > MAX_MENU_TOKEN_INDENT {
        return None;
    }
    let stripped = rendered.trim_start_matches(' ');
    if !stripped.starts_with('/') {
        return None;
    }
    Some(MenuCandidate {
        row,
        token: stripped.split_whitespace().next()?.to_owned(),
    })
}

/// The explicit colour of the first glyph of the typed command in the composer.
///
/// Used as the identity of "selected" so a theme is never named. On 2.1.238
/// and 2.1.257 unselected candidate rows are also a uniform colour, so
/// uniformity alone cannot mark the selection; matching this colour can.
/// MEASURED at 2.1.220 (`❯ /clear` is `[2..7 fg=idx153]`, the selected row's
/// colour) and at 2.1.257 (`[2..7 fg=45201913]`, likewise).
fn composer_command_colour(screen: &StyledScreen, composer_row: u16) -> Option<CellColor> {
    let rendered = screen.row_text(composer_row);
    let prompt_col = prompt_glyph_col(&rendered)?;
    let start = usize::from(prompt_col.checked_add(COMPOSER_TEXT_OFFSET)?);
    let colour: CellColor = screen.row(composer_row).get(start)?.foreground;
    colour.is_styled().then_some(colour)
}

/// Uniform explicit colour of a candidate row's body: leading unstyled indent
/// skipped, then one colour from the token through the last glyph, blanks
/// between token and description included.
///
/// 2.1.220/2.1.227 selected rows have no indent. 2.1.238 and 2.1.257 selected
/// rows indent two unstyled spaces and then paint the rest as one run.
fn candidate_body_colour(cells: &[StyledCell]) -> Option<CellColor> {
    let last_glyph = cells
        .iter()
        .rposition(|cell| !cell.is_padding() && !cell.text.trim().is_empty())?;
    let start = cells.iter().take(last_glyph + 1).position(|cell| {
        if cell.is_padding() {
            return false;
        }
        cell.foreground.is_styled() || !cell.text.chars().all(char::is_whitespace)
    })?;
    let mut colour: Option<CellColor> = None;
    for cell in cells.iter().take(last_glyph + 1).skip(start) {
        if cell.is_padding() {
            continue;
        }
        match colour {
            None => colour = Some(cell.foreground),
            Some(seen) if seen == cell.foreground => {}
            Some(_) => return None,
        }
    }
    colour.filter(|colour| colour.is_styled())
}

/// The refusal every unproven selection produces: one code, one message, and a
/// `reason` naming which check refused. Nothing was submitted when this is
/// raised, so it deliberately does not carry `enter_attempted`.
fn control_selection_refusal(reason: &'static str) -> DriverFailure {
    DriverFailure::new(
        ErrorCode::PromptNotAcknowledged,
        "the menu entry Enter would select was not proven to be the typed command",
    )
    .with_details(json!({
        "field": "terminal",
        "violation": "control_command_selection_unproven",
        "reason": reason,
    }))
}

fn control_selection_refusal_with(
    reason: &'static str,
    key: &'static str,
    value: serde_json::Value,
) -> DriverFailure {
    let mut refusal = control_selection_refusal(reason);
    if let serde_json::Value::Object(details) = &mut refusal.details {
        details.insert(key.to_owned(), value);
    }
    refusal
}

async fn wait_for_snapshot_stability(
    terminal: &mut dyn TerminalSession,
    first: TerminalSnapshot,
    stable_for: Duration,
    timeout: Duration,
    poll_interval: Duration,
) -> DriverResult<Option<TerminalSnapshot>> {
    let deadline = Instant::now().checked_add(timeout).ok_or_else(|| {
        DriverFailure::new(
            ErrorCode::InvalidConfig,
            "terminal stability timeout overflows the monotonic clock domain",
        )
    })?;
    let mut candidate = first;
    let mut stable_since = Instant::now();
    loop {
        if terminal.lease_lost() {
            return Err(DriverFailure::new(
                ErrorCode::DaemonLost,
                "private rmux lease was lost while observing terminal stability",
            )
            .retryable(true));
        }
        if stable_since.elapsed() >= stable_for {
            return Ok(Some(candidate));
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(None);
        }
        tokio::time::sleep(poll_interval.min(deadline - now)).await;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(None);
        }
        let read = tokio::time::timeout(remaining, terminal.snapshot()).await;
        let snapshot = match read {
            Ok(result) => result.map_err(|error| map_terminal_error(terminal, error))?,
            Err(_) => return Ok(None),
        };
        crate::screen_corpus::record_snapshot(SCREEN_STABILITY_SITE, &snapshot);
        if snapshot != candidate {
            candidate = snapshot;
            stable_since = Instant::now();
        }
    }
}

/// Terminal-side implementation used by one v1 actor.
pub struct RmuxTerminalControl {
    terminal: Mutex<Option<Box<dyn TerminalSession>>>,
    quiet_for: Duration,
    evidence_timeout: Duration,
    recovery_timeout: Duration,
    input_poll_interval: Duration,
    input_gate_timeout: Duration,
    lifecycle_expected: bool,
    lifecycle_stop_sequence: Arc<AtomicU64>,
    /// UNIX-millisecond instant of the most recent Stop/StopFailure hook, or
    /// `0` when none has ever been observed. Written by the lifecycle task
    /// before it bumps `lifecycle_stop_sequence`; read only through
    /// [`RmuxTerminalControl::lifecycle_hook`].
    lifecycle_stop_at_ms: Arc<AtomicU64>,
    lifecycle_baseline: AtomicU64,
}

impl RmuxTerminalControl {
    #[must_use]
    pub fn new(terminal: Box<dyn TerminalSession>) -> Self {
        Self {
            terminal: Mutex::new(Some(terminal)),
            quiet_for: SCREEN_QUIET_FOR,
            evidence_timeout: Duration::from_millis(400),
            recovery_timeout: Duration::from_secs(5),
            input_poll_interval: TERMINAL_POLL_INTERVAL,
            input_gate_timeout: INPUT_GATE_MAX_DURATION,
            lifecycle_expected: false,
            lifecycle_stop_sequence: Arc::new(AtomicU64::new(0)),
            lifecycle_stop_at_ms: Arc::new(AtomicU64::new(0)),
            lifecycle_baseline: AtomicU64::new(0),
        }
    }

    #[must_use]
    pub fn with_timings(
        mut self,
        quiet_for: Duration,
        evidence_timeout: Duration,
        recovery_timeout: Duration,
    ) -> Self {
        self.quiet_for = quiet_for;
        self.evidence_timeout = evidence_timeout;
        self.recovery_timeout = recovery_timeout;
        self
    }

    #[cfg(test)]
    #[must_use]
    fn with_input_gate_timings(
        mut self,
        stable_for: Duration,
        poll_interval: Duration,
        gate_timeout: Duration,
    ) -> Self {
        self.quiet_for = stable_for;
        self.input_poll_interval = poll_interval;
        self.input_gate_timeout = gate_timeout;
        self
    }

    #[must_use]
    pub fn with_lifecycle_observation(
        mut self,
        stop_sequence: Arc<AtomicU64>,
        stop_at_ms: Arc<AtomicU64>,
    ) -> Self {
        self.lifecycle_expected = true;
        self.lifecycle_stop_sequence = stop_sequence;
        self.lifecycle_stop_at_ms = stop_at_ms;
        self
    }

    /// Lifecycle-hook evidence for the turn armed by the last `submit_prompt`:
    /// whether a Stop hook arrived since the baseline, and when.
    ///
    /// Both halves come from one sequence read, and the instant is reported
    /// only when that read says a hook arrived. The stamp is session-scoped and
    /// survives across turns, so publishing it unconditionally would pair an
    /// earlier turn's Stop with this turn's transcript activity — the exact
    /// comparison `TurnTimings::stop_hook_at_ms` exists to make, silently
    /// wrong. Reading the sequence with `Acquire` before the stamp pairs with
    /// the writer's stamp-then-bump order, so the instant is never older than
    /// the observed sequence.
    fn lifecycle_hook(&self) -> (bool, Option<u64>) {
        let observed = self.lifecycle_stop_sequence.load(Ordering::Acquire)
            > self.lifecycle_baseline.load(Ordering::Acquire);
        let at_ms = observed
            .then(|| self.lifecycle_stop_at_ms.load(Ordering::Acquire))
            .filter(|stamped| *stamped != 0);
        (observed, at_ms)
    }

    /// Types one privileged control command and sends exactly one Enter.
    ///
    /// Private on purpose. The only caller is [`clear_and_rebind`], which cannot
    /// type a `/clear` without also snapshotting the project directory first and
    /// recording the outcome afterwards. Publishing this method would make it
    /// possible to abandon a transcript and leave nothing that knows it happened
    /// — the bare `TurnTimeout` this work exists to eliminate.
    ///
    /// The lifecycle baseline is deliberately untouched: it is armed by
    /// `submit_prompt` and describes one turn's Stop hook. A control command
    /// happens between turns and has no turn to describe.
    async fn type_control_command(
        &self,
        command: ControlCommand,
        deadline_unix_ms: u64,
    ) -> DriverResult<()> {
        let budget = InputGateBudget::new(deadline_unix_ms, self.input_gate_timeout)?;
        let mut terminal = self.terminal.lock().await;
        let terminal = terminal.as_mut().ok_or_else(closed_terminal)?;

        let (pre_paste_fence, _) = prove_stable_empty_editor(
            terminal.as_mut(),
            &budget,
            self.quiet_for,
            self.input_poll_interval,
        )
        .await?;

        paste_once(terminal.as_mut(), &budget, command.literal()).await?;

        // Gate 2. The frame this settles on is the frame the selection is proven
        // from — one capture, so the proof cannot describe a screen that has
        // already been replaced.
        let rendered = wait_for_stable_control_render(
            terminal.as_mut(),
            &budget,
            &pre_paste_fence,
            self.quiet_for,
            self.input_poll_interval,
        )
        .await?;

        // Gate 3, the precondition Enter has no way to take back. A refusal here
        // is final: there is no second look and no retry, because a screen that
        // could not be proven once is not made provable by asking again, and the
        // only thing a retry could buy is a screen that has drifted into looking
        // right.
        prove_control_command_selection(&rendered, command)?;

        enter_once(terminal.as_mut(), &budget).await
    }

    pub async fn resize(&self, rows: u16, cols: u16) -> DriverResult<()> {
        if rows == 0 || cols == 0 {
            return Err(DriverFailure::new(
                ErrorCode::InvalidConfig,
                "terminal dimensions must be non-zero",
            ));
        }
        let mut terminal = self.terminal.lock().await;
        let terminal = terminal.as_mut().ok_or_else(closed_terminal)?;
        let resized = terminal.resize(rows, cols).await;
        resized.map_err(|error| map_terminal_error(&**terminal, error))
    }
}

#[async_trait]
impl TerminalControl for RmuxTerminalControl {
    async fn submit_prompt(
        &self,
        _session_id: SessionId,
        _turn_id: TurnId,
        prompt: &str,
        deadline_unix_ms: u64,
    ) -> DriverResult<()> {
        let prompt = validate_prompt(prompt)?;
        let budget = InputGateBudget::new(deadline_unix_ms, self.input_gate_timeout)?;
        self.lifecycle_baseline.store(
            self.lifecycle_stop_sequence.load(Ordering::Acquire),
            Ordering::Release,
        );
        let mut terminal = self.terminal.lock().await;
        let terminal = terminal.as_mut().ok_or_else(closed_terminal)?;

        // Gate 1, under the same terminal mutex that will perform the write.
        let (pre_paste_fence, baseline_editor) = prove_stable_empty_editor(
            terminal.as_mut(),
            &budget,
            self.quiet_for,
            self.input_poll_interval,
        )
        .await?;

        paste_once(terminal.as_mut(), &budget, &prompt).await?;

        // Gate 2: a screen revision is necessary but insufficient. Prove that
        // the cursor moved relative to the active prompt anchor, that the
        // cursor-correlated editor rendering changed and remained stable, and
        // that the composer's first row is THIS PROMPT'S head and not some
        // other text that happened to move the same cursor.
        loop {
            let rendered = wait_for_stable_prompt_render(
                terminal.as_mut(),
                &budget,
                &pre_paste_fence,
                &baseline_editor,
                &prompt,
                self.quiet_for,
                self.input_poll_interval,
            )
            .await?;
            let rendered_signature = active_editor(&rendered)
                .expect("stable prompt render requires an active editor")
                .signature;
            let fence = gated_snapshot(terminal.as_mut(), &budget, true).await?;
            if fence == rendered
                && rendered_prompt_is_proven(
                    &fence,
                    pre_paste_fence.revision,
                    &baseline_editor,
                    &prompt,
                )
                && active_editor(&fence)
                    .is_some_and(|editor| editor.signature == rendered_signature)
            {
                break;
            }
            if active_editor(&fence).is_none()
                && let Some(needs_input) = blocking_screen(&fence.visible_text)
            {
                return Err(needs_input_failure(needs_input));
            }
            // Paste is never repeated. A changing editor simply has to
            // stabilize and pass a fresh immediate fence within the same cap.
        }

        enter_once(terminal.as_mut(), &budget).await
    }

    async fn completion_evidence(
        &self,
        _session_id: SessionId,
        _turn_id: TurnId,
    ) -> DriverResult<TerminalEvidence> {
        let mut terminal = self.terminal.lock().await;
        let terminal = terminal.as_mut().ok_or_else(closed_terminal)?;
        if terminal.lease_lost() {
            return Err(
                DriverFailure::new(ErrorCode::DaemonLost, "private rmux lease was lost")
                    .retryable(true),
            );
        }

        let read = terminal.snapshot().await;
        let snapshot = read.map_err(|error| map_terminal_error(&**terminal, error))?;
        // Snapshot acquisition itself can observe the final lease transition.
        // Recheck before interpreting even a modal snapshot; otherwise the
        // early modal return could mask a control-plane loss as ordinary
        // negative readiness evidence.
        if terminal.lease_lost() {
            return Err(
                DriverFailure::new(ErrorCode::DaemonLost, "private rmux lease was lost")
                    .retryable(true),
            );
        }
        crate::screen_corpus::record_snapshot(COMPLETION_GATE_EVIDENCE_SITE, &snapshot);
        if matches!(
            classify_terminal_snapshot(&snapshot),
            TerminalScreenState::NeedsInput(_)
        ) {
            let (lifecycle_hook_observed, lifecycle_hook_at_ms) = self.lifecycle_hook();
            return Ok(TerminalEvidence {
                lifecycle_expected: self.lifecycle_expected,
                lifecycle_hook_observed,
                lifecycle_hook_at_ms,
                ..TerminalEvidence::default()
            });
        }
        let mut ready_prompt = matches!(
            classify_terminal_snapshot(&snapshot),
            TerminalScreenState::Ready
        );
        let quiet = match wait_for_snapshot_stability(
            terminal.as_mut(),
            snapshot,
            self.quiet_for,
            self.evidence_timeout,
            TERMINAL_POLL_INTERVAL,
        )
        .await?
        {
            Some(stable) => {
                let stable_state = classify_terminal_snapshot(&stable);
                if matches!(stable_state, TerminalScreenState::NeedsInput(_)) {
                    let (lifecycle_hook_observed, lifecycle_hook_at_ms) = self.lifecycle_hook();
                    return Ok(TerminalEvidence {
                        lifecycle_expected: self.lifecycle_expected,
                        lifecycle_hook_observed,
                        lifecycle_hook_at_ms,
                        ..TerminalEvidence::default()
                    });
                }
                ready_prompt = matches!(stable_state, TerminalScreenState::Ready);
                true
            }
            // A quiet timeout is negative evidence, not a driver failure. The
            // actor will poll again while the transcript remains terminal.
            None => false,
        };
        let (lifecycle_hook_observed, lifecycle_hook_at_ms) = self.lifecycle_hook();
        Ok(TerminalEvidence {
            ready_prompt,
            quiet,
            lifecycle_expected: self.lifecycle_expected,
            lifecycle_hook_observed,
            lifecycle_hook_at_ms,
        })
    }

    async fn observe_screen(
        &self,
        _session_id: SessionId,
    ) -> DriverResult<TerminalScreenObservation> {
        // `as_mut` rather than `as_ref`, purely so the handle survives the
        // await: `dyn TerminalSession` is `Send` but not `Sync`, so a shared
        // `&Box<dyn TerminalSession>` held across an await point would make
        // this future non-`Send`, while the unique borrow is fine. Nothing
        // below mutates through it.
        let mut terminal = self.terminal.lock().await;
        let terminal = terminal.as_mut().ok_or_else(closed_terminal)?;
        if terminal.lease_lost() {
            return Err(
                DriverFailure::new(ErrorCode::DaemonLost, "private rmux lease was lost")
                    .retryable(true),
            );
        }
        let read = terminal.snapshot().await;
        let snapshot = read.map_err(|error| map_terminal_error(&**terminal, error))?;
        crate::screen_corpus::record_snapshot(TURN_MONITOR_SITE, &snapshot);
        // One arm per state, and no `_`: the redaction boundary is the ONLY
        // thing this match performs, so a state added to the classifier must be
        // given a redacted spelling here rather than silently collapsing into
        // whichever arm a wildcard happened to name.
        Ok(match classify_terminal_snapshot(&snapshot) {
            TerminalScreenState::Ready => TerminalScreenObservation::Ready,
            TerminalScreenState::NeedsInput(needs_input) => {
                TerminalScreenObservation::NeedsInput(needs_input)
            }
            TerminalScreenState::Recognised(recognised) => {
                TerminalScreenObservation::Recognised(recognised)
            }
            TerminalScreenState::Unrecognised(shape) => {
                TerminalScreenObservation::Unrecognised(shape)
            }
        })
    }

    async fn interrupt(
        &self,
        _session_id: SessionId,
        _turn_id: TurnId,
    ) -> DriverResult<InterruptRecovery> {
        let mut terminal = self.terminal.lock().await;
        let terminal = terminal.as_mut().ok_or_else(closed_terminal)?;
        // Exactly one interrupt is sent. Ambiguous recovery is never retried.
        let interrupted = terminal.interrupt().await;
        interrupted.map_err(|error| map_terminal_error(&**terminal, error))?;
        let deadline = Instant::now()
            .checked_add(self.recovery_timeout)
            .ok_or_else(|| {
                DriverFailure::new(
                    ErrorCode::InvalidConfig,
                    "terminal recovery timeout overflows the monotonic clock domain",
                )
            })?;
        while Instant::now() < deadline {
            if terminal.lease_lost() {
                return Ok(InterruptRecovery::RecoveryFailed);
            }
            if let Ok(snapshot) = terminal.snapshot().await
                && {
                    crate::screen_corpus::record_snapshot(INTERRUPT_RECOVERY_SITE, &snapshot);
                    matches!(
                        classify_terminal_snapshot(&snapshot),
                        TerminalScreenState::Ready
                    )
                }
            {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if matches!(
                    wait_for_snapshot_stability(
                        terminal.as_mut(),
                        snapshot,
                        self.quiet_for,
                        self.evidence_timeout.min(remaining),
                        TERMINAL_POLL_INTERVAL,
                    )
                    .await,
                    Ok(Some(stable))
                        if matches!(
                            classify_terminal_snapshot(&stable),
                            TerminalScreenState::Ready
                        )
                ) {
                    return Ok(InterruptRecovery::RecoveredToReady);
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Ok(InterruptRecovery::RecoveryFailed)
    }

    async fn close(&self, _session_id: SessionId, _policy: ClosePolicy) -> DriverResult<bool> {
        let mut terminal = self.terminal.lock().await;
        let Some(active) = terminal.as_mut() else {
            return Ok(true);
        };
        let closed = active.close().await;
        let process_reaped = closed.map_err(|error| map_terminal_error(&**active, error))?;
        if process_reaped {
            *terminal = None;
        }
        Ok(process_reaped)
    }
}

/// Incremental filesystem implementation of the actor transcript boundary.
///
/// The source is bound to one config root and one cwd for its whole life, but
/// only to one session id for the length of a single arm. A Claude session can
/// rotate its id in place — `/clear` abandons the current transcript, leaving
/// its inode and its length untouched forever, and opens a new one under a new
/// UUID — so the id the caller passes to `arm_at_eof`/`poll` is the authority,
/// not the id this source happened to be constructed with.
pub struct FileTranscriptSource {
    locator: TranscriptLocator,
    parser: JsonlParser,
    expected_cwd: String,
    state: StdMutex<TailState>,
    /// Session ids this source watched being abandoned by a `/clear`, newest
    /// last. Held under its own lock, and never acquired while `state` is held,
    /// so the two never order against each other.
    rotations: StdMutex<VecDeque<RotationRecord>>,
    rebind_timeout: Duration,
    rebind_poll_interval: Duration,
}

impl FileTranscriptSource {
    pub fn new(
        config_root: impl Into<PathBuf>,
        cwd: impl AsRef<Path>,
        session_id: SessionId,
    ) -> Result<Self, TranscriptLocationError> {
        let locator = TranscriptLocator::new(config_root, cwd, session_id.to_string())?;
        let expected_cwd = normalize_path(locator.canonical_cwd());
        Ok(Self {
            locator,
            parser: JsonlParser::new(ParseMode::Strict),
            expected_cwd,
            // The launch identity, until the first arm says otherwise. Nothing
            // may be read under it: the tail owns the id it is armed on so the
            // id and the cursor can never disagree.
            state: StdMutex::new(TailState::new(session_id)),
            rotations: StdMutex::new(VecDeque::new()),
            rebind_timeout: CLEAR_REBIND_TIMEOUT,
            rebind_poll_interval: ROTATION_POLL_INTERVAL,
        })
    }

    /// The Claude configuration root this source reads transcripts under.
    ///
    /// Bound for the source's whole life, unlike the session id, so it answers
    /// "which root is this live session using" without consulting the actor.
    #[must_use]
    pub fn config_root(&self) -> &Path {
        self.locator.config_root()
    }

    /// The canonical working directory this live session's transcripts are
    /// located under.
    ///
    /// Also bound for the source's whole life, and canonical: `/clear` rotates
    /// the session id, never the project directory, so this answers "which cwd
    /// is this live session using" for every generation of it.
    #[must_use]
    pub fn cwd(&self) -> &Path {
        self.locator.canonical_cwd()
    }

    #[cfg(test)]
    #[must_use]
    fn with_rebind_timings(mut self, timeout: Duration, poll_interval: Duration) -> Self {
        self.rebind_timeout = timeout;
        self.rebind_poll_interval = poll_interval;
        self
    }

    /// Records the transcript listing of one instance's project directory
    /// immediately before `/clear` is typed.
    ///
    /// Not public: the listing is only meaningful when it is paired with the
    /// command that rotates the directory, and [`clear_and_rebind`] is the one
    /// place that pairs them.
    ///
    /// The directory watched is the one the bound transcript actually lives in,
    /// taken from the locator rather than recomputed, so the rebind cannot end
    /// up watching a directory the tail was never reading. A transcript that
    /// cannot be located has no directory to watch and no id to abandon, so this
    /// refuses rather than guessing where a rotation would land.
    pub(crate) fn watch_rotation(&self, session_id: SessionId) -> DriverResult<RotationWatch> {
        let located = self
            .locator
            .locate_for(&session_id.to_string())
            .map_err(map_location_error)?;
        let before = list_transcripts(&located.project_directory)?;
        Ok(RotationWatch {
            abandoned: session_id,
            before_session_ids: before
                .iter()
                .filter_map(|path| {
                    path.file_stem()
                        .map(|stem| stem.to_string_lossy().to_lowercase())
                })
                .collect(),
            project_directory: located.project_directory,
            before,
            started: Instant::now(),
        })
    }

    /// Waits for the transcript `/clear` opens and returns the session id it
    /// carries.
    ///
    /// Exactly one new transcript carrying exactly one new session id binds.
    /// Zero refuses when the deadline expires, and two or more refuse
    /// immediately. There is no newest-file or mtime tiebreak here on purpose:
    /// the file this picks becomes the sole semantic authority for every
    /// following turn, and a completion authority does not guess.
    ///
    /// Either outcome records the abandonment, because the clear has already
    /// been typed by the time this runs. A refusal that left the ledger empty
    /// would leave the tail quietly following a file that will never grow again
    /// — the failure mode this whole path exists to name.
    pub(crate) async fn resolve_rotation(
        &self,
        mut watch: RotationWatch,
    ) -> DriverResult<SessionId> {
        // The +39ms appearance is measured from Enter, so the deadline runs from
        // here -- this is called the moment the command is submitted. The
        // listing's own age is irrelevant (it is a snapshot, not an
        // observation), but charging the input gate's time against the rebind
        // would silently shorten it.
        watch.started = Instant::now();
        let outcome = self.await_rotation(&watch).await;
        self.record_rotation(watch.abandoned, outcome.as_ref().ok().copied())?;
        outcome
    }

    async fn await_rotation(&self, watch: &RotationWatch) -> DriverResult<SessionId> {
        loop {
            if let Some(rotated) = self.observe_rotation(watch)? {
                return Ok(rotated);
            }
            if watch.started.elapsed() >= self.rebind_timeout {
                let observed = list_transcripts(&watch.project_directory)?;
                return Err(DriverFailure::new(
                    ErrorCode::TranscriptUnavailable,
                    "the transcript /clear opens never appeared; the rebind was refused",
                )
                .with_details(json!({
                    "field": "session_id",
                    "violation": "clear_rebind_not_observed",
                    "abandoned_session_id": watch.abandoned.to_string(),
                    "waited_ms": diagnostic_u64(protocol_milliseconds(
                        watch.started.elapsed().as_millis(),
                        "rebind wait duration",
                    )?),
                    "transcripts_before": watch.before.len(),
                    "transcripts_after": observed.len(),
                })));
            }
            tokio::time::sleep(self.rebind_poll_interval).await;
        }
    }

    /// One pass: `None` means no candidate is resolvable *yet*, which is not the
    /// same answer as a candidate that resolves wrongly.
    fn observe_rotation(&self, watch: &RotationWatch) -> DriverResult<Option<SessionId>> {
        let observed = list_transcripts(&watch.project_directory)?;
        let mut rotated: BTreeSet<SessionId> = BTreeSet::new();
        for path in observed.difference(&watch.before) {
            let Some(anchor) = read_rotation_anchor(path)? else {
                // Row 0 is not a complete JSONL record yet. MEASURED: the file
                // appears +39ms after Enter and its first row is written
                // immediately -- but immediately is not atomically, and a
                // partial first line is a not-yet answer, never a wrong one.
                continue;
            };
            if anchor == watch.abandoned || watch.before_session_ids.contains(&anchor.to_string()) {
                return Err(DriverFailure::new(
                    ErrorCode::SchemaDrift,
                    "a transcript that appeared after /clear carries a session id that already existed",
                )
                .with_details(json!({
                    "field": "session_id",
                    "violation": "clear_rebind_anchor_not_new",
                    "abandoned_session_id": watch.abandoned.to_string(),
                })));
            }
            rotated.insert(anchor);
        }

        match rotated.len() {
            0 => Ok(None),
            1 => Ok(rotated.into_iter().next()),
            // Waiting longer cannot unmake a second candidate, so this refuses
            // now rather than at the deadline.
            candidates => Err(DriverFailure::new(
                ErrorCode::SchemaDrift,
                "more than one transcript appeared after /clear; the rebind was refused",
            )
            .with_details(json!({
                "field": "session_id",
                "violation": "clear_rebind_ambiguous",
                "abandoned_session_id": watch.abandoned.to_string(),
                "candidates": candidates,
            }))),
        }
    }

    fn record_rotation(
        &self,
        abandoned: SessionId,
        successor: Option<SessionId>,
    ) -> DriverResult<()> {
        let mut rotations = self.rotations.lock().map_err(poisoned_tail)?;
        rotations.push_back(RotationRecord {
            abandoned,
            successor,
            observed_at: Instant::now(),
        });
        while rotations.len() > MAX_REMEMBERED_ROTATIONS {
            rotations.pop_front();
        }
        Ok(())
    }

    fn rotation_record(&self, session_id: SessionId) -> DriverResult<Option<RotationRecord>> {
        let rotations = self.rotations.lock().map_err(poisoned_tail)?;
        Ok(rotations
            .iter()
            .rev()
            .find(|record| record.abandoned == session_id)
            .cloned())
    }

    /// Proves the transcript bound to `session_id` carries no served work, and
    /// waits for the `/clear` echo that says this instance is the one that
    /// cleared it.
    ///
    /// The wait exists because `resolve_rotation` returns the instant row 0 is a
    /// complete record, which MEASURED is written in the same millisecond band
    /// as rows 1-4 but is not atomic with them. Refusing immediately on a
    /// half-written preamble would quarantine healthy instances on a race, so a
    /// missing echo or an unterminated trailing row is a not-yet answer until
    /// the rebind deadline. Every *other* refusal is immediate: waiting longer
    /// cannot unmake a semantic row or a wrong command name.
    ///
    /// It also waits for the preamble to go QUIET, which is a separate
    /// requirement from "the last record is terminated" and is load-bearing.
    ///
    /// `arm_at_eof` runs immediately after this returns, and it establishes the
    /// boundary from a `stat` and re-checks the length after the `open`: a
    /// preamble row landing between those two reads is a hard refusal, and on
    /// this path that refusal quarantines the session. Every intermediate state
    /// of a preamble being written row by row is a complete, terminated,
    /// individually-inert set of rows, so the trailing-partial check cannot see
    /// it. Requiring the byte length to hold still for
    /// [`ASSERT_EMPTY_QUIET_FOR`] is what removes the ordinary case.
    ///
    /// Stated plainly, because a check whose limits are unwritten gets trusted
    /// past them: this is a heuristic, not a proof. Nothing observable here can
    /// prove a writer has finished. What makes that acceptable is the direction
    /// of the residue -- a straggler row after the quiet window still fails
    /// closed at the arm, so the worst case is a refused clear rather than a
    /// boundary established over a file that is still moving.
    ///
    /// Bounded by the same deadline the rotation wait uses, and it runs between
    /// turns, so its whole cost is availability and never turn latency.
    pub(crate) async fn assert_empty_after_clear(
        &self,
        session_id: SessionId,
    ) -> DriverResult<EmptinessProof> {
        let started = Instant::now();
        let mut quiet_since: Option<(u64, Instant)> = None;
        loop {
            let settled = self.assert_empty_at(session_id)?;
            match (&settled, quiet_since) {
                (Some(proof), Some((bytes, since)))
                    if proof.bytes == bytes
                        && proof.clear_command_seen
                        && proof.pending_bytes == 0
                        && since.elapsed() >= ASSERT_EMPTY_QUIET_FOR =>
                {
                    return Ok(*proof);
                }
                // Any change of length restarts the window, including a change
                // that only completed a partial row.
                (Some(proof), Some((bytes, _))) if proof.bytes == bytes => {}
                (Some(proof), _) => quiet_since = Some((proof.bytes, Instant::now())),
                (None, _) => quiet_since = None,
            }
            if started.elapsed() >= self.rebind_timeout {
                let waited_ms = protocol_milliseconds(
                    started.elapsed().as_millis(),
                    "assert-empty settle duration",
                )?;
                let (refusal, rows) = match &settled {
                    // The rows are clean and the echo is there; only the last
                    // record never finished being written. That is not a clear
                    // that landed somewhere else, it is a writer that stalled.
                    Some(proof) if proof.clear_command_seen => {
                        (AssertEmptyRefusal::PreambleNotSettled, Some(proof.rows))
                    }
                    Some(proof) => (AssertEmptyRefusal::ClearCommandMissing, Some(proof.rows)),
                    None => (AssertEmptyRefusal::ClearCommandMissing, None),
                };
                return Err(assert_empty_refusal_with(
                    refusal,
                    [
                        ("rows", json!(rows)),
                        ("waited_ms", json!(diagnostic_u64(waited_ms))),
                    ],
                ));
            }
            tokio::time::sleep(self.rebind_poll_interval).await;
        }
    }

    /// The launch half of the same predicate.
    ///
    /// A transcript that does not exist yet has served no work, so absence is a
    /// pass rather than a refusal: Claude creates the file lazily and a session
    /// admitted before it appears is trivially clean. The one extra bit this
    /// caller is entitled to assert is the mirror of the rebind's: a *launch*
    /// preamble carrying a `/clear` echo means the id resolution found a file
    /// this launch did not open.
    pub(crate) fn prove_empty_at_launch(&self, session_id: SessionId) -> DriverResult<()> {
        let Some(proof) = self.assert_empty_at(session_id)? else {
            return Ok(());
        };
        if proof.clear_command_seen {
            return Err(assert_empty_refusal_with(
                AssertEmptyRefusal::UnexpectedClearEcho,
                [("rows", json!(proof.rows))],
            ));
        }
        Ok(())
    }

    /// One pass of the shared predicate. `Ok(None)` means the transcript is not
    /// locatable yet, which is a not-yet answer and never a wrong one.
    fn assert_empty_at(&self, session_id: SessionId) -> DriverResult<Option<EmptinessProof>> {
        let located = match self.locator.locate_for(&session_id.to_string()) {
            Ok(located) => located,
            Err(TranscriptLocationError::NotFound { .. }) => return Ok(None),
            Err(error) => return Err(map_location_error(error)),
        };
        self.prove_transcript_inert(&located.path, session_id)
            .map(Some)
    }

    /// Every row of the bound file, individually proven inert.
    ///
    /// Structured over `RowKind` so a future variant is a compile error rather
    /// than a silent pass. `SystemRow::is_admitted_on_active_chain` is one clause
    /// of this and not the whole of it: it is the clause that proves no *turn*
    /// ran, and on its own it would pass a transcript full of a prior caller's
    /// prompts and replies -- a cancelled or truncated turn leaves no
    /// `turn_duration` row at all.
    fn prove_transcript_inert(
        &self,
        path: &Path,
        session_id: SessionId,
    ) -> DriverResult<EmptinessProof> {
        let file = File::open(path).map_err(|error| io_failure(path, error))?;
        let bytes = metadata_for_file(&file, path)?.len;
        // Checked before the read, so a leaked transcript costs one `stat`.
        if bytes > MAX_ASSERT_EMPTY_BYTES {
            return Err(assert_empty_refusal_with(
                AssertEmptyRefusal::ByteBudgetExceeded,
                [
                    ("bytes", json!(diagnostic_u64(bytes))),
                    ("byte_budget", json!(MAX_ASSERT_EMPTY_BYTES)),
                ],
            ));
        }
        let mut contents = Vec::new();
        (&file)
            .take(MAX_ASSERT_EMPTY_BYTES)
            .read_to_end(&mut contents)
            .map_err(|error| io_failure(path, error))?;

        let mut rows = 0_usize;
        let mut user_rows = 0_usize;
        let mut clear_command_seen = false;
        let mut offset = 0_u64;
        let mut pending_bytes = 0_usize;
        for chunk in contents.split_inclusive(|byte| *byte == b'\n') {
            if chunk.last() != Some(&b'\n') {
                // The cursor's own rule: an unterminated final line is not a
                // record yet and must never be judged as one.
                pending_bytes = chunk.len();
                break;
            }
            rows += 1;
            if rows > MAX_ASSERT_EMPTY_ROWS {
                return Err(assert_empty_refusal_with(
                    AssertEmptyRefusal::RowBudgetExceeded,
                    [
                        ("rows", json!(rows)),
                        ("row_budget", json!(MAX_ASSERT_EMPTY_ROWS)),
                    ],
                ));
            }
            let mut line = &chunk[..chunk.len() - 1];
            if line.last() == Some(&b'\r') {
                line = &line[..line.len() - 1];
            }
            let complete = pseudomux_claude::CompleteLine {
                location: pseudomux_claude::SourceLocation {
                    line: rows as u64,
                    byte_offset: offset,
                },
                bytes: line.to_vec(),
            };
            offset += chunk.len() as u64;
            let row = self.parser.parse(&complete).map_err(|_| {
                assert_empty_refusal_with(
                    AssertEmptyRefusal::UnparseableRow,
                    [("line", json!(rows))],
                )
            })?;
            // Free-riding on the turn path's identity rule, which already
            // covers `user_other`: both preamble user rows carry `sessionId` and
            // an absolute `cwd`, so they are identity-checked here exactly as
            // they would be mid-turn.
            self.validate_semantic_identity(session_id, &row)?;
            match &row.kind {
                // Reject-by-default, like every other arm. "Excluded from the
                // active graph" is a statement about what the completion engine
                // reads, not about what a row CARRIES: `queue-operation` is
                // queued user input and MEASURED carries its `content` in 1076
                // of 2133 rows on this host's corpus, `ai-title` carries a title
                // generated from a conversation, and `summary` carries a
                // conversation's summary. A predicate that admits them declares
                // a transcript empty while a previous caller's text sits in it.
                pseudomux_claude::RowKind::Metadata { record_type } => {
                    let Some(disposition) = preamble_metadata_disposition(record_type) else {
                        return Err(assert_empty_refusal_with(
                            AssertEmptyRefusal::UnexpectedMetadataRecord,
                            [
                                ("row_kind", json!("metadata")),
                                ("line", json!(rows)),
                                ("record_type", json!(redacted_schema_token(record_type))),
                            ],
                        ));
                    };
                    self.validate_metadata_identity(session_id, &row, disposition)?;
                    // `last-prompt` is in the allowlist because MEASURED it is
                    // the sixth row of a clean cleared preamble -- with
                    // `lastPrompt` null, the only value it can take when no
                    // prompt was submitted. Elsewhere in the corpus it carries
                    // the prompt text verbatim (2337 of 2365 rows), so the row
                    // is admitted for what it says, not for its type.
                    if record_type == LAST_PROMPT_RECORD_TYPE
                        && !matches!(
                            row.raw.get(LAST_PROMPT_FIELD),
                            None | Some(serde_json::Value::Null)
                        )
                    {
                        return Err(assert_empty_row_refusal(
                            AssertEmptyRefusal::MetadataPromptPresent,
                            "metadata",
                            rows,
                        ));
                    }
                }
                pseudomux_claude::RowKind::System(system) => {
                    if system.is_admitted_on_active_chain() {
                        // `turn_duration`/`stop_hook_summary` prove a turn
                        // ended; `api_error` proves one is in flight. Either
                        // means the instance served work.
                        return Err(assert_empty_row_refusal(
                            AssertEmptyRefusal::TurnMarkerPresent,
                            "system",
                            rows,
                        ));
                    }
                    if system.subtype.as_deref() != Some(LOCAL_COMMAND_SUBTYPE) {
                        return Err(assert_empty_refusal_with(
                            AssertEmptyRefusal::UnexpectedSystemSubtype,
                            [
                                ("row_kind", json!("system")),
                                ("line", json!(rows)),
                                (
                                    "subtype",
                                    json!(system.subtype.as_deref().map(redacted_schema_token)),
                                ),
                            ],
                        ));
                    }
                }
                pseudomux_claude::RowKind::UserOther => {
                    user_rows += 1;
                    if user_rows > MAX_ASSERT_EMPTY_USER_ROWS {
                        return Err(assert_empty_row_refusal(
                            AssertEmptyRefusal::UnexpectedUserRow,
                            "user_other",
                            rows,
                        ));
                    }
                    match classify_preamble_user_row(&row.raw) {
                        PreambleUserRow::Caveat => {}
                        PreambleUserRow::ClearCommandEcho => clear_command_seen = true,
                        // The whole point of this predicate. Row 3 is Claude's
                        // own authoritative record of which slash command the
                        // fuzzy composer executed, and no anchor check can see
                        // it: a `/model` rotation writes a new file whose row 0
                        // is a `mode` row with a new session id, exactly like a
                        // `/clear` does.
                        PreambleUserRow::OtherCommandEcho(name) => {
                            return Err(assert_empty_refusal_with(
                                AssertEmptyRefusal::WrongLocalCommand,
                                [
                                    ("row_kind", json!("user_other")),
                                    ("line", json!(rows)),
                                    ("command_name", json!(redacted_schema_token(&name))),
                                ],
                            ));
                        }
                        PreambleUserRow::Unrecognized => {
                            return Err(assert_empty_row_refusal(
                                AssertEmptyRefusal::UnexpectedUserRow,
                                "user_other",
                                rows,
                            ));
                        }
                    }
                }
                // A caller prompt, a model reply, a tool result, or injected
                // context. Any of these is leakage.
                pseudomux_claude::RowKind::TypedUser { .. } => {
                    return Err(assert_empty_row_refusal(
                        AssertEmptyRefusal::SemanticRowPresent,
                        "typed_user",
                        rows,
                    ));
                }
                pseudomux_claude::RowKind::Assistant(_) => {
                    return Err(assert_empty_row_refusal(
                        AssertEmptyRefusal::SemanticRowPresent,
                        "assistant",
                        rows,
                    ));
                }
                pseudomux_claude::RowKind::UserToolResults { .. } => {
                    return Err(assert_empty_row_refusal(
                        AssertEmptyRefusal::SemanticRowPresent,
                        "user_tool_results",
                        rows,
                    ));
                }
                pseudomux_claude::RowKind::Attachment { .. } => {
                    return Err(assert_empty_row_refusal(
                        AssertEmptyRefusal::SemanticRowPresent,
                        "attachment",
                        rows,
                    ));
                }
                // `JsonlParser` returns `Unknown` without erroring even in
                // Strict mode. Schema drift in a file pmux is about to declare
                // clean fails closed.
                pseudomux_claude::RowKind::Unknown { .. } => {
                    return Err(assert_empty_row_refusal(
                        AssertEmptyRefusal::UnknownRow,
                        "unknown",
                        rows,
                    ));
                }
            }
        }

        Ok(EmptinessProof {
            session_id,
            rows,
            bytes,
            clear_command_seen,
            pending_bytes,
        })
    }

    /// The identity clause for an allowlisted preamble metadata row.
    ///
    /// Separate from [`Self::validate_semantic_identity`] because metadata rows
    /// stamp a different, smaller identity and the turn path must keep its own
    /// rule: mid-turn, metadata is dropped from the analysis graph before it can
    /// contribute to any completion proof (`ParsedRow::is_analysis_changing`),
    /// so a foreign-stamped metadata row there changes no answer. Here the
    /// answer being given IS about the file's contents, so a row stamped with
    /// another session's id is exactly the evidence that this is not the file
    /// the clear opened.
    ///
    /// MEASURED over 231 transcripts on this host: `mode`, `permission-mode`,
    /// `bridge-session` and `last-prompt` carry `sessionId` on 100% of rows
    /// (7,222 rows) and it equals the transcript's own id on every one;
    /// `file-history-snapshot` carries no `sessionId` at all on 289 of 289 rows;
    /// no metadata record type carries `cwd` at all. So presence is required
    /// where it is measured, absence is tolerated only where it is measured, and
    /// a `cwd` that appears anyway is still held to the semantic rule.
    fn validate_metadata_identity(
        &self,
        expected_session_id: SessionId,
        row: &pseudomux_claude::ParsedRow,
        disposition: MetadataIdentity,
    ) -> DriverResult<()> {
        match (disposition, row.common.session_id.as_deref()) {
            (MetadataIdentity::Stamped, None) => {
                return Err(identity_failure(row, "metadata", "session_id", "missing"));
            }
            (_, Some(session_id)) => {
                let parsed = session_id
                    .parse::<SessionId>()
                    .map_err(|_| identity_failure(row, "metadata", "session_id", "invalid_uuid"))?;
                if parsed != expected_session_id {
                    return Err(identity_failure(row, "metadata", "session_id", "mismatch"));
                }
            }
            (MetadataIdentity::Unstamped, None) => {}
        }
        if let Some(cwd) = row.raw.get("cwd") {
            let cwd = cwd
                .as_str()
                .ok_or_else(|| identity_failure(row, "metadata", "cwd", "invalid_type"))?;
            if !Path::new(cwd).is_absolute() {
                return Err(identity_failure(row, "metadata", "cwd", "not_absolute"));
            }
            if normalize_candidate_cwd(cwd) != self.expected_cwd {
                return Err(identity_failure(row, "metadata", "cwd", "mismatch"));
            }
        }
        Ok(())
    }

    fn validate_semantic_identity(
        &self,
        expected_session_id: SessionId,
        row: &pseudomux_claude::ParsedRow,
    ) -> DriverResult<()> {
        let Some(row_kind) = identity_bound_row_kind(&row.kind) else {
            return Ok(());
        };

        let session_id = row
            .common
            .session_id
            .as_deref()
            .ok_or_else(|| identity_failure(row, row_kind, "session_id", "missing"))?;
        let parsed_session_id = session_id
            .parse::<SessionId>()
            .map_err(|_| identity_failure(row, row_kind, "session_id", "invalid_uuid"))?;
        if parsed_session_id != expected_session_id {
            return Err(identity_failure(row, row_kind, "session_id", "mismatch"));
        }

        let cwd = row
            .raw
            .get("cwd")
            .ok_or_else(|| identity_failure(row, row_kind, "cwd", "missing"))?;
        let cwd = cwd
            .as_str()
            .ok_or_else(|| identity_failure(row, row_kind, "cwd", "invalid_type"))?;
        if !Path::new(cwd).is_absolute() {
            return Err(identity_failure(row, row_kind, "cwd", "not_absolute"));
        }
        if normalize_candidate_cwd(cwd) != self.expected_cwd {
            return Err(identity_failure(row, row_kind, "cwd", "mismatch"));
        }
        Ok(())
    }

    fn arm_sync(&self, session_id: SessionId) -> DriverResult<TranscriptArm> {
        // An id this source watched being abandoned can still be located, and
        // arming on it would succeed: `/clear` leaves the old transcript's inode
        // and length untouched forever, so every fence below stays green against
        // a file that will never grow again. The turn would then be typed into
        // the TUI and wait out its whole deadline for a row that cannot arrive.
        // Refusing here costs that turn nothing it had, and it costs the
        // operator a `TurnTimeout` with no thread to pull.
        if let Some(record) = self.rotation_record(session_id)? {
            return Err(record.into_failure(None));
        }
        let mut state = self.state.lock().map_err(poisoned_tail)?;
        // The reset is what makes re-arming under a rotated id safe: the path
        // is dropped before it is re-located, so `seek_to_validated_eof` can
        // only ever establish the boundary in the file that belongs to
        // `session_id`. Carrying the previous path forward would arm this turn
        // at the EOF of the abandoned transcript.
        *state = TailState::new(session_id);
        match self.locator.locate_for(&session_id.to_string()) {
            Ok(located) => {
                state.path = Some(located.path);
                self.seek_to_validated_eof(&mut state)?;
            }
            // A transcript that does not exist yet has a knowable boundary --
            // there is nothing before it -- so this is a real arm, and the poll
            // that late-locates the file may read it from zero. An arm that
            // failed for any other reason is left unarmed by the early return.
            Err(TranscriptLocationError::NotFound { .. }) => {}
            Err(error) => return Err(map_location_error(error)),
        }
        // Last, and only here: the boundary now exists, so polls under this id
        // may resume. Every path that leaves this function without reaching this
        // line leaves the tail refusing, which is the direction that cannot
        // return work that was never done.
        state.armed = true;
        Ok(TranscriptArm {
            position: state.position(),
            // The EOF cursor is the authority boundary. Re-reading prior turns
            // is both unnecessary for active-chain correlation and unbounded in
            // long-lived sessions.
            historical_rows: Vec::new(),
        })
    }

    fn seek_to_validated_eof(&self, state: &mut TailState) -> DriverResult<()> {
        let path = state.path.clone().ok_or_else(|| {
            DriverFailure::new(
                ErrorCode::TranscriptUnavailable,
                "transcript path is unavailable",
            )
        })?;
        let metadata = metadata_for(&path)?;
        self.seek_to_observed_eof(state, &path, metadata)
    }

    fn seek_to_observed_eof(
        &self,
        state: &mut TailState,
        path: &Path,
        metadata: FileMetadata,
    ) -> DriverResult<()> {
        let mut file = File::open(path).map_err(|error| io_failure(path, error))?;
        let opened_metadata = metadata_for_file(&file, path)?;
        ensure_same_transcript_generation(metadata.identity, opened_metadata.identity)?;
        ensure_same_transcript_length(
            metadata.len,
            opened_metadata.len,
            "transcript length changed while establishing the arm boundary",
        )?;
        if metadata.len > 0 {
            file.seek(SeekFrom::Start(metadata.len - 1))
                .map_err(|error| io_failure(path, error))?;
            let mut final_byte = [0_u8; 1];
            file.read_exact(&mut final_byte)
                .map_err(|error| io_failure(path, error))?;
            if final_byte[0] != b'\n' {
                return Err(DriverFailure::new(
                    ErrorCode::SchemaDrift,
                    "cannot arm at an incomplete transcript record",
                )
                .with_details(json!({
                    "field": "jsonl_boundary",
                    "violation": "unterminated_record",
                    "offset": diagnostic_u64(metadata.len - 1),
                })));
            }
        }
        let read_metadata = metadata_for_file(&file, path)?;
        ensure_same_transcript_generation(metadata.identity, read_metadata.identity)?;
        ensure_same_transcript_length(
            metadata.len,
            read_metadata.len,
            "transcript length changed while establishing the arm boundary",
        )?;
        let path_metadata = metadata_for(path)?;
        ensure_same_transcript_generation(metadata.identity, path_metadata.identity)?;
        ensure_same_transcript_length(
            metadata.len,
            path_metadata.len,
            "transcript length changed while establishing the arm boundary",
        )?;
        state.cursor.seek_to_eof(metadata);
        state.last_change = Instant::now();
        Ok(())
    }

    fn poll_sync(
        &self,
        session_id: SessionId,
        position: &TranscriptPosition,
    ) -> DriverResult<TranscriptBatch> {
        // Read before the tail is locked: this lock is never taken while `state`
        // is held, so the two can never order against each other.
        let rotation = self.rotation_record(session_id)?;
        let mut state = self.state.lock().map_err(poisoned_tail)?;
        // Rotation before identity, and before position. Both of those describe
        // a disagreement about *which* transcript to read; a rotation is the
        // answer to why, and it is the one an operator staring at a turn that
        // produced nothing actually needs. The quiet window is measured here,
        // under the lock that owns it, so the message can say the bound file
        // stopped growing rather than merely asserting it.
        if let Some(record) = rotation {
            return Err(record.into_failure(Some(BoundTranscriptQuiet {
                quiet_ms: protocol_milliseconds(
                    state.last_change.elapsed().as_millis(),
                    "transcript stability duration",
                )?,
                offset: state.cursor.next_offset(),
            })));
        }
        // Identity before position: a cursor minted under one session id says
        // nothing about a file that belongs to another, so reporting an offset
        // mismatch here would point at the wrong thing. The tail is dropped
        // instead of rebound-and-continued because neither continuation is
        // sound. Reading the old file at the old offset tails a transcript that
        // will never grow again; re-locating and reading the new one from zero
        // hands this turn a history that could acknowledge and finish it before
        // the work is done. Only `arm_at_eof` establishes an authority
        // boundary, so a rotated id has to go back through it.
        if state.session_id != session_id {
            state.rebind(session_id);
            return Err(unarmed_tail_failure(
                "transcript poll session identity does not match the armed tail",
            ));
        }
        // The rebind above is not a one-poll refusal. A tail with no boundary
        // refuses EVERY poll under EVERY position until `arm_at_eof` gives it
        // one -- including a position that happens to equal the state the rebind
        // left behind, and including the `{expected_generation, expected_offset}`
        // a caller could adopt from the mismatch details below. Refusing by
        // position alone would be a race the caller can win by guessing; this
        // one cannot be guessed, because no position is right.
        if !state.armed {
            return Err(unarmed_tail_failure(
                "transcript poll requires an arm boundary this tail does not have",
            ));
        }
        if &state.position() != position {
            return Err(DriverFailure::new(
                ErrorCode::SchemaDrift,
                "transcript poll position does not match the source cursor",
            )
            .with_details(json!({
                "expected_generation": diagnostic_u64(state.generation),
                "expected_offset": diagnostic_u64(state.cursor.next_offset()),
                "actual_generation": diagnostic_u64(position.generation),
                "actual_offset": diagnostic_u64(position.offset),
            })));
        }

        if state.path.is_none() {
            match self.locator.locate_for(&session_id.to_string()) {
                Ok(located) => state.path = Some(located.path),
                Err(TranscriptLocationError::NotFound { .. }) => {
                    return Ok(TranscriptBatch {
                        position: state.position(),
                        rows: Vec::new(),
                        // A transcript that still does not exist is an exact,
                        // empty EOF observation. This lets a never-used ready
                        // session reconcile a detached terminal safely while
                        // the locator continues checking for a newly created
                        // file on every subsequent poll.
                        drain: TranscriptDrainEvidence {
                            at_eof: true,
                            has_partial_line: false,
                            stable_for_ms: protocol_milliseconds(
                                state.last_change.elapsed().as_millis(),
                                "transcript stability duration",
                            )?,
                        },
                    });
                }
                Err(error) => return Err(map_location_error(error)),
            }
        }
        let (rows, read_identity) = self.read_available(&mut state)?;
        let metadata = metadata_for(
            state
                .path
                .as_deref()
                .expect("path was established before reading"),
        )?;
        ensure_same_transcript_generation(read_identity, metadata.identity)?;
        if metadata.len < state.cursor.next_offset() {
            return Err(DriverFailure::new(
                ErrorCode::SchemaDrift,
                "transcript was truncated during an active filesystem observation",
            ));
        }
        let at_eof = state.cursor.next_offset() == metadata.len;
        let stable_for_ms = protocol_milliseconds(
            state.last_change.elapsed().as_millis(),
            "transcript stability duration",
        )?;
        Ok(TranscriptBatch {
            position: state.position(),
            rows,
            drain: TranscriptDrainEvidence {
                at_eof,
                has_partial_line: state.cursor.has_partial_line(),
                stable_for_ms,
            },
        })
    }

    fn read_available(
        &self,
        state: &mut TailState,
    ) -> DriverResult<(Vec<pseudomux_claude::ParsedRow>, FileIdentity)> {
        let path = state.path.clone().ok_or_else(|| {
            DriverFailure::new(
                ErrorCode::TranscriptUnavailable,
                "transcript path is unavailable",
            )
        })?;
        let metadata = metadata_for(&path)?;
        let observation = state.cursor.observe(metadata);
        if matches!(
            observation.change,
            CursorChange::Replaced { .. } | CursorChange::Truncated { .. }
        ) {
            state.generation = state.generation.checked_add(1).ok_or_else(|| {
                DriverFailure::new(
                    ErrorCode::SchemaDrift,
                    "transcript cursor generation overflowed",
                )
            })?;
        }
        let read_len = observation
            .read_to
            .checked_sub(observation.read_from)
            .ok_or_else(|| {
                DriverFailure::new(
                    ErrorCode::SchemaDrift,
                    "transcript cursor produced an inverted read range",
                )
            })?;
        if read_len > MAX_TRANSCRIPT_READ_BYTES {
            return Err(DriverFailure::new(
                ErrorCode::SchemaDrift,
                "transcript append exceeds the bounded read limit",
            )
            .with_details(json!({
                "bytes": diagnostic_u64(read_len),
                "limit": MAX_TRANSCRIPT_READ_BYTES,
            })));
        }
        if read_len == 0 {
            return Ok((Vec::new(), metadata.identity));
        }

        self.read_observed_range(
            state,
            &path,
            metadata,
            observation.read_from,
            observation.read_to,
        )
    }

    fn read_observed_range(
        &self,
        state: &mut TailState,
        path: &Path,
        metadata: FileMetadata,
        read_from: u64,
        read_to: u64,
    ) -> DriverResult<(Vec<pseudomux_claude::ParsedRow>, FileIdentity)> {
        // The pathname can be atomically replaced between metadata acquisition
        // and open. Verify the opened descriptor before trusting any bytes,
        // then verify the pathname again in `poll_sync` after the read. These
        // sampled fences prevent a different transcript generation from being
        // parsed under the cursor identity observed above.
        let mut file = File::open(path).map_err(|error| io_failure(path, error))?;
        let opened_metadata = metadata_for_file(&file, path)?;
        ensure_same_transcript_generation(metadata.identity, opened_metadata.identity)?;
        if opened_metadata.len < read_to {
            return Err(DriverFailure::new(
                ErrorCode::SchemaDrift,
                "transcript was truncated before an active range could be read",
            ));
        }
        file.seek(SeekFrom::Start(read_from))
            .map_err(|error| io_failure(path, error))?;
        let read_len = read_to.checked_sub(read_from).ok_or_else(|| {
            DriverFailure::new(
                ErrorCode::SchemaDrift,
                "transcript cursor produced an inverted read range",
            )
        })?;
        let mut bytes = vec![0; read_len as usize];
        file.read_exact(&mut bytes)
            .map_err(|error| io_failure(path, error))?;
        // The identity every row in this range is judged against is the one the
        // tail is armed on, read under the same lock that produced the range.
        let expected_session_id = state.session_id;
        let update = state
            .cursor
            .push(metadata.identity, read_from, &bytes)
            .map_err(map_transcript_error)?;
        state.last_change = Instant::now();
        let rows = update
            .lines
            .iter()
            .map(|line| {
                ensure_protocol_transcript_offset(line.location.byte_offset)?;
                let row = self.parser.parse(line).map_err(map_transcript_error)?;
                self.validate_semantic_identity(expected_session_id, &row)?;
                Ok(row)
            })
            .collect::<DriverResult<Vec<_>>>()?;
        let read_metadata = metadata_for_file(&file, path)?;
        ensure_same_transcript_generation(metadata.identity, read_metadata.identity)?;
        if read_metadata.len < update.next_offset {
            return Err(DriverFailure::new(
                ErrorCode::SchemaDrift,
                "transcript was truncated while an active range was being read",
            ));
        }
        Ok((rows, metadata.identity))
    }
}

#[async_trait]
impl TranscriptSource for FileTranscriptSource {
    async fn arm_at_eof(&self, session_id: SessionId) -> DriverResult<TranscriptArm> {
        self.arm_sync(session_id)
    }

    async fn poll(
        &self,
        session_id: SessionId,
        position: &TranscriptPosition,
    ) -> DriverResult<TranscriptBatch> {
        self.poll_sync(session_id, position)
    }

    async fn assert_empty_at_launch(&self, session_id: SessionId) -> DriverResult<()> {
        self.prove_empty_at_launch(session_id)
    }
}

/// Types `/clear` into a Claude TUI pmux owns and returns the session id Claude
/// rotated to.
///
/// This is the whole privileged path, and it is one function on purpose. Typing
/// the command and identifying the transcript it opens are not two operations a
/// caller may compose: `/clear` ABANDONS the current transcript rather than
/// truncating it — same inode, same length, no further appends — so a clear
/// whose successor was never identified leaves pmux tailing a file that will
/// never grow again. Binding them here means that state cannot be reached by
/// forgetting a step.
///
/// The result is an id, not a file. Nothing is armed on it: only `arm_at_eof`
/// establishes an authority boundary, and it re-resolves the id through the
/// locator, which accepts only a transcript whose own rows corroborate that id
/// and this cwd. A rebind that somehow named the wrong file therefore cannot
/// become a boundary — it becomes a `NotFound`.
///
/// On refusal the abandoned id is poisoned: every later arm or poll under it
/// fails with a message that names the rotation, instead of waiting out a turn
/// deadline against a dead file.
///
/// `terminal` and `transcript` must belong to the same instance. Neither type
/// carries a session id — `RmuxTerminalControl` is one TUI by construction and
/// ignores the id its trait methods pass — so that pairing is a construction-site
/// invariant this function cannot check, only state.
pub async fn clear_and_rebind(
    terminal: &RmuxTerminalControl,
    transcript: &FileTranscriptSource,
    session_id: SessionId,
    deadline_unix_ms: u64,
) -> DriverResult<SessionId> {
    // Snapshot BEFORE anything is typed. MEASURED: the new transcript appears
    // +39ms after Enter, so a listing taken afterwards cannot say which file is
    // new, and "newest" is a guess this design does not make.
    //
    // This refusal needs no malformed input to reach: a `clear_session` issued
    // before the session's first turn -- the natural order for a pool checking
    // an instance out -- finds no transcript to watch, because Claude creates
    // the file lazily. Nothing has been typed at this point, so it is marked
    // exactly like the injection refusal below.
    let watch = transcript
        .watch_rotation(session_id)
        .map_err(refusal_before_clear_submission)?;
    if let Err(error) = terminal
        .type_control_command(ControlCommand::Clear, deadline_unix_ms)
        .await
        && !enter_was_attempted(&error)
    {
        // The command was never submitted, so nothing was abandoned and the
        // instance is exactly as it was. Poisoning the tail here would refuse a
        // session that is still perfectly coherent -- and so would the actor,
        // which quarantines every unmarked failure this function returns.
        return Err(refusal_before_clear_submission(error));
    }
    // Enter was attempted — acknowledged, or acknowledged ambiguously. Either
    // way the clear may have executed, so the bound transcript is suspect from
    // here and the rotation is resolved, and recorded, even though the terminal
    // reported a failure. If a rotation is found, the transcript has answered
    // the question the terminal could not: the command did land, and this is the
    // file it opened.
    let rebound = transcript.resolve_rotation(watch).await?;
    // Between resolving the id and binding it, deliberately. `resolve_rotation`
    // proves a new file appeared whose row 0 is a `mode` row with a new session
    // id -- which is equally true of a file `/model` or `/compact` opened. This
    // is the check that reads Claude's own record of which command executed, and
    // it runs before `arm_at_eof` so pmux never establishes transcript authority
    // over a file it is about to refuse. Its `Err` reaches the actor's
    // `poison_after_failed_rebind`, so a refusal is quarantined by construction
    // rather than by a code path someone can forget.
    let proof = transcript.assert_empty_after_clear(rebound).await?;
    // The proof is recorded rather than returned: a pool that later wants to
    // treat "checkout is eligible" as a cached fact needs the whole triple, and
    // an operator reconstructing a drift needs to see the preamble that was
    // accepted, not only the ones that were refused.
    tracing::debug!(
        operation = "assert_empty_after_clear",
        session_id = %proof.session_id,
        rows = proof.rows,
        bytes = proof.bytes,
        "cleared transcript proven to have served no work"
    );
    Ok(rebound)
}

/// Whether a failed injection reached its single irreversible write.
///
/// [`enter_once`] is the only place a failure can be raised after Enter is sent,
/// and **every** failure it raises there carries the flag — the ambiguous one
/// and the turn-deadline one alike, both through [`mark_enter_attempted`].
/// Reading the flag rather than re-deriving the condition keeps one statement
/// about "Enter may have landed" instead of two that can drift apart; putting
/// the flag itself behind one function keeps the writers from drifting either,
/// which is how a deadline answer came to be raised there without it.
fn enter_was_attempted(error: &DriverFailure) -> bool {
    error.details.get(ENTER_ATTEMPTED) == Some(&serde_json::Value::Bool(true))
}

/// The detail key a clear refusal carries when it provably typed nothing.
const CLEAR_NOT_SUBMITTED: &str = "clear_not_submitted";

/// Marks a refusal raised before `/clear` could reach the TUI.
///
/// Set at the two sites that own that fact and nowhere else, because "was the
/// command submitted" is knowable only where the submission is attempted. It is
/// a positive claim rather than the absence of one so that the actor's default
/// -- quarantine -- applies to every failure that does not make the claim,
/// including any added later.
fn refusal_before_clear_submission(mut error: DriverFailure) -> DriverFailure {
    match &mut error.details {
        serde_json::Value::Object(details) => {
            details.insert(CLEAR_NOT_SUBMITTED.to_owned(), true.into());
        }
        details => *details = json!({ CLEAR_NOT_SUBMITTED: true }),
    }
    error
}

/// Whether a clear refusal proved the bound transcript was left untouched.
///
/// Read by the actor against the already-converted [`ErrorBody`], so the two
/// halves of the statement cannot drift: this is the same key, from the same
/// module, that [`refusal_before_clear_submission`] writes.
pub(crate) fn clear_was_not_submitted(details: &serde_json::Value) -> bool {
    details.get(CLEAR_NOT_SUBMITTED) == Some(&serde_json::Value::Bool(true))
}

/// Every reason a transcript claimed to have served no work is refused for,
/// and -- exhaustively -- which class of fact each one is.
///
/// # Why this is an enum and not thirteen string literals
///
/// It used to be thirteen literals and ONE of them, `wrong_local_command`, was
/// singled out by a `const` and a predicate whose doc said the general thing:
/// *"it means pmux's model of the composer no longer matches the installed
/// Claude, and every other instance is typing `/clear` into the same
/// composer."* That sentence is true of six other reasons here, and none of
/// them was tested for. A cleared preamble carrying a metadata record type
/// pmux has never seen, a `system` row whose subtype is not `local_command`, a
/// line the parser cannot parse, a row kind it does not recognise, a third
/// `user` row, or more rows than the preamble has ever had -- each is Claude
/// writing a post-`/clear` preamble that is not the one
/// `MAX_ASSERT_EMPTY_ROWS` and `MAX_ASSERT_EMPTY_USER_ROWS` were MEASURED
/// against on 2.1.220, and each is a fact about the INSTALLED CLAUDE that every
/// other pool instance is about to hit. They quarantined one instance and the
/// pool went on minting replacements into the same drift.
///
/// So the classification is a wildcard-free `match`
/// ([`Self::is_a_version_drift_signal`]) and a new reason cannot be added
/// without answering the question. This is re-promotion trigger 4 --
/// `docs/version-drift.md` sec.5 P2 -- and
/// [`crate::compatibility::RepromotionTrigger::ClearScreenOrPreambleMismatch`]
/// names that method as its detector.
///
/// # The wire `reason` is unchanged
///
/// [`Self::reason`] returns the exact literal each site shipped, with one
/// deliberate exception: the BYTE budget site reported `row_budget_exceeded`
/// while publishing `bytes` and `byte_budget`, which is a refusal whose reason
/// names a different quantity from the one it measured. It is now
/// `byte_budget_exceeded`, and it is the one member of the budget pair that is
/// NOT classified as drift -- a large file is evidence about a file, not about
/// the shape of a preamble.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssertEmptyRefusal {
    /// More rows than a preamble has ever had.
    RowBudgetExceeded,
    /// More bytes than the cheap pre-parse `stat` allows.
    ByteBudgetExceeded,
    /// A preamble line the parser cannot parse.
    UnparseableRow,
    /// A metadata `record_type` the preamble allowlist has never seen.
    UnexpectedMetadataRecord,
    /// A `last-prompt` row carrying a prompt.
    MetadataPromptPresent,
    /// A `turn_duration`, `stop_hook_summary` or `api_error` row: work ran.
    TurnMarkerPresent,
    /// A `system` row in the preamble whose subtype is not `local_command`.
    UnexpectedSystemSubtype,
    /// A third `user` row, or a preamble `user` row that is neither the caveat
    /// nor a recognised command echo.
    UnexpectedUserRow,
    /// Claude's own record says the composer executed some OTHER slash command.
    WrongLocalCommand,
    /// An assistant reply, a typed user row, a tool result or an attachment.
    SemanticRowPresent,
    /// A row shape the parser does not recognise at all.
    UnknownRow,
    /// The rows are clean and the `/clear` echo is there, but the file never
    /// stopped changing inside the rebind deadline.
    PreambleNotSettled,
    /// The rebind deadline passed without Claude's own `/clear` echo.
    ClearCommandMissing,
    /// A LAUNCH preamble carrying a `/clear` echo: id resolution found a file
    /// this launch did not open.
    UnexpectedClearEcho,
}

impl AssertEmptyRefusal {
    /// Every variant, kept honest by the wildcard-free matches below and by
    /// `tests::every_assert_empty_refusal_is_classified_exactly_once`.
    pub const ALL: [Self; 14] = [
        Self::RowBudgetExceeded,
        Self::ByteBudgetExceeded,
        Self::UnparseableRow,
        Self::UnexpectedMetadataRecord,
        Self::MetadataPromptPresent,
        Self::TurnMarkerPresent,
        Self::UnexpectedSystemSubtype,
        Self::UnexpectedUserRow,
        Self::WrongLocalCommand,
        Self::SemanticRowPresent,
        Self::UnknownRow,
        Self::PreambleNotSettled,
        Self::ClearCommandMissing,
        Self::UnexpectedClearEcho,
    ];

    /// The `reason` an operator reads off the wire.
    pub const fn reason(self) -> &'static str {
        match self {
            Self::RowBudgetExceeded => "row_budget_exceeded",
            Self::ByteBudgetExceeded => "byte_budget_exceeded",
            Self::UnparseableRow => "unparseable_row",
            Self::UnexpectedMetadataRecord => "unexpected_metadata_record",
            Self::MetadataPromptPresent => "metadata_prompt_present",
            Self::TurnMarkerPresent => "turn_marker_present",
            Self::UnexpectedSystemSubtype => "unexpected_system_subtype",
            Self::UnexpectedUserRow => "unexpected_user_row",
            Self::WrongLocalCommand => "wrong_local_command",
            Self::SemanticRowPresent => "semantic_row_present",
            Self::UnknownRow => "unknown_row",
            Self::PreambleNotSettled => "preamble_not_settled",
            Self::ClearCommandMissing => "clear_command_missing",
            Self::UnexpectedClearEcho => "unexpected_clear_echo",
        }
    }

    /// **Re-promotion trigger 4.** Whether this refusal is evidence that the
    /// INSTALLED CLAUDE's post-`/clear` preamble is not the one pmux measured,
    /// rather than evidence about this one instance.
    ///
    /// The distinction is operational, and it is the reason the classification
    /// exists: a fact about one instance quarantines that instance, and a fact
    /// about the installed Claude halts the pool, because every other instance
    /// is typing into a composer with the same drift.
    ///
    /// Four are deliberately NOT drift, and each has a reason rather than a
    /// default:
    ///
    /// * [`Self::ByteBudgetExceeded`] is checked BEFORE any parse, so it can
    ///   fire on a large leaked transcript whose semantic rows were never
    ///   reached. Size is evidence about a file, not about a preamble's shape.
    /// * [`Self::ClearCommandMissing`] is a DEADLINE expiring. An echo row
    ///   whose shape changed would be refused immediately as
    ///   [`Self::UnexpectedUserRow`] instead; what reaches this reason is a
    ///   clear that has not landed yet, which is indistinguishable from a slow
    ///   one and must not halt a pool.
    /// * [`Self::PreambleNotSettled`] is the same deadline with the echo
    ///   already present: a stalled writer, not a changed shape.
    /// * [`Self::UnexpectedClearEcho`] is an IDENTITY fact -- a launch bound a
    ///   transcript some earlier `/clear` opened. The preamble it found is
    ///   exactly the preamble pmux measured; the fault is which file it is.
    ///
    /// The remaining three -- [`Self::MetadataPromptPresent`],
    /// [`Self::TurnMarkerPresent`], [`Self::SemanticRowPresent`] -- are content:
    /// somebody's prompt, somebody's turn, somebody's answer. Those are leaks,
    /// and a leak is one instance.
    pub const fn is_a_version_drift_signal(self) -> bool {
        match self {
            Self::RowBudgetExceeded
            | Self::UnparseableRow
            | Self::UnexpectedMetadataRecord
            | Self::UnexpectedSystemSubtype
            | Self::UnexpectedUserRow
            | Self::WrongLocalCommand
            | Self::UnknownRow => true,
            Self::ByteBudgetExceeded
            | Self::MetadataPromptPresent
            | Self::TurnMarkerPresent
            | Self::SemanticRowPresent
            | Self::PreambleNotSettled
            | Self::ClearCommandMissing
            | Self::UnexpectedClearEcho => false,
        }
    }

    /// The refusal a wire `reason` names, DERIVED from [`Self::reason`] rather
    /// than parsed by a second `match` that could disagree with it.
    pub fn from_reason(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|refusal| refusal.reason() == value)
    }
}

/// The re-promotion trigger a clear refusal is evidence of, or `None`.
///
/// Read from the same `reason` key the refusal sites write, in the same module,
/// for the same reason `clear_was_not_submitted` is here rather than at its
/// reader: a caller matching on the message text would be a second copy of this
/// rule, free to drift from the one copy that produces it.
///
/// The stateless pool treats a `Some` answer as a HALT of the whole pool, not
/// as one bad instance: it means pmux's model of the post-`/clear` preamble no
/// longer matches the installed Claude, and every other instance is typing
/// `/clear` into the same composer.
pub(crate) fn clear_refusal_repromotion_trigger(
    details: &serde_json::Value,
) -> Option<&'static str> {
    let reason = details.get("reason").and_then(serde_json::Value::as_str)?;
    AssertEmptyRefusal::from_reason(reason)
        .filter(|refusal| refusal.is_a_version_drift_signal())
        .map(AssertEmptyRefusal::reason)
}

/// One project directory's transcripts, as they were immediately before a
/// `/clear` was typed into the instance that owns it.
pub(crate) struct RotationWatch {
    abandoned: SessionId,
    project_directory: PathBuf,
    before: BTreeSet<PathBuf>,
    /// Lowercased file stems of `before`: the session ids that already had a
    /// transcript here. A file that appears carrying one of them is not a
    /// rotation, whatever else it is.
    before_session_ids: BTreeSet<String>,
    /// When the wait for the rotation began, i.e. when the command was
    /// submitted. Set by `resolve_rotation`, not by the listing.
    started: Instant,
}

/// One observed abandonment of a session id.
#[derive(Clone, Copy, Debug)]
struct RotationRecord {
    abandoned: SessionId,
    /// The id Claude rotated to, when the rebind identified exactly one.
    successor: Option<SessionId>,
    observed_at: Instant,
}

/// What the abandoned tail has to show for itself, measured under the lock that
/// owns the cursor. A rotation diagnosis that could not say the bound file
/// stopped growing would be an assertion; this is the observation behind it.
#[derive(Clone, Copy, Debug)]
struct BoundTranscriptQuiet {
    quiet_ms: u64,
    offset: u64,
}

impl RotationRecord {
    fn into_failure(self, quiet: Option<BoundTranscriptQuiet>) -> DriverFailure {
        let (message, violation) = match self.successor {
            Some(_) => (
                "the bound transcript was abandoned by /clear and this tail was never re-armed on the rotated session",
                "transcript_rotated",
            ),
            None => (
                "/clear abandoned the bound transcript and no replacement transcript could be identified",
                "clear_rebind_failed",
            ),
        };
        // Both ids name the same Claude process the caller already holds: the
        // one it passed in, and the one pmux minted for it by clearing it.
        // Nothing about a foreign session, a path, a cwd, or transcript content
        // is disclosed. They are here because the diagnostic is worthless
        // without them -- "a rotation happened" with no id is the same dead end
        // as the bare TurnTimeout it replaces.
        let mut details = json!({
            "field": "session_id",
            "violation": violation,
            "abandoned_session_id": self.abandoned.to_string(),
            "rebound_session_id": self.successor.map(|successor| successor.to_string()),
            "cleared_ms_ago": diagnostic_u64(
                u64::try_from(self.observed_at.elapsed().as_millis()).unwrap_or(u64::MAX),
            ),
        });
        if let Some(quiet) = quiet {
            details["bound_transcript_quiet_ms"] = diagnostic_u64(quiet.quiet_ms);
            details["bound_transcript_offset"] = diagnostic_u64(quiet.offset);
        }
        DriverFailure::new(ErrorCode::TranscriptUnavailable, message).with_details(details)
    }
}

/// Every `*.jsonl` directly inside one project directory.
fn list_transcripts(directory: &Path) -> DriverResult<BTreeSet<PathBuf>> {
    list_transcripts_within(directory, MAX_ROTATION_DIRECTORY_ENTRIES)
}

/// [`list_transcripts`], with the scan bound passed in.
///
/// The bound is a parameter for exactly one reason, and it is a testing reason
/// stated rather than disguised: observing that the refusal fires on the entry
/// PAST the bound and not ON it costs one directory entry per unit of
/// `MAX_ROTATION_DIRECTORY_ENTRIES`, and at 20,000 that is a fixture the
/// mutation gate would build once per mutant. The production caller above is
/// the only one that supplies the constant, so the constant is still the only
/// bound this driver enforces.
fn list_transcripts_within(directory: &Path, limit: usize) -> DriverResult<BTreeSet<PathBuf>> {
    let entries = std::fs::read_dir(directory).map_err(|error| io_failure(directory, error))?;
    let mut transcripts = BTreeSet::new();
    let mut scanned = 0_usize;
    for entry in entries {
        let entry = entry.map_err(|error| io_failure(directory, error))?;
        scanned += 1;
        if scanned > limit {
            return Err(DriverFailure::new(
                ErrorCode::SchemaDrift,
                "project directory exceeds the bounded rotation scan",
            )
            .with_details(json!({ "limit": limit })));
        }
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("jsonl"))
            && entry
                .file_type()
                .map_err(|error| io_failure(&path, error))?
                .is_file()
        {
            transcripts.insert(path);
        }
    }
    Ok(transcripts)
}

/// Reads the rotation anchor of a transcript that appeared after `/clear`.
///
/// `Ok(None)` means row 0 is not a complete JSONL record yet, which is a
/// not-yet answer. Anything complete but unrecognized is an error: row 0 being
/// `{"type":"mode","sessionId":...}` is a version-observed fact about Claude
/// Code 2.1.220, so a different row 0 means pmux's model of `/clear` no longer
/// matches the installed Claude. The instance is then quarantined rather than
/// trusted, because the alternative is binding a completion authority to a file
/// chosen by a rule that has just been shown to be wrong.
fn read_rotation_anchor(path: &Path) -> DriverResult<Option<SessionId>> {
    let file = File::open(path).map_err(|error| io_failure(path, error))?;
    let mut bytes = Vec::new();
    file.take(MAX_ROTATION_ANCHOR_BYTES as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| io_failure(path, error))?;
    let Some(newline) = bytes.iter().position(|byte| *byte == b'\n') else {
        if bytes.len() >= MAX_ROTATION_ANCHOR_BYTES {
            return Err(rotation_anchor_failure("oversized_row"));
        }
        return Ok(None);
    };
    let mut line = &bytes[..newline];
    if line.last() == Some(&b'\r') {
        line = &line[..line.len() - 1];
    }
    let row = serde_json::from_slice::<serde_json::Value>(line)
        .map_err(|_| rotation_anchor_failure("unparseable_row"))?;
    if row.get("type").and_then(serde_json::Value::as_str) != Some(ROTATION_ANCHOR_ROW_TYPE) {
        return Err(rotation_anchor_failure("unexpected_row_type"));
    }
    row.get("sessionId")
        .and_then(serde_json::Value::as_str)
        .and_then(|session_id| session_id.parse::<SessionId>().ok())
        .map(Some)
        .ok_or_else(|| rotation_anchor_failure("missing_session_id"))
}

/// What a transcript that has served no work looks like, and the one bit that
/// distinguishes the two ways an instance can be fresh.
#[derive(Clone, Copy, Debug)]
pub(crate) struct EmptinessProof {
    pub session_id: SessionId,
    pub rows: usize,
    pub bytes: u64,
    /// True iff the preamble carries the `/clear` command echo. The rebind
    /// caller requires it; the launch caller requires its absence. One predicate
    /// with one extra assertion per caller, rather than two predicates that will
    /// eventually disagree.
    pub clear_command_seen: bool,
    /// Bytes of a final record that has been started and not terminated. Never
    /// judged as a row.
    pub pending_bytes: usize,
}

/// Which of the three shapes a preamble `user` row may take.
enum PreambleUserRow {
    Caveat,
    ClearCommandEcho,
    OtherCommandEcho(String),
    Unrecognized,
}

/// Classifies a `RowKind::UserOther` row from a supposedly clean preamble.
///
/// MEASURED: a real caller prompt carries `promptSource:"typed"` and therefore
/// parses as `RowKind::TypedUser`, which this predicate refuses outright. So
/// `UserOther` is not a hole a prompt can travel through -- but "any
/// string-content user row is fine" is still too loose, and the command echo is
/// the one row that names what the composer actually did.
fn classify_preamble_user_row(raw: &serde_json::Value) -> PreambleUserRow {
    let Some(content) = raw
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(serde_json::Value::as_str)
    else {
        return PreambleUserRow::Unrecognized;
    };
    if content.starts_with(LOCAL_COMMAND_CAVEAT_OPEN) {
        return PreambleUserRow::Caveat;
    }
    if !content.starts_with(COMMAND_NAME_OPEN) {
        return PreambleUserRow::Unrecognized;
    }
    let rest = &content[COMMAND_NAME_OPEN.len()..];
    let Some(end) = rest.find(COMMAND_NAME_CLOSE) else {
        return PreambleUserRow::Unrecognized;
    };
    let name = &rest[..end];
    if name == CLEAR_COMMAND_NAME {
        PreambleUserRow::ClearCommandEcho
    } else {
        PreambleUserRow::OtherCommandEcho(name.to_owned())
    }
}

/// A Claude schema token, bounded and charset-checked before it reaches a
/// diagnostic.
///
/// A slash-command name and a system subtype are Claude's own vocabulary, never
/// caller bytes -- but a predicate that is refusing precisely because the file
/// is not what it expected must not then quote that file freely.
fn redacted_schema_token(token: &str) -> &str {
    let acceptable = token.len() <= MAX_DIAGNOSTIC_TOKEN_BYTES
        && !token.is_empty()
        && token.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '/' | '-' | '_' | ':' | '.')
        });
    if acceptable { token } else { "unrecognized" }
}

fn assert_empty_refusal(refusal: AssertEmptyRefusal) -> DriverFailure {
    DriverFailure::new(
        ErrorCode::SchemaDrift,
        format!(
            "a transcript claimed to have served no work was refused: {}",
            refusal.reason()
        ),
    )
}

/// The three keys every `assert_empty` refusal carries, so no site writes
/// `"reason"` beside a `refusal` that spells it differently and no site forgets
/// to publish the trigger.
fn assert_empty_details(refusal: AssertEmptyRefusal) -> serde_json::Map<String, serde_json::Value> {
    let mut details = serde_json::Map::new();
    details.insert("field".to_owned(), json!("session_id"));
    details.insert("violation".to_owned(), json!("assert_empty_refused"));
    details.insert("reason".to_owned(), json!(refusal.reason()));
    if refusal.is_a_version_drift_signal() {
        details.insert(
            "repromotion_trigger".to_owned(),
            json!(crate::compatibility::RepromotionTrigger::ClearScreenOrPreambleMismatch.id()),
        );
    }
    details
}

/// The row-shaped refusals, which all disclose the same three redacted facts:
/// why, what kind of row, and where. Never any transcript content -- the same
/// rule `identity_failure` already follows.
fn assert_empty_row_refusal(
    refusal: AssertEmptyRefusal,
    row_kind: &'static str,
    line: usize,
) -> DriverFailure {
    let mut details = assert_empty_details(refusal);
    details.insert("row_kind".to_owned(), json!(row_kind));
    details.insert("line".to_owned(), json!(line));
    assert_empty_refusal(refusal).with_details(serde_json::Value::Object(details))
}

/// The non-row-shaped refusals: the same three keys plus whatever quantity the
/// site measured.
fn assert_empty_refusal_with(
    refusal: AssertEmptyRefusal,
    extra: impl IntoIterator<Item = (&'static str, serde_json::Value)>,
) -> DriverFailure {
    let mut details = assert_empty_details(refusal);
    for (key, value) in extra {
        details.insert(key.to_owned(), value);
    }
    assert_empty_refusal(refusal).with_details(serde_json::Value::Object(details))
}

fn rotation_anchor_failure(reason: &'static str) -> DriverFailure {
    DriverFailure::new(
        ErrorCode::SchemaDrift,
        "a transcript that appeared after /clear does not carry the expected rotation anchor",
    )
    .with_details(json!({
        "field": "rotation_anchor",
        "violation": "clear_rebind_anchor_unrecognized",
        "reason": reason,
    }))
}

/// The one refusal for a tail that has no authority boundary, whatever position
/// the poll carries.
///
/// Both callers share a violation because an operator's next step is the same
/// for both — go back through `arm_at_eof` — and because two refusals over one
/// invariant drift apart. The message differs only in why the boundary is
/// missing, and neither discloses an id: the caller passed one in and the tail
/// holds the other, so naming them would say nothing and disclose a session.
fn unarmed_tail_failure(message: &'static str) -> DriverFailure {
    DriverFailure::new(ErrorCode::SchemaDrift, message).with_details(json!({
        "field": "session_id",
        "violation": "rebind_requires_rearm",
    }))
}

struct TailState {
    /// The session id this tail is armed on. It lives beside the cursor, under
    /// one lock, because an identity that could be updated independently of the
    /// path and the offset would let bytes from one transcript be judged against
    /// another transcript's identity.
    session_id: SessionId,
    /// Whether `arm_at_eof` has established an authority boundary for
    /// `session_id`.
    ///
    /// This is state, not arithmetic, because the arithmetic was not sound. The
    /// tail used to be called unarmed when its `{generation, offset}` matched no
    /// position the caller held — but `arm_at_eof` restarts the generation at
    /// `1` and `rebind` only incremented from wherever it was, so generations
    /// REPEAT across arms and a position minted in an earlier arm could collide
    /// with the state a later rebind left behind. A collision let the very poll
    /// the rebind refused through on its second attempt, to late-locate the
    /// rotated transcript and read its whole pre-existing history from offset
    /// zero: a stale acknowledgement and a stale terminal row, handed to a turn
    /// whose work has not been done. A boolean cannot collide with anything.
    armed: bool,
    path: Option<PathBuf>,
    cursor: TranscriptCursor,
    generation: u64,
    last_change: Instant,
}

impl TailState {
    /// A tail bound to `session_id` with no authority boundary yet. Only
    /// `FileTranscriptSource::arm_sync` may arm it, and only after it has
    /// resolved where that boundary is.
    fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            armed: false,
            path: None,
            cursor: TranscriptCursor::new(),
            generation: 1,
            last_change: Instant::now(),
        }
    }

    /// Drops everything the previous identity established and binds the tail to
    /// `session_id`, leaving it unarmed.
    ///
    /// It deliberately does NOT try to move the position somewhere a stale one
    /// cannot reach. Bumping the generation here used to be for exactly that,
    /// and `armed` records why it could not work: a guard that holds against
    /// most positions is not a guard, it is a race with a plausible retry loop.
    /// Re-arming is what supplies both a boundary and a position matching it.
    fn rebind(&mut self, session_id: SessionId) {
        *self = Self::new(session_id);
    }

    fn position(&self) -> TranscriptPosition {
        TranscriptPosition {
            generation: self.generation,
            offset: self.cursor.next_offset(),
        }
    }
}

/// What identity an allowlisted preamble metadata record stamps on itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MetadataIdentity {
    /// MEASURED to carry `sessionId` on every observed row. Its absence is
    /// schema drift in a file pmux is about to declare clean, so it is required.
    Stamped,
    /// MEASURED to carry no identity fields at all. A `sessionId` that appears
    /// anyway is still checked; its absence is not evidence of anything.
    Unstamped,
}

/// The metadata record types a clean preamble may contain, and nothing else.
///
/// MEASURED on Claude Code 2.1.220. The cleared preamble is `mode`,
/// `file-history-snapshot`, two `user` rows, `system`/`local_command` and a
/// trailing `last-prompt`; the launch preamble is `mode`, `permission-mode`,
/// `bridge-session`, `file-history-snapshot`. This is the union of those two,
/// and it is a closed list rather than an exclusion list because the metadata
/// set is Claude's to extend: `summary`, `ai-title`, `queue-operation`,
/// `progress` and `pr-link` are all metadata by
/// `pseudomux_claude::is_metadata_record`, all carry text derived from work a
/// previous caller did, and none of them ever appears in a preamble that served
/// no work. A record type added by a future Claude version lands here as a
/// refusal that names it, which is the direction a completion authority must
/// fail in.
fn preamble_metadata_disposition(record_type: &str) -> Option<MetadataIdentity> {
    match record_type {
        "mode" | "permission-mode" | "bridge-session" | LAST_PROMPT_RECORD_TYPE => {
            Some(MetadataIdentity::Stamped)
        }
        "file-history-snapshot" => Some(MetadataIdentity::Unstamped),
        _ => None,
    }
}

fn identity_bound_row_kind(kind: &pseudomux_claude::RowKind) -> Option<&'static str> {
    match kind {
        pseudomux_claude::RowKind::TypedUser { .. } => Some("typed_user"),
        pseudomux_claude::RowKind::Assistant(_) => Some("assistant"),
        pseudomux_claude::RowKind::UserToolResults { .. } => Some("user_tool_results"),
        pseudomux_claude::RowKind::UserOther => Some("user_other"),
        pseudomux_claude::RowKind::Attachment { .. } => Some("attachment"),
        pseudomux_claude::RowKind::System(_)
        | pseudomux_claude::RowKind::Unknown { .. }
        | pseudomux_claude::RowKind::Metadata { .. } => None,
    }
}

fn identity_failure(
    row: &pseudomux_claude::ParsedRow,
    row_kind: &'static str,
    field: &'static str,
    violation: &'static str,
) -> DriverFailure {
    DriverFailure::new(
        ErrorCode::SchemaDrift,
        "semantic transcript row identity validation failed",
    )
    .with_details(json!({
        "field": field,
        "violation": violation,
        "row_kind": row_kind,
        "line": diagnostic_u64(row.source.line),
    }))
}

fn diagnostic_u64(value: u64) -> serde_json::Value {
    if value <= MAX_SAFE_JSON_INTEGER {
        value.into()
    } else {
        value.to_string().into()
    }
}

fn ensure_protocol_transcript_offset(offset: u64) -> DriverResult<()> {
    if offset <= MAX_SAFE_JSON_INTEGER {
        return Ok(());
    }
    Err(DriverFailure::new(
        ErrorCode::SchemaDrift,
        "transcript row offset is outside protocol-v1's safe integer domain",
    )
    .with_details(json!({ "offset": diagnostic_u64(offset) })))
}

fn normalize_candidate_cwd(value: &str) -> String {
    Path::new(value)
        .canonicalize()
        .map_or_else(|_| value.nfc().collect(), |path| normalize_path(&path))
}

fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().nfc().collect()
}

#[cfg(test)]
fn has_ready_prompt(visible_text: &str) -> bool {
    // Current Claude releases render placeholder text after the input glyph
    // and keep two footer rows below it. Restrict recognition to that input
    // area: a historical/user-message glyph higher in the transcript is not
    // readiness evidence.
    visible_text.lines().rev().take(3).any(is_prompt_input_line)
}

#[cfg(test)]
fn is_prompt_input_line(line: &str) -> bool {
    let mut characters = line.trim_start().chars();
    characters.next() == Some('❯') && characters.next().is_none_or(char::is_whitespace)
}

fn blocking_screen(visible_text: &str) -> Option<NeedsInput> {
    let lower = visible_text.to_ascii_lowercase();
    if lower.contains("do you trust the files")
        || lower.contains("trust this folder")
        || lower.contains("trust this directory")
    {
        Some(typed_needs_input(NeedsInputKind::Trust))
    } else if (lower.contains("log in to claude")
        || lower.contains("login to claude")
        || lower.contains("not logged in"))
        || lower.contains("authentication required") && lower.contains("claude")
    {
        Some(typed_needs_input(NeedsInputKind::Login))
    } else if lower.contains("update required")
        || lower.contains("please update claude code")
        || lower.contains("new version of claude code is required")
    {
        Some(typed_needs_input(NeedsInputKind::Update))
    } else if lower.contains("usage limit exceeded")
        || lower.contains("usage limit reached")
        || (lower.contains("rate limit")
            && (lower.contains("exceeded") || lower.contains("reached")))
        || (lower.contains("quota") && (lower.contains("exceeded") || lower.contains("reached")))
    {
        Some(typed_needs_input(NeedsInputKind::Quota))
    } else if lower.contains("permission") && (lower.contains("allow") || lower.contains("deny"))
        || lower.contains("allow this tool")
        || (lower.contains("do you want to proceed")
            && lower.contains("yes")
            && lower.contains("no"))
    {
        Some(typed_needs_input(NeedsInputKind::Permission))
    } else if lower.contains("esc to cancel")
        && (lower.contains("enter to confirm")
            || lower.contains("enter to select")
            || lower.contains("press enter")
            || lower.contains("yes") && lower.contains("no"))
    {
        Some(typed_needs_input(NeedsInputKind::UnknownModal))
    } else {
        None
    }
}

fn typed_needs_input(kind: NeedsInputKind) -> NeedsInput {
    NeedsInput {
        kind,
        message: match kind {
            NeedsInputKind::Trust => "Claude requires workspace trust confirmation",
            NeedsInputKind::Login => "Claude requires interactive login",
            NeedsInputKind::Permission => "Claude requires a permission decision",
            NeedsInputKind::Update => "Claude requires interactive update input",
            NeedsInputKind::Quota => "Claude requires quota or rate-limit input",
            NeedsInputKind::UnknownModal => "Claude requires interactive input",
        }
        .to_owned(),
        details: serde_json::Value::Null,
    }
}

fn metadata_for(path: &Path) -> DriverResult<FileMetadata> {
    let metadata = std::fs::metadata(path).map_err(|error| io_failure(path, error))?;
    platform_file_metadata(&metadata)
}

fn metadata_for_file(file: &File, path: &Path) -> DriverResult<FileMetadata> {
    let metadata = file.metadata().map_err(|error| io_failure(path, error))?;
    platform_file_metadata(&metadata)
}

#[cfg(unix)]
fn platform_file_metadata(metadata: &std::fs::Metadata) -> DriverResult<FileMetadata> {
    use std::os::unix::fs::MetadataExt;
    Ok(FileMetadata {
        identity: FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        },
        len: metadata.len(),
    })
}

#[cfg(not(unix))]
fn platform_file_metadata(_metadata: &std::fs::Metadata) -> DriverResult<FileMetadata> {
    Err(DriverFailure::new(
        ErrorCode::UnsupportedFeature,
        "filesystem transcript identity is not implemented on this platform",
    ))
}

fn ensure_same_transcript_generation(
    expected: FileIdentity,
    actual: FileIdentity,
) -> DriverResult<()> {
    if expected == actual {
        return Ok(());
    }
    Err(DriverFailure::new(
        ErrorCode::TranscriptUnavailable,
        "transcript file generation changed during an active filesystem observation",
    ))
}

fn ensure_same_transcript_length(
    expected: u64,
    actual: u64,
    message: &'static str,
) -> DriverResult<()> {
    if expected == actual {
        return Ok(());
    }
    Err(DriverFailure::new(ErrorCode::SchemaDrift, message))
}

fn closed_terminal() -> DriverFailure {
    DriverFailure::new(ErrorCode::SessionNotFound, "terminal session is closed")
}

/// Maps a private-terminal failure onto the wire.
///
/// ## Why this takes the terminal
///
/// `TerminalBackendError::ControlPlaneLost` used to mean one thing, because
/// there was only one thing it could mean: every session in the process shared
/// a single rmux connection, and rmux-sdk's poison latch is write-once, so the
/// first lost request killed terminal I/O for the whole daemon. Per-session
/// transports end that. The variant is now scoped to the terminal that produced
/// it and, on its own, says nothing about the sidecar or about any sibling
/// session.
///
/// That leaves two genuinely different situations sharing one variant, and
/// `TerminalSession::lease_lost` is the plumbed discriminator between them: the
/// lease heartbeat runs on its own dedicated connection (rmux-sdk
/// owned_session/lease.rs:39, :214-217), so it cannot be latched by a poisoned
/// operation. A lost lease is therefore evidence of a *different* failure than
/// a poisoned operation, not evidence of a stronger one — that connection can
/// latch on its own renew timeout, and the limits of the signal are set out on
/// `pseudomux_rmux::RmuxTerminal::classify`. A control-plane loss without a
/// lost lease is evidence only that *a* connection died while the pane very
/// probably kept running. The message says which, so an operator reading a
/// `daemon_lost` is not told a session-scoped fault is a global outage.
///
/// It is used for the message and not for `retryable`, deliberately. The
/// discriminator is not sound in either direction, and the `false` side is the
/// disqualifying one here: the heartbeat renews every
/// `(ttl/3).max(100ms)` and retries until `last_success + ttl` (rmux-sdk
/// owned_session/lease.rs:70-77, :176-210), so for up to one lease TTL after a
/// real sidecar death `lease_lost()` still reads `false` while every snapshot
/// fails immediately. Keying a wire flag on it would report a genuine daemon
/// death as non-retryable for seconds — the exact inversion of what
/// `full_stack.rs::active_public_turn_sidecar_loss_is_typed_and_reaps_the_process_boundary`
/// pins for a sidecar `SIGKILL`.
///
/// ## Why `DaemonLost` stays retryable
///
/// `.context/review/transport-fix-design.md` staged this layer with a
/// `retryable: false` flip. It is not taken, and the reasons are recorded here
/// rather than left as a silent omission:
///
/// * The staged flip was derived from the daemon-wide meaning, where a lost
///   control plane was permanent by construction — one latch, never cleared, no
///   reconnect API. Narrowing the blast radius makes a retry *more* likely to
///   succeed, not less: every read on this terminal is already minted on a
///   fresh connection and recovers on the next call, which
///   `private_runtime.rs::private_terminal_read_recovers_after_the_sdk_aborts_a_transport_on_its_own_deadline`
///   proves against a real stalled sidecar. Moving `retryable` from `true` to
///   `false` on strictly better news is not a defensible direction.
/// * **The residue this paragraph used to name is CLOSED, and closing it is
///   what makes the `true` honest rather than aspirational.** Writes used to
///   ride one connection the terminal could not rebind, so once it latched,
///   `interrupt` (:1108) and `resize` (:923) kept answering `DaemonLost` with
///   `retryable: true` for a terminal that could never write again — a retry
///   the caller could not possibly win, and the one claim layer (b) was left
///   holding. Layer (b) mints a handle per write from the same lazy facade
///   reads use (`pseudomux_rmux::RmuxTerminal::write_pane`), so a latched write
///   connection is one no later write will touch, and
///   `private_runtime.rs::private_terminal_write_recovers_after_the_sdk_aborts_its_write_transport`
///   proves the recovery against a real stalled sidecar. `paste` and `enter`
///   were never in this residue at all: they do not reach this mapper, ending
///   instead in [`paste_ambiguity_failure`] (`PromptNotAcknowledged`) and
///   [`enter_ambiguity_failure`] (`RecoveryFailed`), both already
///   non-retryable.
/// * It is wire-visible and already pinned:
///   `full_stack.rs::active_public_turn_sidecar_loss_is_typed_and_reaps_the_process_boundary`
///   asserts `daemon_lost` *and* `retryable` for a real sidecar `SIGKILL`
///   during an active turn, and that failure reaches the wire through this
///   function. Flipping the flag would require rewriting that assertion, which
///   is the opposite of what an assertion is for. Cited by test name and not by
///   line: the `:394-395` this used to carry had drifted to `:822-823`, which is
///   what a line citation always eventually does.
///
/// `ErrorCode::DaemonLost` itself is also kept. The v1 error surface is closed
/// and asserted whole by `crates/protocol/tests/v1_conformance_vectors.rs`, so
/// a session-scoped code would be a protocol addition; the honest scope is
/// carried in the message instead.
fn map_terminal_error(
    terminal: &dyn TerminalSession,
    error: TerminalBackendError,
) -> DriverFailure {
    let (code, message) = match error {
        TerminalBackendError::InvalidLaunch(_) => {
            (ErrorCode::InvalidConfig, "terminal request was invalid")
        }
        TerminalBackendError::ControlPlaneLost => (
            ErrorCode::DaemonLost,
            if terminal.lease_lost() {
                "private rmux session lease was lost"
            } else {
                "private rmux session control plane was lost"
            },
        ),
        TerminalBackendError::Rmux(_) => {
            (ErrorCode::RmuxUnavailable, "private rmux operation failed")
        }
        TerminalBackendError::ProcessBoundary(_) => (
            ErrorCode::RmuxUnavailable,
            "terminal process-boundary operation failed",
        ),
    };
    DriverFailure::new(code, message).retryable(matches!(
        code,
        ErrorCode::DaemonLost | ErrorCode::RmuxUnavailable
    ))
}

fn map_location_error(error: TranscriptLocationError) -> DriverFailure {
    let code = match error {
        TranscriptLocationError::RelativeConfigRoot(_)
        | TranscriptLocationError::InvalidCwd { .. } => ErrorCode::InvalidConfig,
        TranscriptLocationError::NotFound { .. } => ErrorCode::TranscriptUnavailable,
        TranscriptLocationError::Ambiguous { .. } | TranscriptLocationError::ScanLimit => {
            ErrorCode::SchemaDrift
        }
        TranscriptLocationError::Io { .. } => ErrorCode::TranscriptUnavailable,
    };
    DriverFailure::new(code, error.to_string())
}

fn map_transcript_error(error: pseudomux_claude::TranscriptError) -> DriverFailure {
    DriverFailure::new(ErrorCode::SchemaDrift, error.to_string())
}

fn io_failure(path: &Path, error: std::io::Error) -> DriverFailure {
    DriverFailure::new(
        ErrorCode::TranscriptUnavailable,
        format!("failed to read transcript {}: {error}", path.display()),
    )
}

fn poisoned_tail<T>(_error: std::sync::PoisonError<T>) -> DriverFailure {
    DriverFailure::new(ErrorCode::Internal, "transcript tail lock was poisoned")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v1::{
        Clock, SessionActorConfig, SessionRegistration, SessionRegistry, StoredTurnTerminal,
    };
    use pseudomux_protocol::v1::{
        CloseSessionRequest, CompatibilityReport, InputTransport, RunTurnRequest, SessionCell,
        SessionGenerationId, TerminalProfile, TurnLeasePolicy, TurnRequest,
    };
    use pseudomux_rmux::{BackendSessionRef, TerminalCursor};
    use serde_json::Value;
    use std::collections::VecDeque;
    use std::io::Write;
    use std::sync::atomic::AtomicUsize;
    use tempfile::TempDir;

    // -----------------------------------------------------------------------
    // THE RENDERING REGISTER
    //
    // pmux's correctness depends on undocumented behaviour of a program it does
    // not control, observed through a terminal. There is no API contract with
    // Claude Code -- there is a rendering. So every guard below is a claim about
    // somebody else's UI, and the two tests here exist to make the SET of those
    // claims derived rather than remembered.
    //
    // The defect that motivated them: `blocking_screen` recognized 24 screen
    // shapes and answered `Option<NeedsInput>`, and `None` -- "no rule matched"
    // -- reached every caller as the same value as "this is an ordinary
    // non-modal screen". A real "trust this directory" screen pmux had not been
    // taught was therefore PROCEED, and the turn ran to its 600,000 ms deadline
    // sitting on a modal.
    // -----------------------------------------------------------------------

    /// What happens at a rendering site when the frame matches nothing it knows.
    ///
    /// The crux the register exists to record is whether "matched nothing" is
    /// DISTINGUISHABLE from "matched a negative". Those two being one value is
    /// exactly how the modal hang happened, so every row has to say which it is.
    #[cfg(unix)]
    #[derive(Debug)]
    enum Unmatched {
        /// Answers [`TerminalScreenState::Unrecognised`]: a value of its own,
        /// carrying [`ScreenShape`], that no caller can read as a negative.
        Distinct,
        /// Answers `None` or `false`, which its callers read as "no match" --
        /// and the named caller is the one that turns that into a refusal or
        /// into a distinct outcome. The string names WHO closes it, because a
        /// row of this kind is only safe if somebody downstream does.
        ClosedByCaller(&'static str),
        /// Refuses on the spot, by returning an error that names the gate.
        Refuses(&'static str),
        /// Reads a frame and decides nothing about it -- a describer, a
        /// converter or a recorder. Nothing can be "unmatched" here.
        DecidesNothing(&'static str),
        /// Declared `#[cfg(test)]`. Checked, not trusted: the test asserts the
        /// attribute is really on the declaration.
        TestOnly(&'static str),
    }

    /// Every function in this crate that turns a RENDERING into a decision, and
    /// what each does with a frame it does not recognize.
    ///
    /// The left column is checked against a scan of this crate's own source --
    /// see [`every_rendering_decision_site_is_registered`] -- so a function that
    /// starts reading a frame tomorrow fails this test by name until somebody
    /// says what its unrecognized arm does.
    #[cfg(unix)]
    const RENDERING_SITES: &[(&str, Unmatched)] = &[
        // -- The classifier, and the three predicates it is built out of ------
        (
            "driver_io.rs::classify_terminal_snapshot",
            Unmatched::Distinct,
        ),
        (
            "driver_io.rs::blocking_screen",
            Unmatched::ClosedByCaller(
                "`classify_terminal_snapshot`, whose every non-matching arm falls through to \
                 `Unrecognised`; this is the function whose `None` used to mean PROCEED",
            ),
        ),
        (
            "driver_io.rs::has_ready_prompt",
            Unmatched::ClosedByCaller(
                "`classify_terminal_snapshot`, which answers `Unrecognised` when this is false \
                 and no modal matched either",
            ),
        ),
        (
            "driver_io.rs::active_editor",
            Unmatched::ClosedByCaller(
                "`classify_terminal_snapshot`, which on `None` tries `blocking_screen` and then \
                 answers `Unrecognised`",
            ),
        ),
        // -- The gates: each refuses by running out of a bounded budget -------
        (
            "driver_io.rs::prove_stable_empty_editor",
            Unmatched::Refuses("the input gate's budget, which refuses naming Gate 1"),
        ),
        (
            "driver_io.rs::wait_for_stable_prompt_render",
            Unmatched::Refuses("the input gate's budget"),
        ),
        (
            "driver_io.rs::wait_for_stable_control_render",
            Unmatched::Refuses("the control-channel budget"),
        ),
        (
            "driver_io.rs::prove_control_command_selection",
            Unmatched::Refuses(
                "the `/clear` selection proof, which refuses when no row carries the selected \
                 colour rather than assuming the first",
            ),
        ),
        (
            "driver_io.rs::submit_prompt",
            Unmatched::Refuses("whichever of the gates it drives ran out of budget first"),
        ),
        (
            "driver_io.rs::composer_command_colour",
            Unmatched::ClosedByCaller(
                "`prove_control_command_selection`, which refuses when the composer carries \
                 no explicit colour to compare candidates against",
            ),
        ),
        (
            "driver_io.rs::candidate_body_colour",
            Unmatched::ClosedByCaller(
                "`prove_control_command_selection`, which treats a non-uniform or colourless \
                 body as unselected rather than as a default row",
            ),
        ),
        // -- Describers: they report a frame, they do not judge one -----------
        (
            "driver_io.rs::of",
            Unmatched::DecidesNothing(
                "`ScreenShape::of` IS the description an unrecognized \
                                      screen is reported by",
            ),
        ),
        (
            "driver_io.rs::screen_geometry",
            Unmatched::DecidesNothing(
                "reports a resolved composer's geometry for offline analysis",
            ),
        ),
        (
            "driver_io.rs::gated_styled_screen",
            Unmatched::DecidesNothing(
                "reads and records the one frame that carries cell colours; the decision is \
                 `prove_control_command_selection`'s",
            ),
        ),
        (
            "native.rs::startup_screen_diagnostics",
            Unmatched::DecidesNothing(
                "builds the `details` of a startup refusal, on top of `ScreenShape::to_json`",
            ),
        ),
        (
            "native.rs::launch_bundle_rejected",
            Unmatched::ClosedByCaller(
                "`startup_screen_diagnostics`, which publishes it as the `repromotion_trigger` \
                 diagnostic key; `false` is read as `null`, never as a passing gate",
            ),
        ),
        // -- The corpus: conversion and offline invariants --------------------
        (
            "screen_corpus.rs::from_snapshot",
            Unmatched::DecidesNothing("records a frame verbatim"),
        ),
        (
            "screen_corpus.rs::to_terminal_snapshot",
            Unmatched::DecidesNothing("replays a recorded frame verbatim"),
        ),
        (
            "screen_corpus.rs::to_styled_screen",
            Unmatched::DecidesNothing("replays a recorded styled frame verbatim"),
        ),
        (
            "screen_corpus.rs::from_cell",
            Unmatched::DecidesNothing("converts one styled cell into its recorded form"),
        ),
        (
            "screen_corpus.rs::to_cell",
            Unmatched::DecidesNothing("converts one recorded cell back"),
        ),
        (
            "screen_corpus.rs::check_frame",
            Unmatched::Refuses(
                "the offline corpus invariants, which report a violation per frame rather than \
                 passing a frame they could not classify",
            ),
        ),
        // -- Test-only --------------------------------------------------------
        (
            "driver_io.rs::classify_terminal_screen",
            Unmatched::TestOnly(
                "the text-only classifier used by this file's own fixtures; production reads \
                 frames, never bare text",
            ),
        ),
    ];

    /// What one function that calls `.snapshot()` or `.styled_screen()` does
    /// with the frame it read.
    #[cfg(unix)]
    #[derive(Debug)]
    enum FrameRead {
        /// Reads a real terminal frame and records it, under this site label.
        Recorded(&'static str),
        /// The `snapshot()` in this body is not a terminal frame at all. The
        /// scan matches a method NAME, which over-collects across every type in
        /// the crate that happens to have one; the reason says which type.
        NotAFrame(&'static str),
    }

    /// Every function that reads a frame from a terminal, and where that frame
    /// goes.
    ///
    /// [`crate::screen_corpus`]'s module documentation argues for recording
    /// "every 25 ms for the length of every turn". For a long time only the
    /// input gate recorded anything, so the per-turn polls -- the reads the
    /// modal hang happened underneath -- were taken and thrown away. That is a
    /// paragraph outrunning its own call sites, and this table is what stops it
    /// recurring.
    #[cfg(unix)]
    const FRAME_READS: &[(&str, FrameRead)] = &[
        (
            "driver_io.rs::gated_snapshot",
            FrameRead::Recorded("input_gate.pre_paste / input_gate.post_paste"),
        ),
        (
            "driver_io.rs::gated_styled_screen",
            FrameRead::Recorded(CONTROL_CHANNEL_SELECTION_SITE),
        ),
        (
            "driver_io.rs::wait_for_snapshot_stability",
            FrameRead::Recorded(SCREEN_STABILITY_SITE),
        ),
        (
            "driver_io.rs::completion_evidence",
            FrameRead::Recorded(COMPLETION_GATE_EVIDENCE_SITE),
        ),
        (
            "driver_io.rs::observe_screen",
            FrameRead::Recorded(TURN_MONITOR_SITE),
        ),
        (
            "driver_io.rs::interrupt",
            FrameRead::Recorded(INTERRUPT_RECOVERY_SITE),
        ),
        (
            "native.rs::wait_until_ready_with_timings",
            FrameRead::Recorded(STARTUP_READINESS_SITE),
        ),
        (
            "native.rs::diagnose",
            FrameRead::NotAFrame(
                "`SessionActorHandle::snapshot` -- an actor STATE read, not a terminal capture",
            ),
        ),
        (
            "stateless.rs::run_pool_turn",
            FrameRead::NotAFrame("`SessionActorHandle::snapshot`"),
        ),
        (
            "v1/actor.rs::handle_command",
            FrameRead::NotAFrame("`SessionActor::snapshot`, the actor's own state"),
        ),
        (
            "v1/actor.rs::event_batch",
            FrameRead::NotAFrame("`SessionActor::snapshot`, the actor's own state"),
        ),
        (
            "v1/registry.rs::inspect",
            FrameRead::NotAFrame("`SessionActorHandle::snapshot`"),
        ),
    ];

    /// Every read of a terminal frame is recorded to the screen corpus, and the
    /// site labels are the constants rather than loose strings.
    ///
    /// FOUND BY THIS TEST, which is the reason to write it rather than to read
    /// the file: `RmuxTerminalControl::interrupt`'s recovery loop classified
    /// every frame it took and recorded none of them -- so the recording a
    /// failed recovery most needs, what the pane was showing while it refused to
    /// come back, was the one nothing kept.
    #[cfg(unix)]
    #[test]
    fn every_classified_read_is_recorded_to_the_screen_corpus() {
        // Deliberately `contains` and NOT `source_scan::calls`. `calls` requires
        // the character before the name to be a non-word character, which for a
        // method call means it matches `)\n    .snapshot()` and misses
        // `terminal.snapshot()` -- so it would have derived the set of reads
        // that happen to be written as a WRAPPED chain. That is a rule about
        // line width, and it collapsed the derived set from twelve to two under
        // an unrelated edit before this comment was written.
        let declared = crate::source_scan::declared_functions();
        let derived: BTreeSet<String> = declared
            .iter()
            .filter(|function| {
                function.body.contains(".snapshot()") || function.body.contains(".styled_screen()")
            })
            .map(|function| format!("{}::{}", function.file, function.name))
            .collect();
        let registered: BTreeSet<String> = FRAME_READS
            .iter()
            .map(|(site, _)| (*site).to_owned())
            .collect();
        assert_eq!(
            derived,
            registered,
            "the frame-read table and this crate's source disagree.\n  \
             reads something called `snapshot` and is unregistered: {:?}\n  \
             registered but no longer reads one: {:?}",
            derived.difference(&registered).collect::<Vec<&String>>(),
            registered.difference(&derived).collect::<Vec<&String>>(),
        );

        for (site, read) in FRAME_READS {
            let function = declared
                .iter()
                .find(|function| &format!("{}::{}", function.file, function.name) == site)
                .expect("every registered read was derived from a declaration");
            let records = crate::source_scan::calls(&function.body, "record_snapshot")
                || crate::source_scan::calls(&function.body, "record_styled");
            match read {
                FrameRead::Recorded(label) => assert!(
                    records,
                    "{site} reads a terminal frame and is registered as recording it to \
                     `{label}`, but its body calls neither `record_snapshot` nor `record_styled`"
                ),
                FrameRead::NotAFrame(reason) => assert!(
                    !records,
                    "{site} is registered as not reading a terminal frame ({reason}), yet it \
                     records one to the screen corpus"
                ),
            }
        }

        // Every site label this file declares is USED, and every recording
        // passes a declared constant rather than a loose string. Two spellings
        // of one site is a corpus that cannot be grouped by site, which is the
        // only axis the census reports on.
        let source = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("src")
                .join("driver_io.rs"),
        )
        .expect("driver_io.rs must be readable");
        let production = source
            .split_once("\n#[cfg(test)]\nmod tests {")
            .map_or(source.as_str(), |(before, _)| before);
        let declared_labels: Vec<&str> = production
            .lines()
            .filter_map(|line| line.split_once("_SITE: &str = \""))
            .filter_map(|(_, rest)| rest.split_once('"'))
            .map(|(label, _)| label)
            .collect();
        assert!(
            declared_labels.len() >= 7,
            "the site-label scan found only {} label(s); a derivation that stopped matching \
             must refuse, not report an empty set",
            declared_labels.len()
        );
        let mut sorted = declared_labels.clone();
        sorted.sort_unstable();
        let before = sorted.len();
        sorted.dedup();
        assert_eq!(before, sorted.len(), "two site constants share one label");

        for (index, line) in production.lines().enumerate() {
            let Some((_, rest)) = line
                .split_once("record_snapshot(")
                .or_else(|| line.split_once("record_styled("))
            else {
                continue;
            };
            // The call may wrap, in which case the argument is on the next
            // line; either way it must not be a string literal.
            let argument = if rest.trim().is_empty() {
                production
                    .lines()
                    .nth(index + 1)
                    .unwrap_or_default()
                    .trim()
                    .to_owned()
            } else {
                rest.trim().to_owned()
            };
            assert!(
                !argument.starts_with('"'),
                "line {} passes a string literal to the recorder; site labels are the \
                 `*_SITE` constants so the recorder and this sweep cannot disagree about a \
                 spelling: {line}",
                index + 1
            );
        }
    }

    /// Every call this crate makes that turns a rendering into a decision is a
    /// row of [`RENDERING_SITES`], and every row is still such a call.
    ///
    /// # The derivation
    ///
    /// A rendering enters pmux through exactly three names: `visible_text` (a
    /// captured frame's text), `StyledScreen` and `CellColor` (a captured
    /// frame's cells). Any production function whose BODY mentions one of them
    /// is holding a rendering, so it is a site. That is the "scan for the
    /// operations, not for a list of names" rule: nothing here knows what the
    /// interesting functions are called.
    ///
    /// It OVER-collects on purpose -- a describer and a converter are sites by
    /// this rule -- and over-collection is the safe direction, because the cost
    /// is a register row saying `DecidesNothing` and the cost of
    /// under-collecting is the modal hang.
    #[cfg(unix)]
    #[test]
    fn every_rendering_decision_site_is_registered() {
        // The three ways a rendering enters this crate. A frame reaches code
        // only as text or as cells; there is no third representation.
        const RENDERING_OPERATIONS: [&str; 3] = ["visible_text", "StyledScreen", "CellColor"];

        let declared = crate::source_scan::declared_functions();
        let derived: BTreeSet<String> = declared
            .iter()
            .filter(|function| {
                RENDERING_OPERATIONS
                    .iter()
                    .any(|operation| function.body.contains(operation))
            })
            .map(|function| format!("{}::{}", function.file, function.name))
            .collect();
        let registered: BTreeSet<String> = RENDERING_SITES
            .iter()
            .map(|(site, _)| (*site).to_owned())
            .collect();

        // The register, rendered. This is the INVENTORY the whole exercise asks
        // for -- every site where a rendering becomes a decision, and what each
        // one does when the frame matches nothing -- and printing it on failure
        // is what makes each row's reason a thing that is read rather than a
        // comment in a table nobody prints.
        let table: Vec<String> = RENDERING_SITES
            .iter()
            .map(|(site, unmatched)| match unmatched {
                Unmatched::Distinct => {
                    format!("{site} -> unmatched is DISTINCT: TerminalScreenState::Unrecognised")
                }
                Unmatched::ClosedByCaller(caller) => {
                    format!("{site} -> unmatched is None/false, closed by {caller}")
                }
                Unmatched::Refuses(gate) => format!("{site} -> unmatched refuses: {gate}"),
                Unmatched::DecidesNothing(what) => format!("{site} -> decides nothing: {what}"),
                Unmatched::TestOnly(what) => format!("{site} -> test-only: {what}"),
            })
            .collect();

        assert_eq!(
            derived,
            registered,
            "the rendering register and this crate's source disagree.\n  \
             reads a rendering but is not registered (say what its unrecognized arm does): {:?}\n  \
             registered but no longer reads a rendering (delete the row): {:?}\n  \
             the register says: {table:#?}",
            derived.difference(&registered).collect::<Vec<&String>>(),
            registered.difference(&derived).collect::<Vec<&String>>(),
        );

        // A register that says `TestOnly` must be telling the truth about the
        // declaration, or it is a place to park a production site.
        for (site, unmatched) in RENDERING_SITES {
            let Unmatched::TestOnly(_) = unmatched else {
                continue;
            };
            let (file, name) = site.split_once("::").expect("every row is `file::name`");
            let source = std::fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(file),
            )
            .unwrap_or_else(|error| panic!("{file} must be readable: {error}"));
            let declaration = source
                .find(&format!("fn {name}("))
                .unwrap_or_else(|| panic!("{site} must declare `fn {name}(`"));
            assert!(
                source[..declaration].ends_with("#[cfg(test)]\n#[must_use]\n")
                    || source[..declaration].ends_with("#[cfg(test)]\n"),
                "{site} is registered `TestOnly`, but its declaration does not carry \
                 `#[cfg(test)]`"
            );
        }

        // The whole point of the enum: exactly one arm may answer with a value
        // of its own, and it must be the classifier. Anything else claiming
        // `Distinct` is claiming to return `TerminalScreenState`, so it has to
        // be able to.
        for (site, unmatched) in RENDERING_SITES {
            if matches!(unmatched, Unmatched::Distinct) {
                let function = declared
                    .iter()
                    .find(|function| &format!("{}::{}", function.file, function.name) == site)
                    .expect("every registered site was derived from a declaration");
                assert!(
                    crate::source_scan::calls(&function.body, "TerminalScreenState::Unrecognised"),
                    "{site} is registered `Distinct`, so its body must be able to answer \
                     `TerminalScreenState::Unrecognised`"
                );
            }
        }
    }

    #[derive(Default)]
    struct FakeTerminalState {
        before_paste: VecDeque<TerminalSnapshot>,
        after_paste: VecDeque<TerminalSnapshot>,
        /// Frames with cell colours, served to `styled_screen` only. Kept
        /// separate from `after_paste` rather than replacing it so that every
        /// existing plain-text fixture keeps meaning exactly what it meant, and
        /// a test that never types a control command is never asked to invent
        /// colours it did not measure.
        styled_after_paste: VecDeque<StyledScreen>,
        pasted: bool,
        paste_count: usize,
        /// Exactly what was handed to the terminal. The interesting question
        /// about the slash boundary is not which errors the guard returns, it is
        /// which bytes reach the TUI, so the fake records them.
        pasted_text: Vec<String>,
        /// Runs inside `enter`, which is where a real `/clear` takes effect.
        on_enter: Option<Box<dyn Fn() + Send>>,
        enter_count: usize,
        paste_error: bool,
        enter_error: bool,
        /// How long the write stays in flight after the terminal has recorded
        /// it, so a caller's deadline can expire *inside* `paste`/`enter`
        /// rather than before or after one.
        ///
        /// The counters and the recorded text are updated BEFORE the sleep, on
        /// purpose: the real `RmuxTerminal` detaches every write onto a spawned
        /// task that runs to completion whatever the caller does, so a write
        /// whose caller stopped waiting is a write that still happened. A fake
        /// that incremented afterwards would model a write that unhappens.
        paste_delay: Duration,
        enter_delay: Duration,
        lease_lost: bool,
        lose_lease_after_paste: bool,
        lose_lease_after_snapshot: Option<usize>,
        snapshot_count: usize,
        cycle_before: bool,
        cycle_after: bool,
    }

    #[derive(Clone)]
    struct FakeTerminalHandle {
        state: Arc<StdMutex<FakeTerminalState>>,
    }

    impl FakeTerminalHandle {
        fn counts(&self) -> (usize, usize) {
            let state = self.state.lock().unwrap();
            (state.paste_count, state.enter_count)
        }

        fn pasted_text(&self) -> Vec<String> {
            self.state.lock().unwrap().pasted_text.clone()
        }

        fn on_enter(&self, hook: impl Fn() + Send + 'static) {
            self.state.lock().unwrap().on_enter = Some(Box::new(hook));
        }

        fn serve_styled(&self, screens: impl IntoIterator<Item = StyledScreen>) {
            self.state.lock().unwrap().styled_after_paste = screens.into_iter().collect();
        }

        /// Holds the next `paste` in flight for `delay`, so a budget can expire
        /// inside the write instead of at one of the reads around it.
        fn hold_paste(&self, delay: Duration) {
            self.state.lock().unwrap().paste_delay = delay;
        }

        /// [`Self::hold_paste`] for the one irreversible write.
        fn hold_enter(&self, delay: Duration) {
            self.state.lock().unwrap().enter_delay = delay;
        }
    }

    struct FakeTerminal {
        reference: BackendSessionRef,
        state: Arc<StdMutex<FakeTerminalState>>,
    }

    impl FakeTerminal {
        fn new(
            before_paste: impl IntoIterator<Item = TerminalSnapshot>,
            after_paste: impl IntoIterator<Item = TerminalSnapshot>,
        ) -> (Self, FakeTerminalHandle) {
            let state = Arc::new(StdMutex::new(FakeTerminalState {
                before_paste: before_paste.into_iter().collect(),
                after_paste: after_paste.into_iter().collect(),
                ..FakeTerminalState::default()
            }));
            (
                Self {
                    reference: BackendSessionRef {
                        rmux_session_name: "fake-input-gate".to_owned(),
                        pane_id: 1,
                    },
                    state: Arc::clone(&state),
                },
                FakeTerminalHandle { state },
            )
        }

        fn next_snapshot(queue: &mut VecDeque<TerminalSnapshot>, cycle: bool) -> TerminalSnapshot {
            if cycle {
                let snapshot = queue
                    .pop_front()
                    .expect("fake snapshot queue must not be empty");
                queue.push_back(snapshot.clone());
                return snapshot;
            }
            if queue.len() > 1 {
                queue.pop_front().expect("snapshot queue was non-empty")
            } else {
                queue
                    .front()
                    .cloned()
                    .expect("fake snapshot queue must not be empty")
            }
        }

        fn unsupported() -> TerminalBackendError {
            TerminalBackendError::Rmux("unexpected fake wait operation".to_owned())
        }
    }

    #[async_trait]
    impl TerminalSession for FakeTerminal {
        fn backend_ref(&self) -> &BackendSessionRef {
            &self.reference
        }

        fn lease_lost(&self) -> bool {
            self.state.lock().unwrap().lease_lost
        }

        async fn snapshot(&self) -> Result<TerminalSnapshot, TerminalBackendError> {
            let mut state = self.state.lock().unwrap();
            state.snapshot_count += 1;
            let snapshot = if state.pasted {
                let cycle = state.cycle_after;
                Self::next_snapshot(&mut state.after_paste, cycle)
            } else {
                let cycle = state.cycle_before;
                Self::next_snapshot(&mut state.before_paste, cycle)
            };
            if state.lose_lease_after_snapshot == Some(state.snapshot_count) {
                state.lease_lost = true;
            }
            Ok(snapshot)
        }

        /// Serves the frames a control-command test measured. A fake that was
        /// given none refuses, because "this double has no colours" and "this
        /// screen has no highlight" are different answers and only one of them
        /// may ever be inferred from the other.
        async fn styled_screen(&self) -> Result<StyledScreen, TerminalBackendError> {
            let mut state = self.state.lock().unwrap();
            state.snapshot_count += 1;
            if state.styled_after_paste.is_empty() {
                return Err(TerminalBackendError::Rmux(
                    "fake terminal was given no styled frames".to_owned(),
                ));
            }
            let screen = if state.styled_after_paste.len() > 1 {
                state
                    .styled_after_paste
                    .pop_front()
                    .expect("styled queue was non-empty")
            } else {
                state
                    .styled_after_paste
                    .front()
                    .cloned()
                    .expect("styled queue was non-empty")
            };
            if state.lose_lease_after_snapshot == Some(state.snapshot_count) {
                state.lease_lost = true;
            }
            Ok(screen)
        }

        async fn wait_visible_text(
            &self,
            _needle: &str,
            _timeout: Duration,
        ) -> Result<TerminalSnapshot, TerminalBackendError> {
            Err(Self::unsupported())
        }

        async fn wait_quiet(
            &self,
            _stable_for: Duration,
            _timeout: Duration,
        ) -> Result<TerminalSnapshot, TerminalBackendError> {
            Err(Self::unsupported())
        }

        async fn paste(&mut self, text: &str) -> Result<(), TerminalBackendError> {
            let delay = {
                let mut state = self.state.lock().unwrap();
                state.paste_count += 1;
                state.pasted_text.push(text.to_owned());
                state.pasted = true;
                if state.lose_lease_after_paste {
                    state.lease_lost = true;
                }
                if state.paste_error {
                    return Err(TerminalBackendError::Rmux(
                        "private paste failure diagnostics".to_owned(),
                    ));
                }
                state.paste_delay
            };
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            Ok(())
        }

        async fn enter(&mut self) -> Result<(), TerminalBackendError> {
            let delay = {
                let mut state = self.state.lock().unwrap();
                state.enter_count += 1;
                if let Some(hook) = state.on_enter.as_ref() {
                    hook();
                }
                if state.enter_error {
                    return Err(TerminalBackendError::Rmux(
                        "private Enter failure diagnostics".to_owned(),
                    ));
                }
                state.enter_delay
            };
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            Ok(())
        }

        async fn interrupt(&mut self) -> Result<(), TerminalBackendError> {
            Ok(())
        }

        async fn resize(&mut self, _rows: u16, _cols: u16) -> Result<(), TerminalBackendError> {
            Ok(())
        }

        async fn close(&mut self) -> Result<bool, TerminalBackendError> {
            Ok(true)
        }
    }

    fn structured_snapshot(
        revision: u64,
        lines: impl IntoIterator<Item = &'static str>,
        cursor_row: u16,
        cursor_col: u16,
        cursor_visible: bool,
    ) -> TerminalSnapshot {
        let lines: Vec<_> = lines.into_iter().collect();
        TerminalSnapshot {
            revision,
            rows: lines.len().try_into().unwrap(),
            cols: 80,
            cursor: Some(TerminalCursor {
                row: cursor_row,
                col: cursor_col,
                visible: cursor_visible,
                style: 0,
            }),
            visible_text: lines.join("\n"),
        }
    }

    fn baseline_snapshot(revision: u64) -> TerminalSnapshot {
        structured_snapshot(
            revision,
            ["", "", "", "", "", "❯ Try something", "footer", "status"],
            5,
            2,
            true,
        )
    }

    fn rendered_snapshot(revision: u64, input: &'static str, cursor_col: u16) -> TerminalSnapshot {
        rendered_row_snapshot(revision, input, cursor_col)
    }

    /// [`rendered_snapshot`] for a row that is built at runtime, so a measured
    /// capture can be reproduced beside the prompt that produced it instead of
    /// being retyped as a literal.
    /// A screen holding a composer of `rows`, with the cursor at the end of the
    /// last one — which is where the composer MEASURABLY leaves it after a
    /// paste, on every 2.1.226 frame this session recorded.
    fn composer_snapshot(revision: u64, rows: &[&str]) -> TerminalSnapshot {
        let mut lines: Vec<String> = vec![String::new(); 5];
        lines.extend(rows.iter().map(|row| (*row).to_owned()));
        lines.push("footer".to_owned());
        lines.push("status".to_owned());
        let cursor_row = u16::try_from(4 + rows.len()).unwrap();
        let cursor_col = u16::try_from(rows.last().map_or(0, |row| row.chars().count())).unwrap();
        TerminalSnapshot {
            revision,
            rows: u16::try_from(lines.len()).unwrap(),
            cols: 120,
            cursor: Some(TerminalCursor {
                row: cursor_row,
                col: cursor_col,
                visible: true,
                style: 0,
            }),
            visible_text: lines.join("\n"),
        }
    }

    fn rendered_row_snapshot(revision: u64, input: &str, cursor_col: u16) -> TerminalSnapshot {
        let lines = ["", "", "", "", "", input, "footer", "status"];
        TerminalSnapshot {
            revision,
            rows: u16::try_from(lines.len()).unwrap(),
            cols: 80,
            cursor: Some(TerminalCursor {
                row: 5,
                col: cursor_col,
                visible: true,
                style: 0,
            }),
            visible_text: lines.join("\n"),
        }
    }

    /// The MEASURED palette entries of Claude Code 2.1.220's command menu: the
    /// selected row is idx153 throughout, and every other row renders its own
    /// characters in idx246. Named here ONLY to rebuild the captures byte for
    /// byte. [`candidate_body_colour`] never names a palette index, which is why
    /// a retuned theme costs a refusal rather than a wrong answer.
    const CAPTURED_SELECTED: u8 = 153;
    const CAPTURED_UNSELECTED: u8 = 246;

    /// How a captured row was coloured.
    #[derive(Clone)]
    enum CapturedStyle {
        /// Exactly the foreground runs the capture printed, as inclusive column
        /// ranges — `[0..68 fg=idx153]` is `(0, 68, 153)`.
        Runs(Vec<(usize, usize, u8)>),
        /// The unselected shape MEASURED at `s12` of the bare-`/` capture:
        /// `[0..7] [30..32] [34..39]`, i.e. each rendered word coloured and the
        /// blanks between them left at the terminal default. Used for the rows
        /// whose runs that capture did not print.
        Words(u8),
        /// Nothing on this row carries an explicit colour.
        Plain,
        /// Runs in rmux's opaque foreground encoding, exactly as the screen
        /// corpus records them: `fg=[45201913]` over columns 2..=117 is
        /// `(2, 117, 45201913)`. Used for the 2.1.257 capture, whose colours
        /// are not palette indices.
        Encoded(Vec<(usize, usize, i32)>),
    }

    #[derive(Clone)]
    struct CapturedRow {
        row: u16,
        text: String,
        style: CapturedStyle,
    }

    fn captured(row: u16, text: impl Into<String>, style: CapturedStyle) -> CapturedRow {
        CapturedRow {
            row,
            text: text.into(),
            style,
        }
    }

    /// MEASURED: the menu is ruled off by U+2500 across the full 80-column pane.
    fn menu_rule_row(row: u16) -> CapturedRow {
        captured(row, "─".repeat(80), CapturedStyle::Plain)
    }

    /// Rebuilds a captured 24x80 screen from the rows a capture recorded.
    ///
    /// Rows that are not listed are blank, as they were. Cells beyond a listed
    /// row's text are blanks at the terminal default, which is what the capture
    /// shows: it right-trims and prints non-default runs only.
    fn captured_screen(
        revision: u64,
        rows: Vec<CapturedRow>,
        cursor_row: u16,
        cursor_col: u16,
    ) -> StyledScreen {
        captured_screen_of(80, revision, rows, cursor_row, cursor_col)
    }

    /// [`captured_screen`] at a stated width: the 2.1.220 captures are 24x80,
    /// the 2.1.257 capture is 24x120.
    fn captured_screen_of(
        cols: u16,
        revision: u64,
        rows: Vec<CapturedRow>,
        cursor_row: u16,
        cursor_col: u16,
    ) -> StyledScreen {
        const ROWS: u16 = 24;
        let mut cells: Vec<Vec<StyledCell>> = (0..ROWS)
            .map(|_| {
                (0..cols)
                    .map(|_| StyledCell::new(" ", CellColor::Unstyled))
                    .collect()
            })
            .collect();
        for row in rows {
            let glyphs: Vec<char> = row.text.chars().collect();
            let runs: Vec<(usize, usize, CellColor)> = match &row.style {
                CapturedStyle::Runs(runs) => runs
                    .iter()
                    .map(|(start, end, index)| (*start, *end, CellColor::indexed(*index)))
                    .collect(),
                CapturedStyle::Encoded(runs) => runs
                    .iter()
                    .map(|(start, end, encoded)| (*start, *end, CellColor::Explicit(*encoded)))
                    .collect(),
                CapturedStyle::Plain => Vec::new(),
                CapturedStyle::Words(index) => {
                    let mut runs = Vec::new();
                    let mut start = None;
                    for (column, glyph) in glyphs.iter().enumerate() {
                        match (glyph.is_whitespace(), start) {
                            (false, None) => start = Some(column),
                            (true, Some(opened)) => {
                                runs.push((opened, column - 1, CellColor::indexed(*index)));
                                start = None;
                            }
                            _ => {}
                        }
                    }
                    if let Some(opened) = start {
                        runs.push((opened, glyphs.len() - 1, CellColor::indexed(*index)));
                    }
                    runs
                }
            };
            for (column, cell) in cells[usize::from(row.row)].iter_mut().enumerate() {
                let glyph = glyphs.get(column).copied().unwrap_or(' ');
                let foreground = runs
                    .iter()
                    .find(|(start, end, _)| (*start..=*end).contains(&column))
                    .map_or(CellColor::Unstyled, |(_, _, colour)| *colour);
                *cell = StyledCell::new(glyph.to_string(), foreground);
            }
        }
        StyledScreen::new(
            revision,
            ROWS,
            cols,
            Some(TerminalCursor {
                row: cursor_row,
                col: cursor_col,
                visible: true,
                style: 0,
            }),
            cells,
        )
    }

    /// VERBATIM CAPTURE, Claude Code 2.1.220, composer holding the full
    /// `/clear`. Five candidates survive the filter and the built-in `/clear` is
    /// the selected one: `s11 [0..68 fg=idx153]`, one run covering the token,
    /// the gap and the description.
    fn measured_clear_menu() -> Vec<CapturedRow> {
        vec![
            captured(
                9,
                "❯ /clear",
                CapturedStyle::Runs(vec![(2, 7, CAPTURED_SELECTED)]),
            ),
            menu_rule_row(10),
            captured(
                11,
                "/clear                        Start a new session with empty context;",
                CapturedStyle::Runs(vec![(0, 68, CAPTURED_SELECTED)]),
            ),
            captured(
                13,
                "/code-review                  Review the current diff for correctness bugs",
                CapturedStyle::Words(CAPTURED_UNSELECTED),
            ),
            captured(
                15,
                "/simplify                     Review the changed code for reuse,",
                CapturedStyle::Words(CAPTURED_UNSELECTED),
            ),
            captured(
                17,
                "/doctor                       Health-check the user's Claude Code setup and",
                CapturedStyle::Words(CAPTURED_UNSELECTED),
            ),
            captured(
                19,
                "/run-skill-generator          Author or improve the run-<unit> skill — a",
                CapturedStyle::Words(CAPTURED_UNSELECTED),
            ),
        ]
    }

    fn measured_clear_screen() -> StyledScreen {
        captured_screen(14, measured_clear_menu(), 9, 8)
    }

    /// MEASURED Claude Code 2.1.238, 24-row pane: composer boxed at the
    /// bottom, candidates ABOVE the upper rule, tokens indented two spaces,
    /// unselected rows a uniform colour (not the 2.1.227 mixed-blank shape).
    fn measured_238_above_composer_menu() -> Vec<CapturedRow> {
        vec![
            captured(
                16,
                "  /clear                        Start a new session with empty context;",
                CapturedStyle::Runs(vec![(2, 79, CAPTURED_SELECTED)]),
            ),
            captured(
                17,
                "                                /resume)",
                CapturedStyle::Plain,
            ),
            captured(
                18,
                "  /code-review                  Review the current diff for correctness bugs",
                CapturedStyle::Runs(vec![(2, 79, CAPTURED_UNSELECTED)]),
            ),
            menu_rule_row(20),
            captured(
                21,
                "❯ /clear",
                CapturedStyle::Runs(vec![(2, 7, CAPTURED_SELECTED)]),
            ),
            menu_rule_row(22),
            captured(
                23,
                "  don't ask on (shift+tab to cycle)",
                CapturedStyle::Plain,
            ),
        ]
    }

    /// The MEASURED foreground encodings of Claude Code 2.1.257's command menu,
    /// as rmux reports them (opaque; compared, never interpreted): the selected
    /// entry and the composer's typed command share one, every other menu row
    /// and the transcript's dim text share another, the rules a third. Named
    /// here ONLY to rebuild the capture cell for cell.
    const CAPTURED_257_SELECTED: i32 = 45_201_913;
    const CAPTURED_257_DIM: i32 = 43_620_761;
    const CAPTURED_257_RULE: i32 = 42_502_280;
    const CAPTURED_257_FOOTER_MODE: i32 = 50_293_632;
    const CAPTURED_257_FOOTER_RC: i32 = 38_713_957;

    /// MEASURED at 2.1.257: the rule spans the full 120-column pane and is
    /// itself coloured.
    fn menu_rule_row_257(row: u16) -> CapturedRow {
        captured(
            row,
            "─".repeat(120),
            CapturedStyle::Encoded(vec![(0, 119, CAPTURED_257_RULE)]),
        )
    }

    /// VERBATIM CAPTURE, Claude Code 2.1.257 linux/x86_64, 24x120, the settled
    /// frame `wait_for_stable_control_render` returned for a pasted `/clear`
    /// (corpus `claude-2.1.257-clear-menu.ndjson`, site
    /// `control_channel.selection`, revision 23). The menu is ABOVE the
    /// composer, two entries survive the filter and each wraps its description
    /// onto a continuation row. The rows below the composer are its idle box
    /// rule and the footer.
    fn measured_257_clear_menu() -> Vec<CapturedRow> {
        vec![
            captured(
                14,
                "✻ Cooked for 1s · done 4:38 PM",
                CapturedStyle::Encoded(vec![(0, 0, CAPTURED_257_DIM), (2, 29, CAPTURED_257_DIM)]),
            ),
            captured(
                16,
                "  /clear                        Start a new session with empty context; \
                 previous session stays on disk (resumable with",
                CapturedStyle::Encoded(vec![(2, 117, CAPTURED_257_SELECTED)]),
            ),
            captured(
                17,
                "                                /resume)",
                CapturedStyle::Encoded(vec![(32, 39, CAPTURED_257_SELECTED)]),
            ),
            captured(
                18,
                "  /code-review                  Review the current diff, or a PR \
                 number/branch/path target, for correctness bugs and",
                CapturedStyle::Encoded(vec![(2, 115, CAPTURED_257_DIM)]),
            ),
            captured(
                19,
                "                                reuse/simplification/efficiency cleanups \
                 at the given effort level (low/medium: fewer…",
                CapturedStyle::Encoded(vec![(32, 117, CAPTURED_257_DIM)]),
            ),
            menu_rule_row_257(20),
            captured(
                21,
                "❯\u{a0}/clear",
                CapturedStyle::Encoded(vec![(2, 7, CAPTURED_257_SELECTED)]),
            ),
            menu_rule_row_257(22),
            captured(
                23,
                "  ⏵⏵ don't ask on (shift+tab to cycle)                                       \
                                                  /rc active",
                CapturedStyle::Encoded(vec![
                    (2, 16, CAPTURED_257_FOOTER_MODE),
                    (17, 37, CAPTURED_257_DIM),
                    (108, 117, CAPTURED_257_FOOTER_RC),
                ]),
            ),
        ]
    }

    fn measured_257_clear_screen() -> StyledScreen {
        captured_screen_of(120, 23, measured_257_clear_menu(), 21, 8)
    }

    /// The 2.1.257 rows with the colour of the `/clear` entry (and its
    /// continuation) and of the `/code-review` entry (and its continuation)
    /// each replaced. Only colouring changes; every glyph is the captured glyph.
    fn measured_257_clear_menu_coloured(clear: i32, code_review: i32) -> StyledScreen {
        let rows = measured_257_clear_menu()
            .into_iter()
            .map(|captured| match captured.row {
                16 => CapturedRow {
                    style: CapturedStyle::Encoded(vec![(2, 117, clear)]),
                    ..captured
                },
                17 => CapturedRow {
                    style: CapturedStyle::Encoded(vec![(32, 39, clear)]),
                    ..captured
                },
                18 => CapturedRow {
                    style: CapturedStyle::Encoded(vec![(2, 115, code_review)]),
                    ..captured
                },
                19 => CapturedRow {
                    style: CapturedStyle::Encoded(vec![(32, 117, code_review)]),
                    ..captured
                },
                _ => captured,
            })
            .collect();
        captured_screen_of(120, 23, rows, 21, 8)
    }

    /// The same captured rows with the highlight moved to the neighbour below.
    /// Only the colouring moves; every glyph is the captured glyph. This is the
    /// screen a corpus change would produce, and the one nothing in this
    /// codebase could previously see.
    fn measured_clear_menu_selecting(row: u16, extent: usize) -> StyledScreen {
        let rows = measured_clear_menu()
            .into_iter()
            .map(|captured| match captured.row {
                11 => CapturedRow {
                    style: CapturedStyle::Words(CAPTURED_UNSELECTED),
                    ..captured
                },
                other if other == row => CapturedRow {
                    style: CapturedStyle::Runs(vec![(0, extent, CAPTURED_SELECTED)]),
                    ..captured
                },
                _ => captured,
            })
            .collect();
        captured_screen(14, rows, 9, 8)
    }

    fn control_for(
        before_paste: impl IntoIterator<Item = TerminalSnapshot>,
        after_paste: impl IntoIterator<Item = TerminalSnapshot>,
        stable_for: Duration,
        gate_timeout: Duration,
    ) -> (RmuxTerminalControl, FakeTerminalHandle) {
        let (terminal, handle) = FakeTerminal::new(before_paste, after_paste);
        (
            RmuxTerminalControl::new(Box::new(terminal)).with_input_gate_timings(
                stable_for,
                Duration::from_millis(1),
                gate_timeout,
            ),
            handle,
        )
    }

    fn test_deadline() -> u64 {
        now_unix_ms().unwrap().checked_add(2_000).unwrap()
    }

    #[test]
    fn millisecond_conversion_rejects_values_above_protocol_safe_max() {
        assert_eq!(
            protocol_milliseconds(u128::from(MAX_SAFE_JSON_INTEGER), "test").unwrap(),
            MAX_SAFE_JSON_INTEGER
        );
        assert!(protocol_milliseconds(u128::from(MAX_SAFE_JSON_INTEGER) + 1, "test").is_err());
        assert_eq!(
            ensure_before_turn_deadline(MAX_SAFE_JSON_INTEGER + 1)
                .unwrap_err()
                .code,
            ErrorCode::InvalidConfig
        );
        assert_eq!(
            diagnostic_u64(MAX_SAFE_JSON_INTEGER + 1),
            json!((MAX_SAFE_JSON_INTEGER + 1).to_string())
        );
        assert!(ensure_protocol_transcript_offset(MAX_SAFE_JSON_INTEGER).is_ok());
        let offset_error =
            ensure_protocol_transcript_offset(MAX_SAFE_JSON_INTEGER + 1).unwrap_err();
        assert_eq!(
            offset_error.details["offset"],
            json!((MAX_SAFE_JSON_INTEGER + 1).to_string())
        );
    }

    async fn submit(control: &RmuxTerminalControl, prompt: &str) -> DriverResult<()> {
        control
            .submit_prompt(
                SessionId::new_v4(),
                TurnId::new_v4(),
                prompt,
                test_deadline(),
            )
            .await
    }

    #[test]
    fn prompt_validation_normalizes_and_blocks_driver_controls() {
        assert_eq!(validate_prompt("one\r\ntwo").unwrap(), "one\ntwo");
        assert_eq!(
            validate_prompt("/compact").unwrap_err().code,
            ErrorCode::UnsupportedFeature
        );
        assert_eq!(
            validate_prompt(" \t/compact").unwrap_err().code,
            ErrorCode::UnsupportedFeature
        );
        assert_eq!(
            validate_prompt("bad\u{1b}").unwrap_err().code,
            ErrorCode::InvalidConfig
        );
        assert_eq!(
            validate_prompt(&"x".repeat(MAX_PROMPT_BYTES + 1))
                .unwrap_err()
                .code,
            ErrorCode::InvalidConfig
        );
    }

    /// A composer refusal reaches a caller that can only read structured
    /// fields, which is every caller on the MCP surface.
    ///
    /// The remedy is in `details.recommendation` and not only inside `message`,
    /// because `bin/pmux-mcp` redacts a daemon message: a message can be
    /// composed out of caller bytes (`invalid type: integer 42`) and this field
    /// never is. Before this, `unsupported_feature` on that surface was the
    /// same payload for a `/`-prefixed prompt as for a daemon with no pool.
    ///
    /// Derived from `ComposerRefusal` rather than asserted as a literal, so a
    /// reworded remedy cannot leave a stale copy here, and every refusal the
    /// composer rule can return is covered rather than the one that was easy
    /// to build.
    #[test]
    fn every_composer_refusal_publishes_its_remedy_where_a_redacting_surface_reads() {
        let refused = [
            (
                "/compact",
                pseudomux_claude::ComposerRefusal::ModePrefix('/'),
            ),
            (
                "!rm -rf /",
                pseudomux_claude::ComposerRefusal::ModePrefix('!'),
            ),
            (
                "a\tb",
                pseudomux_claude::ComposerRefusal::RewrittenCharacter('\t'),
            ),
            (
                "ask me\\",
                pseudomux_claude::ComposerRefusal::LineContinuation,
            ),
        ];
        for (prompt, refusal) in refused {
            let error = validate_prompt(prompt).unwrap_err();
            assert_eq!(
                error
                    .details
                    .get(RECOMMENDATION_KEY)
                    .and_then(serde_json::Value::as_str),
                Some(refusal.remedy()),
                "{prompt:?} refuses a caller without publishing what to do next"
            );
            assert_eq!(
                error
                    .details
                    .get("violation")
                    .and_then(serde_json::Value::as_str),
                Some(refusal.code()),
                "{prompt:?} must still say which rule refused it"
            );
            // The two halves still read as the one sentence the CLI has always
            // printed, in the order it prints them.
            assert_eq!(
                format!("{} {}", error.message, refusal.remedy()),
                refusal.describe(),
                "message and advice must rejoin into `describe`"
            );
        }
    }

    #[test]
    fn prompt_submission_deadline_check_fails_closed() {
        assert_eq!(
            ensure_before_turn_deadline(0).unwrap_err().code,
            ErrorCode::TurnTimeout
        );
    }

    #[test]
    fn prompt_and_modal_classification_are_conservative() {
        assert!(has_ready_prompt("Claude Code\n  ❯  \n"));
        assert!(has_ready_prompt(
            "Claude Code\n  ❯ Try a short task\nfooter\nstatus"
        ));
        assert!(!has_ready_prompt("❯ old prompt\nworking\nfooter\nstatus"));
        assert!(!has_ready_prompt("Claude Code\n❯not an input glyph"));
        assert!(!has_ready_prompt("answer contains ❯ inline"));
        for (screen, expected) in [
            (
                "Do you trust the files in this folder?\n❯",
                NeedsInputKind::Trust,
            ),
            ("Log in to Claude to continue", NeedsInputKind::Login),
            (
                "Permission required\nAllow this command or deny it?",
                NeedsInputKind::Permission,
            ),
            ("Update required for Claude Code", NeedsInputKind::Update),
            (
                "Five hour usage limit exceeded; try again later",
                NeedsInputKind::Quota,
            ),
            (
                "Choose an option\nEnter to confirm · Esc to cancel",
                NeedsInputKind::UnknownModal,
            ),
        ] {
            let TerminalScreenState::NeedsInput(needs_input) = classify_terminal_screen(screen)
            else {
                panic!("expected a modal classification for {expected:?}");
            };
            assert_eq!(needs_input.kind, expected);
            assert_eq!(needs_input.details, Value::Null);
            assert!(
                !needs_input.message.contains(screen),
                "screen contents must not escape the classifier"
            );
        }
        assert_eq!(
            classify_terminal_screen("The documentation says to log in elsewhere").label(),
            "unrecognised"
        );
    }

    #[test]
    fn structured_classifier_correlates_the_active_cursor_not_prompt_history() {
        let active = structured_snapshot(
            7,
            [
                "",
                "",
                "❯ historical prompt",
                "",
                "answer",
                "",
                "",
                "",
                "❯ ",
                "footer",
                "footer",
                "status",
            ],
            8,
            2,
            true,
        );
        assert_eq!(
            classify_terminal_snapshot(&active),
            TerminalScreenState::Ready
        );

        let distant_history = structured_snapshot(
            8,
            [
                "",
                "",
                "❯ historical prompt",
                "",
                "answer",
                "",
                "",
                "",
                "unrelated cursor row",
                "footer",
                "footer",
                "status",
            ],
            8,
            2,
            true,
        );
        // NOT `ready`, which is what this test is for. But also not
        // `unrecognised`: `active_editor` searches UPWARD from the cursor with
        // no bound on the distance, so it resolves the row-2 `❯` as this
        // cursor's anchor and reports a six-row composer holding text.
        //
        // That is production's behaviour before and after the `Unknown` split
        // -- both spellings mean "proceed" -- and it is recorded here rather
        // than asserted the way it reads: pmux CLAIMS to recognize this screen,
        // and the claim rests on an anchor six rows away. Narrowing the upward
        // search is a change to the input gate's own geometry and needs its own
        // measurement, so it is named in `docs/path-b-adversarial.md` §13 and
        // not done here.
        assert_eq!(
            classify_terminal_snapshot(&distant_history).label(),
            "composer_holding_text"
        );

        let invisible = TerminalSnapshot {
            cursor: active.cursor.map(|mut cursor| {
                cursor.visible = false;
                cursor
            }),
            ..active.clone()
        };
        assert_eq!(
            classify_terminal_snapshot(&invisible).label(),
            "unrecognised"
        );
        assert_eq!(
            classify_terminal_snapshot(&TerminalSnapshot {
                revision: 0,
                ..active
            })
            .label(),
            "no_frame_yet"
        );
    }

    #[test]
    fn populated_editor_keywords_are_not_misclassified_as_a_modal() {
        let populated = rendered_snapshot(
            2,
            "❯ permission required allow or deny trust this folder",
            52,
        );
        assert_eq!(
            classify_terminal_snapshot(&populated).label(),
            "composer_holding_text"
        );

        let modal = structured_snapshot(
            3,
            [
                "",
                "",
                "Permission required",
                "Allow this command or deny it?",
                "",
                "choose",
                "footer",
                "status",
            ],
            5,
            2,
            true,
        );
        assert!(matches!(
            classify_terminal_snapshot(&modal),
            TerminalScreenState::NeedsInput(NeedsInput {
                kind: NeedsInputKind::Permission,
                ..
            })
        ));
    }

    #[test]
    fn cursorless_ready_fallback_is_test_only_and_stale_revisions_are_rejected() {
        let legacy = TerminalSnapshot {
            revision: 1,
            rows: 0,
            cols: 0,
            cursor: None,
            visible_text: "Claude Code\n❯ Try something\nfooter\nstatus".to_owned(),
        };
        assert_eq!(
            classify_terminal_snapshot(&legacy),
            TerminalScreenState::Ready
        );
        assert_eq!(
            classify_terminal_snapshot(&TerminalSnapshot {
                revision: 0,
                ..legacy
            })
            .label(),
            "no_frame_yet"
        );
    }

    #[tokio::test]
    async fn input_gate_pastes_once_and_enters_once_after_stable_editor_render() {
        let baseline = baseline_snapshot(1);
        let rendered = rendered_snapshot(2, "❯ hello", 7);
        let (control, handle) = control_for(
            [baseline.clone(), baseline],
            [rendered.clone(), rendered],
            Duration::ZERO,
            Duration::from_millis(30),
        );
        submit(&control, "hello").await.unwrap();
        assert_eq!(handle.counts(), (1, 1));
    }

    /// The composer is EMPTY and the screen still reads as this prompt's head,
    /// because Claude renders placeholder text into an empty composer.
    ///
    /// MEASURED at 2.1.226 on this host, `input_gate.post_paste`, the frame
    /// taken before the paste landed: `❯` U+00A0 `Try "refactor <filepath>"`
    /// with the cursor at (18, 2) — the empty position. The row rotates
    /// (`Try "write a test for <filepath>"` on the next run), so a caller who
    /// sends one of them as a prompt is not doing anything exotic.
    ///
    /// `!empty_cursor_position` is the ONLY clause that refuses this. Every
    /// other one holds: the revision moved, the prompt column is identical, the
    /// anchor and cursor rows are the fence's own, and the head is the prompt
    /// byte for byte. Disable it and pmux presses Enter on a composer holding
    /// nothing, which submits nothing and then waits out the caller's deadline
    /// proving it — 600 000 ms under daemon policy, and the instance with it.
    ///
    /// This is one of the three clauses a mutation run could disable at
    /// `8c3d387` while `pseudomux-service --lib` stayed at 415 passed.
    #[tokio::test]
    async fn an_empty_composer_rendering_its_placeholder_is_never_entered() {
        // The fence: an empty composer with a placeholder of its own.
        let baseline = rendered_row_snapshot(1, "❯\u{a0}Try \"write a test for <filepath>\"", 2);
        let baseline_editor = active_editor(&baseline).unwrap();
        assert!(
            baseline_editor.empty_cursor_position,
            "the fence must be the empty editor this clause is about"
        );

        // The caller's prompt IS the next placeholder, and the composer is
        // still empty: the rows changed, the cursor did not.
        let prompt = "Try \"refactor <filepath>\"";
        let rendered = rendered_row_snapshot(2, "❯\u{a0}Try \"refactor <filepath>\"", 2);
        let editor = active_editor(&rendered).unwrap();
        assert_eq!(composer_head(&editor), Some(prompt));
        assert_eq!(editor.prompt_col, baseline_editor.prompt_col);
        assert_eq!(editor.cursor_row, baseline_editor.cursor_row);
        assert_eq!(editor.anchor_row, baseline_editor.anchor_row);
        assert_ne!(
            editor.signature.rendered_rows,
            baseline_editor.signature.rendered_rows
        );

        assert!(!rendered_prompt_is_proven(
            &rendered,
            baseline.revision,
            &baseline_editor,
            prompt
        ));

        let (control, handle) = control_for(
            [baseline.clone(), baseline],
            [rendered.clone(), rendered],
            Duration::ZERO,
            Duration::from_millis(30),
        );
        assert_eq!(
            submit(&control, prompt).await.unwrap_err().code,
            ErrorCode::PromptNotAcknowledged
        );
        assert_eq!(
            handle.counts(),
            (1, 0),
            "Enter must not be pressed on an empty composer"
        );
    }

    /// A second `❯` on the screen is a second editor, and the caller can put it
    /// there.
    ///
    /// `docs/path-b-adversarial.md` sec. 8: a prompt whose second line begins
    /// with `❯` renders as `  ❯ …`, `prompt_glyph_col` accepts the two-space
    /// indent as leading whitespace, and `active_editor` correlates the cursor
    /// to the CALLER'S OWN ROW rather than to the composer the fence proved.
    /// Live at 2.1.226 that cost a pooled instance.
    ///
    /// `same_editor_geometry` is what refuses it, by the prompt column: the
    /// fenced composer's `❯` is at column 0 and the caller's is at column 2.
    /// The head clause cannot see this at all — the row really does hold this
    /// prompt's own text, which is exactly why the caller was able to put it
    /// there. Second of the three clauses that survived mutation at `8c3d387`.
    #[tokio::test]
    async fn a_second_editor_holding_the_prompts_own_text_is_never_entered() {
        let baseline = baseline_snapshot(1);
        let baseline_editor = active_editor(&baseline).unwrap();
        let prompt = "What is 2 plus 2?";
        let rendered = structured_snapshot(
            2,
            ["", "", "", "", "", "❯", "  ❯ What is 2 plus 2?", "footer"],
            6,
            21,
            true,
        );
        let editor = active_editor(&rendered).unwrap();
        assert_eq!(
            composer_head(&editor),
            Some(prompt),
            "the caller's own row must be the one that resolved, or this test is about nothing"
        );
        assert_ne!(editor.prompt_col, baseline_editor.prompt_col);

        assert!(!rendered_prompt_is_proven(
            &rendered,
            baseline.revision,
            &baseline_editor,
            prompt
        ));

        // The prompt column carries this on its own, and the case above does
        // not prove that: there the caller's row is BELOW the fence's, so the
        // row relation refuses it too. Here the caller's indented `❯` has
        // landed on the fence's own row — a bottom-anchored frame whose box
        // was pushed up by exactly one row — so the anchor is pinned, the
        // cursor has not moved, and the column is the only clause left.
        let same_row = structured_snapshot(
            2,
            [
                "",
                "",
                "",
                "",
                "",
                "  ❯ What is 2 plus 2?",
                "footer",
                "status",
            ],
            5,
            23,
            true,
        );
        let same_row_editor = active_editor(&same_row).unwrap();
        assert_eq!(composer_head(&same_row_editor), Some(prompt));
        assert_ne!(same_row_editor.prompt_col, baseline_editor.prompt_col);
        assert_eq!(same_row_editor.cursor_row, baseline_editor.cursor_row);
        assert!(same_row_editor.anchor_row <= baseline_editor.anchor_row);
        assert!(!rendered_prompt_is_proven(
            &same_row,
            baseline.revision,
            &baseline_editor,
            prompt
        ));

        // ...and the other half of the clause: one composer grows in one
        // direction. A frame whose anchor moved DOWN while its cursor moved UP
        // is not the fenced composer having grown, whatever it is holding.
        let displaced = structured_snapshot(
            2,
            ["", "", "", "", "", "", "❯ What is 2 plus 2?", "footer"],
            6,
            19,
            true,
        );
        let displaced_editor = active_editor(&displaced).unwrap();
        assert_eq!(displaced_editor.prompt_col, baseline_editor.prompt_col);
        assert!(displaced_editor.anchor_row > baseline_editor.anchor_row);
        assert!(displaced_editor.cursor_row > baseline_editor.cursor_row);
        assert!(!rendered_prompt_is_proven(
            &displaced,
            baseline.revision,
            &baseline_editor,
            prompt
        ));

        let (control, handle) = control_for(
            [baseline.clone(), baseline],
            [rendered.clone(), rendered],
            Duration::ZERO,
            Duration::from_millis(30),
        );
        assert_eq!(
            submit(&control, prompt).await.unwrap_err().code,
            ErrorCode::PromptNotAcknowledged
        );
        assert_eq!(handle.counts(), (1, 0), "Enter went into a second editor");
    }

    /// Why the third surviving clause was DELETED instead of given a test.
    ///
    /// `cursor_moved || rendered_rows_changed` is `editor.signature !=
    /// baseline.signature` spelled out over the signature's three fields, and
    /// it can never be the clause that refuses, because
    /// `empty_cursor_position` is a function of two of those three fields and
    /// the baseline is the fence, which is empty by construction.
    ///
    /// Both halves of that are asserted here rather than argued, and both are
    /// derived from `active_editor`'s own output: equal signatures give equal
    /// `empty_cursor_position` even when every ABSOLUTE position differs, and
    /// an editor that is not at its empty position never carries an empty
    /// editor's signature. A fourth signature field, or an
    /// `empty_cursor_position` that started reading one, reddens this and says
    /// the deleted clause is needed again.
    #[test]
    fn the_deleted_change_clause_could_never_have_been_the_one_that_refused() {
        // Same signature, different absolute rows: `empty_cursor_position` is
        // blind to everything but the signature.
        let low = structured_snapshot(1, ["", "", "❯ Try something", "footer", ""], 2, 2, true);
        let high = structured_snapshot(
            1,
            ["", "", "", "", "", "❯ Try something", "footer", ""],
            5,
            2,
            true,
        );
        let (low, high) = (
            active_editor(&low).expect("editor"),
            active_editor(&high).expect("editor"),
        );
        assert_eq!(low.signature, high.signature);
        assert_ne!(low.anchor_row, high.anchor_row);
        assert_eq!(
            low.empty_cursor_position, high.empty_cursor_position,
            "empty_cursor_position must be a function of the signature alone"
        );

        // So a populated editor cannot wear the fence's signature, which is
        // the whole of what the deleted clause tested.
        let fence = active_editor(&baseline_snapshot(1)).expect("editor");
        assert!(fence.empty_cursor_position);
        for row in ["❯ h", "❯ hello", "❯\u{a0}[Pasted text #1]"] {
            let populated = active_editor(&rendered_row_snapshot(
                2,
                row,
                u16::try_from(row.chars().count()).unwrap(),
            ))
            .expect("editor");
            assert!(!populated.empty_cursor_position, "row {row:?}");
            assert_ne!(
                populated.signature, fence.signature,
                "a populated editor carried the fence's signature: {row:?}"
            );
        }
    }

    /// The fence invariant the deletion above rests on is CHECKED, not assumed.
    ///
    /// Production reaches this function only through
    /// [`prove_stable_empty_editor`], which filters on `empty_cursor_position`,
    /// so a non-empty baseline is unreachable today. The clause is here anyway
    /// because "unreachable today" is what the deleted clause was relying on
    /// without saying so: with a populated baseline and an unchanged screen,
    /// every remaining clause holds and the gate would prove a render that
    /// never happened.
    #[test]
    fn a_baseline_that_is_not_the_fenced_empty_editor_proves_nothing() {
        let prompt = "hello";
        let populated = rendered_row_snapshot(1, "❯ hello", 7);
        let baseline_editor = active_editor(&populated).expect("editor");
        assert!(!baseline_editor.empty_cursor_position);
        // A new frame carrying the same populated composer: the revision moved
        // and nothing else did.
        let again = TerminalSnapshot {
            revision: 2,
            ..populated
        };
        assert_eq!(
            active_editor(&again).expect("editor").signature,
            baseline_editor.signature
        );
        assert!(!rendered_prompt_is_proven(
            &again,
            1,
            &baseline_editor,
            prompt
        ));
    }

    /// The reproduction of `docs/path-b-adversarial.md` sec. 4.3(b), kept as the
    /// regression for it.
    ///
    /// EVERY geometric clause holds: the prompt column is identical, the anchor
    /// is pinned, the cursor walked right, the revision moved, the editor is not
    /// at its empty position, and the rendered rows changed. The composer is
    /// holding a shell command the caller's prompt does not contain. Before the
    /// head proof this pasted once and PRESSED ENTER, and the two assertions
    /// below read `assert!(rendered_prompt_is_proven(..))` and `(1, 1)`.
    ///
    /// The bash-mode prompt itself is refused earlier now, by
    /// `pseudomux_claude::composer_refusal`. This test is the OTHER half: the
    /// prompt is ordinary and legal, and it is the SCREEN that disagrees with
    /// it. Nothing in the character guard can see that.
    #[tokio::test]
    async fn a_composer_holding_text_this_prompt_never_began_with_is_never_entered() {
        let baseline = baseline_snapshot(1);
        let rendered = rendered_snapshot(2, "❯ ! echo PWNED > /tmp/pmux-render-proof", 40);
        let baseline_editor = active_editor(&baseline).unwrap();
        let prompt = "What is 2 plus 2?";
        assert!(!rendered_prompt_is_proven(
            &rendered,
            baseline.revision,
            &baseline_editor,
            prompt
        ));
        // …and the geometry it satisfies is not weakened, only outvoted: the
        // same frame against its own text still passes every other clause.
        assert!(rendered_prompt_is_proven(
            &rendered,
            baseline.revision,
            &baseline_editor,
            "! echo PWNED > /tmp/pmux-render-proof"
        ));

        let (control, handle) = control_for(
            [baseline.clone(), baseline],
            [rendered.clone(), rendered],
            Duration::ZERO,
            Duration::from_millis(30),
        );
        let error = submit(&control, prompt).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::PromptNotAcknowledged);
        assert!(
            error.message.contains("rendered rows"),
            "the refusal must name what it tested: {}",
            error.message
        );
        assert_eq!(
            handle.counts(),
            (1, 0),
            "the paste happened and Enter must not have"
        );
    }

    /// Every composer render MEASURED at Claude Code 2.1.226 through the input
    /// gate's own corpus recorder, from the SCREEN ROWS rather than from the
    /// text after the glyph.
    ///
    /// macOS 15.7.7 / aarch64, 24x120 pane, `PMUX_SCREEN_CORPUS_DIR` set on a
    /// release `pmuxd` running a Path B pool of one, frames taken at site
    /// `input_gate.post_paste`. The rows are verbatim, U+00A0 separator and
    /// two-cell continuation gutters included, so this test exercises
    /// [`composer_rows`]'s stripping as well as the proof underneath it;
    /// `crates/claude/src/composer.rs` carries the same renders with the
    /// stripping already done.
    ///
    /// The variant is asserted and not just the acceptance. A proof that
    /// quietly started answering `CollapsedPaste` for everything would still
    /// admit all of these, and this is the assertion that would notice.
    #[test]
    fn the_measured_2_1_226_composer_renders_prove_their_own_prompts() {
        use pseudomux_claude::{ComposerRenderProof, composer_render_proof};

        let long_wrapping = "Answer with only the number: what is the sum of 2 and 2, given that \
                             this sentence is deliberately made long enough that it must wrap onto \
                             more than one rendered composer row on any ordinary terminal pane \
                             width, so that the wrapping behaviour of the composer can be recorded?";
        let three_lines = "Reply with only the word THREE.\n    this third line begins with four spaces\nlast line";
        let twenty_lines = format!("Reply with only the word OK.{}", "\nfiller".repeat(19));
        let three_thousand = format!(
            "Reply with only the word LONG. {}",
            "padding word ".repeat(230)
        );
        let measured: [(&[&str], &str, ComposerRenderProof); 6] = [
            (
                &["\u{276f}\u{a0}Reply with the single word FOUR and nothing else."],
                "Reply with the single word FOUR and nothing else.",
                ComposerRenderProof::PromptText,
            ),
            (
                &[
                    "\u{276f}\u{a0}\u{65e5}\u{672c}\u{8a9e}\u{3068}\u{7d75}\u{6587}\u{5b57}\u{1f642}\u{3092}\u{542b}\u{3080}\u{30d7}\u{30ed}\u{30f3}\u{30d7}\u{30c8}\u{3067}\u{3059}\u{3002}Reply with only the word WIDE.",
                ],
                "\u{65e5}\u{672c}\u{8a9e}\u{3068}\u{7d75}\u{6587}\u{5b57}\u{1f642}\u{3092}\u{542b}\u{3080}\u{30d7}\u{30ed}\u{30f3}\u{30d7}\u{30c8}\u{3067}\u{3059}\u{3002}Reply with only the word WIDE.",
                ComposerRenderProof::PromptText,
            ),
            // Three prompt lines, three rows, the middle one carrying its own
            // four leading spaces after the two-cell gutter.
            (
                &[
                    "\u{276f}\u{a0}Reply with only the word THREE.",
                    "      this third line begins with four spaces",
                    "  last line",
                ],
                three_lines,
                ComposerRenderProof::PromptText,
            ),
            (
                &["\u{276f}\u{a0}[Pasted text #6 +3 lines]"],
                "Reply with only the word OK.\nfiller 2\nfiller 3\nfiller 4",
                ComposerRenderProof::CollapsedPaste,
            ),
            (
                &["\u{276f}\u{a0}[Pasted text #5 +19 lines]"],
                &twenty_lines,
                ComposerRenderProof::CollapsedPaste,
            ),
            (
                &["\u{276f}\u{a0}[Pasted text #7]"],
                &three_thousand,
                ComposerRenderProof::CollapsedPaste,
            ),
        ];

        for (rows, prompt, expected) in measured {
            let snapshot = composer_snapshot(2, rows);
            let editor = active_editor(&snapshot).unwrap();
            let stripped =
                composer_rows(&editor).unwrap_or_else(|| panic!("no composer rows in {rows:?}"));
            assert_eq!(
                composer_render_proof(&stripped, prompt),
                Some(expected),
                "measured render {rows:?}"
            );
        }

        // And the wrapping render, whose FIRST row is a strict prefix of a
        // prompt more than twice its length. Its rows are byte-identical to the
        // recorded frame, including the two-cell gutter and the space each
        // break consumed.
        let wrapped = [
            format!("\u{276f}\u{a0}{}", &long_wrapping[..114]),
            format!("  {}", &long_wrapping[115..231]),
            format!("  {}", &long_wrapping[232..]),
        ];
        let rows: Vec<&str> = wrapped.iter().map(String::as_str).collect();
        let snapshot = composer_snapshot(2, &rows);
        let editor = active_editor(&snapshot).unwrap();
        let stripped = composer_rows(&editor).unwrap();
        assert_eq!(stripped[0], &long_wrapping[..114]);
        assert!(stripped[0].len() < long_wrapping.len() / 2);
        assert_eq!(
            composer_render_proof(&stripped, long_wrapping),
            Some(ComposerRenderProof::PromptText)
        );
        // The first row alone is the defect this proof was rewritten to close.
        assert_eq!(
            composer_render_proof(&stripped[..1], long_wrapping),
            None,
            "the first row of a wrapping render must not prove the prompt"
        );
    }

    /// The continuation gutter is removed EXACTLY, not trimmed away.
    ///
    /// Trimming the leading whitespace off a continuation row instead would be
    /// invisible in almost every case, because a row boundary is already
    /// allowed to have eaten whitespace — so the difference only shows on a row
    /// carrying MORE indent than the gutter, which is a composer rendering
    /// whitespace this prompt does not have. The gutter width is derived from
    /// where [`composer_head`] decided the first row's text began, so the two
    /// removals cannot come to disagree about the same composer.
    #[test]
    fn a_continuation_row_gives_up_its_gutter_and_keeps_its_own_indent() {
        // The prompt's own four spaces survive the two-cell gutter, MEASURED at
        // 2.1.226.
        let editor = active_editor(&composer_snapshot(
            2,
            &["❯\u{a0}first", "      indented four"],
        ))
        .unwrap();
        assert_eq!(
            composer_rows(&editor),
            Some(vec!["first", "    indented four"])
        );

        // Two more spaces than the gutter is two more spaces than the prompt.
        let editor = active_editor(&composer_snapshot(2, &["❯\u{a0}first", "    second"])).unwrap();
        assert_eq!(composer_rows(&editor), Some(vec!["first", "  second"]));
        assert!(!rendered_prompt_is_proven(
            &composer_snapshot(2, &["❯\u{a0}first", "    second"]),
            1,
            &active_editor(&baseline_snapshot(1)).unwrap(),
            "first\nsecond"
        ));

        // A row whose gutter is not blank is not a continuation of anything.
        // A row carrying its own `❯` is deliberately not in this list: that one
        // resolves as a SECOND EDITOR, which `same_editor_geometry` refuses by
        // the prompt column rather than this function refusing by the gutter.
        for row in ["────────", "footer text"] {
            let editor = active_editor(&composer_snapshot(2, &["❯\u{a0}first", row])).unwrap();
            assert_eq!(composer_rows(&editor), None, "row {row:?}");
        }
    }

    /// The head is taken by [`prompt_glyph_col`]'s own rule, so every separator
    /// that function admits reaches the same text.
    #[test]
    fn the_composer_head_steps_over_the_glyph_and_at_most_one_separator() {
        for (row, expected) in [
            // MEASURED at 2.1.226: the separator is U+00A0.
            ("❯\u{a0}hello", "hello"),
            // MEASURED at 2.1.70: an empty composer carries no separator at all.
            ("❯", ""),
            ("❯ hello", "hello"),
            // Exactly one cell is stepped over, so a prompt that begins with a
            // space still has it compared.
            ("❯\u{a0} hello", " hello"),
            // A right-trimmed blank composer.
            ("❯\u{a0}", ""),
        ] {
            let snapshot = rendered_row_snapshot(2, row, 30);
            let editor = active_editor(&snapshot).unwrap();
            assert_eq!(composer_head(&editor), Some(expected), "row {row:?}");
        }
    }

    #[tokio::test]
    async fn input_gate_accepts_delayed_multiline_collapsed_and_cursor_only_renders() {
        let baseline = baseline_snapshot(1);
        let delayed_empty = TerminalSnapshot {
            revision: 2,
            visible_text: baseline.visible_text.replace("footer", "footer changed"),
            ..baseline.clone()
        };
        // The continuation gutter is two cells, MEASURED at 2.1.226: a
        // three-line prompt renders `❯` U+00A0 then its first line, and every
        // line under it indented by exactly two.
        let multiline = structured_snapshot(
            3,
            [
                "",
                "",
                "",
                "❯ first line",
                "  continued input",
                "  last line",
                "footer",
                "status",
            ],
            5,
            11,
            true,
        );
        let (control, handle) = control_for(
            [baseline.clone()],
            [delayed_empty, multiline.clone()],
            Duration::from_millis(2),
            Duration::from_millis(50),
        );
        submit(&control, "first line\ncontinued input\nlast line")
            .await
            .unwrap();
        assert_eq!(handle.counts(), (1, 1));

        // `"line\n".repeat(20)` is 20 line breaks as the caller wrote it and 19
        // by the time it reaches a composer: `normalize_prompt` applies the
        // composer's own trailing trim, so the last one is gone before the
        // paste. The placeholder is derived from the prompt the gate is
        // holding, which is why this number moved when that rule landed.
        let collapsed = rendered_snapshot(4, "❯ [Pasted text #1 +19 lines]", 30);
        let (control, handle) = control_for(
            [baseline.clone(), baseline.clone()],
            [collapsed.clone(), collapsed],
            Duration::ZERO,
            Duration::from_millis(30),
        );
        submit(&control, &"line\n".repeat(20)).await.unwrap();
        assert_eq!(handle.counts(), (1, 1));

        // A blank first ROW proves a blank first LINE, and only in the company
        // of the rows that carry the rest. The prompt that used to stand here
        // was `"   "`, which no longer reaches a composer at all: the composer
        // trims it to nothing and Enter never submits an empty buffer, so
        // `validate_prompt` refuses it as empty. A prompt whose first line is
        // genuinely empty still renders this way and still has to be accepted.
        let empty_shape = rendered_snapshot(1, "❯\u{a0}", 2);
        let blank_line = composer_snapshot(5, &["❯\u{a0}", "  What is 1 plus 1?"]);
        let (control, handle) = control_for(
            [empty_shape.clone(), empty_shape.clone()],
            [blank_line.clone(), blank_line],
            Duration::ZERO,
            Duration::from_millis(30),
        );
        submit(&control, "\nWhat is 1 plus 1?").await.unwrap();
        assert_eq!(handle.counts(), (1, 1));

        // ...and the blank row ALONE proves nothing, which is the hole that
        // rule used to have: an empty composer proved every prompt whose first
        // line was blank, however many lines followed it, because nothing below
        // the first row was compared.
        let blank_only = composer_snapshot(5, &["❯\u{a0}"]);
        let (control, handle) = control_for(
            [empty_shape.clone(), empty_shape],
            [blank_only.clone(), blank_only],
            Duration::ZERO,
            Duration::from_millis(30),
        );
        assert_eq!(
            submit(&control, "\nWhat is 1 plus 1?")
                .await
                .unwrap_err()
                .code,
            ErrorCode::PromptNotAcknowledged
        );
        assert_eq!(handle.counts(), (1, 0));
    }

    /// A prompt of nothing but whitespace never reaches a terminal.
    ///
    /// MEASURED at 2.1.226 three ways -- `"   "`, `"\u{a0}"` and `"\n"` -- each
    /// of which pmux typed, waited on, and then destroyed the instance for,
    /// because Enter does not submit a buffer that is empty once the composer's
    /// own trailing trim has run (`docs/path-b-adversarial.md` sec. 11). The
    /// refusal is derived rather than written: `normalize_prompt` states the
    /// trim, and these prompts arrive at the emptiness test as the empty string.
    #[tokio::test]
    async fn a_prompt_of_only_whitespace_is_refused_before_any_terminal_is_touched() {
        for prompt in ["   ", "\u{a0}", "\n", "\r\n", "\u{feff}", "\u{3000}\n  "] {
            let baseline = baseline_snapshot(1);
            let (control, handle) = control_for(
                [baseline.clone()],
                [baseline],
                Duration::ZERO,
                Duration::from_millis(30),
            );
            let failure = submit(&control, prompt)
                .await
                .expect_err("a whitespace-only prompt must be refused");
            assert_eq!(failure.code, ErrorCode::InvalidConfig, "{prompt:?}");
            assert!(
                failure.message.contains("must not be empty"),
                "{prompt:?}: {}",
                failure.message
            );
            assert_eq!(
                handle.counts(),
                (0, 0),
                "{prompt:?} must not be pasted or entered"
            );
        }
    }

    /// A prompt the composer would be left holding a `\` at the end of is
    /// refused, and nothing is typed.
    ///
    /// MEASURED at 2.1.226: both `"… \"` and `"… \\"` ran to the caller's
    /// deadline having written no `user` row, because Enter deletes the
    /// backslash and inserts a newline instead of submitting.
    #[tokio::test]
    async fn a_trailing_line_continuation_is_refused_before_any_terminal_is_touched() {
        for prompt in [
            "What is 1 plus 1? \\",
            "What is 1 plus 1? \\\\",
            "\\",
            "a\\  ",
        ] {
            let baseline = baseline_snapshot(1);
            let (control, handle) = control_for(
                [baseline.clone()],
                [baseline],
                Duration::ZERO,
                Duration::from_millis(30),
            );
            let failure = submit(&control, prompt)
                .await
                .expect_err("a trailing backslash must be refused");
            assert_eq!(failure.code, ErrorCode::InvalidConfig, "{prompt:?}");
            assert!(
                failure.message.contains("line continuation"),
                "{prompt:?}: {}",
                failure.message
            );
            assert_eq!(
                handle.counts(),
                (0, 0),
                "{prompt:?} must not be pasted or entered"
            );
        }
    }

    /// A composer 20 rows deep is still ONE editor, correlated to the anchor
    /// the fence proved, and every one of those rows is compared.
    ///
    /// The wrapped screen is BUILT FROM THE PROMPT rather than filled with
    /// plausible-looking text: each row carries three of the prompt's segments
    /// and the break between rows eats the space, which is what the composer
    /// MEASURABLY does. Before the render proof this screen read
    /// `❯ first wrapped row` over twenty rows of `continued input`, none of
    /// which the prompt contains -- free to be arbitrary because nothing
    /// compared it to anything.
    #[tokio::test]
    async fn input_gate_correlates_a_deep_wrapped_editor_to_the_pre_paste_anchor() {
        let mut baseline_lines = vec![String::new(); 24];
        baseline_lines[21] = "❯ Try something".to_owned();
        baseline_lines[22] = "footer".to_owned();
        baseline_lines[23] = "status".to_owned();
        let baseline = TerminalSnapshot {
            revision: 1,
            rows: 24,
            cols: 80,
            cursor: Some(TerminalCursor {
                row: 21,
                col: 2,
                visible: true,
                style: 0,
            }),
            visible_text: baseline_lines.join("\n"),
        };

        const SEGMENT: &str = "long prompt segment";
        // Anchored at row 1 and reaching the cursor row the fence left at 21,
        // which is the bottom-anchored growth `same_editor_geometry` admits.
        const ROWS: usize = 21;
        const PER_ROW: usize = 3;
        let prompt = vec![SEGMENT; ROWS * PER_ROW].join(" ");
        let mut wrapped_lines = vec![String::new(); 24];
        for row in 0..ROWS {
            let text = [SEGMENT; PER_ROW].join(" ");
            wrapped_lines[1 + row] = if row == 0 {
                format!("❯ {text}")
            } else {
                format!("  {text}")
            };
        }
        wrapped_lines[22] = "footer".to_owned();
        wrapped_lines[23] = "status".to_owned();
        let cursor_col = u16::try_from(2 + PER_ROW * SEGMENT.len() + PER_ROW - 1).unwrap();
        let wrapped = TerminalSnapshot {
            revision: 2,
            rows: 24,
            cols: 80,
            cursor: Some(TerminalCursor {
                row: u16::try_from(ROWS).unwrap(),
                col: cursor_col,
                visible: true,
                style: 0,
            }),
            visible_text: wrapped_lines.join("\n"),
        };
        let editor = active_editor(&wrapped).expect("one editor, anchored at its own first row");
        assert_eq!(editor.anchor_row, 1, "the anchor is the composer's own row");
        assert_eq!(editor.cursor_row, 21, "the cursor row is the fence's own");
        assert_eq!(editor.signature.rendered_rows.len(), ROWS);

        let (control, handle) = control_for(
            [baseline.clone(), baseline.clone()],
            [wrapped.clone(), wrapped.clone()],
            Duration::ZERO,
            Duration::from_millis(30),
        );
        submit(&control, &prompt).await.unwrap();
        assert_eq!(handle.counts(), (1, 1));

        // One SEGMENT short on the last row is a truncated composer, and every
        // geometric clause still holds: same anchor, same cursor row, same
        // prompt column, twenty-one rows. Only the text disagrees, and before
        // this proof nothing below the first row was compared at all.
        let mut short_lines = wrapped_lines.clone();
        short_lines[ROWS] = format!("  {}", [SEGMENT; PER_ROW - 1].join(" "));
        let short = TerminalSnapshot {
            visible_text: short_lines.join("\n"),
            ..wrapped
        };
        let (control, handle) = control_for(
            [baseline.clone(), baseline],
            [short.clone(), short],
            Duration::ZERO,
            Duration::from_millis(30),
        );
        assert_eq!(
            submit(&control, &prompt).await.unwrap_err().code,
            ErrorCode::PromptNotAcknowledged
        );
        assert_eq!(handle.counts(), (1, 0));
    }

    /// The screen a successful `/clear` leaves behind, MEASURED verbatim on
    /// Claude Code 2.1.220 (`rows=24 cols=80 revision=27
    /// cursor=(row=5,col=2,visible=true)`, byte-identical for 285s across ~4,250
    /// samples in a second session).
    ///
    /// Ink repaints from where the previous frame ended, so the whole frame is
    /// four rows tall at the TOP of the grid -- rule, composer, rule, footer --
    /// and rows 8..=23 are of length zero. The composer is 2 rows off the end of
    /// the frame and 18 off the bottom of the grid.
    fn post_clear_lines() -> Vec<String> {
        let rule = "\u{2500}".repeat(80);
        let mut lines = vec![String::new(); 24];
        lines[0] = "  \u{258e} cess".to_owned();
        lines[2] = "\u{276f} /clear".to_owned();
        lines[4] = rule.clone();
        lines[5] = "\u{276f}\u{a0}".to_owned();
        lines[6] = rule;
        lines[7] = "  \u{23f8} manual mode on \u{b7} ? for shortcuts \u{b7} \u{2190} for agents"
            .to_owned();
        lines
    }

    fn grid_snapshot(
        revision: u64,
        lines: &[String],
        cursor_row: u16,
        cursor_col: u16,
    ) -> TerminalSnapshot {
        TerminalSnapshot {
            revision,
            rows: u16::try_from(lines.len()).unwrap(),
            cols: 80,
            cursor: Some(TerminalCursor {
                row: cursor_row,
                col: cursor_col,
                visible: true,
                style: 0,
            }),
            visible_text: lines.join("\n"),
        }
    }

    #[tokio::test]
    async fn input_gate_submits_into_the_top_anchored_composer_a_clear_leaves_behind() {
        let baseline_lines = post_clear_lines();
        let baseline = grid_snapshot(27, &baseline_lines, 5, 2);
        assert_eq!(
            classify_terminal_snapshot(&baseline),
            TerminalScreenState::Ready,
            "the composer a /clear leaves behind is provably empty; refusing it \
             costs the first turn of every cleared cell"
        );

        // MEASURED by a zero-model bracketed-paste probe into that composer: a
        // prompt wider than one composer row keeps the `❯` anchor pinned at row
        // 5 and walks the cursor DOWN, the mirror image of the bottom-anchored
        // growth `input_gate_correlates_a_deep_wrapped_editor_to_the_pre_paste_anchor`
        // pins.
        let mut wrapped_lines = baseline_lines.clone();
        wrapped_lines[5] = "\u{276f} a prompt too wide for one composer row".to_owned();
        wrapped_lines[6] = "  and its second rendered row".to_owned();
        wrapped_lines[7] = "  and its third".to_owned();
        wrapped_lines[8] = "\u{2500}".repeat(80);
        wrapped_lines[9] = "  \u{23f8} manual mode on".to_owned();
        let wrapped = grid_snapshot(28, &wrapped_lines, 7, 17);

        let (control, handle) = control_for(
            [baseline.clone(), baseline],
            [wrapped.clone(), wrapped],
            Duration::ZERO,
            Duration::from_millis(30),
        );
        submit(
            &control,
            "a prompt too wide for one composer row and its second rendered row and its third",
        )
        .await
        .unwrap();
        assert_eq!(handle.counts(), (1, 1));
    }

    /// The bound is still a bound, and it is still four: what moved is the row
    /// it is measured from, not how much slack it grants.
    #[test]
    fn rendered_rows_below_the_cursor_are_bounded_whatever_the_blank_tail_says() {
        let baseline_lines = post_clear_lines();
        for below in 0..=4_usize {
            let mut lines = baseline_lines.clone();
            for (offset, line) in lines.iter_mut().skip(6).enumerate() {
                *line = if offset < below {
                    "  rendered".to_owned()
                } else {
                    String::new()
                };
            }
            assert_eq!(
                classify_terminal_snapshot(&grid_snapshot(27, &lines, 5, 2)),
                TerminalScreenState::Ready,
                "{below} rendered rows below the composer is within the bound"
            );
        }
        let mut deep = baseline_lines.clone();
        for line in deep.iter_mut().take(11).skip(6) {
            *line = "  rendered".to_owned();
        }
        assert_eq!(
            classify_terminal_snapshot(&grid_snapshot(27, &deep, 5, 2)).label(),
            "unrecognised",
            "five rendered rows below the composer is out of the bound, or the \
             assertions above are vacuous"
        );
        // A cursor parked below every rendered row is in no frame at all.
        assert_eq!(
            classify_terminal_snapshot(&grid_snapshot(27, &baseline_lines, 9, 2)).label(),
            "unrecognised"
        );
        // And a grid with nothing rendered on it is not a composer either.
        assert_eq!(
            classify_terminal_snapshot(&grid_snapshot(27, &vec![String::new(); 24], 5, 2)).label(),
            "unrecognised"
        );
    }

    /// The one case where a whole megabyte of prompt leaves NOTHING of itself on
    /// the screen, and the gate must still submit it.
    ///
    /// The placeholder used to read `[Pasted text #1 +12000 lines]` for a prompt
    /// of one million `x` and not one newline — an invented render that the
    /// measurement contradicts twice over: the ` +n lines` clause is absent when
    /// a paste has no line breaks, and when it is present `n` is the line-break
    /// count and never a guess. Both facts are in
    /// `crates/claude/src/composer.rs`.
    #[tokio::test]
    async fn near_service_limit_prompt_accepts_a_collapsed_editor_render() {
        let baseline = baseline_snapshot(1);
        let collapsed = rendered_snapshot(2, "❯ [Pasted text #1]", 20);
        let (control, handle) = control_for(
            [baseline.clone(), baseline.clone()],
            [collapsed.clone(), collapsed],
            Duration::ZERO,
            Duration::from_millis(30),
        );
        let prompt = "x".repeat(MAX_PROMPT_BYTES);
        submit(&control, &prompt).await.unwrap();
        assert_eq!(handle.counts(), (1, 1));
        // A placeholder claiming line breaks this prompt does not have is a
        // different paste, and it is refused.
        let wrong = rendered_snapshot(2, "❯ [Pasted text #1 +12000 lines]", 34);
        let (control, handle) = control_for(
            [baseline.clone(), baseline],
            [wrong.clone(), wrong],
            Duration::ZERO,
            Duration::from_millis(30),
        );
        assert_eq!(
            submit(&control, &prompt).await.unwrap_err().code,
            ErrorCode::PromptNotAcknowledged
        );
        assert_eq!(handle.counts(), (1, 0));
    }

    #[tokio::test]
    async fn input_gate_ignores_modal_words_inside_the_pasted_editor() {
        let baseline = baseline_snapshot(1);
        let rendered = rendered_snapshot(
            2,
            "❯ explain permission allow deny and trust this folder",
            54,
        );
        let (control, handle) = control_for(
            [baseline.clone(), baseline],
            [rendered.clone(), rendered],
            Duration::ZERO,
            Duration::from_millis(30),
        );
        submit(
            &control,
            "explain permission allow deny and trust this folder",
        )
        .await
        .unwrap();
        assert_eq!(handle.counts(), (1, 1));
    }

    #[tokio::test]
    async fn real_modal_without_an_active_editor_blocks_before_paste() {
        let modal = structured_snapshot(
            1,
            [
                "",
                "Permission required",
                "Allow this command or deny it?",
                "",
                "",
                "choose",
                "footer",
                "status",
            ],
            5,
            2,
            true,
        );
        let (control, handle) = control_for(
            [modal.clone()],
            [modal],
            Duration::ZERO,
            Duration::from_millis(20),
        );
        let error = submit(&control, "safe").await.unwrap_err();
        assert_eq!(error.code, ErrorCode::NeedsPermission);
        assert_eq!(handle.counts(), (0, 0));
    }

    #[tokio::test]
    async fn pre_paste_fence_mutation_never_pastes_or_enters() {
        let first = baseline_snapshot(1);
        let second = TerminalSnapshot {
            revision: 2,
            visible_text: first.visible_text.replace("status", "changed status"),
            ..first.clone()
        };
        let (control, handle) = control_for(
            [first, second],
            [rendered_snapshot(3, "❯ safe", 6)],
            Duration::ZERO,
            Duration::from_millis(12),
        );
        handle.state.lock().unwrap().cycle_before = true;
        let error = submit(&control, "safe").await.unwrap_err();
        assert_eq!(error.code, ErrorCode::PromptNotAcknowledged);
        assert_eq!(handle.counts(), (0, 0));
    }

    #[tokio::test]
    async fn one_time_pre_paste_fence_mutation_reacquires_without_duplicate_paste() {
        let first = baseline_snapshot(1);
        let second = TerminalSnapshot {
            revision: 2,
            visible_text: first.visible_text.replace("status", "changed status"),
            ..first.clone()
        };
        let rendered = rendered_snapshot(3, "❯ safe", 6);
        let (control, handle) = control_for(
            [first, second.clone(), second],
            [rendered.clone(), rendered],
            Duration::ZERO,
            Duration::from_millis(30),
        );
        submit(&control, "safe").await.unwrap();
        assert_eq!(handle.counts(), (1, 1));
    }

    #[tokio::test]
    async fn no_editor_echo_or_unrelated_footer_revisions_never_send_enter() {
        let baseline = baseline_snapshot(1);
        let footer_one = TerminalSnapshot {
            revision: 2,
            visible_text: baseline.visible_text.replace("status", "status one"),
            ..baseline.clone()
        };
        let footer_two = TerminalSnapshot {
            revision: 3,
            visible_text: baseline.visible_text.replace("status", "status two"),
            ..baseline.clone()
        };
        let (control, handle) = control_for(
            [baseline.clone(), baseline],
            [footer_one, footer_two],
            Duration::ZERO,
            Duration::from_millis(12),
        );
        handle.state.lock().unwrap().cycle_after = true;
        let error = submit(&control, "safe").await.unwrap_err();
        assert_eq!(error.code, ErrorCode::PromptNotAcknowledged);
        assert_eq!(handle.counts(), (1, 0));
    }

    /// Both frames are legitimate renders of the SAME prompt -- a paste caught
    /// half-drawn and then complete -- so the head proof passes on each and the
    /// only thing left to fail on is the instability this test is named for.
    /// The pair used to be `safe` / `safe!` under the prompt `safe`, which the
    /// head proof would refuse outright: the test would still have gone red,
    /// for a reason its name does not mention.
    #[tokio::test]
    async fn unstable_final_fence_never_repastes_or_enters() {
        let baseline = baseline_snapshot(1);
        let first = rendered_snapshot(2, "❯ safe", 6);
        let second = rendered_snapshot(3, "❯ safe!", 7);
        let (control, handle) = control_for(
            [baseline.clone(), baseline],
            [first, second],
            Duration::ZERO,
            Duration::from_millis(12),
        );
        handle.state.lock().unwrap().cycle_after = true;
        let error = submit(&control, "safe!").await.unwrap_err();
        assert_eq!(error.code, ErrorCode::PromptNotAcknowledged);
        assert_eq!(handle.counts(), (1, 0));
    }

    #[tokio::test]
    async fn one_time_post_render_fence_mutation_restabilizes_without_repaste() {
        let baseline = baseline_snapshot(1);
        let first = rendered_snapshot(2, "❯ safe", 6);
        let second = rendered_snapshot(3, "❯ safe!", 7);
        let (control, handle) = control_for(
            [baseline.clone(), baseline],
            [first, second.clone(), second],
            Duration::ZERO,
            Duration::from_millis(30),
        );
        submit(&control, "safe!").await.unwrap();
        assert_eq!(handle.counts(), (1, 1));
    }

    #[tokio::test]
    async fn lease_loss_after_paste_ack_never_sends_enter() {
        let baseline = baseline_snapshot(1);
        let rendered = rendered_snapshot(2, "❯ safe", 6);
        let (control, handle) = control_for(
            [baseline.clone(), baseline],
            [rendered.clone(), rendered],
            Duration::ZERO,
            Duration::from_millis(30),
        );
        handle.state.lock().unwrap().lose_lease_after_paste = true;
        let error = submit(&control, "safe").await.unwrap_err();
        assert_eq!(error.code, ErrorCode::DaemonLost);
        assert_eq!(handle.counts(), (1, 0));
    }

    #[tokio::test]
    async fn deadline_and_lease_expiry_fail_closed_without_enter() {
        let unknown = structured_snapshot(
            1,
            ["", "", "", "", "", "booting", "footer", "status"],
            5,
            2,
            false,
        );
        let (control, handle) = control_for(
            [unknown.clone()],
            [unknown],
            Duration::ZERO,
            Duration::from_millis(100),
        );
        let error = control
            .submit_prompt(
                SessionId::new_v4(),
                TurnId::new_v4(),
                "safe",
                now_unix_ms().unwrap().checked_add(5).unwrap(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::TurnTimeout);
        assert_eq!(handle.counts(), (0, 0));

        let baseline = baseline_snapshot(1);
        let (control, handle) = control_for(
            [baseline.clone(), baseline],
            [baseline_snapshot(2)],
            Duration::ZERO,
            Duration::from_millis(100),
        );
        let error = control
            .submit_prompt(
                SessionId::new_v4(),
                TurnId::new_v4(),
                "safe",
                now_unix_ms().unwrap().checked_add(5).unwrap(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::TurnTimeout);
        assert_eq!(handle.counts(), (1, 0));

        let baseline = baseline_snapshot(1);
        let rendered = rendered_snapshot(2, "❯ safe", 6);
        let (control, handle) = control_for(
            [baseline.clone(), baseline],
            [rendered.clone(), rendered],
            Duration::ZERO,
            Duration::from_millis(20),
        );
        handle.state.lock().unwrap().lease_lost = true;
        let error = submit(&control, "safe").await.unwrap_err();
        assert_eq!(error.code, ErrorCode::DaemonLost);
        assert_eq!(handle.counts(), (0, 0));
    }

    /// A budget bounded by the turn deadline, so an expiry inside a write is
    /// unambiguously the turn running out.
    ///
    /// `InputGateBudget::cap` is `min(gate maximum, remaining turn)`, so making
    /// the remaining turn the smaller term is what puts the two clocks in a
    /// known order. The gate maximum here is two orders of magnitude larger
    /// than the deadline, which is far more than the few milliseconds the gates
    /// themselves cost against this double.
    const DEADLINE_BOUND_GATE: Duration = Duration::from_secs(20);

    /// Longer than any budget below, so the write is still in flight when the
    /// budget expires rather than racing it.
    const HELD_WRITE: Duration = Duration::from_secs(30);

    fn deadline_in(milliseconds: u64) -> u64 {
        now_unix_ms().unwrap().checked_add(milliseconds).unwrap()
    }

    /// The turn deadline expiring *inside* a write is the deadline, not an
    /// ambiguous terminal.
    ///
    /// `gated_snapshot` and `gated_styled_screen` have always reclassified
    /// their own expiry this way; `paste_once` and `enter_once` did not, so the
    /// answer to one physical event -- this turn ran out of time -- depended on
    /// whether the clock happened to expire during a read or during a write.
    /// Every code below is what the *reads* on either side of these writes
    /// would have returned for the same instant.
    ///
    /// The `counts` assertions are load-bearing: they pin which call the budget
    /// expired in. Without them a gate that expired one read too early would
    /// report `TurnTimeout` from `gated_snapshot` and satisfy the code
    /// assertion while testing nothing about the write.
    #[tokio::test]
    async fn a_turn_deadline_that_expires_inside_a_write_is_reported_as_the_deadline_it_is() {
        // In the paste. Enter is never attempted, and the failure must not
        // claim it was.
        let baseline = baseline_snapshot(1);
        let rendered = rendered_snapshot(2, "❯ safe", 6);
        let (control, handle) = control_for(
            [baseline.clone(), baseline.clone()],
            [rendered.clone(), rendered.clone()],
            Duration::ZERO,
            DEADLINE_BOUND_GATE,
        );
        handle.hold_paste(HELD_WRITE);
        let error = control
            .submit_prompt(
                SessionId::new_v4(),
                TurnId::new_v4(),
                "safe",
                deadline_in(200),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::TurnTimeout);
        assert_eq!(handle.counts(), (1, 0));
        assert!(!enter_was_attempted(&error));

        // In the Enter. Same deadline, same answer -- and the one fact a bare
        // deadline answer would erase survives, because Enter did reach the
        // terminal and the caller's next move depends on knowing that.
        let (control, handle) = control_for(
            [baseline.clone(), baseline],
            [rendered.clone(), rendered],
            Duration::ZERO,
            DEADLINE_BOUND_GATE,
        );
        handle.hold_enter(HELD_WRITE);
        let error = control
            .submit_prompt(
                SessionId::new_v4(),
                TurnId::new_v4(),
                "safe",
                deadline_in(200),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::TurnTimeout);
        assert_eq!(handle.counts(), (1, 1));
        assert!(enter_was_attempted(&error));
    }

    /// The other half of the same rule: a gate maximum that expires with turn
    /// time still on the clock is NOT a deadline, and must not borrow its name.
    ///
    /// Without this, "reclassify a write expiry as a turn timeout" would be
    /// satisfiable by always answering `TurnTimeout`, which would tell a caller
    /// its turn ran out when what actually happened is that pmux could not
    /// prove the terminal took its input inside the gate's own bound.
    #[tokio::test]
    async fn a_gate_maximum_that_expires_inside_a_write_is_still_an_ambiguity() {
        let baseline = baseline_snapshot(1);
        let rendered = rendered_snapshot(2, "❯ safe", 6);
        let (control, handle) = control_for(
            [baseline.clone(), baseline.clone()],
            [rendered.clone(), rendered.clone()],
            Duration::ZERO,
            Duration::from_millis(200),
        );
        handle.hold_paste(HELD_WRITE);
        let error = control
            .submit_prompt(
                SessionId::new_v4(),
                TurnId::new_v4(),
                "safe",
                deadline_in(20_000),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::PromptNotAcknowledged);
        assert_eq!(handle.counts(), (1, 0));

        let (control, handle) = control_for(
            [baseline.clone(), baseline],
            [rendered.clone(), rendered],
            Duration::ZERO,
            Duration::from_millis(200),
        );
        handle.hold_enter(HELD_WRITE);
        let error = control
            .submit_prompt(
                SessionId::new_v4(),
                TurnId::new_v4(),
                "safe",
                deadline_in(20_000),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::RecoveryFailed);
        assert_eq!(handle.counts(), (1, 1));
        assert!(enter_was_attempted(&error));
    }

    /// The misreport reaching a caller, on the path where nothing masks it.
    ///
    /// `submit_prompt` is always wrapped in `await_turn_step`, which rechecks
    /// the deadline after the driver answers and overrides it, so the wrong
    /// code mostly does not survive the actor. `clear_and_rebind` is not: it is
    /// a direct actor command whose `ErrorBody` goes back to `pmux clear` and
    /// to `Pool::clear_pool_instance` exactly as the driver built it.
    ///
    /// And on that path the deadline is ALWAYS the binding term.
    /// `NativeService::clear_session` and `Pool` both compute the deadline as
    /// `unix_now_ms() + DEFAULT_CLEAR_TIMEOUT_MS`, which is 15,000 --
    /// numerically equal to `INPUT_GATE_MAX_DURATION` and computed strictly
    /// earlier -- so `min(gate maximum, remaining turn)` selects the remaining
    /// turn on every clear this daemon has ever issued. Every write expiry on
    /// the clear path was therefore a turn-deadline expiry wearing another
    /// code, not a rare race.
    ///
    /// The `clear_not_submitted` assertion is the reason this fix could not be
    /// "return `turn_deadline_failure()` and be done": `clear_and_rebind` reads
    /// `enter_attempted` to decide whether the bound transcript is suspect, so
    /// a deadline answer that dropped it would tell the actor nothing was typed
    /// after Enter had gone in.
    #[tokio::test]
    async fn a_clear_whose_deadline_expires_inside_a_write_reaches_its_caller_as_a_turn_timeout() {
        // Held in the paste. Enter was never sent, so the refusal is the
        // terminal adapter's own answer, unaltered, all the way to the caller.
        let fixture = RotationFixture::new();
        fixture.open_transcript(fixture.launch_session);
        let (control, handle) = clear_control_showing(measured_clear_screen(), DEADLINE_BOUND_GATE);
        handle.hold_paste(HELD_WRITE);

        let error = clear_and_rebind(
            &control,
            &fixture.source,
            fixture.launch_session,
            deadline_in(200),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::TurnTimeout);
        assert!(clear_was_not_submitted(&error.details));
        assert_eq!(handle.counts(), (1, 0));
        // Nothing was typed, so the bound transcript is still this session's
        // authority and the next turn can still be armed on it.
        fixture
            .source
            .arm_at_eof(fixture.launch_session)
            .await
            .unwrap();

        // Held in the Enter, which is the arm the deadline answer must not
        // simplify. `/clear` may have executed, so the refusal must NOT claim
        // the command was unsubmitted, and the driver must go on to the
        // rotation authority rather than returning the terminal's answer. It
        // reaches the caller as a rebind failure over a transcript that is now
        // unarmable -- which is exactly the quarantine a bare `TurnTimeout`
        // stripped of `enter_attempted` would have skipped.
        let fixture = RotationFixture::new();
        fixture.open_transcript(fixture.launch_session);
        let (control, handle) = clear_control_showing(measured_clear_screen(), DEADLINE_BOUND_GATE);
        handle.hold_enter(HELD_WRITE);

        let error = clear_and_rebind(
            &control,
            &fixture.source,
            fixture.launch_session,
            deadline_in(200),
        )
        .await
        .unwrap_err();
        assert_eq!(handle.counts(), (1, 1));
        assert!(
            !clear_was_not_submitted(&error.details),
            "a clear whose Enter went in claimed it had typed nothing: {error:?}"
        );
        assert_eq!(error.code, ErrorCode::TranscriptUnavailable);
        assert_eq!(error.details["violation"], "clear_rebind_not_observed");
        let stranded = fixture
            .source
            .arm_at_eof(fixture.launch_session)
            .await
            .unwrap_err();
        assert_eq!(stranded.details["violation"], "clear_rebind_failed");
    }

    #[tokio::test]
    async fn completion_evidence_rejects_modal_before_or_during_stability() {
        let modal = structured_snapshot(
            2,
            [
                "",
                "Permission required",
                "Allow this command or deny it?",
                "",
                "",
                "choose",
                "footer",
                "status",
            ],
            5,
            2,
            true,
        );
        let (terminal, handle) = FakeTerminal::new([modal.clone()], [modal.clone()]);
        let control = RmuxTerminalControl::new(Box::new(terminal)).with_timings(
            Duration::from_millis(2),
            Duration::from_millis(80),
            Duration::from_millis(30),
        );
        let evidence = control
            .completion_evidence(SessionId::new_v4(), TurnId::new_v4())
            .await
            .unwrap();
        assert!(!evidence.ready_prompt);
        assert!(!evidence.quiet);
        assert_eq!(handle.counts(), (0, 0));

        let ready = baseline_snapshot(1);
        let (terminal, handle) =
            FakeTerminal::new([ready, modal.clone(), modal.clone()], [modal.clone()]);
        let control = RmuxTerminalControl::new(Box::new(terminal)).with_timings(
            Duration::from_millis(2),
            Duration::from_millis(80),
            Duration::from_millis(30),
        );
        let evidence = control
            .completion_evidence(SessionId::new_v4(), TurnId::new_v4())
            .await
            .unwrap();
        assert!(!evidence.ready_prompt);
        assert!(!evidence.quiet);
        assert_eq!(handle.counts(), (0, 0));
    }

    /// The two modal returns report the same lifecycle observation the ordinary
    /// return does.
    ///
    /// A modal screen is negative readiness evidence, so the actor takes the
    /// evidence and polls again -- which is exactly why dropping the lifecycle
    /// fields on those paths is silent. `lifecycle_expected: false` tells the
    /// actor this turn was never armed for a Stop hook, so the completion
    /// authority stops waiting for one; a dropped `lifecycle_hook_observed`
    /// retires a hook that did fire; and a dropped `lifecycle_hook_at_ms`
    /// erases the only instant `TurnTimings::stop_hook_at_ms` can publish. All
    /// three defaults are the "nothing happened" value, so nothing downstream
    /// can tell the difference between the field being absent and the hook
    /// being absent.
    #[tokio::test]
    async fn completion_evidence_carries_the_lifecycle_observation_on_the_modal_returns() {
        let modal = structured_snapshot(
            2,
            [
                "",
                "Permission required",
                "Allow this command or deny it?",
                "",
                "",
                "choose",
                "footer",
                "status",
            ],
            5,
            2,
            true,
        );
        let before_paste: [(&str, Vec<TerminalSnapshot>); 2] = [
            ("modal on the first snapshot", vec![modal.clone()]),
            (
                "modal only once the screen settles",
                vec![baseline_snapshot(1), modal.clone(), modal.clone()],
            ),
        ];
        for (path, snapshots) in before_paste {
            for (fired, expected_at_ms) in [(false, None), (true, Some(1_700_000_000_123))] {
                let (terminal, _handle) = FakeTerminal::new(snapshots.clone(), [modal.clone()]);
                let stop_sequence = Arc::new(AtomicU64::new(0));
                let stop_at_ms = Arc::new(AtomicU64::new(0));
                if fired {
                    stop_at_ms.store(1_700_000_000_123, Ordering::Release);
                    stop_sequence.fetch_add(1, Ordering::Release);
                }
                let control = RmuxTerminalControl::new(Box::new(terminal))
                    .with_timings(
                        Duration::from_millis(2),
                        Duration::from_millis(80),
                        Duration::from_millis(30),
                    )
                    .with_lifecycle_observation(
                        Arc::clone(&stop_sequence),
                        Arc::clone(&stop_at_ms),
                    );
                let evidence = control
                    .completion_evidence(SessionId::new_v4(), TurnId::new_v4())
                    .await
                    .unwrap();
                let where_ = format!("{path}, hook fired: {fired}");
                assert!(!evidence.ready_prompt, "{where_}");
                assert!(evidence.lifecycle_expected, "{where_}");
                assert_eq!(evidence.lifecycle_hook_observed, fired, "{where_}");
                assert_eq!(evidence.lifecycle_hook_at_ms, expected_at_ms, "{where_}");
            }
        }
    }

    /// Every `TerminalEvidence` `completion_evidence` builds names all three
    /// lifecycle fields, read off the function's own source.
    ///
    /// The behavioural test above covers the return sites that exist today; a
    /// return site added tomorrow would be covered by neither it nor anything
    /// else, because `..TerminalEvidence::default()` makes an omitted lifecycle
    /// field compile into the value that means "no hook, never armed". The
    /// destructuring below is what keeps this test's idea of "the lifecycle
    /// observation" tied to the struct: a fourth `lifecycle_*` field cannot be
    /// added without this failing to compile.
    #[test]
    fn every_completion_evidence_return_names_the_whole_lifecycle_observation() {
        const OPENING: &str = "    async fn completion_evidence(";
        let TerminalEvidence {
            ready_prompt: _,
            quiet: _,
            lifecycle_expected: _,
            lifecycle_hook_observed: _,
            lifecycle_hook_at_ms: _,
        } = TerminalEvidence::default();
        let required = [
            "lifecycle_expected",
            "lifecycle_hook_observed",
            "lifecycle_hook_at_ms",
        ];
        let body = include_str!("driver_io.rs")
            .split_once(OPENING)
            .expect("this module defines completion_evidence")
            .1
            .split_once("\n    }\n")
            .expect("the method's body closes at the impl's indent")
            .0;
        let mut sites = 0;
        let mut rest = body;
        while let Some((_, after_open)) = rest.split_once("TerminalEvidence {") {
            let (expression, after_close) = after_open
                .split_once('}')
                .expect("a struct expression closes inside the method body");
            sites += 1;
            for field in required {
                assert!(
                    expression.contains(field),
                    "the TerminalEvidence expression `{expression}` omits {field}"
                );
            }
            rest = after_close;
        }
        assert!(
            sites >= 3,
            "completion_evidence builds {sites} TerminalEvidence values; the two modal \
             returns and the ordinary one are three, so this parse stopped early"
        );
    }

    #[tokio::test]
    async fn completion_evidence_carries_the_stop_hook_instant_only_for_the_armed_turn() {
        let ready = baseline_snapshot(1);
        let (terminal, _handle) = FakeTerminal::new([ready.clone()], [ready]);
        let stop_sequence = Arc::new(AtomicU64::new(0));
        let stop_at_ms = Arc::new(AtomicU64::new(0));
        let control = RmuxTerminalControl::new(Box::new(terminal))
            .with_timings(
                Duration::from_millis(2),
                Duration::from_millis(80),
                Duration::from_millis(30),
            )
            .with_lifecycle_observation(Arc::clone(&stop_sequence), Arc::clone(&stop_at_ms));

        let evidence = control
            .completion_evidence(SessionId::new_v4(), TurnId::new_v4())
            .await
            .unwrap();
        assert!(evidence.lifecycle_expected);
        assert!(!evidence.lifecycle_hook_observed);
        assert_eq!(evidence.lifecycle_hook_at_ms, None);

        // Exactly what the lifecycle task publishes: stamp, then bump.
        stop_at_ms.store(1_700_000_000_123, Ordering::Release);
        stop_sequence.fetch_add(1, Ordering::Release);
        let evidence = control
            .completion_evidence(SessionId::new_v4(), TurnId::new_v4())
            .await
            .unwrap();
        assert!(evidence.lifecycle_hook_observed);
        assert_eq!(evidence.lifecycle_hook_at_ms, Some(1_700_000_000_123));

        // Re-arming for the next turn (what `submit_prompt` does) retires both
        // halves together. The session-scoped stamp survives, but it describes
        // the previous turn's Stop and must not be published against this one.
        control
            .lifecycle_baseline
            .store(stop_sequence.load(Ordering::Acquire), Ordering::Release);
        let evidence = control
            .completion_evidence(SessionId::new_v4(), TurnId::new_v4())
            .await
            .unwrap();
        assert!(!evidence.lifecycle_hook_observed);
        assert_eq!(evidence.lifecycle_hook_at_ms, None);
        assert_eq!(stop_at_ms.load(Ordering::Acquire), 1_700_000_000_123);
    }

    #[tokio::test]
    async fn completion_evidence_fails_on_lease_loss_at_every_snapshot_boundary() {
        let ready = baseline_snapshot(1);
        let (terminal, handle) = FakeTerminal::new([ready.clone()], [ready.clone()]);
        handle.state.lock().unwrap().lease_lost = true;
        let control = RmuxTerminalControl::new(Box::new(terminal)).with_timings(
            Duration::from_millis(2),
            Duration::from_millis(30),
            Duration::from_millis(30),
        );
        let error = control
            .completion_evidence(SessionId::new_v4(), TurnId::new_v4())
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::DaemonLost);
        assert!(error.retryable);
        assert_eq!(handle.counts(), (0, 0));

        let modal = structured_snapshot(
            2,
            [
                "",
                "Permission required",
                "Allow this command or deny it?",
                "",
                "",
                "choose",
                "footer",
                "status",
            ],
            5,
            2,
            true,
        );
        let (terminal, handle) = FakeTerminal::new([modal.clone()], [modal]);
        handle.state.lock().unwrap().lose_lease_after_snapshot = Some(1);
        let control = RmuxTerminalControl::new(Box::new(terminal)).with_timings(
            Duration::from_millis(2),
            Duration::from_millis(30),
            Duration::from_millis(30),
        );
        let error = control
            .completion_evidence(SessionId::new_v4(), TurnId::new_v4())
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::DaemonLost);
        assert!(error.retryable);
        assert_eq!(handle.counts(), (0, 0));

        // This case is the only one of the three that has to survive a real
        // `TERMINAL_POLL_INTERVAL` sleep: the lease is lost at the *second*
        // snapshot, which `wait_for_snapshot_stability` only takes after
        // sleeping one poll interval. Both budgets are therefore sized in
        // multiples of that interval rather than below it. With a 30 ms
        // stability timeout the 25 ms sleep left under 5 ms of budget, so a
        // loaded machine reached the deadline first and the call returned
        // `quiet: false` instead of the lease error -- a scheduling race, not a
        // behaviour this test was ever trying to assert. A 3 ms quiet window
        // had the same shape: a preemption longer than it, between arming
        // `stable_since` and the first stability check, declared the screen
        // stable before the second snapshot ever happened.
        //
        // Neither budget can expire here now, so the loop's only exit is the
        // lease check, and the test still completes in about one poll interval.
        let (terminal, handle) = FakeTerminal::new([ready.clone(), ready.clone()], [ready.clone()]);
        handle.state.lock().unwrap().lose_lease_after_snapshot = Some(2);
        let control = RmuxTerminalControl::new(Box::new(terminal)).with_timings(
            Duration::from_secs(5),
            Duration::from_secs(5),
            Duration::from_millis(30),
        );
        let error = control
            .completion_evidence(SessionId::new_v4(), TurnId::new_v4())
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::DaemonLost);
        assert!(error.retryable);
        assert_eq!(handle.counts(), (0, 0));
    }

    #[tokio::test]
    async fn ambiguous_paste_and_enter_are_never_retried_and_errors_are_redacted() {
        let secret = "private-secret-prompt-42";
        let baseline = baseline_snapshot(1);
        // The composer renders the secret, because that is what a real composer
        // holding this prompt does. It also makes the redaction assertions
        // below strictly stronger: the string being searched for is now on the
        // screen the failing code was looking at.
        let rendered = rendered_row_snapshot(
            2,
            &format!("❯\u{a0}{secret}"),
            2 + u16::try_from(secret.chars().count()).unwrap(),
        );
        let (control, handle) = control_for(
            [baseline.clone(), baseline.clone()],
            [rendered.clone(), rendered.clone()],
            Duration::ZERO,
            Duration::from_millis(20),
        );
        handle.state.lock().unwrap().paste_error = true;
        let error = submit(&control, secret).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::PromptNotAcknowledged);
        assert_eq!(handle.counts(), (1, 0));
        assert!(!format!("{} {}", error.message, error.details).contains(secret));
        assert!(!error.retryable);

        let (control, handle) = control_for(
            [baseline.clone(), baseline],
            [rendered.clone(), rendered],
            Duration::ZERO,
            Duration::from_millis(20),
        );
        handle.state.lock().unwrap().enter_error = true;
        let error = submit(&control, secret).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::RecoveryFailed);
        assert_eq!(error.details["enter_attempted"], true);
        assert!(!error.retryable);
        assert_eq!(handle.counts(), (1, 1));
        let rendered_error = format!("{} {}", error.message, error.details);
        assert!(!rendered_error.contains(secret));
        assert!(!rendered_error.contains("private Enter failure"));
    }

    #[test]
    fn terminal_backend_errors_are_redacted_at_the_service_boundary() {
        let secret = "private-screen-or-matcher";
        let (terminal, _handle) = FakeTerminal::new([], []);
        for error in [
            TerminalBackendError::InvalidLaunch(secret.to_owned()),
            TerminalBackendError::ControlPlaneLost,
            TerminalBackendError::Rmux(secret.to_owned()),
            TerminalBackendError::ProcessBoundary(secret.to_owned()),
        ] {
            let failure = map_terminal_error(&terminal, error);
            assert!(!format!("{} {}", failure.message, failure.details).contains(secret));
        }
    }

    /// A lost control plane is now scoped to one session, so the wire has to
    /// say which of two very different things happened without inventing a new
    /// error code for it.
    ///
    /// The retryability assertion is the load-bearing one and is deliberately
    /// identical for both: it is pinned end to end by
    /// `crates/e2e/tests/full_stack.rs::active_public_turn_sidecar_loss_is_typed_and_reaps_the_process_boundary`,
    /// which SIGKILLs a real sidecar mid-turn and requires `daemon_lost` *and*
    /// `retryable`. Per-session transports made a retry strictly more likely to
    /// work, never less, so nothing here moves it.
    #[test]
    fn a_lost_control_plane_names_its_scope_without_changing_its_retryability() {
        let (transport_only, _transport_handle) = FakeTerminal::new([], []);
        let (leased_away, lease_handle) = FakeTerminal::new([], []);
        lease_handle.state.lock().unwrap().lease_lost = true;

        let transport_loss =
            map_terminal_error(&transport_only, TerminalBackendError::ControlPlaneLost);
        assert_eq!(transport_loss.code, ErrorCode::DaemonLost);
        assert!(transport_loss.retryable);
        assert!(
            transport_loss.message.contains("session control plane"),
            "a session-scoped loss must not be reported as a daemon-wide one: {}",
            transport_loss.message
        );

        let lease_loss = map_terminal_error(&leased_away, TerminalBackendError::ControlPlaneLost);
        assert_eq!(lease_loss.code, ErrorCode::DaemonLost);
        assert!(lease_loss.retryable);
        assert!(
            lease_loss.message.contains("session lease"),
            "a lost lease is the stronger claim and must be reported as one: {}",
            lease_loss.message
        );
    }

    #[tokio::test]
    async fn file_source_arms_at_eof_and_tails_only_new_complete_rows() {
        let root = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let config = root.path().join("claude");
        let project = config.join("projects/project");
        std::fs::create_dir_all(&project).unwrap();
        let session_id = SessionId::new_v4();
        let transcript = project.join(format!("{session_id}.jsonl"));
        let row = |uuid: &str, content: &str| {
            json!({
                "type": "user",
                "uuid": uuid,
                "parentUuid": null,
                "sessionId": session_id,
                "cwd": cwd.path().canonicalize().unwrap(),
                "promptSource": "typed",
                "message": { "content": content }
            })
            .to_string()
        };
        std::fs::write(&transcript, format!("{}\n", row("old", "old"))).unwrap();

        let source =
            FileTranscriptSource::new(config.canonicalize().unwrap(), cwd.path(), session_id)
                .unwrap();
        let arm = source.arm_at_eof(session_id).await.unwrap();
        assert!(arm.historical_rows.is_empty());
        assert_eq!(
            arm.position.offset,
            std::fs::metadata(&transcript).unwrap().len()
        );

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&transcript)
            .unwrap();
        write!(file, "{}", row("new", "new")).unwrap();
        file.flush().unwrap();
        let partial = source.poll(session_id, &arm.position).await.unwrap();
        assert!(partial.rows.is_empty());
        assert!(partial.drain.has_partial_line);
        writeln!(file).unwrap();
        file.flush().unwrap();
        let complete = source.poll(session_id, &partial.position).await.unwrap();
        assert_eq!(complete.rows.len(), 1);
        assert!(!complete.drain.has_partial_line);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn file_source_rejects_a_path_generation_swap_between_stat_and_open() {
        use std::os::unix::fs::MetadataExt;

        let fixture = transcript_fixture(|session_id, cwd| {
            vec![semantic_row(
                "typed_user",
                Some(json!(session_id)),
                Some(json!(cwd)),
            )]
        });
        let arm = fixture.source.arm_at_eof(fixture.session_id).await.unwrap();
        append_rows(&fixture.transcript, &fixture.pending_rows);

        let replacement = fixture.transcript.with_extension("replacement");
        write_rows(
            &replacement,
            &[json!({
                "type": "file-history-snapshot",
                "sessionId": fixture.session_id,
                "cwd": fixture._cwd.path().canonicalize().unwrap(),
            })],
        );
        let original_metadata = std::fs::metadata(&fixture.transcript).unwrap();
        let replacement_metadata = std::fs::metadata(&replacement).unwrap();
        assert_ne!(
            (original_metadata.dev(), original_metadata.ino()),
            (replacement_metadata.dev(), replacement_metadata.ino())
        );

        let mut state = fixture.source.state.lock().unwrap();
        assert_eq!(state.position(), arm.position);
        let observed = metadata_for(&fixture.transcript).unwrap();
        let range = state.cursor.observe(observed);
        std::fs::rename(&replacement, &fixture.transcript).unwrap();
        let error = fixture
            .source
            .read_observed_range(
                &mut state,
                &fixture.transcript,
                observed,
                range.read_from,
                range.read_to,
            )
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::TranscriptUnavailable);
        assert_eq!(
            error.message,
            "transcript file generation changed during an active filesystem observation"
        );
    }

    #[cfg(unix)]
    #[test]
    fn file_source_rejects_a_path_generation_swap_between_locate_and_arm_open() {
        use std::os::unix::fs::MetadataExt;

        let fixture = transcript_fixture(|_, _| Vec::new());
        let observed = metadata_for(&fixture.transcript).unwrap();
        let replacement = fixture.transcript.with_extension("arm-replacement");
        write_rows(
            &replacement,
            &[json!({
                "type": "file-history-snapshot",
                "sessionId": fixture.session_id,
                "cwd": fixture._cwd.path().canonicalize().unwrap(),
            })],
        );
        let replacement_metadata = std::fs::metadata(&replacement).unwrap();
        assert_ne!(
            (observed.identity.device, observed.identity.inode),
            (replacement_metadata.dev(), replacement_metadata.ino())
        );
        std::fs::rename(&replacement, &fixture.transcript).unwrap();

        let mut state = fixture.source.state.lock().unwrap();
        state.path = Some(fixture.transcript.clone());
        let error = fixture
            .source
            .seek_to_observed_eof(&mut state, &fixture.transcript, observed)
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::TranscriptUnavailable);
        assert_eq!(state.cursor.next_offset(), 0);
    }

    #[tokio::test]
    async fn absent_transcript_is_a_stable_empty_eof_observation() {
        let root = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let session_id = SessionId::new_v4();
        let source = FileTranscriptSource::new(root.path(), cwd.path(), session_id).unwrap();
        let arm = source.arm_at_eof(session_id).await.unwrap();

        tokio::time::sleep(Duration::from_millis(2)).await;
        let batch = source.poll(session_id, &arm.position).await.unwrap();
        assert!(batch.rows.is_empty());
        assert_eq!(batch.position, arm.position);
        assert!(batch.drain.at_eof);
        assert!(!batch.drain.has_partial_line);
        assert!(batch.drain.stable_for_ms >= 1);
    }

    #[tokio::test]
    async fn arming_a_long_existing_transcript_is_constant_space() {
        let fixture = transcript_fixture(|_, _| Vec::new());
        let long_len = MAX_TRANSCRIPT_READ_BYTES + 1024;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&fixture.transcript)
            .unwrap();
        file.set_len(long_len).unwrap();
        file.seek(SeekFrom::Start(long_len - 1)).unwrap();
        file.write_all(b"\n").unwrap();
        file.flush().unwrap();

        let arm = fixture.source.arm_at_eof(fixture.session_id).await.unwrap();
        assert_eq!(arm.position.offset, long_len);
        assert!(arm.historical_rows.is_empty());

        append_rows(
            &fixture.transcript,
            &[semantic_row(
                "typed_user",
                Some(json!(fixture.session_id)),
                Some(json!(fixture._cwd.path().canonicalize().unwrap())),
            )],
        );
        let batch = fixture
            .source
            .poll(fixture.session_id, &arm.position)
            .await
            .unwrap();
        assert_eq!(batch.rows.len(), 1);
    }

    #[tokio::test]
    async fn arming_rejects_an_existing_partial_record() {
        let fixture = transcript_fixture(|_, _| Vec::new());
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&fixture.transcript)
            .unwrap();
        file.write_all(b"partial-private-content").unwrap();
        file.flush().unwrap();

        let error = fixture
            .source
            .arm_at_eof(fixture.session_id)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::SchemaDrift);
        assert_eq!(error.details["violation"], "unterminated_record");
        assert!(!format!("{} {}", error.message, error.details).contains("private-content"));
    }

    #[tokio::test]
    async fn every_semantic_row_kind_rejects_a_different_session() {
        let wrong_session = SessionId::new_v4();
        for row_kind in [
            "typed_user",
            "attachment",
            "assistant",
            "user_tool_results",
            "user_other",
        ] {
            let fixture = transcript_fixture(|_, cwd| {
                vec![semantic_row(
                    row_kind,
                    Some(json!(wrong_session)),
                    Some(json!(cwd)),
                )]
            });
            let error = poll_fixture(&fixture).await.unwrap_err();
            assert_identity_error(&error, row_kind, "session_id", "mismatch");
            assert_redacted(
                &error,
                &[&wrong_session.to_string(), &fixture.session_id.to_string()],
            );
        }
    }

    #[tokio::test]
    async fn semantic_session_identity_is_required_and_must_be_a_uuid() {
        let missing =
            transcript_fixture(|_, cwd| vec![semantic_row("typed_user", None, Some(json!(cwd)))]);
        let error = poll_fixture(&missing).await.unwrap_err();
        assert_identity_error(&error, "typed_user", "session_id", "missing");

        let private_invalid_id = "not-a-uuid-private-value";
        let invalid = transcript_fixture(|_, cwd| {
            vec![semantic_row(
                "assistant",
                Some(json!(private_invalid_id)),
                Some(json!(cwd)),
            )]
        });
        let error = poll_fixture(&invalid).await.unwrap_err();
        assert_identity_error(&error, "assistant", "session_id", "invalid_uuid");
        assert_redacted(&error, &[private_invalid_id]);
    }

    #[tokio::test]
    async fn every_semantic_row_kind_rejects_a_different_cwd() {
        let private_wrong_cwd = format!("/private/pmux-wrong-cwd-{}", SessionId::new_v4());
        for row_kind in [
            "typed_user",
            "attachment",
            "assistant",
            "user_tool_results",
            "user_other",
        ] {
            let fixture = transcript_fixture(|session_id, _| {
                vec![semantic_row(
                    row_kind,
                    Some(json!(session_id)),
                    Some(json!(private_wrong_cwd)),
                )]
            });
            let error = poll_fixture(&fixture).await.unwrap_err();
            assert_identity_error(&error, row_kind, "cwd", "mismatch");
            assert_redacted(&error, &[&private_wrong_cwd]);
        }
    }

    #[tokio::test]
    async fn semantic_cwd_is_required_and_must_be_an_absolute_string() {
        for row_kind in [
            "typed_user",
            "attachment",
            "assistant",
            "user_tool_results",
            "user_other",
        ] {
            let missing = transcript_fixture(|session_id, _| {
                vec![semantic_row(row_kind, Some(json!(session_id)), None)]
            });
            let error = poll_fixture(&missing).await.unwrap_err();
            assert_identity_error(&error, row_kind, "cwd", "missing");
        }

        let private_value = "private-non-string-cwd";
        let invalid_type = transcript_fixture(|session_id, _| {
            vec![semantic_row(
                "typed_user",
                Some(json!(session_id)),
                Some(json!({"private": private_value})),
            )]
        });
        let error = poll_fixture(&invalid_type).await.unwrap_err();
        assert_identity_error(&error, "typed_user", "cwd", "invalid_type");
        assert_redacted(&error, &[private_value]);

        let private_relative_cwd = "relative/private-cwd";
        let relative = transcript_fixture(|session_id, _| {
            vec![semantic_row(
                "assistant",
                Some(json!(session_id)),
                Some(json!(private_relative_cwd)),
            )]
        });
        let error = poll_fixture(&relative).await.unwrap_err();
        assert_identity_error(&error, "assistant", "cwd", "not_absolute");
        assert_redacted(&error, &[private_relative_cwd]);
    }

    #[tokio::test]
    async fn semantic_rows_accept_canonically_equivalent_cwd() {
        let fixture = transcript_fixture(|session_id, cwd| {
            let equivalent_cwd = format!("{}/.", cwd.display());
            vec![
                semantic_row(
                    "typed_user",
                    Some(json!(session_id.to_string().to_uppercase())),
                    Some(json!(cwd)),
                ),
                semantic_row("assistant", Some(json!(session_id)), Some(json!(cwd))),
                semantic_row("attachment", Some(json!(session_id)), Some(json!(cwd))),
                semantic_row(
                    "user_tool_results",
                    Some(json!(session_id)),
                    Some(json!(equivalent_cwd)),
                ),
                semantic_row("user_other", Some(json!(session_id)), Some(json!(cwd))),
            ]
        });
        let batch = poll_fixture(&fixture).await.unwrap();
        assert_eq!(batch.rows.len(), 5);
    }

    #[tokio::test]
    async fn unicode_normalization_equivalent_cwd_is_accepted() {
        let root = TempDir::new().unwrap();
        let cwd = root.path().join("caf\u{e9}");
        std::fs::create_dir(&cwd).unwrap();
        let candidate_cwd = root.path().join("cafe\u{301}");
        let config = root.path().join("claude");
        let project = config.join("projects/project");
        std::fs::create_dir_all(&project).unwrap();
        let session_id = SessionId::new_v4();
        let transcript = project.join(format!("{session_id}.jsonl"));
        write_rows(
            &transcript,
            &[json!({
                "type": "file-history-snapshot",
                "sessionId": session_id,
                "cwd": cwd.canonicalize().unwrap(),
            })],
        );

        let source =
            FileTranscriptSource::new(config.canonicalize().unwrap(), &cwd, session_id).unwrap();
        let arm = source.arm_at_eof(session_id).await.unwrap();
        append_rows(
            &transcript,
            &[semantic_row(
                "typed_user",
                Some(json!(session_id)),
                Some(json!(candidate_cwd)),
            )],
        );
        let batch = source.poll(session_id, &arm.position).await.unwrap();
        assert_eq!(batch.rows.len(), 1);
    }

    #[tokio::test]
    async fn metadata_system_and_unknown_rows_are_not_identity_bound() {
        let private_metadata = "metadata-private-value";
        let private_system_cwd = "/private/system-row-cwd";
        let fixture = transcript_fixture(|session_id, cwd| {
            vec![
                json!({
                    "type": "file-history-snapshot",
                    "sessionId": private_metadata,
                    "cwd": {"private": private_metadata},
                }),
                json!({
                    "type": "system",
                    "subtype": "turn_duration",
                    "uuid": "system-row",
                    "parentUuid": "typed-user-row",
                    "sessionId": private_metadata,
                    "cwd": private_system_cwd,
                }),
                json!({
                    "type": "future-semantic-row",
                    "uuid": "unknown-row",
                    "parentUuid": "system-row",
                    "sessionId": private_metadata,
                    "cwd": {"private": private_metadata},
                }),
                semantic_row("typed_user", Some(json!(session_id)), Some(json!(cwd))),
            ]
        });
        let batch = poll_fixture(&fixture).await.unwrap();
        assert_eq!(batch.rows.len(), 4);
    }

    #[tokio::test]
    async fn appended_semantic_identity_violation_fails_the_poll_closed() {
        let fixture = transcript_fixture(|session_id, cwd| {
            vec![semantic_row(
                "typed_user",
                Some(json!(session_id)),
                Some(json!(cwd)),
            )]
        });
        let arm = fixture.source.arm_at_eof(fixture.session_id).await.unwrap();
        let private_wrong_session = SessionId::new_v4();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&fixture.transcript)
            .unwrap();
        for row in &fixture.pending_rows {
            writeln!(file, "{row}").unwrap();
        }
        writeln!(
            file,
            "{}",
            semantic_row("assistant", Some(json!(private_wrong_session)), None,)
        )
        .unwrap();
        file.flush().unwrap();

        let error = fixture
            .source
            .poll(fixture.session_id, &arm.position)
            .await
            .unwrap_err();
        assert_identity_error(&error, "assistant", "session_id", "mismatch");
        assert_redacted(&error, &[&private_wrong_session.to_string()]);
    }

    // ---- following a session that rotates its id ----------------------------
    //
    // A session can change its id without changing its cwd or its process:
    // `/clear` ABANDONS the current transcript instead of truncating it -- same
    // inode, same length, no further appends -- and opens a new file under a new
    // UUID in the same project directory. Every fence in this source stays green
    // against the abandoned file, which is exactly why the identity has to
    // travel per call: nothing else can tell the tail that the file it is
    // watching will never grow again.

    struct RotationFixture {
        _root: TempDir,
        _cwd: TempDir,
        canonical_cwd: PathBuf,
        project: PathBuf,
        source: Arc<FileTranscriptSource>,
        launch_session: SessionId,
    }

    impl RotationFixture {
        fn new() -> Self {
            Self::with_rebind_timeout(Duration::from_millis(150))
        }

        fn with_rebind_timeout(rebind_timeout: Duration) -> Self {
            let root = TempDir::new().unwrap();
            let cwd = TempDir::new().unwrap();
            let canonical_cwd = cwd.path().canonicalize().unwrap();
            let config = root.path().join("claude");
            let project = config.join("projects/project");
            std::fs::create_dir_all(&project).unwrap();
            let launch_session = SessionId::new_v4();
            let source = FileTranscriptSource::new(
                config.canonicalize().unwrap(),
                &canonical_cwd,
                launch_session,
            )
            .unwrap()
            .with_rebind_timings(rebind_timeout, Duration::from_millis(2));
            let source = Arc::new(source);
            Self {
                _root: root,
                _cwd: cwd,
                canonical_cwd,
                project,
                source,
                launch_session,
            }
        }

        /// Opens a transcript the way `/clear` does: a new file whose row 0 is a
        /// `mode` row carrying the new session id, written immediately.
        fn open_transcript(&self, session_id: SessionId) -> PathBuf {
            let path = self.project.join(format!("{session_id}.jsonl"));
            write_rows(
                &path,
                &[json!({
                    "type": "mode",
                    "sessionId": session_id,
                    "cwd": self.canonical_cwd,
                })],
            );
            path
        }

        fn typed_user(&self, session_id: SessionId) -> Value {
            semantic_row(
                "typed_user",
                Some(json!(session_id)),
                Some(json!(self.canonical_cwd)),
            )
        }

        fn transcript_path(&self, session_id: SessionId) -> PathBuf {
            self.project.join(format!("{session_id}.jsonl"))
        }

        /// Row 0 of the transcript `/clear` opens: MEASURED as a `mode` row
        /// carrying the rotated id, and the anchor the rebind matches on.
        fn rotation_anchor(&self, session_id: SessionId) -> Value {
            json!({
                "type": "mode",
                "sessionId": session_id,
                "cwd": self.canonical_cwd,
            })
        }

        /// The whole preamble a slash command writes, MEASURED verbatim on
        /// Claude Code 2.1.220 across 61 post-`/clear` transcripts in
        /// `~/.claude/projects/-private-tmp-clearprobe-cwd`: `mode`,
        /// `file-history-snapshot`, the meta caveat `user` row, the
        /// command-echo `user` row, `system`/`local_command`, and the
        /// `last-prompt` marker that lands once nothing follows.
        ///
        /// `command_name` is what makes this fixture load-bearing rather than
        /// decorative: every other observable is identical whichever command the
        /// composer executed, so this row is the only place the difference
        /// exists at all.
        fn cleared_preamble(&self, session_id: SessionId, command_name: &str) -> Vec<Value> {
            let caveat_uuid = SessionId::new_v4();
            let echo_uuid = SessionId::new_v4();
            let stdout_uuid = SessionId::new_v4();
            vec![
                self.rotation_anchor(session_id),
                json!({
                    "type": "file-history-snapshot",
                    "messageId": echo_uuid,
                    "snapshot": {"messageId": echo_uuid, "trackedFileBackups": {}},
                    "isSnapshotUpdate": false,
                }),
                json!({
                    "parentUuid": null,
                    "isSidechain": false,
                    "type": "user",
                    "message": {
                        "role": "user",
                        "content": "<local-command-caveat>Caveat: The messages below were generated by the user while running local commands. DO NOT respond to these messages or otherwise consider them in your response unless the user explicitly asks you to.</local-command-caveat>",
                    },
                    "isMeta": true,
                    "uuid": caveat_uuid,
                    "cwd": self.canonical_cwd,
                    "sessionId": session_id,
                }),
                json!({
                    "parentUuid": caveat_uuid,
                    "isSidechain": false,
                    "type": "user",
                    "message": {
                        "role": "user",
                        "content": format!(
                            "<command-name>{command_name}</command-name>\n            <command-message>{}</command-message>\n            <command-args></command-args>",
                            command_name.trim_start_matches('/'),
                        ),
                    },
                    "uuid": echo_uuid,
                    "cwd": self.canonical_cwd,
                    "sessionId": session_id,
                }),
                json!({
                    "parentUuid": echo_uuid,
                    "isSidechain": false,
                    "type": "system",
                    "subtype": "local_command",
                    "content": "<local-command-stdout></local-command-stdout>",
                    "level": "info",
                    "uuid": stdout_uuid,
                    "isMeta": false,
                    "cwd": self.canonical_cwd,
                    "sessionId": session_id,
                }),
                json!({
                    "type": "last-prompt",
                    "leafUuid": stdout_uuid,
                    "sessionId": session_id,
                }),
            ]
        }

        /// The preamble `/clear` itself writes.
        fn clear_preamble(&self, session_id: SessionId) -> Vec<Value> {
            self.cleared_preamble(session_id, CLEAR_COMMAND_NAME)
        }

        /// A terminal whose single Enter does on disk what `/clear` does in the
        /// TUI. Everything before Enter is the real injection path.
        fn clearing_terminal(
            &self,
            rotate: impl Fn() + Send + 'static,
        ) -> (RmuxTerminalControl, FakeTerminalHandle) {
            let (control, handle) = clear_control(Duration::from_millis(500));
            handle.on_enter(rotate);
            (control, handle)
        }
    }

    #[tokio::test]
    async fn the_armed_session_id_keeps_the_tail_on_its_own_transcript() {
        let fixture = RotationFixture::new();
        let launch_transcript = fixture.open_transcript(fixture.launch_session);
        let rotated_session = SessionId::new_v4();
        let rotated_transcript = fixture.open_transcript(rotated_session);

        let arm = fixture
            .source
            .arm_at_eof(fixture.launch_session)
            .await
            .unwrap();
        assert_eq!(
            arm.position.offset,
            std::fs::metadata(&launch_transcript).unwrap().len()
        );

        // Bytes land in both files. A neighbouring transcript is not evidence
        // about this turn no matter how recently it was written.
        append_rows(
            &launch_transcript,
            &[fixture.typed_user(fixture.launch_session)],
        );
        append_rows(&rotated_transcript, &[fixture.typed_user(rotated_session)]);
        let batch = fixture
            .source
            .poll(fixture.launch_session, &arm.position)
            .await
            .unwrap();
        assert_eq!(batch.rows.len(), 1);
        assert_eq!(
            batch.position.offset,
            std::fs::metadata(&launch_transcript).unwrap().len()
        );
        assert!(batch.drain.at_eof);
    }

    #[tokio::test]
    async fn rearming_under_a_rotated_session_id_follows_the_new_transcript() {
        let fixture = RotationFixture::new();
        let launch_transcript = fixture.open_transcript(fixture.launch_session);
        // Leave the abandoned file longer than the rotated one, so a cursor that
        // survived the rotation shows up as an offset and not merely as a row.
        for _ in 0..3 {
            append_rows(
                &launch_transcript,
                &[fixture.typed_user(fixture.launch_session)],
            );
        }
        let launch_arm = fixture
            .source
            .arm_at_eof(fixture.launch_session)
            .await
            .unwrap();

        let rotated_session = SessionId::new_v4();
        let rotated_transcript = fixture.open_transcript(rotated_session);
        let rotated_arm = fixture.source.arm_at_eof(rotated_session).await.unwrap();
        assert_eq!(
            rotated_arm.position.offset,
            std::fs::metadata(&rotated_transcript).unwrap().len(),
            "the boundary is the rotated file's EOF, not the abandoned file's"
        );
        assert!(rotated_arm.position.offset < launch_arm.position.offset);

        append_rows(
            &launch_transcript,
            &[fixture.typed_user(fixture.launch_session)],
        );
        append_rows(&rotated_transcript, &[fixture.typed_user(rotated_session)]);
        let batch = fixture
            .source
            .poll(rotated_session, &rotated_arm.position)
            .await
            .unwrap();
        assert_eq!(batch.rows.len(), 1);
        assert_eq!(
            batch.position.offset,
            std::fs::metadata(&rotated_transcript).unwrap().len()
        );
        assert!(
            batch.drain.at_eof,
            "drain evidence is measured against the rotated transcript"
        );
    }

    #[tokio::test]
    async fn row_identity_is_judged_against_the_armed_session_not_the_launch_one() {
        let fixture = RotationFixture::new();
        fixture.open_transcript(fixture.launch_session);
        let rotated_session = SessionId::new_v4();
        let rotated_transcript = fixture.open_transcript(rotated_session);
        let arm = fixture.source.arm_at_eof(rotated_session).await.unwrap();

        // The id this source was constructed with stopped being the authority
        // the moment the tail was armed elsewhere: a row still carrying it is a
        // foreign row now, and admitting one would attribute another session's
        // work to this turn.
        append_rows(
            &rotated_transcript,
            &[fixture.typed_user(fixture.launch_session)],
        );
        let error = fixture
            .source
            .poll(rotated_session, &arm.position)
            .await
            .unwrap_err();
        assert_identity_error(&error, "typed_user", "session_id", "mismatch");
        assert_redacted(
            &error,
            &[
                &fixture.launch_session.to_string(),
                &rotated_session.to_string(),
            ],
        );
    }

    #[tokio::test]
    async fn polling_under_an_unarmed_session_id_refuses_and_drops_the_tail() {
        let fixture = RotationFixture::new();
        let launch_transcript = fixture.open_transcript(fixture.launch_session);
        let arm = fixture
            .source
            .arm_at_eof(fixture.launch_session)
            .await
            .unwrap();
        append_rows(
            &launch_transcript,
            &[fixture.typed_user(fixture.launch_session)],
        );

        let rotated_session = SessionId::new_v4();
        let rotated_transcript = fixture.open_transcript(rotated_session);
        let error = fixture
            .source
            .poll(rotated_session, &arm.position)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::SchemaDrift);
        assert_eq!(error.details["field"], "session_id");
        assert_eq!(error.details["violation"], "rebind_requires_rearm");
        assert_redacted(
            &error,
            &[
                &fixture.launch_session.to_string(),
                &rotated_session.to_string(),
            ],
        );

        // The tail was dropped, not rebound in place, and the refusal is not one
        // poll deep. Nothing the caller can put in a position resumes it: not
        // the position minted against the abandoned file, and not any position
        // the rebound state might be holding. The rotated file already contains
        // a full turn -- an acknowledgement and a terminal assistant row -- so a
        // poll that got through here would not merely read early bytes, it would
        // finish this turn on a history written before the prompt was typed.
        append_rows(
            &rotated_transcript,
            &[
                fixture.typed_user(rotated_session),
                semantic_row(
                    "assistant",
                    Some(json!(rotated_session)),
                    Some(json!(fixture.canonical_cwd)),
                ),
            ],
        );
        for generation in 1..=4_u64 {
            for offset in [0, arm.position.offset] {
                let position = TranscriptPosition { generation, offset };
                let stale = fixture
                    .source
                    .poll(rotated_session, &position)
                    .await
                    .unwrap_err();
                assert_eq!(stale.code, ErrorCode::SchemaDrift);
                assert_eq!(
                    stale.details["violation"], "rebind_requires_rearm",
                    "position {position:?} resumed a tail with no arm boundary"
                );
            }
        }

        // The tail was dropped rather than merely refused, so the identity it
        // used to hold is gone too: the launch id and the position that was
        // minted for it no longer resume the abandoned file either, and the row
        // appended to it above stays unread. One poll under a foreign id ends
        // the old tail's authority, whichever id the caller meant.
        let abandoned = fixture
            .source
            .poll(fixture.launch_session, &arm.position)
            .await
            .unwrap_err();
        assert_eq!(
            abandoned.details["violation"], "rebind_requires_rearm",
            "the tail kept serving the identity it was armed on before the rebind"
        );

        // Going back through the arm is what re-establishes an authority
        // boundary, and it lands in the rotated file.
        let rearmed = fixture.source.arm_at_eof(rotated_session).await.unwrap();
        assert_eq!(
            rearmed.position.offset,
            std::fs::metadata(&rotated_transcript).unwrap().len()
        );
        let batch = fixture
            .source
            .poll(rotated_session, &rearmed.position)
            .await
            .unwrap();
        assert!(batch.rows.is_empty());
        assert!(batch.drain.at_eof);
    }

    struct TranscriptFixture {
        _root: TempDir,
        _cwd: TempDir,
        transcript: PathBuf,
        source: FileTranscriptSource,
        session_id: SessionId,
        pending_rows: Vec<Value>,
    }

    fn transcript_fixture(rows: impl FnOnce(SessionId, &Path) -> Vec<Value>) -> TranscriptFixture {
        let root = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let canonical_cwd = cwd.path().canonicalize().unwrap();
        let config = root.path().join("claude");
        let project = config.join("projects/project");
        std::fs::create_dir_all(&project).unwrap();
        let session_id = SessionId::new_v4();
        let transcript = project.join(format!("{session_id}.jsonl"));
        let transcript_rows = vec![json!({
            "type": "file-history-snapshot",
            "sessionId": session_id,
            "cwd": canonical_cwd,
        })];
        let pending_rows = rows(session_id, &canonical_cwd);
        write_rows(&transcript, &transcript_rows);
        let source =
            FileTranscriptSource::new(config.canonicalize().unwrap(), &canonical_cwd, session_id)
                .unwrap();
        TranscriptFixture {
            _root: root,
            _cwd: cwd,
            transcript,
            source,
            session_id,
            pending_rows,
        }
    }

    async fn poll_fixture(fixture: &TranscriptFixture) -> DriverResult<TranscriptBatch> {
        let arm = fixture.source.arm_at_eof(fixture.session_id).await?;
        append_rows(&fixture.transcript, &fixture.pending_rows);
        fixture.source.poll(fixture.session_id, &arm.position).await
    }

    fn semantic_row(row_kind: &str, session_id: Option<Value>, cwd: Option<Value>) -> Value {
        let mut row = match row_kind {
            "typed_user" => json!({
                "type": "user",
                "uuid": "typed-user-row",
                "parentUuid": null,
                "promptSource": "typed",
                "message": {"content": "safe prompt"},
            }),
            "assistant" => json!({
                "type": "assistant",
                "uuid": "assistant-row",
                "parentUuid": "typed-user-row",
                "message": {
                    "id": "assistant-message",
                    "model": "test-model",
                    "content": [{"type": "text", "text": "safe answer"}],
                    "stop_reason": "end_turn",
                    "usage": {"input_tokens": 1, "output_tokens": 1},
                },
            }),
            "attachment" => json!({
                "type": "attachment",
                "uuid": "attachment-row",
                "parentUuid": "typed-user-row",
                "attachment": {"type": "skill_listing"},
            }),
            "user_tool_results" => json!({
                "type": "user",
                "uuid": "tool-result-row",
                "parentUuid": "assistant-row",
                "message": {
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "tool-call",
                        "content": "safe result",
                    }],
                },
            }),
            "user_other" => json!({
                "type": "user",
                "uuid": "other-user-row",
                "parentUuid": "assistant-row",
                "message": {"content": "non-typed user content"},
            }),
            other => panic!("unsupported semantic row kind: {other}"),
        };
        let object = row.as_object_mut().unwrap();
        if let Some(session_id) = session_id {
            object.insert("sessionId".to_owned(), session_id);
        }
        if let Some(cwd) = cwd {
            object.insert("cwd".to_owned(), cwd);
        }
        row
    }

    fn write_rows(path: &Path, rows: &[Value]) {
        let mut file = std::fs::File::create(path).unwrap();
        for row in rows {
            writeln!(file, "{row}").unwrap();
        }
        file.flush().unwrap();
    }

    fn append_rows(path: &Path, rows: &[Value]) {
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        for row in rows {
            writeln!(file, "{row}").unwrap();
        }
        file.flush().unwrap();
    }

    fn assert_identity_error(error: &DriverFailure, row_kind: &str, field: &str, violation: &str) {
        assert_eq!(error.code, ErrorCode::SchemaDrift);
        assert_eq!(
            error.message,
            "semantic transcript row identity validation failed"
        );
        assert_eq!(error.details["row_kind"], row_kind);
        assert_eq!(error.details["field"], field);
        assert_eq!(error.details["violation"], violation);
        assert!(error.details["line"].as_u64().is_some());
    }

    fn assert_redacted(error: &DriverFailure, secrets: &[&str]) {
        let rendered = format!("{} {}", error.message, error.details);
        for secret in secrets {
            assert!(!rendered.contains(secret));
        }
    }

    // ---- the control channel, and the caller door it does not open ---------
    //
    // pmux has to be able to type `/clear` while a caller still cannot type any
    // slash command. The two facts are held apart structurally, not by a
    // stricter filter: caller text is data and is refused, while the control
    // command is a variant whose text is chosen in this file at compile time.

    /// Every shape a caller might reach for to get a leading solidus past the
    /// guard, including the whitespace forms Rust's `trim_start` recognizes but
    /// a byte-level prefix check would not, and the invisible ones it does not.
    ///
    /// The zero-width entries were once pinned as MUST-ACCEPT here, on the
    /// argument that the composer keys on U+002F in first position and these do
    /// not put one there. That argument assumed something never measured: that
    /// nothing strips them first. JS `String.prototype.trim` DOES strip U+FEFF,
    /// and Claude Code is a Node/Ink TUI, so `"\u{feff}/clear"` plausibly
    /// reaches its command detector as `/clear` — a caller-typed slash command.
    /// The guard now reads past every invisible format character before
    /// deciding, and the test says so.
    /// Every shape an invisible can take in front of a composer mode character.
    ///
    /// A measured list of LOOKALIKES, not a list of attempts: each is a
    /// character some reader on the far end discards before parsing, and the
    /// point of the set is that no member of it can put a mode character out of
    /// the guard's reach.
    const INVISIBLE_PREFIXES: &[&str] = &[
        "",
        " ",
        "\t",
        "\n",
        "\r\n",
        "\r",
        "  \t\n  ",
        "\u{a0}",              // NO-BREAK SPACE
        "\u{85}",              // NEXT LINE
        "\u{2003}",            // EM SPACE
        "\u{202f}",            // NARROW NO-BREAK SPACE
        "\u{3000}",            // IDEOGRAPHIC SPACE
        "\u{feff}",            // ZERO WIDTH NO-BREAK SPACE: stripped by JS `trim`
        "\u{200b}",            // ZERO WIDTH SPACE
        "\u{2060}",            // WORD JOINER
        "\u{ad}",              // SOFT HYPHEN
        "\u{200e}",            // LEFT-TO-RIGHT MARK
        "\u{202e}",            // RIGHT-TO-LEFT OVERRIDE
        "\u{feff} \u{200b}\t", // invisibles and whitespace interleaved
    ];

    /// The refused forms, DERIVED from the shipped mode set rather than typed
    /// out against one member of it.
    ///
    /// The list this replaced spelled `/clear` twenty-two times and said
    /// nothing about `!`, which is how a prompt that ran a shell command on the
    /// host passed a suite whose name promised no caller prompt could reach the
    /// composer's control surface. Adding a character to
    /// [`pseudomux_claude::COMPOSER_MODE_PREFIXES`] now adds 22 cases here.
    fn refused_composer_forms() -> Vec<String> {
        let mut forms = Vec::new();
        for prefix in pseudomux_claude::COMPOSER_MODE_PREFIXES {
            for invisible in INVISIBLE_PREFIXES {
                forms.push(format!("{invisible}{prefix}payload"));
            }
            forms.push(format!("{prefix}payload\nand more"));
            forms.push(format!("{prefix}{prefix}payload"));
            forms.push(prefix.to_string());
        }
        forms
    }

    /// Shapes that are NOT slash commands and must keep working. No reading of
    /// these puts U+002F in first position — not Rust's `trim_start`, not JS's
    /// `trim`, not the invisible-format rule the guard applies — and refusing
    /// them would break ordinary prompts (a pasted path, a diff, a quoted
    /// command, text carried out of a Windows-authored file) for a threat that
    /// does not exist.
    const ACCEPTED_NON_COMMAND_FORMS: &[&str] = &[
        "\u{2044}clear",        // FRACTION SLASH
        "\u{2215}clear",        // DIVISION SLASH
        "\u{ff0f}clear",        // FULLWIDTH SOLIDUS
        "\u{29f8}clear",        // BIG SOLIDUS
        "\u{feff}explain this", // a BOM ahead of ordinary text is ordinary text
        "\u{200b}explain this",
        "explain this:\n/clear",
        "explain this:\r\n/clear",
        "src/main.rs",
    ];

    fn ready_control(gate_timeout: Duration) -> (RmuxTerminalControl, FakeTerminalHandle) {
        let baseline = baseline_snapshot(1);
        let rendered = rendered_snapshot(2, "❯ typed", 8);
        control_for(
            [baseline.clone(), baseline],
            [rendered.clone(), rendered],
            Duration::ZERO,
            gate_timeout,
        )
    }

    /// [`ready_control`] whose post-paste frame RENDERS THE PROMPT it is about
    /// to be given, in the shape Claude Code 2.1.226 was measured to render one
    /// that fits on a row: `❯`, U+00A0, the text, cursor at its end.
    ///
    /// Derived from the prompt rather than written beside it. A fixed row was
    /// free to say anything at all while the render gate read geometry only, and
    /// this one said `❯ typed` under nine prompts none of which contain the
    /// word.
    fn ready_control_showing(
        prompt: &str,
        gate_timeout: Duration,
    ) -> (RmuxTerminalControl, FakeTerminalHandle) {
        let normalized = pseudomux_claude::normalize_prompt(prompt);
        // One row per prompt line, with the measured two-cell gutter under the
        // first. A screen showing only the first line of a multi-line prompt is
        // a TRUNCATED composer, which is the thing the render proof refuses.
        let rows: Vec<String> = normalized
            .split('\n')
            .enumerate()
            .map(|(index, line)| {
                if index == 0 {
                    format!("❯\u{a0}{line}")
                } else {
                    format!("  {line}")
                }
            })
            .collect();
        let rendered = composer_snapshot(2, &rows.iter().map(String::as_str).collect::<Vec<_>>());
        let baseline = baseline_snapshot(1);
        control_for(
            [baseline.clone(), baseline],
            [rendered.clone(), rendered],
            Duration::ZERO,
            gate_timeout,
        )
    }

    /// A control-channel terminal whose post-paste frame is a real capture of
    /// Claude Code's command menu.
    ///
    /// The double's screen is a capture rather than an invention because the
    /// pre-Enter proof is a statement about a measured rendering: a fixture that
    /// merely satisfied the predicate would test the predicate against itself.
    fn clear_control_showing(
        screen: StyledScreen,
        gate_timeout: Duration,
    ) -> (RmuxTerminalControl, FakeTerminalHandle) {
        let baseline = baseline_snapshot(1);
        let rendered = screen.to_terminal_snapshot();
        let (control, handle) = control_for(
            [baseline.clone(), baseline],
            [rendered.clone(), rendered],
            Duration::ZERO,
            gate_timeout,
        );
        handle.serve_styled([screen]);
        (control, handle)
    }

    fn clear_control(gate_timeout: Duration) -> (RmuxTerminalControl, FakeTerminalHandle) {
        clear_control_showing(measured_clear_screen(), gate_timeout)
    }

    #[tokio::test]
    async fn no_caller_prompt_can_put_a_composer_mode_command_into_the_terminal() {
        for attempt in refused_composer_forms() {
            let attempt = attempt.as_str();
            let error = validate_prompt(attempt).unwrap_err();
            assert_eq!(
                error.code,
                ErrorCode::UnsupportedFeature,
                "prompt {attempt:?} must be refused as a composer mode command"
            );
            // The refusal is at the terminal adapter's own trust boundary, so it
            // is not enough that an error came back: nothing may have been
            // written. A guard that refused after pasting would have already
            // typed the command.
            let (control, handle) = ready_control(Duration::from_millis(30));
            assert_eq!(
                submit(&control, attempt).await.unwrap_err().code,
                error.code
            );
            assert_eq!(
                handle.counts(),
                (0, 0),
                "prompt {attempt:?} reached the terminal"
            );
            assert!(handle.pasted_text().is_empty());
        }

        for attempt in ACCEPTED_NON_COMMAND_FORMS {
            let (control, handle) = ready_control_showing(attempt, Duration::from_millis(60));
            submit(&control, attempt)
                .await
                .unwrap_or_else(|error| panic!("prompt {attempt:?} was refused: {error:?}"));
            let pasted = handle.pasted_text();
            assert_eq!(pasted.len(), 1);
            // The property is about the byte stream Claude parses, not about
            // which branch of the guard ran. It is stated under the widest
            // reading available, because which characters the composer looks
            // past is its choice and not ours: even after every invisible
            // format character is discarded, the first character it could see
            // is in no mode set pmux ships.
            assert_eq!(
                pseudomux_claude::composer_refusal(&pasted[0]),
                None,
                "prompt {attempt:?} handed the composer a mode character"
            );
            // And the guard reads past those characters without removing them.
            // Rewriting caller bytes would change the prompt Claude records,
            // which is the text the typed-prompt acknowledgement is matched
            // against -- a silent `UnexpectedTypedPrompt` on every BOM.
            assert_eq!(
                pasted[0],
                pseudomux_claude::normalize_prompt(attempt),
                "prompt {attempt:?} was rewritten on its way to the composer"
            );
        }
    }

    #[tokio::test]
    async fn the_control_channel_types_the_one_literal_a_caller_may_never_type() {
        let (control, handle) = clear_control(Duration::from_millis(60));
        control
            .type_control_command(ControlCommand::Clear, test_deadline())
            .await
            .unwrap();
        assert_eq!(handle.counts(), (1, 1));
        assert_eq!(handle.pasted_text(), vec![ControlCommand::Clear.literal()]);

        // Same bytes, same terminal shape, caller door: refused, and nothing is
        // written. The privilege is the route, not the text.
        let (control, handle) = ready_control(Duration::from_millis(30));
        assert_eq!(
            submit(&control, ControlCommand::Clear.literal())
                .await
                .unwrap_err()
                .code,
            ErrorCode::UnsupportedFeature
        );
        assert_eq!(handle.counts(), (0, 0));
    }

    #[tokio::test]
    async fn a_clear_binds_the_transcript_that_appeared_and_names_the_one_it_abandoned() {
        let fixture = RotationFixture::new();
        let launch_transcript = fixture.open_transcript(fixture.launch_session);
        let launch_len = std::fs::metadata(&launch_transcript).unwrap().len();
        let rotated_session = SessionId::new_v4();
        let rotated_path = fixture.transcript_path(rotated_session);
        let preamble = fixture.clear_preamble(rotated_session);
        let written_path = rotated_path.clone();
        let (control, handle) =
            fixture.clearing_terminal(move || write_rows(&written_path, &preamble));
        // A turn that was already in flight when the cell was cleared: its
        // position was minted against the file the clear is about to abandon.
        let in_flight = fixture
            .source
            .arm_at_eof(fixture.launch_session)
            .await
            .unwrap();

        let rebound = clear_and_rebind(
            &control,
            &fixture.source,
            fixture.launch_session,
            test_deadline(),
        )
        .await
        .unwrap();
        assert_eq!(rebound, rotated_session);
        assert_eq!(handle.counts(), (1, 1));
        // The abandoned transcript is untouched -- same file, same length -- so
        // nothing on disk would have told the tail to stop reading it.
        assert_eq!(
            std::fs::metadata(&launch_transcript).unwrap().len(),
            launch_len
        );

        let error = fixture
            .source
            .arm_at_eof(fixture.launch_session)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::TranscriptUnavailable);
        assert_eq!(error.details["violation"], "transcript_rotated");
        assert_eq!(
            error.details["abandoned_session_id"],
            fixture.launch_session.to_string()
        );
        assert_eq!(
            error.details["rebound_session_id"],
            rotated_session.to_string()
        );

        // The in-flight turn's poll is the entry point the pool actually hits,
        // and it is the one that must not come back as a bare timeout: the same
        // rotation, named the same way, plus the observation behind the
        // diagnosis -- this file has not grown, and here is how long it has been
        // sitting at this offset.
        let straddled = fixture
            .source
            .poll(fixture.launch_session, &in_flight.position)
            .await
            .unwrap_err();
        assert_eq!(straddled.code, ErrorCode::TranscriptUnavailable);
        assert_eq!(straddled.details["violation"], "transcript_rotated");
        assert_eq!(
            straddled.details["rebound_session_id"],
            rotated_session.to_string()
        );
        assert!(straddled.details["bound_transcript_quiet_ms"].is_number());
        assert_eq!(
            straddled.details["bound_transcript_offset"],
            json!(in_flight.position.offset)
        );

        // The rebind hands back an id, not a file. Arming it is the ordinary
        // path, and it is what re-establishes an authority boundary.
        let arm = fixture.source.arm_at_eof(rotated_session).await.unwrap();
        append_rows(&rotated_path, &[fixture.typed_user(rotated_session)]);
        let batch = fixture
            .source
            .poll(rotated_session, &arm.position)
            .await
            .unwrap();
        assert_eq!(batch.rows.len(), 1);
    }

    #[tokio::test]
    async fn a_clear_that_leaves_two_candidates_refuses_rather_than_choosing() {
        let fixture = RotationFixture::new();
        fixture.open_transcript(fixture.launch_session);
        let first = fixture.transcript_path(SessionId::new_v4());
        let second = fixture.transcript_path(SessionId::new_v4());
        let anchors = [
            (
                first.clone(),
                fixture.rotation_anchor(
                    first
                        .file_stem()
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .parse()
                        .unwrap(),
                ),
            ),
            (
                second.clone(),
                fixture.rotation_anchor(
                    second
                        .file_stem()
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .parse()
                        .unwrap(),
                ),
            ),
        ];
        let (control, _) = fixture.clearing_terminal(move || {
            for (path, anchor) in &anchors {
                write_rows(path, &[anchor.clone()]);
            }
        });

        // Two files, both anchored, both new. Newest-first or mtime ordering
        // would answer this; a completion authority does not get to.
        let error = clear_and_rebind(
            &control,
            &fixture.source,
            fixture.launch_session,
            test_deadline(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::SchemaDrift);
        assert_eq!(error.details["violation"], "clear_rebind_ambiguous");
        assert_eq!(error.details["candidates"], json!(2));

        // A refused rebind still abandoned the transcript, so the old id has to
        // stop being armable -- with the reason attached.
        let stranded = fixture
            .source
            .arm_at_eof(fixture.launch_session)
            .await
            .unwrap_err();
        assert_eq!(stranded.code, ErrorCode::TranscriptUnavailable);
        assert_eq!(stranded.details["violation"], "clear_rebind_failed");
        assert_eq!(stranded.details["rebound_session_id"], Value::Null);
    }

    #[tokio::test]
    async fn a_transcript_carrying_an_id_this_directory_already_knew_is_not_the_successor() {
        // MEASURED: `/clear` rotates to an id that has never had a transcript
        // here, and writes it into row 0 of the file it opens. A file that
        // appears carrying an id this directory already knows is therefore
        // something else -- a copy, a sibling tool's write, a Claude whose
        // `/clear` no longer works the way pmux models it. Binding to it would
        // arm the next turn on a history written before the clear, which is a
        // history that can acknowledge and finish a prompt nobody has answered
        // yet. Refusing costs one instance; binding costs the guarantee.
        for anchor_is_the_abandoned_id in [true, false] {
            let fixture = RotationFixture::new();
            fixture.open_transcript(fixture.launch_session);
            let sibling = SessionId::new_v4();
            fixture.open_transcript(sibling);
            let stale = if anchor_is_the_abandoned_id {
                fixture.launch_session
            } else {
                sibling
            };
            let appeared = fixture.transcript_path(SessionId::new_v4());
            let anchor = fixture.rotation_anchor(stale);
            let (control, _) = fixture.clearing_terminal(move || {
                write_rows(&appeared, &[anchor.clone()]);
            });

            let error = clear_and_rebind(
                &control,
                &fixture.source,
                fixture.launch_session,
                test_deadline(),
            )
            .await
            .unwrap_err();
            assert_eq!(error.code, ErrorCode::SchemaDrift);
            assert_eq!(error.details["violation"], "clear_rebind_anchor_not_new");

            // The command was still typed, so the abandoned id is still
            // abandoned. A refusal that left it armable would hand the next turn
            // a file that will never grow again.
            let stranded = fixture
                .source
                .arm_at_eof(fixture.launch_session)
                .await
                .unwrap_err();
            assert_eq!(stranded.details["violation"], "clear_rebind_failed");
        }
    }

    #[tokio::test]
    async fn a_clear_whose_transcript_never_appears_names_the_stalled_tail() {
        let fixture = RotationFixture::with_rebind_timeout(Duration::from_millis(60));
        let launch_transcript = fixture.open_transcript(fixture.launch_session);
        let arm = fixture
            .source
            .arm_at_eof(fixture.launch_session)
            .await
            .unwrap();
        // Nothing rotates. This is the shape an operator actually hits: the
        // command went in, and the evidence never showed up.
        let (control, _) = fixture.clearing_terminal(|| {});

        let error = clear_and_rebind(
            &control,
            &fixture.source,
            fixture.launch_session,
            test_deadline(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::TranscriptUnavailable);
        assert_eq!(error.details["violation"], "clear_rebind_not_observed");
        assert!(error.details["waited_ms"].as_u64().unwrap() >= 60);
        assert_eq!(error.details["transcripts_before"], json!(1));
        assert_eq!(error.details["transcripts_after"], json!(1));

        // The still-armed tail is the one an in-flight turn is holding. Its
        // refusal has to carry the observation behind the diagnosis: this file
        // stopped growing, and here is how long ago.
        append_rows(
            &launch_transcript,
            &[fixture.typed_user(fixture.launch_session)],
        );
        let stalled = fixture
            .source
            .poll(fixture.launch_session, &arm.position)
            .await
            .unwrap_err();
        assert_eq!(stalled.code, ErrorCode::TranscriptUnavailable);
        assert_eq!(stalled.details["violation"], "clear_rebind_failed");
        assert_eq!(
            stalled.details["abandoned_session_id"],
            fixture.launch_session.to_string()
        );
        assert!(stalled.details["bound_transcript_quiet_ms"].is_number());
        assert_eq!(
            stalled.details["bound_transcript_offset"],
            json!(arm.position.offset)
        );
        assert_ne!(
            stalled.code,
            ErrorCode::TurnTimeout,
            "a stranded tail must not be reported as an ordinary deadline"
        );
    }

    /// The two refusals that provably typed nothing, and the mark that keeps the
    /// actor from quarantining them.
    ///
    /// Neither needs malformed input. A `clear_session` issued before the
    /// session's first turn finds no transcript to watch -- the natural order for
    /// a pool checking an instance out, since Claude creates the file lazily --
    /// and a `clear_session` whose deadline has already passed is refused by the
    /// input gate before a byte is written. In both cases the bound transcript is
    /// untouched, the instance is exactly as it was, and the session is still
    /// provable; without the mark the actor turned each into a permanently
    /// `Tainted` cell holding a live Claude process.
    ///
    /// The assertion is not only the flag: nothing was typed, and no abandonment
    /// was recorded, so the tail is still armable on the id it was bound to.
    #[tokio::test]
    async fn a_clear_refused_before_submission_says_so_and_abandons_nothing() {
        // No transcript yet: there is nothing to watch for a rotation.
        let fixture = RotationFixture::new();
        let (control, handle) = ready_control(Duration::from_millis(60));
        let error = clear_and_rebind(
            &control,
            &fixture.source,
            fixture.launch_session,
            test_deadline(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::TranscriptUnavailable);
        assert!(
            clear_was_not_submitted(&error.details),
            "a refusal raised before the command is typed must say so: {error:?}"
        );
        assert_eq!(handle.counts(), (0, 0), "nothing may have been typed");

        // A deadline that has already passed: refused inside the input gate,
        // after the rotation watch and before any keystroke.
        let fixture = RotationFixture::new();
        fixture.open_transcript(fixture.launch_session);
        let (control, handle) = ready_control(Duration::from_millis(60));
        let error = clear_and_rebind(
            &control,
            &fixture.source,
            fixture.launch_session,
            now_unix_ms().unwrap() - 1,
        )
        .await
        .unwrap_err();
        assert!(
            clear_was_not_submitted(&error.details),
            "a past-deadline clear must say it typed nothing: {error:?}"
        );
        assert_eq!(handle.counts(), (0, 0), "nothing may have been typed");
        // And the session is exactly as it was: the tail still arms on the id it
        // was bound to, which is what "still coherent" means here.
        fixture
            .source
            .arm_at_eof(fixture.launch_session)
            .await
            .expect("a clear that typed nothing must leave the tail armable");

        // The contrast that makes the mark mean something: a refusal AFTER Enter
        // was attempted carries no such claim, so the actor still quarantines.
        let fixture = RotationFixture::with_rebind_timeout(Duration::from_millis(60));
        fixture.open_transcript(fixture.launch_session);
        let (control, _) = fixture.clearing_terminal(|| {});
        let error = clear_and_rebind(
            &control,
            &fixture.source,
            fixture.launch_session,
            test_deadline(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.details["violation"], "clear_rebind_not_observed");
        assert!(
            !clear_was_not_submitted(&error.details),
            "a clear that reached Enter must not claim it typed nothing: {error:?}"
        );
    }

    #[tokio::test]
    async fn a_rotation_anchor_that_is_not_a_mode_row_is_quarantined_not_bound() {
        let fixture = RotationFixture::with_rebind_timeout(Duration::from_millis(60));
        fixture.open_transcript(fixture.launch_session);
        let rotated_session = SessionId::new_v4();
        let rotated_path = fixture.transcript_path(rotated_session);
        let impostor = fixture.typed_user(rotated_session);
        let written_path = rotated_path.clone();
        let (control, _) =
            fixture.clearing_terminal(move || write_rows(&written_path, &[impostor.clone()]));

        // Row 0 being a `mode` row is a version-observed fact. A file that
        // appears without it means the model of `/clear` no longer matches the
        // installed Claude, which is the moment to stop, not to improvise.
        let error = clear_and_rebind(
            &control,
            &fixture.source,
            fixture.launch_session,
            test_deadline(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::SchemaDrift);
        assert_eq!(
            error.details["violation"],
            "clear_rebind_anchor_unrecognized"
        );
        assert_eq!(error.details["reason"], "unexpected_row_type");
    }

    #[tokio::test]
    async fn a_half_written_anchor_is_a_not_yet_answer_and_never_a_binding() {
        let fixture = RotationFixture::with_rebind_timeout(Duration::from_millis(80));
        fixture.open_transcript(fixture.launch_session);
        let rotated_session = SessionId::new_v4();
        let rotated_path = fixture.transcript_path(rotated_session);
        let partial = fixture.rotation_anchor(rotated_session).to_string();
        let written_path = rotated_path.clone();
        // The file appears +39ms after Enter, but appearing is not flushing: a
        // first line without its newline is an incomplete JSONL record, and the
        // same complete-record discipline the tailer uses applies here.
        let (control, _) = fixture.clearing_terminal(move || {
            std::fs::write(&written_path, &partial[..partial.len() - 4]).unwrap();
        });

        let error = clear_and_rebind(
            &control,
            &fixture.source,
            fixture.launch_session,
            test_deadline(),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.details["violation"], "clear_rebind_not_observed",
            "a partial row 0 must expire as unresolved, never bind and never claim drift"
        );

        // Completed, the same file resolves -- the rule is about the record, not
        // about the file's existence.
        let fixture = RotationFixture::with_rebind_timeout(Duration::from_millis(500));
        fixture.open_transcript(fixture.launch_session);
        let rotated_session = SessionId::new_v4();
        let rotated_path = fixture.transcript_path(rotated_session);
        let preamble = fixture.clear_preamble(rotated_session);
        let written_path = rotated_path.clone();
        let (control, _) = fixture.clearing_terminal(move || {
            let anchor = preamble[0].to_string();
            std::fs::write(&written_path, &anchor[..anchor.len() - 4]).unwrap();
            let finished = written_path.clone();
            let completed = preamble.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(30));
                write_rows(&finished, &completed);
            });
        });
        assert_eq!(
            clear_and_rebind(
                &control,
                &fixture.source,
                fixture.launch_session,
                test_deadline(),
            )
            .await
            .unwrap(),
            rotated_session
        );
    }

    #[tokio::test]
    async fn a_clear_that_never_reached_enter_leaves_the_session_alone() {
        let fixture = RotationFixture::new();
        let launch_transcript = fixture.open_transcript(fixture.launch_session);
        let (control, handle) = fixture.clearing_terminal(|| {});
        handle.state.lock().unwrap().paste_error = true;

        let error = clear_and_rebind(
            &control,
            &fixture.source,
            fixture.launch_session,
            test_deadline(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::PromptNotAcknowledged);
        assert_eq!(handle.counts(), (1, 0));

        // Nothing was submitted, so nothing was abandoned. Poisoning here would
        // refuse a session that is still exactly what it was.
        let arm = fixture
            .source
            .arm_at_eof(fixture.launch_session)
            .await
            .unwrap();
        append_rows(
            &launch_transcript,
            &[fixture.typed_user(fixture.launch_session)],
        );
        assert_eq!(
            fixture
                .source
                .poll(fixture.launch_session, &arm.position)
                .await
                .unwrap()
                .rows
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn an_ambiguous_enter_is_resolved_by_the_transcript_not_by_the_terminal() {
        let fixture = RotationFixture::new();
        fixture.open_transcript(fixture.launch_session);
        let rotated_session = SessionId::new_v4();
        let rotated_path = fixture.transcript_path(rotated_session);
        let preamble = fixture.clear_preamble(rotated_session);
        let (control, handle) =
            fixture.clearing_terminal(move || write_rows(&rotated_path, &preamble));
        // Enter went out and its acknowledgement was lost. The terminal cannot
        // say whether the command landed; the transcript can, and it is the only
        // authority in this codebase that is allowed to.
        handle.state.lock().unwrap().enter_error = true;

        assert_eq!(
            clear_and_rebind(
                &control,
                &fixture.source,
                fixture.launch_session,
                test_deadline(),
            )
            .await
            .unwrap(),
            rotated_session
        );
    }

    // ---- assert-empty: what a cleared instance has to prove ----------------
    //
    // Every observable the rebind already checks is identical whichever slash
    // command the fuzzy composer executed: a new file appears, its row 0 is a
    // `mode` row, and the id it carries is new and unseen. So the anchor check
    // cannot distinguish `/clear` from `/model`, and the only thing in the
    // system that can is Claude's own command-echo row. These tests are written
    // around that: the positive cases exist so a too-strict predicate fails
    // loudly, and the negative cases exist so a too-weak one does.

    /// Runs a whole clear whose Enter writes `rows` as the successor transcript.
    async fn clear_writing(
        fixture: &RotationFixture,
        rotated_session: SessionId,
        rows: Vec<Value>,
    ) -> DriverResult<SessionId> {
        fixture.open_transcript(fixture.launch_session);
        let rotated_path = fixture.transcript_path(rotated_session);
        let (control, _handle) =
            fixture.clearing_terminal(move || write_rows(&rotated_path, &rows));
        clear_and_rebind(
            &control,
            &fixture.source,
            fixture.launch_session,
            test_deadline(),
        )
        .await
    }

    fn assert_assert_empty_refusal(error: &DriverFailure, reason: &str) {
        assert_eq!(error.code, ErrorCode::SchemaDrift);
        assert_eq!(error.details["violation"], "assert_empty_refused");
        assert_eq!(error.details["reason"], reason);
    }

    /// Every [`AssertEmptyRefusal`] is in `ALL` exactly once, spells one
    /// `reason`, and round-trips through it.
    ///
    /// `index` carries no wildcard, so a fifteenth refusal stops this file
    /// compiling until somebody puts it in `ALL` -- and `from_reason` is
    /// DERIVED from `reason`, so the string a site writes and the string a
    /// reader parses are one string by construction rather than by agreement.
    #[test]
    fn every_assert_empty_refusal_is_in_all_exactly_once_and_round_trips() {
        const fn index(refusal: AssertEmptyRefusal) -> usize {
            match refusal {
                AssertEmptyRefusal::RowBudgetExceeded => 0,
                AssertEmptyRefusal::ByteBudgetExceeded => 1,
                AssertEmptyRefusal::UnparseableRow => 2,
                AssertEmptyRefusal::UnexpectedMetadataRecord => 3,
                AssertEmptyRefusal::MetadataPromptPresent => 4,
                AssertEmptyRefusal::TurnMarkerPresent => 5,
                AssertEmptyRefusal::UnexpectedSystemSubtype => 6,
                AssertEmptyRefusal::UnexpectedUserRow => 7,
                AssertEmptyRefusal::WrongLocalCommand => 8,
                AssertEmptyRefusal::SemanticRowPresent => 9,
                AssertEmptyRefusal::UnknownRow => 10,
                AssertEmptyRefusal::PreambleNotSettled => 11,
                AssertEmptyRefusal::ClearCommandMissing => 12,
                AssertEmptyRefusal::UnexpectedClearEcho => 13,
            }
        }
        let mut seen: Vec<usize> = AssertEmptyRefusal::ALL.into_iter().map(index).collect();
        seen.sort_unstable();
        assert_eq!(
            seen,
            (0..AssertEmptyRefusal::ALL.len()).collect::<Vec<_>>(),
            "AssertEmptyRefusal::ALL is not every variant, exactly once"
        );
        let reasons: std::collections::BTreeSet<_> = AssertEmptyRefusal::ALL
            .iter()
            .map(|refusal| refusal.reason())
            .collect();
        assert_eq!(
            reasons.len(),
            AssertEmptyRefusal::ALL.len(),
            "two refusals share a reason, so a reader cannot tell them apart"
        );
        for refusal in AssertEmptyRefusal::ALL {
            assert_eq!(
                AssertEmptyRefusal::from_reason(refusal.reason()),
                Some(refusal)
            );
        }
        assert_eq!(AssertEmptyRefusal::from_reason("not_a_reason"), None);
    }

    /// **Re-promotion trigger 4.** A refusal that says the INSTALLED CLAUDE's
    /// post-`/clear` preamble moved publishes the trigger and is readable back
    /// as one; a refusal that says THIS INSTANCE leaked does neither.
    ///
    /// Asserted over the real refusal the real helper builds, for every
    /// variant, so there is no way to add a refusal reason that is silently
    /// neither. That is the whole defect this replaced: the reader was
    /// `reason == "wrong_local_command"` -- ONE literal -- under a doc that
    /// claimed the general thing, and six other reasons meaning the identical
    /// thing quarantined one instance each while the pool minted replacements
    /// into the same drift.
    #[test]
    fn a_preamble_that_moved_is_a_repromotion_trigger_and_a_leak_is_not() {
        let drifted: Vec<&str> = AssertEmptyRefusal::ALL
            .iter()
            .filter(|refusal| refusal.is_a_version_drift_signal())
            .map(|refusal| refusal.reason())
            .collect();
        assert!(
            drifted.len() > 1,
            "the classification is back to one literal, which is the defect: {drifted:?}"
        );
        assert!(
            drifted.contains(&"wrong_local_command"),
            "the one reason that always halted must still halt: {drifted:?}"
        );
        assert!(
            AssertEmptyRefusal::ALL
                .iter()
                .any(|refusal| !refusal.is_a_version_drift_signal()),
            "every reason classified as drift halts the pool on a leak, which is the opposite \
             defect"
        );

        let trigger = crate::compatibility::RepromotionTrigger::ClearScreenOrPreambleMismatch.id();
        for refusal in AssertEmptyRefusal::ALL {
            let error = assert_empty_row_refusal(refusal, "user_other", 3);
            assert_assert_empty_refusal(&error, refusal.reason());
            assert_eq!(
                error.details["repromotion_trigger"].as_str(),
                refusal.is_a_version_drift_signal().then_some(trigger),
                "{refusal:?} publishes the wrong trigger"
            );
            assert_eq!(
                clear_refusal_repromotion_trigger(&error.details),
                refusal
                    .is_a_version_drift_signal()
                    .then_some(refusal.reason()),
                "{refusal:?} reads back as the wrong trigger"
            );
        }
    }

    /// The non-vacuity anchor. MEASURED bytes in, `Ok` out.
    ///
    /// Without this, any predicate strict enough to refuse everything would
    /// satisfy every negative test below while making Path B unusable.
    #[tokio::test]
    async fn a_cleared_transcript_carrying_the_measured_preamble_rebinds() {
        let fixture = RotationFixture::new();
        let rotated_session = SessionId::new_v4();
        let preamble = fixture.clear_preamble(rotated_session);
        assert_eq!(preamble.len(), 6, "the measured preamble is six rows");
        assert_eq!(
            clear_writing(&fixture, rotated_session, preamble)
                .await
                .unwrap(),
            rotated_session
        );
    }

    /// The same predicate over a *launch* preamble, which MEASURED carries no
    /// `/clear` echo. One function covers both ways an instance can be fresh;
    /// each caller asserts the one extra bit it is entitled to.
    #[test]
    fn the_measured_launch_preamble_is_accepted_and_carries_no_clear_echo() {
        let fixture = RotationFixture::new();
        let session_id = fixture.launch_session;
        write_rows(
            &fixture.transcript_path(session_id),
            &[
                json!({"type": "mode", "sessionId": session_id, "mode": "normal"}),
                json!({"type": "permission-mode", "sessionId": session_id}),
                json!({"type": "bridge-session", "sessionId": session_id}),
                json!({
                    "type": "file-history-snapshot",
                    "messageId": "launch-snapshot",
                    "snapshot": {"trackedFileBackups": {}},
                }),
                // One row must carry both id and cwd or the locator will not
                // corroborate the file at all.
                json!({
                    "type": "user",
                    "isMeta": true,
                    "uuid": "launch-caveat",
                    "parentUuid": null,
                    "message": {"role": "user", "content": "<local-command-caveat>x</local-command-caveat>"},
                    "cwd": fixture.canonical_cwd,
                    "sessionId": session_id,
                }),
            ],
        );
        let proof = fixture
            .source
            .assert_empty_at(session_id)
            .unwrap()
            .expect("a located transcript yields a proof");
        assert_eq!(proof.rows, 5);
        assert!(!proof.clear_command_seen);
        assert_eq!(proof.pending_bytes, 0);
        // And the launch caller's extra assertion holds.
        fixture.source.prove_empty_at_launch(session_id).unwrap();
    }

    /// A transcript that does not exist has served no work, so the launch caller
    /// passes it. Absence is not evidence of leakage, and Claude creates the
    /// file lazily.
    #[test]
    fn a_launch_with_no_transcript_yet_is_not_a_refusal() {
        let fixture = RotationFixture::new();
        assert!(
            fixture
                .source
                .assert_empty_at(fixture.launch_session)
                .unwrap()
                .is_none()
        );
        fixture
            .source
            .prove_empty_at_launch(fixture.launch_session)
            .unwrap();
    }

    /// The threat, encoded. Everything the rebind already checks still passes --
    /// a new file, a `mode` row 0, a new and unseen session id -- and the clear
    /// is still refused, because Claude wrote down which command it ran.
    ///
    /// This test cannot be made to pass by any change to the anchor check.
    #[tokio::test]
    async fn a_successor_opened_by_a_different_slash_command_is_refused() {
        let fixture = RotationFixture::new();
        let rotated_session = SessionId::new_v4();
        let preamble = fixture.cleared_preamble(rotated_session, "/model");
        let error = clear_writing(&fixture, rotated_session, preamble)
            .await
            .unwrap_err();
        assert_assert_empty_refusal(&error, "wrong_local_command");
        assert_eq!(error.details["command_name"], "/model");
        assert_eq!(error.details["row_kind"], "user_other");

        // The refusal leaves no usable session behind: the abandonment was
        // recorded before the predicate ran, so the old id is already poisoned.
        let stranded = fixture
            .source
            .arm_at_eof(fixture.launch_session)
            .await
            .unwrap_err();
        assert_eq!(stranded.details["violation"], "transcript_rotated");
    }

    /// The 54-of-61 shape: a preamble that then served a turn. A clear whose
    /// successor already carries a prompt, a reply and a completion marker is
    /// leakage however it got there.
    #[tokio::test]
    async fn a_successor_transcript_that_is_not_empty_is_refused() {
        let fixture = RotationFixture::new();
        let rotated_session = SessionId::new_v4();
        let private_prompt = "the prompt from the previous caller";
        let mut rows = fixture.clear_preamble(rotated_session);
        rows.push(json!({
            "type": "user",
            "uuid": "leaked-prompt",
            "parentUuid": null,
            "promptSource": "typed",
            "message": {"content": private_prompt},
            "cwd": fixture.canonical_cwd,
            "sessionId": rotated_session,
        }));
        let error = clear_writing(&fixture, rotated_session, rows)
            .await
            .unwrap_err();
        assert_assert_empty_refusal(&error, "semantic_row_present");
        assert_eq!(error.details["row_kind"], "typed_user");
        assert_redacted(&error, &[private_prompt]);

        let stranded = fixture
            .source
            .arm_at_eof(fixture.launch_session)
            .await
            .unwrap_err();
        assert_eq!(stranded.details["violation"], "transcript_rotated");
    }

    /// `is_admitted_on_active_chain` is one clause of the predicate and not the
    /// whole of it. This case is the one that clause exists for.
    #[tokio::test]
    async fn a_successor_carrying_a_turn_marker_is_refused() {
        let fixture = RotationFixture::new();
        let rotated_session = SessionId::new_v4();
        let mut rows = fixture.clear_preamble(rotated_session);
        rows.push(json!({
            "type": "system",
            "subtype": "turn_duration",
            "uuid": "duration-row",
            "parentUuid": "leaked-prompt",
            "durationMs": 42,
            "messageCount": 1,
            "sessionId": rotated_session,
        }));
        let error = clear_writing(&fixture, rotated_session, rows)
            .await
            .unwrap_err();
        assert_assert_empty_refusal(&error, "turn_marker_present");
    }

    /// Reject-by-default over METADATA, which used to be the one accept-by-default
    /// arm of the predicate -- and the one carrying text.
    ///
    /// `queue-operation` is queued USER INPUT. MEASURED over 231 transcripts on
    /// the development host: 2,133 `queue-operation` rows, 1,076 of which carry
    /// a `content` field, and this repo's own post-`turn_duration` census counts
    /// 7 of them (`docs/current-state.md`). `ai-title` and `summary` carry text
    /// derived from a conversation. None of the three ever appears in a preamble
    /// that served no work, and admitting them let a clear return
    /// `rotated: true` over a transcript holding a previous caller's words.
    #[tokio::test]
    async fn a_successor_carrying_metadata_that_is_not_preamble_is_refused() {
        let leaked = "the queued prompt from the previous caller";
        for (record_type, row) in [
            (
                "queue-operation",
                json!({
                    "type": "queue-operation",
                    "operation": "add",
                    "content": leaked,
                    "timestamp": "2026-08-03T00:00:00.000Z",
                }),
            ),
            ("ai-title", json!({"type": "ai-title", "aiTitle": leaked})),
            (
                "summary",
                json!({"type": "summary", "summary": leaked, "leafUuid": "leaf"}),
            ),
            (
                "progress",
                json!({"type": "progress", "content": leaked, "toolUseID": "toolu_1"}),
            ),
            (
                "pr-link",
                json!({"type": "pr-link", "prNumber": 1, "prUrl": leaked, "prRepository": "o/r"}),
            ),
        ] {
            let fixture = RotationFixture::new();
            let rotated_session = SessionId::new_v4();
            let mut rows = fixture.clear_preamble(rotated_session);
            let mut row = row;
            // Stamped with the RIGHT id, so nothing but the record type itself
            // can be what refuses it.
            row["sessionId"] = json!(rotated_session);
            rows.push(row);
            let error = clear_writing(&fixture, rotated_session, rows)
                .await
                .unwrap_err();
            assert_assert_empty_refusal(&error, "unexpected_metadata_record");
            assert_eq!(error.details["row_kind"], "metadata");
            assert_eq!(error.details["record_type"], record_type);
            assert_redacted(&error, &[leaked]);
            // ...and this is trigger 4 arriving through the WHOLE path -- a
            // rotation resolved, a preamble read, a record type nobody has
            // measured -- rather than through a hand-built refusal. Until this
            // change it quarantined one instance and the pool minted the next
            // one into the identical drift.
            assert_eq!(
                clear_refusal_repromotion_trigger(&error.details),
                Some("unexpected_metadata_record"),
                "a preamble record type Claude has never written is a fact about the installed \
                 Claude, not about this instance"
            );
        }
    }

    /// Identity applies to metadata too, in the predicate that reads the file as
    /// evidence about itself.
    ///
    /// MEASURED: `mode`, `permission-mode`, `bridge-session` and `last-prompt`
    /// carry `sessionId` on 100% of 7,222 rows and it equals the transcript's own
    /// id on every one; `file-history-snapshot` carries none on 289 of 289. So a
    /// preamble row stamped with a FOREIGN id is not drift to be tolerated, it is
    /// the file saying it belongs to someone else -- and an unstamped `mode` row
    /// is a shape no observed Claude writes.
    #[tokio::test]
    async fn preamble_metadata_must_carry_this_transcripts_own_identity() {
        let fixture = RotationFixture::new();
        let rotated_session = SessionId::new_v4();
        let mut rows = fixture.clear_preamble(rotated_session);
        // Not row 0: that one is the rotation anchor and its id is already what
        // `resolve_rotation` matched on. This is a preamble row that carries a
        // different session's stamp, which is the shape a mis-resolved file has.
        rows.push(json!({
            "type": "bridge-session",
            "sessionId": SessionId::new_v4(),
            "bridgeSessionId": "bridge",
            "lastSequenceNum": 1,
        }));
        let error = clear_writing(&fixture, rotated_session, rows)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::SchemaDrift);
        assert_eq!(error.details["row_kind"], "metadata");
        assert_eq!(error.details["field"], "session_id");
        assert_eq!(error.details["violation"], "mismatch");

        // The other half: a stamped record type that stopped stamping itself.
        let fixture = RotationFixture::new();
        let rotated_session = SessionId::new_v4();
        let mut rows = fixture.clear_preamble(rotated_session);
        rows[5]
            .as_object_mut()
            .unwrap()
            .remove("sessionId")
            .expect("the measured last-prompt row carries one");
        let error = clear_writing(&fixture, rotated_session, rows)
            .await
            .unwrap_err();
        assert_eq!(error.details["row_kind"], "metadata");
        assert_eq!(error.details["violation"], "missing");
    }

    /// `last-prompt` is allowlisted for what it says, not for its type: MEASURED
    /// it carries `lastPrompt: null` in the one clean post-`/clear` transcript of
    /// the 61-file corpus, and the prompt text verbatim in 2,337 of 2,365 rows
    /// everywhere else. A cleared cell whose trailing marker names a prompt is a
    /// cell that ran one.
    #[tokio::test]
    async fn a_last_prompt_row_that_names_a_prompt_is_refused() {
        let leaked = "the prompt from the previous caller";
        let fixture = RotationFixture::new();
        let rotated_session = SessionId::new_v4();
        let mut rows = fixture.clear_preamble(rotated_session);
        rows[5]["lastPrompt"] = json!(leaked);
        let error = clear_writing(&fixture, rotated_session, rows)
            .await
            .unwrap_err();
        assert_assert_empty_refusal(&error, "metadata_prompt_present");
        assert_eq!(error.details["row_kind"], "metadata");
        assert_redacted(&error, &[leaked]);
    }

    /// Reject-by-default over system subtypes: `compact_boundary` means a
    /// compaction happened, which is state.
    #[tokio::test]
    async fn a_successor_carrying_an_unexpected_system_subtype_is_refused() {
        let fixture = RotationFixture::new();
        let rotated_session = SessionId::new_v4();
        let mut rows = fixture.clear_preamble(rotated_session);
        rows.push(json!({
            "type": "system",
            "subtype": "compact_boundary",
            "uuid": "compact-row",
            "sessionId": rotated_session,
        }));
        let error = clear_writing(&fixture, rotated_session, rows)
            .await
            .unwrap_err();
        assert_assert_empty_refusal(&error, "unexpected_system_subtype");
        assert_eq!(error.details["subtype"], "compact_boundary");
    }

    /// `JsonlParser` returns `Unknown` without erroring even in Strict mode, so
    /// a row type Claude added tomorrow would otherwise pass silently.
    #[tokio::test]
    async fn a_successor_carrying_an_unknown_row_type_is_refused() {
        let fixture = RotationFixture::new();
        let rotated_session = SessionId::new_v4();
        let mut rows = fixture.clear_preamble(rotated_session);
        rows.push(json!({
            "type": "future_semantic_row",
            "uuid": "future-row",
            "sessionId": rotated_session,
        }));
        let error = clear_writing(&fixture, rotated_session, rows)
            .await
            .unwrap_err();
        assert_assert_empty_refusal(&error, "unknown_row");
    }

    /// A `UserOther` row that is neither the caveat nor a command echo.
    #[tokio::test]
    async fn a_successor_carrying_an_unrecognized_user_row_is_refused() {
        let fixture = RotationFixture::new();
        let rotated_session = SessionId::new_v4();
        let private_text = "unrecognized preamble body";
        let mut rows = fixture.clear_preamble(rotated_session);
        rows.push(json!({
            "type": "user",
            "uuid": "odd-user-row",
            "parentUuid": null,
            "message": {"role": "user", "content": private_text},
            "cwd": fixture.canonical_cwd,
            "sessionId": rotated_session,
        }));
        let error = clear_writing(&fixture, rotated_session, rows)
            .await
            .unwrap_err();
        assert_assert_empty_refusal(&error, "unexpected_user_row");
        assert_redacted(&error, &[private_text]);
    }

    /// The row budget, refused before anything is parsed.
    #[tokio::test]
    async fn a_successor_over_the_row_budget_is_refused() {
        let fixture = RotationFixture::new();
        let rotated_session = SessionId::new_v4();
        let mut rows = fixture.clear_preamble(rotated_session);
        while rows.len() <= MAX_ASSERT_EMPTY_ROWS {
            rows.push(json!({"type": "last-prompt", "sessionId": rotated_session}));
        }
        let error = clear_writing(&fixture, rotated_session, rows)
            .await
            .unwrap_err();
        assert_assert_empty_refusal(&error, "row_budget_exceeded");
    }

    /// A successor that rotated but never carried the echo. Something other
    /// than this instance's `/clear` opened that transcript, and waiting longer
    /// cannot change the answer -- so the wait is bounded and then refuses.
    #[tokio::test]
    async fn a_successor_with_no_clear_echo_is_refused_after_the_settle_wait() {
        let fixture = RotationFixture::with_rebind_timeout(Duration::from_millis(120));
        let rotated_session = SessionId::new_v4();
        let mut rows = fixture.clear_preamble(rotated_session);
        // Drop the command echo, keep everything else -- including the
        // `local_command` system row, so nothing but the echo distinguishes it.
        rows.remove(3);
        let error = clear_writing(&fixture, rotated_session, rows)
            .await
            .unwrap_err();
        assert_assert_empty_refusal(&error, "clear_command_missing");
    }

    /// The race the settle wait exists for: `resolve_rotation` returns the
    /// instant row 0 parses, which can be before the echo has been written.
    /// Refusing there would quarantine healthy instances on nothing.
    #[tokio::test]
    async fn a_preamble_that_lands_after_the_anchor_still_rebinds() {
        let fixture = RotationFixture::with_rebind_timeout(Duration::from_millis(800));
        let rotated_session = SessionId::new_v4();
        fixture.open_transcript(fixture.launch_session);
        let rotated_path = fixture.transcript_path(rotated_session);
        let preamble = fixture.clear_preamble(rotated_session);
        let (control, _handle) = fixture.clearing_terminal(move || {
            write_rows(&rotated_path, &preamble[..1]);
            let finished = rotated_path.clone();
            let rest = preamble[1..].to_vec();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(40));
                append_rows(&finished, &rest);
            });
        });
        assert_eq!(
            clear_and_rebind(
                &control,
                &fixture.source,
                fixture.launch_session,
                test_deadline(),
            )
            .await
            .unwrap(),
            rotated_session
        );
    }

    /// A preamble still being written row by row must not be declared settled
    /// between two of them.
    ///
    /// `arm_at_eof` runs immediately after this returns and refuses if the file
    /// grows between its `stat` and its `open`. Every intermediate state here is
    /// a complete, terminated, individually-inert set of rows -- the trailing
    /// partial check cannot see it -- so without a quiescence requirement this
    /// path quarantines healthy instances on the writer's ordinary cadence.
    #[tokio::test]
    async fn a_preamble_written_row_by_row_is_not_settled_until_it_stops_growing() {
        let fixture = RotationFixture::with_rebind_timeout(Duration::from_millis(1_500));
        let rotated_session = SessionId::new_v4();
        fixture.open_transcript(fixture.launch_session);
        let rotated_path = fixture.transcript_path(rotated_session);
        let preamble = fixture.clear_preamble(rotated_session);
        let (control, _handle) = fixture.clearing_terminal(move || {
            // Row 0 only, then the rest one row at a time with a gap wider than
            // the poll interval, so every observation lands on a boundary
            // between two complete rows.
            write_rows(&rotated_path, &preamble[..1]);
            let finished = rotated_path.clone();
            let rest = preamble[1..].to_vec();
            std::thread::spawn(move || {
                for row in rest {
                    std::thread::sleep(Duration::from_millis(10));
                    append_rows(&finished, &[row]);
                }
            });
        });
        let rebound = clear_and_rebind(
            &control,
            &fixture.source,
            fixture.launch_session,
            test_deadline(),
        )
        .await
        .unwrap();
        assert_eq!(rebound, rotated_session);
        let proof = fixture
            .source
            .assert_empty_at(rotated_session)
            .unwrap()
            .expect("the rebound transcript is locatable");
        assert_eq!(
            proof.bytes,
            std::fs::metadata(fixture.transcript_path(rotated_session))
                .unwrap()
                .len(),
            "the accepted proof must describe the whole settled file"
        );
        assert_eq!(proof.rows, 6);
    }

    /// A half-written trailing record is never judged as a row, and never
    /// admitted as a settled preamble either.
    #[tokio::test]
    async fn a_partly_written_trailing_row_is_not_a_settled_preamble() {
        let fixture = RotationFixture::with_rebind_timeout(Duration::from_millis(120));
        let rotated_session = SessionId::new_v4();
        fixture.open_transcript(fixture.launch_session);
        let rotated_path = fixture.transcript_path(rotated_session);
        let preamble = fixture.clear_preamble(rotated_session);
        let (control, _handle) = fixture.clearing_terminal(move || {
            write_rows(&rotated_path, &preamble);
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&rotated_path)
                .unwrap();
            file.write_all(b"{\"type\":\"user\",\"uuid\":\"half")
                .unwrap();
            file.flush().unwrap();
        });
        let error = clear_and_rebind(
            &control,
            &fixture.source,
            fixture.launch_session,
            test_deadline(),
        )
        .await
        .unwrap_err();
        assert_assert_empty_refusal(&error, "preamble_not_settled");
    }

    /// A launch preamble that carries a `/clear` echo means the id resolution
    /// found a file this launch did not open.
    #[test]
    fn a_launch_preamble_carrying_a_clear_echo_is_refused() {
        let fixture = RotationFixture::new();
        write_rows(
            &fixture.transcript_path(fixture.launch_session),
            &fixture.clear_preamble(fixture.launch_session),
        );
        let error = fixture
            .source
            .prove_empty_at_launch(fixture.launch_session)
            .unwrap_err();
        assert_assert_empty_refusal(&error, "unexpected_clear_echo");
    }

    /// A schema token reaching a diagnostic is bounded and charset-checked,
    /// because the predicate is refusing precisely because the file is not what
    /// it expected.
    #[test]
    fn a_diagnostic_schema_token_is_bounded_before_it_is_reproduced() {
        assert_eq!(redacted_schema_token("/clear"), "/clear");
        assert_eq!(
            redacted_schema_token("compact_boundary"),
            "compact_boundary"
        );
        assert_eq!(redacted_schema_token(""), "unrecognized");
        assert_eq!(redacted_schema_token("/a name with spaces"), "unrecognized");
        assert_eq!(
            redacted_schema_token(&"x".repeat(MAX_DIAGNOSTIC_TOKEN_BYTES + 1)),
            "unrecognized"
        );
    }

    // ---- fail closed: a stranded turn never returns a result --------------

    const STRANDED_PROMPT: &str = "the prompt whose answer went somewhere else";
    const STRANDED_ANSWER: &str = "the only answer that was ever written";
    const ACTOR_CLOCK_MS: u64 = 1_000_000;

    #[derive(Debug)]
    struct ActorClock;

    impl Clock for ActorClock {
        fn now_ms(&self) -> u64 {
            ACTOR_CLOCK_MS
        }
    }

    /// A terminal that is always ready and counts submissions.
    ///
    /// A double is the right shape here on purpose: the screen is a liveness
    /// veto in this codebase and never a source of truth, so a question about
    /// WHICH FILE a turn is decided from must not be answerable from the
    /// terminal at all. If this test could pass because of something the
    /// terminal said, it would be testing the wrong authority.
    #[derive(Debug, Default)]
    struct ReadyTerminalDouble {
        submissions: AtomicUsize,
    }

    impl ReadyTerminalDouble {
        async fn wait_for_submission(&self) {
            tokio::time::timeout(Duration::from_secs(5), async {
                while self.submissions.load(Ordering::Acquire) == 0 {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            })
            .await
            .expect("the prompt was never submitted");
        }
    }

    #[async_trait]
    impl TerminalControl for ReadyTerminalDouble {
        async fn submit_prompt(
            &self,
            _session_id: SessionId,
            _turn_id: TurnId,
            _prompt: &str,
            _deadline_unix_ms: u64,
        ) -> DriverResult<()> {
            self.submissions.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }

        async fn completion_evidence(
            &self,
            _session_id: SessionId,
            _turn_id: TurnId,
        ) -> DriverResult<TerminalEvidence> {
            Ok(TerminalEvidence {
                ready_prompt: true,
                quiet: true,
                ..TerminalEvidence::default()
            })
        }

        async fn observe_screen(
            &self,
            _session_id: SessionId,
        ) -> DriverResult<TerminalScreenObservation> {
            Ok(TerminalScreenObservation::Ready)
        }

        async fn interrupt(
            &self,
            _session_id: SessionId,
            _turn_id: TurnId,
        ) -> DriverResult<InterruptRecovery> {
            Ok(InterruptRecovery::RecoveredToReady)
        }

        async fn close(&self, _session_id: SessionId, _policy: ClosePolicy) -> DriverResult<bool> {
            Ok(true)
        }
    }

    struct StrandedTurnFixture {
        rotation: RotationFixture,
        registry: SessionRegistry,
        terminal: Arc<ReadyTerminalDouble>,
        generation_id: SessionGenerationId,
    }

    impl StrandedTurnFixture {
        async fn new() -> Self {
            let rotation = RotationFixture::new();
            rotation.open_transcript(rotation.launch_session);
            let terminal = Arc::new(ReadyTerminalDouble::default());
            let registry = SessionRegistry::with_clock(
                SessionActorConfig {
                    poll_interval: Duration::from_millis(2),
                    default_turn_timeout_ms: 2_000,
                    ..SessionActorConfig::default()
                },
                Arc::new(ActorClock),
            );
            let generation_id = SessionGenerationId::new();
            registry
                .register(SessionRegistration {
                    agent: None,
                    owner: crate::v1::SessionOwner::Caller,
                    session_id: rotation.launch_session,
                    generation_id,
                    cwd: rotation.canonical_cwd.to_string_lossy().into_owned(),
                    compatibility: CompatibilityReport {
                        claude_version: "rotation-test".to_owned(),
                        os: std::env::consts::OS.to_owned(),
                        arch: std::env::consts::ARCH.to_owned(),
                        terminal_profile: TerminalProfile::Transparent,
                        input_transport: InputTransport::Sdk,
                        tested: true,
                        transcript_drain_ms: 1,
                    },
                    dangerous_permission_bypass: false,
                    resumable: true,
                    cell: SessionCell::Full,
                    idle_ttl_ms: None,
                    initial_needs_input: None,
                    terminal: Arc::clone(&terminal) as Arc<dyn TerminalControl>,
                    transcript: Arc::clone(&rotation.source) as Arc<dyn TranscriptSource>,
                })
                .await
                .unwrap();
            Self {
                rotation,
                registry,
                terminal,
                generation_id,
            }
        }

        async fn submit_turn(&self) -> TurnId {
            let turn_id = TurnId::new_v4();
            self.registry
                .run_turn(RunTurnRequest {
                    session_id: self.rotation.launch_session,
                    generation_id: self.generation_id,
                    turn: TurnRequest {
                        turn_id,
                        prompt: STRANDED_PROMPT.to_owned(),
                        deadline_unix_ms: Some(ACTOR_CLOCK_MS + 1_500),
                        lease: TurnLeasePolicy::default(),
                    },
                })
                .await
                .unwrap();
            turn_id
        }

        async fn outcome(&self, turn_id: TurnId) -> StoredTurnTerminal {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let Some(outcome) = self
                        .registry
                        .stored_turn(self.rotation.launch_session, self.generation_id, turn_id)
                        .await
                        .unwrap()
                    {
                        return outcome;
                    }
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            })
            .await
            .expect("the turn never reached a terminal outcome")
        }

        /// A complete, ordinary, successful turn: the typed prompt Claude
        /// records on acknowledgement, and the terminal assistant message.
        fn completing_rows(&self, session_id: SessionId) -> Vec<Value> {
            vec![
                json!({
                    "type": "user",
                    "uuid": "stranded-typed-user",
                    "parentUuid": null,
                    "sessionId": session_id,
                    "cwd": self.rotation.canonical_cwd,
                    "promptSource": "typed",
                    "message": {"content": STRANDED_PROMPT},
                }),
                json!({
                    "type": "assistant",
                    "uuid": "stranded-assistant",
                    "parentUuid": "stranded-typed-user",
                    "sessionId": session_id,
                    "cwd": self.rotation.canonical_cwd,
                    "message": {
                        "id": "stranded-message",
                        "model": "rotation-test-model",
                        "content": [{"type": "text", "text": STRANDED_ANSWER}],
                        "stop_reason": "end_turn",
                        "usage": {"input_tokens": 1, "output_tokens": 1},
                    },
                }),
            ]
        }

        async fn cleanup(self) {
            self.registry
                .close(CloseSessionRequest {
                    session_id: self.rotation.launch_session,
                    generation_id: self.generation_id,
                    policy: ClosePolicy::Force,
                })
                .await
                .unwrap();
            self.registry
                .unregister(self.rotation.launch_session, self.generation_id)
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn a_clear_whose_rebind_failed_can_never_complete_the_turn_it_stranded() {
        // CONTROL. The same rows, in the file the tail is bound to, complete the
        // turn. Without this half the treatment below proves only that some rows
        // somewhere failed to finish a turn, which is not the claim.
        let healthy = StrandedTurnFixture::new().await;
        let turn_id = healthy.submit_turn().await;
        healthy.terminal.wait_for_submission().await;
        append_rows(
            &healthy
                .rotation
                .transcript_path(healthy.rotation.launch_session),
            &healthy.completing_rows(healthy.rotation.launch_session),
        );
        let StoredTurnTerminal::Result(result) = healthy.outcome(turn_id).await else {
            panic!("rows in the bound transcript must complete the turn");
        };
        assert_eq!(result.text, STRANDED_ANSWER);
        healthy.cleanup().await;

        // TREATMENT. A `/clear` lands, its rebind is refused because two
        // candidate transcripts appeared, and Claude then writes this turn --
        // acknowledgement and terminal message, byte-identical to the control --
        // into the transcript it actually rotated to. The bound file never grows
        // again, and every fence in the source stays green against it.
        let stranded = StrandedTurnFixture::new().await;
        let rotated = stranded.rotation.transcript_path(SessionId::new_v4());
        let decoy = stranded.rotation.transcript_path(SessionId::new_v4());
        let candidates: Vec<(PathBuf, Value)> = [&rotated, &decoy]
            .into_iter()
            .map(|path| {
                let session_id = path.file_stem().unwrap().to_str().unwrap().parse().unwrap();
                (path.clone(), stranded.rotation.rotation_anchor(session_id))
            })
            .collect();
        let (control, _) = stranded.rotation.clearing_terminal(move || {
            for (path, anchor) in &candidates {
                write_rows(path, &[anchor.clone()]);
            }
        });
        let refusal = clear_and_rebind(
            &control,
            &stranded.rotation.source,
            stranded.rotation.launch_session,
            test_deadline(),
        )
        .await
        .unwrap_err();
        assert_eq!(refusal.details["violation"], "clear_rebind_ambiguous");

        let rotated_session = rotated
            .file_stem()
            .unwrap()
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        append_rows(&rotated, &stranded.completing_rows(rotated_session));

        let turn_id = stranded.submit_turn().await;
        let outcome = stranded.outcome(turn_id).await;
        let StoredTurnTerminal::Failed(error) = outcome else {
            panic!("a turn stranded by a failed rebind must never produce a result");
        };
        // It fails, and it fails by NAME. The bare `TurnTimeout` this replaces
        // gave an operator nothing to pull on; this gives them the id that was
        // abandoned and the fact that a clear is what abandoned it.
        assert_eq!(error.code, ErrorCode::TranscriptUnavailable);
        assert_ne!(error.code, ErrorCode::TurnTimeout);
        assert_eq!(error.details["violation"], "clear_rebind_failed");
        assert_eq!(
            error.details["abandoned_session_id"],
            stranded.rotation.launch_session.to_string()
        );
        assert_eq!(error.details["rebound_session_id"], Value::Null);
        assert!(error.message.contains("/clear"));
        stranded.cleanup().await;
    }

    fn selection_refusal(screen: &StyledScreen) -> DriverFailure {
        prove_control_command_selection(screen, ControlCommand::Clear)
            .expect_err("this captured screen must not prove a /clear selection")
    }

    fn assert_selection_refused(screen: &StyledScreen, reason: &str) -> DriverFailure {
        let error = selection_refusal(screen);
        assert_eq!(error.code, ErrorCode::PromptNotAcknowledged);
        assert_eq!(
            error.details["violation"], "control_command_selection_unproven",
            "{:?}",
            error.details
        );
        assert_eq!(error.details["reason"], reason, "{:?}", error.details);
        // Nothing was submitted, so a refusal here must never claim otherwise.
        assert_eq!(error.details.get("enter_attempted"), None);
        error
    }

    /// The positive: the real screen a real `/clear` produces is proven.
    ///
    /// If this ever fails, the shipped predicate has stopped describing Claude
    /// Code's menu and every clear will refuse. That is the safe direction, but
    /// it is a version-compatibility break and must be read as one rather than
    /// as a flaky clear.
    #[test]
    fn the_measured_clear_menu_proves_its_own_selection() {
        prove_control_command_selection(&measured_clear_screen(), ControlCommand::Clear).unwrap();
    }

    /// 2.1.238: menu above the composer, indented tokens, unselected rows
    /// themselves uniform. A proof that still required "below, column 0, any
    /// uniform row is selected" refuses this screen (`menu_not_rendered`) and
    /// every `/clear` on that version destroys the cell.
    #[test]
    fn the_measured_238_menu_above_the_composer_proves_its_own_selection() {
        prove_control_command_selection(
            &captured_screen(21, measured_238_above_composer_menu(), 21, 8),
            ControlCommand::Clear,
        )
        .unwrap();
    }

    /// Same 2.1.238 geometry with the highlight on `/code-review`. The wrap
    /// line ` /resume)` is indented past the candidate bound and must not
    /// become a second selected entry.
    #[test]
    fn a_238_menu_that_would_select_a_different_command_is_refused() {
        let rows = measured_238_above_composer_menu()
            .into_iter()
            .map(|captured| match captured.row {
                16 => CapturedRow {
                    style: CapturedStyle::Runs(vec![(2, 79, CAPTURED_UNSELECTED)]),
                    ..captured
                },
                18 => CapturedRow {
                    style: CapturedStyle::Runs(vec![(2, 79, CAPTURED_SELECTED)]),
                    ..captured
                },
                _ => captured,
            })
            .collect();
        let error = assert_selection_refused(
            &captured_screen(21, rows, 21, 8),
            "menu_selects_a_different_command",
        );
        assert_eq!(error.details["selected_command"], "/code-review");
    }

    /// The linux measurement of the same geometry: the real 2.1.257 screen,
    /// menu above the composer, is proven. This is the frame the pool refused
    /// with `menu_not_rendered` on 2026-09-01 at both 2.1.236 and 2.1.257 under
    /// the below-only proof, which turned every post-turn `/clear` into a
    /// silent ~5 s relaunch. The same frame is replayed from the corpus by
    /// `tests/screen_corpus_replay.rs`.
    ///
    /// The continuation row of the selected entry (row 17) carries the
    /// selection colour and is NOT a second highlighted candidate — if it were,
    /// this would refuse `menu_selection_not_unique`.
    #[test]
    fn the_measured_2_1_257_menu_above_the_composer_proves_its_own_selection() {
        let screen = measured_257_clear_screen();
        prove_control_command_selection(&screen, ControlCommand::Clear).unwrap();
        // The two-cell indent is unstyled and skipped; the body is one colour,
        // the composer's own.
        assert_eq!(
            candidate_body_colour(screen.row(16)),
            Some(CellColor::Explicit(CAPTURED_257_SELECTED))
        );
        assert_eq!(
            composer_command_colour(&screen, 21),
            Some(CellColor::Explicit(CAPTURED_257_SELECTED))
        );
        // The continuation row shares the colour, which is why it is excluded
        // by indent rather than by colour.
        assert!(candidate_body_colour(screen.row(17)).is_some());
        assert!(menu_candidate_at(&screen, 17).is_none());
        // MEASURED: the unselected row is ALSO uniform, in the other colour, so
        // uniformity alone would count two rows.
        assert_eq!(
            candidate_body_colour(screen.row(18)),
            Some(CellColor::Explicit(CAPTURED_257_DIM))
        );
    }

    /// The negative at 2.1.257: the composer says `/clear`, the entry Enter
    /// would select (the row in the composer's colour) is `/code-review`.
    #[test]
    fn a_2_1_257_menu_that_would_select_a_different_command_is_refused() {
        let error = assert_selection_refused(
            &measured_257_clear_menu_coloured(CAPTURED_257_DIM, CAPTURED_257_SELECTED),
            "menu_selects_a_different_command",
        );
        assert_eq!(error.details["selected_command"], "/code-review");
    }

    /// Two counts that are not one at 2.1.257: both entries in the composer's
    /// colour, neither in it, and a colourless composer (a theme in which the
    /// typed command carries no explicit colour names no entry).
    #[test]
    fn a_2_1_257_menu_whose_selection_is_not_exactly_one_row_is_refused() {
        let doubled =
            measured_257_clear_menu_coloured(CAPTURED_257_SELECTED, CAPTURED_257_SELECTED);
        let error = assert_selection_refused(&doubled, "menu_selection_not_unique");
        assert_eq!(error.details["highlighted_rows"], 2);

        let unhighlighted = measured_257_clear_menu_coloured(CAPTURED_257_DIM, CAPTURED_257_DIM);
        let error = assert_selection_refused(&unhighlighted, "menu_selection_not_unique");
        assert_eq!(error.details["highlighted_rows"], 0);

        let colourless_composer = captured_screen_of(
            120,
            23,
            measured_257_clear_menu()
                .into_iter()
                .map(|captured| match captured.row {
                    21 => CapturedRow {
                        style: CapturedStyle::Plain,
                        ..captured
                    },
                    _ => captured,
                })
                .collect(),
            21,
            8,
        );
        let error = assert_selection_refused(&colourless_composer, "menu_selection_not_unique");
        assert_eq!(error.details["highlighted_rows"], 0);
    }

    /// VERBATIM CAPTURE, 2.1.257, the idle boxed composer (corpus revision 22,
    /// the frame before the paste): rows 16–19 blank, the same two rules around
    /// `❯`, and the footer. A rule on either side of the composer proves
    /// nothing without a candidate beside it.
    #[test]
    fn the_idle_boxed_composer_at_2_1_257_is_not_a_menu() {
        let screen = captured_screen_of(
            120,
            23,
            measured_257_clear_menu()
                .into_iter()
                .filter(|captured| !(16..=19).contains(&captured.row))
                .collect(),
            21,
            8,
        );
        assert_selection_refused(&screen, "menu_not_rendered");
    }

    /// Continuation rows are never candidates. A screen whose only rows above
    /// the rule are the 32-space continuations — no row with a two-cell indent
    /// and a solidus — offers no candidate, even though one of them is painted
    /// in the selection colour and begins with `/resume)`.
    #[test]
    fn continuation_rows_alone_above_the_2_1_257_rule_are_not_a_menu() {
        let screen = captured_screen_of(
            120,
            23,
            measured_257_clear_menu()
                .into_iter()
                .filter(|captured| captured.row != 16 && captured.row != 18)
                .collect(),
            21,
            8,
        );
        assert_selection_refused(&screen, "menu_not_rendered");
    }

    /// The precondition is a precondition at 2.1.257 too: a refusal leaves
    /// Enter unsent, and the accepted measured screen sends it once.
    #[tokio::test]
    async fn the_2_1_257_menu_gates_enter_the_same_way_the_2_1_220_menu_does() {
        let (control, handle) = clear_control_showing(
            measured_257_clear_menu_coloured(CAPTURED_257_DIM, CAPTURED_257_SELECTED),
            Duration::from_secs(1),
        );
        let error = control
            .type_control_command(ControlCommand::Clear, test_deadline())
            .await
            .unwrap_err();
        assert_eq!(error.details["reason"], "menu_selects_a_different_command");
        assert_eq!(handle.counts(), (1, 0));

        let (control, handle) =
            clear_control_showing(measured_257_clear_screen(), Duration::from_secs(1));
        control
            .type_control_command(ControlCommand::Clear, test_deadline())
            .await
            .unwrap();
        assert_eq!(handle.counts(), (1, 1));
    }

    /// The negative the whole check exists for: the composer says `/clear`, the
    /// screen is a real capture, and the entry Enter would select is a different
    /// command. Nothing in this codebase could previously see this screen —
    /// `visible_text` renders it identically to the accepted one — and the
    /// post-hoc `wrong_local_command` clause cannot fire on it either, because
    /// `/code-review` opens no transcript to inspect.
    #[test]
    fn a_menu_that_would_select_a_different_command_is_refused() {
        let error = assert_selection_refused(
            &measured_clear_menu_selecting(13, 73),
            "menu_selects_a_different_command",
        );
        assert_eq!(error.details["selected_command"], "/code-review");

        // And the same screen with the highlight on the two other real
        // neighbours, both of which are prompt-expanding skills: selecting one
        // spends a model turn and dirties the instance.
        for (row, extent, command) in [(15, 63, "/simplify"), (19, 71, "/run-skill-generator")] {
            let error = assert_selection_refused(
                &measured_clear_menu_selecting(row, extent),
                "menu_selects_a_different_command",
            );
            assert_eq!(error.details["selected_command"], command);
        }
    }

    /// VERBATIM CAPTURE at prefix `/c`, the screen that produced the
    /// load-bearing measurement: Enter here ran `/cd`, not the typed `/c`, and
    /// printed `Usage: /cd <path>`. `/cd` rotates nothing, so the caller saw a
    /// rebind timeout blamed on a file that never appeared.
    #[test]
    fn the_captured_screen_whose_enter_ran_cd_is_refused() {
        let screen = captured_screen(
            14,
            vec![
                captured(9, "❯ /c", CapturedStyle::Plain),
                menu_rule_row(10),
                captured(
                    11,
                    "/cd                           Move this session to a new working directory",
                    CapturedStyle::Runs(vec![(0, 73, CAPTURED_SELECTED)]),
                ),
                captured(
                    12,
                    "/copy                         Copy Claude's last response to clipboard (or",
                    CapturedStyle::Runs(vec![
                        (0, 0, CAPTURED_UNSELECTED),
                        (1, 1, CAPTURED_SELECTED),
                        (2, 7, CAPTURED_UNSELECTED),
                    ]),
                ),
                captured(
                    14,
                    "/clear                        Start a new session with empty context;",
                    CapturedStyle::Words(CAPTURED_UNSELECTED),
                ),
                captured(
                    16,
                    "/color                        Set the prompt bar color for this session",
                    CapturedStyle::Words(CAPTURED_UNSELECTED),
                ),
                captured(
                    17,
                    "/chrome                       Open Claude in Chrome settings",
                    CapturedStyle::Words(CAPTURED_UNSELECTED),
                ),
                captured(
                    18,
                    "/config                       Open settings",
                    CapturedStyle::Words(CAPTURED_UNSELECTED),
                ),
            ],
            9,
            4,
        );
        assert_selection_refused(&screen, "composer_text_unproven");
    }

    /// VERBATIM CAPTURE at a bare `/`, where the selected entry is `/add-dir`.
    /// Enter on this screen opened a modal with no composer and an invisible
    /// cursor, which pmux cannot classify and cannot escape from: the instance
    /// wedged until the input-gate timeout on every later call.
    #[test]
    fn the_captured_screen_whose_enter_wedged_the_instance_is_refused() {
        let screen = captured_screen(
            14,
            vec![
                captured(9, "❯ /", CapturedStyle::Plain),
                menu_rule_row(10),
                captured(
                    11,
                    "/add-dir                      Add a new working directory",
                    CapturedStyle::Runs(vec![(0, 56, CAPTURED_SELECTED)]),
                ),
                captured(
                    12,
                    "/advisor                      Let Claude consult a stronger model at key",
                    CapturedStyle::Runs(vec![
                        (0, 7, CAPTURED_UNSELECTED),
                        (30, 32, CAPTURED_UNSELECTED),
                        (34, 39, CAPTURED_UNSELECTED),
                    ]),
                ),
                captured(
                    13,
                    "                              moments",
                    CapturedStyle::Plain,
                ),
                captured(
                    14,
                    "/agents                       (removed) Ask Claude to create/manage",
                    CapturedStyle::Words(CAPTURED_UNSELECTED),
                ),
                captured(
                    16,
                    "/artifacts                    Browse your published and shared artifacts",
                    CapturedStyle::Words(CAPTURED_UNSELECTED),
                ),
            ],
            9,
            3,
        );
        assert_selection_refused(&screen, "composer_text_unproven");
    }

    /// VERBATIM CAPTURE of the pre-menu window, MEASURED at 14–32 ms after the
    /// paste: a complete, settled `/clear` in the composer with no menu painted
    /// anywhere. Enter here executes `/clear` correctly — the filter is computed
    /// on input and only the paint lags — and this refuses it anyway, because an
    /// absent menu is not evidence about the selection. Refusing a frame that
    /// would have worked is the cost, and it is the direction this is allowed to
    /// be wrong in.
    #[test]
    fn a_composer_with_no_menu_painted_yet_is_refused_even_though_enter_would_work() {
        let screen = captured_screen(
            13,
            vec![
                menu_rule_row(19),
                captured(
                    20,
                    "❯ /clear",
                    CapturedStyle::Runs(vec![(2, 7, CAPTURED_SELECTED)]),
                ),
                menu_rule_row(21),
            ],
            20,
            8,
        );
        assert_selection_refused(&screen, "menu_not_rendered");
    }

    /// Two counts that are not one. A screen where nothing carries the measured
    /// selected shape — a retuned theme, a release that marks the selection some
    /// other way — refuses, and so does a screen where two rows do.
    #[test]
    fn a_selection_that_is_not_exactly_one_row_is_refused() {
        let unhighlighted = captured_screen(
            14,
            measured_clear_menu()
                .into_iter()
                .map(|captured| match captured.row {
                    11 => CapturedRow {
                        style: CapturedStyle::Words(CAPTURED_UNSELECTED),
                        ..captured
                    },
                    _ => captured,
                })
                .collect(),
            9,
            8,
        );
        let error = assert_selection_refused(&unhighlighted, "menu_selection_not_unique");
        assert_eq!(error.details["highlighted_rows"], 0);

        let doubled = captured_screen(
            14,
            measured_clear_menu()
                .into_iter()
                .map(|captured| match captured.row {
                    13 => CapturedRow {
                        style: CapturedStyle::Runs(vec![(0, 73, CAPTURED_SELECTED)]),
                        ..captured
                    },
                    _ => captured,
                })
                .collect(),
            9,
            8,
        );
        let error = assert_selection_refused(&doubled, "menu_selection_not_unique");
        assert_eq!(error.details["highlighted_rows"], 2);
    }

    /// The composer proof is cursor-correlated, not textual. `row_text`
    /// right-trims, so a composer holding `/clear` followed by anything the trim
    /// removes renders identically to a clean one; the caret column is the part
    /// that cannot be trimmed away.
    #[test]
    fn a_composer_whose_caret_is_not_at_the_end_of_the_literal_is_refused() {
        let rows = measured_clear_menu();
        assert_selection_refused(
            &captured_screen(14, rows.clone(), 9, 12),
            "composer_text_unproven",
        );
        // An invisible cursor is no anchor at all, and neither is a row with no
        // prompt glyph on it.
        let mut hidden = captured_screen(14, rows.clone(), 9, 8);
        hidden.cursor = hidden.cursor.map(|cursor| TerminalCursor {
            visible: false,
            ..cursor
        });
        assert_selection_refused(&hidden, "composer_not_rendered");
        assert_selection_refused(&captured_screen(14, rows, 11, 8), "composer_not_rendered");
    }

    /// The composer must hold exactly what pmux pasted, even when the menu
    /// beside it looks right.
    ///
    /// MEASURED, the paint lags the input by 14–32 ms, so a frame whose menu
    /// still describes an earlier composer is a shape this screen really can
    /// take. But the reason to refuse is simpler than the race: pmux never types
    /// `/model`, so a composer holding it means something other than pmux wrote
    /// into this TUI, and nothing else on that screen can then be trusted to
    /// describe pmux's own paste — including the menu that agrees with it.
    ///
    /// Six characters, so the caret still lands where `/clear` would put it.
    /// This is the case the caret correlation cannot see and the text equality
    /// can.
    #[test]
    fn a_composer_holding_a_different_command_of_the_same_length_is_refused() {
        let rows = measured_clear_menu()
            .into_iter()
            .map(|captured| match captured.row {
                9 => CapturedRow {
                    text: "❯ /model".to_owned(),
                    ..captured
                },
                _ => captured,
            })
            .collect();
        assert_selection_refused(&captured_screen(14, rows, 9, 8), "composer_text_unproven");
    }

    /// SYNTHETIC, and deliberately so: no capture looks like this, which is the
    /// point. The rule is what makes the rows below the composer a MENU rather
    /// than whatever else the screen happens to be showing, and the candidate
    /// scan reads any row that starts with a solidus. Without the rule to anchor
    /// it, a settled composer with unrelated content below it reads as a menu
    /// whose entry is `/clear`, and Enter goes out on a screen with no menu on
    /// it at all.
    #[test]
    fn rows_below_a_composer_are_not_a_menu_without_the_rule_that_makes_them_one() {
        let screen = captured_screen(
            13,
            vec![
                menu_rule_row(19),
                captured(
                    20,
                    "❯ /clear",
                    CapturedStyle::Runs(vec![(2, 7, CAPTURED_SELECTED)]),
                ),
                captured(21, "? for shortcuts", CapturedStyle::Plain),
                captured(
                    22,
                    "/clear                        Start a new session with empty context;",
                    CapturedStyle::Runs(vec![(0, 68, CAPTURED_SELECTED)]),
                ),
            ],
            20,
            8,
        );
        assert_selection_refused(&screen, "menu_not_rendered");
    }

    /// Absent evidence is not benign evidence.
    ///
    /// If the colours were ever lost — a capture path that dropped them, a
    /// terminal that reports none — every row would be uniform in the same
    /// terminal default, and a predicate that only asked "is this row uniform"
    /// would call the first candidate selected. On a screen filtered down to a
    /// single `/clear` that reads as a proof, and it would be a proof of
    /// nothing. The selected shape therefore requires a colour to have been
    /// EXPLICITLY selected, so a colourless capture refuses.
    #[test]
    fn a_capture_that_carries_no_colour_at_all_proves_no_selection() {
        let screen = captured_screen(
            14,
            vec![
                captured(9, "❯ /clear", CapturedStyle::Plain),
                menu_rule_row(10),
                captured(
                    11,
                    "/clear                        Start a new session with empty context;",
                    CapturedStyle::Plain,
                ),
            ],
            9,
            8,
        );
        let error = assert_selection_refused(&screen, "menu_selection_not_unique");
        assert_eq!(error.details["highlighted_rows"], 0);
    }

    /// THE RESIDUE. VERBATIM CAPTURE with a project command at
    /// `.claude/commands/clear.md`: two entries are named `/clear`, the
    /// project-local one sorts ABOVE the built-in and is the highlighted row.
    /// Every check passes. MEASURED, Enter on this screen served a real model
    /// turn, rotated nothing, left the instance dirty, and surfaced to the
    /// caller as a rebind timeout.
    ///
    /// This test asserts the proof ACCEPTS that screen. It is here so the gap is
    /// a recorded fact rather than an unstated assumption: nothing on the screen
    /// distinguishes the two entries except description prose. Closing it is a
    /// launch-side job — no user, project or plugin command may shadow `/clear`
    /// — and the shipped launch bundle does not do it yet.
    #[test]
    fn a_project_command_that_shadows_clear_is_not_ruled_out_by_this_proof() {
        let shadowed = captured_screen(
            5,
            vec![
                captured(
                    9,
                    "❯ /clear",
                    CapturedStyle::Runs(vec![(2, 7, CAPTURED_SELECTED)]),
                ),
                menu_rule_row(10),
                captured(
                    11,
                    "/clear                        Project-local command deliberately named clear",
                    CapturedStyle::Runs(vec![(0, 75, CAPTURED_SELECTED)]),
                ),
                captured(
                    12,
                    "                              for a collision probe (project)",
                    CapturedStyle::Runs(vec![(0, 60, CAPTURED_SELECTED)]),
                ),
                captured(
                    13,
                    "/clear                        Start a new session with empty context;",
                    CapturedStyle::Runs(vec![
                        (0, 0, CAPTURED_UNSELECTED),
                        (1, 5, CAPTURED_SELECTED),
                        (6, 6, CAPTURED_UNSELECTED),
                    ]),
                ),
            ],
            9,
            8,
        );
        prove_control_command_selection(&shadowed, ControlCommand::Clear).expect(
            "the proof cannot tell two commands with the same name apart; if this ever starts \
             failing, say what new evidence made it possible rather than deleting the test",
        );
        // The wrapped continuation line of the selected entry carries the same
        // highlight, and must not be counted as a second selected entry: it is
        // indented, so it offers no command.
        assert!(candidate_body_colour(shadowed.row(12)).is_some());
    }

    /// The precondition is a precondition: a refusal must leave Enter unsent.
    #[tokio::test]
    async fn an_unproven_selection_refuses_before_enter_and_is_never_retried() {
        let (control, handle) = clear_control_showing(
            measured_clear_menu_selecting(13, 73),
            Duration::from_secs(1),
        );
        let error = control
            .type_control_command(ControlCommand::Clear, test_deadline())
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::PromptNotAcknowledged);
        assert_eq!(error.details["reason"], "menu_selects_a_different_command");
        // One paste, NO Enter, and no second attempt at either.
        assert_eq!(handle.counts(), (1, 0));
        assert_eq!(handle.pasted_text(), vec![ControlCommand::Clear.literal()]);
        // `clear_and_rebind` reads this failure as one that typed nothing, which
        // is what keeps the bound transcript unpoisoned.
        assert!(!enter_was_attempted(&error));
        assert!(clear_was_not_submitted(
            &refusal_before_clear_submission(error).details
        ));
    }

    /// Belt AND braces, in one run. The pre-Enter proof is a precondition, not a
    /// replacement: on a screen it accepts, Enter is sent, and the post-hoc
    /// `wrong_local_command` clause still gets to read what Claude wrote down
    /// and still refuses a rotation opened by another command.
    ///
    /// The two layers cover different failures and neither subsumes the other.
    /// This one catches the commands that DO rotate, after the fact; the
    /// pre-Enter proof catches the ones that do not rotate at all, before the
    /// fact, which is the only place they can be caught.
    #[tokio::test]
    async fn the_pre_enter_proof_did_not_replace_the_post_hoc_command_check() {
        let fixture = RotationFixture::new();
        fixture.open_transcript(fixture.launch_session);
        let rotated_session = SessionId::new_v4();
        let rotated_path = fixture.transcript_path(rotated_session);
        let preamble = fixture.cleared_preamble(rotated_session, "/model");
        let (control, handle) =
            fixture.clearing_terminal(move || write_rows(&rotated_path, &preamble));
        let error = clear_and_rebind(
            &control,
            &fixture.source,
            fixture.launch_session,
            test_deadline(),
        )
        .await
        .unwrap_err();
        assert_assert_empty_refusal(&error, "wrong_local_command");
        assert_eq!(error.details["command_name"], "/model");
        // Enter really was sent: the captured menu proved its selection, so this
        // refusal came from the transcript and not from the screen.
        assert_eq!(handle.counts(), (1, 1));
    }

    #[tokio::test]
    async fn the_control_channel_refuses_a_terminal_that_is_not_idle_and_empty() {
        // A composer that is not proven stable, empty and cursor-anchored is
        // either mid-turn or in some state the pool's model does not cover.
        // `/clear` typed into it would concatenate with whatever is there, so
        // Gate 1 is what keeps the control channel from ever interleaving with a
        // turn -- no separate lock is required.
        let busy = rendered_snapshot(1, "❯ already typing", 18);
        let (control, handle) = control_for(
            [busy.clone(), busy],
            [baseline_snapshot(2)],
            Duration::ZERO,
            Duration::from_millis(20),
        );
        let error = control
            .type_control_command(ControlCommand::Clear, test_deadline())
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::PromptNotAcknowledged);
        assert_eq!(handle.counts(), (0, 0));

        let modal = structured_snapshot(
            1,
            ["Do you trust the files in this folder?", "yes", "no"],
            2,
            0,
            true,
        );
        let (control, handle) = control_for(
            [modal.clone(), modal],
            [baseline_snapshot(2)],
            Duration::ZERO,
            Duration::from_millis(20),
        );
        assert_eq!(
            control
                .type_control_command(ControlCommand::Clear, test_deadline())
                .await
                .unwrap_err()
                .code,
            ErrorCode::NeedsTrust
        );
        assert_eq!(handle.counts(), (0, 0));
    }

    /// Every phrase [`blocking_screen`] recognizes, as the ALTERNATIVES it
    /// recognizes them in: one row per independent way a screen reaches one
    /// kind, and every phrase a row needs together inside that row.
    ///
    /// MUTATION EVIDENCE, full-scope run at `1882dee`: the table beside
    /// `prompt_and_modal_classification_are_conservative` holds one screen per
    /// kind, so the FIRST phrase of each arm was the only one any test could
    /// see. Ten mutants inside this one classifier survived the whole suite --
    /// each of them replacing a `||` with `&&` or an `&&` with `||` -- and
    /// under any of them pmux answers `unknown` to a real "trust this
    /// directory", "not logged in", "please update claude code",
    /// "quota exceeded" or "esc to cancel / press enter" screen. An `unknown`
    /// screen is not a refusal: the caller's turn runs on into its deadline
    /// with the instance sitting on a modal.
    ///
    /// This is a hand-written table of a hand-written predicate, which is the
    /// same defect one level up, so it is not left as one:
    /// [`the_blocking_phrase_table_holds_every_phrase_the_classifier_names`]
    /// reads the classifier's own source and fails if it names a phrase this
    /// table does not.
    const BLOCKING_SCREEN_ALTERNATIVES: &[(NeedsInputKind, &[&str])] = &[
        (NeedsInputKind::Trust, &["do you trust the files"]),
        (NeedsInputKind::Trust, &["trust this folder"]),
        (NeedsInputKind::Trust, &["trust this directory"]),
        (NeedsInputKind::Login, &["log in to claude"]),
        (NeedsInputKind::Login, &["login to claude"]),
        (NeedsInputKind::Login, &["not logged in"]),
        (
            NeedsInputKind::Login,
            &["authentication required", "claude"],
        ),
        (NeedsInputKind::Update, &["update required"]),
        (NeedsInputKind::Update, &["please update claude code"]),
        (
            NeedsInputKind::Update,
            &["new version of claude code is required"],
        ),
        (NeedsInputKind::Quota, &["usage limit exceeded"]),
        (NeedsInputKind::Quota, &["usage limit reached"]),
        (NeedsInputKind::Quota, &["rate limit", "exceeded"]),
        (NeedsInputKind::Quota, &["rate limit", "reached"]),
        (NeedsInputKind::Quota, &["quota", "exceeded"]),
        (NeedsInputKind::Quota, &["quota", "reached"]),
        (NeedsInputKind::Permission, &["permission", "allow"]),
        (NeedsInputKind::Permission, &["permission", "deny"]),
        (NeedsInputKind::Permission, &["allow this tool"]),
        (
            NeedsInputKind::Permission,
            &["do you want to proceed", "yes", "no"],
        ),
        (
            NeedsInputKind::UnknownModal,
            &["esc to cancel", "enter to confirm"],
        ),
        (
            NeedsInputKind::UnknownModal,
            &["esc to cancel", "enter to select"],
        ),
        (
            NeedsInputKind::UnknownModal,
            &["esc to cancel", "press enter"],
        ),
        (
            NeedsInputKind::UnknownModal,
            &["esc to cancel", "yes", "no"],
        ),
    ];

    /// Each alternative fires ON ITS OWN, and each phrase inside one is
    /// load-bearing.
    ///
    /// The second half is the half a table of one example per kind cannot
    /// state: an arm that reads `a && b` is indistinguishable from `a || b`
    /// until some screen carries `a` without `b`.
    #[test]
    fn every_blocking_phrase_alternative_is_recognized_and_needs_all_of_its_phrases() {
        for (kind, phrases) in BLOCKING_SCREEN_ALTERNATIVES {
            let screen = phrases.join("\n");
            let observed = blocking_screen(&screen)
                .unwrap_or_else(|| panic!("{phrases:?} reached no modal arm at all"));
            assert_eq!(observed.kind, *kind, "{phrases:?}");
            // The classifier lowercases first, so the table holds the lowercase
            // of what Claude paints and the screen may hold any case at all.
            assert_eq!(
                blocking_screen(&screen.to_uppercase()).map(|needs| needs.kind),
                Some(*kind),
                "{phrases:?} in upper case",
            );
            if phrases.len() < 2 {
                continue;
            }
            for dropped in 0..phrases.len() {
                let remainder: Vec<&str> = phrases
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| *index != dropped)
                    .map(|(_, phrase)| *phrase)
                    .collect();
                assert_ne!(
                    blocking_screen(&remainder.join("\n")).map(|needs| needs.kind),
                    Some(*kind),
                    "{remainder:?} reached {kind:?} without {:?}",
                    phrases[dropped],
                );
            }
        }
    }

    /// The table above, held to the classifier's own source.
    ///
    /// `blocking_screen` holds no string literal that is not a phrase it
    /// matches on, so the set of phrases IS the set of literals in its body.
    /// Deriving the completeness check from the function rather than from a
    /// second reading of it is the only version of this test that cannot fall
    /// behind: a phrase added to the classifier tomorrow fails here until it is
    /// given an alternative, and it then acquires the positive and negative
    /// cases above for free.
    #[test]
    fn the_blocking_phrase_table_holds_every_phrase_the_classifier_names() {
        const OPENING: &str = "fn blocking_screen(visible_text: &str) -> Option<NeedsInput> {";
        let body = include_str!("driver_io.rs")
            .split_once(OPENING)
            .expect("this module defines blocking_screen")
            .1
            .split_once("\n}\n")
            .expect("the classifier's body closes at column zero")
            .0;
        let known: BTreeSet<&str> = BLOCKING_SCREEN_ALTERNATIVES
            .iter()
            .flat_map(|(_, phrases)| phrases.iter().copied())
            .collect();
        let mut named = BTreeSet::new();
        let mut rest = body;
        while let Some((_, after_open)) = rest.split_once('"') {
            let (literal, after_close) = after_open
                .split_once('"')
                .expect("a string literal in the classifier closes");
            named.insert(literal);
            rest = after_close;
        }
        assert!(
            !named.is_empty(),
            "the classifier's phrases could not be read out of its source"
        );
        let unlisted: Vec<&&str> = named.difference(&known).collect();
        assert!(
            unlisted.is_empty(),
            "blocking_screen matches {unlisted:?}, which BLOCKING_SCREEN_ALTERNATIVES does not list"
        );
    }

    /// The composer is located by the cursor, so a cursor outside the grid the
    /// same snapshot reports is not a composer.
    ///
    /// MUTATION EVIDENCE: `active_editor`'s column bound could be conjoined
    /// with its row bound and nothing noticed. Under that mutant a snapshot
    /// reporting 80 columns and a cursor at column 80 resolves an editor whose
    /// `cursor_col_from_prompt` is off the screen -- and every geometric proof
    /// downstream is stated in that number.
    #[test]
    fn a_cursor_outside_the_reported_grid_resolves_no_editor() {
        let inside = structured_snapshot(3, ["", "", "❯ ", "footer", "status"], 2, 2, true);
        assert!(screen_geometry(&inside).is_some());
        for column in [inside.cols, inside.cols + 1] {
            let past_last_column = TerminalSnapshot {
                cursor: inside.cursor.map(|mut cursor| {
                    cursor.col = column;
                    cursor
                }),
                ..inside.clone()
            };
            assert_eq!(
                screen_geometry(&past_last_column),
                None,
                "cursor at column {column} of {} resolved an editor",
                inside.cols
            );
        }
        let past_last_row = TerminalSnapshot {
            cursor: inside.cursor.map(|mut cursor| {
                cursor.row = inside.rows;
                cursor
            }),
            ..inside.clone()
        };
        assert_eq!(screen_geometry(&past_last_row), None);
        // A grid with no cells is the same statement about the same two bounds:
        // every cursor is outside it.
        assert_eq!(
            screen_geometry(&TerminalSnapshot {
                rows: 0,
                cols: 0,
                ..inside
            }),
            None
        );
    }

    /// MEASURED: live Claude leaves the cursor exactly two cells after the
    /// glyph on an empty composer. A cursor ON the glyph, or in the single cell
    /// after it, is a rendering this module has never measured and refuses.
    ///
    /// MUTATION EVIDENCE: inverting that comparison let a cursor sitting on the
    /// glyph resolve an editor, which `composer_head` would then read a buffer
    /// out of.
    #[test]
    fn a_cursor_on_the_prompt_glyph_is_not_a_resolved_composer() {
        for column in [0, 1] {
            let snapshot =
                structured_snapshot(3, ["", "", "❯ ", "footer", "status"], 2, column, true);
            assert_eq!(
                screen_geometry(&snapshot),
                None,
                "a cursor at column {column} of the anchor row resolved an editor"
            );
        }
        let anchored = structured_snapshot(3, ["", "", "❯ ", "footer", "status"], 2, 2, true);
        assert!(
            screen_geometry(&anchored).is_some_and(|geometry| geometry.empty_cursor_position),
            "the measured empty composer must still resolve"
        );
        // The same rule read from the other side: a cursor two cells past the
        // glyph on a CONTINUATION row is a wrapped composer, and the column
        // rule is about the anchor row only.
        let wrapped = composer_snapshot(3, &["❯\u{a0}first", "  second"]);
        assert!(screen_geometry(&wrapped).is_some());
    }

    /// `❯` with text against it is a transcript row, not a composer anchor.
    ///
    /// MUTATION EVIDENCE: `prompt_glyph_col`'s two rejections could be
    /// conjoined, and since the first of them is unreachable -- `find` located
    /// the glyph, so the glyph is there -- the conjunction is never true and
    /// the separator rule stops existing. A historical `❯reply` row would then
    /// anchor the editor the render proof is stated against.
    #[test]
    fn a_glyph_with_text_against_it_is_not_a_composer_anchor() {
        assert_eq!(prompt_glyph_col("❯text"), None);
        assert_eq!(prompt_glyph_col("  ❯text"), None);
        assert_eq!(prompt_glyph_col("  ❯ text"), Some(2));
        assert_eq!(prompt_glyph_col("  ❯\u{a0}text"), Some(2));
        assert_eq!(prompt_glyph_col("x ❯ text"), None);
        let against = structured_snapshot(3, ["", "", "❯text", "footer", "status"], 2, 5, true);
        assert_eq!(screen_geometry(&against), None);
    }

    /// [`composer_head`] removes the glyph and the one cell
    /// [`prompt_glyph_split`] stepped over -- BY STEPPING OVER IT, rather than
    /// by deciding a second time which cell that was.
    ///
    /// MUTATION EVIDENCE: the whitespace test that used to stand here was that
    /// second decision, and it was unfalsifiable. The anchor rule admits a row
    /// only when the cell after the glyph is whitespace or absent, so the test
    /// was true on every row that could reach it and replacing it with `true`
    /// changed no answer anywhere in the suite. The rule is now stated once and
    /// the head is what its own iterator has left, which is why this test can
    /// state all three measured separators without a fourth statement of the
    /// rule to keep them honest.
    #[test]
    fn the_composer_head_removes_exactly_what_the_glyph_rule_stepped_over() {
        // MEASURED at 2.1.226 the separator is U+00A0; a plain space is the
        // same rule's other admitted shape.
        for row in ["❯\u{a0}explain this", "❯ explain this"] {
            let snapshot = composer_snapshot(2, &[row]);
            let editor = active_editor(&snapshot).expect("a populated composer resolves");
            assert_eq!(composer_head(&editor), Some("explain this"), "row {row:?}");
        }
        // MEASURED at 2.1.70: an empty composer is the glyph and nothing after
        // it, which is why the separator is optional rather than a literal.
        let empty = structured_snapshot(2, ["", "", "❯", "footer", "status"], 2, 2, true);
        let editor = active_editor(&empty).expect("the measured empty composer resolves");
        assert!(editor.empty_cursor_position);
        assert_eq!(composer_head(&editor), Some(""));
    }

    /// The frame Enter is sent on must be the frame that was FENCED, not
    /// another frame that would also have passed the proof.
    ///
    /// MUTATION EVIDENCE: two of the three conjuncts in that fence could each
    /// be turned into an `||` and the suite stayed green, because the only
    /// test that repaints between the proof and the fence repaints a composer
    /// that never held the prompt -- so the other two conjuncts were false and
    /// the mutants had nothing to admit. Here both frames prove the prompt and
    /// carry one editor signature, and the only thing that separates them is
    /// the row below the composer. Under either mutant Enter is typed against
    /// a screen no fence ever saw.
    #[tokio::test]
    async fn a_fence_that_is_not_the_proven_frame_never_sends_enter() {
        let prompt = "explain this";
        let rendered = composer_snapshot(2, &["❯\u{a0}explain this"]);
        let repainted = TerminalSnapshot {
            revision: 3,
            visible_text: rendered.visible_text.replace("status", "status one"),
            ..rendered.clone()
        };
        assert_eq!(
            active_editor(&rendered).map(|editor| editor.signature),
            active_editor(&repainted).map(|editor| editor.signature),
            "the two frames must differ only below the composer"
        );
        let baseline = baseline_snapshot(1);
        let (control, handle) = control_for(
            [baseline.clone(), baseline],
            [rendered, repainted],
            Duration::ZERO,
            Duration::from_millis(12),
        );
        handle.state.lock().unwrap().cycle_after = true;
        assert_eq!(
            submit(&control, prompt).await.unwrap_err().code,
            ErrorCode::PromptNotAcknowledged
        );
        assert_eq!(handle.counts(), (1, 0));
    }

    /// A composer that moved BOTH ways at once is not the composer the fence
    /// proved empty, whatever it now holds.
    ///
    /// The geometry rule admits exactly two shapes, both MEASURED: a bottom-
    /// anchored frame whose anchor rises while the cursor stays, and a
    /// top-anchored one whose anchor stays while the cursor descends. Each is
    /// one row pinned and one row moving, and it is the pinned row that makes
    /// the frame the same frame.
    ///
    /// MUTATION EVIDENCE: the pinned row of the first shape could be turned
    /// into an alternative -- `cursor stayed OR anchor rose` -- and no test
    /// noticed. Under that mutant a frame in which everything moved proves the
    /// prompt, which is to say the render proof stops being about the editor
    /// the paste went into at all.
    #[tokio::test]
    async fn a_composer_whose_anchor_and_cursor_both_moved_is_not_this_prompt_rendered() {
        let baseline = baseline_snapshot(1);
        let fence_editor = active_editor(&baseline).expect("the fence resolves an editor");
        let elsewhere = structured_snapshot(
            2,
            ["", "", "", "❯\u{a0}first", "  second", "footer", "status"],
            4,
            8,
            true,
        );
        let moved = active_editor(&elsewhere).expect("the moved composer resolves an editor");
        assert_ne!(moved.anchor_row, fence_editor.anchor_row);
        assert_ne!(moved.cursor_row, fence_editor.cursor_row);
        assert!(
            !rendered_prompt_is_proven(
                &elsewhere,
                baseline.revision,
                &fence_editor,
                "first\nsecond"
            ),
            "a composer that moved on both axes is a different frame"
        );

        // The same rows under the fence's own geometry ARE this prompt: the
        // refusal above is the geometry clause and not the row comparison.
        let here = composer_snapshot(2, &["❯\u{a0}first", "  second"]);
        assert!(rendered_prompt_is_proven(
            &here,
            baseline.revision,
            &fence_editor,
            "first\nsecond"
        ));
    }

    /// Both axes of the resize guard, and the accepted case that says the
    /// guard is a test of the zero and not of the call.
    ///
    /// MUTATION EVIDENCE: all three operators in `rows == 0 || cols == 0`
    /// survived. Two of them make every resize a refusal; the third lets a
    /// zero-sized pane through to the backend, which is a pane no composer can
    /// render in and therefore a cell no render proof can ever pass.
    #[tokio::test]
    async fn a_resize_to_a_zero_dimension_is_refused_on_either_axis() {
        let (control, _handle) = ready_control(Duration::from_millis(30));
        for (rows, cols) in [(0, 24), (24, 0), (0, 0)] {
            assert_eq!(
                control.resize(rows, cols).await.unwrap_err().code,
                ErrorCode::InvalidConfig,
                "resize({rows}, {cols})"
            );
        }
        control
            .resize(24, 80)
            .await
            .expect("a resize with two non-zero dimensions is not the refused shape");
    }

    /// `rows` with one padded allowlisted metadata row appended, so that the
    /// file [`write_rows`] writes out of it is exactly `bytes` long.
    ///
    /// Derived from what the writer will actually produce rather than from a
    /// count typed beside it: the assertion at the end is the whole point of
    /// the helper, since a boundary test that misses the boundary by one byte
    /// tests the interior.
    fn padded_to(rows: &[Value], session_id: SessionId, bytes: usize) -> Vec<Value> {
        fn written(rows: &[Value]) -> usize {
            rows.iter().map(|row| row.to_string().len() + 1).sum()
        }
        let mut rows = rows.to_vec();
        let mut pad = json!({"type": "last-prompt", "sessionId": session_id, "pad": ""});
        let overhead = written(&rows) + pad.to_string().len() + 1;
        assert!(
            overhead <= bytes,
            "the preamble alone is longer than {bytes}"
        );
        pad["pad"] = json!("x".repeat(bytes - overhead));
        rows.push(pad);
        assert_eq!(written(&rows), bytes);
        rows
    }

    /// The row budget is a BOUNDARY, and the only test it had stood one row
    /// beyond it -- a position from which `>` and `>=` say the same thing. The
    /// mutant that tightens it by one refuses a preamble Claude is free to
    /// write, and quarantines a healthy instance for it.
    #[tokio::test]
    async fn a_successor_at_exactly_the_row_budget_is_admitted() {
        let fixture = RotationFixture::new();
        let rotated_session = SessionId::new_v4();
        let mut rows = fixture.clear_preamble(rotated_session);
        while rows.len() < MAX_ASSERT_EMPTY_ROWS {
            rows.push(json!({"type": "last-prompt", "sessionId": rotated_session}));
        }
        assert_eq!(rows.len(), MAX_ASSERT_EMPTY_ROWS);
        assert_eq!(
            clear_writing(&fixture, rotated_session, rows)
                .await
                .unwrap(),
            rotated_session
        );
    }

    /// Three preamble `user` rows, every one of them individually admissible,
    /// so the only thing that can refuse the third is the COUNT.
    ///
    /// MUTATION EVIDENCE: the counter could be made to stay at zero and the
    /// suite stayed green, because the only test that reached this refusal
    /// reached it with a row the classifier refuses anyway -- both spell
    /// `unexpected_user_row`, so the budget was never the clause under test.
    #[tokio::test]
    async fn a_third_preamble_user_row_is_refused_by_the_user_row_budget() {
        let fixture = RotationFixture::new();
        let rotated_session = SessionId::new_v4();
        let mut rows = fixture.clear_preamble(rotated_session);
        let caveat = rows[2].clone();
        rows.insert(3, caveat);
        let error = clear_writing(&fixture, rotated_session, rows)
            .await
            .unwrap_err();
        assert_assert_empty_refusal(&error, "unexpected_user_row");
        assert_eq!(error.details["row_kind"], "user_other");
    }

    /// The byte budget is a boundary AND a precondition for the read: a file
    /// one byte over it is refused FOR BEING OVER IT, not read up to the budget
    /// and judged on the prefix.
    ///
    /// MUTATION EVIDENCE: `>` could become `==` and nothing noticed. Under that
    /// mutant a leaked transcript of any size larger than the budget skips the
    /// refusal, the bounded read stops mid-row, and the rows that survive the
    /// truncation are the preamble -- so the file is judged on a prefix chosen
    /// by the budget rather than on its contents. It could also become `>=`,
    /// which refuses a transcript exactly at the budget that the loop below
    /// would have accepted.
    #[tokio::test]
    async fn the_assert_empty_byte_budget_refuses_rather_than_judging_a_prefix() {
        let budget = usize::try_from(MAX_ASSERT_EMPTY_BYTES).unwrap();

        let at_budget = RotationFixture::new();
        let rotated_session = SessionId::new_v4();
        let rows = padded_to(
            &at_budget.clear_preamble(rotated_session),
            rotated_session,
            budget,
        );
        assert_eq!(
            clear_writing(&at_budget, rotated_session, rows)
                .await
                .unwrap(),
            rotated_session
        );

        let over_budget = RotationFixture::new();
        let rotated_session = SessionId::new_v4();
        let rows = padded_to(
            &over_budget.clear_preamble(rotated_session),
            rotated_session,
            budget + 1,
        );
        let error = clear_writing(&over_budget, rotated_session, rows)
            .await
            .unwrap_err();
        assert_assert_empty_refusal(&error, "byte_budget_exceeded");
        assert_eq!(error.details["bytes"], json!(MAX_ASSERT_EMPTY_BYTES + 1));
    }

    /// A transcript whose records end `\r\n`.
    ///
    /// Claude writes `\n` and this is not a claim that it will not; the clause
    /// that strips the `\r` is there because a JSONL reader that judges records
    /// must judge the record and not the line ending. It had no test, and the
    /// mutant that turns its slice into an out-of-range one is a panic inside
    /// the daemon rather than a refusal.
    #[tokio::test]
    async fn a_successor_written_with_carriage_returns_is_still_proven_inert() {
        let fixture = RotationFixture::new();
        let rotated_session = SessionId::new_v4();
        fixture.open_transcript(fixture.launch_session);
        let rotated_path = fixture.transcript_path(rotated_session);
        let preamble = fixture.clear_preamble(rotated_session);
        let (control, _handle) = fixture.clearing_terminal(move || {
            let mut file = std::fs::File::create(&rotated_path).unwrap();
            for row in &preamble {
                write!(file, "{row}\r\n").unwrap();
            }
            file.flush().unwrap();
        });
        assert_eq!(
            clear_and_rebind(
                &control,
                &fixture.source,
                fixture.launch_session,
                test_deadline(),
            )
            .await
            .unwrap(),
            rotated_session
        );
    }

    /// A project directory holds whatever the filesystem put in it. Only a
    /// regular file whose extension is `jsonl` is a transcript, and the two
    /// halves of that sentence are one `&&` that could be an `||`.
    ///
    /// Under that mutant a directory named `*.jsonl`, or any ordinary file
    /// beside the transcripts, becomes a candidate transcript -- and the
    /// rotation scan's whole answer is which candidates are NEW.
    #[test]
    fn only_a_regular_file_named_jsonl_is_a_transcript() {
        let directory = TempDir::new().unwrap();
        let transcript = directory.path().join("a.jsonl");
        let shouted = directory.path().join("b.JSONL");
        std::fs::write(&transcript, b"").unwrap();
        std::fs::write(&shouted, b"").unwrap();
        std::fs::write(directory.path().join("notes.txt"), b"").unwrap();
        std::fs::write(directory.path().join("jsonl"), b"").unwrap();
        std::fs::create_dir(directory.path().join("nested.jsonl")).unwrap();
        assert_eq!(
            list_transcripts(directory.path()).unwrap(),
            BTreeSet::from([transcript, shouted])
        );
    }

    /// The bounded scan admits a directory holding exactly its bound and
    /// refuses the entry after it.
    ///
    /// Both halves are load-bearing and neither was observed before. `scanned >
    /// limit` reading `>=` refuses a directory that is inside the bound -- a
    /// rebind that returns `SchemaDrift` instead of the successor transcript,
    /// which quarantines a healthy pooled instance. `scanned += 1` reading `*=`
    /// pins the counter at zero, so the bound never fires at all and the scan
    /// this constant exists to bound is unbounded.
    ///
    /// The bound under test is [`list_transcripts_within`]'s parameter, not the
    /// constant: at 20,000 entries the second half alone is 20,001 files, built
    /// once per mutant.
    #[test]
    fn the_rotation_scan_admits_its_bound_and_refuses_the_entry_past_it() {
        const LIMIT: usize = 3;
        let directory = TempDir::new().unwrap();
        let mut expected = BTreeSet::new();
        for index in 0..LIMIT {
            let transcript = directory.path().join(format!("{index}.jsonl"));
            std::fs::write(&transcript, b"").unwrap();
            expected.insert(transcript);
        }
        assert_eq!(
            list_transcripts_within(directory.path(), LIMIT).unwrap(),
            expected,
            "a directory holding exactly the bound is inside it"
        );

        let past = directory.path().join("one-too-many.jsonl");
        std::fs::write(&past, b"").unwrap();
        let refusal = list_transcripts_within(directory.path(), LIMIT).unwrap_err();
        assert_eq!(refusal.code, ErrorCode::SchemaDrift);
        assert_eq!(refusal.details["limit"], json!(LIMIT));
    }

    /// The abandoned-rotation ledger holds `MAX_REMEMBERED_ROTATIONS` records
    /// and retires the oldest to make room for the next.
    ///
    /// Stated over the constant rather than over eight, so the property is the
    /// bound and not a number copied out of it. `len() > bound` read as `>=`
    /// or as `==` both settle one record short, and what is lost is the named
    /// rotation diagnostic -- the arm that turns a bare `TurnTimeout` into
    /// "this session was abandoned by `/clear` and rebound to that one".
    #[test]
    fn the_rotation_ledger_remembers_its_bound_and_retires_the_oldest_first() {
        let root = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let session_id = SessionId::new_v4();
        let source = FileTranscriptSource::new(root.path(), cwd.path(), session_id).unwrap();
        let abandoned: Vec<SessionId> = (0..=MAX_REMEMBERED_ROTATIONS)
            .map(|_| SessionId::new_v4())
            .collect();
        for (index, id) in abandoned[..MAX_REMEMBERED_ROTATIONS].iter().enumerate() {
            source.record_rotation(*id, None).unwrap();
            assert!(
                source.rotation_record(*id).unwrap().is_some(),
                "rotation {index} was not recorded at all"
            );
        }
        assert!(
            source.rotation_record(abandoned[0]).unwrap().is_some(),
            "a ledger holding exactly its bound retired the oldest anyway"
        );

        source
            .record_rotation(abandoned[MAX_REMEMBERED_ROTATIONS], None)
            .unwrap();
        assert!(
            source.rotation_record(abandoned[0]).unwrap().is_none(),
            "the record past the bound did not retire the oldest"
        );
        for (index, id) in abandoned[1..].iter().enumerate() {
            assert!(
                source.rotation_record(*id).unwrap().is_some(),
                "rotation {} was retired before the oldest one",
                index + 1
            );
        }
    }

    /// A styled frame carrying one row of text and one revision, small enough
    /// to build a hundred of.
    fn revision_screen(revision: u64, text: &str) -> StyledScreen {
        let cells: Vec<Vec<StyledCell>> = vec![
            text.chars()
                .map(|glyph| StyledCell::new(glyph.to_string(), CellColor::Unstyled))
                .collect(),
        ];
        StyledScreen::new(
            revision,
            1,
            u16::try_from(text.chars().count()).unwrap(),
            Some(TerminalCursor {
                row: 0,
                col: 0,
                visible: true,
                style: 0,
            }),
            cells,
        )
    }

    /// [`wait_for_stable_control_render`] driven directly, so each clause of its
    /// predicate can be asked its own question.
    async fn control_render(
        screens: impl IntoIterator<Item = StyledScreen>,
        baseline: &TerminalSnapshot,
        stable_for: Duration,
        gate_timeout: Duration,
    ) -> DriverResult<StyledScreen> {
        let (mut terminal, handle) = FakeTerminal::new([baseline.clone()], [baseline.clone()]);
        handle.serve_styled(screens);
        let budget = InputGateBudget::new(test_deadline(), gate_timeout)
            .expect("a two-second deadline leaves a gate budget");
        wait_for_stable_control_render(
            &mut terminal,
            &budget,
            baseline,
            stable_for,
            Duration::from_millis(1),
        )
        .await
    }

    /// Gate 2 on the control channel claims the frame it hands the selection
    /// proof was PAINTED AFTER the fence. Two of the three clauses that say so
    /// could be turned off with one operator each and the whole workspace
    /// stayed green, because every existing control fixture serves a frame that
    /// satisfies all three at once.
    ///
    /// A frame that is not new is the pre-`/clear` screen, and proving a
    /// selection from it is proving that the composer Enter is about to act on
    /// holds a command that has not been typed yet.
    #[tokio::test]
    async fn the_control_render_gate_refuses_a_frame_the_fence_already_saw() {
        // Byte-identical to the fence: nothing has been drawn yet.
        let unchanged = revision_screen(7, "working");
        let baseline = unchanged.to_terminal_snapshot();
        assert_eq!(
            control_render(
                [unchanged],
                &baseline,
                Duration::ZERO,
                Duration::from_millis(30)
            )
            .await
            .unwrap_err()
            .code,
            ErrorCode::PromptNotAcknowledged
        );

        // A different frame REUSING the fence's revision. rmux's revision counts
        // paints, so a frame reporting the fence's count is the fence's paint
        // whatever its text says -- and text alone is not evidence of a repaint.
        let restyled = revision_screen(7, "the command menu");
        assert_ne!(restyled.to_terminal_snapshot(), baseline);
        assert_eq!(
            control_render(
                [restyled],
                &baseline,
                Duration::ZERO,
                Duration::from_millis(30)
            )
            .await
            .unwrap_err()
            .code,
            ErrorCode::PromptNotAcknowledged
        );

        // And the frame that is genuinely new settles.
        let painted = revision_screen(8, "the command menu");
        assert_eq!(
            control_render(
                [painted.clone()],
                &baseline,
                Duration::ZERO,
                Duration::from_millis(200)
            )
            .await
            .unwrap(),
            painted
        );
    }

    /// The frame handed to the selection proof must have HELD STILL, and the
    /// comparison that decides whether two polls saw the same screen is the
    /// whole of that. Inverting it inverts the meaning of stability: a screen
    /// repainting on every poll settles immediately, and one holding perfectly
    /// still never settles at all.
    ///
    /// A menu still animating is exactly when the highlighted row is about to
    /// move, which is the mis-selection the pre-Enter proof exists to catch.
    #[tokio::test]
    async fn the_control_render_gate_never_settles_on_a_screen_that_keeps_repainting() {
        let baseline = revision_screen(1, "working").to_terminal_snapshot();
        let repainting = (2..120).map(|revision| revision_screen(revision, "the command menu"));
        assert_eq!(
            control_render(
                repainting,
                &baseline,
                Duration::from_millis(10),
                Duration::from_millis(40)
            )
            .await
            .unwrap_err()
            .code,
            ErrorCode::PromptNotAcknowledged
        );

        // The same budget and the same quiet window, spent on a screen that
        // stops changing: this settles, so the refusal above is the stability
        // rule and not the budget being too small to settle in.
        let settling = std::iter::once(revision_screen(2, "the command menu"))
            .chain((3..6).map(|revision| revision_screen(revision, "the command menu, settled")));
        assert!(
            control_render(
                settling,
                &baseline,
                Duration::from_millis(10),
                Duration::from_millis(400)
            )
            .await
            .is_ok()
        );
    }

    /// Every C0 and C1 control character, held to one rule stated over the
    /// whole domain instead of over the two code points that were named.
    ///
    /// MUTATION EVIDENCE: `validate_prompt`'s control-character clause could be
    /// conjoined with the two literal comparisons in front of it, leaving BEL,
    /// backspace, VT, DEL and the whole C1 range accepted into a prompt pmux
    /// types into a TUI -- and no test saw it, because the only control
    /// characters any test used were the two literals. The literals are also
    /// what made the mutant that removes THEM equivalent: U+0000 and U+001B are
    /// Cc, so the clause behind them already refused both.
    ///
    /// `\r` is the one exception and it is not an exception to the guard: the
    /// normalization in front of it has already turned it into `\n`.
    #[test]
    fn every_control_character_but_a_newline_is_refused_from_a_prompt() {
        for code in (0x00..=0x1f_u32).chain(0x7f..=0x9f) {
            let character = char::from_u32(code).expect("C0 and C1 are scalar values");
            let prompt = format!("a{character}b");
            let refused = validate_prompt(&prompt).err();
            let expected = !matches!(character, '\n' | '\r');
            assert_eq!(
                refused.is_some(),
                expected,
                "U+{code:04X} refused: {:?}",
                refused.as_ref().map(|error| &error.message)
            );
            // Where the composer rule has not already spoken, the refusal is
            // this guard's own -- so the sweep is measuring the clause it names
            // and not some earlier one.
            let normalized = pseudomux_claude::normalize_prompt(&prompt);
            if expected && pseudomux_claude::composer_refusal(&normalized).is_none() {
                let error = refused.expect("a control character is refused");
                assert_eq!(error.code, ErrorCode::InvalidConfig);
                assert!(
                    error.message.contains("unsafe control character"),
                    "U+{code:04X} was refused by some other rule: {}",
                    error.message
                );
            }
        }
    }
    /// A transcript file that exists and is empty is not a located transcript:
    /// the locator admits a candidate only on a COMPLETE record, so the arm
    /// takes the not-yet-written path and binds the beginning.
    ///
    /// This is also why `seek_to_observed_eof`'s `len > 0` cannot be reached at
    /// zero from here, and why the mutation run's `>= 0` at that line changes
    /// no answer this suite can produce: reaching it requires the file to be
    /// truncated between the locate that read a record and the stat beside it.
    #[tokio::test]
    async fn an_empty_transcript_file_is_not_locatable_and_arms_at_the_beginning() {
        let fixture = RotationFixture::new();
        std::fs::write(fixture.transcript_path(fixture.launch_session), b"").unwrap();
        let arm = fixture
            .source
            .arm_at_eof(fixture.launch_session)
            .await
            .expect("an empty transcript has a knowable boundary: the beginning");
        assert_eq!(arm.position.offset, 0);
    }

    /// The turn-deadline domain is a boundary, and the only test it had stood
    /// one millisecond past it.
    #[test]
    fn the_turn_deadline_domain_admits_the_last_safe_integer() {
        assert!(
            validate_turn_deadline_domain(MAX_SAFE_JSON_INTEGER).is_ok(),
            "the last representable millisecond is inside the domain"
        );
        assert_eq!(
            validate_turn_deadline_domain(MAX_SAFE_JSON_INTEGER + 1)
                .unwrap_err()
                .code,
            ErrorCode::InvalidConfig
        );
    }
}
