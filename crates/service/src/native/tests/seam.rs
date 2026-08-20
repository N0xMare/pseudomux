//! The guards that need a whole `NativeService` and no live rmux at all.
//!
//! # Why this file exists
//!
//! A full-scope mutation run left a block of survivors in `native.rs` whose
//! shared cause was not weak assertions but unreachability: every one of them
//! is behind a method on `NativeService`, `NativeService` held an
//! `Arc<PrivateRuntime>`, and a `PrivateRuntime` cannot exist without a real
//! `pmux-rmuxd` sidecar, a real launcher socket and a completed rmux handshake.
//! The only tests that built one were the Path A lanes in
//! `crates/service/tests/native_service.rs`, now removed. So the completion proof
//! (`wait_for_turn`), generation fences (`clear_boundary`,
//! `close_session_with_state`), the idle reaper, the pool-disclosure filter in
//! `diagnose`, the clear deadline domain and `shutdown`'s first-error rule were
//! all untested -- not lightly tested, untested -- and a mutation run is how
//! that was discovered rather than argued.
//!
//! [`crate::runtime::SessionRuntime`] is the seam that fixed it, and
//! [`crate::runtime::ScriptedRuntime`] is the double: it answers what a test
//! scripted and refuses everything else by name, so a guard sitting above it
//! can still FAIL. Each test below states the mutation it refuses in its own
//! name; `evidence/mutation-survivor-register.json` holds the rows.
//!
//! # What a test here may and may not use
//!
//! Everything except a live runtime is real: the real `SessionRegistry`, real
//! `SessionActor`s, the real `RmuxTerminalControl` over a scripted
//! `TerminalSession`, and a real `FileTranscriptSource` over a real temporary
//! directory. The double stops exactly at the process boundary.

use super::*;
use crate::runtime::ScriptedRuntime;
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;

/// A `NativeService` with no live rmux behind it, and the handles a test needs
/// to reach into what it published.
struct Seam {
    service: Arc<NativeService>,
    runtime: Arc<ScriptedRuntime>,
    /// Config root and cwd for every transcript and sensitive-launch file this
    /// service's sessions own. Dropped with the seam, so a test leaves nothing.
    root: TempDir,
    lifecycle_tasks: Arc<TrackedTasks>,
}

impl Seam {
    fn build() -> Self {
        Self::with_config(seam_config())
    }

    fn with_config(config: NativeServiceConfig) -> Self {
        let root = tempfile::Builder::new()
            .prefix("pmux-seam-")
            .tempdir()
            .unwrap();
        let runtime = Arc::new(ScriptedRuntime::new());
        let service = Arc::new(NativeService::from_runtime(
            Arc::clone(&runtime) as Arc<dyn SessionRuntime>,
            config,
        ));
        Self {
            service,
            runtime,
            root,
            lifecycle_tasks: Arc::new(TrackedTasks::default()),
        }
    }

    /// One published session: an actor in the registry and the metadata entry
    /// beside it, both holding the SAME terminal, exactly as a real start does.
    async fn publish(
        &self,
        session_id: SessionId,
        generation_id: SessionGenerationId,
        owner: SessionOwner,
        terminal: Arc<RmuxTerminalControl>,
    ) {
        let transcript = Arc::new(
            FileTranscriptSource::new(self.root.path(), self.root.path(), session_id).unwrap(),
        );
        self.service
            .registry
            .register(SessionRegistration {
                agent: None,
                session_id,
                generation_id,
                owner,
                cwd: self.root.path().to_string_lossy().into_owned(),
                compatibility: seam_compatibility(),
                dangerous_permission_bypass: false,
                resumable: true,
                cell: SessionCell::Full,
                idle_ttl_ms: Some(0),
                initial_needs_input: None,
                terminal: Arc::clone(&terminal) as Arc<dyn TerminalControl>,
                transcript: Arc::clone(&transcript) as Arc<dyn TranscriptSource>,
            })
            .await
            .unwrap();
        self.insert_metadata(session_id, generation_id, owner, terminal, transcript)
            .await;
    }

    /// The metadata half alone, for the fences that read the map under a
    /// generation the registry does not have.
    async fn insert_metadata(
        &self,
        session_id: SessionId,
        generation_id: SessionGenerationId,
        owner: SessionOwner,
        terminal: Arc<RmuxTerminalControl>,
        transcript: Arc<FileTranscriptSource>,
    ) {
        self.service.sessions.write().await.insert(
            session_id,
            SessionMetadata {
                generation_id,
                owner,
                terminal,
                transcript,
                private_session_name: format!("pmux-seam-{session_id}"),
                cell: SessionCell::Full,
                _sensitive_launch: empty_sensitive_launch(self.root.path(), session_id),
                _lifecycle: lifecycle_with_probe(
                    Arc::new(AtomicBool::new(false)),
                    &self.lifecycle_tasks,
                ),
            },
        );
    }

    async fn published_sessions(&self) -> Vec<SessionId> {
        let mut ids: Vec<_> = self.service.sessions.read().await.keys().copied().collect();
        ids.sort();
        ids
    }
}

/// The service configuration every seam test starts from: a real one with the
/// waits shortened, so a test that hangs fails instead of taking ten minutes.
fn seam_config() -> NativeServiceConfig {
    NativeServiceConfig {
        actor: SessionActorConfig {
            poll_interval: Duration::from_millis(1),
            cancel_recovery_timeout: Duration::from_millis(50),
            attach_reconciliation_timeout: Duration::from_millis(50),
            default_turn_timeout_ms: 5_000,
            ..SessionActorConfig::default()
        },
        readiness_timeout: Duration::from_millis(250),
        // NOT shortened, unlike the waits above: this one bounds a real
        // `claude --version` spawn, and a seam test that runs beside a mutation
        // job on a loaded host measured 250 ms as a flake. A timeout whose only
        // job is to bound a hang costs nothing by being generous.
        version_timeout: Duration::from_secs(10),
        ..NativeServiceConfig::default()
    }
}

fn seam_compatibility() -> CompatibilityReport {
    CompatibilityReport {
        claude_version: "9.9.9".to_owned(),
        os: std::env::consts::OS.to_owned(),
        arch: std::env::consts::ARCH.to_owned(),
        terminal_profile: TerminalProfile::Transparent,
        input_transport: InputTransport::Sdk,
        tested: true,
        transcript_drain_ms: 1,
    }
}

/// What one `close` on a seam terminal answers.
#[derive(Clone, Copy)]
enum SeamClose {
    /// The pane closed and the Claude process was positively reaped.
    Reaped,
    /// The pane closed and the process was NOT proven reaped, which is the
    /// input `require_process_reaped` exists to refuse.
    Unreaped,
}

/// The order in which a set of seam terminals were closed.
///
/// A terminal cannot say WHICH close failed by putting a marker in its error:
/// every rmux failure reaching a caller is deliberately content-free ("private
/// rmux operation failed"), which is a property worth keeping. So the order is
/// recorded here, by the terminals themselves, and the test reads "the session
/// closed first" out of it rather than assuming a `HashMap` order it is not
/// allowed to know.
type CloseOrder = Arc<StdMutex<Vec<SessionId>>>;

/// A terminal that refuses every operation except the one close a test scripts.
///
/// Deliberately not `FakeStartupTerminal`: that double answers `snapshot`,
/// because it exists to script startup screens. A seam test is never about a
/// screen, and a terminal that answers reads it was never asked to answer is
/// how a test starts passing for a reason it did not state. The refusals are
/// what let a guard above this fail.
struct SeamTerminal {
    reference: BackendSessionRef,
    close: SeamClose,
    gate: Option<Arc<CloseGate>>,
    /// Which session this terminal belongs to, and where to record that it was
    /// closed. `None` for the tests that never ask about close order.
    recorder: Option<(SessionId, CloseOrder)>,
    /// Counts closes for a test that holds no other handle on this terminal --
    /// an unpublished startup pane, which the service owns and never returns.
    closes: Option<Arc<AtomicUsize>>,
}

/// Holds one close open, so a test can change the state a caller reads on the
/// far side of its `await`.
///
/// The reaper reads the session map, awaits an expiry that closes a terminal,
/// and reads the map again; the fence between those two reads is only
/// observable if something moves in between. This is that something.
#[derive(Default)]
struct CloseGate {
    entered: Notify,
    release: Notify,
}

impl CloseGate {
    /// Returns once a close has begun and is waiting to be released.
    async fn wait_until_entered(&self) {
        tokio::time::timeout(Duration::from_secs(5), self.entered.notified())
            .await
            .expect("a gated close was never reached");
    }
}

impl SeamTerminal {
    fn new(close: SeamClose) -> Self {
        Self {
            reference: BackendSessionRef {
                rmux_session_name: "pmux-seam".to_owned(),
                pane_id: 1,
            },
            close,
            gate: None,
            recorder: None,
            closes: None,
        }
    }

    fn gated(close: SeamClose, gate: Arc<CloseGate>) -> Self {
        Self {
            gate: Some(gate),
            ..Self::new(close)
        }
    }

    fn counting_closes(close: SeamClose, closes: &Arc<AtomicUsize>) -> Self {
        Self {
            closes: Some(Arc::clone(closes)),
            ..Self::new(close)
        }
    }

    fn recording(close: SeamClose, session_id: SessionId, order: &CloseOrder) -> Self {
        Self {
            recorder: Some((session_id, Arc::clone(order))),
            ..Self::new(close)
        }
    }

    fn refused(operation: &str) -> TerminalBackendError {
        TerminalBackendError::Rmux(format!("a seam terminal was asked to {operation}"))
    }
}

#[async_trait]
impl TerminalSession for SeamTerminal {
    fn backend_ref(&self) -> &BackendSessionRef {
        &self.reference
    }

    fn lease_lost(&self) -> bool {
        false
    }

    async fn snapshot(&self) -> Result<TerminalSnapshot, TerminalBackendError> {
        Err(Self::refused("render a screen"))
    }

    async fn styled_screen(&self) -> Result<pseudomux_rmux::StyledScreen, TerminalBackendError> {
        Err(Self::refused("render a styled screen"))
    }

    async fn wait_visible_text(
        &self,
        _needle: &str,
        _timeout: Duration,
    ) -> Result<TerminalSnapshot, TerminalBackendError> {
        Err(Self::refused("wait for visible text"))
    }

    async fn wait_quiet(
        &self,
        _stable_for: Duration,
        _timeout: Duration,
    ) -> Result<TerminalSnapshot, TerminalBackendError> {
        Err(Self::refused("wait for quiet"))
    }

    async fn paste(&mut self, _text: &str) -> Result<(), TerminalBackendError> {
        Err(Self::refused("paste"))
    }

    async fn enter(&mut self) -> Result<(), TerminalBackendError> {
        Err(Self::refused("press enter"))
    }

    async fn interrupt(&mut self) -> Result<(), TerminalBackendError> {
        Err(Self::refused("interrupt"))
    }

    async fn resize(&mut self, _rows: u16, _cols: u16) -> Result<(), TerminalBackendError> {
        Err(Self::refused("resize"))
    }

    async fn close(&mut self) -> Result<bool, TerminalBackendError> {
        if let Some(closes) = &self.closes {
            closes.fetch_add(1, Ordering::SeqCst);
        }
        if let Some((session_id, order)) = &self.recorder {
            order.lock().unwrap().push(*session_id);
        }
        if let Some(gate) = &self.gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
        match self.close {
            SeamClose::Reaped => Ok(true),
            SeamClose::Unreaped => Ok(false),
        }
    }
}

/// A terminal whose close outcome the test decides, wrapped in the real
/// `RmuxTerminalControl` the service stores.
fn seam_terminal(close: SeamClose) -> Arc<RmuxTerminalControl> {
    Arc::new(RmuxTerminalControl::new(Box::new(SeamTerminal::new(close))))
}

/// The same, recording the order in which a set of sessions were closed.
fn recording_seam_terminal(
    close: SeamClose,
    session_id: SessionId,
    order: &CloseOrder,
) -> Arc<RmuxTerminalControl> {
    Arc::new(RmuxTerminalControl::new(Box::new(SeamTerminal::recording(
        close, session_id, order,
    ))))
}

/// The same, with its close held open until the test releases it.
fn gated_seam_terminal(close: SeamClose, gate: &Arc<CloseGate>) -> Arc<RmuxTerminalControl> {
    Arc::new(RmuxTerminalControl::new(Box::new(SeamTerminal::gated(
        close,
        Arc::clone(gate),
    ))))
}

/// A clear of the transcript the session was launched on, which is the fence
/// value every first clear carries.
fn seam_clear_request(
    session_id: SessionId,
    generation_id: SessionGenerationId,
    deadline_unix_ms: Option<u64>,
) -> ClearSessionRequest {
    ClearSessionRequest {
        session_id,
        generation_id,
        expected_transcript_session_id: session_id,
        deadline_unix_ms,
    }
}

// ---------------------------------------------------------------------------
// The generation fences
// ---------------------------------------------------------------------------

/// Register: `native.rs::clear_boundary`, `replace == with !=`.
///
/// `/clear` types into a terminal and rebinds a transcript, and the pair has to
/// belong to the incarnation the caller named. Read as `!=` the fence hands
/// over the pair of a DIFFERENT process generation and refuses the right one.
#[tokio::test]
async fn a_clear_boundary_is_the_pair_of_the_generation_the_caller_named() {
    let seam = Seam::build();
    let session_id = SessionId::new_v4();
    let live = SessionGenerationId::new();
    let superseded = SessionGenerationId::new();
    assert_ne!(live, superseded);
    seam.publish(
        session_id,
        live,
        SessionOwner::Caller,
        seam_terminal(SeamClose::Reaped),
    )
    .await;

    assert!(
        seam.service.clear_boundary(session_id, live).await.is_ok(),
        "the live generation owns this session's terminal and transcript"
    );

    let Err(refusal) = seam.service.clear_boundary(session_id, superseded).await else {
        panic!("a generation this session does not have owns nothing to clear");
    };
    assert_eq!(refusal.code, ErrorCode::StaleSessionGeneration);
}

// ---------------------------------------------------------------------------
// The clear deadline's domain
// ---------------------------------------------------------------------------

/// Register: `native.rs::clear_session`, `replace > with <`, `with ==`,
/// `with >=`, all three at `deadline_unix_ms > MAX_SAFE_JSON_INTEGER`.
///
/// The guard has two sides and each mutation breaks one of them, so the test
/// states both against the same service:
///
/// * a deadline AT the top of the safe-integer domain is admitted -- it reaches
///   the actor, which refuses it for a reason that has nothing to do with
///   deadlines -- which is what `>=` and `==` break;
/// * a deadline the service SYNTHESISES past the top is refused, which is what
///   `<` and `==` break.
///
/// The synthesised half is the only way the refusal is reachable at all:
/// `deadline_unix_ms` is serialised through `optional_safe_u64`, so a wire
/// caller cannot present a value above the domain -- `validate_native_request`
/// refuses it first. `default_clear_timeout_ms` is not on the wire, and
/// `now + timeout` saturates.
#[tokio::test]
async fn the_clear_deadline_domain_admits_its_own_top_and_refuses_a_synthesised_deadline_past_it() {
    let session_id = SessionId::new_v4();
    let generation_id = SessionGenerationId::new();

    let seam = Seam::build();
    seam.publish(
        session_id,
        generation_id,
        SessionOwner::Caller,
        seam_terminal(SeamClose::Reaped),
    )
    .await;
    let Err(admitted) = seam
        .service
        .clear_session_internal(seam_clear_request(
            session_id,
            generation_id,
            Some(MAX_SAFE_JSON_INTEGER),
        ))
        .await
    else {
        panic!("a full-cell session cannot be cleared, so no clear here can succeed");
    };
    // Past the deadline guard and refused by the actor instead: this session is
    // not a minified cell. The point is WHICH refusal, not that it refused.
    assert_eq!(admitted.code, ErrorCode::UnsupportedFeature);

    let mut config = seam_config();
    config.default_clear_timeout_ms = MAX_SAFE_JSON_INTEGER;
    let saturating = Seam::with_config(config);
    saturating
        .publish(
            session_id,
            generation_id,
            SessionOwner::Caller,
            seam_terminal(SeamClose::Reaped),
        )
        .await;
    let Err(refused) = saturating
        .service
        .clear_session_internal(seam_clear_request(session_id, generation_id, None))
        .await
    else {
        panic!("a deadline outside the wire domain cannot be handed to an actor");
    };
    assert_eq!(refused.code, ErrorCode::InvalidConfig);
    assert!(
        refused.message.contains("safe-integer domain"),
        "the refusal must name the domain it is about: {}",
        refused.message
    );
}

/// Register: `native.rs::clear_timeout_ms`, `replace -> u64 with 0` and
/// `with 1`.
///
/// The deadline a `/clear` gets when its caller supplies none. Read as `0` or
/// `1`, every pooled clear is handed a deadline it cannot meet.
///
/// **This states the accessor and not its use.** The pool's clear
/// (`stateless.rs::clear_pool_instance`) is the only caller, and the deadline
/// it computes is first read past `clear_and_rebind`'s transcript watch -- so
/// observing it needs a rotation-capable transcript on disk, which is a live
/// Claude. What is asserted here is the property both mutations break: the
/// value is READ FROM THE CONFIGURATION rather than being a constant.
#[tokio::test]
async fn the_default_clear_deadline_is_read_from_the_configuration() {
    let mut config = seam_config();
    // Neither of the two constants a mutation replaces this with, and not the
    // shipped default either, so the assertion cannot pass by coincidence.
    config.default_clear_timeout_ms = 7_919;
    let seam = Seam::with_config(config);

    assert_eq!(seam.service.clear_timeout_ms(), 7_919);
}

// ---------------------------------------------------------------------------
// The generic idle reaper
// ---------------------------------------------------------------------------

/// Register: `native.rs::reap_idle_sessions`, `replace == with !=` at
/// `metadata.owner == SessionOwner::Caller`.
///
/// The reaper skips the pool's instances AT THE ENUMERATION, positively, and
/// the comment above the filter says why: relying on `expire_idle` to refuse
/// them means the day a second close path appears, the generic reaper starts
/// tearing down instances the pool believes it owns. Read as `!=` the reaper
/// enumerates exactly the sessions it must not touch and skips the ones it
/// exists to expire.
#[tokio::test]
async fn the_idle_reaper_expires_caller_sessions_and_enumerates_no_pool_instance() {
    let seam = Seam::build();
    let caller = SessionId::new_v4();
    let caller_generation = SessionGenerationId::new();
    let pooled = SessionId::new_v4();
    let pooled_generation = SessionGenerationId::new();
    seam.publish(
        caller,
        caller_generation,
        SessionOwner::Caller,
        seam_terminal(SeamClose::Reaped),
    )
    .await;
    seam.publish(
        pooled,
        pooled_generation,
        SessionOwner::Pool,
        seam_terminal(SeamClose::Reaped),
    )
    .await;

    seam.service.reap_idle_sessions().await;

    assert_eq!(
        seam.published_sessions().await,
        vec![pooled],
        "the caller session was past its TTL and the pool's instance is not this reaper's"
    );
    assert!(
        seam.service
            .closed_sessions
            .read()
            .await
            .contains(caller, caller_generation)
    );
    // Still the pool's, and still addressable as the pool's: the reaper did not
    // merely fail to remove it, it never asked about it.
    assert!(
        seam.service
            .registry
            .pool_actor(pooled, pooled_generation)
            .await
            .is_ok()
    );
}

/// Register: `native.rs::reap_idle_sessions`, `delete !` at
/// `!result.process_reaped`.
///
/// An expiry that could not prove the Claude process reaped leaves the session
/// registered, because unregistering it is what makes the same Claude UUID
/// resumable in a fresh pane. With the `!` deleted the reaper does the opposite
/// of what it does now: it drops exactly the sessions whose process is still
/// alive and keeps the ones it proved dead.
#[tokio::test]
async fn a_session_whose_process_was_not_proven_reaped_survives_the_reaper() {
    let seam = Seam::build();
    let session_id = SessionId::new_v4();
    let generation_id = SessionGenerationId::new();
    seam.publish(
        session_id,
        generation_id,
        SessionOwner::Caller,
        seam_terminal(SeamClose::Unreaped),
    )
    .await;

    seam.service.reap_idle_sessions().await;

    assert_eq!(
        seam.published_sessions().await,
        vec![session_id],
        "an unreaped process keeps its session's metadata alive"
    );
    assert!(
        !seam
            .service
            .closed_sessions
            .read()
            .await
            .contains(session_id, generation_id)
    );
}

/// Register: `native.rs::reap_idle_sessions`, `replace == with !=` at
/// `metadata.generation_id == generation_id`.
///
/// The reaper enumerates, awaits an expiry, and only then removes -- so the map
/// it removes from is not the map it read. The fence says: remove the entry
/// only if it is still the one this pass expired. Read as `!=` it removes any
/// entry EXCEPT that one, which is precisely the newly published successor of a
/// session that restarted while the pass was in flight.
///
/// The gate is what makes the window exist at all; without something held open
/// inside `expire_idle` there is no interleaving for a fence to be wrong about.
#[tokio::test]
async fn a_successor_published_while_the_reaper_was_in_flight_is_not_removed_by_it() {
    let seam = Seam::build();
    let session_id = SessionId::new_v4();
    let expiring = SessionGenerationId::new();
    let successor = SessionGenerationId::new();
    assert_ne!(expiring, successor);
    let gate = Arc::new(CloseGate::default());
    seam.publish(
        session_id,
        expiring,
        SessionOwner::Caller,
        gated_seam_terminal(SeamClose::Reaped, &gate),
    )
    .await;

    let service = Arc::clone(&seam.service);
    let reaping = tokio::spawn(async move { service.reap_idle_sessions().await });
    gate.wait_until_entered().await;

    // The restart: the same session id, published again under a NEWER
    // generation, while the reaper is inside the expiry of the older one.
    let transcript = Arc::new(
        FileTranscriptSource::new(seam.root.path(), seam.root.path(), session_id).unwrap(),
    );
    seam.insert_metadata(
        session_id,
        successor,
        SessionOwner::Caller,
        seam_terminal(SeamClose::Reaped),
        transcript,
    )
    .await;
    gate.release.notify_one();
    reaping.await.unwrap();

    assert_eq!(
        seam.published_sessions().await,
        vec![session_id],
        "the successor generation is not what this pass expired"
    );
    assert_eq!(
        seam.service
            .sessions
            .read()
            .await
            .get(&session_id)
            .map(|metadata| metadata.generation_id),
        Some(successor)
    );
}

// ---------------------------------------------------------------------------
// The health report, which is a disclosure boundary
// ---------------------------------------------------------------------------

/// A pool with an empty warm set: real `Pool`, real host, real config, and not
/// one Claude process, because nothing declared a warm instance to mint.
async fn attach_empty_pool(seam: &Seam) {
    let parent_dir = seam.root.path().join("pool");
    std::fs::create_dir(&parent_dir).unwrap();
    std::fs::set_permissions(&parent_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = crate::pool::PoolSettings {
        warm_set: Vec::new(),
        ..crate::pool::PoolSettings::defaults(
            parent_dir,
            // Never launched here: nothing in this test mints an instance. It
            // is read by `admit_pool_claude`, which reports it unreadable.
            seam.root.path().join("no-such-claude"),
        )
    }
    .validate()
    .unwrap();
    seam.service.start_pool(config).await.unwrap();
}

fn pool_layer_evidence(diagnosis: &DaemonDiagnosis) -> serde_json::Value {
    diagnosis
        .layers
        .iter()
        .find(|layer| layer.layer == HealthLayerName::Pool)
        .expect("every layer name is reported, so the pool layer is always present")
        .evidence
        .clone()
}

/// Register: `native.rs::diagnose`, `replace == with !=` at
/// `metadata.owner == SessionOwner::Caller` and at
/// `metadata.owner == SessionOwner::Pool`; and `native.rs::pool`,
/// `replace -> Option<&Arc<Pool>> with None`.
///
/// `DaemonDiagnosis::sessions` publishes a session id per entry, and a pool
/// instance's session id is the one name `SessionOwner` exists to keep off the
/// wire. This is a disclosure boundary rather than a report detail, and it was
/// crossed live once already: the first health tree this daemon produced listed
/// a pool instance's session id and generation id.
///
/// The three mutations are one property read three ways, so they are stated
/// against one report:
///
/// * the caller filter read as `!=` puts the POOL's ids in `sessions`;
/// * the pool filter read as `!=` counts the CALLER's terminal as the pool's,
///   which the layer's `instance_terminals_present` shows because the sidecar
///   was scripted to report the pool's terminal and not the caller's;
/// * `pool()` read as `None` erases the census entirely and reports a daemon
///   that was given a pool as one that has none.
#[tokio::test]
async fn a_diagnosis_names_caller_sessions_and_counts_pool_instances_without_naming_them() {
    let seam = Seam::build();
    attach_empty_pool(&seam).await;
    let caller = SessionId::new_v4();
    let pooled = SessionId::new_v4();
    seam.publish(
        caller,
        SessionGenerationId::new(),
        SessionOwner::Caller,
        seam_terminal(SeamClose::Reaped),
    )
    .await;
    seam.publish(
        pooled,
        SessionGenerationId::new(),
        SessionOwner::Pool,
        seam_terminal(SeamClose::Reaped),
    )
    .await;
    // The sidecar reports the POOL's private terminal and not the caller's, so
    // "how many of the pool's terminals are live" separates the two owners by
    // more than a count of rows.
    seam.runtime
        .script_control_plane(Ok(BTreeSet::from([format!("pmux-seam-{pooled}")])));
    seam.runtime
        .script_launch_broker(true, crate::launch_broker::BrokerProbe::Exchanged);

    let diagnosis = seam.service.diagnose().await;

    assert_eq!(
        diagnosis
            .sessions
            .iter()
            .map(|probe| probe.session_id)
            .collect::<Vec<_>>(),
        vec![caller],
        "a pool instance's session id may not be published to any client"
    );
    let evidence = pool_layer_evidence(&diagnosis);
    assert_eq!(evidence["configured"], serde_json::json!(true));
    assert_eq!(evidence["registered_instances"], serde_json::json!(1));
    assert_eq!(
        evidence["instance_terminals_present"],
        serde_json::json!(1),
        "the pool's own terminal is the one the sidecar reported: {evidence}"
    );
}

// ---------------------------------------------------------------------------
// Close, and the daemon-wide close
// ---------------------------------------------------------------------------

/// Register: `native.rs::close_session_with_state`, `replace == with !=` at
/// `metadata.generation_id == request.generation_id`.
///
/// The same interleaving the reaper has, on the explicit close path: the close
/// reads the map only after its terminal has been closed, and by then the
/// session it named may have been re-published under a newer generation. Read
/// as `!=` the close removes the entry it did NOT name -- the live successor --
/// and leaves the one it did.
#[tokio::test]
async fn a_close_removes_only_the_generation_it_named() {
    let seam = Seam::build();
    let session_id = SessionId::new_v4();
    let closing = SessionGenerationId::new();
    let successor = SessionGenerationId::new();
    let gate = Arc::new(CloseGate::default());
    seam.publish(
        session_id,
        closing,
        SessionOwner::Caller,
        gated_seam_terminal(SeamClose::Reaped, &gate),
    )
    .await;

    let service = Arc::clone(&seam.service);
    let closing_call = tokio::spawn(async move {
        service
            .close_session_owned(
                SessionOwner::Caller,
                CloseSessionRequest {
                    session_id,
                    generation_id: closing,
                    policy: ClosePolicy::Force,
                },
            )
            .await
    });
    gate.wait_until_entered().await;
    let transcript = Arc::new(
        FileTranscriptSource::new(seam.root.path(), seam.root.path(), session_id).unwrap(),
    );
    seam.insert_metadata(
        session_id,
        successor,
        SessionOwner::Caller,
        seam_terminal(SeamClose::Reaped),
        transcript,
    )
    .await;
    gate.release.notify_one();

    let closed = closing_call.await.unwrap().unwrap();
    assert!(closed.process_reaped);
    assert_eq!(
        seam.service
            .sessions
            .read()
            .await
            .get(&session_id)
            .map(|metadata| metadata.generation_id),
        Some(successor),
        "the successor generation is not what this close named"
    );
}

/// Register: `native.rs::shutdown`, `replace match guard first_error.is_none()
/// with false` and `with true`.
///
/// `shutdown` force-closes every session it still holds and reports the FIRST
/// failure. Read as `false` the arm never runs, so a shutdown that could not
/// prove a process reaped returns success; read as `true` it runs every time,
/// so the answer becomes the LAST failure. Both are visible only from a
/// shutdown that meets more than one failing close, and neither is visible at
/// all without knowing which close ran first -- which the terminals stamp into
/// their own failures rather than the test assuming a map order.
#[tokio::test]
async fn a_shutdown_reports_the_first_close_failure_and_not_the_last() {
    let seam = Seam::build();
    let order: CloseOrder = Arc::new(StdMutex::new(Vec::new()));
    for _ in 0..2 {
        let session_id = SessionId::new_v4();
        seam.publish(
            session_id,
            SessionGenerationId::new(),
            SessionOwner::Caller,
            recording_seam_terminal(SeamClose::Unreaped, session_id, &order),
        )
        .await;
    }

    let Err(error) = seam.service.shutdown().await else {
        panic!("a shutdown that proved no process reaped cannot report success");
    };

    let closed = order.lock().unwrap().clone();
    assert_eq!(closed.len(), 2, "every held session is force-closed");
    assert_eq!(error.code, ErrorCode::RecoveryFailed);
    assert_eq!(
        error.details["session_id"],
        serde_json::json!(closed[0]),
        "the FIRST close failure is the one reported, and {:?} was closed first",
        closed[0]
    );
    assert_eq!(
        seam.runtime.shutdowns(),
        1,
        "the private runtime is stopped exactly once, after the sessions"
    );
}

/// Register: `native.rs::shutdown`, `replace match guard first_error.is_none()
/// with false` and `with true` -- the guard on the arm that records a PRIVATE
/// RUNTIME shutdown failure, not the one in the close loop above it.
///
/// Two mutations, two directions, and the arm can only be wrong in one of them
/// at a time, so each half of this test is the one that sees its own:
///
/// * read as `false` the arm never matches, the failure falls into `Err(_) =>
///   {}`, and a daemon whose private rmux sidecar could not be stopped reports
///   a clean shutdown;
/// * read as `true` it always matches, so the runtime's failure OVERWRITES the
///   first session close failure and the caller is told about the sidecar
///   instead of about the process that was never reaped.
#[tokio::test]
async fn a_private_runtime_that_will_not_stop_is_reported_unless_a_close_already_failed() {
    let quiet = Seam::build();
    quiet
        .runtime
        .script_shutdown_failure("the seam's sidecar refused to stop");

    let Err(error) = quiet.service.shutdown().await else {
        panic!("a runtime that could not be stopped is not a clean shutdown");
    };
    assert_eq!(error.code, ErrorCode::RmuxUnavailable);
    assert!(
        error.message.contains("the seam's sidecar refused to stop"),
        "the refusal carries the runtime's own cause: {}",
        error.message
    );

    let contested = Seam::build();
    contested
        .runtime
        .script_shutdown_failure("the seam's sidecar refused to stop");
    contested
        .publish(
            SessionId::new_v4(),
            SessionGenerationId::new(),
            SessionOwner::Caller,
            seam_terminal(SeamClose::Unreaped),
        )
        .await;

    let Err(error) = contested.service.shutdown().await else {
        panic!("an unreaped process is not a clean shutdown either");
    };
    assert_eq!(
        error.code,
        ErrorCode::RecoveryFailed,
        "the first failure was the unreaped process, and the runtime's own failure does not \
         replace it: {}",
        error.message
    );
}

// ---------------------------------------------------------------------------
// The completion proof
// ---------------------------------------------------------------------------

/// A terminal whose turn does not finish until the test lets it.
///
/// Everything after the gate is the ordinary happy path -- the screen is ready,
/// the evidence is complete -- because the property under test is what the
/// waiter does WHILE a turn is still running, and a turn that fails instantly
/// is one the waiter never has to wait for.
struct SeamTurnTerminal {
    gate: Arc<CloseGate>,
}

#[async_trait]
impl TerminalControl for SeamTurnTerminal {
    async fn submit_prompt(
        &self,
        _session_id: SessionId,
        _turn_id: pseudomux_protocol::v1::TurnId,
        _prompt: &str,
        _deadline_unix_ms: u64,
    ) -> DriverResult<()> {
        self.gate.entered.notify_one();
        self.gate.release.notified().await;
        Ok(())
    }

    async fn completion_evidence(
        &self,
        _session_id: SessionId,
        _turn_id: pseudomux_protocol::v1::TurnId,
    ) -> DriverResult<TerminalEvidence> {
        Ok(TerminalEvidence {
            ready_prompt: true,
            quiet: true,
            lifecycle_expected: false,
            lifecycle_hook_observed: false,
            lifecycle_hook_at_ms: None,
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
        _turn_id: pseudomux_protocol::v1::TurnId,
    ) -> DriverResult<InterruptRecovery> {
        Ok(InterruptRecovery::RecoveredToReady)
    }

    async fn close(&self, _session_id: SessionId, _policy: ClosePolicy) -> DriverResult<bool> {
        Ok(true)
    }
}

/// A transcript that is at EOF, stable, and holds nothing.
struct SeamTurnTranscript;

#[async_trait]
impl TranscriptSource for SeamTurnTranscript {
    async fn arm_at_eof(&self, _session_id: SessionId) -> DriverResult<TranscriptArm> {
        Ok(TranscriptArm::default())
    }

    async fn poll(
        &self,
        _session_id: SessionId,
        position: &TranscriptPosition,
    ) -> DriverResult<TranscriptBatch> {
        Ok(TranscriptBatch {
            position: position.clone(),
            rows: Vec::new(),
            drain: crate::v1::TranscriptDrainEvidence {
                at_eof: true,
                has_partial_line: false,
                stable_for_ms: u64::MAX,
            },
        })
    }
}

/// Register: `native.rs::wait_for_turn`, `replace >= with <` at
/// `Instant::now() >= safety_deadline`.
///
/// The safety guard is infrastructure: it exists to answer a caller whose actor
/// died without publishing anything, and it is deliberately outside the turn's
/// own deadline plus every recovery window, because it is NOT ALLOWED TO
/// PUBLISH A COMPETING TURN OUTCOME. Read as `<` it fires on the first
/// iteration of every wait, so no turn can ever complete and every caller is
/// told the daemon lost its actor -- which is the whole of Path B answering
/// `daemon_lost` on the happy path.
///
/// What is asserted is that the answer is the ACTOR's: this turn is held open
/// past its own deadline, so the actor publishes `turn_timeout`, and the guard
/// -- whose window is that deadline plus three recovery windows plus the drain
/// plus a second -- is nowhere near expiring when it does. A turn that had
/// already published would be returned by the check ABOVE the guard, and a wait
/// that never reaches the guard proves nothing about it.
#[tokio::test]
async fn a_turn_still_running_is_waited_for_rather_than_declared_lost() {
    let seam = Seam::build();
    let session_id = SessionId::new_v4();
    let generation_id = SessionGenerationId::new();
    let turn_id = uuid::Uuid::new_v4();
    let gate = Arc::new(CloseGate::default());
    seam.service
        .registry
        .register(SessionRegistration {
            agent: None,
            session_id,
            generation_id,
            owner: SessionOwner::Caller,
            cwd: seam.root.path().to_string_lossy().into_owned(),
            compatibility: seam_compatibility(),
            dangerous_permission_bypass: false,
            resumable: true,
            cell: SessionCell::Full,
            idle_ttl_ms: None,
            initial_needs_input: None,
            terminal: Arc::new(SeamTurnTerminal {
                gate: Arc::clone(&gate),
            }),
            transcript: Arc::new(SeamTurnTranscript),
        })
        .await
        .unwrap();
    let actor = seam
        .service
        .registry
        .actor(session_id, generation_id)
        .await
        .unwrap();
    // The turn's own deadline, short so the actor publishes an outcome
    // quickly. The safety guard is this plus three cancel-recovery windows plus
    // the drain plus a second, so the two cannot be confused for each other.
    let deadline_unix_ms = unix_now_ms().unwrap() + 300;
    actor
        .submit_turn(pseudomux_protocol::v1::TurnRequest {
            turn_id,
            prompt: "a turn the seam holds open".to_owned(),
            deadline_unix_ms: Some(deadline_unix_ms),
            lease: pseudomux_protocol::v1::TurnLeasePolicy::default(),
        })
        .await
        .unwrap();

    let releasing = tokio::spawn({
        let gate = Arc::clone(&gate);
        async move {
            gate.wait_until_entered().await;
            // Long enough that the waiter has certainly polled a turn that is
            // still running, which is the state the guard is asked about.
            tokio::time::sleep(Duration::from_millis(30)).await;
            gate.release.notify_one();
        }
    });
    let outcome = seam
        .service
        .wait_for_turn(&actor, turn_id, Some(deadline_unix_ms), 1)
        .await;
    releasing.await.unwrap();

    let Err(published) = outcome else {
        panic!("this turn produced no assistant output, so it cannot have succeeded");
    };
    assert_eq!(
        published.code,
        ErrorCode::TurnTimeout,
        "the caller is told what the ACTOR published about its turn, and the infrastructure \
         guard publishes nothing: {published:?}"
    );
}

// ---------------------------------------------------------------------------
// The clock every synthesized deadline is built from
// ---------------------------------------------------------------------------

/// Register: `native.rs::unix_now_ms`, `replace -> Result<u64, ErrorBody> with
/// Ok(0)` and `with Ok(1)`.
///
/// Every synthesized deadline in the daemon is `unix_now_ms() + something`: the
/// clear default above, the turn default in `wait_for_turn`, the attach
/// expiry. Read as a constant, every one of them is computed from the epoch and
/// is already in the past. It needs no seam -- two clock reads bracket it --
/// and it is closed here because this is where the callers that make it
/// observable now live.
#[test]
fn unix_now_ms_is_a_reading_of_this_hosts_clock() {
    let millis = || {
        u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .unwrap()
    };

    let before = millis();
    let observed = unix_now_ms().unwrap();
    let after = millis();

    assert!(
        before <= observed && observed <= after,
        "{observed} is not a reading taken between {before} and {after}"
    );
}

// ---------------------------------------------------------------------------
// The cell's admission
// ---------------------------------------------------------------------------

/// A Claude that answers `--version` and nothing else.
///
/// `detect_claude_version` runs the real executable with `env_clear`, so this
/// is a real script and not a scripted string: what the start admits has to be
/// what a process actually printed.
fn versioned_claude(directory: &Path, version: &str) -> PathBuf {
    let path = directory.join("seam-claude");
    std::fs::write(
        &path,
        format!("#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  echo '{version} (Claude Code)'\n  exit 0\nfi\nexit 9\n"),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}

/// One start of `cell` on `seam`, against a Claude no promoted profile covers,
/// taken as far as it will go.
///
/// A free function taking the seam rather than building one, so a caller can
/// script the runtime -- with a terminal, or with nothing -- before the start
/// asks it for one.
async fn start_one_cell(seam: &Seam, cell: SessionCell) -> ErrorBody {
    use pseudomux_protocol::v1::{
        AuthPolicy, EnvironmentSpec, LifecycleMode, RetentionPolicy, SessionIdentity, TerminalSpec,
    };

    let cwd = owner_only_child(seam.root.path(), "work");
    let config_root = owner_only_child(seam.root.path(), "claude-config");
    // Untested by construction: the seam's registry of tested profiles is
    // empty, so no version this script could print is in it.
    let claude = versioned_claude(seam.root.path(), "9.9.9");

    let error = seam
        .service
        .start_session_owned_with_retention(
            StartSessionRequest {
                identity: SessionIdentity::New { session_id: None },
                cwd: cwd.canonicalize().unwrap().to_string_lossy().into_owned(),
                agent: None,
                claude: Some(ClaudeLaunchConfig {
                    executable: claude.to_string_lossy().into_owned(),
                    model: None,
                    effort: None,
                    permission_mode: None,
                    allowed_tools: Vec::new(),
                    denied_tools: Vec::new(),
                    settings: Vec::new(),
                    mcp_configs: Vec::new(),
                    plugin_dirs: Vec::new(),
                    system_prompt: SystemPromptPolicy::Default,
                    extra_args: Vec::new(),
                }),
                environment: EnvironmentSpec {
                    snapshot: BTreeMap::from([
                        (
                            "HOME".to_owned(),
                            seam.root.path().to_string_lossy().into_owned(),
                        ),
                        ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
                    ]),
                    set: BTreeMap::new(),
                    unset: BTreeSet::new(),
                },
                auth_policy: AuthPolicy::Inherit,
                // Both cells get one, because the minified cell REFUSES without
                // it -- a separate rule, earlier, and one this test is not
                // about. Asking the same question of both cells is the whole
                // point, so neither is allowed to fail for a different reason.
                config_isolation: Some(pseudomux_protocol::v1::ConfigIsolation {
                    root: config_root.to_string_lossy().into_owned(),
                }),
                terminal: TerminalSpec::default(),
                lifecycle: LifecycleMode::Transcript,
                retention: RetentionPolicy::OneShot,
                // The cell guard is the subject, so the version gate above it
                // is opened deliberately: `RequireTested` would refuse both
                // cells before either reached the guard.
                compatibility: CompatibilityPolicy::AllowUntested,
                cell,
            },
            SessionOwner::Caller,
        )
        .await
        .expect_err("no seam start can produce a terminal");
    // Whatever it answered, no session was published by a start that failed.
    assert!(seam.published_sessions().await.is_empty());
    error
}

/// Register: `native.rs::start_session_owned_with_retention`, `replace == with
/// !=` at `request.cell == SessionCell::Minified`.
///
/// `RequireTested` for the minified cell is the one rule that stands between a
/// pooled `/clear` and a Claude whose local command menu pmux has never
/// measured. Read as `!=` the rule INVERTS: the minified cell launches against
/// an unpromoted profile, and an ordinary full cell -- which never needed a
/// tested profile, because it types no control command -- is refused instead.
///
/// Both halves are asserted, because one alone is satisfied by a guard that
/// refuses everything or by one that refuses nothing.
#[tokio::test]
async fn only_the_minified_cell_requires_a_tested_profile_to_start() {
    let minified = start_one_cell(&Seam::build(), SessionCell::Minified).await;
    assert_eq!(minified.code, ErrorCode::UnsupportedClaudeVersion);
    assert!(
        minified.message.contains("minified cell requires a tested"),
        "the refusal names the rule: {}",
        minified.message
    );

    // The full cell is admitted past the guard and stops where every seam start
    // stops: the runtime scripted no terminal for it.
    let full_seam = Seam::build();
    let full = start_one_cell(&full_seam, SessionCell::Full).await;
    assert_ne!(
        full.code,
        ErrorCode::UnsupportedClaudeVersion,
        "a full cell needs no tested profile: {}",
        full.message
    );
    assert_eq!(full.code, ErrorCode::RmuxUnavailable);
    let requested = pseudomux_protocol::v1::TerminalSpec::default();
    let creations = full_seam.runtime.creations();
    assert_eq!(
        creations
            .iter()
            .map(|(_, rows, cols)| (*rows, *cols))
            .collect::<Vec<_>>(),
        vec![(requested.rows, requested.cols)],
        "the full cell reached the runtime and asked it for exactly the requested geometry, so \
         the refusal above is the runtime having nothing scripted and not an earlier rule"
    );
}

/// The double ANSWERS as well as refuses, and a start that gets its terminal
/// still publishes nothing when the terminal never renders a prompt.
///
/// This is the control for every refusal above it. `ScriptedRuntime` refusing
/// by default is only worth something if a scripted answer changes what
/// happens, and it does: the same start that stops at `rmux_unavailable` with
/// nothing scripted gets its pane here and stops one step later, at readiness,
/// with the pane closed behind it and no session in the map.
#[tokio::test]
async fn a_start_whose_terminal_never_renders_a_prompt_publishes_nothing_and_keeps_no_pane() {
    let seam = Seam::build();
    let closes = Arc::new(AtomicUsize::new(0));
    seam.runtime
        .script_terminal(Ok(Box::new(SeamTerminal::counting_closes(
            SeamClose::Reaped,
            &closes,
        ))));

    let error = start_one_cell(&seam, SessionCell::Full).await;

    // Both refusals are `rmux_unavailable`, and they are not the same refusal:
    // the unscripted one is the CREATE failing and carries the classification
    // `create_terminal` attaches to it; this one is the created pane failing to
    // render, and carries none. The counted close below is what proves which.
    assert_eq!(error.code, ErrorCode::RmuxUnavailable);
    assert_eq!(
        error.details,
        serde_json::Value::Null,
        "a startup-operation failure carries no create classification: {error:?}"
    );
    assert_eq!(seam.runtime.creations().len(), 1);
    assert_eq!(
        closes.load(Ordering::SeqCst),
        1,
        "a start that failed after creating a pane closes the pane it created"
    );
}

// ---------------------------------------------------------------------------
// The public session surface is gone; dispatch keeps three living methods
// ---------------------------------------------------------------------------

fn refused_start_request() -> StartSessionRequest {
    use pseudomux_protocol::v1::{
        AuthPolicy, EnvironmentSpec, LifecycleMode, RetentionPolicy, SessionIdentity, TerminalSpec,
    };

    StartSessionRequest {
        identity: SessionIdentity::New { session_id: None },
        cwd: "/tmp".to_owned(),
        agent: None,
        claude: Some(ClaudeLaunchConfig {
            executable: "/usr/bin/true".to_owned(),
            model: None,
            effort: None,
            permission_mode: None,
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            settings: Vec::new(),
            mcp_configs: Vec::new(),
            plugin_dirs: Vec::new(),
            system_prompt: SystemPromptPolicy::Default,
            extra_args: Vec::new(),
        }),
        environment: EnvironmentSpec {
            snapshot: BTreeMap::new(),
            set: BTreeMap::new(),
            unset: BTreeSet::new(),
        },
        auth_policy: AuthPolicy::Inherit,
        config_isolation: None,
        terminal: TerminalSpec::default(),
        lifecycle: LifecycleMode::Transcript,
        retention: RetentionPolicy::OneShot,
        compatibility: CompatibilityPolicy::AllowUntested,
        cell: SessionCell::Full,
    }
}

fn refused_turn() -> pseudomux_protocol::v1::TurnRequest {
    pseudomux_protocol::v1::TurnRequest {
        turn_id: SessionId::from_u128(3),
        prompt: "x".to_owned(),
        deadline_unix_ms: None,
        lease: pseudomux_protocol::v1::TurnLeasePolicy::default(),
    }
}

fn request_from_json(value: serde_json::Value) -> Request {
    serde_json::from_value(value).unwrap_or_else(|error| {
        panic!("fixture is not a Request: {error}");
    })
}

/// One of every [`Request`] variant. Adding a variant fails
/// [`request_method`] at compile time; add a fixture here in the same change.
fn every_request_variant() -> Vec<(&'static str, Request)> {
    let session = "00000000-0000-0000-0000-000000000001";
    let generation = "00000000-0000-0000-0000-000000000002";
    let turn = "00000000-0000-0000-0000-000000000003";
    let agent = "00000000-0000-0000-0000-000000000004";
    let start = json!({
        "identity": {"mode": "new"},
        "cwd": "/tmp",
        "claude": {"executable": "/usr/bin/true"}
    });
    let agent_spec = json!({
        "name": "reviewer",
        "claude": {"executable": "/usr/bin/true"}
    });
    vec![
        ("ping", Request::Ping),
        (
            "start_session",
            request_from_json(json!({
                "method": "start_session",
                "params": start.clone()
            })),
        ),
        (
            "run_turn",
            request_from_json(json!({
                "method": "run_turn",
                "params": {
                    "session_id": session,
                    "generation_id": generation,
                    "turn": {"turn_id": turn, "prompt": "x"}
                }
            })),
        ),
        (
            "cancel_turn",
            request_from_json(json!({
                "method": "cancel_turn",
                "params": {
                    "session_id": session,
                    "generation_id": generation,
                    "turn_id": turn
                }
            })),
        ),
        (
            "inspect_session",
            request_from_json(json!({
                "method": "inspect_session",
                "params": {
                    "session_id": session,
                    "generation_id": generation
                }
            })),
        ),
        (
            "attach_session",
            request_from_json(json!({
                "method": "attach_session",
                "params": {
                    "session_id": session,
                    "generation_id": generation
                }
            })),
        ),
        (
            "close_session",
            request_from_json(json!({
                "method": "close_session",
                "params": {
                    "session_id": session,
                    "generation_id": generation
                }
            })),
        ),
        (
            "subscribe_events",
            request_from_json(json!({
                "method": "subscribe_events",
                "params": {
                    "session_id": session,
                    "generation_id": generation
                }
            })),
        ),
        (
            "run_once",
            request_from_json(json!({
                "method": "run_once",
                "params": {
                    "session": start,
                    "turn": {"turn_id": turn, "prompt": "x"}
                }
            })),
        ),
        (
            "clear_session",
            request_from_json(json!({
                "method": "clear_session",
                "params": {
                    "session_id": session,
                    "generation_id": generation,
                    "expected_transcript_session_id": session
                }
            })),
        ),
        ("diagnose", Request::Diagnose),
        (
            "run_stateless",
            request_from_json(json!({
                "method": "run_stateless",
                "params": {"model": "sonnet", "prompt": "hi"}
            })),
        ),
        (
            "create_agent",
            request_from_json(json!({
                "method": "create_agent",
                "params": {"spec": agent_spec.clone()}
            })),
        ),
        (
            "get_agent",
            request_from_json(json!({
                "method": "get_agent",
                "params": {"agent_id": agent}
            })),
        ),
        (
            "list_agents",
            request_from_json(json!({
                "method": "list_agents",
                "params": {}
            })),
        ),
        (
            "update_agent",
            request_from_json(json!({
                "method": "update_agent",
                "params": {
                    "agent_id": agent,
                    "expected_version": 1,
                    "spec": agent_spec
                }
            })),
        ),
    ]
}

fn request_method(request: &Request) -> &'static str {
    match request {
        Request::Ping => "ping",
        Request::StartSession(_) => "start_session",
        Request::RunTurn(_) => "run_turn",
        Request::CancelTurn(_) => "cancel_turn",
        Request::InspectSession(_) => "inspect_session",
        Request::AttachSession(_) => "attach_session",
        Request::CloseSession(_) => "close_session",
        Request::SubscribeEvents(_) => "subscribe_events",
        Request::RunOnce(_) => "run_once",
        Request::ClearSession(_) => "clear_session",
        Request::Diagnose => "diagnose",
        Request::RunStateless(_) => "run_stateless",
        Request::CreateAgent(_) => "create_agent",
        Request::GetAgent(_) => "get_agent",
        Request::ListAgents(_) => "list_agents",
        Request::UpdateAgent(_) => "update_agent",
    }
}

fn assert_session_surface_removed(label: &str, error: &ErrorBody) {
    assert_eq!(error.code, ErrorCode::UnsupportedFeature, "{label}");
    assert_eq!(
        error.details.get("violation").and_then(|value| value.as_str()),
        Some("session_surface_removed"),
        "{label}: {error:?}"
    );
    assert!(
        error.message.contains("not part of this product"),
        "{label}: {}",
        error.message
    );
}

async fn assert_public_session_verbs_refuse(seam: &Seam) {
    let start = refused_start_request();
    let session_id = SessionId::from_u128(1);
    let generation_id = SessionGenerationId::from_u128(1);

    let refusals = [
        (
            "start_session",
            seam.service.start_session(start.clone()).await.unwrap_err(),
        ),
        (
            "run_once",
            seam.service
                .run_once(RunOnceRequest {
                    session: start,
                    turn: refused_turn(),
                })
                .await
                .unwrap_err(),
        ),
        (
            "run_turn",
            seam.service
                .run_turn(RunTurnRequest {
                    session_id,
                    generation_id,
                    turn: refused_turn(),
                })
                .await
                .unwrap_err(),
        ),
        (
            "clear_session",
            seam.service
                .clear_session(seam_clear_request(session_id, generation_id, None))
                .await
                .unwrap_err(),
        ),
        (
            "close_session",
            seam.service
                .close_session(CloseSessionRequest {
                    session_id,
                    generation_id,
                    policy: ClosePolicy::Force,
                })
                .await
                .unwrap_err(),
        ),
    ];
    for (name, error) in refusals {
        assert_session_surface_removed(name, &error);
    }
    assert!(
        seam.published_sessions().await.is_empty(),
        "a refused public verb must not mint"
    );
    assert!(
        seam.runtime.creations().is_empty(),
        "a refused public verb must not create a pane"
    );
}

/// Public verbs refuse on a constructed service. Mint stays on
/// `start_session_owned`.
#[tokio::test]
async fn public_session_verbs_refuse_without_minting() {
    let seam = Seam::build();
    assert_public_session_verbs_refuse(&seam).await;
}

/// The registry getter is crate-visible so the pool can mint; an embedder
/// outside this crate cannot take that door. Public verbs still refuse.
#[tokio::test]
async fn registry_is_crate_visible_and_public_verbs_still_refuse() {
    let seam = Seam::build();
    assert_public_session_verbs_refuse(&seam).await;
    let _ = seam.service.registry();
    const NATIVE: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/native.rs"));
    assert!(
        NATIVE.contains("pub(crate) fn registry("),
        "NativeService::registry must stay crate-visible for the pool"
    );
    assert!(
        !NATIVE.contains("pub fn registry("),
        "an embedder outside the crate must not take the registry door"
    );
}

/// Every non-living `Request` is refused by `dispatch` itself. Ping and
/// Diagnose still answer; `run_stateless` is living and reaches the pool
/// door (`path_b_not_enabled` with no pool), not `session_surface_removed`.
#[tokio::test]
async fn dispatch_refuses_every_non_living_request_and_keeps_the_living_allowlist() {
    let seam = Seam::build();
    let fixtures = every_request_variant();
    assert_eq!(
        fixtures.len(),
        16,
        "protocol v1 currently has 16 Request variants; add a fixture when one lands"
    );

    for (name, request) in fixtures {
        assert_eq!(request_method(&request), name);
        match seam.service.dispatch(request).await {
            Ok(ResponseResult::Pong(_)) if name == "ping" => {}
            Ok(ResponseResult::Diagnosis(_)) if name == "diagnose" => {}
            Err(error) if name == "run_stateless" => {
                assert_eq!(error.code, ErrorCode::UnsupportedFeature, "{name}");
                assert_eq!(
                    error
                        .details
                        .get("violation")
                        .and_then(|value| value.as_str()),
                    Some("path_b_not_enabled"),
                    "{name} must dispatch as living: {error:?}"
                );
            }
            Err(error)
                if !matches!(name, "ping" | "diagnose" | "run_stateless") =>
            {
                assert_session_surface_removed(name, &error);
            }
            other => panic!("{name}: unexpected dispatch outcome: {other:?}"),
        }
    }
    assert!(
        seam.published_sessions().await.is_empty(),
        "dispatch of a removed method must not mint"
    );
    assert!(
        seam.runtime.creations().is_empty(),
        "dispatch of a removed method must not create a pane"
    );
}
