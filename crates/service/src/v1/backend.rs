use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use pseudomux_claude::ParsedRow;
use pseudomux_protocol::v1::{
    ClosePolicy, ErrorBody, ErrorCode, MAX_SAFE_JSON_INTEGER, NeedsInput, SessionId, TimestampMs,
    TurnId, validate_v1_serializable,
};

use crate::driver_io::{RecognisedScreen, ScreenShape};

// `Clock` predates fallible wall-clock access. This one-past-the-wire-domain
// sentinel is never public: the actor rejects it before constructing any v1
// timestamp. It avoids silently clamping an unavailable system time to a
// plausible timestamp.
const INVALID_CLOCK_TIMESTAMP: TimestampMs = MAX_SAFE_JSON_INTEGER + 1;

/// A structured backend failure that can cross an actor boundary without
/// collapsing protocol semantics into strings.
#[derive(Clone, Debug, PartialEq)]
pub struct DriverFailure {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
    pub details: serde_json::Value,
}

impl DriverFailure {
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: false,
            details: serde_json::Value::Null,
        }
    }

    #[must_use]
    pub const fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    #[must_use]
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = details;
        self
    }

    #[must_use]
    pub fn into_protocol(self) -> ErrorBody {
        let mut error = ErrorBody {
            code: self.code,
            message: self.message,
            retryable: self.retryable,
            details: self.details,
        };
        // Terminal implementations are an internal extension boundary and can
        // be supplied directly by Rust callers. Never let arbitrary diagnostic
        // JSON turn the public typed failure into a response-serialization
        // failure. The operation's code/message/retryability remain truthful;
        // only invalid optional details are discarded.
        if validate_v1_serializable(&error).is_err() {
            error.details = serde_json::Value::Null;
        }
        debug_assert!(validate_v1_serializable(&error).is_ok());
        error
    }
}

pub type DriverResult<T> = Result<T, DriverFailure>;

/// Time source used for protocol timestamps, deadlines, and idle metadata.
pub trait Clock: Send + Sync + 'static {
    fn now_ms(&self) -> u64;
}

#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        let milliseconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|elapsed| elapsed.as_millis());
        protocol_clock_milliseconds(milliseconds)
    }
}

fn protocol_clock_milliseconds(milliseconds: Option<u128>) -> TimestampMs {
    milliseconds
        .and_then(|value| u64::try_from(value).ok())
        .filter(|value| *value <= MAX_SAFE_JSON_INTEGER)
        .unwrap_or(INVALID_CLOCK_TIMESTAMP)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TerminalEvidence {
    pub ready_prompt: bool,
    pub quiet: bool,
    pub lifecycle_expected: bool,
    pub lifecycle_hook_observed: bool,
    /// UNIX-millisecond instant of the Stop/StopFailure hook reported by
    /// `lifecycle_hook_observed`, or `None` when this turn observed no hook.
    ///
    /// Measurement only: no completion decision reads it. It is published as
    /// `TurnTimings::stop_hook_at_ms` so the signed difference against
    /// `TurnTimings::last_transcript_activity_at_ms` can establish whether
    /// Claude flushes the transcript before firing Stop.
    ///
    /// Always `None` when `lifecycle_hook_observed` is false, so the instant
    /// can only ever describe the same observation the boolean reports; a stamp
    /// left by an earlier turn's hook must never be published against this
    /// turn's transcript activity.
    pub lifecycle_hook_at_ms: Option<u64>,
}

/// Redacted terminal state exposed to the session actor.
///
/// Raw screen contents are deliberately confined to the terminal adapter. The
/// actor only learns whether Claude is ready, is displaying a recognized
/// interactive screen, is on some other screen the adapter RECOGNIZES, or is on
/// one it does not.
///
/// The last two were one variant, `Other`, and the actor's turn monitor treated
/// it as "nothing to report" -- which is correct for a composer holding the
/// caller's own text and is exactly the silent-hang class for a screen no rule
/// matched. They are separated here, and not only inside the adapter, because
/// this is the type the DECISION is made on: an arm the actor cannot see is an
/// arm the actor cannot act on.
#[derive(Clone, Debug, PartialEq)]
pub enum TerminalScreenObservation {
    Ready,
    NeedsInput(NeedsInput),
    /// A screen the adapter recognizes as neither ready nor blocking.
    Recognised(RecognisedScreen),
    /// A screen no rule matched, described by its structural shape. Never its
    /// text: that stays in the adapter.
    Unrecognised(ScreenShape),
}

/// How long a running turn will sit on a screen no rule matched, with no
/// transcript row arriving, before the turn is refused.
///
/// # What this trades
///
/// Refusing more trades correctness for availability, and this is the trade:
/// a turn vetoed here costs the caller its answer and costs the pool its
/// instance, and a turn NOT vetoed here on a screen pmux cannot read runs to a
/// deadline of up to 600,000 ms and costs the same instance plus ten minutes.
/// The veto is the cheaper of the two failures and it is the only one that says
/// why.
///
/// # Where the number comes from
///
/// MEASURED at Claude Code 2.1.227, macOS/aarch64, over **24 real Sonnet 5
/// turns** across all three efforts (8 each at `low`, `medium`, `high`),
/// **4,415 recorded frames**, replayed through the production classifier by
/// `crates/service/examples/screen_census.rs`. Receipt:
/// `evidence/screen-veto-cost-2.1.227-macos-aarch64.json`.
///
/// * On the turn path -- `turn_monitor.observe`, the read this veto is decided
///   from -- the longest CONTINUOUS run of unrecognized frames was **0 ms**.
///   Not "short": there was not ONE unrecognized frame in 2,629 observations,
///   because a Claude that is working still renders its own empty composer and
///   that composer is what `Ready` is.
/// * The longest legitimate unrecognized run measured ANYWHERE was **844 ms**,
///   at `startup.wait_until_ready`, which is a cold pane that has not painted a
///   composer yet. That site does not consult this constant -- it has its own
///   bounded startup deadline -- but it is the only number in the corpus that
///   says how long a real Claude can render something pmux cannot name, so it is
///   the anchor rather than the turn path's uninformative zero.
/// * **0 of 24 turns were refused.** The false-refusal rate of this rule, on
///   this corpus, is zero.
///
/// 30,000 ms is ~35x that 844 ms. Stated plainly: the window is NOT derived from
/// the measurement, it is a bound set far above it, in the idiom
/// `MAX_RENDERED_ROWS_BELOW_ACTIVE_CURSOR` already uses for a screen constant --
/// the gate admits a large multiple of what was measured so the first frame
/// after a Claude release changes its rendering is REPORTED, not refused. A
/// silent hang costs ten minutes and says nothing; this costs 30 seconds and
/// names the screen, and buying that with a wide margin is the intended trade.
///
/// # What this run did NOT establish
///
/// The veto never fired, because no unrecognized frame was ever observed on the
/// turn path at all. So this corpus is strong evidence about the FALSE-refusal
/// rate and no evidence at all that the firing path behaves correctly against a
/// live Claude; that path is covered by unit tests only.
///
/// # What it is NOT derived from
///
/// It is deliberately not a fraction of the turn deadline. The deadline is the
/// caller's and may be 30 s or 600 s; how long a Claude can legitimately render
/// something pmux has no rule for is a property of Claude Code, and tying the
/// two would make the same screen acceptable on a long turn and refused on a
/// short one.
pub const UNRECOGNISED_SCREEN_VETO: Duration = Duration::from_millis(30_000);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterruptRecovery {
    RecoveredToReady,
    RecoveryFailed,
}

/// The actor's only authority for terminal mutation and terminal-side evidence.
#[async_trait]
pub trait TerminalControl: Send + Sync + 'static {
    async fn submit_prompt(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        prompt: &str,
        // Absolute protocol deadline. Implementations must recheck this at
        // their last irreversible input boundary (for Claude, immediately
        // before Enter), rather than relying only on the caller's timeout.
        deadline_unix_ms: TimestampMs,
    ) -> DriverResult<()>;

    async fn completion_evidence(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> DriverResult<TerminalEvidence>;

    /// Fresh ready/quiet evidence after an authenticated writable attachment
    /// disconnects. Lifecycle-hook fields are ignored for this use.
    async fn attach_reconciliation_evidence(
        &self,
        session_id: SessionId,
    ) -> DriverResult<TerminalEvidence> {
        self.completion_evidence(session_id, TurnId::nil()).await
    }

    /// Returns a redacted classification of the currently rendered screen.
    async fn observe_screen(
        &self,
        session_id: SessionId,
    ) -> DriverResult<TerminalScreenObservation>;

    async fn interrupt(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> DriverResult<InterruptRecovery>;

    /// Returns whether the backend positively observed its owned process
    /// boundary empty without an observed descendant escape. A control-plane
    /// kill acknowledgement is not sufficient.
    async fn close(&self, session_id: SessionId, policy: ClosePolicy) -> DriverResult<bool>;
}

/// Opaque transcript tail cursor. The source owns its platform-specific meaning.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TranscriptPosition {
    pub generation: u64,
    pub offset: u64,
}

/// Result of opening and reconciling the transcript at the pre-injection EOF.
#[derive(Clone, Debug, Default)]
pub struct TranscriptArm {
    pub position: TranscriptPosition,
    /// Rows before the arm point. They are indexed before the engine is armed so
    /// historical terminal messages can never acknowledge or finish this turn.
    pub historical_rows: Vec<ParsedRow>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TranscriptDrainEvidence {
    pub at_eof: bool,
    pub has_partial_line: bool,
    pub stable_for_ms: u64,
}

impl TranscriptDrainEvidence {
    #[must_use]
    pub const fn satisfies(self, required_stable_ms: u64) -> bool {
        self.at_eof && !self.has_partial_line && self.stable_for_ms >= required_stable_ms
    }
}

/// The stability floor a turn still owes after Claude's in-band `turn_duration`
/// marker has been observed on the active chain.
///
/// This number is stated here, in the drain, on purpose. Today a ~250ms window
/// exists only as a side effect of the screen-stability wait running inside a
/// much longer drain (`driver_io.rs` `quiet_for`), so the drain has never had to
/// own it. Once the graduated drain stops dominating, that coincidence protects
/// nothing, and a later tuning of `quiet_for` -- a screen-liveness constant with
/// no reason to know about transcript arrival order -- could silently delete the
/// protection. So: this is NOT a copy of `quiet_for` and must not be re-derived
/// from it.
///
/// What it encodes is the observed-empty-band floor. Across 87 main-chain
/// markers and Claude 2.1.177/207/215/220, the only rows ever seen after the
/// marker were harness-injected `<task-notification>` user rows at gaps of 25ms,
/// 25ms, 284s, 3014s and 18079s: the band (25ms, 284s) is empirically empty, and
/// those +25ms notifications are followed by autonomous generation. Committing
/// at the marker itself would turn pmux's deterministic refusal into a race with
/// that generation; keeping a floor an order of magnitude above the only
/// observed near gap keeps the refusal deterministic.
pub const TURN_DURATION_DRAIN_FLOOR_MS: u64 = 250;

/// How long the transcript must have been unchanged before a turn may commit.
///
/// The marker only ever *lowers* the requirement. `min` rather than a bare
/// substitution: an operator who configured a drain below the floor has already
/// chosen a shorter window for every turn, and a marker -- evidence that the
/// turn is over -- must never be the reason pmux waits longer than it was told
/// to. The asymmetry is preserved in the direction that matters, because the
/// unmarked case is untouched and still owes the full configured drain.
#[must_use]
pub const fn graduated_drain_ms(configured_drain_ms: u64, turn_duration_seen: bool) -> u64 {
    if turn_duration_seen && TURN_DURATION_DRAIN_FLOOR_MS < configured_drain_ms {
        TURN_DURATION_DRAIN_FLOOR_MS
    } else {
        configured_drain_ms
    }
}

/// The most a transcript row may lag the last byte of a turn and still be read
/// before that turn commits -- and, until this line existed, the one quantity in
/// the completion gate that nothing in this tree had a name for.
///
/// It is NOT the drain. The drain is what the transcript must *prove*
/// ([`TranscriptDrainEvidence::satisfies`]); this is how long pmux keeps looking
/// after that proof first comes back satisfied. A row inside the window is
/// ingested and published; a row outside it is committed past, which for a
/// main-chain row is a truncated answer.
///
/// **NOTHING WAITS FOR IT.** It is the arithmetic product of the drain
/// requirement and the commit loop's sampling period, and that period is set by
/// two constants in `crate::driver_io` -- the screen-quiet window and the
/// terminal poll interval -- neither of which knows it is deciding transcript
/// truncation risk. [`TURN_DURATION_DRAIN_FLOOR_MS`] says in its own doc that a
/// later tuning of `quiet_for` could silently delete the 250ms floor. The same
/// tuning silently narrows THIS, one level up, and nobody had written that down
/// anywhere. `driver_io.rs` now refuses to compile a screen configuration whose
/// product falls below this floor.
///
/// MEASURED, and stated as two numbers because they answer two questions:
///
/// - **438ms** is the largest post-answer transcript arrival in the promotion
///   campaign: 456 turns across 189 real 2.1.220 transcripts, median 42, p90
///   120, p95 240, p99 344 (`docs/current-state.md` 6.2.1). It is the floor
///   because it is the largest thing ever observed, not because it is round.
/// - **352ms** is the only such arrival ever observed live *through pmux*, on
///   ordinal 70 of that same campaign. It is pinned as an absolute by
///   `crates/service/tests/v1_actor.rs`'s
///   `the_live_352ms_post_marker_arrival_is_still_caught`, and it is 86ms inside
///   this floor.
///
/// The shipped configuration's window is 550ms, and it is OBSERVED: across NINE
/// independent n=30 runs the published `drain_ms` median ranged 550.0-573.5,
/// median-of-medians 555.5, and **every one of the nine was at or above the
/// derived 550** (`docs/current-state.md` 6.1.2). So this floor does not bind
/// today, with 112ms of headroom. It binds the day somebody shortens a screen
/// constant for latency.
pub const POST_MARKER_CATCH_WINDOW_FLOOR_MS: u64 = 438;

/// The catchable window a commit loop with this sampling period actually offers.
///
/// The gate is first satisfied by the poll that *reports* the requirement, so
/// the requirement is only ever observed on a sampling boundary -- which is the
/// `div_ceil`, and not a rounding convenience. The commit then lands on the
/// confirming re-poll one period later, and every read up to and including that
/// one can still carry bytes, so the furthest a row can lag the last byte and
/// still be read is exactly one period past the first boundary at or beyond the
/// requirement.
///
/// A `period_ms` of zero describes a loop that never samples. It is answered
/// rather than divided by, because the caller has asked about a loop that cannot
/// exist and a division panic inside the `const` assertions `driver_io.rs`
/// evaluates against this would be a compile error naming the wrong thing.
///
/// This is the same arithmetic `crates/service/tests/v1_actor.rs` spells as
/// `catchable_window_ms` for its graduated-band suite, and
/// `the_bands_catchable_window_is_the_products_own_derivation` there asserts the
/// two agree at every cadence that suite samples -- which is what makes those
/// six tests known to be pinning the shipped guarantee rather than a local
/// convenience.
///
/// Validated against measurement at two configurations (`docs/current-state.md`
/// 6.1.2): a nominal period of 275ms (250ms quiet + 25ms poll) predicts 550ms
/// against `drain_ms` medians of 550.0-573.5 over nine n=30 runs; a nominal
/// period of 150ms (125ms quiet) predicts 450ms against a measured 468.0. The
/// excess in both is the per-read overhead this nominal model deliberately
/// leaves out.
///
/// So it is a FLOOR on the real window and not an estimate of it -- and that is
/// the property the refusal needs rather than accuracy. All ten observed medians
/// are at or above what it derives, and for a period at or above the requirement
/// the window is exactly twice the period, so under-stating the period
/// under-states the window. A refusal built on it can therefore be wrong only in
/// the direction of refusing a configuration that would in fact have been safe.
#[must_use]
pub const fn post_marker_catch_window_ms(required_stable_ms: u64, period_ms: u64) -> u64 {
    let period_ms = if period_ms == 0 { 1 } else { period_ms };
    required_stable_ms
        .div_ceil(period_ms)
        .saturating_add(1)
        .saturating_mul(period_ms)
}

#[derive(Clone, Debug, Default)]
pub struct TranscriptBatch {
    pub position: TranscriptPosition,
    pub rows: Vec<ParsedRow>,
    pub drain: TranscriptDrainEvidence,
}

/// Source of reconciled, fully parsed transcript rows.
///
/// Implementations are responsible for file discovery, complete-line framing,
/// and mapping malformed JSON/UTF-8 to `SchemaDrift`. The actor owns semantic
/// correlation and never accepts a terminal-only success fallback.
#[async_trait]
pub trait TranscriptSource: Send + Sync + 'static {
    async fn arm_at_eof(&self, session_id: SessionId) -> DriverResult<TranscriptArm>;

    async fn poll(
        &self,
        session_id: SessionId,
        position: &TranscriptPosition,
    ) -> DriverResult<TranscriptBatch>;

    /// Proves this source's transcript has served no work, for a session about
    /// to be admitted as a `SessionCell::Minified` cell.
    ///
    /// The admission rule this implements is not "is this a new session": it is
    /// "has this transcript already served work". `SessionIdentity::Resume`
    /// names a transcript that already holds a prior caller's context, and
    /// admitting it as a minified cell would run turn 1 against everything that
    /// session ever said while the caller believed it was empty. A caller-chosen
    /// `New` id that collides with an existing transcript reaches the same
    /// place, which is why the question is asked of the file and not of the
    /// request.
    ///
    /// The default REFUSES. A source that cannot prove the claim has not made
    /// it, and the one thing this predicate must never do is pass by omission —
    /// which is exactly what a `Ok(())` default would do for every source
    /// written after it, including every one written by an embedder.
    async fn assert_empty_at_launch(&self, _session_id: SessionId) -> DriverResult<()> {
        Err(DriverFailure::new(
            ErrorCode::UnsupportedFeature,
            "this transcript source cannot prove a launch transcript has served no work, so it may not back a minified cell",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_marker_only_ever_lowers_the_required_drain() {
        // The never-longer property was previously defended only incidentally --
        // a fixture test would time out if the floor rounded a shorter configured
        // drain UP. That is a liveness kill, not a statement, and it would move
        // if the fixture moved. An operator who configures a drain below the
        // floor asked for something faster than the floor, and observing the
        // marker must never make them wait longer than they asked.
        assert_eq!(
            graduated_drain_ms(2_000, true),
            TURN_DURATION_DRAIN_FLOOR_MS
        );
        assert_eq!(graduated_drain_ms(2_000, false), 2_000);
        // At and below the floor the configured value is returned unchanged.
        assert_eq!(
            graduated_drain_ms(TURN_DURATION_DRAIN_FLOOR_MS, true),
            TURN_DURATION_DRAIN_FLOOR_MS
        );
        assert_eq!(graduated_drain_ms(10, true), 10);
        assert_eq!(graduated_drain_ms(10, false), 10);
        assert_eq!(graduated_drain_ms(1, true), 1);
    }

    #[test]
    fn the_catch_window_is_one_period_past_the_first_boundary_at_or_beyond_the_requirement() {
        // THE SHIPPED POINT, and the reason this function is stated as
        // arithmetic rather than as a literal: it PREDICTS the measurement. A
        // 250ms requirement sampled at 275ms predicts 550ms; the observed
        // `drain_ms` medians at the shipped constants are 550.0-573.5 over nine
        // n=30 runs, every one of them at or above that 550.
        assert_eq!(post_marker_catch_window_ms(250, 275), 550);
        // Halving the screen constant. The model says 450 and the run said
        // 468.0; the excess is per-read overhead the nominal period leaves out,
        // and what this arm pins is the direction and the size, not the 18.
        assert_eq!(post_marker_catch_window_ms(250, 150), 450);
        // The `+ 1` is the confirming re-poll and not an off-by-one: a
        // requirement that lands exactly on a boundary still pays it.
        assert_eq!(post_marker_catch_window_ms(250, 250), 500);
        assert_eq!(post_marker_catch_window_ms(250, 125), 375);
        // Below one period the window is twice the period WHATEVER the
        // requirement is, which is why `driver_io.rs` asserts the minified
        // cell's floor separately from the graduated one and gets the same
        // answer -- and why a shorter drain buys a caller nothing here.
        assert_eq!(post_marker_catch_window_ms(50, 275), 550);
        assert_eq!(post_marker_catch_window_ms(1, 275), 550);
        assert_eq!(post_marker_catch_window_ms(0, 275), 275);
        // A loop that samples faster than the requirement pays more boundaries
        // and a smaller window. This is the R2 reorder's shape, and it is the
        // arm that makes the floor in `POST_MARKER_CATCH_WINDOW_FLOOR_MS`
        // capable of refusing something.
        assert_eq!(post_marker_catch_window_ms(250, 1), 251);
        assert!(post_marker_catch_window_ms(250, 1) < POST_MARKER_CATCH_WINDOW_FLOOR_MS);
        // Degenerate period: answered, not divided by.
        assert_eq!(post_marker_catch_window_ms(250, 0), 251);
        // And the point of the whole function, at the shipped period: the window
        // it derives covers the one post-marker arrival ever seen live, with
        // 198ms to spare. Written through the function rather than as a
        // comparison of two constants so that it is a check and not a tautology.
        assert!(
            post_marker_catch_window_ms(TURN_DURATION_DRAIN_FLOOR_MS, 275)
                > ORDINAL_70_POST_MARKER_MS
        );
    }

    /// The one post-marker arrival ever observed live through pmux, on ordinal
    /// 70 of the promotion campaign. Restated here rather than imported because
    /// the timeline test that reproduces it lives in another crate's test
    /// binary; the two are compared by
    /// `the_bands_catchable_window_is_the_products_own_derivation`.
    const ORDINAL_70_POST_MARKER_MS: u64 = 352;

    /// The floor is the campaign's own maximum, not a rounding of it: 438ms over
    /// 456 turns in 189 real 2.1.220 transcripts. Both arms are `const` and not
    /// `#[test]` on purpose -- every term is a compile-time constant, so a
    /// runtime assertion over them is a check that clippy is right to say gets
    /// optimised out. Lowering the floor past the live sample stops the crate
    /// compiling, which is a stronger place to find out than a red test.
    const _: () = assert!(
        ORDINAL_70_POST_MARKER_MS < POST_MARKER_CATCH_WINDOW_FLOOR_MS,
        "the floor must cover the one post-marker arrival ever observed live through pmux"
    );
    /// And the floor is not vacuous: it is above the drain requirement it
    /// guards, so a window that merely met the drain would not clear it.
    const _: () = assert!(
        TURN_DURATION_DRAIN_FLOOR_MS < POST_MARKER_CATCH_WINDOW_FLOOR_MS,
        "a catch-window floor at or below the drain requirement guards nothing"
    );

    #[test]
    fn system_clock_uses_an_out_of_domain_sentinel_instead_of_clamping() {
        assert_eq!(
            protocol_clock_milliseconds(Some(u128::from(MAX_SAFE_JSON_INTEGER))),
            MAX_SAFE_JSON_INTEGER
        );
        assert_eq!(
            protocol_clock_milliseconds(Some(u128::from(MAX_SAFE_JSON_INTEGER) + 1)),
            INVALID_CLOCK_TIMESTAMP
        );
        assert_eq!(protocol_clock_milliseconds(None), INVALID_CLOCK_TIMESTAMP);
    }
}
