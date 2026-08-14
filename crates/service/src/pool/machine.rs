//! The instance state machine, as a total function over (state, transition).
//!
//! Nothing here touches a process, a file, or a clock, so every reachable edge
//! is exercisable by a unit test and there is no excuse for a thin one.
//!
//! # The global invariant, which holds in every state including the transients
//!
//! **G.** For every instance in every state: (a) exactly one Claude process,
//! one config root, one cwd and one bound transcript belong to it and to no
//! other instance or session; (b) at most one caller holds it, and only in
//! [`InstanceState::CheckedOut`]; (c) its pmux `SessionId` has never appeared in
//! any byte pmux wrote to any client socket; (d) its root exists on disk **iff**
//! it has not yet completed destruction, or its process was never proven reaped.
//!
//! # The invariant this module exists to make unforgeable
//!
//! **Membership in the idle set IS the emptiness proof.** An instance reaches
//! [`InstanceState::Idle`] by exactly two transitions --
//! [`Transition::WarmProven`] (through `assert_empty_at_launch`) and
//! [`Transition::ClearProven`] (through `assert_empty_after_clear`) -- and a
//! failure of either sends it to [`InstanceState::Quarantined`] instead. There
//! is deliberately no cached `EmptinessProof` re-checked at checkout: a cached
//! copy re-checked later is checking the applicant's paperwork instead of the
//! resource, which is how six of the nine leaks in this codebase happened.
//!
//! [`idle_is_proof_carrying`] states that implication as executable code, and
//! [`step`] is the only way to change a state, so a third insertion into the
//! idle set cannot be added without contradicting one of them.

use std::fmt;

/// Where one instance is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum InstanceState {
    /// Counted against `pool_size`; owns no filesystem path and no process.
    ///
    /// Exists so that N concurrent cold calls cannot each read `live == 0` and
    /// start N launches for one slot.
    Reserved,
    /// Root and cwd exist, are mode 0700, and are claimed. No turn may be
    /// submitted; not in any idle set.
    Warming,
    /// The bound transcript has been proven inert by a proof minted after the
    /// last byte any caller caused to be written to it, and `turns_started` is
    /// below the recycle cap.
    Idle,
    /// Exactly one turn, one caller, one deadline. Removed from the idle set
    /// before any I/O.
    CheckedOut,
    /// The turn committed a transcript-proven terminal and the answer is on its
    /// way to the caller. No clear has been typed yet: the response is written
    /// before the clear starts, so a slow clear costs capacity, never latency.
    Delivering,
    /// The caller's response has already been handed back. Nobody waits here.
    Clearing,
    /// Not in any idle set, never re-enterable, root retained under the
    /// retention floor. An instance that cannot be proven clean is destroyed,
    /// not reused.
    Quarantined,
    /// Teardown is in flight. The slot is STILL counted against `pool_size`
    /// until destruction completes -- if the slot were released earlier, a
    /// replacement could mint while a prior caller's `history.jsonl` still
    /// existed on disk.
    Destroying,
    /// `process_reaped` was false. Slot permanently subtracted; root and cwd
    /// never deleted; operator paged. An undeletable root is permanent capacity
    /// loss, never a log-and-continue.
    Leaked,
    /// Destroyed, reaped, erased, slot released. Absorbing.
    Retired,
}

impl InstanceState {
    /// Whether this state still consumes a slot against `pool_size`.
    #[must_use]
    pub const fn counts_against_pool(self) -> bool {
        !matches!(self, Self::Retired | Self::Leaked)
    }

    /// Whether an instance in this state owns a filesystem root.
    #[must_use]
    pub const fn owns_a_root(self) -> bool {
        !matches!(self, Self::Reserved | Self::Retired)
    }

    /// Whether this state is absorbing -- no transition leaves it.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Retired | Self::Leaked)
    }

    /// Which clause of a capacity refusal's census counts this state.
    ///
    /// **This match is the only place a state is grouped for an operator, and
    /// it has no wildcard.** A new [`InstanceState`] is a compile error here,
    /// so it cannot be added and then silently counted by none of the clauses
    /// -- which is a slot held by an instance no sentence in the refusal
    /// describes, while `live` counts it.
    ///
    /// The grouping is a claim about the CALLER, not about the pool's activity:
    /// [`Self::Clearing`] is its own bucket precisely because its own doc says
    /// "the caller's response has already been handed back; nobody waits here".
    /// Folding it into the serving bucket -- which is what an `in_flight` count
    /// spanning all three transient states did -- makes a refusal say "8 of 8
    /// are serving a turn" at the exact moment zero of them are, and the
    /// post-answer clear is the state a pool under load is MOST often refusing
    /// from.
    #[must_use]
    pub const fn census_bucket(self) -> CensusBucket {
        match self {
            Self::CheckedOut | Self::Delivering => CensusBucket::Serving,
            Self::Clearing => CensusBucket::Clearing,
            Self::Idle => CensusBucket::Idle,
            Self::Reserved | Self::Warming => CensusBucket::Reserved,
            Self::Quarantined | Self::Destroying => CensusBucket::TearingDown,
            Self::Leaked | Self::Retired => CensusBucket::Released,
        }
    }
}

/// One line of the census a capacity refusal prints.
///
/// Every state maps to exactly one of these through
/// [`InstanceState::census_bucket`], and every bucket renders through one
/// wildcard-free match in `refusal`, so "counted" and "described" are the same
/// fact rather than two lists that can drift.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CensusBucket {
    /// A caller is waiting on this instance right now.
    Serving,
    /// The caller already has its answer; the instance is running `/clear`.
    Clearing,
    /// Published in an idle set and checkout-able.
    Idle,
    /// A slot claimed for a launch that has not finished.
    Reserved,
    /// On its way out, slot still held.
    TearingDown,
    /// Holds no slot. Unreachable from the pool's live map -- both states are
    /// reached and removed under one lock -- and counted anyway, because a
    /// bucket that cannot be observed is cheaper to keep than a wildcard that
    /// would swallow the next state somebody adds.
    Released,
}

impl CensusBucket {
    /// Whether a slot in this bucket comes back with **no caller's help**.
    ///
    /// This is the predicate a refused caller waits on, and it is a claim about
    /// who the pool is waiting FOR, not about how busy the pool is.
    ///
    /// - [`Self::Clearing`] and [`Self::TearingDown`] hold slots nobody is
    ///   waiting on, which pmux itself is already finishing with: a clear
    ///   MEASURED at 703-756 ms end to end over the socket (the "~30 ms" in
    ///   `docs/path-b.md` sec.3.4 is the transcript ROTATION, not the emptiness
    ///   proof the pool awaits -- see `config::ADMISSION_WAIT_CEILING_MS`), and
    ///   a teardown is a close, a reap and one
    ///   `rmtree`. Refusing a caller for either is a FALSE capacity signal --
    ///   the pool says "no instance is available for this turn" about instances
    ///   that will be available almost immediately. MEASURED at 8 concurrent
    ///   callers against 3 slots: rounds 2 and 3 refused all sixteen callers in
    ///   539 and 782 MICROSECONDS, over the sentence "3 clearing between turns,
    ///   with no caller waiting".
    /// - [`Self::Serving`] is the opposite fact and must stay `false`. A caller
    ///   holds that instance and is waiting for a model, which takes however
    ///   long a model takes -- 3186 ms was the warm median at sonnet/low.
    ///   Waiting there is a queue, and a queue turns a fast refusal into a slow
    ///   indeterminate wait, which is the failure mode this design is most
    ///   resolved against.
    /// - [`Self::Reserved`] is `false`, and the reason is worth stating because
    ///   this is the arm most likely to be widened by somebody reading the
    ///   sentence above. A reservation is a launch already claimed by the caller
    ///   who paid for it: `Pool::publish_idle_and_check_out` publishes it and
    ///   takes it under one lock acquisition, so a waiter gets nothing until
    ///   that caller's TURN also finishes, which is the `Serving` case. A
    ///   background re-warm is the one reservation that does end up idle for
    ///   somebody else, and this bucket cannot tell the two apart -- so it
    ///   answers for the case it can prove and refuses the other. That refusal
    ///   is the round-1 behaviour of a cold pool, and it is honest: eight
    ///   callers against three slots means five of them are over the cap.
    /// - [`Self::Idle`] would not have produced a refusal to wait on, and
    ///   [`Self::Released`] holds no slot to come back.
    #[must_use]
    pub const fn comes_back_on_its_own(self) -> bool {
        match self {
            Self::Clearing | Self::TearingDown => true,
            Self::Serving | Self::Reserved | Self::Idle | Self::Released => false,
        }
    }
}

impl fmt::Display for InstanceState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Reserved => "reserved",
            Self::Warming => "warming",
            Self::Idle => "idle",
            Self::CheckedOut => "checked_out",
            Self::Delivering => "delivering",
            Self::Clearing => "clearing",
            Self::Quarantined => "quarantined",
            Self::Destroying => "destroying",
            Self::Leaked => "leaked",
            Self::Retired => "retired",
        };
        formatter.write_str(name)
    }
}

/// What happened to an instance.
///
/// Named for the EVIDENCE rather than for the destination, so that reading a
/// transition tells you what was proven rather than what the pool decided --
/// `ClearProven` and `ClearFailedCoherent` are different facts about the world,
/// not two ways of spelling "destroy".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Transition {
    /// The reservation was taken; the roots are minted and the mint begins.
    BeginWarm,
    /// `assert_empty_at_launch` returned before any actor existed.
    WarmProven,
    /// Any mint failure: the roots, the launch, or the registration.
    MintFailed,
    /// Admission rule 1. Pure bookkeeping, zero I/O.
    CheckOut,
    /// The turn committed a transcript-proven terminal.
    TurnCommitted,
    /// **Any** outcome other than a delivered, transcript-proven turn.
    ///
    /// This is the rule a naive pool is missing. A turn that exhausts its
    /// deadline means pmux does not know whether the model is still generating
    /// into the bound transcript, so typing `/clear` into it and re-admitting it
    /// lets the next caller's prompt interleave with the previous caller's
    /// in-flight generation.
    TurnNotDelivered,
    /// The answer has been handed back to the caller; the clear may start.
    ResponseDelivered,
    /// `clear_and_rebind` resolved the rotation and `assert_empty_after_clear`
    /// passed before the transcript was bound.
    ClearProven,
    /// A clear that succeeded on an instance that has now served its cap.
    RecycleDue,
    /// A clear refusal marked `clear_not_submitted`: nothing was typed, so the
    /// instance is coherent -- but it has no turn to serve and just failed to
    /// clear, so it is destroyed rather than retried.
    ClearFailedCoherent,
    /// Any clear failure NOT marked `clear_not_submitted`. The command may have
    /// landed, so the bound transcript is suspect.
    ClearFailedIncoherent,
    /// The idle TTL elapsed for a class above its declared warm floor.
    IdleExpired,
    /// Chosen as the LRU victim so a class with a live caller can mint.
    ColdSwapVictim,
    /// Daemon shutdown drained this instance.
    ShutdownDrain,
    /// Quarantine teardown begins. A `Tainted` session is never auto-reaped, so
    /// this is a pool obligation rather than a nicety.
    BeginDestroy,
    /// `close_session(Force)` confirmed the owned process boundary was empty
    /// and the root was erased.
    Reaped,
    /// `process_reaped` was false, or the root could not be erased.
    ReapFailed,
}

impl Transition {
    /// The two transitions that carry an emptiness proof, and therefore the
    /// only two that may put an instance into the idle set.
    ///
    /// Stated as data rather than as a condition inside [`step`] so a test can
    /// assert the implication directly.
    pub const PROOF_CARRYING: [Self; 2] = [Self::WarmProven, Self::ClearProven];

    #[must_use]
    pub const fn is_proof_carrying(self) -> bool {
        matches!(self, Self::WarmProven | Self::ClearProven)
    }
}

impl fmt::Display for Transition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

/// A transition that is not an edge of the machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IllegalTransition {
    pub from: InstanceState,
    pub transition: Transition,
}

impl fmt::Display for IllegalTransition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "instance transition {} is not an edge out of {}",
            self.transition, self.from
        )
    }
}

impl std::error::Error for IllegalTransition {}

/// Where an instance begins. Nothing constructs an instance in another state.
pub const INITIAL: InstanceState = InstanceState::Reserved;

/// The whole machine, as one total function.
///
/// Exhaustive on both axes so a state or a transition added later is a compile
/// error at this match rather than a silently-illegal edge.
///
/// # Errors
///
/// Returns [`IllegalTransition`] for every pair that is not an edge. A caller
/// that hits one has a bug in the pool, not in its input, so this is an error
/// rather than a panic only because a daemon must not abort on one.
pub fn step(
    from: InstanceState,
    transition: Transition,
) -> Result<InstanceState, IllegalTransition> {
    use InstanceState as S;
    use Transition as T;

    let to = match (from, transition) {
        // Reservation -> a process.
        (S::Reserved, T::BeginWarm) => S::Warming,
        (S::Reserved, T::MintFailed) => S::Destroying,
        (S::Reserved, T::ShutdownDrain) => S::Destroying,

        // The launch half of assert-empty. The only proof-carrying edge into
        // Idle that does not pass through a caller's turn.
        (S::Warming, T::WarmProven) => S::Idle,
        (S::Warming, T::MintFailed) => S::Destroying,
        (S::Warming, T::ShutdownDrain) => S::Destroying,

        // Idle is the only state a caller can be handed an instance from, and
        // the only exits are a checkout or a destruction.
        (S::Idle, T::CheckOut) => S::CheckedOut,
        (S::Idle, T::IdleExpired | T::ColdSwapVictim | T::ShutdownDrain) => S::Destroying,

        // The governing asymmetry, in two arms: a delivered, transcript-proven
        // turn continues; everything else quarantines.
        (S::CheckedOut, T::TurnCommitted) => S::Delivering,
        (S::CheckedOut, T::TurnNotDelivered) => S::Quarantined,

        // Respond first, clear second. Nobody waits on the clear.
        (S::Delivering, T::ResponseDelivered) => S::Clearing,

        // The clear half of assert-empty, plus the recycle cap and the two
        // failure shapes the driver distinguishes.
        (S::Clearing, T::ClearProven) => S::Idle,
        (S::Clearing, T::RecycleDue | T::ClearFailedCoherent) => S::Destroying,
        (S::Clearing, T::ClearFailedIncoherent) => S::Quarantined,
        // A shutdown takes a clearing instance too, and this edge is why: the
        // caller was answered before `/clear` was typed, so nothing is owed and
        // the instance is being torn down rather than returned to service.
        // Without it, `Pool::shutdown` skipped every instance that had just
        // answered -- which is EVERY instance, right after any burst of work --
        // and left its whole config root on disk. MEASURED: one `pmux ask`
        // followed immediately by SIGTERM left
        // `<parent>/0/0/root/projects/pmux-e2e/<id>.jsonl` carrying that
        // caller's prompt, plus `.claude.json` and `settings.json`, with
        // `leaked` still 0 and nothing logged.
        (S::Clearing, T::ShutdownDrain) => S::Destroying,

        // Quarantine is never re-enterable. Teardown is immediate and always.
        (S::Quarantined, T::BeginDestroy) => S::Destroying,

        // The slot is released only here, and only on a positive reaping.
        (S::Destroying, T::Reaped) => S::Retired,
        (S::Destroying, T::ReapFailed) => S::Leaked,

        // Everything else is not an edge. Written as explicit non-arms so a new
        // state or transition forces a decision here.
        (
            S::Reserved,
            T::WarmProven
            | T::CheckOut
            | T::TurnCommitted
            | T::TurnNotDelivered
            | T::ResponseDelivered
            | T::ClearProven
            | T::RecycleDue
            | T::ClearFailedCoherent
            | T::ClearFailedIncoherent
            | T::IdleExpired
            | T::ColdSwapVictim
            | T::BeginDestroy
            | T::Reaped
            | T::ReapFailed,
        )
        | (
            S::Warming,
            T::BeginWarm
            | T::CheckOut
            | T::TurnCommitted
            | T::TurnNotDelivered
            | T::ResponseDelivered
            | T::ClearProven
            | T::RecycleDue
            | T::ClearFailedCoherent
            | T::ClearFailedIncoherent
            | T::IdleExpired
            | T::ColdSwapVictim
            | T::BeginDestroy
            | T::Reaped
            | T::ReapFailed,
        )
        | (
            S::Idle,
            T::BeginWarm
            | T::WarmProven
            | T::MintFailed
            | T::TurnCommitted
            | T::TurnNotDelivered
            | T::ResponseDelivered
            | T::ClearProven
            | T::RecycleDue
            | T::ClearFailedCoherent
            | T::ClearFailedIncoherent
            | T::BeginDestroy
            | T::Reaped
            | T::ReapFailed,
        )
        | (
            S::CheckedOut,
            T::BeginWarm
            | T::WarmProven
            | T::MintFailed
            | T::CheckOut
            | T::ResponseDelivered
            | T::ClearProven
            | T::RecycleDue
            | T::ClearFailedCoherent
            | T::ClearFailedIncoherent
            | T::IdleExpired
            | T::ColdSwapVictim
            | T::ShutdownDrain
            | T::BeginDestroy
            | T::Reaped
            | T::ReapFailed,
        )
        | (
            S::Delivering,
            T::BeginWarm
            | T::WarmProven
            | T::MintFailed
            | T::CheckOut
            | T::TurnCommitted
            | T::TurnNotDelivered
            | T::ClearProven
            | T::RecycleDue
            | T::ClearFailedCoherent
            | T::ClearFailedIncoherent
            | T::IdleExpired
            | T::ColdSwapVictim
            | T::ShutdownDrain
            | T::BeginDestroy
            | T::Reaped
            | T::ReapFailed,
        )
        | (
            S::Clearing,
            T::BeginWarm
            | T::WarmProven
            | T::MintFailed
            | T::CheckOut
            | T::TurnCommitted
            | T::TurnNotDelivered
            | T::ResponseDelivered
            | T::IdleExpired
            | T::ColdSwapVictim
            | T::BeginDestroy
            | T::Reaped
            | T::ReapFailed,
        )
        | (
            S::Quarantined,
            T::BeginWarm
            | T::WarmProven
            | T::MintFailed
            | T::CheckOut
            | T::TurnCommitted
            | T::TurnNotDelivered
            | T::ResponseDelivered
            | T::ClearProven
            | T::RecycleDue
            | T::ClearFailedCoherent
            | T::ClearFailedIncoherent
            | T::IdleExpired
            | T::ColdSwapVictim
            | T::ShutdownDrain
            | T::Reaped
            | T::ReapFailed,
        )
        | (
            S::Destroying,
            T::BeginWarm
            | T::WarmProven
            | T::MintFailed
            | T::CheckOut
            | T::TurnCommitted
            | T::TurnNotDelivered
            | T::ResponseDelivered
            | T::ClearProven
            | T::RecycleDue
            | T::ClearFailedCoherent
            | T::ClearFailedIncoherent
            | T::IdleExpired
            | T::ColdSwapVictim
            | T::ShutdownDrain
            | T::BeginDestroy,
        )
        | (
            S::Leaked | S::Retired,
            T::BeginWarm
            | T::WarmProven
            | T::MintFailed
            | T::CheckOut
            | T::TurnCommitted
            | T::TurnNotDelivered
            | T::ResponseDelivered
            | T::ClearProven
            | T::RecycleDue
            | T::ClearFailedCoherent
            | T::ClearFailedIncoherent
            | T::IdleExpired
            | T::ColdSwapVictim
            | T::ShutdownDrain
            | T::BeginDestroy
            | T::Reaped
            | T::ReapFailed,
        ) => return Err(IllegalTransition { from, transition }),
    };
    Ok(to)
}

/// What a daemon shutdown does with one instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShutdownAction {
    /// Move it into teardown with this transition, then destroy it.
    Drain(Transition),
    /// A teardown is already under way on another task. Complete it here
    /// rather than starting a second one, so the daemon does not exit with the
    /// tree still on disk.
    Finish,
    /// Shutdown does nothing to it.
    ///
    /// Two different reasons, and both are stated at the arms that choose it
    /// rather than generalized here: a caller is still waiting on an answer, or
    /// the state holds no slot and is not present in the pool's live map at all.
    Keep,
}

/// The shutdown decision, per state, with **no wildcard**.
///
/// This replaced a `match` whose only non-drain arm was `_ => continue` under
/// the comment "a turn in flight keeps its instance: the caller is owed either
/// an answer or a refusal". That sentence is true of `CheckedOut` and
/// `Delivering` and false of the other four states the arm covered.
/// `Clearing` in particular is the state an instance is in immediately after
/// answering -- `spawn_clear` exists so the caller does not wait for the clear
/// -- so a daemon stopped at the end of any burst of work skipped every
/// instance it had just used and left every one of their roots behind.
#[must_use]
pub const fn shutdown_action(state: InstanceState) -> ShutdownAction {
    match state {
        InstanceState::Reserved
        | InstanceState::Warming
        | InstanceState::Idle
        | InstanceState::Clearing => ShutdownAction::Drain(Transition::ShutdownDrain),
        // Quarantine teardown is a pool obligation and the instance is already
        // out of service; shutdown starts the destroy rather than leaving the
        // tree for whichever task gets there first.
        InstanceState::Quarantined => ShutdownAction::Drain(Transition::BeginDestroy),
        InstanceState::Destroying => ShutdownAction::Finish,
        InstanceState::CheckedOut | InstanceState::Delivering => ShutdownAction::Keep,
        // Neither holds a slot, and neither is ever present in the pool's live
        // map: both are reached and removed under one lock.
        InstanceState::Leaked | InstanceState::Retired => ShutdownAction::Keep,
    }
}

/// The choice between returning an instance to service and recycling it, made
/// in exactly one place.
///
/// Both arms follow a proven clear, so the difference is not evidence but
/// capacity hygiene: an instance that has served its cap is torn down and
/// replaced rather than kept forever.
///
/// **Recycle here is capacity hygiene, not a privacy bound.** The measurement
/// that settles it: with 40k tokens seeded into `history.jsonl`, the next
/// turn's `input_tokens` was unchanged at 186 -- the file never reaches model
/// context. The turn cap therefore bounds process growth and long-lived
/// filesystem residue; it does not bound what one caller can read of another.
/// Anyone who later argues the cap protects callers from each other is arguing
/// against that measurement and needs a new one.
#[must_use]
pub const fn clear_success_transition(turns_started: u32, recycle_turns: u32) -> Transition {
    if turns_started >= recycle_turns {
        Transition::RecycleDue
    } else {
        Transition::ClearProven
    }
}

/// The implication that makes the idle set a proof: an instance may sit in the
/// idle set only if the transition that put it there carried a proof.
///
/// Written as a predicate over the pair rather than as a condition inside
/// [`step`] so a model test can quantify over reachable states and assert it,
/// and so a third insertion into the idle set has to contradict a named
/// function rather than slip past an `if`.
#[must_use]
pub const fn idle_is_proof_carrying(state: InstanceState, last: Transition) -> bool {
    !matches!(state, InstanceState::Idle) || last.is_proof_carrying()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_STATES: &[InstanceState] = &[
        InstanceState::Reserved,
        InstanceState::Warming,
        InstanceState::Idle,
        InstanceState::CheckedOut,
        InstanceState::Delivering,
        InstanceState::Clearing,
        InstanceState::Quarantined,
        InstanceState::Destroying,
        InstanceState::Leaked,
        InstanceState::Retired,
    ];

    const ALL_TRANSITIONS: &[Transition] = &[
        Transition::BeginWarm,
        Transition::WarmProven,
        Transition::MintFailed,
        Transition::CheckOut,
        Transition::TurnCommitted,
        Transition::TurnNotDelivered,
        Transition::ResponseDelivered,
        Transition::ClearProven,
        Transition::RecycleDue,
        Transition::ClearFailedCoherent,
        Transition::ClearFailedIncoherent,
        Transition::IdleExpired,
        Transition::ColdSwapVictim,
        Transition::ShutdownDrain,
        Transition::BeginDestroy,
        Transition::Reaped,
        Transition::ReapFailed,
    ];

    /// Every edge of the machine, written out as data.
    ///
    /// This is the second, independent statement of the transition table: `step`
    /// is the implementation and this is the specification. A pair that appears
    /// here and not there, or there and not here, fails
    /// `the_machine_has_exactly_the_edges_the_table_names`.
    const EDGES: &[(InstanceState, Transition, InstanceState)] = &[
        (
            InstanceState::Reserved,
            Transition::BeginWarm,
            InstanceState::Warming,
        ),
        (
            InstanceState::Reserved,
            Transition::MintFailed,
            InstanceState::Destroying,
        ),
        (
            InstanceState::Reserved,
            Transition::ShutdownDrain,
            InstanceState::Destroying,
        ),
        (
            InstanceState::Warming,
            Transition::WarmProven,
            InstanceState::Idle,
        ),
        (
            InstanceState::Warming,
            Transition::MintFailed,
            InstanceState::Destroying,
        ),
        (
            InstanceState::Warming,
            Transition::ShutdownDrain,
            InstanceState::Destroying,
        ),
        (
            InstanceState::Idle,
            Transition::CheckOut,
            InstanceState::CheckedOut,
        ),
        (
            InstanceState::Idle,
            Transition::IdleExpired,
            InstanceState::Destroying,
        ),
        (
            InstanceState::Idle,
            Transition::ColdSwapVictim,
            InstanceState::Destroying,
        ),
        (
            InstanceState::Idle,
            Transition::ShutdownDrain,
            InstanceState::Destroying,
        ),
        (
            InstanceState::CheckedOut,
            Transition::TurnCommitted,
            InstanceState::Delivering,
        ),
        (
            InstanceState::CheckedOut,
            Transition::TurnNotDelivered,
            InstanceState::Quarantined,
        ),
        (
            InstanceState::Delivering,
            Transition::ResponseDelivered,
            InstanceState::Clearing,
        ),
        (
            InstanceState::Clearing,
            Transition::ClearProven,
            InstanceState::Idle,
        ),
        (
            InstanceState::Clearing,
            Transition::RecycleDue,
            InstanceState::Destroying,
        ),
        (
            InstanceState::Clearing,
            Transition::ClearFailedCoherent,
            InstanceState::Destroying,
        ),
        (
            InstanceState::Clearing,
            Transition::ClearFailedIncoherent,
            InstanceState::Quarantined,
        ),
        (
            InstanceState::Clearing,
            Transition::ShutdownDrain,
            InstanceState::Destroying,
        ),
        (
            InstanceState::Quarantined,
            Transition::BeginDestroy,
            InstanceState::Destroying,
        ),
        (
            InstanceState::Destroying,
            Transition::Reaped,
            InstanceState::Retired,
        ),
        (
            InstanceState::Destroying,
            Transition::ReapFailed,
            InstanceState::Leaked,
        ),
    ];

    #[test]
    fn the_machine_has_exactly_the_edges_the_table_names() {
        for from in ALL_STATES {
            for transition in ALL_TRANSITIONS {
                let expected = EDGES
                    .iter()
                    .find(|(edge_from, edge_transition, _)| {
                        edge_from == from && edge_transition == transition
                    })
                    .map(|(_, _, to)| *to);
                match (step(*from, *transition), expected) {
                    (Ok(actual), Some(want)) => assert_eq!(
                        actual, want,
                        "{from} + {transition} must land in {want}, not {actual}"
                    ),
                    (Err(refusal), None) => assert_eq!(
                        refusal,
                        IllegalTransition {
                            from: *from,
                            transition: *transition,
                        }
                    ),
                    (Ok(actual), None) => panic!(
                        "{from} + {transition} is an edge to {actual} that the table does not name"
                    ),
                    (Err(_), Some(want)) => {
                        panic!("{from} + {transition} must be an edge to {want}, but was refused")
                    }
                }
            }
        }
    }

    #[test]
    fn every_state_and_every_transition_is_covered_by_the_matrix() {
        // The matrix above is only exhaustive if these lists are. A variant
        // added to either enum without a row here shows up as a state or
        // transition that participates in no edge at all.
        for state in ALL_STATES {
            let participates = EDGES
                .iter()
                .any(|(from, _, to)| from == state || to == state);
            assert!(participates, "{state} participates in no edge");
        }
        for transition in ALL_TRANSITIONS {
            let participates = EDGES.iter().any(|(_, edge, _)| edge == transition);
            assert!(participates, "{transition} is an edge of nothing");
        }
    }

    #[test]
    fn idle_is_entered_only_by_a_proof_carrying_transition() {
        // The load-bearing assertion of the whole module, quantified over every
        // edge rather than over the two the author had in mind.
        let into_idle: Vec<Transition> = EDGES
            .iter()
            .filter(|(_, _, to)| *to == InstanceState::Idle)
            .map(|(_, transition, _)| *transition)
            .collect();
        assert_eq!(
            into_idle.len(),
            2,
            "exactly two edges may enter the idle set, found {into_idle:?}"
        );
        for transition in &into_idle {
            assert!(
                transition.is_proof_carrying(),
                "{transition} enters the idle set without carrying a proof"
            );
            assert!(Transition::PROOF_CARRYING.contains(transition));
        }
        for transition in ALL_TRANSITIONS {
            assert_eq!(
                idle_is_proof_carrying(InstanceState::Idle, *transition),
                transition.is_proof_carrying(),
                "the idle-set implication must agree with the edge set for {transition}"
            );
        }
    }

    #[test]
    fn every_non_delivered_outcome_leaves_the_checked_out_state_destroyed() {
        // The rule that fixes the single worst flaw a pool can have: an
        // instance whose turn did not complete is never returned to service.
        let from_checked_out: Vec<(Transition, InstanceState)> = EDGES
            .iter()
            .filter(|(from, _, _)| *from == InstanceState::CheckedOut)
            .map(|(_, transition, to)| (*transition, *to))
            .collect();
        assert_eq!(from_checked_out.len(), 2, "{from_checked_out:?}");
        for (transition, to) in from_checked_out {
            if transition == Transition::TurnCommitted {
                assert_eq!(to, InstanceState::Delivering);
            } else {
                assert_eq!(
                    to,
                    InstanceState::Quarantined,
                    "{transition} must quarantine, not return to service"
                );
            }
        }
        // ...and quarantine has exactly one exit, and it is teardown.
        let from_quarantine: Vec<InstanceState> = EDGES
            .iter()
            .filter(|(from, _, _)| *from == InstanceState::Quarantined)
            .map(|(_, _, to)| *to)
            .collect();
        assert_eq!(from_quarantine, vec![InstanceState::Destroying]);
    }

    #[test]
    fn a_delivered_turn_answers_before_it_clears() {
        // CheckedOut cannot reach Clearing without passing through Delivering,
        // which is the state whose whole content is "the caller already has the
        // bytes". Asserted as reachability rather than as code ordering.
        assert_eq!(
            step(InstanceState::CheckedOut, Transition::TurnCommitted),
            Ok(InstanceState::Delivering)
        );
        assert!(step(InstanceState::CheckedOut, Transition::ResponseDelivered).is_err());
        assert_eq!(
            step(InstanceState::Delivering, Transition::ResponseDelivered),
            Ok(InstanceState::Clearing)
        );
    }

    #[test]
    fn the_slot_is_released_only_on_a_positive_reaping() {
        assert_eq!(
            step(InstanceState::Destroying, Transition::Reaped),
            Ok(InstanceState::Retired)
        );
        assert_eq!(
            step(InstanceState::Destroying, Transition::ReapFailed),
            Ok(InstanceState::Leaked)
        );
        assert!(
            InstanceState::Destroying.counts_against_pool(),
            "a destroying instance still holds its slot"
        );
        assert!(
            !InstanceState::Retired.counts_against_pool(),
            "a retired instance released its slot"
        );
        assert!(
            !InstanceState::Leaked.counts_against_pool(),
            "a leaked slot is permanently subtracted, not held"
        );
        assert!(
            InstanceState::Leaked.owns_a_root(),
            "a leaked instance keeps its root: a root a live process may be writing to is evidence"
        );
    }

    #[test]
    fn the_terminal_states_absorb() {
        for terminal in [InstanceState::Retired, InstanceState::Leaked] {
            assert!(terminal.is_terminal());
            for transition in ALL_TRANSITIONS {
                assert!(
                    step(terminal, *transition).is_err(),
                    "{terminal} must absorb {transition}"
                );
            }
        }
    }

    #[test]
    fn the_recycle_decision_is_made_in_exactly_one_place() {
        assert_eq!(clear_success_transition(0, 50), Transition::ClearProven);
        assert_eq!(clear_success_transition(49, 50), Transition::ClearProven);
        assert_eq!(clear_success_transition(50, 50), Transition::RecycleDue);
        assert_eq!(clear_success_transition(51, 50), Transition::RecycleDue);
        // A cap of one is `run_once` with extra steps, and the machine says so.
        assert_eq!(clear_success_transition(1, 1), Transition::RecycleDue);
    }

    #[test]
    fn the_only_reachable_states_are_the_ones_the_table_reaches() {
        // A breadth-first walk from INITIAL, so a state that is named but
        // unreachable -- or reachable but never named -- shows up here rather
        // than as an untested edge somebody assumed was dead.
        let mut reached = vec![INITIAL];
        let mut frontier = vec![INITIAL];
        while let Some(state) = frontier.pop() {
            for transition in ALL_TRANSITIONS {
                if let Ok(next) = step(state, *transition)
                    && !reached.contains(&next)
                {
                    reached.push(next);
                    frontier.push(next);
                }
            }
        }
        reached.sort_unstable();
        let mut expected = ALL_STATES.to_vec();
        expected.sort_unstable();
        assert_eq!(reached, expected, "every named state must be reachable");
    }

    #[test]
    fn a_turn_cannot_be_submitted_before_a_proof_or_after_a_checkout() {
        // Only Idle admits CheckOut, which is what makes "the idle set is the
        // proof" mean something operationally rather than decoratively.
        for state in ALL_STATES {
            let admitted = step(*state, Transition::CheckOut).is_ok();
            assert_eq!(
                admitted,
                *state == InstanceState::Idle,
                "{state} must not admit a checkout"
            );
        }
    }

    /// A state comes back on its own **iff** it can leave itself with no
    /// caller's help, and that is asked of the state machine rather than of a
    /// list.
    ///
    /// The predicate decides whether a refused caller waits, so the danger is
    /// exactly the bug class this module keeps meeting: a name that promises
    /// more than the match tests. Here the claim is checked against `step`
    /// itself -- for every state, is there a transition out of it that the pool
    /// applies with no caller in the picture? -- so widening the match to a
    /// state a caller is holding fails here rather than in production, where it
    /// would show up as a caller waiting out somebody else's turn.
    #[test]
    fn a_bucket_comes_back_on_its_own_exactly_when_nobody_has_to_finish_a_turn() {
        // The transitions the POOL applies without a caller: the outcomes of
        // `/clear`, and every step of a teardown already under way. Named as
        // transitions rather than as destination states, because the question is
        // "who has to act", and a transition is the only thing that says so.
        let unattended = [
            Transition::ClearProven,
            Transition::ClearFailedCoherent,
            Transition::ClearFailedIncoherent,
            Transition::RecycleDue,
            Transition::BeginDestroy,
            Transition::Reaped,
            Transition::ReapFailed,
        ];
        for state in ALL_STATES {
            let leaves_unattended = unattended
                .iter()
                .any(|transition| step(*state, *transition).is_ok());
            let claimed = state.census_bucket().comes_back_on_its_own();
            assert_eq!(
                claimed,
                leaves_unattended,
                "{state} is in the {:?} bucket, which claims comes_back_on_its_own = {claimed}, \
                 while the machine says it {} leave that state unattended",
                state.census_bucket(),
                if leaves_unattended { "CAN" } else { "cannot" },
            );
        }
        // Anti-vacuity in both directions: an all-true or all-false predicate
        // would satisfy the loop above only if the machine agreed, but a
        // partition with an empty side would still mean the wait is either
        // never taken or always taken.
        let (waited, refused): (Vec<_>, Vec<_>) = ALL_STATES
            .iter()
            .partition(|state| state.census_bucket().comes_back_on_its_own());
        assert!(!waited.is_empty(), "no state is ever waited for");
        assert!(!refused.is_empty(), "every state is waited for");
        assert!(
            !waited.contains(&&InstanceState::CheckedOut)
                && !waited.contains(&&InstanceState::Delivering),
            "a caller is waiting on {waited:?}, so waiting there is a queue behind a model"
        );
    }
    /// `is_terminal` and `owns_a_root` answer per state, and neither is a
    /// constant.
    ///
    /// SURVIVING MUTANTS CLOSED: `InstanceState::owns_a_root -> true`
    /// (`machine.rs:83`) and `InstanceState::is_terminal -> true`
    /// (`machine.rs:89`). Both are `const fn` predicates over the state enum,
    /// and every existing case that reached either did so through a pool whose
    /// states happened to answer `true` -- so a predicate that answered `true`
    /// for EVERY state, including the ones it exists to exclude, was
    /// indistinguishable. `owns_a_root -> true` makes a `Reserved` slot -- which
    /// by construction has no paths yet -- look like one holding a filesystem
    /// root to erase, and `is_terminal -> true` makes every live state look
    /// absorbing.
    ///
    /// **`is_terminal` is DERIVED from the edge table rather than restated.**
    /// Its doc says "absorbing -- no transition leaves it", and that is a
    /// property `EDGES` already answers exactly, so the assertion is the doc
    /// rather than a second copy of the answer. A state that grows an outgoing
    /// edge tomorrow and stays `is_terminal` fails here without anyone
    /// remembering to come back.
    #[test]
    fn terminal_and_root_owning_states_are_the_ones_the_machine_says_they_are() {
        for state in ALL_STATES {
            let leaves = EDGES.iter().any(|(from, _, _)| from == state);
            assert_eq!(
                state.is_terminal(),
                !leaves,
                "{state} is_terminal={} but the edge table gives it {} outgoing edge(s)",
                state.is_terminal(),
                EDGES.iter().filter(|(from, _, _)| from == state).count()
            );
        }
        // Both answers must actually occur, or the assertion above is satisfied
        // by a constant just as well as by the predicate.
        assert!(
            ALL_STATES.iter().any(|state| state.is_terminal()),
            "no state is terminal; the derivation is broken"
        );
        assert!(
            ALL_STATES.iter().any(|state| !state.is_terminal()),
            "every state is terminal, so a constant `true` would pass"
        );

        // `owns_a_root` is not derivable from the edge table -- it is a claim
        // about the filesystem, not about the machine -- so it is a table, and
        // the table is required to cover every state exactly once.
        const OWNS_A_ROOT: &[(InstanceState, bool)] = &[
            // A reservation is a counted slot and nothing else: `mint_roots`
            // has not run, so there is no directory to erase.
            (InstanceState::Reserved, false),
            (InstanceState::Warming, true),
            (InstanceState::Idle, true),
            (InstanceState::CheckedOut, true),
            (InstanceState::Delivering, true),
            (InstanceState::Clearing, true),
            (InstanceState::Quarantined, true),
            (InstanceState::Destroying, true),
            // Leaked still owns the tree nobody could erase; Retired is the one
            // terminal state whose tree is gone.
            (InstanceState::Leaked, true),
            (InstanceState::Retired, false),
        ];
        let tabled: Vec<InstanceState> = OWNS_A_ROOT.iter().map(|(state, _)| *state).collect();
        assert_eq!(
            tabled,
            ALL_STATES.to_vec(),
            "the root-ownership table must name every state, in the same order, \
             so a state added tomorrow cannot be silently unclassified"
        );
        for (state, owns) in OWNS_A_ROOT {
            assert_eq!(
                state.owns_a_root(),
                *owns,
                "{state} owns_a_root must be {owns}"
            );
        }
        assert!(
            OWNS_A_ROOT.iter().any(|(_, owns)| !owns),
            "every state owns a root, so a constant `true` would pass"
        );
    }

    /// No edge of the machine both starts and ends at `Idle`.
    ///
    /// **CLOSES NO SURVIVING MUTANT, and says so rather than claiming one.** It
    /// is the PREMISE that makes `mod.rs:1308 && -> ||` an equivalent mutant
    /// rather than a gap. That line is
    /// `instance.state == Idle && next != Idle`, and it decides whether a
    /// transition unpublishes the instance from its class's idle set. Under
    /// `||` the extra cases are (a) `Idle -> Idle`, which this test proves does
    /// not exist, and (b) any transition between two non-`Idle` states, where
    /// the slot is not in an idle set to begin with -- so `remove_from_idle` is
    /// a no-op and the two spellings agree.
    ///
    /// Written as a property so the equivalence can be re-checked instead of
    /// remembered: an `Idle -> Idle` edge added tomorrow makes that mutant REAL
    /// -- it would unpublish an instance that stayed idle, which is
    /// `PoolInvariantViolation::IdleInstanceNotPublished`, an instance holding a
    /// slot no caller can ever reach -- and this test is what says so.
    #[test]
    fn the_machine_has_no_edge_from_idle_back_to_idle() {
        let self_edges: Vec<Transition> = ALL_TRANSITIONS
            .iter()
            .copied()
            .filter(|transition| step(InstanceState::Idle, *transition) == Ok(InstanceState::Idle))
            .collect();
        assert!(
            self_edges.is_empty(),
            "{self_edges:?} return Idle to Idle, which makes both clauses of the \
             unpublish condition at mod.rs:1308 load-bearing"
        );
        // The specification table says the same thing, so this does not rest on
        // one of the machine's two independent statements.
        assert!(
            !EDGES
                .iter()
                .any(|(from, _, to)| *from == InstanceState::Idle && *to == InstanceState::Idle),
            "the edge table names an Idle -> Idle edge that `step` does not"
        );
        // ...and `Idle` is not a state with no outgoing edges at all, which
        // would satisfy the assertions above without saying anything.
        assert!(
            ALL_TRANSITIONS
                .iter()
                .any(|transition| step(InstanceState::Idle, *transition).is_ok()),
            "Idle has no outgoing edge at all, so the property above is vacuous"
        );
    }
}
