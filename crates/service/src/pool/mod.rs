//! Path B: the token engine's instance pool.
//!
//! The runtime API is, at its most verbose, `(model, effort, input tokens) ->
//! output tokens` and nothing else. No session handle. No caller-supplied
//! configuration. **The caller names no resource** -- that is the product, not
//! an aesthetic. Nine leaks have been found in this codebase and every one of
//! them was reachable only because a caller could name a resource pmux also
//! used. A caller who cannot name a resource cannot alias one.
//!
//! # What this module owns, and what it refuses to own
//!
//! It owns admission, the class key, slots, epochs, the filesystem roots and
//! the state machine. It owns no process: everything that touches a child, a
//! TUI, a transcript or a session registry is behind [`host::InstanceHost`], so
//! the whole machine runs deterministically with no Claude on the box.
//!
//! # The guarantees, stated once
//!
//! - **pmux mints every resource.** Config root, cwd and system prompt come
//!   from daemon configuration and a slot identity, never from a request.
//! - **Membership in the idle set IS the emptiness proof.** See [`machine`].
//! - **Every transition preserves a stated invariant**, checked after the fact
//!   by [`instance::Instance::check_invariants`] and [`Pool::check_invariants`].
//! - **Every failure ends in a refusal or a correct answer, never a wrong
//!   answer.** An instance that cannot be proven clean is destroyed, not reused.
//! - **Refuse and name the budget at the cap.** No queue.

pub mod class;
pub mod config;
pub mod evidence;
pub mod host;
pub mod instance;
pub mod machine;
pub mod refusal;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use pseudomux_protocol::v1::{ErrorBody, ErrorCode, RunStatelessRequest, StatelessResult};
use serde_json::json;
use tokio::sync::Mutex;

use crate::driver_io::validate_prompt;
use crate::private_dir::{create_private_dir_all, seal_owner_only};
use crate::v1::{Clock, DriverFailure};

pub use class::{InstanceClass, ModelEffortRefusal, resolve_model_effort, resolve_pool_class};
pub use config::{ConfigField, PoolConfig, PoolSettings, WarmClassSetting};
pub use host::{
    ClearFailure, Destroyed, HostFailure, HostTurn, InstanceHandle, InstanceHost, MintSpec,
    Spawner, TrackedSpawner,
};
pub use instance::{Epoch, Instance, SlotId, SlotPaths};
pub use machine::{InstanceState, Transition};
pub use refusal::path_b_not_enabled;

/// What [`Pool::run_sticky`] hands back. `cell` is a slot/epoch token, never a
/// pmux `SessionId` -- that id must not appear on any client socket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StickyTurn {
    pub result: StatelessResult,
    pub cell: String,
}

/// One conversation currently holding a pool instance.
///
/// Published on `pmux doctor`'s pool layer so an operator can map
/// `x-pmux-conversation` to `x-pmux-cell` without a new wire method.
/// The conversation id is the harness session id (or an implicit hash
/// when the operator opted in), never a Claude `SessionId`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationLease {
    pub conversation_id: String,
    pub cell: String,
    pub state: String,
}

/// What one admission decision produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Admission {
    /// Rule 1: an idle instance of the requested class, below the recycle cap.
    Warm(SlotId),
    /// Rule 2: capacity was free, so a slot was reserved for a fresh mint.
    Reserved(SlotId),
    /// Rule 3: no capacity but an idle instance of another class exists, so it
    /// is destroyed and its slot handed directly to this caller. "Exhausted"
    /// means *no instance is free*, not *no instance of your shape is free* --
    /// refusing while holding fifteen idle instances of another class is both a
    /// bug an operator will correctly report and permanent starvation for any
    /// class whose slots are all held by long-idle instances of other classes.
    ColdSwap(SlotId),
}

/// One admission decision and what it cost to reach.
///
/// The wait is carried out of admission rather than recomputed downstream, so
/// the reclaim refusal on the cold-swap path names the same number the
/// exhaustion refusal would have.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Admitted {
    admission: Admission,
    /// Milliseconds spent inside [`Pool::admit`] waiting for a slot to come
    /// back. Zero on every uncontended call, which is every call that finds an
    /// idle instance or a free slot on its first read.
    waited_ms: u64,
}

/// One refused admission attempt, and whether another read could go differently.
///
/// `coming_back` is not a hint. It is [`refusal::PoolPressure::coming_back`] --
/// the number of slots held by an instance that leaves its state with no
/// caller's help -- asked at the same instant, under the same lock, and off the
/// same `PoolPressure` as the census the refusal prints. Two independent
/// answers to "is anything on its way back" is how a refusal comes to say one
/// thing while the loop above it believes another.
struct AdmissionRefusal {
    body: ErrorBody,
    coming_back: u32,
}

/// The pool's mutable state. Never held across an await.
struct PoolState {
    instances: BTreeMap<SlotId, Instance>,
    /// Per class, because fungibility is per class. A single queue hands an
    /// opus/max call to a haiku/low process.
    idle: BTreeMap<InstanceClass, BTreeSet<SlotId>>,
    /// The next epoch to mint into each slot, so a surviving orphan can never
    /// share a directory with a new instance.
    next_epoch: BTreeMap<SlotId, Epoch>,
    /// Slots permanently subtracted from `pool_size` because a teardown could
    /// not prove the process reaped. An undeletable root is permanent capacity
    /// loss and a page, never a log-and-continue.
    leaked_slots: BTreeSet<SlotId>,
    /// Classes with a re-warm already in flight, so a burst of checkouts cannot
    /// queue N background mints for one empty idle set.
    rewarming: BTreeSet<InstanceClass>,
    /// Conversation ids whose first sticky turn has been admitted but not yet
    /// bound to a slot. Stops two concurrent primes minting two cells.
    pending_leases: BTreeSet<String>,
    /// Conversations the Messages book has pinned across the lock-drop →
    /// `resume_lease` window. The leased TTL sweep skips these; `resume_lease`
    /// does not refuse them (unlike `pending_leases`).
    protected_leases: BTreeSet<String>,
    halted: Option<&'static str>,
    shutting_down: bool,
}

impl PoolState {
    fn live(&self) -> u32 {
        u32::try_from(self.instances.len()).unwrap_or(u32::MAX)
    }

    /// Every live instance, binned by the bucket its own state names.
    ///
    /// ONE pass and one classification. This replaced four independent filter
    /// closures, each naming its own subset of `InstanceState` -- a shape in
    /// which a state named by none of them holds a slot that `live` counts and
    /// no clause of a refusal describes, and in which `Clearing` was named by
    /// the closure called `in_flight` and printed as "serving a turn".
    fn buckets(&self) -> refusal::BucketCounts {
        let mut counts = refusal::BucketCounts::default();
        for instance in self.instances.values() {
            counts.record(instance.state);
        }
        counts
    }

    fn idle_count(&self) -> u32 {
        u32::try_from(self.idle.values().map(BTreeSet::len).sum::<usize>()).unwrap_or(u32::MAX)
    }

    fn capacity(&self, pool_size: u32) -> u32 {
        pool_size.saturating_sub(u32::try_from(self.leaked_slots.len()).unwrap_or(u32::MAX))
    }

    /// Everything a capacity refusal has to say, derived here once.
    ///
    /// Both refusal sites read this rather than assembling their own tuple. The
    /// two that assembled their own each passed `pool_size` where the budget
    /// belonged, so after a leak both overstated the budget permanently, and
    /// neither could see the teardown states at all.
    fn pressure(&self, pool_size: u32) -> refusal::PoolPressure {
        refusal::PoolPressure {
            configured_instances: pool_size,
            usable_instances: self.capacity(pool_size),
            counts: self.buckets(),
            leaked: u32::try_from(self.leaked_slots.len()).unwrap_or(u32::MAX),
        }
    }

    fn free_slot(&self, pool_size: u32) -> Option<SlotId> {
        (0..pool_size)
            .find(|slot| !self.instances.contains_key(slot) && !self.leaked_slots.contains(slot))
    }

    fn take_epoch(&mut self, slot: SlotId) -> Epoch {
        let epoch = self.next_epoch.entry(slot).or_insert(0);
        let current = *epoch;
        *epoch = current.saturating_add(1);
        current
    }

    fn remove_from_idle(&mut self, class: InstanceClass, slot: SlotId) {
        if let Some(members) = self.idle.get_mut(&class) {
            members.remove(&slot);
            if members.is_empty() {
                self.idle.remove(&class);
            }
        }
    }

    /// The least-recently-idle member of a class, which is the eviction order
    /// the TTL sweep and the cold swap both use.
    fn lru_of(&self, class: InstanceClass) -> Option<SlotId> {
        self.idle.get(&class)?.iter().copied().min_by_key(|slot| {
            self.instances
                .get(slot)
                .map_or(u64::MAX, |instance| instance.idle_since_ms)
        })
    }
}

/// A pool-level invariant that did not hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PoolInvariantViolation {
    /// The idle set names a slot that is not in the `Idle` state.
    IdleSetHoldsNonIdle { slot: SlotId, state: InstanceState },
    /// An `Idle` instance is missing from the idle set for its class, so it is
    /// holding a slot nobody can reach.
    IdleInstanceNotPublished { slot: SlotId },
    /// The idle set files an instance under a class that is not its own.
    IdleSetClassMismatch { slot: SlotId },
    /// More live instances than the configured budget admits.
    OverCapacity { live: u32, capacity: u32 },
    /// A slot is both live and leaked.
    LeakedSlotStillLive { slot: SlotId },
    /// A per-instance invariant failed.
    Instance(instance::InvariantViolation),
}

impl std::fmt::Display for PoolInvariantViolation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IdleSetHoldsNonIdle { slot, state } => write!(
                formatter,
                "the idle set holds slot {slot}, which is {state}"
            ),
            Self::IdleInstanceNotPublished { slot } => write!(
                formatter,
                "slot {slot} is idle but is not published in its class's idle set"
            ),
            Self::IdleSetClassMismatch { slot } => {
                write!(formatter, "slot {slot} is filed under another class")
            }
            Self::OverCapacity { live, capacity } => {
                write!(
                    formatter,
                    "{live} live instances against a budget of {capacity}"
                )
            }
            Self::LeakedSlotStillLive { slot } => {
                write!(formatter, "slot {slot} is both leaked and live")
            }
            Self::Instance(violation) => write!(formatter, "{violation}"),
        }
    }
}

/// The stateless instance pool.
pub struct Pool {
    config: PoolConfig,
    host: Arc<dyn InstanceHost>,
    clock: Arc<dyn Clock>,
    spawner: Arc<dyn Spawner>,
    state: Mutex<PoolState>,
}

impl Pool {
    /// Build a pool over a validated configuration.
    ///
    /// The configuration is already validated -- [`PoolConfig`] is only
    /// constructible through [`PoolSettings::validate`] -- so nothing here can
    /// refuse, and no runtime path has to re-check a bound.
    #[must_use]
    pub fn new(
        config: PoolConfig,
        host: Arc<dyn InstanceHost>,
        clock: Arc<dyn Clock>,
        spawner: Arc<dyn Spawner>,
    ) -> Arc<Self> {
        Arc::new(Self {
            config,
            host,
            clock,
            spawner,
            state: Mutex::new(PoolState {
                instances: BTreeMap::new(),
                idle: BTreeMap::new(),
                next_epoch: BTreeMap::new(),
                leaked_slots: BTreeSet::new(),
                rewarming: BTreeSet::new(),
                pending_leases: BTreeSet::new(),
                protected_leases: BTreeSet::new(),
                halted: None,
                shutting_down: false,
            }),
        })
    }

    #[must_use]
    pub fn config(&self) -> &PoolConfig {
        &self.config
    }

    /// Seal the pool parent, then mint the operator-declared warm set.
    ///
    /// The primary warming mechanism: an operator who knows which shapes their
    /// callers use declares them, and pays the launch cost once at boot rather
    /// than on a caller's first request. A class that is not declared is still
    /// served -- it pays a mint on its first call, which is honest, because it
    /// is the cost of a machine that does not exist yet.
    ///
    /// # Errors
    ///
    /// Returns the parent-directory refusal, or the first mint failure. Both
    /// are operator errors worth failing startup over, not degraded modes.
    pub async fn start(self: &Arc<Self>) -> Result<(), ErrorBody> {
        require_private_parent(&self.config.parent_dir, ConfigField::ParentDir)?;
        if let Some(retain_dir) = &self.config.retain_dir {
            require_private_parent(retain_dir, ConfigField::RetainDir)?;
        }
        for warm in self.config.warm_set.clone() {
            for _ in 0..warm.count {
                let slot = {
                    let mut state = self.state.lock().await;
                    let Some(slot) = state.free_slot(self.config.pool_size) else {
                        break;
                    };
                    self.reserve_locked(&mut state, slot, warm.class);
                    slot
                };
                self.mint(slot).await?;
                self.publish_idle(slot).await;
            }
        }
        Ok(())
    }

    /// One stateless turn: `(model, effort, prompt) -> tokens`.
    ///
    /// # Errors
    ///
    /// Every refusal is an existing [`ErrorCode`]; see [`refusal`] for the
    /// complete table and for the variant this design deliberately does not add.
    pub async fn run(
        self: &Arc<Self>,
        request: RunStatelessRequest,
    ) -> Result<StatelessResult, ErrorBody> {
        // Steps 1-3 happen before any instance is touched, so a bad request can
        // never evict an idle instance.
        let prompt = validate_prompt(&request.prompt).map_err(DriverFailure::into_protocol)?;
        // The pool computes its class key by calling the SAME function that
        // renders argv. The "early check" and the "real check" are literally
        // one function with one set of inputs, so they cannot drift.
        let (class, resolved) = resolve_pool_class(&request.model, request.effort)
            .map_err(ModelEffortRefusal::into_error_body)?;

        // Resolved BEFORE admission, and used by both. It is one absolute
        // instant, so admission's bounded wait spends the caller's own budget
        // rather than a second budget the caller never asked for -- a caller
        // that sent `deadline_unix_ms` 100 ms out must be refused, not waited
        // with for the whole admission ceiling and then given a turn against an
        // already-dead deadline. It also stops the pool from granting
        // `turn_timeout_ms`
        // measured from AFTER a mint, which is a budget starting later than the
        // caller's clock says it should.
        let deadline = self
            .config
            .effective_deadline_ms(self.clock.now_ms(), request.deadline_unix_ms);

        let Admitted {
            admission,
            waited_ms,
        } = self.admit(class, deadline).await?;
        let slot = match admission {
            Admission::Warm(slot) => slot,
            Admission::Reserved(slot) => {
                self.mint(slot).await?;
                self.publish_idle_and_check_out(slot).await?
            }
            Admission::ColdSwap(slot) => {
                // The victim's teardown completes before the slot is re-minted:
                // the slot is released only after its root is gone, so a
                // replacement can never exist while a prior caller's
                // `history.jsonl` is still on disk.
                self.destroy(slot).await;
                self.reclaim(slot, class, waited_ms).await?;
                self.mint(slot).await?;
                self.publish_idle_and_check_out(slot).await?
            }
        };

        let handle = self
            .handle_of(slot)
            .await
            .ok_or_else(|| internal("a checked-out instance lost its process handle"))?;

        match self.host.run_turn(&handle, prompt, deadline).await {
            Ok(turn) => self.commit(slot, &class, &resolved, turn).await,
            Err(failure) => {
                // ANY outcome other than a delivered, transcript-proven turn.
                // A timeout means pmux does not know whether the model is still
                // generating into the bound transcript, so returning this
                // instance to service would let the next caller's prompt
                // interleave with this caller's in-flight generation.
                self.quarantine_and_destroy(slot).await;
                Err(failure.error)
            }
        }
    }

    /// One sticky turn: pin a conversation to one instance and do not `/clear`.
    ///
    /// `resume` is whether this conversation already holds an instance. A
    /// continuation that lost its cell (`SessionNotFound`) is the caller's cue
    /// to send the full primer rather than a suffix.
    ///
    /// Recycle is lease-end only: a resume increments `turns_started` and does
    /// not refuse at the cap. Remint happens when the lease ends (`/clear` or
    /// idle TTL) and `turns_started >= recycle_turns`.
    ///
    /// # Errors
    ///
    /// The same refusals as [`Self::run`], plus [`ErrorCode::SessionNotFound`]
    /// when `resume` names a conversation this pool is not holding, and
    /// [`ErrorCode::SessionBusy`] when that conversation already has a turn in
    /// flight.
    pub async fn run_sticky(
        self: &Arc<Self>,
        conversation_id: &str,
        request: RunStatelessRequest,
        resume: bool,
    ) -> Result<StickyTurn, ErrorBody> {
        if conversation_id.trim().is_empty() {
            return Err(ErrorBody::new(
                ErrorCode::InvalidConfig,
                "conversation id must not be empty",
            ));
        }
        let prompt = validate_prompt(&request.prompt).map_err(DriverFailure::into_protocol)?;
        let (class, resolved) = resolve_pool_class(&request.model, request.effort)
            .map_err(ModelEffortRefusal::into_error_body)?;
        let deadline = self
            .config
            .effective_deadline_ms(self.clock.now_ms(), request.deadline_unix_ms);

        let slot = if resume {
            self.resume_lease(conversation_id, class).await?
        } else {
            self.claim_new_lease(conversation_id, class, deadline)
                .await?
        };

        let handle = match self.handle_of(slot).await {
            Some(handle) => handle,
            None => {
                self.quarantine_and_destroy(slot).await;
                return Err(internal("a checked-out instance lost its process handle"));
            }
        };

        match self.host.run_turn(&handle, prompt, deadline).await {
            Ok(turn) => {
                self.commit_sticky(slot, conversation_id, &class, &resolved, turn)
                    .await
            }
            Err(failure) => {
                self.quarantine_and_destroy(slot).await;
                Err(failure.error)
            }
        }
    }

    /// End a conversation: `/clear` the instance back to Idle, or recycle it.
    ///
    /// Missing conversations succeed. A turn in flight, or a first prime that
    /// has reserved the id but not yet bound a slot, is
    /// [`ErrorCode::SessionBusy`]. Lookup and `ReleaseLease` share one lock
    /// so a recycled slot rebound to another conversation cannot be cleared
    /// by a stale releaser.
    pub async fn release_conversation(
        self: &Arc<Self>,
        conversation_id: &str,
    ) -> Result<(), ErrorBody> {
        let slot = {
            let mut state = self.state.lock().await;
            if state.pending_leases.contains(conversation_id) {
                return Err(ErrorBody::new(
                    ErrorCode::SessionBusy,
                    "this conversation already has a turn in flight; nothing is queued",
                )
                .retryable(true));
            }
            let Some((slot, current)) = state.instances.iter().find_map(|(slot, instance)| {
                (instance.conversation_id.as_deref() == Some(conversation_id))
                    .then_some((*slot, instance.state))
            }) else {
                return Ok(());
            };
            match current {
                InstanceState::CheckedOut | InstanceState::Delivering => {
                    return Err(ErrorBody::new(
                        ErrorCode::SessionBusy,
                        "this conversation already has a turn in flight; nothing is queued",
                    )
                    .retryable(true));
                }
                InstanceState::Leased => {}
                _ => return Ok(()),
            }
            self.transition_locked(&mut state, slot, Transition::ReleaseLease)
                .map_err(|violation| internal(&violation.to_string()))?;
            slot
        };
        self.spawn_clear(slot);
        Ok(())
    }

    async fn claim_new_lease(
        self: &Arc<Self>,
        conversation_id: &str,
        class: InstanceClass,
        deadline: u64,
    ) -> Result<SlotId, ErrorBody> {
        {
            let mut state = self.state.lock().await;
            if state.pending_leases.contains(conversation_id)
                || state.instances.values().any(|instance| {
                    instance.conversation_id.as_deref() == Some(conversation_id)
                        && matches!(
                            instance.state,
                            InstanceState::Leased
                                | InstanceState::CheckedOut
                                | InstanceState::Delivering
                        )
                })
            {
                return Err(ErrorBody::new(
                    ErrorCode::SessionBusy,
                    format!("conversation {conversation_id} already holds a pool instance"),
                )
                .retryable(true));
            }
            state.pending_leases.insert(conversation_id.to_owned());
        }
        let admitted = self.admit(class, deadline).await;
        let Admitted {
            admission,
            waited_ms,
        } = match admitted {
            Ok(admitted) => admitted,
            Err(error) => {
                self.state
                    .lock()
                    .await
                    .pending_leases
                    .remove(conversation_id);
                return Err(error);
            }
        };
        let slot = match admission {
            Admission::Warm(slot) => slot,
            Admission::Reserved(slot) => {
                if let Err(error) = self.mint(slot).await {
                    self.state
                        .lock()
                        .await
                        .pending_leases
                        .remove(conversation_id);
                    return Err(error);
                }
                match self.publish_idle_and_check_out(slot).await {
                    Ok(slot) => slot,
                    Err(error) => {
                        self.state
                            .lock()
                            .await
                            .pending_leases
                            .remove(conversation_id);
                        return Err(error);
                    }
                }
            }
            Admission::ColdSwap(slot) => {
                self.destroy(slot).await;
                if let Err(error) = self.reclaim(slot, class, waited_ms).await {
                    self.state
                        .lock()
                        .await
                        .pending_leases
                        .remove(conversation_id);
                    return Err(error);
                }
                if let Err(error) = self.mint(slot).await {
                    self.state
                        .lock()
                        .await
                        .pending_leases
                        .remove(conversation_id);
                    return Err(error);
                }
                match self.publish_idle_and_check_out(slot).await {
                    Ok(slot) => slot,
                    Err(error) => {
                        self.state
                            .lock()
                            .await
                            .pending_leases
                            .remove(conversation_id);
                        return Err(error);
                    }
                }
            }
        };
        {
            let mut state = self.state.lock().await;
            state.pending_leases.remove(conversation_id);
            if let Some(instance) = state.instances.get_mut(&slot) {
                instance.conversation_id = Some(conversation_id.to_owned());
            }
        }
        Ok(slot)
    }

    async fn resume_lease(
        &self,
        conversation_id: &str,
        class: InstanceClass,
    ) -> Result<SlotId, ErrorBody> {
        let mut state = self.state.lock().await;
        if state.pending_leases.contains(conversation_id) {
            return Err(ErrorBody::new(
                ErrorCode::SessionBusy,
                "this conversation already has a turn in flight; nothing is queued",
            )
            .retryable(true));
        }
        let Some(slot) = state.instances.iter().find_map(|(slot, instance)| {
            (instance.conversation_id.as_deref() == Some(conversation_id)
                && matches!(
                    instance.state,
                    InstanceState::Leased | InstanceState::CheckedOut | InstanceState::Delivering
                ))
            .then_some(*slot)
        }) else {
            return Err(ErrorBody::new(
                ErrorCode::SessionNotFound,
                "this conversation has no leased instance; send the full primer",
            ));
        };
        let instance = &state.instances[&slot];
        match instance.state {
            InstanceState::CheckedOut | InstanceState::Delivering => {
                return Err(ErrorBody::new(
                    ErrorCode::SessionBusy,
                    "this conversation already has a turn in flight; nothing is queued",
                )
                .retryable(true));
            }
            InstanceState::Leased => {}
            other => {
                return Err(internal(&format!(
                    "conversation {conversation_id} is bound to slot {slot} in state {other}"
                )));
            }
        }
        if instance.class != class {
            return Err(ErrorBody::new(
                ErrorCode::InvalidConfig,
                format!(
                    "conversation {conversation_id} is leased on {}, not {class}; release it first",
                    instance.class
                ),
            ));
        }
        self.transition_locked(&mut state, slot, Transition::ResumeLease)
            .map_err(|violation| internal(&violation.to_string()))?;
        Ok(slot)
    }

    /// Refresh `idle_since_ms` on a `Leased` conversation. A Messages replay
    /// is activity; the pool clock is the TTL owner.
    ///
    /// Returns whether a leased instance for this id was found and touched.
    pub async fn touch_conversation(&self, conversation_id: &str) -> bool {
        let mut state = self.state.lock().await;
        let now = self.clock.now_ms();
        let Some(instance) = state.instances.values_mut().find(|instance| {
            instance.conversation_id.as_deref() == Some(conversation_id)
                && instance.state == InstanceState::Leased
        }) else {
            return false;
        };
        instance.idle_since_ms = now;
        true
    }

    /// Pin a leased conversation so the idle TTL sweep cannot `/clear` it.
    ///
    /// Used by the Messages book after it marks a continue `in_flight` and
    /// before `resume_lease` takes the cell out of `Leased`.
    pub async fn protect_conversation(&self, conversation_id: &str) {
        if conversation_id.trim().is_empty() {
            return;
        }
        self.state
            .lock()
            .await
            .protected_leases
            .insert(conversation_id.to_owned());
    }

    /// Drop a pin taken by [`Self::protect_conversation`]. Missing is success.
    pub async fn unprotect_conversation(&self, conversation_id: &str) {
        self.state
            .lock()
            .await
            .protected_leases
            .remove(conversation_id);
    }

    /// Destroy every instance idle past the TTL, down to each class's declared
    /// warm floor.
    ///
    /// The TTL and the cold swap are a pair, and neither alone is enough: the
    /// TTL drains a pool full of stale wrong-class instances so a cold swap only
    /// ever fires inside the TTL window, and the cold swap bounds the worst case
    /// inside it.
    ///
    /// The warm floor is honoured here and not by the cold swap, deliberately:
    /// declared capacity exists precisely so it is there when a caller arrives
    /// cold, so the clock must not take it -- but an actual demand for an
    /// undeclared class beats a declared-but-idle instance, because refusing a
    /// live caller in order to hold a speculative instance is the starvation
    /// bug rule 3 exists to prevent.
    pub async fn sweep_idle(self: &Arc<Self>) {
        let now = self.clock.now_ms();
        let victims = {
            let mut state = self.state.lock().await;
            let mut victims = Vec::new();
            let classes: Vec<InstanceClass> = state.idle.keys().copied().collect();
            for class in classes {
                let floor = self.config.warm_floor(class);
                loop {
                    let held = state.idle.get(&class).map_or(0, |members| {
                        u32::try_from(members.len()).unwrap_or(u32::MAX)
                    });
                    if held <= floor {
                        break;
                    }
                    let Some(slot) = state.lru_of(class) else {
                        break;
                    };
                    let expired = state.instances.get(&slot).is_some_and(|instance| {
                        now.saturating_sub(instance.idle_since_ms)
                            >= self.config.instance_idle_ttl_ms
                    });
                    if !expired {
                        break;
                    }
                    if self
                        .transition_locked(&mut state, slot, Transition::IdleExpired)
                        .is_ok()
                    {
                        victims.push(slot);
                    } else {
                        break;
                    }
                }
            }
            victims
        };
        for slot in victims {
            self.destroy(slot).await;
        }

        // Leased conversations use the same TTL as idle instances. Expiry is
        // `/clear` back to Idle, or recycle at the turn cap (with a remint
        // when that would drop the class below its warm floor). A book pin
        // (`protected_leases`) skips the sweep so an in-flight continue is
        // not `/clear`ed out from under `resume_lease`.
        let expired_leases = {
            let mut state = self.state.lock().await;
            let mut expired = Vec::new();
            let slots: Vec<SlotId> = state.instances.keys().copied().collect();
            for slot in slots {
                let (expired_lease, conversation_id) = match state.instances.get(&slot) {
                    Some(instance)
                        if instance.state == InstanceState::Leased
                            && now.saturating_sub(instance.idle_since_ms)
                                >= self.config.instance_idle_ttl_ms =>
                    {
                        (true, instance.conversation_id.clone())
                    }
                    _ => (false, None),
                };
                let due = expired_lease
                    && !conversation_id
                        .as_ref()
                        .is_some_and(|id| state.protected_leases.contains(id));
                if due
                    && self
                        .transition_locked(&mut state, slot, Transition::ReleaseLease)
                        .is_ok()
                {
                    expired.push(slot);
                }
            }
            expired
        };
        for slot in expired_leases {
            self.spawn_clear(slot);
        }
    }

    /// Drain every instance and erase every root.
    ///
    /// The one exception is an instance whose teardown never confirmed reaping:
    /// its root is left on disk and reported, because a root a live process may
    /// still be writing to is evidence, not garbage.
    ///
    /// Which instances are drained is decided by [`machine::shutdown_action`],
    /// per state and with no wildcard, and only two states are kept: the two
    /// where a caller is still waiting for an answer. Everything else is torn
    /// down here, including `Clearing` -- an instance that has already answered
    /// its caller and is running housekeeping. The previous `_ => continue`
    /// skipped it, and since the pool answers before it clears, that meant a
    /// daemon stopped after serving traffic left the roots of every instance it
    /// had just used on disk, transcripts included, with nothing reported.
    pub async fn shutdown(self: &Arc<Self>) {
        let victims = {
            let mut state = self.state.lock().await;
            state.shutting_down = true;
            let mut victims = Vec::new();
            let slots: Vec<SlotId> = state.instances.keys().copied().collect();
            for slot in slots {
                let current = state.instances[&slot].state;
                match machine::shutdown_action(current) {
                    machine::ShutdownAction::Drain(transition) => {
                        if self.transition_locked(&mut state, slot, transition).is_ok() {
                            victims.push(slot);
                        }
                    }
                    // Already on its way out on another task. `destroy` is safe
                    // to enter twice -- the second close reports the session
                    // gone, which the host reads as a positive reaping, and the
                    // second erase finds nothing -- and entering it here is what
                    // stops the daemon exiting while the tree is still there.
                    machine::ShutdownAction::Finish => victims.push(slot),
                    machine::ShutdownAction::Keep => {}
                }
            }
            victims
        };
        for slot in victims {
            self.destroy(slot).await;
        }
    }

    /// A census, for operators and for tests.
    pub async fn census(&self) -> PoolCensus {
        let state = self.state.lock().await;
        let counts = state.buckets();
        PoolCensus {
            live: state.live(),
            idle: state.idle_count(),
            in_flight: counts.in_flight(),
            clearing: counts.clearing(),
            leased: counts.leased(),
            reserved: counts.reserved(),
            tearing_down: counts.tearing_down(),
            leaked: u32::try_from(state.leaked_slots.len()).unwrap_or(u32::MAX),
            capacity: state.capacity(self.config.pool_size),
            halted: state.halted,
        }
    }

    /// Conversation → cell map for the doctor pool layer.
    ///
    /// Only instances that currently name a conversation and can resume or
    /// finish a turn (`Leased`, `CheckedOut`, `Delivering`). Sorted by cell
    /// so the report is stable.
    pub async fn conversation_leases(&self) -> Vec<ConversationLease> {
        let state = self.state.lock().await;
        let mut leases: Vec<ConversationLease> = state
            .instances
            .values()
            .filter_map(|instance| {
                let conversation_id = instance.conversation_id.as_ref()?;
                matches!(
                    instance.state,
                    InstanceState::Leased | InstanceState::CheckedOut | InstanceState::Delivering
                )
                .then(|| ConversationLease {
                    conversation_id: conversation_id.clone(),
                    cell: format!("s{}e{}", instance.slot, instance.epoch),
                    state: instance.state.to_string(),
                })
            })
            .collect();
        leases.sort_by(|left, right| left.cell.cmp(&right.cell));
        leases
    }

    /// Every pool-level invariant, checked from outside.
    ///
    /// Exposed rather than private so a test can assert it after every step of
    /// an arbitrary command sequence: an invariant only the implementation can
    /// see is an invariant a test cannot prove.
    ///
    /// # Errors
    ///
    /// Returns the first violated invariant.
    pub async fn check_invariants(&self) -> Result<(), PoolInvariantViolation> {
        let state = self.state.lock().await;
        let capacity = state.capacity(self.config.pool_size);
        if state.live() > capacity {
            return Err(PoolInvariantViolation::OverCapacity {
                live: state.live(),
                capacity,
            });
        }
        for (class, members) in &state.idle {
            for slot in members {
                let Some(instance) = state.instances.get(slot) else {
                    return Err(PoolInvariantViolation::IdleSetHoldsNonIdle {
                        slot: *slot,
                        state: InstanceState::Retired,
                    });
                };
                if instance.state != InstanceState::Idle {
                    return Err(PoolInvariantViolation::IdleSetHoldsNonIdle {
                        slot: *slot,
                        state: instance.state,
                    });
                }
                if instance.class != *class {
                    return Err(PoolInvariantViolation::IdleSetClassMismatch { slot: *slot });
                }
            }
        }
        for (slot, instance) in &state.instances {
            if state.leaked_slots.contains(slot) {
                return Err(PoolInvariantViolation::LeakedSlotStillLive { slot: *slot });
            }
            if instance.state == InstanceState::Idle
                && !state
                    .idle
                    .get(&instance.class)
                    .is_some_and(|members| members.contains(slot))
            {
                return Err(PoolInvariantViolation::IdleInstanceNotPublished { slot: *slot });
            }
            instance
                .check_invariants(
                    self.config.recycle_turns,
                    self.config.system_prompt_fingerprint,
                )
                .map_err(PoolInvariantViolation::Instance)?;
        }
        Ok(())
    }

    // ---- admission -------------------------------------------------------

    /// Admit this caller, waiting -- bounded -- for a slot that is coming back.
    ///
    /// # The defect this loop exists for, MEASURED
    ///
    /// `admit_once` is the whole of admission and it never waited. The pool
    /// answers its caller BEFORE it types `/clear` (see `spawn_clear`, which
    /// exists so a slow clear costs capacity and never latency), so the ordinary
    /// state of a pool one instant after a burst is **every slot clearing** --
    /// housekeeping MEASURED at 703-756 ms with nobody waiting on any of it. A
    /// caller arriving there was refused, and the refusal said so in as many
    /// words: "3 of 3 usable instance(s) are live -- 0 serving a turn, 3
    /// clearing between turns, with no caller waiting, 0 idle".
    ///
    /// At 8 concurrent callers against 3 slots over 3 rounds, that refused 21 of
    /// 24 callers and left rounds 2 and 3 taking 782 and 539 MICROSECONDS: the
    /// pool served 3 calls from 3 launches, so no instance ever served a second
    /// caller and every fungibility claim in that wave was vacuous. It failed
    /// FASTER than it passed, which is the signature of a capacity signal that
    /// is false rather than a pool that is busy.
    ///
    /// # What is waited on, and what is not
    ///
    /// [`machine::CensusBucket::comes_back_on_its_own`] decides, and it is a
    /// claim about who the pool is waiting for: `Clearing` and teardown come
    /// back with no caller's help, a `Serving` slot is holding a model that
    /// takes however long a model takes, and a `Reserved` one is a launch
    /// already spoken for. So genuine exhaustion still refuses on the first read
    /// with `waited_ms == 0`.
    ///
    /// # Why this is still not a queue
    ///
    /// There is no order, no fairness, no wait list and no per-class quota. Each
    /// waiter re-reads the pool and races every other waiter, and both bounds
    /// are hard: [`config::ADMISSION_WAIT_CEILING_MS`], because a pool under
    /// sustained load always has something clearing and the predicate alone
    /// would wait forever; and the caller's own deadline, because spending a
    /// caller's budget on admission and then handing it a turn it can no longer
    /// finish is worse than refusing. The smaller of the two wins, and a
    /// refusal at the end of a real wait names the wait.
    async fn admit(
        self: &Arc<Self>,
        class: InstanceClass,
        deadline_ms: u64,
    ) -> Result<Admitted, ErrorBody> {
        let started = tokio::time::Instant::now();
        let ceiling = Duration::from_millis(config::ADMISSION_WAIT_CEILING_MS);
        let poll = Duration::from_millis(config::ADMISSION_POLL_MS);
        loop {
            let waited = started.elapsed();
            let waited_ms = u64::try_from(waited.as_millis()).unwrap_or(u64::MAX);
            // Both bounds, resolved BEFORE the read rather than after it,
            // because the read has to know whether this caller can still afford
            // to wait: a caller with nothing left must take the cold swap rather
            // than be refused beside an idle instance. `None` is "this is the
            // last look".
            let budget = ceiling
                .checked_sub(waited)
                // The caller's own deadline, on the pool's clock, re-read every
                // pass so a caller whose deadline expires WHILE it waits is
                // refused at that moment rather than at the ceiling.
                .map(|under_ceiling| {
                    under_ceiling.min(Duration::from_millis(
                        deadline_ms.saturating_sub(self.clock.now_ms()),
                    ))
                })
                .filter(|budget| !budget.is_zero());
            let refusal = match self.admit_once(class, waited_ms, budget.is_some()).await {
                Ok(admission) => {
                    return Ok(Admitted {
                        admission,
                        waited_ms,
                    });
                }
                Err(refusal) => refusal,
            };
            // Nothing here comes back without a caller finishing a turn, so a
            // wait is a queue with this same refusal at the end of it. This arm
            // also covers every refusal that is not about capacity at all --
            // shutdown, a halt, a broken invariant -- because `admit_once`
            // reports zero for all of them.
            let Some(budget) = budget.filter(|_| refusal.coming_back > 0) else {
                return Err(refusal.body);
            };
            tokio::time::sleep(budget.min(poll)).await;
        }
    }

    /// One read of the pool under one lock: the four admission rules.
    ///
    /// `waited_ms` is threaded in rather than measured here so that the refusal
    /// this builds and the census it prints describe the same instant.
    ///
    /// `may_wait_longer` is false on the caller's LAST look, and it is what
    /// keeps rule 3 from being deferred into a refusal -- see the rule itself.
    async fn admit_once(
        self: &Arc<Self>,
        class: InstanceClass,
        waited_ms: u64,
        may_wait_longer: bool,
    ) -> Result<Admission, AdmissionRefusal> {
        // Every refusal that is not about capacity: waiting cannot change any of
        // them, and `coming_back: 0` is what says so.
        let final_refusal = |body: ErrorBody| AdmissionRefusal {
            body,
            coming_back: 0,
        };
        let mut state = self.state.lock().await;
        if state.shutting_down {
            return Err(final_refusal(refusal::daemon_shutting_down()));
        }
        if let Some(violation) = state.halted {
            return Err(final_refusal(refusal::pool_halted(violation)));
        }

        // Rule 1: a warm instance of this class. Pure bookkeeping, zero I/O.
        if let Some(slot) = state.lru_of(class) {
            self.transition_locked(&mut state, slot, Transition::CheckOut)
                .map_err(|violation| final_refusal(internal(&violation.to_string())))?;
            let should_rewarm = !state.idle.contains_key(&class)
                && state.live() < state.capacity(self.config.pool_size)
                && state.rewarming.insert(class);
            drop(state);
            if should_rewarm {
                self.spawn_rewarm(class);
            }
            return Ok(Admission::Warm(slot));
        }

        // Rule 2: capacity is free, so reserve before releasing the lock. The
        // reservation is what stops N concurrent cold calls each reading
        // `live == 0` and starting N launches for one slot.
        if state.live() < state.capacity(self.config.pool_size)
            && let Some(slot) = state.free_slot(self.config.pool_size)
        {
            self.reserve_locked(&mut state, slot, class);
            return Ok(Admission::Reserved(slot));
        }

        // The pool read ONCE, and every remaining answer comes out of it: the
        // census the refusal prints, the count `admit` decides to wait on, and
        // the deferral below. Reading the state twice would let those describe
        // different instants, and a refusal describing a pool the caller was
        // never actually up against is the whole defect being fixed here.
        let pressure = state.pressure(self.config.pool_size);

        // Rule 3: no capacity, but some other class is idle. Take its LRU --
        // unless something is already on its way back and this caller can still
        // afford to wait for it.
        //
        // **A cold swap is the alternative to REFUSING, not the alternative to
        // WAITING.** It destroys an instance that has been proven clean and
        // pays a full mint for the replacement, to save a caller a wait that
        // MEASURES at 703-756 ms. Firing it the instant a slot appears also
        // takes that slot out from under a caller of the instance's OWN class
        // who is waiting beside it, and once callers wait at all that stops
        // being an edge case and becomes the steady state. MEASURED at 8
        // concurrent callers across 4 classes against 3 slots, with the wait in
        // and this deferral out: **7 launches for 7 served calls** -- every
        // single call was served by a process the pool had just built, having
        // destroyed one it had just proven clean, and no instance ever served a
        // second caller.
        //
        // The deferral is bounded by exactly the thing being waited for, and it
        // cannot become a refusal: it needs BOTH a slot on its way back and a
        // caller with budget left, and `may_wait_longer` is false on the last
        // look. So the starvation guarantee rule 3 exists for is unchanged --
        // no caller is ever refused while another class sits idle -- it is only
        // made to wait for its own class first. A pool holding nothing but idle
        // instances of another class has `coming_back == 0` and swaps on the
        // first read, at no added latency, which is the ordinary cold case.
        let defer_cold_swap = may_wait_longer && pressure.coming_back() > 0;
        let victim = (!defer_cold_swap)
            .then(|| {
                state
                    .idle
                    .keys()
                    .copied()
                    .filter_map(|other| state.lru_of(other))
                    .min_by_key(|slot| {
                        state
                            .instances
                            .get(slot)
                            .map_or(u64::MAX, |instance| instance.idle_since_ms)
                    })
            })
            .flatten();
        if let Some(slot) = victim {
            self.transition_locked(&mut state, slot, Transition::ColdSwapVictim)
                .map_err(|violation| final_refusal(internal(&violation.to_string())))?;
            return Ok(Admission::ColdSwap(slot));
        }

        // Rule 4: nothing this caller may have. Refuse, name the budget, and
        // name what -- if anything -- is on its way back.
        //
        // NOT "every instance is mid-turn": the instances holding the slots may
        // equally be `Reserved`, `Warming`, `Quarantined` or `Destroying`, none
        // of which `in_flight` counts. See `refusal::pool_exhausted`.
        Err(AdmissionRefusal {
            body: refusal::pool_exhausted(pressure, class, waited_ms),
            coming_back: pressure.coming_back(),
        })
    }

    fn reserve_locked(&self, state: &mut PoolState, slot: SlotId, class: InstanceClass) {
        let epoch = state.take_epoch(slot);
        let paths = SlotPaths::new(&self.config.parent_dir, slot, epoch);
        state.instances.insert(
            slot,
            Instance::reserved(
                slot,
                epoch,
                class,
                paths,
                self.config.system_prompt_fingerprint,
            ),
        );
    }

    /// Hand a just-destroyed slot straight back to the caller who evicted its
    /// occupant, at a fresh epoch.
    async fn reclaim(
        &self,
        slot: SlotId,
        class: InstanceClass,
        waited_ms: u64,
    ) -> Result<(), ErrorBody> {
        let mut state = self.state.lock().await;
        if state.instances.contains_key(&slot) {
            return Err(internal("a reclaimed slot was still occupied"));
        }
        if state.leaked_slots.contains(&slot) {
            // The victim's teardown could not prove its process reaped, so its
            // slot is gone for good. Refusing here costs this caller one turn;
            // reusing it would cost the guarantee.
            //
            // A refusal of its OWN, not the exhaustion one: this path can be
            // reached with nothing in flight and another class still idle, and
            // an exhaustion message would then describe a pool state that is
            // not the reason for the refusal.
            return Err(refusal::reclaimed_slot_leaked(
                state.pressure(self.config.pool_size),
                class,
                waited_ms,
            ));
        }
        self.reserve_locked(&mut state, slot, class);
        Ok(())
    }

    // ---- mint ------------------------------------------------------------

    async fn mint(self: &Arc<Self>, slot: SlotId) -> Result<(), ErrorBody> {
        let (spec, paths) = {
            let mut state = self.state.lock().await;
            self.transition_locked(&mut state, slot, Transition::BeginWarm)
                .map_err(|violation| internal(&violation.to_string()))?;
            let instance = &state.instances[&slot];
            (
                MintSpec {
                    slot,
                    epoch: instance.epoch,
                    class: instance.class,
                    root: instance.paths.root.clone(),
                    cwd: instance.paths.cwd.clone(),
                    claude_executable: self.config.claude_executable.clone(),
                    system_prompt: self.config.system_prompt.clone(),
                    instance_idle_ttl_ms: self.config.instance_idle_ttl_ms,
                },
                instance.paths.clone(),
            )
        };

        // A minified cell requires a pristine root, so the roots are minted
        // 0700 and empty by pmux, from operator config plus a slot identity,
        // with no request byte anywhere in the path.
        if let Err(error) = mint_roots(&self.config.parent_dir, &paths) {
            self.abandon_mint(slot, false).await;
            return Err(error);
        }

        // THE RESERVATION FOR THE LAUNCH ITSELF, taken immediately before it
        // and not one statement earlier: from here until the host answers, a
        // child may exist that this instance has no handle for, and
        // `Instance::mint_in_flight` is the only thing that says so. A
        // concurrent `shutdown` drains `Warming`, and without this bit its
        // `destroy` reads `handle: None` as "no process was ever launched" and
        // erases the root out from under a live Claude, releasing the slot with
        // the census reporting nothing wrong.
        //
        // A slot already gone here was torn down between the two locks, before
        // any launch: nothing was started, so nothing is owed but the refusal.
        {
            let mut state = self.state.lock().await;
            let Some(instance) = state.instances.get_mut(&slot) else {
                return Err(internal(
                    "a launching slot was released before its mint began",
                ));
            };
            instance.mint_in_flight = true;
        }

        match self.host.mint(spec.clone()).await {
            Ok(handle) => {
                // Best effort, and deliberately not fatal: the pid file turns a
                // boot-time cwd scan into an exact kill list, but a pool that
                // refuses to serve because it could not write a diagnostic
                // would be trading a guarantee for a convenience.
                if let Some(pid) = handle.pid {
                    let paths = SlotPaths::new(&self.config.parent_dir, spec.slot, spec.epoch);
                    let _ = std::fs::write(&paths.pid_file, pid.to_string());
                }
                let mut state = self.state.lock().await;
                let Some(instance) = state.instances.get_mut(&slot) else {
                    // The slot was torn down while this launch was in flight,
                    // so the handle names a process the pool no longer owns a
                    // slot for. The teardown already subtracted that slot and
                    // said so -- it read `mint_in_flight` and leaked -- which
                    // leaves exactly one thing worth doing with a handle that
                    // arrived too late: spend it closing the child it names.
                    // Not to un-leak the slot: the root was retained because a
                    // live process may have been writing into it, and reaping
                    // the process now does not make that root pmux's to delete.
                    drop(state);
                    tracing::error!(
                        operation = "path_b_orphan",
                        slot,
                        epoch = spec.epoch,
                        pid = handle.pid,
                        "a stateless mint completed into a slot that had already been torn down; \
                         closing the child it launched"
                    );
                    let _ = self.host.destroy(&handle).await;
                    return Err(internal(
                        "a launching slot was torn down before its mint returned",
                    ));
                };
                instance.handle = Some(handle);
                instance.mint_in_flight = false;
                Ok(())
            }
            Err(failure) => {
                self.abandon_mint(slot, failure.process_may_survive).await;
                Err(failure.error)
            }
        }
    }

    async fn abandon_mint(self: &Arc<Self>, slot: SlotId, process_may_survive: bool) {
        {
            let mut state = self.state.lock().await;
            let _ = self.transition_locked(&mut state, slot, Transition::MintFailed);
            // The mint has RETURNED, so no launch is outstanding any more, and
            // `process_may_survive` below is what the pool knows about the
            // child. That is the better answer of the two: the host measured
            // it, where `mint_in_flight` only ever meant "nobody has measured
            // yet". Leaving it set would leak the slot of every mint the host
            // positively proved left no process.
            if let Some(instance) = state.instances.get_mut(&slot) {
                instance.mint_in_flight = false;
            }
        }
        if process_may_survive {
            // The host could not prove the child is gone, so the tree is
            // evidence rather than garbage and the slot is subtracted.
            self.leak(slot, "mint_left_a_possibly_live_process").await;
        } else {
            self.destroy(slot).await;
        }
    }

    async fn remint_if_below_floor(self: &Arc<Self>, class: InstanceClass) {
        let should = {
            let mut state = self.state.lock().await;
            let idle = state.idle.get(&class).map_or(0, |members| {
                u32::try_from(members.len()).unwrap_or(u32::MAX)
            });
            idle < self.config.warm_floor(class) && state.rewarming.insert(class)
        };
        if should {
            self.spawn_rewarm(class);
        }
    }

    fn spawn_rewarm(self: &Arc<Self>, class: InstanceClass) {
        // High-water-mark re-warm: a checkout that emptied a class's idle set
        // mints a replacement immediately, so the NEXT caller of that shape
        // finds a warm instance instead of paying a launch. It is bounded by
        // the same `pool_size` every other mint is, and by one outstanding
        // re-warm per class, so a burst cannot queue background work.
        let pool = Arc::clone(self);
        self.spawner.spawn(Box::pin(async move {
            let slot = {
                let mut state = pool.state.lock().await;
                let free = (!state.shutting_down && state.halted.is_none())
                    .then(|| state.free_slot(pool.config.pool_size))
                    .flatten();
                match free {
                    Some(slot) if state.live() < state.capacity(pool.config.pool_size) => {
                        pool.reserve_locked(&mut state, slot, class);
                        Some(slot)
                    }
                    _ => None,
                }
            };
            if let Some(slot) = slot
                && pool.mint(slot).await.is_ok()
            {
                pool.publish_idle(slot).await;
            }
            pool.state.lock().await.rewarming.remove(&class);
        }));
    }

    // ---- idle publication ------------------------------------------------

    async fn publish_idle(self: &Arc<Self>, slot: SlotId) {
        let published = {
            let mut state = self.state.lock().await;
            self.publish_idle_locked(&mut state, slot, Transition::WarmProven)
        };
        if !published {
            self.abandon_unpublishable(slot, Self::AFTER_WARM).await;
        }
    }

    /// Publish a freshly-proven instance and take it, atomically.
    ///
    /// The instance really does pass through the idle set with
    /// [`Transition::WarmProven`] as its last transition -- the proof-carrying
    /// invariant is satisfied, not bypassed -- and it does so under one lock
    /// acquisition, so the caller who paid for the mint cannot have it stolen.
    async fn publish_idle_and_check_out(
        self: &Arc<Self>,
        slot: SlotId,
    ) -> Result<SlotId, ErrorBody> {
        let mut state = self.state.lock().await;
        if !self.publish_idle_locked(&mut state, slot, Transition::WarmProven) {
            drop(state);
            self.abandon_unpublishable(slot, Self::AFTER_WARM).await;
            return Err(internal(
                "a freshly proven instance could not enter the idle set",
            ));
        }
        // A refused checkout leaves the instance IDLE and published, which is
        // the correct outcome: the launch proof stands, so the instance is
        // serviceable -- this caller simply cannot have it. Tearing it down
        // here would destroy a provably clean instance over a bookkeeping
        // refusal.
        self.transition_locked(&mut state, slot, Transition::CheckOut)
            .map_err(|violation| internal(&violation.to_string()))?;
        Ok(slot)
    }

    /// Returns whether the instance actually reached the idle set.
    ///
    /// A refusal here means the proof-carrying transition was legal but the
    /// resulting state broke an invariant -- reachable if the daemon ever
    /// reloads its system prompt while instances are live, which
    /// [`Instance::check_invariants`] refuses so a caller cannot be served
    /// under a prompt the daemon no longer holds. Every caller must destroy on
    /// `false`: an instance that is neither idle nor being torn down holds its
    /// slot forever, which is a capacity leak with no diagnostic.
    #[must_use]
    fn publish_idle_locked(
        &self,
        state: &mut PoolState,
        slot: SlotId,
        transition: Transition,
    ) -> bool {
        if self.transition_locked(state, slot, transition).is_err() {
            return false;
        }
        let now = self.clock.now_ms();
        if let Some(instance) = state.instances.get_mut(&slot) {
            instance.idle_since_ms = now;
            let class = instance.class;
            state.idle.entry(class).or_default().insert(slot);
            return true;
        }
        false
    }

    // ---- turn completion -------------------------------------------------

    async fn commit(
        self: &Arc<Self>,
        slot: SlotId,
        class: &InstanceClass,
        resolved: &class::ResolvedModelEffort,
        turn: HostTurn,
    ) -> Result<StatelessResult, ErrorBody> {
        // The sidechain guard, which is what makes reusing `UsageBreakdown`
        // honest here. Two independent checks, because either alone could be
        // the one that drifts.
        //
        // `None` REFUSES. It used to read as `0` -- `unwrap_or(0)` -- beside a
        // comment saying the token check made that safe. It did not: a sidechain
        // row that carried no usage at all leaves `usage.sidechain` at its
        // default, so both checks passed and the turn committed with its
        // isolation claim unmade. The type exists so a host that cannot count
        // can say so; the pool's part of that bargain is to treat the silence as
        // a failed check. Production counts -- `TurnResult::sidechain_rows` --
        // so this arm is unreachable from the native host and is here for any
        // other implementation of the seam.
        let Some(counted_rows) = turn.sidechain_rows else {
            self.quarantine_and_destroy(slot).await;
            return Err(refusal::sidechain_rows_not_counted());
        };
        if counted_rows > 0 || turn.usage.sidechain != Default::default() {
            self.quarantine_and_destroy(slot).await;
            return Err(refusal::sidechain_on_toolless_cell(counted_rows));
        }

        let claude_version = self
            .handle_of(slot)
            .await
            .map(|handle| handle.claude_version)
            .ok_or_else(|| internal("a committed turn lost its process handle"))?;

        let result = StatelessResult {
            // What pmux ASKED for: the class key, resolved before checkout.
            model: class.canonical_model.to_owned(),
            // What REPLIED, when the transcript carried a row saying so. A
            // second field rather than a narrowing of the first, because
            // conflating them is how a probe measures the wrong thing.
            reported_model: turn.reported_model,
            effort: resolved.effort_level,
            text: turn.text,
            stop_reason: turn.stop_reason,
            usage: turn.usage,
            claude_version,
        };

        {
            let mut state = self.state.lock().await;
            self.transition_locked(&mut state, slot, Transition::TurnCommitted)
                .map_err(|violation| internal(&violation.to_string()))?;
            self.transition_locked(&mut state, slot, Transition::ResponseDelivered)
                .map_err(|violation| internal(&violation.to_string()))?;
        }
        // The clear runs on a task nobody waits on, so a slow clear costs
        // capacity and never latency. The instance is already out of every idle
        // set, so no other caller can reach it while it clears.
        self.spawn_clear(slot);
        Ok(result)
    }

    async fn commit_sticky(
        self: &Arc<Self>,
        slot: SlotId,
        conversation_id: &str,
        class: &InstanceClass,
        resolved: &class::ResolvedModelEffort,
        turn: HostTurn,
    ) -> Result<StickyTurn, ErrorBody> {
        let Some(counted_rows) = turn.sidechain_rows else {
            self.quarantine_and_destroy(slot).await;
            return Err(refusal::sidechain_rows_not_counted());
        };
        if counted_rows > 0 || turn.usage.sidechain != Default::default() {
            self.quarantine_and_destroy(slot).await;
            return Err(refusal::sidechain_on_toolless_cell(counted_rows));
        }

        let (claude_version, cell) = {
            let state = self.state.lock().await;
            let instance = state
                .instances
                .get(&slot)
                .ok_or_else(|| internal("a committed sticky turn lost its instance"))?;
            let claude_version = instance
                .handle
                .as_ref()
                .map(|handle| handle.claude_version.clone())
                .ok_or_else(|| internal("a committed sticky turn lost its process handle"))?;
            (
                claude_version,
                format!("s{}e{}", instance.slot, instance.epoch),
            )
        };

        let result = StatelessResult {
            model: class.canonical_model.to_owned(),
            reported_model: turn.reported_model,
            effort: resolved.effort_level,
            text: turn.text,
            stop_reason: turn.stop_reason,
            usage: turn.usage,
            claude_version,
        };

        {
            let mut state = self.state.lock().await;
            self.transition_locked(&mut state, slot, Transition::TurnCommitted)
                .map_err(|violation| internal(&violation.to_string()))?;
            if state.shutting_down {
                // The caller already has the bytes. Do not LeaseHeld after the
                // shutdown scan: that is the leftover-root class measured for
                // Clearing. Drain through the existing Delivering → Clearing
                // → Destroying edges.
                self.transition_locked(&mut state, slot, Transition::ResponseDelivered)
                    .map_err(|violation| internal(&violation.to_string()))?;
                self.transition_locked(&mut state, slot, Transition::ShutdownDrain)
                    .map_err(|violation| internal(&violation.to_string()))?;
                drop(state);
                self.destroy(slot).await;
                return Ok(StickyTurn { result, cell });
            }
            // Bind before LeaseHeld: Leased requires a conversation id, and
            // Delivering admits one. Setting it after the transition would
            // refuse the candidate as LeasedWithoutConversation.
            if let Some(instance) = state.instances.get_mut(&slot) {
                instance.conversation_id = Some(conversation_id.to_owned());
            }
            self.transition_locked(&mut state, slot, Transition::LeaseHeld)
                .map_err(|violation| internal(&violation.to_string()))?;
            if let Some(instance) = state.instances.get_mut(&slot) {
                instance.idle_since_ms = self.clock.now_ms();
            }
        }
        Ok(StickyTurn { result, cell })
    }

    fn spawn_clear(self: &Arc<Self>, slot: SlotId) {
        let pool = Arc::clone(self);
        self.spawner.spawn(Box::pin(async move {
            pool.finish_turn(slot).await;
        }));
    }

    async fn finish_turn(self: &Arc<Self>, slot: SlotId) {
        let Some(handle) = self.handle_of(slot).await else {
            self.destroy(slot).await;
            return;
        };
        match self.host.clear(&handle).await {
            Ok(()) => {
                let Some(transition) = ({
                    let state = self.state.lock().await;
                    state.instances.get(&slot).map(|instance| {
                        machine::clear_success_transition(
                            instance.turns_started,
                            self.config.recycle_turns,
                        )
                    })
                }) else {
                    // The slot was torn down while `/clear` was in the host.
                    // `spawn_clear` runs this on a task nobody waits on and
                    // `shutdown` drains `Clearing`, so the teardown really can
                    // complete under a clear that then resumes -- and it is a
                    // COMPLETED teardown, not a partial one: the only two ways
                    // an entry leaves the map are `destroy` proving the process
                    // reaped and `leak` subtracting the slot, and both have
                    // already reported themselves. There is nothing left to
                    // return to service and nothing left to tear down.
                    //
                    // The fallible read is the same one `destroy` makes for the
                    // same reason; this was the one site that indexed instead,
                    // and it panicked the daemon's own task with `no entry
                    // found for key`.
                    tracing::warn!(
                        operation = "path_b_clear",
                        slot,
                        "a stateless instance was torn down while its clear was in flight"
                    );
                    return;
                };
                if transition == Transition::ClearProven {
                    let published = {
                        let mut state = self.state.lock().await;
                        self.publish_idle_locked(&mut state, slot, Transition::ClearProven)
                    };
                    if !published {
                        self.abandon_unpublishable(slot, Self::AFTER_CLEAR).await;
                    }
                } else {
                    let class = {
                        let mut state = self.state.lock().await;
                        let class = state.instances.get(&slot).map(|instance| instance.class);
                        let _ = self.transition_locked(&mut state, slot, Transition::RecycleDue);
                        class
                    };
                    self.destroy(slot).await;
                    // Recycle is lease-end only. Destroying at the cap must
                    // remint when the class would fall below its warm floor;
                    // leased-TTL expiry is a lease end and must not leave a
                    // declared floor empty.
                    if let Some(class) = class {
                        self.remint_if_below_floor(class).await;
                    }
                }
            }
            Err(failure) => {
                // Classification only: code, the two fate bits, and the driver's
                // `violation`/`reason`/`field` keys. Never `ErrorBody::message`
                // and never the rest of `details` -- those can carry screen
                // text, paths, and wait matchers.
                let details = &failure.error.details;
                tracing::warn!(
                    operation = "path_b_clear",
                    slot,
                    code = ?failure.error.code,
                    retryable = failure.error.retryable,
                    clear_not_submitted = failure.clear_not_submitted,
                    preamble_mismatch = failure.preamble_mismatch.unwrap_or(""),
                    violation = details
                        .get("violation")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or(""),
                    reason = details
                        .get("reason")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or(""),
                    field = details
                        .get("field")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or(""),
                    "a pooled instance failed /clear"
                );
                if let Some(reason) = failure.preamble_mismatch {
                    // Not one bad instance: pmux's model of the post-`/clear`
                    // preamble no longer matches the installed Claude. Stop
                    // minting, refuse every checkout, page. The reason travels
                    // with the halt because it is the only place the operator
                    // sees WHICH part of the preamble moved.
                    self.halt(reason).await;
                }
                let transition =
                    if failure.clear_not_submitted && failure.preamble_mismatch.is_none() {
                        // The driver positively claims nothing was typed, so the
                        // instance is coherent -- but it has no turn to serve and
                        // just failed to clear.
                        Transition::ClearFailedCoherent
                    } else {
                        Transition::ClearFailedIncoherent
                    };
                {
                    let mut state = self.state.lock().await;
                    let _ = self.transition_locked(&mut state, slot, transition);
                    if transition == Transition::ClearFailedIncoherent {
                        let _ = self.transition_locked(&mut state, slot, Transition::BeginDestroy);
                    }
                }
                self.destroy(slot).await;
            }
        }
    }

    /// Tear down an instance that could not be published to the idle set.
    ///
    /// It is neither serviceable nor being destroyed, and an instance in that
    /// position holds its slot forever.
    ///
    /// `teardown` is the transition path out, and it differs by where the
    /// publish was refused -- a launch proof that did not stick is a mint
    /// failure, while a clear proof that did not stick follows `/clear` that
    /// may already have been typed and is therefore a quarantine. Passing the
    /// path in rather than guessing it here keeps that distinction at the site
    /// that knows the answer.
    async fn abandon_unpublishable(self: &Arc<Self>, slot: SlotId, teardown: &[Transition]) {
        {
            let mut state = self.state.lock().await;
            for transition in teardown {
                let _ = self.transition_locked(&mut state, slot, *transition);
            }
        }
        self.destroy(slot).await;
    }

    /// From `WARMING`: the launch proof was refused, so this is a mint failure.
    const AFTER_WARM: &'static [Transition] = &[Transition::MintFailed];
    /// From `CLEARING`: the clear proof was refused after `/clear` may have
    /// been typed, so the instance is quarantined before it is destroyed.
    const AFTER_CLEAR: &'static [Transition] =
        &[Transition::ClearFailedIncoherent, Transition::BeginDestroy];

    async fn quarantine_and_destroy(self: &Arc<Self>, slot: SlotId) {
        {
            let mut state = self.state.lock().await;
            let _ = self.transition_locked(&mut state, slot, Transition::TurnNotDelivered);
            let _ = self.transition_locked(&mut state, slot, Transition::BeginDestroy);
        }
        self.destroy(slot).await;
    }

    // ---- teardown --------------------------------------------------------

    /// Destroy in strict order, and the order IS the guarantee.
    ///
    /// 1. Force-close and require the process positively reaped. **Nothing on
    ///    disk is touched until this returns true**: a live Claude holds
    ///    `history.jsonl` under a lock and recreates what you delete, and
    ///    deleting a config root out from under a live Claude races its own
    ///    `.claude.json` writer.
    /// 2. Retain the EVIDENCE: mirror this instance's transcripts, pruned to
    ///    the eight fields the drain measurement reads, into the evidence
    ///    corpus. This is the only window in which the file exists and nothing
    ///    is writing to it -- step 1 has just proven the process reaped and
    ///    step 3 is about to delete it. `crate::pool::evidence` is what it
    ///    keeps and why it keeps so little.
    /// 3. Discharge retention: a quarantined instance's tree is moved to the
    ///    retain dir when one is configured; a cleanly recycled instance's tree
    ///    is erased with no floor, because a quarantine is precisely the case
    ///    where an operator has something to read.
    /// 4. Erase the epoch directory. This is what finally destroys
    ///    `history.jsonl`, `paste-cache/`, `projects/`, `backups/`,
    ///    `.claude.json` and `settings.json` -- every channel at once, because
    ///    they are all under one root.
    /// 5. **Only now release the slot**, and bump the epoch. If the slot were
    ///    released at step 1, a replacement could mint while a prior caller's
    ///    `history.jsonl` still existed on disk, and the pool's live count
    ///    would understate what is retained.
    async fn destroy(self: &Arc<Self>, slot: SlotId) {
        let Some((handle, mint_in_flight, paths, quarantined)) = ({
            let state = self.state.lock().await;
            state.instances.get(&slot).map(|instance| {
                (
                    instance.handle.clone(),
                    instance.mint_in_flight,
                    instance.paths.clone(),
                    instance.was_quarantined,
                )
            })
        }) else {
            return;
        };

        // Why this is a reason and not a bool: step 1 has three outcomes, not
        // two, and the third one used to be spelled the same as the first.
        let unreaped = match &handle {
            Some(handle) => (!self
                .host
                .destroy(handle)
                .await
                .is_ok_and(|destroyed| destroyed.process_reaped))
            .then_some("close_without_confirmed_reaping"),
            // A LAUNCH IS OUTSTANDING. `InstanceHost::mint` has been called and
            // has not answered, so a child may already exist that this instance
            // has no handle to close. Nothing here can prove the boundary
            // empty, and this is precisely the arm that used to be folded into
            // the one below it -- "no handle yet" read as "no process ever",
            // which erased a live Claude's config root and released its slot
            // with `leaked` still 0.
            None if mint_in_flight => Some("destroyed_while_a_launch_was_in_flight"),
            // No process was ever launched, so the boundary is empty by
            // construction. This is the only arm that skips step 1, and it
            // skips it because there is nothing to prove.
            None => None,
        };

        if let Some(violation) = unreaped {
            self.leak(slot, violation).await;
            return;
        }

        // The corpus for the NEXT Claude Code version, taken here because
        // here is the only place it exists. A failure is LOGGED and never
        // fatal: retention is evidence-gathering, and a teardown that refused
        // to erase a config root because a mirror could not be written would
        // trade a guarantee for a convenience.
        if let Some(evidence_dir) = &self.config.evidence_dir {
            match evidence::retain_instance_transcripts(&paths.root, evidence_dir) {
                Ok(retained) if retained.files > 0 => tracing::debug!(
                    operation = "path_b_evidence",
                    slot,
                    files = retained.files,
                    rows = retained.rows,
                    pruned = retained.pruned,
                    directory = evidence_dir.display().to_string(),
                    "retained redacted Path B transcripts as version-drift evidence"
                ),
                Ok(_) => {}
                Err(error) => tracing::warn!(
                    operation = "path_b_evidence",
                    slot,
                    %error,
                    directory = evidence_dir.display().to_string(),
                    "Path B evidence could not be retained; the teardown continues"
                ),
            }
        }

        let retain = quarantined
            .then(|| self.config.retain_dir.clone())
            .flatten();
        if let Err(error) = erase_tree(&self.config.parent_dir, &paths, retain.as_deref()) {
            tracing::error!(
                operation = "path_b_destroy",
                slot,
                epoch = paths.epoch_dir.display().to_string(),
                %error,
                "stateless instance root could not be erased; slot subtracted and retained as evidence"
            );
            self.leak(slot, "root_could_not_be_erased").await;
            return;
        }

        let mut state = self.state.lock().await;
        if self
            .transition_locked(&mut state, slot, Transition::Reaped)
            .is_ok()
        {
            state.instances.remove(&slot);
        }
    }

    async fn leak(self: &Arc<Self>, slot: SlotId, violation: &'static str) {
        let mut state = self.state.lock().await;
        let _ = self.transition_locked(&mut state, slot, Transition::ReapFailed);
        if let Some(instance) = state.instances.remove(&slot) {
            state.leaked_slots.insert(slot);
            tracing::error!(
                operation = "path_b_leak",
                slot,
                epoch = instance.epoch,
                pid = instance.handle.as_ref().and_then(|handle| handle.pid),
                root = instance.paths.root.display().to_string(),
                violation,
                "stateless instance leaked: slot permanently subtracted, root retained, operator must reap by hand"
            );
        }
    }

    async fn halt(&self, violation: &'static str) {
        let mut state = self.state.lock().await;
        if state.halted.is_none() {
            state.halted = Some(violation);
            tracing::error!(
                operation = "path_b_halt",
                violation,
                "stateless pool halted: pmux's model of the installed Claude's post-/clear preamble no longer matches it"
            );
        }
    }

    // ---- plumbing --------------------------------------------------------

    fn transition_locked(
        &self,
        state: &mut PoolState,
        slot: SlotId,
        transition: Transition,
    ) -> Result<InstanceState, TransitionRefusal> {
        let Some(instance) = state.instances.get_mut(&slot) else {
            return Err(TransitionRefusal::NoSuchSlot { slot });
        };
        let next = machine::step(instance.state, transition).map_err(TransitionRefusal::Illegal)?;
        // Built as a CANDIDATE and only then committed, so a refused transition
        // leaves the instance exactly as it was. Mutating first and validating
        // afterwards would strand an instance in the half-applied state its own
        // invariant just rejected -- neither serviceable nor destroyable,
        // holding a slot with no path out of it.
        let mut candidate = instance.clone();
        if matches!(transition, Transition::CheckOut | Transition::ResumeLease) {
            // Incremented at CHECKOUT, not at check-in: a prompt reaches
            // `history.jsonl` at submission, so a counter incremented at
            // check-in miscounts a turn that was submitted and then failed.
            candidate.turns_started = candidate.turns_started.saturating_add(1);
        }
        if matches!(
            transition,
            Transition::ReleaseLease
                | Transition::ShutdownDrain
                | Transition::TurnNotDelivered
                | Transition::ResponseDelivered
        ) {
            candidate.conversation_id = None;
        }
        candidate.state = next;
        candidate.last_transition = Some(transition);
        candidate.was_quarantined |= next == InstanceState::Quarantined;
        if let Err(violation) = candidate.check_invariants(
            self.config.recycle_turns,
            self.config.system_prompt_fingerprint,
        ) {
            // A transition that breaks an invariant is a bug in this module, so
            // it is logged loudly and refused rather than published.
            tracing::error!(
                operation = "path_b_invariant",
                slot,
                %transition,
                %violation,
                "stateless instance transition violated its state invariant"
            );
            return Err(TransitionRefusal::Invariant(violation));
        }
        // Leaving `Idle` unpublishes the instance HERE, in the one place a
        // state can change, rather than at each of the four call sites that
        // can cause it. A call site that forgot would leave the idle set naming
        // an instance that is on its way to teardown, and the next caller to
        // read that set under the same lock would try to check out a
        // `Destroying` process.
        let leaving_idle = instance.state == InstanceState::Idle && next != InstanceState::Idle;
        let class = candidate.class;
        *instance = candidate;
        if leaving_idle {
            state.remove_from_idle(class, slot);
        }
        Ok(next)
    }

    async fn handle_of(&self, slot: SlotId) -> Option<InstanceHandle> {
        let state = self.state.lock().await;
        state
            .instances
            .get(&slot)
            .and_then(|instance| instance.handle.clone())
    }
}

/// A census of the pool, for operators and tests. Never on the wire: a caller
/// cannot choose an instance, cannot influence warmth and cannot act on these
/// numbers, so publishing them would only invite retry policies built on top of
/// pmux's internal scheduling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolCensus {
    pub live: u32,
    pub idle: u32,
    /// `CheckedOut | Delivering`: a caller is waiting on these right now.
    pub in_flight: u32,
    /// `Clearing`: the caller already has its answer and nobody is waiting.
    /// Reported separately from `in_flight` because an operator's response to
    /// the two is different -- a pool full of clearing instances frees up in
    /// milliseconds, a pool full of serving ones in however long the model
    /// takes -- and because folding them made a refusal say "8 of 8 are
    /// serving a turn" with nobody waiting on any of them.
    pub clearing: u32,
    /// `Leased`: a Messages conversation owns the instance between turns.
    pub leased: u32,
    pub reserved: u32,
    /// `Quarantined | Destroying`: the slot is still held.
    pub tearing_down: u32,
    pub leaked: u32,
    pub capacity: u32,
    pub halted: Option<&'static str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TransitionRefusal {
    NoSuchSlot { slot: SlotId },
    Illegal(machine::IllegalTransition),
    Invariant(instance::InvariantViolation),
}

impl std::fmt::Display for TransitionRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSuchSlot { slot } => write!(formatter, "slot {slot} is not live"),
            Self::Illegal(illegal) => write!(formatter, "{illegal}"),
            Self::Invariant(violation) => write!(formatter, "{violation}"),
        }
    }
}

fn internal(message: &str) -> ErrorBody {
    ErrorBody::new(ErrorCode::Internal, message.to_owned())
        .with_details(json!({ "violation": "path_b_pool_internal" }))
}

/// Refuse to boot on an operator directory that is not the caller's and
/// owner-only, exactly as the daemon already refuses on its socket directory.
///
/// `PoolSettings::validate` cannot ask this: it is a pure function over
/// operator strings, and its whole value is that it runs before anything
/// exists. So the mode-and-ownership half runs here, at the one point where the
/// pool is allowed to touch the filesystem and where a refusal still fails
/// daemon startup rather than a caller's turn.
///
/// The bar is the socket directory's, and deliberately not a lower one: this
/// tree receives every pool instance's `CLAUDE_CONFIG_DIR` and cwd. A tree
/// pmux creates is created owner-only; a tree the operator already made is
/// REFUSED rather than silently re-permissioned, because widening or narrowing
/// an operator's existing directory is a decision pmux has no standing to make
/// and a `chmod` nobody asked for is a worse surprise than a boot refusal.
fn require_private_parent(path: &Path, field: ConfigField) -> Result<(), ErrorBody> {
    create_private_dir_all(path).map_err(|error| {
        ErrorBody::new(
            ErrorCode::InvalidConfig,
            format!("{field} {}: could not create it: {error}", path.display()),
        )
    })?;
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        ErrorBody::new(
            ErrorCode::InvalidConfig,
            format!("{field} {}: could not inspect it: {error}", path.display()),
        )
    })?;
    if !metadata.file_type().is_dir() {
        return Err(ErrorBody::new(
            ErrorCode::InvalidConfig,
            format!("{field} {} is not a directory", path.display()),
        ));
    }
    #[cfg(unix)]
    if let Some(reason) = crate::private_dir::owner_only_violation(&metadata, effective_uid()) {
        return Err(ErrorBody::new(
            ErrorCode::InvalidConfig,
            format!(
                "{field} {} must be owner-only and owned by the daemon's user, the same bar the \
                 daemon's socket directory is held to, because every pool instance's config root \
                 and cwd live under it: {reason}",
                path.display()
            ),
        ));
    }
    Ok(())
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn effective_uid() -> u32 {
    // SAFETY: `geteuid` has no preconditions and does not dereference pointers.
    unsafe { libc::geteuid() }
}

/// Mint every directory of `<parent>/<slot>/<epoch>/{root,cwd}` 0700 and empty.
///
/// A minified cell requires a pristine root, so "empty" is not a nicety: a
/// directory that already exists is refused rather than reused, because the
/// only way an epoch directory can pre-exist is that a previous daemon left it
/// there, and adopting a tree pmux was not watching means inferring emptiness
/// rather than proving it.
///
/// The set of directories walked is [`SlotPaths::minted_dirs`] and is not
/// listed here. The previous version listed `epoch_dir`, `root` and `cwd`,
/// leaving `create_dir_all` to make `<parent>/<slot>` at `0o777 & !umask` --
/// MEASURED at `drwxr-xr-x` on this host, surviving daemon shutdown, its
/// entries enumerating pool size and epoch counters to any local user.
///
/// # The refusal says what happens next, because the refusal is what an
/// operator gets
///
/// It used to stop at "the pool never adopts a tree it did not create", which
/// is the RULE and not the SITUATION. An operator reading it learns that pmux
/// declined, not that a previous daemon was killed before it could clean up,
/// not that this start has already erased the tree it named, and not that
/// starting again makes progress. All three are true and all three are tested:
/// the sole caller is [`Pool::mint`], whose only response to this error is
/// `abandon_mint(slot, false)` -- and that instance has no handle and no
/// in-flight launch, so `destroy` takes the arm that erases the tree.
/// `path_b_pool::a_refused_epoch_tree_is_erased_by_the_start_that_refused_it`
/// is the predicate behind the sentence.
fn mint_roots(parent: &Path, paths: &SlotPaths) -> Result<(), ErrorBody> {
    if paths.epoch_dir.exists() {
        return Err(internal(&format!(
            "epoch directory {} already exists, so a previous daemon did not shut down cleanly; \
             the pool never adopts a tree it did not create, and this mint erases that tree as it \
             fails, so a repeated start passes this slot",
            paths.epoch_dir.display()
        )));
    }
    let minted = paths.minted_dirs(parent).ok_or_else(|| {
        internal(&format!(
            "{} is not under the pool parent {}",
            paths.epoch_dir.display(),
            parent.display()
        ))
    })?;
    for directory in &minted {
        create_owner_only(directory)?;
    }
    Ok(())
}

/// Create one directory owner-only, and seal it if a previous daemon left it
/// wider.
///
/// The seal is not redundant with [`create_private_dir_all`]: `<parent>/<slot>`
/// outlives the epochs beneath it, so a slot directory a pre-fix daemon created
/// at `0o755` is still there when this daemon mints epoch 1 into it. Creating
/// privately fixes new trees; the seal fixes the ones already on disk.
fn create_owner_only(path: &Path) -> Result<(), ErrorBody> {
    create_private_dir_all(path)
        .map_err(|error| internal(&format!("could not create {}: {error}", path.display())))?;
    seal_owner_only(path).map_err(|error| {
        internal(&format!(
            "could not seal {} to 0700: {error}",
            path.display()
        ))
    })
}

/// Erase one instance's whole tree, after its process was proven reaped.
///
/// The containment check is not decoration: `remove_dir_all` on a path that is
/// not under the pool parent, or that is a symlink, is the difference between
/// erasing an instance and erasing something an operator cared about.
fn erase_tree(
    parent: &Path,
    paths: &SlotPaths,
    retain_dir: Option<&Path>,
) -> Result<(), std::io::Error> {
    if !paths.epoch_dir.starts_with(parent) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "{} is not under the pool parent {}",
                paths.epoch_dir.display(),
                parent.display()
            ),
        ));
    }
    let metadata = match std::fs::symlink_metadata(&paths.epoch_dir) {
        Ok(metadata) => metadata,
        // Already gone: a mint that failed before creating the tree has nothing
        // to erase, and that is a success, not a leak.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "{} is not a directory pmux minted",
                paths.epoch_dir.display()
            ),
        ));
    }

    if let Some(retain_dir) = retain_dir {
        // Quarantine evidence: moved out whole, under the operator's retention
        // floor, because a quarantine is exactly the case where there is
        // something to read. A clean recycle gets no floor at all.
        //
        // Owner-only at every level pmux creates, for the same reason the pool
        // parent is: this directory receives an instance's whole config root,
        // which is the single richest thing the pool ever writes to disk.
        create_private_dir_all(retain_dir)?;
        let stamped = retain_dir.join(format!(
            "{}-{}",
            paths
                .epoch_dir
                .parent()
                .and_then(|slot| slot.file_name())
                .and_then(|name| name.to_str())
                .unwrap_or("slot"),
            paths
                .epoch_dir
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("epoch")
        ));
        if stamped.exists() {
            std::fs::remove_dir_all(&stamped)?;
        }
        std::fs::rename(&paths.epoch_dir, &stamped)?;
        return Ok(());
    }

    std::fs::remove_dir_all(&paths.epoch_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn erasing_refuses_a_tree_outside_the_pool_parent() {
        let outside = SlotPaths::new(Path::new("/somewhere/else"), 0, 0);
        let error = erase_tree(Path::new("/pool"), &outside, None)
            .expect_err("a tree outside the parent must be refused");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn erasing_a_tree_that_never_existed_is_not_a_leak() {
        let temp = tempfile::tempdir().expect("tempdir");
        let paths = SlotPaths::new(temp.path(), 4, 1);
        erase_tree(temp.path(), &paths, None)
            .expect("a mint that failed before creating anything has nothing to erase");
    }

    /// Every level of a minted tree is owner-only, and the set walked is the
    /// chain itself.
    ///
    /// The previous version of this test walked a HAND-WRITTEN
    /// `[&paths.epoch_dir, &paths.root, &paths.cwd]`, and the one directory in
    /// the chain that `mint_roots` did not seal -- `<parent>/<slot>`, created as
    /// a side effect by `create_dir_all` -- was the one absent from the array.
    /// It passed against `drwxr-xr-x /tmp/pmux-parent-probe/0` on this host.
    /// The walk is now `SlotPaths::minted_dirs`, the same derivation
    /// `mint_roots` creates from, so a level cannot be sealed-but-unchecked or
    /// checked-but-unsealed.
    #[test]
    fn every_minted_level_is_owner_only_and_the_tree_is_empty_and_never_adopted() {
        let temp = tempfile::tempdir().expect("tempdir");
        let parent = temp.path().join("pool-parent");
        let paths = SlotPaths::new(&parent, 2, 5);
        mint_roots(&parent, &paths).expect("a fresh epoch mints");
        assert!(paths.root.is_dir());
        assert!(paths.cwd.is_dir());
        assert_eq!(
            std::fs::read_dir(&paths.root)
                .expect("root readable")
                .count(),
            0,
            "a minified cell requires a pristine root"
        );

        let minted = paths
            .minted_dirs(&parent)
            .expect("the tree is under the pool parent");
        assert!(
            minted.contains(&parent.join("2")),
            "the slot directory is part of the chain pmux mints and must be walked: {minted:?}"
        );
        assert_eq!(
            minted.last().map(std::path::PathBuf::as_path),
            Some(paths.cwd.as_path()),
            "the chain ends at the deepest leaf: {minted:?}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in &minted {
                let mode = std::fs::metadata(path)
                    .expect("metadata")
                    .permissions()
                    .mode();
                assert_eq!(mode & 0o777, 0o700, "{} must be owner-only", path.display());
            }
        }

        // The pool never adopts a tree it did not create in this process.
        assert!(
            mint_roots(&parent, &paths).is_err(),
            "an existing epoch directory is evidence of a previous daemon, not a reusable tree"
        );
    }

    /// A tree that is not under the pool parent is refused before `mkdir`, not
    /// only before `remove_dir_all`.
    ///
    /// Without the containment answer, the ancestor walk that derives the chain
    /// has no stopping point and would seal every directory up to `/`.
    #[test]
    fn minting_refuses_a_tree_outside_the_pool_parent() {
        let outside = SlotPaths::new(Path::new("/somewhere/else"), 0, 0);
        assert!(outside.minted_dirs(Path::new("/pool")).is_none());
        let error = mint_roots(Path::new("/pool"), &outside)
            .expect_err("a tree outside the parent must be refused before it is created");
        assert!(
            error.message.contains("is not under the pool parent"),
            "{}",
            error.message
        );
    }

    /// A slot directory a pre-fix daemon left at 0755 is sealed by the next
    /// mint into that slot rather than inherited.
    ///
    /// The fix has to reach trees already on disk: `<parent>/<slot>` outlives
    /// every epoch under it, so creating new levels privately is not enough on
    /// a host that has already run the shipped daemon.
    #[test]
    #[cfg(unix)]
    fn a_slot_directory_left_wide_by_an_older_daemon_is_sealed_by_the_next_mint() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let parent = temp.path().join("pool-parent");
        let legacy_slot = parent.join("7");
        std::fs::create_dir_all(&legacy_slot).expect("legacy slot");
        std::fs::set_permissions(&legacy_slot, std::fs::Permissions::from_mode(0o755))
            .expect("the mode a pre-fix daemon left");

        let paths = SlotPaths::new(&parent, 7, 0);
        mint_roots(&parent, &paths).expect("a fresh epoch mints into a legacy slot");
        assert_eq!(
            std::fs::metadata(&legacy_slot)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700,
            "the slot directory an older daemon left wide must be sealed, not inherited"
        );
    }

    /// The pool parent is held to the socket directory's bar, at boot.
    #[tokio::test]
    #[cfg(unix)]
    async fn a_group_readable_pool_parent_refuses_to_boot() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let parent = temp.path().join("wide-parent");
        std::fs::create_dir(&parent).expect("parent");
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755))
            .expect("group/other readable");

        let error = require_private_parent(&parent, ConfigField::ParentDir)
            .expect_err("a world-readable pool parent must refuse to boot");
        assert_eq!(error.code, ErrorCode::InvalidConfig);
        assert!(
            error.message.contains("--pool-parent") && error.message.contains("755"),
            "{}",
            error.message
        );

        // ...and pmux is not in the business of silently re-permissioning it.
        assert_eq!(
            std::fs::metadata(&parent)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o755,
            "a refusal must not be a chmod nobody asked for"
        );

        // An absent parent is CREATED owner-only rather than refused: that tree
        // is pmux's, and every level of it, not only the last.
        let created = temp.path().join("deep/made/by/pmux");
        require_private_parent(&created, ConfigField::ParentDir).expect("pmux creates its own");
        for level in ["deep", "deep/made", "deep/made/by", "deep/made/by/pmux"] {
            let path = temp.path().join(level);
            assert_eq!(
                std::fs::metadata(&path)
                    .expect("metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700,
                "{} must be owner-only",
                path.display()
            );
        }
    }

    #[test]
    fn a_quarantined_tree_is_moved_to_the_retain_dir_rather_than_erased() {
        let temp = tempfile::tempdir().expect("tempdir");
        let parent = temp.path().join("pool");
        let retain = temp.path().join("evidence");
        let paths = SlotPaths::new(&parent, 1, 3);
        std::fs::create_dir_all(&paths.root).expect("root");
        std::fs::write(paths.root.join("history.jsonl"), b"secret").expect("residue");

        erase_tree(&parent, &paths, Some(&retain)).expect("quarantine retains");
        assert!(!paths.epoch_dir.exists(), "the tree left the pool parent");
        let kept = retain.join("1-3");
        assert!(
            kept.join("root/history.jsonl").exists(),
            "a quarantine is exactly the case where an operator has something to read"
        );

        // ...while a clean recycle keeps nothing at all.
        let clean = SlotPaths::new(&parent, 2, 0);
        std::fs::create_dir_all(&clean.root).expect("root");
        std::fs::write(clean.root.join("history.jsonl"), b"secret").expect("residue");
        erase_tree(&parent, &clean, None).expect("clean recycle erases");
        assert!(!clean.epoch_dir.exists());
    }

    /// Every `Display` this module tree implements renders something, and the
    /// SET OF THEM IS DERIVED from the sources rather than listed.
    ///
    /// SURVIVING MUTANTS CLOSED: eight `<impl Display for _>::fmt ->
    /// Ok(Default::default())` -- `PoolInvariantViolation` (`mod.rs:216`),
    /// `TransitionRefusal` (`mod.rs:1360`), `ConfigRefusal` (`config.rs:395`),
    /// `InvariantViolation` (`instance.rs:186`), `InstanceClass`
    /// (`class.rs:258`), `InstanceState` (`machine.rs:194`), `Transition`
    /// (`machine.rs:282`) and `IllegalTransition` (`machine.rs:295`).
    ///
    /// `Ok(Default::default())` is a `fmt` that writes NOTHING and reports
    /// success, so every one of these types rendered the empty string with the
    /// suite green. They are not decoration: `PoolInvariantViolation` and
    /// `InvariantViolation` are what `check_invariants` puts in front of an
    /// operator when the pool halts, `TransitionRefusal` and `IllegalTransition`
    /// name the edge a slot could not take, and `InstanceClass` is half of the
    /// key a refusal quotes back. A pool that halts and prints "" is a pool
    /// nobody can diagnose.
    ///
    /// THE LIST IS DERIVED because a hand-written one is the defect this
    /// repository has now found thirty-three times: it is right the day it is
    /// written and silently narrows afterwards. `SAMPLES` must name every type this
    /// directory implements `Display` for, and a type that grows one tomorrow
    /// fails here by name until somebody renders it.
    #[test]
    fn every_display_this_pool_implements_renders_a_non_empty_reason() {
        // (type name, one rendered value, a substring that value must contain)
        //
        // The substring is what makes this more than a length check: a `fmt`
        // that wrote a constant non-empty string would pass "non-empty" and
        // still name the wrong thing.
        let samples: Vec<(&str, String, &str)> = vec![
            (
                "PoolInvariantViolation",
                PoolInvariantViolation::OverCapacity {
                    live: 9,
                    capacity: 4,
                }
                .to_string(),
                "9",
            ),
            (
                "TransitionRefusal",
                TransitionRefusal::NoSuchSlot { slot: 7 }.to_string(),
                "7",
            ),
            (
                "ConfigField",
                ConfigField::ClaudeExecutable.to_string(),
                "--pool-claude",
            ),
            (
                "ConfigRefusal",
                config::ConfigRefusal::PoolSizeOutOfRange {
                    requested: 99,
                    maximum: 8,
                }
                .to_string(),
                "99",
            ),
            (
                "InvariantViolation",
                instance::InvariantViolation::IdleUnderStalePrompt { slot: 5 }.to_string(),
                "5",
            ),
            (
                "InstanceClass",
                resolve_pool_class("sonnet", Some(pseudomux_protocol::v1::EffortLevel::Medium))
                    .expect("sonnet/medium is an admitted pool class")
                    .0
                    .to_string(),
                "sonnet",
            ),
            (
                "ModelEffortRefusal",
                resolve_pool_class("no-such-model", None)
                    .expect_err("an unknown model is refused")
                    .to_string(),
                "no-such-model",
            ),
            (
                "InstanceState",
                InstanceState::CheckedOut.to_string(),
                "checked_out",
            ),
            ("Transition", Transition::BeginWarm.to_string(), "BeginWarm"),
            (
                "IllegalTransition",
                machine::IllegalTransition {
                    from: InstanceState::Idle,
                    transition: Transition::BeginWarm,
                }
                .to_string(),
                "idle",
            ),
        ];

        // The derivation: every `impl ... Display for <Type>` this directory
        // declares, read at test time from `CARGO_MANIFEST_DIR` so a file added
        // tomorrow is scanned without anyone remembering to add a line.
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/pool");
        let mut declared = BTreeSet::new();
        let mut files = 0usize;
        for entry in std::fs::read_dir(&directory).expect("the pool source directory is readable") {
            let path = entry.expect("a readable directory entry").path();
            if path.extension().and_then(|value| value.to_str()) != Some("rs") {
                continue;
            }
            files += 1;
            let text = std::fs::read_to_string(&path).expect("a readable pool source file");
            for line in text.lines() {
                let line = line.trim_start();
                // `starts_with("impl")` and NOT `strip_prefix("impl ")`: the
                // latter misses `impl<'a> fmt::Display for X<'a>`, so a
                // lifetime or type parameter added to any of these types
                // tomorrow would drop it out of the derived set and this test
                // would go on passing while covering one fewer `Display`. A
                // derivation that narrows silently is the defect it exists to
                // prevent. Comment lines are excluded by the same test, since
                // `// impl Display for X` does not start with `impl`.
                if !line.starts_with("impl") {
                    continue;
                }
                // `std::fmt::Display for X {` and `fmt::Display for X {` both.
                let Some((_, tail)) = line.split_once("Display for ") else {
                    continue;
                };
                let name: String = tail
                    .chars()
                    .take_while(|value| value.is_alphanumeric() || *value == '_')
                    .collect();
                if !name.is_empty() {
                    declared.insert(name);
                }
            }
        }
        // Floors, in the idiom the rest of this tree uses: a scan that silently
        // stops matching reports the same empty set as one that found nothing
        // to complain about, so it REFUSES instead of passing.
        assert!(
            files >= 7,
            "the pool source scan found only {files} file(s) in {}",
            directory.display()
        );
        assert!(
            declared.len() >= 8,
            "the Display scan found only {} impl(s); a derivation that stopped \
             matching must refuse, not report full coverage",
            declared.len()
        );

        let rendered: BTreeSet<String> = samples
            .iter()
            .map(|(name, _, _)| (*name).to_owned())
            .collect();
        assert_eq!(
            declared,
            rendered,
            "every Display this directory implements must be rendered here; \
             declared-but-unrendered: {:?}; rendered-but-no-longer-declared: {:?}",
            declared.difference(&rendered).collect::<Vec<&String>>(),
            rendered.difference(&declared).collect::<Vec<&String>>(),
        );

        for (name, value, expected) in &samples {
            assert!(
                !value.is_empty(),
                "{name} rendered the empty string; a refusal nobody can read is \
                 not a refusal"
            );
            assert!(
                value.contains(expected),
                "{name} rendered {value:?}, which does not name {expected:?}"
            );
        }
    }

    /// An epoch directory pmux cannot even INSPECT is a refusal, not a clean
    /// erase.
    ///
    /// SURVIVING MUTANT CLOSED: `mod.rs:1504` -- the `NotFound` match guard on
    /// `symlink_metadata` replaced with `true`, which reports EVERY stat failure
    /// as "already gone" and answers `Ok(())`. `Pool::destroy` reads that `Ok`
    /// as "the root is erased", takes the `Reaped` edge, releases the slot and
    /// bumps the epoch -- so a tree pmux could not even look at is recorded as
    /// destroyed, `leaked` stays 0, nothing is logged, and the caller's
    /// `history.jsonl` is still on disk. The one existing case,
    /// `erasing_a_tree_that_never_existed_is_not_a_leak`, exercises the arm the
    /// guard SELECTS, so it passes identically either way.
    ///
    /// `0o600` AND NOT `0o300`: `stat(2)` needs SEARCH permission on each parent
    /// directory, which is the execute bit, and `0o300` is `-wx` -- it GRANTS
    /// exactly what this test means to take away. A fixture reaching for `0o300`
    /// here would create no condition at all and pass with the guard deleted,
    /// which is instance twenty-nine of this repository's bug class, found by
    /// this same tool in `crates/protocol/src/v1.rs`. So the
    /// premise is asserted before anything depends on it, and this test FAILS AS
    /// A BROKEN FIXTURE rather than passing vacuously if the process running it
    /// can walk a closed directory anyway -- which is what running as root does.
    #[test]
    #[cfg(unix)]
    fn an_epoch_directory_that_cannot_be_inspected_is_refused_rather_than_called_erased() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let parent = temp.path().join("pool-parent");
        let paths = SlotPaths::new(&parent, 3, 0);
        mint_roots(&parent, &paths).expect("a fresh epoch mints");
        let slot_dir = parent.join("3");

        std::fs::set_permissions(&slot_dir, std::fs::Permissions::from_mode(0o600))
            .expect("close the slot directory");
        let premise = std::fs::symlink_metadata(&paths.epoch_dir).err();
        let refused = erase_tree(&parent, &paths, None);
        std::fs::set_permissions(&slot_dir, std::fs::Permissions::from_mode(0o700))
            .expect("reopen the slot directory");

        assert_eq!(
            premise.map(|error| error.kind()),
            Some(std::io::ErrorKind::PermissionDenied),
            "a parent with no search bit must make `stat` fail with EACCES, or \
             this test proves nothing about the arm that reports it"
        );
        let error = refused.expect_err(
            "a tree pmux cannot inspect must be a refusal -- which leaks the slot \
             and pages an operator -- and never a silent Ok that erased nothing",
        );
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(
            paths.root.is_dir(),
            "and the tree really is still there, which is what the refusal is for"
        );
    }

    /// A path that is not a directory pmux minted is refused BY THAT NAME, and
    /// the two ways it can fail to be one are two clauses rather than one.
    ///
    /// SURVIVING MUTANT CLOSED: `mod.rs:1507 || -> &&`, which demands an entry
    /// be BOTH a symlink AND not a directory. `symlink_metadata` never follows,
    /// so a symlink's own `is_dir()` is always false and the two clauses agree
    /// about every symlink -- which is the only shape the existing cases build.
    /// The shape they do not build is the one `&&` lets through: a REGULAR FILE
    /// where the epoch directory belongs, which under the mutant falls past the
    /// guard into `remove_dir_all` on a file. Both shapes are here, so the
    /// clause that already held is not dropped in order to add the one that did
    /// not.
    #[test]
    fn an_epoch_path_that_is_not_a_minted_directory_is_refused_by_that_name() {
        let temp = tempfile::tempdir().expect("tempdir");
        let parent = temp.path().join("pool-parent");

        // A regular file where the epoch directory belongs.
        let file = SlotPaths::new(&parent, 5, 0);
        std::fs::create_dir_all(file.epoch_dir.parent().expect("the slot directory"))
            .expect("slot directory");
        std::fs::write(&file.epoch_dir, b"not a directory").expect("a file where the tree belongs");
        let error = erase_tree(&parent, &file, None)
            .expect_err("a regular file is not a directory pmux minted");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(
            error.to_string().contains("is not a directory pmux minted"),
            "the refusal must name what it refused: {error}"
        );
        assert!(
            file.epoch_dir.is_file(),
            "and it must not have been removed on the way to refusing it"
        );

        // ...and a symlink, which is the clause that already held: the whole
        // point of the containment checks is that `remove_dir_all` through a
        // link erases something an operator cared about.
        #[cfg(unix)]
        {
            let elsewhere = temp.path().join("somebody-elses-directory");
            std::fs::create_dir_all(&elsewhere).expect("a directory worth not deleting");
            std::fs::write(elsewhere.join("keep-me"), b"evidence").expect("contents");
            let link = SlotPaths::new(&parent, 6, 0);
            std::fs::create_dir_all(link.epoch_dir.parent().expect("the slot directory"))
                .expect("slot directory");
            std::os::unix::fs::symlink(&elsewhere, &link.epoch_dir).expect("symlink");
            let error = erase_tree(&parent, &link, None)
                .expect_err("a symlink is not a directory pmux minted");
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
            assert!(
                elsewhere.join("keep-me").exists(),
                "the directory the link pointed at must be untouched"
            );
        }
    }

    /// A live pool under `tokio`, with this module's PRIVATE state reachable.
    ///
    /// # Why this exists beside `crates/service/tests/path_b_pool.rs`
    ///
    /// That file already drives this pool deterministically against a double,
    /// with a real filesystem, a driven clock and a queueing spawner, and
    /// everything observable from OUTSIDE the pool is observed there. What an
    /// integration test structurally cannot do is stand the pool in a state the
    /// pool refuses to enter -- and `Pool::check_invariants` is the one public
    /// method whose entire job is to answer for exactly those states.
    ///
    /// It had NO test. `cargo-mutants` replaced its whole body with `Ok(())` and
    /// the suite stayed green, because every one of its callers asserts that it
    /// returns `Ok`: forty-odd `assert_invariants(&harness)` calls, including
    /// forty inside one mixed sequence, all of which a constant `Ok(())`
    /// satisfies perfectly. A checker nothing ever makes fail is a checker that
    /// can be deleted, and this pool's invariants are what stands between a
    /// re-used instance and a leaked transcript.
    ///
    /// So the harness is in-module on purpose: `PoolState`, `Pool::state` and
    /// `PoolState`'s fields are private to `crate::pool`, and a test that plants
    /// a violation has to be able to reach them.
    mod live {
        use std::collections::{BTreeMap, BTreeSet};
        use std::future::Future;
        use std::path::{Path, PathBuf};
        use std::pin::Pin;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering};

        use async_trait::async_trait;
        use pseudomux_protocol::v1::{
            EffortLevel, ErrorBody, ErrorCode, SessionGenerationId, SessionId,
        };

        use crate::pool::class::{InstanceClass, resolve_pool_class};
        use crate::pool::config::PoolSettings;
        use crate::pool::host::{
            ClearFailure, Destroyed, HostFailure, HostTurn, InstanceHandle, InstanceHost, MintSpec,
            Spawner,
        };
        use crate::pool::instance::{Instance, InvariantViolation, SlotId, SlotPaths};
        use crate::pool::machine::{InstanceState, Transition};
        use crate::pool::{Pool, PoolInvariantViolation, PoolState, mint_roots};
        use crate::v1::Clock;

        /// A host that mints and does nothing else.
        ///
        /// Deliberately thinner than `path_b_pool.rs`'s double: the tests below
        /// plant states and read the pool's own bookkeeping, so a host able to
        /// script a turn failure would be scripting something none of them asks
        /// for. `mint` succeeds because a pool holding no instances cannot
        /// violate a pool-level invariant.
        struct MintingHost {
            next_pid: AtomicU64,
        }

        #[async_trait]
        impl InstanceHost for MintingHost {
            async fn mint(&self, spec: MintSpec) -> Result<InstanceHandle, HostFailure> {
                let mut bytes = [0_u8; 16];
                bytes[0..4].copy_from_slice(&spec.slot.to_be_bytes());
                bytes[4..12].copy_from_slice(&spec.epoch.to_be_bytes());
                Ok(InstanceHandle {
                    session_id: SessionId::from_bytes(bytes),
                    generation_id: SessionGenerationId::default(),
                    pid: Some(
                        i32::try_from(self.next_pid.fetch_add(1, Ordering::Relaxed))
                            .unwrap_or(i32::MAX),
                    ),
                    claude_version: "2.1.220".to_owned(),
                })
            }

            async fn run_turn(
                &self,
                _handle: &InstanceHandle,
                _prompt: String,
                _deadline_unix_ms: u64,
            ) -> Result<HostTurn, HostFailure> {
                Err(HostFailure::reaped(ErrorBody::new(
                    ErrorCode::Internal,
                    "no test in this module runs a turn",
                )))
            }

            async fn clear(&self, _handle: &InstanceHandle) -> Result<(), ClearFailure> {
                Ok(())
            }

            async fn destroy(&self, _handle: &InstanceHandle) -> Result<Destroyed, HostFailure> {
                Ok(Destroyed {
                    process_reaped: true,
                })
            }
        }

        /// A spawner that drops the work, stated rather than implied.
        ///
        /// That is the shape of a real defect -- it is `host.rs:246`'s surviving
        /// mutant, closed by a test in that file -- and it is admissible HERE
        /// only because nothing in this module queues background work at all:
        /// these tests plant a state and read `check_invariants`. Every test
        /// that cares what the background work does lives in `path_b_pool.rs`,
        /// over a spawner that queues it so the test can drain it.
        struct DroppingSpawner;

        impl Spawner for DroppingSpawner {
            fn spawn(&self, _work: Pin<Box<dyn Future<Output = ()> + Send>>) {}
        }

        struct FrozenClock;

        impl Clock for FrozenClock {
            fn now_ms(&self) -> u64 {
                1_000
            }
        }

        struct LivePool {
            pool: Arc<Pool>,
            _temp: tempfile::TempDir,
        }

        impl LivePool {
            fn build(mutate: impl FnOnce(&mut PoolSettings)) -> Self {
                let temp = tempfile::tempdir().expect("tempdir");
                let mut settings = PoolSettings::defaults(
                    temp.path().join("pool"),
                    PathBuf::from("/usr/bin/claude"),
                );
                settings.pool_size = 4;
                settings.rss_budget_mb = 4 * 1024;
                mutate(&mut settings);
                let config = settings.validate().expect("test settings must validate");
                let pool = Pool::new(
                    config,
                    Arc::new(MintingHost {
                        next_pid: AtomicU64::new(4_000),
                    }) as Arc<dyn InstanceHost>,
                    Arc::new(FrozenClock) as Arc<dyn Clock>,
                    Arc::new(DroppingSpawner) as Arc<dyn Spawner>,
                );
                Self { pool, _temp: temp }
            }

            fn class(&self, model: &str) -> InstanceClass {
                resolve_pool_class(model, Some(EffortLevel::High))
                    .expect("an admitted pool class")
                    .0
            }

            /// One instance in a given state, with everything ITS OWN invariant
            /// requires already true.
            ///
            /// The individual invariant is asserted here, so a test planting a
            /// POOL-level violation asserts the violation it means to instead of
            /// tripping `PoolInvariantViolation::Instance` on the way. A test
            /// that wants a broken instance breaks one after this returns, in
            /// one line, and says so.
            fn instance(
                &self,
                slot: SlotId,
                class: InstanceClass,
                state: InstanceState,
            ) -> Instance {
                let config = self.pool.config();
                let mut instance = Instance::reserved(
                    slot,
                    0,
                    class,
                    SlotPaths::new(&config.parent_dir, slot, 0),
                    config.system_prompt_fingerprint,
                );
                instance.state = state;
                if state != InstanceState::Reserved {
                    instance.handle = Some(InstanceHandle {
                        session_id: SessionId::from_bytes([7_u8; 16]),
                        generation_id: SessionGenerationId::default(),
                        pid: None,
                        claude_version: "2.1.220".to_owned(),
                    });
                }
                if state == InstanceState::Idle {
                    instance.last_transition = Some(Transition::WarmProven);
                }
                if matches!(
                    state,
                    InstanceState::CheckedOut
                        | InstanceState::Delivering
                        | InstanceState::Leased
                        | InstanceState::Clearing
                ) {
                    instance.turns_started = 1;
                }
                if state == InstanceState::Leased {
                    instance.conversation_id = Some("planted".to_owned());
                    instance.last_transition = Some(Transition::LeaseHeld);
                }
                instance
                    .check_invariants(config.recycle_turns, config.system_prompt_fingerprint)
                    .expect("a planted instance must be individually well formed");
                instance
            }

            async fn plant(&self, mutate: impl FnOnce(&mut PoolState)) {
                let mut state = self.pool.state.lock().await;
                mutate(&mut state);
            }

            async fn violation(&self) -> PoolInvariantViolation {
                self.pool
                    .check_invariants()
                    .await
                    .expect_err("the planted state violates a pool invariant")
            }
        }

        /// The name of one `PoolInvariantViolation` variant.
        ///
        /// WILDCARD-FREE ON PURPOSE: a variant added to the enum is a compile
        /// error here, and the scan below then requires somebody to plant it.
        fn variant_name(violation: &PoolInvariantViolation) -> &'static str {
            match violation {
                PoolInvariantViolation::IdleSetHoldsNonIdle { .. } => "IdleSetHoldsNonIdle",
                PoolInvariantViolation::IdleInstanceNotPublished { .. } => {
                    "IdleInstanceNotPublished"
                }
                PoolInvariantViolation::IdleSetClassMismatch { .. } => "IdleSetClassMismatch",
                PoolInvariantViolation::OverCapacity { .. } => "OverCapacity",
                PoolInvariantViolation::LeakedSlotStillLive { .. } => "LeakedSlotStillLive",
                PoolInvariantViolation::Instance(_) => "Instance",
            }
        }

        /// Every variant of `PoolInvariantViolation`, READ FROM THE SOURCE.
        ///
        /// A hand-written list is the defect this repository has now found
        /// thirty-three times: correct the day it is written, silently narrower
        /// afterwards. This one cannot narrow -- a variant added tomorrow
        /// appears here and fails the test until a case plants it.
        fn declared_violations() -> BTreeSet<String> {
            let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/pool/mod.rs");
            let text =
                std::fs::read_to_string(&source).expect("this module's own source is readable");
            let body = text
                .split_once("pub enum PoolInvariantViolation {")
                .expect("this module declares PoolInvariantViolation")
                .1
                .split_once("\n}")
                .expect("the declaration is closed by a brace in column zero")
                .0;
            let mut declared = BTreeSet::new();
            for line in body.lines() {
                let line = line.trim_start();
                let name: String = line
                    .chars()
                    .take_while(|value| value.is_alphanumeric() || *value == '_')
                    .collect();
                if !name.starts_with(|value: char| value.is_ascii_uppercase()) {
                    continue;
                }
                let Some(rest) = line.strip_prefix(name.as_str()) else {
                    continue;
                };
                // `trim_start`, because `rustfmt` writes `Name { field: T }`
                // with a space and `Name(T)` without one. Without it this scan
                // found exactly ONE variant -- the tuple one -- and the floor
                // below is what said so instead of reporting full coverage over
                // a set of one.
                let rest = rest.trim_start();
                // A variant is `Name {`, `Name(` or a bare `Name,`. Every other
                // line inside the declaration is a doc comment or an attribute,
                // and both fail the uppercase test above.
                if rest.starts_with('{') || rest.starts_with('(') || rest.starts_with(',') {
                    declared.insert(name);
                }
            }
            assert!(
                declared.len() >= 6,
                "the PoolInvariantViolation scan found only {} variant(s); a \
                 derivation that stopped matching must refuse, not report full \
                 coverage",
                declared.len()
            );
            declared
        }

        /// Every pool-level invariant this module NAMES is refused when it is
        /// planted, and the set of them is derived from the enum rather than
        /// written down.
        ///
        /// SURVIVING MUTANT CLOSED: `mod.rs:520 Pool::check_invariants ->
        /// Ok(())`. It is a `pub` checker with dozens of callers and it had no
        /// test that could fail: every call site asserts `Ok`, which is exactly
        /// what the mutant returns. The states below are the ones an operator
        /// would otherwise be handed a wrong answer about -- an idle set naming
        /// an instance that is tearing down, an idle instance no caller can
        /// reach, a slot both leaked and live -- and none of them is reachable
        /// through the public API, which is why this harness is in-module and
        /// why here is the only place they can be built at all.
        #[tokio::test]
        async fn every_pool_invariant_this_module_names_is_refused_when_it_is_planted() {
            let mut refused: Vec<PoolInvariantViolation> = Vec::new();

            // 1. More live instances than the budget admits. Checked first, so
            //    this is also the case that must not be shadowed by the others.
            {
                let live = LivePool::build(|settings| {
                    settings.pool_size = 2;
                    settings.rss_budget_mb = 2 * 1024;
                });
                let class = live.class("claude-opus-5");
                let planted: Vec<Instance> = (0..3_u32)
                    .map(|slot| live.instance(slot, class, InstanceState::Reserved))
                    .collect();
                live.plant(move |state| {
                    for instance in planted {
                        state.instances.insert(instance.slot, instance);
                    }
                })
                .await;
                let violation = live.violation().await;
                assert_eq!(
                    violation,
                    PoolInvariantViolation::OverCapacity {
                        live: 3,
                        capacity: 2
                    }
                );
                refused.push(violation);
            }

            // 2. The idle set names a slot whose instance is not idle. This is
            //    the one a checkout acts on: it hands a caller an instance that
            //    is on its way to teardown.
            {
                let live = LivePool::build(|_| {});
                let class = live.class("claude-opus-5");
                let planted = live.instance(0, class, InstanceState::Clearing);
                live.plant(move |state| {
                    state.instances.insert(0, planted);
                    state.idle.entry(class).or_default().insert(0);
                })
                .await;
                let violation = live.violation().await;
                assert_eq!(
                    violation,
                    PoolInvariantViolation::IdleSetHoldsNonIdle {
                        slot: 0,
                        state: InstanceState::Clearing
                    }
                );
                refused.push(violation);
            }

            // 3. ...and the same clause when there is no instance at all, which
            //    reports `Retired` because that is what a slot with no instance
            //    means.
            {
                let live = LivePool::build(|_| {});
                let class = live.class("claude-opus-5");
                live.plant(move |state| {
                    state.idle.entry(class).or_default().insert(3);
                })
                .await;
                let violation = live.violation().await;
                assert_eq!(
                    violation,
                    PoolInvariantViolation::IdleSetHoldsNonIdle {
                        slot: 3,
                        state: InstanceState::Retired
                    }
                );
                refused.push(violation);
            }

            // 4. The idle set files an instance under a class that is not its
            //    own -- which is how an opus caller is handed a sonnet process.
            {
                let live = LivePool::build(|_| {});
                let opus = live.class("claude-opus-5");
                let sonnet = live.class("sonnet");
                assert_ne!(opus, sonnet, "this case needs two distinct classes");
                let planted = live.instance(0, opus, InstanceState::Idle);
                live.plant(move |state| {
                    state.instances.insert(0, planted);
                    state.idle.entry(sonnet).or_default().insert(0);
                })
                .await;
                let violation = live.violation().await;
                assert_eq!(
                    violation,
                    PoolInvariantViolation::IdleSetClassMismatch { slot: 0 }
                );
                refused.push(violation);
            }

            // 5. An idle instance in no idle set: it holds a slot forever and no
            //    caller can ever reach it.
            {
                let live = LivePool::build(|_| {});
                let class = live.class("claude-opus-5");
                let planted = live.instance(0, class, InstanceState::Idle);
                live.plant(move |state| {
                    state.instances.insert(0, planted);
                })
                .await;
                let violation = live.violation().await;
                assert_eq!(
                    violation,
                    PoolInvariantViolation::IdleInstanceNotPublished { slot: 0 }
                );
                refused.push(violation);
            }

            // 6. A slot both leaked and live. Leaking is permanent capacity
            //    loss, so a live instance in a leaked slot means the budget is
            //    being counted twice.
            {
                let live = LivePool::build(|_| {});
                let class = live.class("claude-opus-5");
                let planted = live.instance(0, class, InstanceState::Reserved);
                live.plant(move |state| {
                    state.instances.insert(0, planted);
                    state.leaked_slots.insert(0);
                })
                .await;
                let violation = live.violation().await;
                assert_eq!(
                    violation,
                    PoolInvariantViolation::LeakedSlotStillLive { slot: 0 }
                );
                refused.push(violation);
            }

            // 7. A per-instance invariant, reached through the pool: an idle
            //    instance whose last transition carries no emptiness proof. The
            //    helper refuses to build this one, so it is broken AFTER it is
            //    built, deliberately and in one line.
            {
                let live = LivePool::build(|_| {});
                let class = live.class("claude-opus-5");
                let mut planted = live.instance(0, class, InstanceState::Idle);
                planted.last_transition = None;
                live.plant(move |state| {
                    state.instances.insert(0, planted);
                    state.idle.entry(class).or_default().insert(0);
                })
                .await;
                let violation = live.violation().await;
                assert_eq!(
                    violation,
                    PoolInvariantViolation::Instance(InvariantViolation::IdleWithoutProof {
                        slot: 0,
                        last_transition: None,
                    })
                );
                refused.push(violation);
            }

            // ...and a pool with nothing wrong with it PASSES, so none of the
            // above is satisfied by a checker that refuses everything.
            {
                let live = LivePool::build(|_| {});
                let class = live.class("claude-opus-5");
                let planted = live.instance(0, class, InstanceState::Idle);
                live.plant(move |state| {
                    state.instances.insert(0, planted);
                    state.idle.entry(class).or_default().insert(0);
                })
                .await;
                live.pool
                    .check_invariants()
                    .await
                    .expect("a well formed pool must pass its own invariants");
            }

            let named: BTreeSet<String> = refused
                .iter()
                .map(|violation| variant_name(violation).to_owned())
                .collect();
            let declared = declared_violations();
            assert_eq!(
                declared,
                named,
                "every PoolInvariantViolation this module declares must be \
                 planted and refused here; declared-but-never-planted: {:?}; \
                 planted-but-no-longer-declared: {:?}",
                declared.difference(&named).collect::<Vec<&String>>(),
                named.difference(&declared).collect::<Vec<&String>>(),
            );
        }

        /// A free slot is never offered while the pool is at its budget, for
        /// every shape a pool state can take.
        ///
        /// **CLOSES NO SURVIVING MUTANT, and says so rather than claiming one.**
        /// It is the PREMISE three surviving mutants rest on, written as
        /// executable code so the equivalence argument can be re-checked instead
        /// of remembered:
        ///
        /// * `mod.rs:702 < -> <=`, the capacity test in `Pool::admit_once`'s
        ///   rule 2;
        /// * `mod.rs:903 < -> <=`, the same test in `Pool::spawn_rewarm`; and
        /// * `mod.rs:903`, that whole match guard replaced with `true`.
        ///
        /// All three widen a capacity test that is conjoined, in the same
        /// expression, with `free_slot(..).is_some()`. `free_slot` skips exactly
        /// the slots `capacity` subtracts -- the occupied ones and the leaked
        /// ones -- so `free_slot` answering `Some` ALREADY implies
        /// `live() < capacity()`, and `<` and `<=` agree on every state either
        /// site can be in. **They are equivalent mutants, not gaps.**
        ///
        /// The day that stops being true -- an instance minted into a slot
        /// outside `0..pool_size`, a leaked slot that stays live -- this test
        /// fails and those three mutants become real.
        #[test]
        fn a_free_slot_is_never_offered_while_the_pool_is_at_its_budget() {
            let class = resolve_pool_class("claude-opus-5", Some(EffortLevel::High))
                .expect("an admitted pool class")
                .0;
            let mut offered = 0_u32;
            let mut withheld = 0_u32;
            for pool_size in 1_u32..=4 {
                for occupied in 0..(1_u32 << pool_size) {
                    for leaked in 0..(1_u32 << pool_size) {
                        // Disjoint, because a slot that is both is exactly
                        // `PoolInvariantViolation::LeakedSlotStillLive`, which
                        // the pool refuses to hold.
                        if occupied & leaked != 0 {
                            continue;
                        }
                        let mut state = PoolState {
                            instances: BTreeMap::new(),
                            idle: BTreeMap::new(),
                            next_epoch: BTreeMap::new(),
                            leaked_slots: BTreeSet::new(),
                            rewarming: BTreeSet::new(),
                            pending_leases: BTreeSet::new(),
                            protected_leases: BTreeSet::new(),
                            halted: None,
                            shutting_down: false,
                        };
                        for slot in 0..pool_size {
                            if occupied & (1 << slot) != 0 {
                                state.instances.insert(
                                    slot,
                                    Instance::reserved(
                                        slot,
                                        0,
                                        class,
                                        SlotPaths::new(Path::new("/pool"), slot, 0),
                                        0,
                                    ),
                                );
                            }
                            if leaked & (1 << slot) != 0 {
                                state.leaked_slots.insert(slot);
                            }
                        }
                        match state.free_slot(pool_size) {
                            Some(slot) => {
                                offered += 1;
                                assert!(
                                    slot < pool_size,
                                    "free_slot offered {slot}, outside 0..{pool_size}"
                                );
                                assert!(
                                    state.live() < state.capacity(pool_size),
                                    "free_slot offered slot {slot} with live={} against \
                                     capacity={} (pool_size={pool_size}, occupied={occupied:#b}, \
                                     leaked={leaked:#b}); the three mutants that rest on this \
                                     implication are now REAL",
                                    state.live(),
                                    state.capacity(pool_size),
                                );
                            }
                            None => withheld += 1,
                        }
                    }
                }
            }
            assert!(
                offered > 0 && withheld > 0,
                "both outcomes must occur ({offered} offered, {withheld} withheld) \
                 or the implication above is vacuous"
            );
        }

        /// An instance that could not enter the idle set is torn down rather
        /// than left holding its slot.
        ///
        /// SURVIVING MUTANT CLOSED: `mod.rs:1130 Pool::abandon_unpublishable
        /// with ()`. `publish_idle_locked`'s own doc names what that costs:
        /// "an instance that is neither idle nor being torn down holds its slot
        /// forever, which is a capacity leak with no diagnostic".
        ///
        /// **AND IT IS THE MUTANT THAT PROVES THE MEASUREMENT NEEDS A QUIET
        /// MACHINE.** The pool-only run of 2026-08-07 recorded it CAUGHT; the
        /// complete whole-scope run on an idle machine recorded it MISSED.
        /// MISSED is the true answer and the argument is short:
        /// `abandon_unpublishable` is reached only when `publish_idle_locked`
        /// returns false, which happens only when a proof-carrying transition
        /// would leave an instance that breaks its own invariant -- and nothing
        /// in this tree constructed one until this test. A test that goes flaky
        /// under load has its failure attributed to whatever mutant was in
        /// flight, which is the drift `docs/archive/testing-gate-a-census.md` records; this is the
        /// first instance of it measured inside `pool/**`.
        #[tokio::test]
        async fn an_instance_that_cannot_enter_the_idle_set_is_torn_down_rather_than_stranded() {
            let live = LivePool::build(|_| {});
            let class = live.class("claude-opus-5");
            let held = live.pool.config().system_prompt_fingerprint;

            // A warming instance minted under a system prompt the daemon no
            // longer holds. This is the case `Instance::check_invariants`
            // exists to refuse, and the reachable cause `publish_idle_locked`
            // names: a configuration reload while instances are live.
            let mut planted = live.instance(0, class, InstanceState::Warming);
            planted.prompt_fingerprint = held ^ 0xdead_beef;
            let paths = planted.paths.clone();
            mint_roots(&live.pool.config().parent_dir, &paths)
                .expect("the tree a warming instance owns");
            assert!(paths.root.is_dir(), "the fixture's own premise");

            live.plant(move |state| {
                state.instances.insert(0, planted);
            })
            .await;

            live.pool.publish_idle(0).await;

            let census = live.pool.census().await;
            assert_eq!(
                census.live, 0,
                "an instance that cannot be published is neither serviceable nor \
                 being destroyed, and one left in that position holds its slot \
                 for the daemon's whole life"
            );
            assert_eq!(census.idle, 0, "and it must not have entered the idle set");
            assert_eq!(census.leaked, 0, "a proven reap releases the slot");
            assert!(
                !paths.epoch_dir.exists(),
                "the tree goes with it: a launch proof that did not stick is a \
                 mint failure, and a mint failure erases"
            );
            live.pool
                .check_invariants()
                .await
                .expect("the pool is coherent after the teardown");
        }
    }
}
