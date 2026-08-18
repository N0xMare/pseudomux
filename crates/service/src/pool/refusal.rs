//! Every way the stateless pool says no, and **no new `ErrorCode`**.
//!
//! This module adds zero variants to [`ErrorCode`], and that is a load-bearing
//! decision rather than an omission. Both shipped clients HARD-REJECT unknown
//! error codes -- TypeScript through `PMUX_ERROR_CODES`, Python through
//! `KNOWN_ERROR_CODES` -- so a daemon emitting an unknown code to an older
//! client makes that client reject **the whole response frame**. The caller
//! loses the result, not merely the label.
//!
//! # The variant this module deliberately does NOT add
//!
//! `ErrorCode::PoolExhausted` is the better name and is **DECLINED**.
//!
//! - **Trigger to add it:** when a caller must branch programmatically on pool
//!   exhaustion versus every other `session_busy`, and can therefore no longer
//!   key on `details.violation`, which is opaque JSON and not part of the
//!   pinned surface.
//! - **Required migration order, in this order and no other:** (1) widen
//!   `tests/conformance/v1/manifest.json`'s `error_codes`; (2) ship the widened
//!   `PMUX_ERROR_CODES` and `KNOWN_ERROR_CODES` and regenerate
//!   `tests/conformance/v1/{cases,golden}.json`, whose `error_bodies` corpus
//!   pins `unknown_code` as invalid; (3) let those releases reach **every**
//!   deployment; (4) only then may the daemon emit it. Bundle it with any other
//!   closed-enum addition into one loud protocol event.
//!
//! `RateLimited` was the other candidate and is **rejected outright**: it makes
//! a pmux-local instance budget indistinguishable, in every dashboard keying on
//! the code, from Anthropic quota exhaustion. Those demand opposite operator
//! responses -- quota means stop, pool means retry shortly.

use pseudomux_protocol::v1::{ErrorBody, ErrorCode};
use serde_json::json;

use super::class::InstanceClass;
use super::machine::{CensusBucket, InstanceState};

/// How many instances are in each census bucket, and nothing else.
///
/// The pool fills this by binning every live instance through
/// [`InstanceState::census_bucket`], so `live` is the SUM of the parts rather
/// than a separately-maintained count that can disagree with them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BucketCounts {
    serving: u32,
    clearing: u32,
    idle: u32,
    leased: u32,
    reserved: u32,
    tearing_down: u32,
    released: u32,
}

impl BucketCounts {
    /// Count one instance, in whichever bucket its state names.
    pub fn record(&mut self, state: InstanceState) {
        let slot = match state.census_bucket() {
            CensusBucket::Serving => &mut self.serving,
            CensusBucket::Clearing => &mut self.clearing,
            CensusBucket::Idle => &mut self.idle,
            CensusBucket::Leased => &mut self.leased,
            CensusBucket::Reserved => &mut self.reserved,
            CensusBucket::TearingDown => &mut self.tearing_down,
            CensusBucket::Released => &mut self.released,
        };
        *slot = slot.saturating_add(1);
    }

    /// Instances holding a slot. Derived, so no arithmetic in a message can
    /// contradict the clauses beside it.
    #[must_use]
    pub const fn live(self) -> u32 {
        self.serving
            .saturating_add(self.clearing)
            .saturating_add(self.idle)
            .saturating_add(self.leased)
            .saturating_add(self.reserved)
            .saturating_add(self.tearing_down)
    }

    /// Instances a caller is waiting on: `CheckedOut | Delivering`, and
    /// deliberately NOT `Clearing`.
    #[must_use]
    pub const fn in_flight(self) -> u32 {
        self.serving
    }

    /// Instances running `/clear` with no caller waiting on them.
    #[must_use]
    pub const fn clearing(self) -> u32 {
        self.clearing
    }

    /// Instances in the `Idle` STATE.
    ///
    /// The pool's own census publishes the size of the idle SET instead, and
    /// the difference is deliberate: the set is what a checkout reads and the
    /// state is what an instance is, and `Pool::check_invariants` is what ties
    /// them. Two independent derivations of one number means a divergence is
    /// visible -- the pool layer's five counts are asserted to sum to its live
    /// count, and an idle set that has drifted from the states breaks that sum.
    #[must_use]
    pub const fn idle(self) -> u32 {
        self.idle
    }

    #[must_use]
    pub const fn leased(self) -> u32 {
        self.leased
    }

    #[must_use]
    pub const fn reserved(self) -> u32 {
        self.reserved
    }

    #[must_use]
    pub const fn tearing_down(self) -> u32 {
        self.tearing_down
    }

    /// Instances holding a slot that comes back with no caller's help.
    ///
    /// **DERIVED from the same table the census prints**, through
    /// [`CensusBucket::comes_back_on_its_own`], so the number a refused caller
    /// waits on and the numbers the refusal names cannot disagree. Written as
    /// `self.clearing + self.tearing_down` it would be a second, independent
    /// answer to "which states come back on their own" -- the shape that put
    /// `Clearing` in a count called `in_flight` and printed it as "serving a
    /// turn".
    #[must_use]
    pub fn coming_back(self) -> u32 {
        self.clauses()
            .into_iter()
            .filter(|(bucket, _, _)| bucket.comes_back_on_its_own())
            .map(|(_, count, _)| count)
            .fold(0, u32::saturating_add)
    }

    /// Every bucket that has a clause in the census, paired with the phrase
    /// that describes it.
    ///
    /// A wildcard-free match over [`CensusBucket`], so a new bucket is a
    /// compile error here rather than a count with no sentence.
    ///
    /// The array holds the six buckets that hold a slot, and [`Self::live`]
    /// sums exactly those six, so the sentence and the total cannot disagree.
    /// `Released` is deliberately absent from both: an instance in it holds no
    /// slot, and it is unreachable from the pool's live map, since `Leaked` and
    /// `Retired` are both reached and removed under one lock. It exists so the
    /// match above needs no wildcard, and it is given a phrase for the same
    /// reason -- a bucket with no phrase is the thing this shape prevents.
    fn clauses(self) -> [(CensusBucket, u32, &'static str); 6] {
        let phrase = |bucket: CensusBucket| match bucket {
            CensusBucket::Serving => "serving a turn",
            // The sentence this whole type exists for. MEASURED at 8 concurrent
            // callers against 8 slots: the refusal read "7 serving a turn" at a
            // moment when all seven had already handed their answers back and
            // were running `/clear`, which is 703-756 ms of MEASURED work with
            // nobody waiting on it, and not the minutes "serving a turn" tells
            // an operator to wait for. Admission now waits this bucket out
            // rather than refusing over it; see
            // `CensusBucket::comes_back_on_its_own`.
            CensusBucket::Clearing => "clearing between turns, with no caller waiting",
            CensusBucket::Idle => "idle",
            CensusBucket::Leased => "holding a conversation lease",
            CensusBucket::Reserved => "reserved or warming",
            CensusBucket::TearingDown => "in teardown",
            CensusBucket::Released => "holding no slot",
        };
        [
            (
                CensusBucket::Serving,
                self.serving,
                phrase(CensusBucket::Serving),
            ),
            (
                CensusBucket::Clearing,
                self.clearing,
                phrase(CensusBucket::Clearing),
            ),
            (CensusBucket::Idle, self.idle, phrase(CensusBucket::Idle)),
            (
                CensusBucket::Leased,
                self.leased,
                phrase(CensusBucket::Leased),
            ),
            (
                CensusBucket::Reserved,
                self.reserved,
                phrase(CensusBucket::Reserved),
            ),
            (
                CensusBucket::TearingDown,
                self.tearing_down,
                phrase(CensusBucket::TearingDown),
            ),
        ]
    }
}

/// What the pool held when it refused, counted once and rendered by every
/// refusal that has to describe capacity.
///
/// A struct rather than five positional `u32`s because the previous signature
/// was `(budget_instances, in_flight, idle, reserved)` and both call sites
/// passed `pool_size` as the budget. `pool_size` is the CONFIGURED size;
/// [`Self::usable_instances`] is `pool_size - leaked`, and a leak subtracts a
/// slot permanently. A refusal naming the configured size after a leak
/// overstates, forever, what the pool can ever hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolPressure {
    /// `--path-b-pool-size`.
    pub configured_instances: u32,
    /// `configured_instances - leaked`: what this pool can still hold.
    pub usable_instances: u32,
    /// Every live instance, binned by state. `live` is the sum of these.
    pub counts: BucketCounts,
    /// Slots permanently subtracted.
    pub leaked: u32,
}

impl PoolPressure {
    /// Slots held by an instance that comes back with no caller's help.
    ///
    /// Non-zero is exactly the condition under which admission waits instead of
    /// refusing; zero is genuine exhaustion. Delegated to [`BucketCounts`] so
    /// the predicate has one implementation and the refusal that prints the
    /// census and the loop that decides to wait are reading one number.
    #[must_use]
    pub fn coming_back(self) -> u32 {
        self.counts.coming_back()
    }

    fn details(
        self,
        violation: &'static str,
        requested: InstanceClass,
        waited_ms: u64,
    ) -> serde_json::Value {
        json!({
            "violation": violation,
            "budget_instances": self.usable_instances,
            "configured_instances": self.configured_instances,
            "live": self.counts.live(),
            // `CheckedOut | Delivering`. A caller reading this to decide how
            // long to back off is asking "is anyone waiting on those slots",
            // and `clearing` is the answer to a different question.
            "in_flight": self.counts.serving,
            "clearing": self.counts.clearing,
            "idle": self.counts.idle,
            "leased": self.counts.leased,
            "reserved": self.counts.reserved,
            "tearing_down": self.counts.tearing_down,
            "leaked": self.leaked,
            // How long this caller spent inside admission waiting for a slot to
            // come back before it was refused. A client backing off reads a
            // different situation from `0` -- nothing was on its way, retry when
            // something changes -- than from a number, which says pmux already
            // waited this long and the pool is genuinely full.
            "admission_wait_ms": waited_ms,
            "requested_class": {
                "model": requested.canonical_model,
                "effort": requested.effort_argv,
            },
        })
    }

    /// The census clause, in the pool's own vocabulary.
    ///
    /// Every counted state appears, and appears under a phrase that describes
    /// that state and no other. The clause list is derived from
    /// [`CensusBucket`], so a state that is counted but not described is not
    /// expressible: `live` is the sum of exactly the numbers printed here.
    fn census(self) -> String {
        let leaked = if self.leaked == 0 {
            String::new()
        } else {
            format!(
                ", against {} configured before {} slot(s) leaked permanently",
                self.configured_instances, self.leaked
            )
        };
        let clauses = self
            .counts
            .clauses()
            .into_iter()
            .map(|(_, count, phrase)| format!("{count} {phrase}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "{} of {} usable instance(s) are live -- {clauses}{leaked}",
            self.counts.live(),
            self.usable_instances,
        )
    }
}

/// The exhaustion refusal: retryable, and it names the budget and the wait.
///
/// # What actually fires this
///
/// Rule 4 fires when **no instance of the requested class is idle, no other
/// class is idle either, and no slot is free** -- not, as this doc said until
/// the claim was checked against the code, "iff every instance is mid-turn".
/// The three are different: `in_flight` counts `CheckedOut | Delivering |
/// Clearing`, while a slot is equally unavailable when its instance is
/// `Reserved`, `Warming`, `Quarantined` or `Destroying`. A pool whose two
/// instances are both in teardown refuses here with `in_flight == 0`, which the
/// old message rendered as "serving 0 of its 2 configured instances".
///
/// # Why this doc no longer says "immediate"
///
/// It was immediate for every caller, including the caller whose whole pool was
/// running `/clear` -- housekeeping MEASURED at 703-756 ms with nobody waiting
/// on any of it, and none of it a model. MEASURED at
/// 8 concurrent callers against 3 slots: rounds 2 and 3 refused all sixteen
/// callers in 539 and 782 microseconds, each refusal reading "3 clearing between
/// turns, with no caller waiting, 0 idle". That is a false capacity signal, and
/// it starved the pool of any reuse at all -- 3 launches for 3 served calls
/// across 24 attempts.
///
/// Admission now waits, bounded, while [`PoolPressure::coming_back`] is
/// non-zero, and `waited_ms` is what that cost. The design statement is
/// unchanged and is the reason the sentence still ends "nothing is queued":
/// there is no queue, no fairness order, no reservation table and no per-class
/// wait list. A waiter re-reads the pool and races every other waiter; what it
/// is promised is that it will not be told the pool is full while three slots
/// are 30 ms from being free.
#[must_use]
pub fn pool_exhausted(
    pressure: PoolPressure,
    requested: InstanceClass,
    waited_ms: u64,
) -> ErrorBody {
    // The two situations a caller must be able to tell apart, and the reason
    // this clause is in the sentence rather than only in the details blob: an
    // operator reading "waited 0 ms" in a log needs to know that pmux looked and
    // found nothing on its way back, not that pmux refused to look.
    let waited = if waited_ms == 0 {
        "no slot was on its way back, so none was waited for".to_owned()
    } else {
        format!("no slot came back in the {waited_ms} ms this turn waited for one")
    };
    ErrorBody::new(
        ErrorCode::SessionBusy,
        format!(
            "the stateless pool has no instance available for this turn: {}, and no slot is free; \
             {waited}; nothing is queued",
            pressure.census()
        ),
    )
    .retryable(true)
    .with_details(pressure.details("pool_exhausted", requested, waited_ms))
    // The number is the CONFIGURED size and not `usable_instances`, because it
    // is the value of the flag the reader would have to change; a leak makes
    // the two differ and the census clause in the message above already says by
    // how much.
    .advising(format!(
        "retry this turn: nothing is queued, so a slot is only reached by asking for it again. If \
         every turn is refused here, restart pmuxd with --path-b-pool-size above the {} this pool \
         was given (refused above 15)",
        pressure.configured_instances
    ))
}

/// The slot a caller just evicted an instance from turned out to be leaked, so
/// the caller cannot have it back.
///
/// A SEPARATE refusal from [`pool_exhausted`] rather than a reuse of it. The
/// reclaim path reached `pool_exhausted` with `in_flight == 0` and a non-zero
/// `idle` -- another class can still be idle at that moment, since a cold swap
/// takes one victim and leaves the rest -- so the exhaustion message described
/// a pool state that was not the reason for the refusal. The cause here is not
/// capacity pressure; it is that this specific slot's teardown could not prove
/// its process reaped, and reusing it would risk minting a replacement beside a
/// prior caller's `history.jsonl`.
///
/// Same code and same retryability: the caller's next attempt goes through
/// admission afresh, and the pool is one slot smaller from now on.
///
/// `waited_ms` is this caller's ADMISSION wait, carried down from
/// `super::Pool::admit` -- private, so deliberately not an intra-doc link from
/// public documentation -- rather than defaulted to zero here. It is the same
/// field with the same meaning as in [`pool_exhausted`]: a caller that waited
/// 400 ms for a slot to come back and then found the slot leaked has waited
/// 400 ms, and publishing `0` would make this the one refusal whose details
/// understate what the turn cost.
#[must_use]
pub fn reclaimed_slot_leaked(
    pressure: PoolPressure,
    requested: InstanceClass,
    waited_ms: u64,
) -> ErrorBody {
    ErrorBody::new(
        ErrorCode::SessionBusy,
        format!(
            "the stateless pool slot this turn evicted an instance from could not be proven \
             reaped, so it is permanently subtracted rather than reused: {}; nothing is queued",
            pressure.census()
        ),
    )
    .retryable(true)
    .with_details(pressure.details("reclaimed_slot_leaked", requested, waited_ms))
    .advising(
        "retry this turn: it is refused about one slot and the rest of the pool is unaffected. The \
         lost slot does not come back on its own -- restart pmuxd to recover it",
    )
}

/// No pool is configured. A Path B caller is the only caller of its method and
/// has no fallback path, so this refusal must be unambiguous.
///
/// It names the flag, because the person who reads it is often not the person
/// who started the daemon and has no other way to learn what is missing. The
/// health tree's own answer for this condition already said so
/// (`native.rs`: "--path-b-parent is what enables one and this daemon was not
/// given it"); this is the same fact on the path a caller actually hits.
#[must_use]
pub fn path_b_not_enabled() -> ErrorBody {
    ErrorBody::new(
        ErrorCode::UnsupportedFeature,
        "the stateless token engine is not enabled on this daemon: it is off unless pmuxd was \
         started with --path-b-parent, and restarting pmuxd with --path-b-parent DIR and \
         --path-b-claude PATH is what enables it",
    )
    .with_details(json!({"violation": "path_b_not_enabled"}))
    .advising("restart pmuxd with --path-b-parent DIR --path-b-claude /absolute/path/to/claude")
}

/// The pool has halted. Reached when the transcript `/clear` opened is not the
/// preamble pmux measured -- `/clear` selected some other command, or the
/// preamble carries a record type, a subtype, a row shape or a row count pmux
/// has never seen. None of those is one bad instance: each is pmux's model of
/// the composer no longer matching the installed Claude, so the pool stops
/// minting, refuses every checkout, and pages.
///
/// `violation` is the `assert_empty` reason, so the operator is sent to the
/// part of the preamble that moved rather than to the general claim. It is also
/// re-promotion trigger 4 (`docs/version-drift.md` sec.5 P2), which is why the
/// details name the trigger beside it.
#[must_use]
pub fn pool_halted(violation: &'static str) -> ErrorBody {
    ErrorBody::new(
        ErrorCode::SchemaDrift,
        "the stateless pool has halted: pmux's model of the installed Claude's post-/clear preamble no longer matches it",
    )
    .with_details(json!({
        "violation": violation,
        "repromotion_trigger":
            crate::compatibility::RepromotionTrigger::ClearScreenOrPreambleMismatch.id(),
    }))
    .advising(crate::compatibility::RepromotionTrigger::ClearScreenOrPreambleMismatch.detector().how)
}

/// A sidechain row on a cell whose tool surface is denied.
///
/// A Path B cell launches with `--disallowedTools "*"`, so a sidechain is
/// structurally unreachable. A row means the tool surface is not empty and the
/// isolation claim is false. Under-reporting that turn's tokens would be a
/// wrong answer; refusing is merely bad.
#[must_use]
pub fn sidechain_on_toolless_cell(rows: usize) -> ErrorBody {
    ErrorBody::new(
        ErrorCode::SchemaDrift,
        "the stateless turn's transcript carried a sidechain row on a cell whose tool surface is denied",
    )
    .with_details(json!({
        "violation": "sidechain_row_on_toolless_cell",
        "sidechain_rows": rows,
    }))
    .advising(
        "do not retry this prompt: the next instance is launched the same way and answers the same \
         way. Restart pmuxd with --path-b-retain-dir DIR, reproduce, and read the retained \
         transcript -- a sidechain row on a cell launched with --disallowedTools \"*\" means the \
         tool surface pmux denied is reachable, which is a finding and not a bad turn",
    )
}

/// The host committed a turn without counting the transcript's sidechain rows.
///
/// FAILS CLOSED, and that is the whole point. `HostTurn::sidechain_rows` is an
/// `Option` so a host that cannot count is able to say so instead of
/// fabricating a `0`; the cost of that honesty is that the pool must then treat
/// the silence as a failed check rather than a passed one. A sidechain row
/// carrying no usage is invisible to the token check beside it, so a `None`
/// that committed would be a turn whose isolation claim rested on nothing.
///
/// Not retryable: the same host will answer `None` again.
#[must_use]
pub fn sidechain_rows_not_counted() -> ErrorBody {
    ErrorBody::new(
        ErrorCode::UnsupportedFeature,
        "the stateless host committed a turn without counting the transcript's sidechain rows, so the tool-surface isolation claim could not be checked",
    )
    .with_details(json!({
        "violation": "sidechain_rows_not_counted",
    }))
    .advising(
        "do not retry: the same host answers `None` again. This is a pmux build whose stateless \
         host cannot count a transcript's sidechain rows, so upgrade pmuxd rather than the prompt",
    )
}

/// The daemon is shutting down. Retryable against a replacement.
///
/// `retryable` says a retry CAN succeed and says nothing about where, and this
/// is the one refusal in the module where the two differ: retrying against this
/// daemon is refused for as long as it answers at all. The advice is therefore
/// not decoration on an obvious case -- it is the only part of the body that
/// says the retry has to go somewhere else.
#[must_use]
pub fn daemon_shutting_down() -> ErrorBody {
    ErrorBody::new(ErrorCode::DaemonLost, "the stateless pool is shutting down")
        .retryable(true)
        .advising(
            "retry against the replacement daemon once its socket accepts connections: this one is \
             draining and will refuse every remaining turn",
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::class::resolve_pool_class;
    use pseudomux_protocol::v1::EffortLevel;
    use std::collections::BTreeSet;

    fn class() -> InstanceClass {
        resolve_pool_class("claude-opus-5", Some(EffortLevel::High))
            .expect("admitted")
            .0
    }

    /// A pool of `size`, every slot occupied by an instance in `state`.
    ///
    /// Built by RECORDING a real [`InstanceState`] rather than by assigning a
    /// count. The previous helper took a closure that wrote a field directly,
    /// so a test could assert a sentence about a census the state machine
    /// cannot produce -- and, more to the point, could assert "15 serving a
    /// turn" without any instance ever being in a state where a caller waits.
    fn all_in(size: u32, state: InstanceState) -> PoolPressure {
        let mut counts = BucketCounts::default();
        for _ in 0..size {
            counts.record(state);
        }
        PoolPressure {
            configured_instances: size,
            usable_instances: size,
            counts,
            leaked: 0,
        }
    }

    #[test]
    fn exhaustion_names_the_budget_and_invites_a_retry() {
        let body = pool_exhausted(all_in(15, InstanceState::CheckedOut), class(), 0);
        assert_eq!(body.code, ErrorCode::SessionBusy);
        assert!(body.retryable, "capacity comes back; the caller may retry");
        assert_eq!(
            body.details
                .get("violation")
                .and_then(|value| value.as_str()),
            Some("pool_exhausted")
        );
        assert_eq!(
            body.details
                .get("budget_instances")
                .and_then(serde_json::Value::as_u64),
            Some(15)
        );
        assert_eq!(
            body.details
                .get("in_flight")
                .and_then(serde_json::Value::as_u64),
            Some(15)
        );
        assert_eq!(
            body.details
                .get("requested_class")
                .and_then(|value| value.get("model"))
                .and_then(|value| value.as_str()),
            Some("claude-opus-5"),
            "an operator reading this must be able to see which shape was refused"
        );
        assert!(
            body.message
                .contains("15 of 15 usable instance(s) are live")
                && body.message.contains("15 serving a turn"),
            "the message names the budget, not just the details blob: {}",
            body.message
        );
        assert!(
            body.message.contains("nothing is queued"),
            "the message says there is no queue, because a caller that assumes one will not retry"
        );
    }

    /// The refusal describes the states that actually held the slots.
    ///
    /// MEASURED against the old message, which was `"the stateless pool is
    /// serving {in_flight} of its {budget} configured instances"`. Rule 4 fires
    /// whenever nothing is idle and no slot is free, so a pool refusing with
    /// both instances in teardown produced "serving 0 of its 2", a sentence
    /// that reads as spare capacity while refusing for the lack of it.
    #[test]
    fn a_refusal_with_nothing_in_flight_still_says_what_held_the_slots() {
        for (label, state) in [
            ("teardown", InstanceState::Destroying),
            ("quarantine", InstanceState::Quarantined),
            ("warming", InstanceState::Warming),
            ("reserved", InstanceState::Reserved),
            ("clearing", InstanceState::Clearing),
        ] {
            let body = pool_exhausted(all_in(2, state), class(), 0);
            assert!(
                !body.message.contains("serving 0"),
                "{label}: the refusal must not read as spare capacity: {}",
                body.message
            );
            assert!(
                body.message.contains("2 of 2 usable instance(s) are live"),
                "{label}: {}",
                body.message
            );
            assert_eq!(
                body.details
                    .get("in_flight")
                    .and_then(serde_json::Value::as_u64),
                Some(0),
                "{label}: no caller is waiting on any of these"
            );
        }
        assert!(
            pool_exhausted(all_in(2, InstanceState::Destroying), class(), 0)
                .message
                .contains("2 in teardown")
        );
        assert!(
            pool_exhausted(all_in(2, InstanceState::Warming), class(), 0)
                .message
                .contains("2 reserved or warming")
        );
    }

    /// **A pool whose every instance is running `/clear` is not serving a turn.**
    ///
    /// This is the state a pool under load refuses from most often, and the one
    /// the old message got wrong. `spawn_clear` exists so the caller's answer is
    /// written BEFORE `/clear` is typed, so a caller that immediately asks again
    /// meets a pool whose slots are all `Clearing` -- and was told all of them
    /// were serving turns. MEASURED over the real socket at 8 concurrent callers
    /// against 8 slots: `"8 of 8 usable instance(s) are live -- 7 serving a
    /// turn, 0 idle, 0 reserved or warming, 1 in teardown"`, at an instant when
    /// every one of those seven had already handed its answer back.
    ///
    /// The two numbers an operator acts on are opposite in magnitude: a clear
    /// MEASURES 703-756 ms end to end (`config::ADMISSION_WAIT_CEILING_MS`; the
    /// ~30 ms in path-b.md sec.3.4 is the transcript rotation, not the emptiness
    /// proof), and a turn is however long the model takes -- 3186 ms was the
    /// warm median at sonnet/low, and there is no upper bound on it.
    #[test]
    fn a_pool_that_is_only_clearing_says_so_and_claims_no_caller_is_waiting() {
        let body = pool_exhausted(all_in(8, InstanceState::Clearing), class(), 0);
        assert!(
            body.message
                .contains("8 clearing between turns, with no caller waiting"),
            "the refusal must name the state that actually held the slots: {}",
            body.message
        );
        assert!(
            body.message.contains("0 serving a turn"),
            "a clearing instance has already answered, so nobody is waiting on it: {}",
            body.message
        );
        assert_eq!(
            body.details
                .get("in_flight")
                .and_then(serde_json::Value::as_u64),
            Some(0),
            "`in_flight` answers 'is a caller waiting', and no caller is"
        );
        assert_eq!(
            body.details
                .get("clearing")
                .and_then(serde_json::Value::as_u64),
            Some(8),
            "the count still has to be reachable programmatically"
        );
        assert!(
            body.message.contains("8 of 8 usable instance(s) are live"),
            "{}",
            body.message
        );
    }

    /// `coming_back` counts exactly the slots the census says come back.
    ///
    /// The number `Pool::admit` waits on. It is checked against the CLAUSES the
    /// same refusal prints -- the only other place the buckets are enumerated --
    /// so a count and a sentence that disagree is not expressible. A hand-written
    /// `clearing + tearing_down` would be a second answer to the question, which
    /// is how `Clearing` came to live inside a count called `in_flight`.
    #[test]
    fn the_slots_a_caller_waits_for_are_the_ones_the_census_says_come_back() {
        for state in [
            InstanceState::Reserved,
            InstanceState::Warming,
            InstanceState::Idle,
            InstanceState::Leased,
            InstanceState::CheckedOut,
            InstanceState::Delivering,
            InstanceState::Clearing,
            InstanceState::Quarantined,
            InstanceState::Destroying,
        ] {
            let pressure = all_in(4, state);
            let expected = if state.census_bucket().comes_back_on_its_own() {
                4
            } else {
                0
            };
            assert_eq!(
                pressure.coming_back(),
                expected,
                "{state} is in the {:?} bucket",
                state.census_bucket()
            );
        }
        // Mixed, so the filter is exercised rather than an all-or-nothing
        // census: two of the five clauses come back and three do not.
        let mut counts = BucketCounts::default();
        for state in [
            InstanceState::CheckedOut,
            InstanceState::Idle,
            InstanceState::Reserved,
            InstanceState::Clearing,
            InstanceState::Clearing,
            InstanceState::Destroying,
        ] {
            counts.record(state);
        }
        assert_eq!(counts.coming_back(), 3);
        assert_eq!(
            counts.coming_back() + 3,
            counts.live(),
            "every live instance either comes back on its own or does not"
        );
    }

    /// The refusal states whether it waited, and for how long.
    ///
    /// Both sentences are asserted, because the pair is the operator-facing
    /// content of the fix: `0` means pmux looked and found nothing on its way
    /// back, and a number means pmux already waited that long and the pool is
    /// genuinely full. A refusal that could not tell those apart is the one that
    /// shipped -- it refused a whole clearing pool in microseconds and said
    /// "nothing is queued", which is true and not the thing the caller needed
    /// to know.
    #[test]
    fn a_capacity_refusal_says_whether_it_waited_and_for_how_long() {
        let immediate = pool_exhausted(all_in(2, InstanceState::CheckedOut), class(), 0);
        assert!(
            immediate
                .message
                .contains("no slot was on its way back, so none was waited for"),
            "{}",
            immediate.message
        );
        assert_eq!(
            immediate
                .details
                .get("admission_wait_ms")
                .and_then(serde_json::Value::as_u64),
            Some(0)
        );

        let waited = pool_exhausted(all_in(2, InstanceState::Clearing), class(), 503);
        assert!(
            waited
                .message
                .contains("no slot came back in the 503 ms this turn waited for one"),
            "{}",
            waited.message
        );
        assert_eq!(
            waited
                .details
                .get("admission_wait_ms")
                .and_then(serde_json::Value::as_u64),
            Some(503)
        );
        // The leaked-slot reclaim publishes the SAME field with the same
        // meaning: it is reached through admission and inherits its wait, so a
        // caller cannot read one refusal as free and the other as costly.
        assert_eq!(
            reclaimed_slot_leaked(all_in(2, InstanceState::Clearing), class(), 503)
                .details
                .get("admission_wait_ms")
                .and_then(serde_json::Value::as_u64),
            Some(503)
        );
        for body in [&immediate, &waited] {
            assert!(
                body.message.contains("nothing is queued"),
                "waiting is not queueing, and the sentence that says so must survive: {}",
                body.message
            );
        }
    }

    /// Every counted state is described by a clause, and the clauses sum to the
    /// live count the same sentence names.
    ///
    /// The property the bucket table exists for: a state that holds a slot but
    /// appears in no clause is a refusal that describes less than it refuses
    /// for. It is checked over the states themselves rather than over a list of
    /// clause names, so a new `InstanceState` cannot pass by being forgotten in
    /// two places at once.
    #[test]
    fn every_slot_holding_state_is_named_by_a_clause_that_adds_up() {
        let states = [
            InstanceState::Reserved,
            InstanceState::Warming,
            InstanceState::Idle,
            InstanceState::Leased,
            InstanceState::CheckedOut,
            InstanceState::Delivering,
            InstanceState::Clearing,
            InstanceState::Quarantined,
            InstanceState::Destroying,
        ];
        let mut counts = BucketCounts::default();
        for state in states {
            assert!(
                state.counts_against_pool(),
                "{state} does not hold a slot and does not belong in this census"
            );
            counts.record(state);
        }
        let live = u32::try_from(states.len()).expect("nine states");
        assert_eq!(
            counts.live(),
            live,
            "every slot-holding state must land in exactly one clause"
        );
        let printed: u32 = counts
            .clauses()
            .into_iter()
            .map(|(_, count, _)| count)
            .sum();
        assert_eq!(
            printed, live,
            "the clauses a refusal prints must account for every live instance"
        );
        let body = pool_exhausted(
            PoolPressure {
                configured_instances: live,
                usable_instances: live,
                counts,
                leaked: 0,
            },
            class(),
            0,
        );
        for (_, count, phrase) in counts.clauses() {
            assert!(
                body.message.contains(&format!("{count} {phrase}")),
                "the census omits the {phrase:?} clause: {}",
                body.message
            );
        }
        // The two states that hold no slot are counted nowhere in the live
        // census, which is what makes `live()` the sum of the clauses.
        let mut released = BucketCounts::default();
        released.record(InstanceState::Leaked);
        released.record(InstanceState::Retired);
        assert_eq!(released.live(), 0);
    }

    /// A leak subtracts a slot permanently, and the refusal says so.
    ///
    /// The old signature took `budget_instances` and both call sites passed
    /// `pool_size`, so a pool that had leaked 1 of 2 slots kept saying "its 2
    /// configured instances" for the rest of the process's life.
    #[test]
    fn a_leaked_slot_is_subtracted_from_the_budget_the_refusal_names() {
        let mut counts = BucketCounts::default();
        counts.record(InstanceState::CheckedOut);
        let pressure = PoolPressure {
            configured_instances: 2,
            usable_instances: 1,
            counts,
            leaked: 1,
        };
        let body = pool_exhausted(pressure, class(), 0);
        assert_eq!(
            body.details
                .get("budget_instances")
                .and_then(serde_json::Value::as_u64),
            Some(1),
            "the budget is what the pool can still hold, not what it was configured for"
        );
        assert_eq!(
            body.details
                .get("configured_instances")
                .and_then(serde_json::Value::as_u64),
            Some(2),
            "the configured size is still published, so the loss is legible"
        );
        assert!(
            body.message.contains("1 of 1 usable instance(s)")
                && body
                    .message
                    .contains("against 2 configured before 1 slot(s) leaked permanently"),
            "{}",
            body.message
        );
    }

    /// The leaked-slot reclaim is its own refusal, not a reused exhaustion one.
    #[test]
    fn a_reclaimed_leaked_slot_is_not_reported_as_exhaustion() {
        // The state that made the reuse wrong: nothing in flight, another
        // class still idle, and the refusal is nevertheless correct.
        let mut counts = BucketCounts::default();
        counts.record(InstanceState::Idle);
        let pressure = PoolPressure {
            configured_instances: 3,
            usable_instances: 2,
            counts,
            leaked: 1,
        };
        let body = reclaimed_slot_leaked(pressure, class(), 0);
        assert_eq!(body.code, ErrorCode::SessionBusy);
        assert!(body.retryable);
        assert_eq!(
            body.details
                .get("violation")
                .and_then(|value| value.as_str()),
            Some("reclaimed_slot_leaked"),
            "an operator must be able to tell a leak from a busy pool"
        );
        assert!(
            body.message.contains("could not be proven reaped"),
            "the message names the cause, which is not capacity pressure: {}",
            body.message
        );
        assert!(
            !pool_exhausted(pressure, class(), 0)
                .message
                .contains("could not be proven reaped"),
            "the two refusals must not converge on one sentence"
        );
    }

    /// Every refusal constructor this module exposes, named once.
    ///
    /// The census exists because the check below used to build its own list of
    /// six by hand, under a name that says *every*, while the module had seven.
    /// `sidechain_rows_not_counted` was the seventh and nothing tested its code
    /// at all -- MEASURED: with that function answering `ErrorCode::Internal`,
    /// a code this module never committed to, the whole `pool::refusal` suite
    /// stayed green at 12 passed.
    ///
    /// One list, checked from two directions, so neither can quietly narrow:
    /// [`the_refusal_census_names_every_constructor_this_module_has`] derives
    /// the truth from the module's own source and refuses an omission, and
    /// [`every_pool_refusal_uses_a_code_both_shipped_clients_already_know`]
    /// requires a constructed body for each name.
    const REFUSAL_CENSUS: &[&str] = &[
        "daemon_shutting_down",
        "path_b_not_enabled",
        "pool_exhausted",
        "pool_halted",
        "reclaimed_slot_leaked",
        "sidechain_on_toolless_cell",
        "sidechain_rows_not_counted",
    ];

    /// The census is the WHOLE module, derived rather than trusted.
    ///
    /// Every `pub fn` in this file is a refusal constructor today, so "the
    /// module's public surface" and "the set of refusals" are the same set and
    /// the derivation needs no classifier. A `pub fn` added here that is *not*
    /// a refusal turns this red rather than passing silently, which is the
    /// right way round: whoever adds it decides, in the open, whether the
    /// census grew.
    #[test]
    fn the_refusal_census_names_every_constructor_this_module_has() {
        let declared: BTreeSet<&str> = include_str!("refusal.rs")
            .lines()
            .filter_map(|line| line.strip_prefix("pub fn "))
            .map(|rest| {
                rest.split(['(', '<'])
                    .next()
                    .expect("split always yields a first item")
            })
            .collect();
        let censused: BTreeSet<&str> = REFUSAL_CENSUS.iter().copied().collect();
        assert_eq!(
            declared, censused,
            "the refusal census and this module's public surface disagree; \
             every refusal must be named in REFUSAL_CENSUS and given a body below"
        );
    }

    /// One constructed body per censused refusal, paired with the name it was
    /// built from.
    ///
    /// Shared by every property below rather than rebuilt per test, so a
    /// refusal cannot be covered by one of them and missed by another: the
    /// census check on this list is what makes "every refusal" mean the whole
    /// module in each of them at once.
    fn every_refusal_body() -> Vec<(&'static str, ErrorBody)> {
        vec![
            (
                "pool_exhausted",
                pool_exhausted(all_in(1, InstanceState::CheckedOut), class(), 0),
            ),
            (
                "reclaimed_slot_leaked",
                reclaimed_slot_leaked(all_in(1, InstanceState::CheckedOut), class(), 0),
            ),
            ("path_b_not_enabled", path_b_not_enabled()),
            ("pool_halted", pool_halted("wrong_local_command")),
            ("sidechain_on_toolless_cell", sidechain_on_toolless_cell(1)),
            ("sidechain_rows_not_counted", sidechain_rows_not_counted()),
            ("daemon_shutting_down", daemon_shutting_down()),
        ]
    }

    /// The list above is the census, so "every refusal" is the whole module in
    /// every property that iterates it.
    #[test]
    fn every_censused_refusal_has_exactly_one_constructed_body() {
        assert_eq!(
            every_refusal_body()
                .iter()
                .map(|(name, _)| *name)
                .collect::<BTreeSet<_>>(),
            REFUSAL_CENSUS.iter().copied().collect::<BTreeSet<_>>(),
            "a censused refusal has no body here, or a body has no census entry"
        );
        assert_eq!(
            every_refusal_body().len(),
            REFUSAL_CENSUS.len(),
            "a refusal is built twice, which would let one property cover it and another miss it"
        );
    }

    #[test]
    fn every_pool_refusal_uses_a_code_both_shipped_clients_already_know() {
        // The whole point of this module. If one of these ever needs a new
        // variant, the migration in the module doc applies -- it is not a
        // one-line change.
        let admitted = [
            ErrorCode::SessionBusy,
            ErrorCode::UnsupportedFeature,
            ErrorCode::SchemaDrift,
            ErrorCode::DaemonLost,
        ];
        for (name, body) in every_refusal_body() {
            assert!(
                admitted.contains(&body.code),
                "{name} answers {:?}, which is not one of the codes this module committed to",
                body.code
            );
        }
    }

    /// Every refusal says what to do next, and says it in the one place a
    /// reader looks.
    ///
    /// **Five of the seven said nothing.** `path_b_not_enabled` and
    /// `pool_halted` carried a `recommendation` and the other five carried a
    /// census, a violation and no action at all -- so a caller reading
    /// `session_busy` learnt that the pool was full and not that retrying is
    /// the whole of the response, and a caller reading `schema_drift` on a
    /// sidechain row learnt that a row existed and not that retrying is
    /// pointless. `retryable` is not that sentence: it says a retry CAN
    /// succeed, not what else has to change first, and
    /// [`daemon_shutting_down`] is `retryable` against a daemon that will
    /// refuse every remaining turn.
    ///
    /// Derived from the census, so this cannot be satisfied by a list that
    /// stops growing: a new `pub fn` here reddens
    /// [`the_refusal_census_names_every_constructor_this_module_has`], which
    /// forces a census entry, which forces a body in [`every_refusal_body`],
    /// which lands here with no advice.
    #[test]
    fn every_pool_refusal_says_what_to_do_next() {
        for (name, body) in every_refusal_body() {
            let recommendation = body
                .recommendation()
                .unwrap_or_else(|| panic!("{name} refuses a caller without naming an action"));
            // An action, not a restatement. Every one of these names either a
            // command to run or the decision to retry, and a recommendation
            // that merely repeats the message is the shape this test exists to
            // stop: it would leave the caller exactly where the message did.
            assert!(
                recommendation.contains("retry") || recommendation.contains("pmuxd"),
                "{name}'s recommendation names no command and no retry decision: {recommendation}"
            );
            assert_ne!(
                recommendation, body.message,
                "{name}'s recommendation is its message again, which advises nothing"
            );
            // The advice is IN `details`, beside the violation rather than
            // instead of it. `advising` merges, and a builder that replaced
            // would silently drop whichever half was written first.
            assert!(
                body.details.get("violation").is_some() || name == "daemon_shutting_down",
                "{name} lost its violation when it gained advice"
            );
        }
    }

    #[test]
    fn a_disabled_pool_is_unsupported_rather_than_busy() {
        let body = path_b_not_enabled();
        assert_eq!(body.code, ErrorCode::UnsupportedFeature);
        assert!(
            !body.retryable,
            "a daemon without a pool will not grow one on a retry"
        );
        assert_eq!(
            body.details
                .get("violation")
                .and_then(|value| value.as_str()),
            Some("path_b_not_enabled")
        );
    }

    #[test]
    fn a_sidechain_row_is_schema_drift_and_never_a_silent_undercount() {
        let body = sidechain_on_toolless_cell(3);
        assert_eq!(body.code, ErrorCode::SchemaDrift);
        assert!(!body.retryable);
        assert_eq!(
            body.details
                .get("violation")
                .and_then(|value| value.as_str()),
            Some("sidechain_row_on_toolless_cell")
        );
        assert_eq!(
            body.details
                .get("sidechain_rows")
                .and_then(serde_json::Value::as_u64),
            Some(3)
        );
    }

    #[test]
    fn a_halted_pool_and_a_shutting_down_daemon_are_told_apart() {
        // Two different operator responses: a halt means go read the composer
        // geometry, a shutdown means wait for the replacement.
        assert_eq!(
            pool_halted("wrong_local_command").code,
            ErrorCode::SchemaDrift
        );
        assert!(!pool_halted("wrong_local_command").retryable);
        assert_eq!(daemon_shutting_down().code, ErrorCode::DaemonLost);
        assert!(daemon_shutting_down().retryable);
    }
    /// Each per-bucket accessor answers about ITS OWN bucket.
    ///
    /// SURVIVING MUTANTS CLOSED: six -- `BucketCounts::idle -> 0` and `-> 1`
    /// (`refusal.rs:100`), `::reserved -> 0` and `-> 1` (`:105`), and
    /// `::tearing_down -> 0` and `-> 1` (`:110`). Every existing case reached
    /// these through `pool_exhausted`'s rendered sentence, and a census where
    /// the interesting bucket happened to hold zero or one instance cannot tell
    /// the accessor from a constant. So each bucket here is loaded with a
    /// DISTINCT count of at least two: distinct so an accessor reading its
    /// neighbour's field is caught too, and at least two so neither constant
    /// the mutation tool substitutes can pass.
    ///
    /// The states are chosen through `census_bucket`, which is the mapping the
    /// counter itself uses, rather than assumed -- and the mapping is asserted
    /// per row, so a state that is refiled under another bucket tomorrow fails
    /// here instead of silently moving what this test is about.
    #[test]
    fn every_bucket_accessor_reports_its_own_bucket_and_not_a_constant() {
        /// One row: a state, the bucket it files under, how many of it to
        /// record, and the accessor that must report exactly that many.
        type BucketRow = (InstanceState, CensusBucket, u32, fn(BucketCounts) -> u32);

        let rows: &[BucketRow] = &[
            (
                InstanceState::Idle,
                CensusBucket::Idle,
                2,
                BucketCounts::idle,
            ),
            (
                InstanceState::Reserved,
                CensusBucket::Reserved,
                3,
                BucketCounts::reserved,
            ),
            (
                InstanceState::Quarantined,
                CensusBucket::TearingDown,
                4,
                BucketCounts::tearing_down,
            ),
            (
                InstanceState::Clearing,
                CensusBucket::Clearing,
                5,
                BucketCounts::clearing,
            ),
            (
                InstanceState::Leased,
                CensusBucket::Leased,
                6,
                BucketCounts::leased,
            ),
        ];

        let mut counts = BucketCounts::default();
        for (state, bucket, many, _) in rows {
            assert_eq!(
                state.census_bucket(),
                *bucket,
                "{state} must file under {bucket:?} for this row to be about that bucket"
            );
            for _ in 0..*many {
                counts.record(*state);
            }
        }
        for (state, _, many, accessor) in rows {
            assert_eq!(
                accessor(counts),
                *many,
                "the accessor for {state}'s bucket must report the {many} \
                 instance(s) recorded in it and nothing else"
            );
        }
        // ...and `live` still agrees with the sum, so the counts above are the
        // pool's own arithmetic rather than four independent numbers.
        assert_eq!(
            counts.live(),
            rows.iter().map(|(_, _, many, _)| many).sum::<u32>(),
            "live must be the sum of the buckets that hold a slot"
        );
    }
}
