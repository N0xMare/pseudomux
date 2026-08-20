use std::collections::HashMap;
use std::sync::Arc;

use pseudomux_protocol::v1::{
    CancelTurnRequest, CancelTurnResult, CloseSessionRequest, CloseSessionResult,
    CompatibilityReport, ErrorBody, ErrorCode, EventBatch, InspectSessionRequest,
    MAX_SAFE_JSON_INTEGER, MAX_SUBSCRIBE_EVENTS, MAX_SUBSCRIBE_WAIT_MS, NeedsInput, RunTurnRequest,
    SessionAgentPin, SessionCell, SessionGenerationId, SessionHandle, SessionId, SessionSnapshot,
    SubscribeEventsRequest, TurnAccepted, TurnId, validate_v1_serializable,
};
use tokio::sync::RwLock;

use super::actor::{
    ActorInit, SessionActorConfig, SessionActorHandle, StoredTurnTerminal,
    WritableAttachCompletion, require_tested_for_minified_cell,
};
use super::backend::{Clock, DriverFailure, SystemClock, TerminalControl, TranscriptSource};
use crate::tasks::TrackedTasks;

/// Who a registered session belongs to.
///
/// A session-addressed wire method may only reach a [`Self::Caller`] session,
/// and the pool may only reach a [`Self::Pool`] one. Both directions are
/// enforced by `SessionRegistry::actor_owned` -- private, so deliberately not an
/// intra-doc link from public documentation -- which is the single site the
/// question is asked, so neither can be widened by adding a method.
///
/// The refusal a wire caller gets for a pool instance is byte-identical to the
/// one it gets for a session that does not exist, and that is the point rather
/// than an accident. A distinguishable refusal -- `permission_denied`,
/// `session_busy`, anything that says "this exists but is not yours" -- is an
/// oracle: it lets a caller enumerate the pool's session ids by asking, and a
/// caller who can learn a resource's name is one step from aliasing it. The
/// whole product statement of Path B is that the caller names no resource.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SessionOwner {
    /// Historical owner tag. Public wire session methods refuse; pool uses
    /// [`Self::Pool`]. Still reachable by the generic idle reaper.
    Caller,
    /// Minted by the stateless pool. Unreachable by every session-addressed
    /// method, and excluded from the generic idle reaper positively -- the pool
    /// runs its own TTL sweep, and two reapers over one session would race the
    /// pool's own teardown.
    Pool,
}

pub struct SessionRegistration {
    pub session_id: SessionId,
    pub generation_id: SessionGenerationId,
    /// Who may address this session. See [`SessionOwner`].
    ///
    /// Deliberately a required field with no default. A default would make
    /// every registration site that forgets it a `Caller` session, and the site
    /// that forgets it is exactly the pool's.
    pub owner: SessionOwner,
    pub cwd: String,
    pub compatibility: CompatibilityReport,
    /// The Claude process behind this session was launched with
    /// `--dangerously-skip-permissions`.
    pub dangerous_permission_bypass: bool,
    pub resumable: bool,
    /// The cell this session is driven as. `SessionCell::Minified` is admitted
    /// only on a tested compatibility profile AND only over a transcript that
    /// [`TranscriptSource::assert_empty_at_launch`] proves has served no work;
    /// `register` refuses otherwise, before any actor exists, so neither rule
    /// can be widened by a registry caller.
    pub cell: SessionCell,
    /// The stored agent version this session resolved and pinned at start, when
    /// it named one. See `ActorInit::agent`.
    pub agent: Option<SessionAgentPin>,
    /// Overrides the default 30-minute atomically enforced idle TTL.
    pub idle_ttl_ms: Option<u64>,
    /// A recognized startup screen retained for interactive user resolution.
    pub initial_needs_input: Option<NeedsInput>,
    pub terminal: Arc<dyn TerminalControl>,
    pub transcript: Arc<dyn TranscriptSource>,
}

/// Concurrent registry of stable session IDs to one-owner session actors.
pub struct SessionRegistry {
    /// The owner travels IN the map, beside the handle, rather than in a second
    /// map keyed by the same id. Two maps can disagree, and the disagreement a
    /// second map admits is precisely "a pool instance whose owner entry was
    /// dropped", which reads as a caller session.
    actors: RwLock<HashMap<SessionId, (SessionOwner, SessionActorHandle)>>,
    config: SessionActorConfig,
    clock: Arc<dyn Clock>,
    detached_tasks: Arc<TrackedTasks>,
}

impl SessionRegistry {
    #[must_use]
    pub fn new(config: SessionActorConfig) -> Self {
        Self::with_clock(config, Arc::new(SystemClock))
    }

    #[must_use]
    pub fn with_clock(config: SessionActorConfig, clock: Arc<dyn Clock>) -> Self {
        Self {
            actors: RwLock::new(HashMap::new()),
            config,
            clock,
            detached_tasks: Arc::new(TrackedTasks::default()),
        }
    }

    /// Binds the accounting that an owner's shutdown fence will wait on.
    ///
    /// Every actor registered afterwards registers its detached
    /// `close(Force)` here, so an owner that stops accepting work and then
    /// awaits this fence cannot finish while a kill request it started is
    /// still in flight. Registries built without it get their own private
    /// counter, which keeps the permit accounting correct for tests and for
    /// embedders that have no shutdown sequence to fence.
    #[must_use]
    pub fn with_detached_tasks(mut self, detached_tasks: Arc<TrackedTasks>) -> Self {
        self.detached_tasks = detached_tasks;
        self
    }

    pub async fn register(
        &self,
        registration: SessionRegistration,
    ) -> Result<SessionHandle, ErrorBody> {
        validate_registration_domain(&registration)?;
        // The launch half of assert-empty, at the admission boundary rather than
        // in the wire path. It is the same argument `SessionActor::spawn` makes
        // for the require-tested rule one line further in: this registry is
        // `pub`, so a rule that lives only in `start_session_owned_with_retention` is a
        // rule a direct embedder does not get -- and, more to the point, one
        // whose deletion no test that can run without a real Claude would
        // notice. Refusing here means no actor is created and no state is
        // published, and it is the same refusal the wire path returns.
        //
        // Before the actor lock, deliberately: the proof reads a file, and
        // `register`'s cancellation-free actor/metadata publication segment
        // begins at the acquisition below.
        //
        // The compatibility rule runs first because it is a statement about the
        // request that costs nothing, while the emptiness proof reads a file: an
        // inadmissible profile should not pay for I/O to be told so, and the two
        // refusals must not race to name the same session.
        if registration.cell == SessionCell::Minified {
            require_tested_for_minified_cell(&registration.compatibility)?;
            registration
                .transcript
                .assert_empty_at_launch(registration.session_id)
                .await
                .map_err(DriverFailure::into_protocol)?;
        }
        let mut actors = self.actors.write().await;
        if actors.contains_key(&registration.session_id) {
            return Err(ErrorBody::new(
                ErrorCode::IdCollision,
                format!("session {} is already registered", registration.session_id),
            ));
        }
        let init = ActorInit {
            session_id: registration.session_id,
            generation_id: registration.generation_id,
            cwd: registration.cwd,
            compatibility: registration.compatibility,
            dangerous_permission_bypass: registration.dangerous_permission_bypass,
            resumable: registration.resumable,
            cell: registration.cell,
            agent: registration.agent,
            idle_ttl_ms: registration.idle_ttl_ms,
            initial_needs_input: registration.initial_needs_input,
            terminal: registration.terminal,
            transcript: registration.transcript,
            detached_tasks: Arc::clone(&self.detached_tasks),
        };
        let (actor, handle) =
            SessionActorHandle::spawn_actor(init, self.config.clone(), Arc::clone(&self.clock))?;
        actors.insert(registration.session_id, (registration.owner, actor));
        Ok(handle)
    }

    pub async fn run_turn(&self, request: RunTurnRequest) -> Result<TurnAccepted, ErrorBody> {
        self.actor(request.session_id, request.generation_id)
            .await?
            .submit_turn(request.turn)
            .await
    }

    pub async fn cancel_turn(
        &self,
        request: CancelTurnRequest,
    ) -> Result<CancelTurnResult, ErrorBody> {
        self.actor(request.session_id, request.generation_id)
            .await?
            .cancel_turn(request.turn_id)
            .await
    }

    pub async fn inspect(
        &self,
        request: InspectSessionRequest,
    ) -> Result<SessionSnapshot, ErrorBody> {
        self.actor(request.session_id, request.generation_id)
            .await?
            .snapshot()
            .await
    }

    pub async fn events(&self, request: SubscribeEventsRequest) -> Result<EventBatch, ErrorBody> {
        validate_event_subscription_request(&request)?;
        self.actor(request.session_id, request.generation_id)
            .await?
            .events(request.after_sequence, request.wait_ms, request.max_events)
            .await
    }

    pub async fn close(
        &self,
        request: CloseSessionRequest,
    ) -> Result<CloseSessionResult, ErrorBody> {
        self.close_as(SessionOwner::Caller, request).await
    }

    /// Close, resolved under an explicit owner.
    ///
    /// The pool tears its own instances down, and [`Self::close`] cannot serve
    /// it: that resolver refuses a pool instance, which is the property this
    /// whole split exists for.
    pub(crate) async fn close_as(
        &self,
        owner: SessionOwner,
        request: CloseSessionRequest,
    ) -> Result<CloseSessionResult, ErrorBody> {
        self.actor_owned(request.session_id, request.generation_id, owner)
            .await?
            .close(request.policy)
            .await
    }

    /// Atomically reserves interactive input for one writable attach proxy.
    pub async fn reserve_writable_attach(
        &self,
        session_id: SessionId,
        generation_id: SessionGenerationId,
        attach_id: uuid::Uuid,
    ) -> Result<(), ErrorBody> {
        self.actor(session_id, generation_id)
            .await?
            .reserve_writable_attach(attach_id)
            .await
    }

    /// Releases the matching writable attach reservation.
    pub async fn release_writable_attach(
        &self,
        session_id: SessionId,
        generation_id: SessionGenerationId,
        attach_id: uuid::Uuid,
        completion: WritableAttachCompletion,
    ) -> Result<(), ErrorBody> {
        self.actor(session_id, generation_id)
            .await?
            .release_writable_attach(attach_id, completion)
            .await
    }

    pub async fn stored_turn(
        &self,
        session_id: SessionId,
        generation_id: SessionGenerationId,
        turn_id: TurnId,
    ) -> Result<Option<StoredTurnTerminal>, ErrorBody> {
        self.actor(session_id, generation_id)
            .await?
            .stored_turn(turn_id)
            .await
    }

    /// Atomically closes a non-running, unattached session past its idle deadline.
    ///
    /// Resolves through [`Self::actor`], so a pool instance is refused here for
    /// exactly the reason it is refused everywhere else. That refusal is what
    /// makes the generic reaper decline pool sessions POSITIVELY rather than by
    /// never happening to be handed one: the pool runs its own TTL sweep, and a
    /// second reaper closing an instance the pool believes is idle would race
    /// the pool's own teardown and leave a slot holding a destroyed process.
    pub async fn expire_idle(
        &self,
        session_id: SessionId,
        generation_id: SessionGenerationId,
        now_ms: u64,
    ) -> Result<Option<CloseSessionResult>, ErrorBody> {
        self.actor(session_id, generation_id)
            .await?
            .expire_idle(now_ms)
            .await
    }

    /// The session-addressed resolver every wire method goes through.
    ///
    /// Admits [`SessionOwner::Caller`] only. A pool instance is refused here
    /// with the not-found body a session that never existed gets; see
    /// [`SessionOwner`] for why the two must be indistinguishable.
    pub async fn actor(
        &self,
        session_id: SessionId,
        generation_id: SessionGenerationId,
    ) -> Result<SessionActorHandle, ErrorBody> {
        self.actor_owned(session_id, generation_id, SessionOwner::Caller)
            .await
    }

    /// The pool's resolver. Admits [`SessionOwner::Pool`] only.
    ///
    /// The mirror image of [`Self::actor`], and mirrored on purpose: a single
    /// resolver with an `Option<SessionOwner>` filter would have an
    /// admits-everything value, and the whole property here is that no caller
    /// has one.
    pub(crate) async fn pool_actor(
        &self,
        session_id: SessionId,
        generation_id: SessionGenerationId,
    ) -> Result<SessionActorHandle, ErrorBody> {
        self.actor_owned(session_id, generation_id, SessionOwner::Pool)
            .await
    }

    async fn actor_owned(
        &self,
        session_id: SessionId,
        generation_id: SessionGenerationId,
        expected: SessionOwner,
    ) -> Result<SessionActorHandle, ErrorBody> {
        let not_found = || {
            ErrorBody::new(
                ErrorCode::SessionNotFound,
                format!("session {session_id} is not registered"),
            )
        };
        let (owner, actor) = self
            .actors
            .read()
            .await
            .get(&session_id)
            .cloned()
            .ok_or_else(not_found)?;
        // Before the generation fence, deliberately. A stale-generation body
        // names the session and therefore confirms it exists, so answering it
        // for a pool instance would rebuild the oracle the owner check removes.
        if owner != expected {
            return Err(not_found());
        }
        if actor.generation_id() != generation_id {
            return Err(stale_generation(session_id, generation_id));
        }
        Ok(actor)
    }

    /// Removes an actor after its terminal has been closed and reaped. This is
    /// required so the same Claude UUID can later be resumed in a fresh pane.
    pub async fn unregister(
        &self,
        session_id: SessionId,
        generation_id: SessionGenerationId,
    ) -> Result<(), ErrorBody> {
        let mut actors = self.actors.write().await;
        let Some((_, actor)) = actors.get(&session_id) else {
            return Ok(());
        };
        if actor.generation_id() != generation_id {
            return Err(stale_generation(session_id, generation_id));
        }
        actors.remove(&session_id);
        Ok(())
    }
}

fn validate_event_subscription_request(request: &SubscribeEventsRequest) -> Result<(), ErrorBody> {
    validate_v1_serializable(request).map_err(|_| {
        ErrorBody::new(
            ErrorCode::InvalidConfig,
            "event subscription cannot be represented within protocol v1",
        )
    })?;
    if request.wait_ms > MAX_SUBSCRIBE_WAIT_MS || request.max_events > MAX_SUBSCRIBE_EVENTS {
        return Err(ErrorBody::new(
            ErrorCode::InvalidConfig,
            "event subscription exceeds the public wait or batch bound",
        ));
    }
    Ok(())
}

fn validate_registration_domain(registration: &SessionRegistration) -> Result<(), ErrorBody> {
    validate_v1_serializable(&registration.compatibility).map_err(|_| {
        ErrorBody::new(
            ErrorCode::InvalidConfig,
            "session compatibility is outside the protocol-v1 wire domain",
        )
    })?;
    crate::compatibility::validate_v1_terminal_support(
        registration.compatibility.terminal_profile,
        registration.compatibility.input_transport,
    )?;
    if registration
        .idle_ttl_ms
        .is_some_and(|idle_ttl_ms| idle_ttl_ms > MAX_SAFE_JSON_INTEGER)
    {
        return Err(ErrorBody::new(
            ErrorCode::InvalidConfig,
            "session idle TTL is outside the protocol-v1 safe-integer domain",
        ));
    }
    if let Some(needs_input) = &registration.initial_needs_input {
        validate_v1_serializable(needs_input).map_err(|_| {
            ErrorBody::new(
                ErrorCode::InvalidConfig,
                "initial needs-input state is outside the protocol-v1 wire domain",
            )
        })?;
    }
    Ok(())
}

fn stale_generation(session_id: SessionId, requested: SessionGenerationId) -> ErrorBody {
    ErrorBody::new(
        ErrorCode::StaleSessionGeneration,
        format!("session {session_id} no longer refers to the requested process generation"),
    )
    .with_details(serde_json::json!({
        "requested_generation_id": requested,
    }))
}
