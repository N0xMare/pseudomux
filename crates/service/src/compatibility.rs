//! Evidence-backed Claude compatibility admission.

use anyhow::{Result, bail, ensure};
use pseudomux_protocol::v1::{
    CompatibilityPolicy, CompatibilityReport, ErrorBody, ErrorCode, InputTransport, TerminalProfile,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Conservative drain used only for an explicit `allow_untested` request that
/// does not match an admitted profile.
pub const DEFAULT_UNTESTED_TRANSCRIPT_DRAIN_MS: u64 = 2_000;
pub const MAX_TRANSCRIPT_DRAIN_MS: u64 = 60_000;

/// One Claude Code version, parsed into the three components pmux's
/// compatibility policy is actually stated in.
///
/// A struct rather than a string because the policy is ORDERED -- "at or above
/// a measured floor, at or below a tested ceiling, and never across a minor" --
/// and an ordered policy expressed over strings is a lexicographic comparison
/// waiting to decide that `2.1.99` outranks `2.1.207`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClaudeVersion {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl ClaudeVersion {
    /// Parses an exact normalized `major.minor.patch`.
    ///
    /// This is the whole of what `validate_exact_version` used to do, except
    /// that it KEEPS the value. A validator that throws its parse away is how a
    /// version comes to be compared as a string three lines later.
    ///
    /// # Errors
    ///
    /// Any value that is not three non-empty ASCII-digit components, without
    /// surrounding whitespace, each fitting a `u64`.
    pub fn parse(value: &str) -> Result<Self> {
        ensure!(
            !value.is_empty() && value.trim() == value,
            "tested Claude profile version must be a non-empty normalized value"
        );
        let mut parts = value.split('.');
        let parsed = match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some(major), Some(minor), Some(patch), None) => {
                Self::component(major).zip(Self::component(minor).zip(Self::component(patch)))
            }
            _ => None,
        };
        let Some((major, (minor, patch))) = parsed else {
            bail!("tested Claude profile version must be exact normalized major.minor.patch");
        };
        Ok(Self {
            major,
            minor,
            patch,
        })
    }

    fn component(value: &str) -> Option<u64> {
        (!value.is_empty() && value.chars().all(|character| character.is_ascii_digit()))
            .then(|| value.parse().ok())
            .flatten()
    }

    /// Whether two versions are the same `major.minor` line, i.e. differ at
    /// most by a patch.
    #[must_use]
    pub const fn same_line(self, other: Self) -> bool {
        self.major == other.major && self.minor == other.minor
    }
}

impl std::fmt::Display for ClaudeVersion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// The versions one compatibility cell admits: a MEASURED FLOOR and a
/// TESTED-THROUGH CEILING, inclusive at both ends.
///
/// # Why a range and not a string
///
/// The exact-string key made every Claude Code patch release refuse Path B
/// until 13 ledger ordinals had been spent re-promoting it, against a budget of
/// 15 remaining and a ceiling of 100 for all time. `docs/version-drift.md`
/// sec.3.3 then measured what that pin was buying and found it measuring noise:
/// the only version-keyed quantity in the profile is `transcript_drain_ms`, the
/// 2.1.215-to-2.1.220 spread in its statistic is 100 ms against a
/// *within*-version p95 of 176-216 ms, and a permutation test on the maxima
/// gives p = 0.730. Meanwhile the things a Claude Code update really does break
/// -- the launch bundle, the composer geometry, the post-`/clear` preamble --
/// were keyed to no version at all.
///
/// So the key widens and the drain becomes a pooled bound (see
/// [`PROMOTED_PROFILES`]), and what replaces the pin is
/// [`RepromotionTrigger`]: five named conditions, each bound to a detector that
/// exists, any one of which retracts the range.
///
/// # What it deliberately does NOT do
///
/// **It does not open backward.** sec.3.1 measured 2.1.201 and earlier at
/// ZERO reachable `cli` arrivals -- not a small sample, no sample -- so
/// everything below the floor is unestablished, and unestablished refuses.
///
/// **It does not span a minor.** [`Self::new`] refuses a floor and a ceiling on
/// different `major.minor` lines, and because the bounds share a line the
/// ordered containment in [`Self::admits`] refuses a different line for free.
/// That is re-promotion trigger 5 -- a conservative policy default, not a
/// measurement, and the one the owner confirmed: patch drift tolerated, minor
/// forces re-promotion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VersionRange {
    pub floor: ClaudeVersion,
    pub tested_through: ClaudeVersion,
}

impl VersionRange {
    /// # Errors
    ///
    /// An unparseable bound, a range spanning a major or minor version, or a
    /// ceiling below its floor.
    pub fn new(floor: &str, tested_through: &str) -> Result<Self> {
        let floor = ClaudeVersion::parse(floor)?;
        let tested_through = ClaudeVersion::parse(tested_through)?;
        ensure!(
            floor.same_line(tested_through),
            "a tested Claude profile may not span a major or minor version ({floor} through \
             {tested_through}): {}",
            RepromotionTrigger::MajorOrMinorVersionChange.detector().how
        );
        ensure!(
            floor <= tested_through,
            "a tested Claude profile's ceiling {tested_through} is below its floor {floor}"
        );
        Ok(Self {
            floor,
            tested_through,
        })
    }

    /// Whether this cell admits `value`.
    ///
    /// An unparseable version is refused rather than admitted: the one thing a
    /// compatibility gate must never do is admit what it could not read.
    #[must_use]
    pub fn admits(&self, value: &str) -> bool {
        ClaudeVersion::parse(value)
            .is_ok_and(|version| version >= self.floor && version <= self.tested_through)
    }

    /// Whether two ranges could both admit the same version.
    ///
    /// This is what "duplicate" means once the key is a range. Equality is not:
    /// two cells with different but overlapping ranges are exactly as ambiguous
    /// as two identical ones, and it is the ambiguity `insert` refuses.
    #[must_use]
    pub const fn overlaps(&self, other: &Self) -> bool {
        // `<=` on ClaudeVersion is not const, so compare the tuples the derived
        // Ord compares.
        let self_floor = (self.floor.major, self.floor.minor, self.floor.patch);
        let self_top = (
            self.tested_through.major,
            self.tested_through.minor,
            self.tested_through.patch,
        );
        let other_floor = (other.floor.major, other.floor.minor, other.floor.patch);
        let other_top = (
            other.tested_through.major,
            other.tested_through.minor,
            other.tested_through.patch,
        );
        le(self_floor, other_top) && le(other_floor, self_top)
    }
}

const fn le(left: (u64, u64, u64), right: (u64, u64, u64)) -> bool {
    if left.0 != right.0 {
        return left.0 < right.0;
    }
    if left.1 != right.1 {
        return left.1 < right.1;
    }
    left.2 <= right.2
}

impl std::fmt::Display for VersionRange {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}..={}", self.floor, self.tested_through)
    }
}

/// The five conditions that retract a promoted range, each bound to the code
/// that detects it.
///
/// `docs/version-drift.md` sec.5 P2 names these five. A list of five sentences
/// in a document is not a policy -- it is the house bug class, a claim whose
/// predicate nobody wrote -- so each variant here carries the FILE and the
/// SYMBOL that detects it, and
/// `tests::every_repromotion_trigger_names_a_detector_that_exists` opens each
/// file and fails when the symbol is not in it. Renaming a detector, or
/// deleting one, turns that test red rather than leaving a trigger that is only
/// a noun.
///
/// Triggers 1 and 2 are detected in Python, by
/// `tools/promotion/measure_transcript_drain.py`, and cost 0 ledger ordinals:
/// they read transcripts that already exist. Triggers 3, 4 and 5 are detected
/// in this daemon, at runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RepromotionTrigger {
    /// A post-answer row kind nobody has classified, or a kind classified
    /// `retrospective` whose premise no longer holds.
    UnclassifiedTranscriptRowKind,
    /// A reachable post-answer arrival above the drain pmux ships.
    ReachableArrivalAboveTheBound,
    /// The child refused one of the flags in the minified launch bundle.
    LaunchBundleRejected,
    /// The transcript `/clear` opens is not the preamble pmux measured, or the
    /// composer it opens is not the screen pmux measured.
    ClearScreenOrPreambleMismatch,
    /// A major or minor version change rather than a patch.
    MajorOrMinorVersionChange,
}

/// Where one [`RepromotionTrigger`] is detected, in terms a test can check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TriggerDetector {
    /// The id, spelled identically in every language that reports it.
    pub id: &'static str,
    /// Repository-relative path of the file that detects it.
    pub file: &'static str,
    /// A token that must appear in that file. The binding, and the thing the
    /// test checks.
    pub symbol: &'static str,
    /// What the operator does about it.
    pub how: &'static str,
}

impl RepromotionTrigger {
    /// Every trigger. Kept honest by [`Self::detector`], whose `match` carries
    /// no wildcard, and by
    /// `tests::every_repromotion_trigger_is_in_ALL_exactly_once`.
    pub const ALL: [Self; 5] = [
        Self::UnclassifiedTranscriptRowKind,
        Self::ReachableArrivalAboveTheBound,
        Self::LaunchBundleRejected,
        Self::ClearScreenOrPreambleMismatch,
        Self::MajorOrMinorVersionChange,
    ];

    /// The file and symbol that detect this trigger.
    ///
    /// WILDCARD-FREE on purpose: a sixth trigger stops this function
    /// compiling, which is the only reliable way to make someone say where the
    /// new one is detected.
    #[must_use]
    pub const fn detector(self) -> TriggerDetector {
        match self {
            Self::UnclassifiedTranscriptRowKind => TriggerDetector {
                id: "unclassified_transcript_row_kind",
                file: "tools/promotion/measure_transcript_drain.py",
                symbol: "TRIGGER_UNCLASSIFIED_ROW_KIND",
                how: "re-run measure_transcript_drain.py; exit 2 names a post-answer row kind \
                      absent from ROW_KINDS and exit 3 names a `retrospective` kind stamped after \
                      the terminal candidate. Classify it with a reason, then re-measure.",
            },
            Self::ReachableArrivalAboveTheBound => TriggerDetector {
                id: "reachable_arrival_above_the_bound",
                file: "tools/promotion/measure_transcript_drain.py",
                symbol: "TRIGGER_ARRIVAL_ABOVE_THE_BOUND",
                how: "re-run measure_transcript_drain.py --bound-ms with the drain this profile \
                      ships; exit 4 means an arrival exceeded it and exit 5 means there was \
                      nothing to check, which is not the same as passing.",
            },
            Self::LaunchBundleRejected => TriggerDetector {
                id: "launch_bundle_rejected",
                file: "crates/service/src/native.rs",
                symbol: "LAUNCH_BUNDLE_REJECTED_MARKER",
                how: "the child named an option it does not know and exited before running. The \
                      startup refusal's `screen_shape.child_rejected_a_launch_flag` is true. Read \
                      claude_launch.rs and sensitive_launch.rs against `claude --help`.",
            },
            Self::ClearScreenOrPreambleMismatch => TriggerDetector {
                id: "clear_screen_or_preamble_mismatch",
                file: "crates/service/src/driver_io.rs",
                symbol: "is_a_version_drift_signal",
                // The only `how` in this table that pmux also SHIPS as a
                // caller-facing `recommendation` (`pool::refusal::pool_halted`),
                // and until 2026-08-10 it was the only one that described the
                // state without naming a next step -- its four siblings all end
                // in an imperative. A halted pool refuses every checkout until
                // something outside it changes, so a reader who is told only
                // what happened has been told nothing they can act on.
                how: "the transcript `/clear` opened is not the preamble measured at \
                      driver_io.rs's MAX_ASSERT_EMPTY_ROWS/MAX_ASSERT_EMPTY_USER_ROWS block. The \
                      stateless pool HALTS on it rather than quarantining one instance, because \
                      every instance is typing into the same composer. Re-promote against the \
                      installed Claude Code version and restart pmuxd; no retry clears a halt.",
            },
            Self::MajorOrMinorVersionChange => TriggerDetector {
                id: "major_or_minor_version_change",
                file: "crates/service/src/compatibility.rs",
                symbol: "same_line",
                how: "a conservative policy default, not a measurement: patch drift inside a \
                      tested range is tolerated, a minor is not. Re-promote against the new line.",
            },
        }
    }

    /// The id, for a wire field or a log.
    #[must_use]
    pub const fn id(self) -> &'static str {
        self.detector().id
    }
}

/// One compatibility cell pmux itself promotes, and the evidence that promoted
/// it.
///
/// The provenance is a field and not a comment because this is the difference
/// between "works on this machine" and "works for anyone": a promoted profile
/// is an assertion pmux makes on every operator's behalf, and an operator who
/// wants to know what backs it must be able to read it out of the running
/// daemon rather than out of a commit message. [`crate::native`] publishes it
/// in the configuration layer of the health tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PromotedProfile {
    /// The lowest Claude Code version this cell admits. MEASURED: below it
    /// there is no evidence at all (`docs/version-drift.md` sec.3.1).
    pub claude_version_floor: &'static str,
    /// The highest Claude Code version this cell has been tested THROUGH.
    /// Above it, pmux refuses -- the range opens forward from a floor, it is
    /// not unbounded.
    pub claude_version_tested_through: &'static str,
    pub os: &'static str,
    pub arch: &'static str,
    pub terminal_profile: TerminalProfile,
    pub input_transport: InputTransport,
    /// MEASURED. See [`PROMOTED_PROFILES`] for the measurement, the corpus and
    /// what invalidates it.
    pub transcript_drain_ms: u64,
    /// How `transcript_drain_ms` was arrived at, in one line an operator can
    /// act on.
    pub drain_provenance: &'static str,
    /// What was tested at the CEILING, in one line, so an operator can tell a
    /// range that was measured from a range that was assumed.
    pub range_provenance: &'static str,
}

impl PromotedProfile {
    /// # Panics
    ///
    /// Never in practice: `every_promoted_profile_passes_the_admission_an_operator_profile_must`
    /// runs the whole set through `insert`, which validates the range, so an
    /// unparseable promoted range cannot reach a release.
    #[must_use]
    pub fn version_range(&self) -> VersionRange {
        VersionRange::new(
            self.claude_version_floor,
            self.claude_version_tested_through,
        )
        .expect("a promoted profile's range is validated by insert")
    }

    fn to_profile(self) -> TestedCompatibilityProfile {
        TestedCompatibilityProfile {
            claude_version: self.claude_version_floor.to_owned(),
            claude_version_tested_through: Some(self.claude_version_tested_through.to_owned()),
            os: self.os.to_owned(),
            arch: self.arch.to_owned(),
            terminal_profile: self.terminal_profile,
            // NORMALIZED here, exactly as `insert` normalizes an operator's.
            // `resolve` compares against a transport that has already been
            // resolved, so a promoted cell declaring `Auto` would match nothing
            // -- silently, and only on the promoted path, because an operator's
            // copy of the same profile goes through `insert` and is normalized
            // on the way in. That asymmetry is the whole reason this is a
            // method and not a struct literal at the call site.
            input_transport: resolved_input_transport(self.input_transport),
            transcript_drain_ms: self.transcript_drain_ms,
        }
    }
}

/// The cells pmux ships as promoted, so Path B is reachable on a supported host
/// with no `--tested-claude-profile` on argv at all.
///
/// # Why this list exists
///
/// `require_tested_for_minified_cell` refuses a minified cell that no promoted
/// profile admits, and the promoted set used to be EMPTY. Every session that
/// ever drove Path B passed `--tested-claude-profile` on `pmuxd` argv, which
/// means Path B worked for the people who knew the flag and refused for
/// everyone else. A promoted list is what turns a private capability into a
/// product; an operator flag remains, and an operator profile for the same
/// identity WINS, because an operator who measured their own host outranks a
/// number pmux measured on someone else's.
///
/// # How `transcript_drain_ms` was measured
///
/// The drain answers exactly one question: *how long after pmux has a terminal
/// candidate can the transcript still move?* It is measured, not chosen, from
/// the arrival timestamps Claude Code itself writes into its JSONL.
///
/// **It is a POOLED CONSERVATIVE BOUND, not a per-version fit**, and that is
/// the load-bearing sentence. `docs/version-drift.md` sec.3.3 measured the
/// per-version pin and found it measuring noise: the 2.1.215-to-2.1.220 spread
/// in maxima is 100 ms against a *within*-version p95 of 176-216 ms for the
/// same statistic, and a permutation test on the difference in maxima gives
/// **p = 0.730**. Worse, a small sample under-estimates a tail maximum, so a
/// per-version fit errs in exactly the direction that TRUNCATES an answer:
/// fitting 2.1.223 from its own corpus today recommends **250 ms**, which is
/// below the 438 ms already observed one version earlier and below
/// `POST_MARKER_CATCH_WINDOW_FLOOR_MS`.
///
/// - **Corpus.** 425 macos/aarch64 transcripts spanning Claude Code 2.1.207,
///   2.1.215, 2.1.220 and 2.1.223 -- the Path B `/clear` probe, the drain and
///   gate-B calibration runs, the phase-1/2 runs and ordinary interactive
///   agent sessions. Both shapes are in it -- tool-less minified-shaped turns
///   and full tool-using ones.
/// - **Statistic.** For each turn, the gap between every pair of consecutive
///   rows at or after that turn's final `assistant` row, POOLED over every
///   version measured. 226 reachable rows arrived: median 45 ms, p90 122 ms,
///   p95 240 ms, p99 338 ms, **max 438 ms**, at 2.1.220.
/// - **Composition.** Every reachable arrival was a structural end-of-turn row
///   -- `system/turn_duration` or `system/stop_hook_summary`. **No semantic row
///   ever arrived after the answer.**
/// - **Excluded, and named.** Four arrivals sat far outside that band: 4.3 s
///   (`queue-operation`), 204 s (`system/away_summary`), 1562 s (a
///   `<task-notification>` user row) and 18075 s (`queue-operation`). All four
///   are harness-injected rows in an interactive agent session. They are the
///   same FAMILY -- not the same values -- as the far gaps
///   `v1::backend`'s `TURN_DURATION_DRAIN_FLOOR_MS` doc records at 284 s /
///   3014 s / 18079 s, and a minified cell has no harness that can inject one:
///   no tasks, no queue, no away summary. A drain sized to cover them would be
///   an hour long and would still be a race. They are not dropped: the tool
///   publishes every bucket with its own maximum and fails on a row kind
///   nobody has classified.
/// - **Value.** 438 ms x 2.0, rounded up to the tool's 250 ms step, is
///   **1000 ms** -- the number already shipped. Widening the key from one
///   version to a range therefore changed no number, only what the number
///   means. It halves the 2000 ms `DEFAULT_UNTESTED_TRANSCRIPT_DRAIN_MS`,
///   which is the value an unmeasured host falls back to and which nothing
///   ever measured.
/// - **PRICED, because headroom is not free.** The full drain binds only on a
///   turn with no `turn_duration` marker of its own -- **166 of 385 `cli`
///   turns, 43%**; a marked turn already owes only `TURN_DURATION_DRAIN_FLOOR_MS`.
///   Going to 1250 ms would spend 250 ms on 43% of turns to move the expected
///   first truncation from one in 337,000 unmarked turns to one in 4.5 M
///   (sec.3.5). That is not recommended, and the price is recorded so the
///   question is never re-opened without it.
/// - **Reproduce it.** `tools/promotion/measure_transcript_drain.py`, which
///   emits `evidence/pooled-transcript-drain-macos-aarch64.json` (the bound)
///   and `evidence/promoted-profile-2.1.220-macos-aarch64.json` (the floor's
///   own receipt).
///
/// # What would invalidate it
///
/// The five conditions that retract the whole cell are enumerated, each bound
/// to a detector that exists, at [`RepromotionTrigger`]. In terms of this value
/// specifically:
///
/// 1. A `claude_version` outside [`VersionRange`], or a different `os` or
///    `arch`: the identity is the key, so a different one simply does not match
///    and is refused.
/// 2. A semantic row -- `assistant`, or a `user` row that is not `isMeta` --
///    arriving after a turn's final assistant row. None was seen in 1,336
///    turns. One is enough to retract this value.
/// 3. Any post-answer structural arrival above 1000 ms. **0 of 226** exceeded
///    it, and `measure_transcript_drain.py --bound-ms` re-checks that for
///    0 ledger ordinals against whatever the corpus now holds.
/// 4. A minified cell that acquires a harness able to inject rows -- a hook, a
///    task queue, an MCP server. The exclusion above is an argument from the
///    launch bundle, and the launch bundle is what makes it true.
///
/// The graduated drain reduces this number further whenever the turn's own
/// `turn_duration` marker is observed (`graduated_drain_ms`, floor 250 ms) and
/// the minified fast path further still (50 ms). This value is what an
/// UNMARKED turn owes -- the turn whose marker had not landed yet, which is
/// exactly the 438 ms case above.
pub const PROMOTED_PROFILES: &[PromotedProfile] = &[
    PromotedProfile {
        claude_version_floor: "2.1.220",
        claude_version_tested_through: "2.1.227",
        os: "macos",
        arch: "aarch64",
        terminal_profile: TerminalProfile::Transparent,
        input_transport: InputTransport::Sdk,
        transcript_drain_ms: 1_000,
        drain_provenance: "POOLED conservative bound, not a per-version fit: max reachable \
                       post-answer transcript arrival 438 ms over 226 arrivals in 425 \
                       macos/aarch64 transcripts spanning Claude Code 2.1.207/2.1.215/2.1.220/\
                       2.1.223, x2.0 and rounded up to a 250 ms step = 1000 ms. Priced: the full \
                       drain binds only on the 166 of 385 cli turns carrying no turn_duration \
                       marker. evidence/pooled-transcript-drain-macos-aarch64.json, \
                       tools/promotion/measure_transcript_drain.py",
        range_provenance: "floor 2.1.220: the version with a drain receipt, a Gate B campaign and the \
                       screen/preamble measurements; below it 2.1.201 and earlier have ZERO \
                       reachable cli arrivals, which is unestablished rather than safe. Tested \
                       through 2.1.227: promote_claude_version.py drove 5 minified-cell turns \
                       through `pmux ask` at claude-sonnet-5 low/high -- every graded reply exact, \
                       the four-grade suite served by one unchanging process across a `/clear` per \
                       turn, sidechain and cache zero on every result, the pool never halted -- and \
                       measured 5 reachable post-answer arrival(s) at this version, max 52 ms \
                       against the pooled 1000 ms bound. NOT measured at 2.1.227: anything outside \
                       a minified cell on macos/aarch64, and the per-version fit of 250 ms, which \
                       is published to be read and NOT shipped.",
    },
    PromotedProfile {
        claude_version_floor: "2.1.227",
        claude_version_tested_through: "2.1.236",
        os: "linux",
        arch: "x86_64",
        terminal_profile: TerminalProfile::Transparent,
        input_transport: InputTransport::Sdk,
        transcript_drain_ms: 250,
        drain_provenance: "POOLED conservative bound, not a per-version fit: max reachable \
                       post-answer transcript arrival 118 ms over 191 arrivals in 209 \
                       linux/x86_64 transcripts spanning Claude Code 2.1.227/2.1.232/2.1.233, \
                       x2.0 and rounded up to a 250 ms step = 250 ms. Every named version's own \
                       fit is also 250 ms because 118×2.0=236 sits inside the 250 ms rounding \
                       quantum, not because the corpus is one version. Priced: the full drain \
                       binds on 0 of 191 cli turns (every Path B turn carried a turn_duration \
                       marker). evidence/pooled-transcript-drain-linux-x86_64.json, \
                       tools/promotion/measure_transcript_drain.py",
        range_provenance: "floor 2.1.227: first linux/x86_64 Path B drain receipt \
                       (evidence/promoted-profile-2.1.227-linux-x86_64.json, max reachable 46 ms) \
                       pooled with 2.1.232/2.1.233 in evidence/pooled-transcript-drain-linux-x86_64.json; \
                       below it linux minified cells were not measured as a promotion floor. Tested \
                       through 2.1.236: promote_claude_version.py drove 5 minified-cell turns \
                       through `pmux run` at claude-sonnet-5 low/high -- every graded reply exact, \
                       the four-grade suite answered across a `/clear` per turn, sidechain and \
                       cache zero on every result, the pool never halted -- and measured 5 \
                       reachable post-answer arrival(s) at this version, max 46 ms against the \
                       pooled 250 ms bound. NOT measured at 2.1.236: anything outside a minified \
                       cell on linux/x86_64, and the per-version fit of 250 ms, which is published \
                       to be read and NOT shipped.",
    },
];

/// One empirically promoted compatibility cell.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestedCompatibilityProfile {
    /// The lowest version this cell admits. For an operator's own cell this is
    /// the version they measured, and it is the whole range unless they say
    /// otherwise.
    pub claude_version: String,
    /// The highest version this cell was tested THROUGH, inclusive.
    ///
    /// OPTIONAL, and absent means `claude_version` -- an exact match, which is
    /// what every operator profile written before this field existed meant and
    /// still means. An operator who measured one host states one version; an
    /// operator who tested a range states both, and pmux says so in the
    /// refusal it stops issuing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_version_tested_through: Option<String>,
    pub os: String,
    pub arch: String,
    pub terminal_profile: TerminalProfile,
    pub input_transport: InputTransport,
    pub transcript_drain_ms: u64,
}

impl TestedCompatibilityProfile {
    /// # Errors
    ///
    /// An unparseable bound, a range across a minor, or an inverted range.
    /// `validate` is what makes this infallible everywhere else.
    pub fn version_range(&self) -> Result<VersionRange> {
        VersionRange::new(
            &self.claude_version,
            self.claude_version_tested_through
                .as_deref()
                .unwrap_or(&self.claude_version),
        )
    }

    fn matches(
        &self,
        claude_version: &str,
        os: &str,
        arch: &str,
        terminal_profile: TerminalProfile,
        input_transport: InputTransport,
    ) -> bool {
        // A cell whose own range does not parse admits NOTHING. `insert`
        // rejects one, and `PROMOTED_PROFILES` is held to the same rule by
        // `every_promoted_profile_passes_the_admission_an_operator_profile_must`,
        // so this arm is unreachable -- but the safe direction for an
        // unreachable arm in an admission gate is refusal.
        self.version_range()
            .is_ok_and(|range| range.admits(claude_version))
            && self.os == os
            && self.arch == arch
            && self.terminal_profile == terminal_profile
            && self.input_transport == input_transport
    }

    /// Whether two cells could both admit one session.
    ///
    /// Once the key is a range, "duplicate" means OVERLAPPING and not equal:
    /// 2.1.220..=2.1.226 and 2.1.223..=2.1.230 are exactly as ambiguous as two
    /// identical cells, and ambiguity is what `insert` refuses.
    fn overlapping_key(&self, other: &Self) -> bool {
        self.os == other.os
            && self.arch == other.arch
            && self.terminal_profile == other.terminal_profile
            && self.input_transport == other.input_transport
            && match (self.version_range(), other.version_range()) {
                (Ok(mine), Ok(theirs)) => mine.overlaps(&theirs),
                _ => false,
            }
    }

    fn validate(&self) -> Result<()> {
        self.version_range()?;
        validate_platform_component(&self.os, "os")?;
        validate_platform_component(&self.arch, "arch")?;
        ensure!(
            self.terminal_profile == TerminalProfile::Transparent,
            "rmux-standard terminal identity cannot be admitted as a tested Claude compatibility profile"
        );
        ensure!(
            self.input_transport != InputTransport::AttachedStream,
            "attached-stream input cannot be admitted as a tested Claude compatibility profile"
        );
        validate_transcript_drain_ms(self.transcript_drain_ms)?;
        Ok(())
    }
}

/// Validated, ambiguity-free set of promoted compatibility cells.
#[derive(Clone, Debug, Default)]
pub struct CompatibilityProfileRegistry {
    profiles: Vec<TestedCompatibilityProfile>,
}

impl CompatibilityProfileRegistry {
    pub fn try_from_profiles(
        profiles: impl IntoIterator<Item = TestedCompatibilityProfile>,
    ) -> Result<Self> {
        let mut registry = Self::default();
        for profile in profiles {
            registry.insert(profile)?;
        }
        Ok(registry)
    }

    pub fn insert(&mut self, mut profile: TestedCompatibilityProfile) -> Result<()> {
        profile.input_transport = resolved_input_transport(profile.input_transport);
        profile.validate()?;
        ensure!(
            !self
                .profiles
                .iter()
                .any(|existing| existing.overlapping_key(&profile)),
            "overlapping tested Claude compatibility profile for versions {}, OS {}, architecture {}, terminal profile {:?}, and input transport {:?}",
            profile.version_range().map_or_else(
                |_| profile.claude_version.clone(),
                |range| range.to_string()
            ),
            profile.os,
            profile.arch,
            profile.terminal_profile,
            profile.input_transport,
        );
        self.profiles.push(profile);
        Ok(())
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.profiles.is_empty()
    }

    /// How many cells the OPERATOR admitted. Does not count
    /// [`PROMOTED_PROFILES`]; [`Self::admissible_here`] is the count that does.
    #[must_use]
    pub fn len(&self) -> usize {
        self.profiles.len()
    }

    /// Every cell that could admit a session ON THIS HOST -- operator-admitted
    /// first, then pmux's own promoted set, in exactly the order
    /// [`Self::resolve`] searches them.
    ///
    /// DERIVED from the same iterator `resolve` uses, rather than a second
    /// hand-written traversal, because the whole point of the count is to
    /// answer "would a mint find a cell?" and a count that walks a different
    /// set answers a different question.
    ///
    /// The version is deliberately NOT filtered on: nothing here knows which
    /// Claude a caller will name. So this counts cells whose platform and
    /// terminal identity match, and every surface that reports it has to say
    /// that is what it counted.
    fn candidates(&self) -> impl Iterator<Item = TestedCompatibilityProfile> + '_ {
        self.profiles.iter().cloned().chain(
            PROMOTED_PROFILES
                .iter()
                .copied()
                .map(PromotedProfile::to_profile),
        )
    }

    /// How many cells match this platform and terminal identity, from both the
    /// operator's set and pmux's promoted one. See `Self::candidates` -- private,
    /// so deliberately not an intra-doc link from public documentation -- for
    /// what this deliberately does not check.
    #[must_use]
    pub fn admissible_here(&self) -> usize {
        let os = std::env::consts::OS;
        let arch = std::env::consts::ARCH;
        // Platform only. There is deliberately NO terminal/input clause here:
        // every candidate is already a supported v1 cell, because `insert`
        // runs `validate` on the operator's and
        // `every_promoted_profile_passes_the_admission_an_operator_profile_must`
        // runs the same `insert` over `PROMOTED_PROFILES`. A clause was written
        // here first and MEASURED as unreachable -- deleting it broke no test,
        // which is a guard that cannot fail wearing the costume of one. What
        // replaces it is a check that CAN fail:
        // `every_candidate_is_a_supported_v1_cell_with_a_resolved_transport`.
        self.candidates()
            .filter(|profile| profile.os == os && profile.arch == arch)
            .count()
    }

    /// How many of [`PROMOTED_PROFILES`] name this platform. Reported beside
    /// the operator count so an operator can tell a daemon that works because
    /// pmux promoted a cell from one that works because they admitted one.
    #[must_use]
    pub fn promoted_here() -> usize {
        Self::default().admissible_here()
    }

    /// Resolves exactly the current process platform and requested terminal
    /// cell. A profile for another OS, architecture, terminal identity, or
    /// input transport never admits this session.
    ///
    /// Operator profiles are searched BEFORE [`PROMOTED_PROFILES`], so an
    /// operator who measured their own host overrides pmux's promoted number
    /// for the same identity rather than colliding with it.
    pub fn resolve(
        &self,
        policy: CompatibilityPolicy,
        claude_version: &str,
        terminal_profile: TerminalProfile,
        input_transport: InputTransport,
        untested_transcript_drain_ms: u64,
    ) -> Result<CompatibilityReport, ErrorBody> {
        validate_v1_terminal_support(terminal_profile, input_transport)?;
        if let Err(error) = validate_transcript_drain_ms(untested_transcript_drain_ms) {
            return Err(ErrorBody::new(ErrorCode::InvalidConfig, error.to_string()));
        }
        let input_transport = resolved_input_transport(input_transport);
        let os = std::env::consts::OS;
        let arch = std::env::consts::ARCH;
        if let Some(profile) = self.candidates().find(|profile| {
            profile.matches(claude_version, os, arch, terminal_profile, input_transport)
        }) {
            return Ok(CompatibilityReport {
                claude_version: claude_version.to_owned(),
                os: os.to_owned(),
                arch: arch.to_owned(),
                terminal_profile,
                input_transport,
                tested: true,
                transcript_drain_ms: profile.transcript_drain_ms,
            });
        }

        if policy == CompatibilityPolicy::RequireTested {
            return Err(ErrorBody::new(
                ErrorCode::UnsupportedClaudeVersion,
                format!(
                    "Claude Code {claude_version} has no tested pmux compatibility profile for {os}/{arch}, {terminal_profile:?}, {input_transport:?}"
                ),
            )
            .with_details(json!({
                "claude_version": claude_version,
                "os": os,
                "arch": arch,
                "terminal_profile": terminal_profile,
                "input_transport": input_transport,
                "recommendation": "run and review the guarded pmux Phase 0 cell, then admit its structured compatibility profile with --tested-claude-profile",
                // Every version RANGE that WOULD have matched on this platform,
                // so a refused caller can see how far off they are without
                // reading the daemon's argv. Named for what it contains --
                // operator cells and promoted ones together -- because the
                // caller cannot act on the difference and a name that implied
                // otherwise would be one more thing that is not quite true.
                "compatibility_cells_for_this_platform": self.candidates()
                    .filter(|profile| profile.os == os && profile.arch == arch)
                    .map(|profile| profile
                        .version_range()
                        .map_or(profile.claude_version, |range| range.to_string()))
                    .collect::<Vec<_>>(),
                // WHICH trigger the caller is looking at, when the answer is
                // knowable. A version on another `major.minor` line than every
                // cell here is trigger 5 by construction, and the operator's
                // next step for it is different from the next step for a patch
                // past a tested ceiling: one needs a re-promotion against a new
                // line, the other needs the ceiling advanced.
                "repromotion_trigger": repromotion_trigger_for(
                    claude_version,
                    self.candidates().filter(|profile| profile.os == os && profile.arch == arch),
                ).map(RepromotionTrigger::id),
            })));
        }

        Ok(CompatibilityReport {
            claude_version: claude_version.to_owned(),
            os: os.to_owned(),
            arch: arch.to_owned(),
            terminal_profile,
            input_transport,
            tested: false,
            transcript_drain_ms: untested_transcript_drain_ms,
        })
    }
}

/// Which [`RepromotionTrigger`] a refused version is an instance of, when that
/// is knowable from the version alone.
///
/// Returns [`RepromotionTrigger::MajorOrMinorVersionChange`] only when EVERY
/// cell on this platform is on a different `major.minor` line, because a
/// version can be a minor away from one cell and a patch away from another and
/// only the first case is the trigger. `None` when there is no cell to compare
/// against, or when the version simply sits outside a range on its own line --
/// which is not one of the five triggers, it is the ceiling doing its job, and
/// naming a trigger there would be the house bug class in a diagnostic.
fn repromotion_trigger_for(
    claude_version: &str,
    cells: impl Iterator<Item = TestedCompatibilityProfile>,
) -> Option<RepromotionTrigger> {
    let version = ClaudeVersion::parse(claude_version).ok()?;
    let mut any = false;
    for cell in cells {
        any = true;
        if cell
            .version_range()
            .is_ok_and(|range| range.floor.same_line(version))
        {
            return None;
        }
    }
    any.then_some(RepromotionTrigger::MajorOrMinorVersionChange)
}

/// Rejects v1 terminal/input cells that are present in the public type system
/// only as reserved future behavior. This validation runs before any launch or
/// compatibility lookup so `allow_untested` can never turn a reserved cell
/// into an attempted child process.
pub fn validate_v1_terminal_support(
    terminal_profile: TerminalProfile,
    input_transport: InputTransport,
) -> Result<(), ErrorBody> {
    if terminal_profile == TerminalProfile::RmuxStandard {
        return Err(ErrorBody::new(
            ErrorCode::UnsupportedFeature,
            "rmux-standard terminal identity is reserved and is not implemented in protocol v1",
        ));
    }
    if input_transport == InputTransport::AttachedStream {
        return Err(ErrorBody::new(
            ErrorCode::UnsupportedFeature,
            "attached-stream prompt injection is reserved and is not implemented in protocol v1",
        ));
    }
    Ok(())
}

pub fn validate_transcript_drain_ms(value: u64) -> Result<()> {
    ensure!(
        (1..=MAX_TRANSCRIPT_DRAIN_MS).contains(&value),
        "transcript drain must be between 1 and {MAX_TRANSCRIPT_DRAIN_MS} milliseconds"
    );
    Ok(())
}

/// Resolves the request-level preference to the transport actually used by the
/// native backend. Compatibility evidence is keyed to this value, never to the
/// `auto` alias.
#[must_use]
pub const fn resolved_input_transport(value: InputTransport) -> InputTransport {
    match value {
        InputTransport::Auto => InputTransport::Sdk,
        other => other,
    }
}

fn validate_platform_component(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value.trim() != value
        || !value.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '_' | '-')
        })
    {
        bail!("tested Claude profile {label} must be a normalized platform token");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn current_profile(
        input_transport: InputTransport,
        drain_ms: u64,
    ) -> TestedCompatibilityProfile {
        TestedCompatibilityProfile {
            claude_version: "2.1.207".to_owned(),
            claude_version_tested_through: None,
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            terminal_profile: TerminalProfile::Transparent,
            input_transport,
            transcript_drain_ms: drain_ms,
        }
    }

    #[test]
    fn require_tested_matches_the_complete_cell_and_uses_its_drain() {
        let registry = CompatibilityProfileRegistry::try_from_profiles([current_profile(
            InputTransport::Sdk,
            875,
        )])
        .unwrap();
        let report = registry
            .resolve(
                CompatibilityPolicy::RequireTested,
                "2.1.207",
                TerminalProfile::Transparent,
                InputTransport::Sdk,
                DEFAULT_UNTESTED_TRANSCRIPT_DRAIN_MS,
            )
            .unwrap();
        assert!(report.tested);
        assert_eq!(report.transcript_drain_ms, 875);
        assert_eq!(report.input_transport, InputTransport::Sdk);

        let auto_report = registry
            .resolve(
                CompatibilityPolicy::RequireTested,
                "2.1.207",
                TerminalProfile::Transparent,
                InputTransport::Auto,
                DEFAULT_UNTESTED_TRANSCRIPT_DRAIN_MS,
            )
            .unwrap();
        assert!(auto_report.tested);
        assert_eq!(auto_report.input_transport, InputTransport::Sdk);

        let error = registry
            .resolve(
                CompatibilityPolicy::RequireTested,
                "2.1.207",
                TerminalProfile::RmuxStandard,
                InputTransport::Sdk,
                DEFAULT_UNTESTED_TRANSCRIPT_DRAIN_MS,
            )
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::UnsupportedFeature);
    }

    #[test]
    fn allow_untested_is_explicit_and_uses_the_daemon_fallback() {
        let report = CompatibilityProfileRegistry::default()
            .resolve(
                CompatibilityPolicy::AllowUntested,
                "2.1.207",
                TerminalProfile::Transparent,
                InputTransport::Sdk,
                3_000,
            )
            .unwrap();
        assert!(!report.tested);
        assert_eq!(report.transcript_drain_ms, 3_000);
        assert_eq!(report.os, std::env::consts::OS);
        assert_eq!(report.arch, std::env::consts::ARCH);
    }

    #[test]
    fn invalid_and_duplicate_profiles_are_rejected() {
        let mut registry = CompatibilityProfileRegistry::default();
        registry
            .insert(current_profile(InputTransport::Sdk, 500))
            .unwrap();
        assert!(
            registry
                .insert(current_profile(InputTransport::Sdk, 750))
                .is_err()
        );
        let mut invalid = current_profile(InputTransport::Auto, 0);
        assert!(registry.insert(invalid.clone()).is_err());
        invalid.transcript_drain_ms = 500;
        invalid.claude_version = "2.1".to_owned();
        assert!(registry.insert(invalid).is_err());
        let too_large = current_profile(InputTransport::Auto, MAX_TRANSCRIPT_DRAIN_MS + 1);
        assert!(registry.insert(too_large).is_err());
        let attached = current_profile(InputTransport::AttachedStream, 500);
        assert!(registry.insert(attached).is_err());
        let mut rmux_standard = current_profile(InputTransport::Sdk, 500);
        rmux_standard.terminal_profile = TerminalProfile::RmuxStandard;
        assert!(registry.insert(rmux_standard).is_err());

        let error = CompatibilityProfileRegistry::default()
            .resolve(
                CompatibilityPolicy::AllowUntested,
                "2.1.207",
                TerminalProfile::Transparent,
                InputTransport::AttachedStream,
                DEFAULT_UNTESTED_TRANSCRIPT_DRAIN_MS,
            )
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::UnsupportedFeature);
    }

    /// A version is ORDERED, and the ordering is on numbers.
    ///
    /// `2.1.99` above `2.1.207` is the one bug a range key is guaranteed to
    /// have if the components are ever compared as text, and it fails silently
    /// -- as an admission, which is the direction that must never be wrong.
    #[test]
    fn versions_are_ordered_numerically_and_unparseable_ones_are_refused() {
        assert!(
            ClaudeVersion::parse("2.1.99").unwrap() < ClaudeVersion::parse("2.1.207").unwrap(),
            "versions are compared as numbers, not as text"
        );
        let range = VersionRange::new("2.1.99", "2.1.207").unwrap();
        assert!(range.admits("2.1.100"));
        assert!(range.admits("2.1.99"));
        assert!(range.admits("2.1.207"));
        assert!(!range.admits("2.1.98"));
        assert!(!range.admits("2.1.208"));

        for refused in [
            "",
            " 2.1.220",
            "2.1.220 ",
            "2.1",
            "2.1.220.1",
            "2.1.x",
            "2.1.",
        ] {
            assert!(
                ClaudeVersion::parse(refused).is_err(),
                "{refused:?} is not an exact normalized major.minor.patch"
            );
            assert!(
                !range.admits(refused),
                "{refused:?} does not parse, so nothing may admit it"
            );
        }
    }

    /// Trigger 5, as a predicate: a tested range may not span a minor, and a
    /// version on another line is refused.
    ///
    /// The refusal is DERIVED from the ordering rather than written as a second
    /// clause: because [`VersionRange::new`] refuses a floor and a ceiling on
    /// different lines, `floor <= v <= tested_through` cannot admit another
    /// line, and there is no separate `same_line` check in `admits` to forget
    /// to update.
    #[test]
    fn a_tested_range_may_never_span_a_major_or_minor_version() {
        for (floor, ceiling) in [
            ("2.1.220", "2.2.0"),
            ("2.1.220", "3.1.220"),
            ("2.1.220", "2.1.219"),
        ] {
            let error = VersionRange::new(floor, ceiling)
                .expect_err("{floor}..={ceiling} must not be admissible as a tested range");
            let _ = error;
        }
        let error = VersionRange::new("2.1.220", "2.2.0")
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(RepromotionTrigger::MajorOrMinorVersionChange.detector().how),
            "the refusal must say what to do about it: {error}"
        );

        let range = VersionRange::new("2.1.220", "2.1.226").unwrap();
        for elsewhere in ["2.0.226", "2.2.220", "1.1.220", "3.1.220"] {
            assert!(
                !range.admits(elsewhere),
                "{elsewhere} is not on {range}'s line and must be refused"
            );
        }
    }

    /// Two cells that could both admit one version are refused as ambiguous,
    /// even when neither is a copy of the other.
    #[test]
    fn overlapping_ranges_are_refused_and_adjacent_ones_are_not() {
        let cell = |floor: &str, ceiling: &str| TestedCompatibilityProfile {
            claude_version: floor.to_owned(),
            claude_version_tested_through: Some(ceiling.to_owned()),
            ..current_profile(InputTransport::Sdk, 500)
        };

        let mut registry = CompatibilityProfileRegistry::default();
        registry.insert(cell("2.1.220", "2.1.226")).unwrap();
        for overlapping in [
            ("2.1.226", "2.1.230"),
            ("2.1.210", "2.1.220"),
            ("2.1.221", "2.1.222"),
            ("2.1.210", "2.1.240"),
        ] {
            let error = registry
                .clone()
                .insert(cell(overlapping.0, overlapping.1))
                .expect_err("an overlapping range is as ambiguous as a duplicate");
            assert!(
                error.to_string().contains("overlapping"),
                "the refusal must say why: {error:#}"
            );
        }
        // Adjacent, not overlapping. Two cells that partition the line are a
        // legitimate thing to hold -- a floor measured once and a ceiling
        // advanced later, each with its own drain.
        registry
            .insert(cell("2.1.227", "2.1.230"))
            .expect("adjacent ranges are unambiguous");
        assert_eq!(registry.len(), 2);
    }

    /// Every [`RepromotionTrigger`] is in `ALL`, exactly once.
    ///
    /// `ALL` is a hand-written array and this is what keeps it from being one:
    /// `index` below carries no wildcard, so a sixth variant stops the file
    /// compiling, and the permutation check catches a variant added to the
    /// match but left out of `ALL`.
    #[test]
    fn every_repromotion_trigger_is_in_all_exactly_once() {
        const fn index(trigger: RepromotionTrigger) -> usize {
            match trigger {
                RepromotionTrigger::UnclassifiedTranscriptRowKind => 0,
                RepromotionTrigger::ReachableArrivalAboveTheBound => 1,
                RepromotionTrigger::LaunchBundleRejected => 2,
                RepromotionTrigger::ClearScreenOrPreambleMismatch => 3,
                RepromotionTrigger::MajorOrMinorVersionChange => 4,
            }
        }
        let mut seen: Vec<usize> = RepromotionTrigger::ALL.into_iter().map(index).collect();
        seen.sort_unstable();
        assert_eq!(
            seen,
            (0..RepromotionTrigger::ALL.len()).collect::<Vec<_>>(),
            "RepromotionTrigger::ALL is not every variant, exactly once"
        );

        let ids: std::collections::BTreeSet<_> =
            RepromotionTrigger::ALL.iter().map(|t| t.id()).collect();
        assert_eq!(
            ids.len(),
            RepromotionTrigger::ALL.len(),
            "two triggers share an id, so a report naming one names both"
        );
    }

    /// **The check that makes a trigger a trigger rather than a noun.**
    ///
    /// A five-item list in a document is the house bug class -- a claim whose
    /// predicate nobody wrote. Each trigger names the FILE and the SYMBOL that
    /// detects it, and this opens the file and looks for the symbol. Deleting a
    /// detector, or renaming one, turns this red; so does inventing a sixth
    /// trigger with nowhere to point.
    ///
    /// Two of the five point into Python, which is the point: triggers 1 and 2
    /// are detected by `tools/promotion/measure_transcript_drain.py` for 0
    /// ledger ordinals, and a Rust-only binding would have silently stopped
    /// covering them.
    #[test]
    fn every_repromotion_trigger_names_a_detector_that_exists() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("the workspace root is the repository");
        for trigger in RepromotionTrigger::ALL {
            let detector = trigger.detector();
            let path = root.join(detector.file);
            let source = std::fs::read_to_string(&path).unwrap_or_else(|error| {
                panic!(
                    "{trigger:?} says it is detected in {} and that file does not exist: {error}",
                    detector.file
                )
            });
            assert!(
                source.contains(detector.symbol),
                "{trigger:?} says {} detects it, and `{}` is not in that file",
                detector.file,
                detector.symbol
            );
            // What this tests is that the field is not blank, and that is all
            // it used to SAY it tested. Whether the sentence names an action is
            // a different question and this predicate cannot answer it:
            // `ClearScreenOrPreambleMismatch`'s `how` was three clauses of
            // description and passed here for as long as the assertion existed.
            // The one `how` pmux ships as a caller-facing `recommendation` is
            // held to the stronger bar by
            // `pool::refusal::tests::every_pool_refusal_says_what_to_do_next`.
            assert!(
                !detector.how.trim().is_empty(),
                "{trigger:?} publishes an empty `what_to_do` in the health tree"
            );
        }
    }

    /// A promoted range's tested-through half is the sentence its own promotion
    /// receipt GENERATED, not a sentence someone wrote beside it.
    ///
    /// This string is the house bug class with a history. It once described a
    /// launch bundle pmux does not emit; then, after
    /// `docs/2.1.226-acceptance.md` measured the drain at 2.1.226, it went on
    /// saying *"NOT measured at 2.1.226: the drain"* -- understating its own
    /// evidence, which is the same defect as overstating it. Both are only
    /// possible while the sentence and the measurement are separate artifacts.
    /// `promote_claude_version.py` assembles `range_provenance` from its check
    /// results, so this test is what makes the shipped copy the assembled one.
    ///
    /// It also refuses a receipt that is not a promotion: `verdict` must be
    /// `promotable`, which a rehearsal against the test double never is.
    #[test]
    fn every_promoted_range_is_the_sentence_its_promotion_receipt_generated() {
        for promoted in PROMOTED_PROFILES {
            let through = promoted.claude_version_tested_through;
            let path = evidence_dir().join(format!(
                "promotion-{through}-{}-{}.json",
                promoted.os, promoted.arch
            ));
            let receipt = receipt_at(&path);
            assert_eq!(
                receipt["verdict"],
                "promotable",
                "{} is not a promotion receipt",
                path.display()
            );
            assert_eq!(
                receipt["driver"]["environment"],
                "operator",
                "{} was taken against something other than a real Claude",
                path.display()
            );
            let profile = &receipt["profile"];
            assert_eq!(profile["claude_version"], promoted.claude_version_floor);
            assert_eq!(profile["claude_version_tested_through"], through);
            assert_eq!(profile["os"], promoted.os);
            assert_eq!(profile["arch"], promoted.arch);
            assert_eq!(
                profile["transcript_drain_ms"].as_u64(),
                Some(promoted.transcript_drain_ms),
                "the shipped drain is not the one {} promoted",
                path.display()
            );
            assert_eq!(
                receipt["range_provenance"].as_str(),
                Some(promoted.range_provenance),
                "the shipped range_provenance is not the one {} generated",
                path.display()
            );
            for check in receipt["checks"]
                .as_array()
                .expect("a promotion receipt lists its checks")
            {
                assert_eq!(
                    check["outcome"],
                    "passed",
                    "{} promoted with check {} not passed",
                    path.display(),
                    check["id"]
                );
            }
        }
    }

    /// Every trigger is also EXERCISED by the runnable promotion path, not only
    /// detectable once something has already gone wrong.
    ///
    /// A detector answers "did this fire in production". A promotion has to ask
    /// the same five questions on purpose, before widening a range, and until
    /// `tools/promotion/promote_claude_version.py` existed there was nothing to
    /// ask them with: 2.1.220 and 2.1.226 were both promoted by an agent
    /// improvising a session. The tool refuses to run unless its own ordered
    /// check list covers exactly the ids read out of [`RepromotionTrigger`]
    /// above, and this test is the other half of that binding -- a trigger
    /// renamed here without a corresponding check turns Rust red rather than
    /// waiting for someone to run the tool.
    #[test]
    fn every_repromotion_trigger_is_exercised_by_the_promotion_path() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tools/promotion/promote_claude_version.py");
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("no promotion path at {}: {error}", path.display()));
        for trigger in RepromotionTrigger::ALL {
            let id = trigger.detector().id;
            assert!(
                source.contains(&format!("\"{id}\"")),
                "{trigger:?} is detectable but no check in {} claims to exercise it, so a \
                 promotion would never have looked for it",
                path.display()
            );
        }
    }

    /// Every promoted cell has to survive the SAME admission an operator's
    /// would, and the set has to be unambiguous.
    ///
    /// Derived rather than restated: the check builds a registry out of
    /// [`PROMOTED_PROFILES`] through [`CompatibilityProfileRegistry::insert`],
    /// which is the one function that knows what a valid, non-duplicate cell is.
    /// A promoted profile with a zero drain, an unnormalized version, a
    /// reserved terminal identity or a twin would ship silently otherwise --
    /// `PROMOTED_PROFILES` is a `const` and nothing on the daemon's path ever
    /// calls `validate` on it.
    #[test]
    fn every_promoted_profile_passes_the_admission_an_operator_profile_must() {
        let mut registry = CompatibilityProfileRegistry::default();
        for promoted in PROMOTED_PROFILES {
            registry
                .insert(promoted.to_profile())
                .unwrap_or_else(|error| {
                    panic!(
                    "promoted profile {promoted:?} is not admissible as a tested cell: {error:#}"
                )
                });
            assert!(
                !promoted.drain_provenance.trim().is_empty(),
                "promoted profile {promoted:?} states no provenance for its measured drain"
            );
        }
        assert_eq!(
            registry.len(),
            PROMOTED_PROFILES.len(),
            "two promoted profiles share an identity"
        );
    }

    /// Every promoted drain equals the number its own receipt recommends.
    ///
    /// The constant and the measurement have to be the same number or the
    /// provenance string is decoration. This reads the committed receipt --
    /// `evidence/promoted-profile-<version>-<os>-<arch>.json`, emitted by
    /// `tools/promotion/measure_transcript_drain.py` -- and compares its
    /// `recommended_transcript_drain_ms` against the value shipped here. A
    /// re-measurement that moves the recommendation fails this test until
    /// someone decides which number is right, which is the point.
    ///
    /// The receipt's own corpus is host-local and is NOT in the repository, so
    /// what is checked is the recommendation and the identity, not a
    /// re-derivation from transcripts a clone does not have.
    #[test]
    fn every_promoted_drain_is_the_one_its_receipt_recommends() {
        let evidence = evidence_dir();
        for promoted in PROMOTED_PROFILES {
            let path = evidence.join(format!(
                "promoted-profile-{}-{}-{}.json",
                promoted.claude_version_floor, promoted.os, promoted.arch
            ));
            let raw = std::fs::read_to_string(&path).unwrap_or_else(|error| {
                panic!(
                    "promoted profile {promoted:?} ships no receipt at {}: {error}",
                    path.display()
                )
            });
            let receipt: serde_json::Value = serde_json::from_str(&raw).expect("a receipt is JSON");
            assert_eq!(
                receipt["recommended_transcript_drain_ms"].as_u64(),
                Some(promoted.transcript_drain_ms),
                "the promoted drain and its receipt disagree, at {}",
                path.display()
            );
            // And the receipt is about the cell it is filed under.
            for (field, expected) in [
                ("claude_version", promoted.claude_version_floor),
                ("os", promoted.os),
                ("arch", promoted.arch),
            ] {
                assert_eq!(
                    receipt[field].as_str(),
                    Some(expected),
                    "the receipt at {} is filed under the wrong identity",
                    path.display()
                );
            }
            // A receipt whose reachable-arrival maximum already exceeds the
            // value it recommends is a receipt that retracted itself.
            let observed_max =
                receipt["post_answer_arrivals"]["reachable_on_a_minified_cell"]["max_ms"]
                    .as_u64()
                    .expect("the receipt reports a reachable maximum");
            assert!(
                observed_max < promoted.transcript_drain_ms,
                "the receipt at {} observed a {observed_max} ms arrival against a promoted drain \
                 of {} ms",
                path.display(),
                promoted.transcript_drain_ms
            );
        }
    }

    fn evidence_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../evidence")
            .canonicalize()
            .expect("the evidence directory is part of the repository")
    }

    fn receipt_at(path: &std::path::Path) -> serde_json::Value {
        let raw = std::fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("no receipt at {}: {error}", path.display()));
        serde_json::from_str(&raw).expect("a receipt is JSON")
    }

    fn reachable_max_ms(body: &serde_json::Value) -> Option<u64> {
        body["post_answer_arrivals"]["reachable_on_a_minified_cell"]["max_ms"].as_u64()
    }

    /// The promoted drain is the POOLED bound over every version measured, and
    /// it is NOT any version's own fit.
    ///
    /// The estimator is re-derived here rather than restated: the margin and
    /// the rounding step are read out of the receipt's own
    /// `recommendation_basis`, so nothing in this crate carries a second copy
    /// of `RECOMMENDATION_MARGIN` or `RECOMMENDATION_STEP_MS` that could
    /// silently disagree with `tools/promotion/measure_transcript_drain.py`.
    ///
    /// Two of the assertions exist purely to keep the word "pooled" honest,
    /// because a pooled bound over one version is a per-version fit wearing a
    /// different noun:
    ///
    /// * the receipt must name at least two versions, each with arrivals of
    ///   its own -- naming a version that contributed nothing is the same
    ///   vacuity the tool's own exit 5 exists to refuse; and
    /// * at least one per-version fit must be STRICTLY BELOW the pooled bound,
    ///   which is what makes this test able to fail on macos: 2.1.207 and
    ///   2.1.223 each fit 250 ms and 2.1.215 fits 750 ms, against the pooled
    ///   1000. The exception is estimator saturation at the rounding quantum:
    ///   when `2 × pooled_max` is already ≤ the 250 ms step, every version
    ///   rounds to the same number the pool does. linux/x86_64 is that case
    ///   (max 118 ms × 2.0 = 236 ms → 250 ms over 2.1.227/2.1.232/2.1.233).
    ///   That is still a pooled bound — three versions, each with arrivals —
    ///   not a one-version fit. If every fit equals the pooled bound AND the
    ///   estimator is not saturated, this assertion goes red rather than
    ///   passing while proving nothing.
    #[test]
    fn every_promoted_drain_is_the_pooled_bound_and_not_a_per_version_fit() {
        for promoted in PROMOTED_PROFILES {
            let path = evidence_dir().join(format!(
                "pooled-transcript-drain-{}-{}.json",
                promoted.os, promoted.arch
            ));
            let receipt = receipt_at(&path);
            let basis = &receipt["recommendation_basis"];
            let margin = basis["margin"]
                .as_f64()
                .expect("the receipt states the margin it applied");
            let step = basis["rounded_up_to_ms"]
                .as_u64()
                .expect("the receipt states the step it rounded up to");
            let pooled_max =
                reachable_max_ms(&receipt).expect("the receipt reports a pooled reachable maximum");

            let derived = (((pooled_max as f64 * margin) / step as f64).ceil() as u64) * step;
            assert_eq!(
                Some(derived),
                receipt["recommended_transcript_drain_ms"].as_u64(),
                "the receipt at {} recommends a number its own stated estimator does not produce",
                path.display()
            );
            assert_eq!(
                derived,
                promoted.transcript_drain_ms,
                "the promoted drain is not the pooled bound the receipt at {} derives",
                path.display()
            );

            let versions = receipt["claude_versions"]
                .as_array()
                .expect("the receipt names the versions it pooled");
            assert!(
                versions.len() >= 2,
                "a bound pooled over {} version(s) is a per-version fit, at {}",
                versions.len(),
                path.display()
            );
            for version in versions {
                let version = version.as_str().expect("a version is a string");
                let body = &receipt["by_version"][version];
                let max = reachable_max_ms(body).unwrap_or_else(|| {
                    panic!(
                        "{version} is named as pooled at {} but contributed no reachable arrival, \
                         so it was never checked",
                        path.display()
                    )
                });
                assert!(
                    max <= pooled_max,
                    "{version} reports a {max} ms arrival the pooled maximum of {pooled_max} ms \
                     does not cover, at {}",
                    path.display()
                );
                // The bound is a bound. This is re-promotion trigger 2, checked
                // against the committed evidence rather than only at the tool's
                // `--bound-ms`.
                assert!(
                    max < promoted.transcript_drain_ms,
                    "{version} observed a {max} ms arrival against a promoted drain of {} ms",
                    promoted.transcript_drain_ms
                );
            }

            let fits = receipt["per_version_recommendations_not_to_be_shipped"]
                .as_object()
                .expect("the receipt publishes what each version would have been fitted to");
            let any_fit_below = fits
                .values()
                .filter_map(serde_json::Value::as_u64)
                .any(|fit| fit < derived);
            if !any_fit_below {
                // Saturated at the rounding quantum: 2 × max already fits in
                // one step, so every named version and the pool recommend the
                // same number. Still pooled (versions.len() >= 2 above). Not
                // a licence to ship a one-version 250 as "pooled".
                assert!(
                    derived == step && (pooled_max as f64 * margin) <= step as f64,
                    "every per-version fit at {} already equals the pooled bound, and the \
                     estimator is not saturated at the rounding quantum, so this test can no \
                     longer tell a pooled bound from a fit",
                    path.display()
                );
            }

            // The provenance QUOTES the receipt. A re-measurement that moves any
            // of these numbers has to move the sentence an operator reads out of
            // the running daemon's health tree, instead of leaving it describing
            // a corpus that no longer exists.
            let price = &receipt["full_drain_binds_on"];
            for (label, quantity) in [
                ("the pooled maximum", pooled_max),
                ("the bound", derived),
                (
                    "the count of cli turns priced",
                    price["cli_turns_with_a_terminal_candidate"]
                        .as_u64()
                        .expect("the receipt prices the bound"),
                ),
                (
                    "the count of cli turns that owe the full drain",
                    price["without_a_turn_duration_marker"]
                        .as_u64()
                        .expect("the receipt prices the bound"),
                ),
            ] {
                assert!(
                    promoted.drain_provenance.contains(&quantity.to_string()),
                    "the drain provenance does not name {label} ({quantity}) that the receipt at \
                     {} measured: {}",
                    path.display(),
                    promoted.drain_provenance
                );
            }
        }
    }

    /// Linux/x86_64 minified ground truth for the 438 quantity is written down
    /// and is not a licence to lower the product-wide catch-window floor.
    ///
    /// `POST_MARKER_CATCH_WINDOW_FLOOR_MS` is the macos campaign max (438ms)
    /// plus the 352ms live sample. The same statistic on linux minified cells
    /// is 46ms (`LINUX_MINIFIED_POST_ANSWER_ARRIVAL_MAX_MS`). This test is the
    /// language-boundary pin: the constant equals the receipt, the receipt is
    /// the unique-file measurement (not a double-counted retain tree), the
    /// post-marker set is empty, and 438 is still the floor. Lowering the
    /// floor to 46 fails the compile-time asserts in `v1/backend.rs`; changing
    /// 46 without re-measuring fails here.
    #[test]
    fn linux_minified_post_answer_max_is_the_written_receipt_and_does_not_lower_the_floor() {
        use crate::v1::{
            LINUX_MINIFIED_POST_ANSWER_ARRIVAL_MAX_MS, POST_MARKER_CATCH_WINDOW_FLOOR_MS,
        };

        let path = evidence_dir().join("linux-minified-post-answer-x86_64.json");
        let receipt = receipt_at(&path);

        assert_eq!(receipt["os"].as_str(), Some("linux"));
        assert_eq!(receipt["arch"].as_str(), Some("x86_64"));
        assert_eq!(
            receipt["claude_versions"],
            serde_json::json!(["2.1.227", "2.1.232"])
        );

        let observed_max = reachable_max_ms(&receipt)
            .expect("the linux minified receipt reports a reachable maximum");
        assert_eq!(
            observed_max,
            LINUX_MINIFIED_POST_ANSWER_ARRIVAL_MAX_MS,
            "the linux constant and its receipt disagree, at {}",
            path.display()
        );
        assert_eq!(
            receipt["linux_minified_post_answer_arrival_max_ms"].as_u64(),
            Some(LINUX_MINIFIED_POST_ANSWER_ARRIVAL_MAX_MS)
        );
        assert_eq!(
            receipt["post_marker_catch_window_floor_ms"].as_u64(),
            Some(POST_MARKER_CATCH_WINDOW_FLOOR_MS)
        );
        assert_eq!(POST_MARKER_CATCH_WINDOW_FLOOR_MS, 438);
        assert_eq!(LINUX_MINIFIED_POST_ANSWER_ARRIVAL_MAX_MS, 46);
        // 46 < 438 is the claim: the linux minified max is inside the floor,
        // not a replacement for it. The two equalities above prove the
        // inequality; a runtime assert of two consts is optimized out.

        assert_eq!(
            receipt["post_marker_arrivals_after_turn_duration"]["count"].as_u64(),
            Some(0),
            "a post-marker row on linux minified retracts the empty-set claim this receipt makes"
        );
        let kinds = receipt["post_answer_arrivals"]["by_kind"]
            .as_object()
            .expect("the receipt publishes by_kind");
        let kind_names: Vec<&str> = kinds.keys().map(String::as_str).collect();
        assert_eq!(
            kind_names,
            ["system/turn_duration"],
            "a new reachable kind on linux minified is a new measurement, not a silent extra bucket"
        );
        assert_eq!(
            receipt["full_drain_binds_on"]["without_a_turn_duration_marker"].as_u64(),
            Some(0)
        );
        assert_eq!(
            receipt["recommended_transcript_drain_ms"].as_u64(),
            Some(250),
            "the estimator over a 46ms max is 250ms; that is a drain recommendation, not a new floor"
        );
        assert_eq!(
            receipt["corpus"]["dedup"]["unique_jsonl"].as_u64(),
            Some(61)
        );
        let arrivals = receipt["post_answer_arrivals"]["reachable_on_a_minified_cell"]["count"]
            .as_u64()
            .expect("the receipt counts unique reachable arrivals");
        assert!(
            arrivals >= 47,
            "the unique-file corpus shrank below the written n={arrivals} at {}",
            path.display()
        );
        // Named so this file cannot be mistaken for the promotion drain.
        // The pooled bound lives at `pooled-transcript-drain-linux-x86_64.json`
        // (max 118 ms → 250 ms). This file remains the fast-path 46 ms pin.
        assert!(
            receipt["role"]
                .as_str()
                .is_some_and(|role| role.contains("NOT a promoted-profile drain receipt")),
            "the linux receipt must say it is not the promotion drain, at {}",
            path.display()
        );
    }

    /// Every cell `resolve` will search is a supported v1 cell whose transport
    /// is already RESOLVED.
    ///
    /// The second half is the one that bites. `resolve` normalizes the
    /// REQUEST's transport before matching, and `insert` normalizes an
    /// operator's profile on the way in -- but `PROMOTED_PROFILES` is a `const`
    /// that never passes through `insert`, so a promoted cell declaring
    /// `InputTransport::Auto` would be searched un-normalized and match
    /// nothing, silently, on the promoted path only. An operator who wrote the
    /// identical profile would be admitted. This asserts over `candidates()` --
    /// the iterator `resolve` actually walks -- rather than over either source.
    #[test]
    fn every_candidate_is_a_supported_v1_cell_with_a_resolved_transport() {
        let mut registry = CompatibilityProfileRegistry::default();
        registry
            .insert(current_profile(InputTransport::Auto, 500))
            .unwrap();
        let mut seen = 0;
        for profile in registry.candidates() {
            seen += 1;
            validate_v1_terminal_support(profile.terminal_profile, profile.input_transport)
                .unwrap_or_else(|error| {
                    panic!("{profile:?} is not a v1 cell resolve can match: {error:?}")
                });
            assert_eq!(
                profile.input_transport,
                resolved_input_transport(profile.input_transport),
                "{profile:?} carries an unresolved transport, so resolve compares a resolved \
                 request against an alias and never matches"
            );
        }
        assert_eq!(
            seen,
            PROMOTED_PROFILES.len() + 1,
            "candidates() must yield every promoted cell and every operator cell"
        );
    }

    /// A promoted cell admits a minified session with NO operator flag, and it
    /// carries the drain that was measured for it rather than the untested
    /// fallback.
    ///
    /// This is the whole point of promotion, so it is asserted on the default
    /// registry -- the one a daemon started without `--tested-claude-profile`
    /// actually holds.
    #[test]
    fn a_promoted_cell_admits_this_platform_with_no_operator_profile() {
        let registry = CompatibilityProfileRegistry::default();
        let here = PROMOTED_PROFILES
            .iter()
            .find(|profile| {
                profile.os == std::env::consts::OS && profile.arch == std::env::consts::ARCH
            })
            .copied();
        let Some(here) = here else {
            // Not a promoted platform. The claim then is the opposite one and
            // it is still checked: nothing may be admitted by accident.
            assert_eq!(
                registry.admissible_here(),
                0,
                "no cell is promoted for {}/{} yet one is counted as admissible here",
                std::env::consts::OS,
                std::env::consts::ARCH
            );
            return;
        };
        // EVERY version in the range, not just the floor. A containment
        // predicate that admits its endpoints and nothing between them would
        // pass a two-endpoint test and refuse most of the range in production.
        let range = here.version_range();
        for patch in range.floor.patch..=range.tested_through.patch {
            let inside = format!("{}.{}.{patch}", range.floor.major, range.floor.minor);
            let report = registry
                .resolve(
                    CompatibilityPolicy::RequireTested,
                    &inside,
                    here.terminal_profile,
                    here.input_transport,
                    DEFAULT_UNTESTED_TRANSCRIPT_DRAIN_MS,
                )
                .unwrap_or_else(|error| {
                    panic!("{inside} is inside {range} and must be admitted: {error:?}")
                });
            assert!(report.tested);
            assert_eq!(report.transcript_drain_ms, here.transcript_drain_ms);
            assert_ne!(
                report.transcript_drain_ms, DEFAULT_UNTESTED_TRANSCRIPT_DRAIN_MS,
                "the promoted drain is the measured one, not the untested fallback"
            );
        }
        assert!(registry.admissible_here() >= 1);

        // A cell for ANOTHER platform is not counted as admissible here. This
        // is what keeps `compatibility_layer` from reporting an admission path
        // on a host where every mint would be refused -- the exact shape of the
        // question this project keeps getting wrong: "is the set non-empty?"
        // rather than "is the set non-empty FOR THE THING THAT WILL RUN?".
        let mut elsewhere = here.to_profile();
        elsewhere.arch = format!("{}-not-this-one", here.arch);
        let with_foreign = CompatibilityProfileRegistry::try_from_profiles([elsewhere]).unwrap();
        assert_eq!(
            with_foreign.admissible_here(),
            CompatibilityProfileRegistry::promoted_here(),
            "a profile for another architecture was counted as admissible on this host"
        );

        // The range has TWO closed ends and a line. Promotion widens the door
        // by a bounded set of patches, and the three refusals below are the
        // three ways out of it -- one patch past the tested ceiling, one patch
        // below the measured floor, and the next minor.
        for (outside, expected_trigger) in [
            (
                format!(
                    "{}.{}.{}",
                    range.tested_through.major,
                    range.tested_through.minor,
                    range.tested_through.patch + 1
                ),
                None,
            ),
            (
                format!(
                    "{}.{}.{}",
                    range.floor.major,
                    range.floor.minor,
                    range.floor.patch - 1
                ),
                None,
            ),
            (
                format!("{}.{}.0", range.floor.major, range.floor.minor + 1),
                Some(RepromotionTrigger::MajorOrMinorVersionChange.id()),
            ),
            (
                format!("{}.0.0", range.floor.major + 1),
                Some(RepromotionTrigger::MajorOrMinorVersionChange.id()),
            ),
        ] {
            let error = registry
                .resolve(
                    CompatibilityPolicy::RequireTested,
                    &outside,
                    here.terminal_profile,
                    here.input_transport,
                    DEFAULT_UNTESTED_TRANSCRIPT_DRAIN_MS,
                )
                .expect_err("a version outside every promoted range must still be refused");
            assert_eq!(error.code, ErrorCode::UnsupportedClaudeVersion);
            assert_eq!(
                error.details["repromotion_trigger"].as_str(),
                expected_trigger,
                "the refusal of {outside} against {range} names the wrong trigger"
            );
        }
    }

    /// An operator who measured their own host beats the promoted number.
    #[test]
    fn an_operator_profile_overrides_the_promoted_one_for_the_same_identity() {
        let Some(here) = PROMOTED_PROFILES
            .iter()
            .find(|profile| {
                profile.os == std::env::consts::OS && profile.arch == std::env::consts::ARCH
            })
            .copied()
        else {
            return;
        };
        let mut override_profile = here.to_profile();
        override_profile.transcript_drain_ms = here.transcript_drain_ms + 7;
        let registry = CompatibilityProfileRegistry::try_from_profiles([override_profile]).unwrap();
        let report = registry
            .resolve(
                CompatibilityPolicy::RequireTested,
                here.claude_version_floor,
                here.terminal_profile,
                here.input_transport,
                DEFAULT_UNTESTED_TRANSCRIPT_DRAIN_MS,
            )
            .unwrap();
        assert_eq!(
            report.transcript_drain_ms,
            here.transcript_drain_ms + 7,
            "the operator's own measurement must win over the promoted one"
        );
        assert_eq!(
            registry.admissible_here(),
            CompatibilityProfileRegistry::promoted_here() + 1,
            "an operator profile for a promoted identity adds a candidate rather than replacing \
             the promoted entry in the count"
        );
    }

    #[test]
    fn reserved_terminal_cells_fail_with_stable_typed_rejections() {
        for (terminal_profile, input_transport) in [
            (TerminalProfile::RmuxStandard, InputTransport::Sdk),
            (TerminalProfile::Transparent, InputTransport::AttachedStream),
        ] {
            let error = validate_v1_terminal_support(terminal_profile, input_transport)
                .expect_err("reserved v1 terminal cell must fail closed");
            assert_eq!(error.code, ErrorCode::UnsupportedFeature);
            assert!(!error.retryable);
        }

        for input_transport in [InputTransport::Auto, InputTransport::Sdk] {
            validate_v1_terminal_support(TerminalProfile::Transparent, input_transport)
                .expect("implemented transparent terminal cell must remain admitted");
        }
    }
}
