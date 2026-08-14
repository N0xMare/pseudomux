//! The stateless pool, driven end to end against a deterministic double.
//!
//! Nothing here launches a Claude. The pool has no live Claude dependency at
//! this layer -- every process interaction is behind `InstanceHost` -- so every
//! edge of the state machine, including the ones that are hard to provoke
//! against a real child (a close that cannot confirm reaping, a clear that
//! typed nothing, a transcript carrying a sidechain row), is exercised here by
//! telling the double to produce it.
//!
//! The double DOES use a real filesystem, deliberately: the pool owns the
//! roots, and "nothing on disk is touched before the process is proven reaped"
//! is only a guarantee if a test can watch the disk.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use pseudomux_protocol::v1::{
    EffortLevel, ErrorBody, ErrorCode, RunStatelessRequest, SessionGenerationId, SessionId,
    StopReason, StopReasonKind, TokenUsage, UsageBreakdown,
};
use pseudomux_service::driver_io::AssertEmptyRefusal;
use pseudomux_service::pool::config as pool_config;
use pseudomux_service::pool::{
    ClearFailure, Destroyed, HostFailure, HostTurn, InstanceHandle, InstanceHost, MintSpec, Pool,
    PoolSettings, Spawner, WarmClassSetting, resolve_pool_class,
};
use pseudomux_service::v1::Clock;
use tokio::sync::Mutex;

// ---------------------------------------------------------------------------
// The deterministic double
// ---------------------------------------------------------------------------

/// What the double should do on the next call of each kind.
#[derive(Clone, Debug, Default)]
struct Script {
    mint_failures: Vec<HostFailure>,
    turn_failures: Vec<HostFailure>,
    clear_failures: Vec<ClearFailure>,
    /// When set, every `destroy` reports the process was NOT reaped.
    never_reaps: bool,
    /// Sidechain rows the next turn's transcript carries.
    sidechain_rows: usize,
    /// When set, the next turn reports `HostTurn::sidechain_rows: None` -- a
    /// host that did not count. `bool` rather than folding the two fields into
    /// one `Option` so the default stays "counted zero", which is what every
    /// other test in this file means.
    sidechain_rows_uncounted: bool,
    /// Sidechain TOKENS the next turn's transcript reports.
    ///
    /// Separate from the row count, because the three states the pool
    /// distinguishes -- counted zero, counted positive, and not counted -- have
    /// to be exercisable independently of the tokens.
    sidechain_tokens: TokenUsage,
}

#[derive(Debug, Default)]
struct Journal {
    mints: Vec<MintSpec>,
    turns: Vec<(SessionId, String)>,
    clears: Vec<SessionId>,
    destroys: Vec<SessionId>,
    /// Whether the tree existed at the moment `destroy` was called. This is how
    /// "nothing on disk is deleted before the process is proven reaped" is
    /// observed rather than assumed.
    tree_present_at_destroy: Vec<bool>,
    /// Every root a turn was served from, in order.
    served_roots: Vec<PathBuf>,
}

/// A one-shot hold a test can place on the double's next call of one kind.
///
/// ONE type rather than a set of fields per operation. The pool holds no lock
/// across a host call, so every race worth testing is "another task ran while
/// the host was still inside `x`" -- and that test is only writable if `x` can
/// be stopped mid-call. The turn gate was written first, by hand; a second and
/// third copy of the same three fields is the shape this repository keeps
/// finding narrowed, so the pattern is a type.
#[derive(Default)]
struct Gate {
    held: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    release: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    started: tokio::sync::Notify,
}

impl Gate {
    /// Make the next call through [`Self::pass`] block until [`Self::release`].
    async fn hold(&self) {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        *self.held.lock().await = Some(receiver);
        *self.release.lock().await = Some(sender);
    }

    /// Return once the held call has actually reached the host.
    async fn await_started(&self) {
        self.started.notified().await;
    }

    /// Called from inside the double: announce arrival, then wait if held.
    async fn pass(&self) {
        if let Some(held) = self.held.lock().await.take() {
            self.started.notify_one();
            let _ = held.await;
        }
    }

    async fn release(&self) {
        if let Some(sender) = self.release.lock().await.take() {
            let _ = sender.send(());
        }
    }
}

struct DoubleHost {
    script: Mutex<Script>,
    journal: Mutex<Journal>,
    next_pid: AtomicU64,
    /// Released to let a held turn finish. Present so a test can observe the
    /// pool while a caller really is waiting, which is the only way to tell
    /// "shutdown keeps an instance a caller is waiting on" apart from
    /// "shutdown keeps everything".
    turn_gate: Gate,
    /// Released to let a held mint finish. A launch is the one host call the
    /// pool enters holding no process handle, so it is the window in which a
    /// concurrent teardown cannot see the child from the instance alone.
    mint_gate: Gate,
    /// Released to let a held clear finish. `spawn_clear` runs on a task nobody
    /// waits on, so a clear in flight is the state a pool spends most of its
    /// life in -- and the one a shutdown is most likely to land inside.
    clear_gate: Gate,
}

impl DoubleHost {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(Script::default()),
            journal: Mutex::new(Journal::default()),
            next_pid: AtomicU64::new(1_000),
            turn_gate: Gate::default(),
            mint_gate: Gate::default(),
            clear_gate: Gate::default(),
        })
    }

    /// Make the next `run_turn` block until [`Self::release_held_turn`].
    async fn hold_next_turn(&self) {
        self.turn_gate.hold().await;
    }

    /// Return once the held turn has actually reached the host.
    async fn await_turn_started(&self) {
        self.turn_gate.await_started().await;
    }

    async fn release_held_turn(&self) {
        self.turn_gate.release().await;
    }

    async fn fail_next_turn(&self, error: ErrorBody) {
        self.script.lock().await.turn_failures.push(HostFailure {
            error,
            process_may_survive: false,
        });
    }

    async fn fail_next_clear(&self, failure: ClearFailure) {
        self.script.lock().await.clear_failures.push(failure);
    }

    /// Fail the next `mint`.
    ///
    /// `Script::mint_failures` has been READ by `DoubleHost::mint` since this
    /// file was written and written by nothing, so no test in this tree had
    /// ever made a mint fail. That is why `Pool::abandon_mint` -- the whole
    /// compensation path for a launch that did not happen -- could be replaced
    /// with `()` by the mutation tool with the suite green.
    async fn fail_next_mint(&self, failure: HostFailure) {
        self.script.lock().await.mint_failures.push(failure);
    }

    async fn journal(&self) -> Journal {
        let journal = self.journal.lock().await;
        Journal {
            mints: journal.mints.clone(),
            turns: journal.turns.clone(),
            clears: journal.clears.clone(),
            destroys: journal.destroys.clone(),
            tree_present_at_destroy: journal.tree_present_at_destroy.clone(),
            served_roots: journal.served_roots.clone(),
        }
    }

    async fn root_of(&self, session: SessionId) -> Option<PathBuf> {
        self.journal
            .lock()
            .await
            .mints
            .iter()
            .find(|spec| spec.slot != u32::MAX && session_for(spec) == session)
            .map(|spec| spec.root.clone())
    }
}

/// The double derives a stable session id from the slot and epoch, so a test can
/// map a session back to the tree it owns without the pool publishing anything.
fn session_for(spec: &MintSpec) -> SessionId {
    let mut bytes = [0_u8; 16];
    bytes[0..4].copy_from_slice(&spec.slot.to_be_bytes());
    bytes[4..12].copy_from_slice(&spec.epoch.to_be_bytes());
    SessionId::from_bytes(bytes)
}

#[async_trait]
impl InstanceHost for DoubleHost {
    async fn mint(&self, spec: MintSpec) -> Result<InstanceHandle, HostFailure> {
        self.journal.lock().await.mints.push(spec.clone());
        self.mint_gate.pass().await;
        if let Some(failure) = self.script.lock().await.mint_failures.pop() {
            return Err(failure);
        }
        Ok(InstanceHandle {
            session_id: session_for(&spec),
            generation_id: SessionGenerationId::default(),
            pid: Some(
                i32::try_from(self.next_pid.fetch_add(1, Ordering::Relaxed)).unwrap_or(i32::MAX),
            ),
            claude_version: "2.1.220".to_owned(),
        })
    }

    async fn run_turn(
        &self,
        handle: &InstanceHandle,
        prompt: String,
        _deadline_unix_ms: u64,
    ) -> Result<HostTurn, HostFailure> {
        {
            let mut journal = self.journal.lock().await;
            journal.turns.push((handle.session_id, prompt.clone()));
        }
        self.turn_gate.pass().await;
        // Write the prompt into the instance's own `history.jsonl`, exactly the
        // per-root, cwd-scoped residue channel recycle exists to bound. The
        // path is recovered from the mint journal, so the test observes the
        // real file the pool will later erase.
        if let Some(root) = self.root_of(handle.session_id).await {
            self.journal.lock().await.served_roots.push(root.clone());
            let history = root.join("history.jsonl");
            let mut existing = std::fs::read_to_string(&history).unwrap_or_default();
            existing.push_str(&prompt);
            existing.push('\n');
            std::fs::write(&history, existing).expect("history is writable");
            // ...and the TRANSCRIPT, in the one shape Claude Code writes it:
            // `<root>/projects/<slug>/<session>.jsonl`. This is the file the
            // pool now mirrors into the evidence corpus before erasing the
            // tree, so the double has to produce one or the retention path is
            // exercised against an empty directory forever.
            let project = root.join("projects").join("-pmux-pool-cwd");
            std::fs::create_dir_all(&project).expect("the project directory is creatable");
            let mut rows = String::new();
            for row in [
                serde_json::json!({
                    "type": "user", "promptId": handle.session_id.to_string(),
                    "isMeta": false, "isSidechain": false, "entrypoint": "cli",
                    "version": handle.claude_version,
                    "timestamp": "2026-08-09T10:00:00.000Z",
                    "message": {"role": "user", "content": prompt},
                }),
                serde_json::json!({
                    "type": "assistant", "entrypoint": "cli",
                    "version": handle.claude_version,
                    "timestamp": "2026-08-09T10:00:01.000Z",
                    "message": {"role": "assistant", "content": format!("answered: {prompt}")},
                }),
                serde_json::json!({
                    "type": "system", "subtype": "turn_duration", "entrypoint": "cli",
                    "version": handle.claude_version,
                    "timestamp": "2026-08-09T10:00:01.150Z",
                    "durationMs": 1150,
                }),
            ] {
                rows.push_str(&serde_json::to_string(&row).unwrap());
                rows.push('\n');
            }
            std::fs::write(project.join(format!("{}.jsonl", handle.session_id)), rows)
                .expect("the transcript is writable");
        }
        let (sidechain_rows, sidechain_rows_uncounted, sidechain_tokens) = {
            let mut script = self.script.lock().await;
            if let Some(failure) = script.turn_failures.pop() {
                return Err(failure);
            }
            (
                std::mem::take(&mut script.sidechain_rows),
                std::mem::take(&mut script.sidechain_rows_uncounted),
                std::mem::take(&mut script.sidechain_tokens),
            )
        };
        Ok(HostTurn {
            text: format!("answered: {prompt}"),
            reported_model: Some("claude-opus-5".to_owned()),
            stop_reason: Some(StopReason {
                kind: StopReasonKind::EndTurn,
                raw: None,
            }),
            usage: UsageBreakdown {
                main: TokenUsage {
                    input_tokens: 186,
                    output_tokens: 12,
                    ..TokenUsage::default()
                },
                sidechain: sidechain_tokens,
                combined: TokenUsage {
                    input_tokens: 186,
                    output_tokens: 12,
                    ..TokenUsage::default()
                },
                cost_usd: None,
            },
            // `Some(...)` by default, which is also what the production host
            // reports: `TurnResult::sidechain_rows` carries the count out of the
            // transcript analysis. `None` is reachable here on purpose, because
            // it is a REFUSAL in `commit` and a refusal with no test is a
            // refusal that can be deleted.
            sidechain_rows: (!sidechain_rows_uncounted).then_some(sidechain_rows),
        })
    }

    async fn clear(&self, handle: &InstanceHandle) -> Result<(), ClearFailure> {
        self.journal.lock().await.clears.push(handle.session_id);
        self.clear_gate.pass().await;
        match self.script.lock().await.clear_failures.pop() {
            Some(failure) => Err(failure),
            None => Ok(()),
        }
    }

    async fn destroy(&self, handle: &InstanceHandle) -> Result<Destroyed, HostFailure> {
        let present = match self.root_of(handle.session_id).await {
            Some(root) => root.exists(),
            None => false,
        };
        {
            let mut journal = self.journal.lock().await;
            journal.destroys.push(handle.session_id);
            journal.tree_present_at_destroy.push(present);
        }
        // A real teardown takes seconds and the pool holds no lock across it.
        // Yielding here reproduces that on the double, so a test can observe
        // the pool's state from another task while a teardown is in flight.
        tokio::task::yield_now().await;
        Ok(Destroyed {
            process_reaped: !self.script.lock().await.never_reaps,
        })
    }
}

// ---------------------------------------------------------------------------
// A clock and a spawner a test can drive
// ---------------------------------------------------------------------------

#[derive(Default)]
struct TestClock {
    now_ms: AtomicU64,
}

impl TestClock {
    fn advance(&self, delta: u64) {
        self.now_ms.fetch_add(delta, Ordering::Relaxed);
    }
}

impl Clock for TestClock {
    fn now_ms(&self) -> u64 {
        self.now_ms.load(Ordering::Relaxed)
    }
}

/// Queues the pool's post-response work instead of running it.
///
/// This is what makes "the caller never waits on the clear" observable: the
/// test can assert it already holds the answer while the clear is still an
/// undrained future, then drain and assert what the clear did.
#[derive(Default)]
struct QueueSpawner {
    queued: std::sync::Mutex<Vec<std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>>>,
}

impl QueueSpawner {
    fn pending(&self) -> usize {
        self.queued.lock().expect("spawner lock").len()
    }

    async fn drain(&self) {
        loop {
            let batch: Vec<_> = std::mem::take(&mut *self.queued.lock().expect("spawner lock"));
            if batch.is_empty() {
                return;
            }
            for work in batch {
                work.await;
            }
        }
    }
}

impl Spawner for QueueSpawner {
    fn spawn(&self, work: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>) {
        self.queued.lock().expect("spawner lock").push(work);
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Harness {
    pool: Arc<Pool>,
    host: Arc<DoubleHost>,
    clock: Arc<TestClock>,
    spawner: Arc<QueueSpawner>,
    _temp: tempfile::TempDir,
    parent: PathBuf,
}

fn build(mutate: impl FnOnce(&mut PoolSettings)) -> Harness {
    let temp = tempfile::tempdir().expect("tempdir");
    let parent = temp.path().join("pool");
    let mut settings = PoolSettings::defaults(parent.clone(), PathBuf::from("/usr/bin/claude"));
    settings.pool_size = 2;
    settings.rss_budget_mb = 2 * 1024;
    settings.instance_idle_ttl_ms = 10_000;
    mutate(&mut settings);
    let config = settings.validate().expect("test settings must validate");
    let host = DoubleHost::new();
    let clock = Arc::new(TestClock::default());
    let spawner = Arc::new(QueueSpawner::default());
    let pool = Pool::new(
        config,
        Arc::clone(&host) as Arc<dyn InstanceHost>,
        Arc::clone(&clock) as Arc<dyn Clock>,
        Arc::clone(&spawner) as Arc<dyn Spawner>,
    );
    Harness {
        pool,
        host,
        clock,
        spawner,
        _temp: temp,
        parent,
    }
}

fn ask(prompt: &str) -> RunStatelessRequest {
    RunStatelessRequest {
        model: "claude-opus-5".to_owned(),
        effort: Some(EffortLevel::High),
        prompt: prompt.to_owned(),
        deadline_unix_ms: None,
    }
}

/// Every epoch directory currently on disk under the pool parent.
fn trees(parent: &PathBuf) -> Vec<String> {
    let mut found = Vec::new();
    // PANICS on an unreadable parent rather than answering "no trees". Every
    // `assert!(trees(..).is_empty())` in this file is a claim that teardown
    // erased what it made, and an early return here satisfies all of them for
    // a parent that does not exist -- which the harness always creates, so a
    // missing one is a broken test rather than a drained pool.
    let slots = std::fs::read_dir(parent).unwrap_or_else(|error| {
        panic!(
            "the pool parent {} is unreadable, so 'it holds no trees' says nothing: {error}",
            parent.display()
        )
    });
    for slot in slots.flatten() {
        let Ok(epochs) = std::fs::read_dir(slot.path()) else {
            continue;
        };
        for epoch in epochs.flatten() {
            found.push(format!(
                "{}/{}",
                slot.file_name().to_string_lossy(),
                epoch.file_name().to_string_lossy()
            ));
        }
    }
    found.sort();
    found
}

/// Plant the residue a daemon that was killed mid-mint leaves behind.
///
/// Every level is 0700, including the pool parent itself: `Pool::start`'s first
/// act is `require_private_parent`, so a parent left at the harness's default
/// `create_dir_all` mode refuses before any slot is examined and every
/// assertion about slots downstream would be about the wrong refusal.
fn plant_residue(parent: &Path, relative: &[&str]) {
    use std::os::unix::fs::PermissionsExt;

    for path in std::iter::once(parent.to_path_buf())
        .chain(relative.iter().map(|suffix| parent.join(suffix)))
    {
        std::fs::create_dir_all(&path).expect("the residue is creatable");
        let mut walked = parent.to_path_buf();
        std::fs::set_permissions(&walked, std::fs::Permissions::from_mode(0o700)).unwrap();
        for component in path.strip_prefix(parent).expect("under the parent") {
            walked.push(component);
            std::fs::set_permissions(&walked, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
}

/// Every tree currently sitting in a retention directory, sorted.
///
/// A retention directory that does not exist is EMPTY, and that is the honest
/// answer here rather than the panic `trees` raises for the pool parent. The
/// difference is which claim the absence would hide: the harness always creates
/// the pool parent, so a missing one is a broken test, while `erase_tree`
/// creates a retention directory only when it has something to put in it, so a
/// missing one is precisely the "kept nothing" this reports. Every assertion
/// that reads zero from this function is paired, in the same test, with one that
/// reads one -- so an implementation that retained nothing at all could not
/// satisfy them both.
fn retained(retain: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(retain) else {
        return Vec::new();
    };
    let mut found: Vec<String> = entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    found.sort();
    found
}

async fn assert_invariants(harness: &Harness) {
    harness
        .pool
        .check_invariants()
        .await
        .expect("every pool invariant must hold");
}

// ---------------------------------------------------------------------------
// Admission and the product surface
// ---------------------------------------------------------------------------

#[tokio::test]
async fn one_stateless_turn_answers_and_names_no_resource() {
    let harness = build(|_| {});
    let result = harness
        .pool
        .run(ask("what is two plus two"))
        .await
        .expect("a stateless turn answers");

    assert_eq!(result.text, "answered: what is two plus two");
    assert_eq!(result.model, "claude-opus-5");
    assert_eq!(result.effort, Some(EffortLevel::High));
    assert_eq!(result.claude_version, "2.1.220");
    assert_eq!(result.usage.main.input_tokens, 186);

    // The whole product statement, asserted over the serialized frame rather
    // than field by field: nothing a caller receives names a resource pmux
    // owns. A sweep for the instance's session id, its root and its cwd.
    let frame = serde_json::to_string(&result).expect("result serializes");
    let journal = harness.host.journal().await;
    let spec = &journal.mints[0];
    let session = session_for(spec).to_string();
    for needle in [
        session.as_str(),
        spec.root.to_str().expect("utf8 root"),
        spec.cwd.to_str().expect("utf8 cwd"),
        harness.parent.to_str().expect("utf8 parent"),
    ] {
        assert!(
            !frame.contains(needle),
            "the response frame names {needle}, which a second caller could then reach:\n{frame}"
        );
    }

    // The discrimination self-test: a sweep that searched the wrong buffer
    // would pass forever, so prove it can fail by planting a value it must find.
    let planted = format!("{{\"session_id\":\"{session}\"}}");
    assert!(
        planted.contains(&session),
        "the sweep must be able to find a planted resource name"
    );

    assert_invariants(&harness).await;
}

#[tokio::test]
async fn a_warm_instance_of_the_class_serves_the_next_turn_with_no_second_mint() {
    let harness = build(|_| {});
    harness.pool.run(ask("first")).await.expect("first answers");
    harness.spawner.drain().await;
    harness
        .pool
        .run(ask("second"))
        .await
        .expect("second answers");

    let journal = harness.host.journal().await;
    assert_eq!(journal.mints.len(), 1, "the warm path mints nothing");
    assert_eq!(journal.turns.len(), 2);
    assert_eq!(
        journal.turns[0].0, journal.turns[1].0,
        "both turns were served by one instance"
    );
    assert_invariants(&harness).await;
}

#[tokio::test]
async fn an_instance_of_another_class_is_never_handed_an_opus_call() {
    let harness = build(|settings| {
        settings.pool_size = 4;
        settings.rss_budget_mb = 4 * 1024;
    });
    harness
        .pool
        .run(RunStatelessRequest {
            model: "claude-haiku-4-5".to_owned(),
            effort: None,
            prompt: "cheap".to_owned(),
            deadline_unix_ms: None,
        })
        .await
        .expect("haiku answers");
    harness.spawner.drain().await;

    // A haiku instance is idle. An opus/high call must NOT be served by it.
    harness
        .pool
        .run(ask("expensive"))
        .await
        .expect("opus answers");
    let journal = harness.host.journal().await;
    assert_eq!(
        journal.mints.len(),
        2,
        "fungibility is per class: an idle haiku process cannot serve an opus/high turn"
    );
    assert_eq!(journal.mints[0].class.canonical_model, "claude-haiku-4-5");
    assert_eq!(journal.mints[0].class.effort_argv, None);
    assert_eq!(journal.mints[1].class.canonical_model, "claude-opus-5");
    assert_eq!(journal.mints[1].class.effort_argv, Some("high"));
    assert_invariants(&harness).await;
}

#[tokio::test]
async fn the_class_key_and_the_child_argv_come_from_one_call() {
    let harness = build(|_| {});
    harness.pool.run(ask("hello")).await.expect("answers");
    let journal = harness.host.journal().await;
    let spec = &journal.mints[0];
    // The pool's model of the instance renders byte-identically to the argv the
    // process was spawned with, because both came from `resolve_pool_class`.
    let (expected, _) =
        resolve_pool_class("claude-opus-5", Some(EffortLevel::High)).expect("admitted");
    assert_eq!(spec.class, expected);
    assert_eq!(
        spec.class.argv(),
        vec![
            "--model".to_owned(),
            "claude-opus-5".to_owned(),
            "--effort".to_owned(),
            "high".to_owned(),
        ]
    );
}

/// A caller that arrives while every slot is clearing WAITS for one.
///
/// The defect this pins, and it was a product defect at HEAD rather than a
/// flake. `spawn_clear` exists so a caller is answered BEFORE `/clear` is typed,
/// so the ordinary state of a pool one instant after a burst is every slot
/// `Clearing` -- ~30 ms of housekeeping with nobody waiting on any of it. A
/// caller arriving there was refused, over a sentence that said so outright: "3
/// of 3 usable instance(s) are live -- 0 serving a turn, 3 clearing between
/// turns, with no caller waiting, 0 idle".
///
/// MEASURED at 8 concurrent callers against 3 slots over 3 rounds: 21 of 24
/// refused, rounds 2 and 3 finishing in 782 and 539 MICROSECONDS, and 3 launches
/// for 3 served calls -- so no instance ever served a second caller and every
/// fungibility claim in that wave was vacuous.
///
/// The clear is driven from THIS task, after the third call is already parked in
/// admission, so the test observes the order rather than assuming it: the census
/// says both slots are clearing and the waiter has not returned, and only then
/// is the clear allowed to run.
#[tokio::test]
async fn a_caller_waits_for_a_clearing_slot_instead_of_being_refused() {
    let harness = build(|settings| {
        settings.pool_size = 2;
        settings.rss_budget_mb = 2 * 1024;
    });
    harness.pool.run(ask("one")).await.expect("first answers");
    harness.pool.run(ask("two")).await.expect("second answers");
    assert_eq!(harness.spawner.pending(), 2, "two clears are still owed");

    let census = harness.pool.census().await;
    assert_eq!(census.clearing, 2, "both slots are held by the clear");
    assert_eq!(census.in_flight, 0, "and no caller is waiting on either");
    assert_eq!(census.idle, 0, "so nothing is checkout-able");

    let pool = Arc::clone(&harness.pool);
    let waiting = tokio::spawn(async move { pool.run(ask("three")).await });

    // The waiter is parked in admission with nothing it can be given. Anything
    // that lets it finish here -- a refusal -- fails the assertion below,
    // because the clears have deliberately not been run yet.
    tokio::task::yield_now().await;
    assert!(
        !waiting.is_finished(),
        "the third caller reached a decision while every slot was still clearing"
    );

    harness.spawner.drain().await;
    let result = waiting
        .await
        .expect("the waiting task")
        .expect("a clearing slot comes back, so the caller is served rather than refused");
    assert_eq!(result.text, "answered: three");

    let journal = harness.host.journal().await;
    assert_eq!(
        journal.mints.len(),
        2,
        "the waiter was served by an instance that already existed, not by a third mint"
    );
    assert_eq!(journal.turns.len(), 3);
    assert_invariants(&harness).await;
}

/// Genuine exhaustion refuses on the FIRST read, with no wait at all.
///
/// The other half of the property above, and the one that stops the fix from
/// being a queue: an instance a caller is waiting on is holding a model, which
/// takes however long a model takes. `admission_wait_ms == 0` is the observable
/// claim -- the pool looked, found nothing on its way back, and said so.
#[tokio::test]
async fn a_pool_whose_slot_is_serving_a_turn_refuses_without_waiting() {
    let harness = build(|settings| {
        settings.pool_size = 1;
        settings.rss_budget_mb = 1024;
    });
    harness.host.hold_next_turn().await;
    let pool = Arc::clone(&harness.pool);
    let held = tokio::spawn(async move { pool.run(ask("one")).await });
    harness.host.await_turn_started().await;
    assert_eq!(harness.pool.census().await.in_flight, 1);

    let started = std::time::Instant::now();
    let refusal = harness
        .pool
        .run(ask("two"))
        .await
        .expect_err("the only slot is holding a model");
    let elapsed = started.elapsed();

    assert_eq!(refusal.code, ErrorCode::SessionBusy);
    assert_eq!(
        refusal
            .details
            .get("admission_wait_ms")
            .and_then(serde_json::Value::as_u64),
        Some(0),
        "a slot holding a model does not come back on its own, so nothing was waited for: {}",
        refusal.message
    );
    assert!(
        refusal
            .message
            .contains("no slot was on its way back, so none was waited for"),
        "the refusal must say it did not wait, and why: {}",
        refusal.message
    );
    assert!(
        refusal.message.contains("1 serving a turn"),
        "the census must name the state that actually held the slot: {}",
        refusal.message
    );
    // The wall clock, as a second and independent statement: `admission_wait_ms`
    // is the pool's own account of itself, and a bug that reported 0 while
    // sleeping would satisfy every assertion above.
    assert!(
        elapsed < Duration::from_millis(pool_config::ADMISSION_WAIT_CEILING_MS),
        "the refusal took {elapsed:?}, which is not 'without waiting'"
    );

    harness.host.release_held_turn().await;
    held.await.expect("the held turn task").expect("answers");
    harness.spawner.drain().await;
    assert_invariants(&harness).await;
}

/// The admission wait spends the CALLER's budget, and stops when it runs out.
///
/// The ceiling is not the only bound, and it must not be: a caller that sent
/// `deadline_unix_ms` 50 ms out has asked to be told inside 50 ms, and waiting
/// 500 ms for a slot and then handing that caller a turn against an already-dead
/// deadline is worse than refusing. So the deadline is resolved BEFORE admission
/// and re-read on every pass, and the smaller of the two bounds wins.
///
/// The clock is the harness's, so the deadline passes because the test moves
/// time rather than because the test sleeps: at 500 ms the ceiling would also
/// end this wait, and a test that could not tell those apart would pass with the
/// deadline bound deleted.
#[tokio::test]
async fn the_wait_ends_at_the_callers_deadline_when_that_comes_first() {
    let harness = build(|settings| {
        settings.pool_size = 2;
        settings.rss_budget_mb = 2 * 1024;
    });
    harness.pool.run(ask("one")).await.expect("first answers");
    harness.pool.run(ask("two")).await.expect("second answers");
    assert_eq!(harness.pool.census().await.clearing, 2);

    let pool = Arc::clone(&harness.pool);
    let started = std::time::Instant::now();
    let waiting = tokio::spawn(async move {
        pool.run(RunStatelessRequest {
            model: "claude-opus-5".to_owned(),
            effort: Some(EffortLevel::High),
            prompt: "three".to_owned(),
            // The harness clock starts at 0, so this is 50 ms of the pool's own
            // time, and nothing but `TestClock::advance` moves it.
            deadline_unix_ms: Some(50),
        })
        .await
    });
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(
        !waiting.is_finished(),
        "the deadline has not passed on the pool's clock, so this caller is still waiting"
    );

    harness.clock.advance(60);
    let refusal = waiting
        .await
        .expect("the waiting task")
        .expect_err("a caller past its own deadline is refused");
    let elapsed = started.elapsed();
    assert_eq!(refusal.code, ErrorCode::SessionBusy);
    let waited_ms = refusal
        .details
        .get("admission_wait_ms")
        .and_then(serde_json::Value::as_u64)
        .expect("every capacity refusal publishes what it waited");
    assert!(
        waited_ms < pool_config::ADMISSION_WAIT_CEILING_MS,
        "the deadline, not the ceiling, must be what ended this wait: {waited_ms} ms"
    );
    assert!(
        elapsed < Duration::from_millis(pool_config::ADMISSION_WAIT_CEILING_MS),
        "the caller was held past its own deadline for {elapsed:?}"
    );
    assert_eq!(
        harness.pool.census().await.clearing,
        2,
        "the refusal is about this caller's budget; the pool is unchanged"
    );
    harness.spawner.drain().await;
    assert_invariants(&harness).await;
}

#[tokio::test]
async fn an_exhausted_pool_refuses_after_a_bounded_wait_and_names_its_budget() {
    let harness = build(|settings| {
        settings.pool_size = 2;
        settings.rss_budget_mb = 2 * 1024;
    });

    // Two turns whose clears have not been drained. Both callers ALREADY HAVE
    // their answers -- `run` returned them -- so the two instances are
    // `Clearing` and nobody is waiting on either. Nothing is idle and nothing
    // is free, so the third call waits for the ceiling and is then refused --
    // this harness never runs the clear -- and the refusal has to say which of
    // those two very different things is holding the slots.
    //
    // This comment used to read "both instances are mid-turn from the pool's
    // point of view", and the assertion below used to be `in_flight == 2`. Both
    // were false, and together they pinned the defect: `in_flight` spanned
    // `CheckedOut | Delivering | Clearing`, so the census printed "2 serving a
    // turn" for two instances that had finished serving. Measured over the real
    // socket at 8 concurrent callers against 8 slots, the shipped refusal read
    // "8 of 8 usable instance(s) are live -- 7 serving a turn" at an instant
    // when zero were.
    harness.pool.run(ask("one")).await.expect("first answers");
    harness.pool.run(ask("two")).await.expect("second answers");
    assert_eq!(harness.spawner.pending(), 2, "two clears are still owed");

    let started = std::time::Instant::now();
    let refusal = harness
        .pool
        .run(ask("three"))
        .await
        .expect_err("a full pool refuses");
    let elapsed = started.elapsed();
    assert_eq!(refusal.code, ErrorCode::SessionBusy);
    assert!(refusal.retryable);
    // The wait is REAL and it is BOUNDED, and both halves have to be asserted.
    // A refusal that never waited is the defect; a wait with no ceiling is the
    // hang it must not be traded for. The bound is read from the pool's own
    // constant rather than restated, so tuning the ceiling cannot leave a test
    // asserting a number the product no longer uses.
    let ceiling = Duration::from_millis(pool_config::ADMISSION_WAIT_CEILING_MS);
    let waited_ms = refusal
        .details
        .get("admission_wait_ms")
        .and_then(serde_json::Value::as_u64)
        .expect("every capacity refusal publishes what it waited");
    assert!(
        waited_ms >= pool_config::ADMISSION_WAIT_CEILING_MS,
        "the clear never ran, so this caller had to spend the whole ceiling: {waited_ms} ms"
    );
    assert!(
        elapsed >= ceiling && elapsed < ceiling * 4,
        "the wall clock must agree with the pool's own account and stay bounded: {elapsed:?}"
    );
    assert!(
        refusal
            .message
            .contains(&format!("no slot came back in the {waited_ms} ms")),
        "a refusal that waited must say so, and say how long: {}",
        refusal.message
    );
    assert_eq!(
        refusal.details.get("violation").and_then(|v| v.as_str()),
        Some("pool_exhausted")
    );
    assert_eq!(
        refusal
            .details
            .get("budget_instances")
            .and_then(serde_json::Value::as_u64),
        Some(2)
    );
    assert_eq!(
        refusal
            .details
            .get("in_flight")
            .and_then(serde_json::Value::as_u64),
        Some(0),
        "no caller is waiting on either instance: both already have their answers"
    );
    assert_eq!(
        refusal
            .details
            .get("clearing")
            .and_then(serde_json::Value::as_u64),
        Some(2),
        "the two slots are held by the post-answer clear, and the details must say so"
    );
    assert!(
        refusal
            .message
            .contains("2 clearing between turns, with no caller waiting"),
        "the refusal must name the state that actually held the slots: {}",
        refusal.message
    );
    assert!(
        refusal.message.contains("0 serving a turn"),
        "a caller deciding how long to back off must not be told a clear is a turn: {}",
        refusal.message
    );
    assert!(
        refusal.message.contains("nothing is queued"),
        "the refusal must say there is no queue: {}",
        refusal.message
    );

    // Nothing queued: a third mint was never attempted, and no third turn ran.
    let journal = harness.host.journal().await;
    assert_eq!(journal.mints.len(), 2);
    assert_eq!(journal.turns.len(), 2);
    assert_invariants(&harness).await;

    // ...and once the clears run, the same pool answers, on the same two
    // instances. The refusal was about a state that lasts milliseconds, which
    // is exactly why naming it correctly is the difference between a caller
    // retrying and a caller giving up.
    harness.spawner.drain().await;
    harness
        .pool
        .run(ask("three"))
        .await
        .expect("a drained clear returns the slot");
    let journal = harness.host.journal().await;
    assert_eq!(
        journal.mints.len(),
        2,
        "the retry was served by an existing instance, not a new one"
    );
    assert_eq!(journal.turns.len(), 3);
    assert_invariants(&harness).await;
}

#[tokio::test]
async fn a_cold_swap_takes_another_classes_idle_instance_rather_than_starving_it() {
    let harness = build(|settings| {
        settings.pool_size = 1;
        settings.rss_budget_mb = 1024;
    });

    harness
        .pool
        .run(RunStatelessRequest {
            model: "claude-haiku-4-5".to_owned(),
            effort: None,
            prompt: "cheap".to_owned(),
            deadline_unix_ms: None,
        })
        .await
        .expect("haiku answers");
    harness.spawner.drain().await;

    // The single slot is held by an idle haiku instance. Refusing here would be
    // permanent starvation for every other class, so the pool destroys the
    // idle occupant and mints the shape a live caller actually asked for.
    harness
        .pool
        .run(ask("expensive"))
        .await
        .expect("opus answers");
    harness.spawner.drain().await;

    let journal = harness.host.journal().await;
    assert_eq!(journal.mints.len(), 2);
    assert_eq!(journal.destroys.len(), 1, "the idle victim was torn down");
    assert_eq!(
        journal.mints[1].epoch, 1,
        "the reclaimed slot mints at a fresh epoch, so an orphan can never share a directory"
    );
    assert_eq!(trees(&harness.parent).len(), 1, "the victim's tree is gone");
    assert_invariants(&harness).await;
}

/// A cold swap is what the pool does instead of REFUSING, not instead of
/// WAITING.
///
/// The other half of the starvation fix, and it is the half that decides
/// whether the pool ever reuses anything. A cold swap destroys an instance the
/// pool has proven clean and pays a full mint for the replacement. Firing it the
/// instant any slot appears also takes that slot out from under a caller of the
/// instance's OWN class waiting beside it -- and once callers wait at all, that
/// stops being an edge case and becomes every admission.
///
/// MEASURED over the socket at 8 concurrent callers across 4 classes against 3
/// slots, with the wait in and this deferral out: **7 launches for 7 served
/// calls**. Every call was served by a process the pool had just built, having
/// destroyed one it had just proven clean, and no instance ever served a second
/// caller -- so the wave still failed `claim_reuse_was_exercised`, for a
/// different reason than before. With the deferral: 3 launches for 7 served.
///
/// The negative control is a separate test on purpose:
/// `a_cold_swap_takes_another_classes_idle_instance_rather_than_starving_it`
/// drains its clear first, so nothing is coming back, and the swap fires on the
/// first read at no added latency. That is the ordinary cold case and it must
/// not regress.
#[tokio::test]
async fn a_cold_swap_waits_for_a_clearing_slot_before_destroying_a_warm_one() {
    let harness = build(|settings| {
        settings.pool_size = 2;
        settings.rss_budget_mb = 2 * 1024;
    });
    let haiku = || RunStatelessRequest {
        model: "claude-haiku-4-5".to_owned(),
        effort: None,
        prompt: "cheap".to_owned(),
        deadline_unix_ms: None,
    };

    // Slot 0: an IDLE haiku instance -- the cold swap's only possible victim.
    harness.pool.run(haiku()).await.expect("haiku answers");
    harness.spawner.drain().await;
    // Slot 1: an opus instance still CLEARING, because this clear is not drained.
    harness.pool.run(ask("first opus")).await.expect("answers");
    let census = harness.pool.census().await;
    assert_eq!(census.idle, 1, "the haiku instance is the swap victim");
    assert_eq!(census.clearing, 1, "the opus instance is coming back");

    // A second opus caller. Nothing opus is idle, no slot is free, and an
    // instance of another class IS idle -- the exact shape rule 3 fires on.
    let pool = Arc::clone(&harness.pool);
    let waiting = tokio::spawn(async move { pool.run(ask("second opus")).await });
    tokio::task::yield_now().await;
    assert!(
        !waiting.is_finished(),
        "the caller took the warm haiku instance while an opus slot was 730 ms from free"
    );
    assert_eq!(
        harness.host.journal().await.destroys.len(),
        0,
        "nothing may be destroyed while the caller can still wait for its own class"
    );

    harness.spawner.drain().await;
    let result = waiting
        .await
        .expect("the waiting task")
        .expect("the clearing opus instance comes back and serves this caller");
    assert_eq!(result.text, "answered: second opus");

    let journal = harness.host.journal().await;
    assert_eq!(
        journal.mints.len(),
        2,
        "no third mint: the caller was served warm by the instance it waited for"
    );
    assert_eq!(
        journal.destroys.len(),
        0,
        "the idle haiku instance of another class was never destroyed"
    );
    let census = harness.pool.census().await;
    assert_eq!(census.live, 2);
    assert_invariants(&harness).await;
}

/// A deferred cold swap FIRES when the wait runs out. It never becomes a
/// refusal.
///
/// The guarantee rule 3 exists for -- "no caller is refused while an instance
/// of another class sits idle" -- has to survive the deferral, and the only
/// thing standing between the two is `may_wait_longer` being false on the last
/// look. Without that, a caller whose budget expired would be refused beside a
/// perfectly good victim, which is the permanent starvation rule 3 was written
/// against, reintroduced by the fix for a different starvation.
///
/// The clear is deliberately never drained, so the pool holds a slot that is
/// "coming back" for the whole ceiling and the deferral is live the entire time.
/// This test therefore costs one `ADMISSION_WAIT_CEILING_MS` of real time, and
/// that is the price of asserting the last look rather than assuming it.
#[tokio::test]
async fn a_deferred_cold_swap_fires_when_the_wait_runs_out_rather_than_refusing() {
    let harness = build(|settings| {
        settings.pool_size = 2;
        settings.rss_budget_mb = 2 * 1024;
    });
    harness
        .pool
        .run(RunStatelessRequest {
            model: "claude-haiku-4-5".to_owned(),
            effort: None,
            prompt: "cheap".to_owned(),
            deadline_unix_ms: None,
        })
        .await
        .expect("haiku answers");
    harness.spawner.drain().await;
    harness.pool.run(ask("first opus")).await.expect("answers");
    let census = harness.pool.census().await;
    assert_eq!((census.idle, census.clearing), (1, 1));

    let started = std::time::Instant::now();
    let result = harness
        .pool
        .run(ask("second opus"))
        .await
        .expect("a caller out of wait is cold-swapped in, never refused");
    let elapsed = started.elapsed();
    assert_eq!(result.text, "answered: second opus");
    assert!(
        elapsed >= Duration::from_millis(pool_config::ADMISSION_WAIT_CEILING_MS),
        "the deferral must have been live for the whole wait, or this proves nothing: {elapsed:?}"
    );

    let journal = harness.host.journal().await;
    assert_eq!(
        journal.destroys.len(),
        1,
        "the idle haiku instance was the victim, once the wait was spent"
    );
    assert_eq!(
        journal.mints.len(),
        3,
        "the swapped slot was re-minted for the class that asked"
    );
    assert_eq!(
        journal.mints[2].epoch, 1,
        "the reclaimed slot mints at a fresh epoch, so an orphan can never share a directory"
    );
    assert_invariants(&harness).await;
}

// ---------------------------------------------------------------------------
// The governing asymmetry: any non-delivered outcome destroys the instance
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_turn_that_did_not_complete_never_returns_its_instance_to_the_pool() {
    // A table over every non-delivered outcome. For each: the caller sees that
    // code, the instance's tree is gone, its process was force-closed, and the
    // NEXT call is served by a different (slot, epoch).
    let outcomes = [
        (ErrorCode::TurnTimeout, "deadline expiry"),
        (ErrorCode::DaemonLost, "actor safety guard"),
        (ErrorCode::ClaudeExited, "the child died"),
        (ErrorCode::NeedsInput, "an unattended modal"),
        (ErrorCode::SchemaDrift, "a transcript pmux cannot read"),
    ];

    for (code, why) in outcomes {
        let harness = build(|_| {});
        harness
            .host
            .fail_next_turn(ErrorBody::new(code, why.to_owned()))
            .await;

        let refusal = harness
            .pool
            .run(ask("first"))
            .await
            .expect_err("a failed turn refuses");
        assert_eq!(refusal.code, code, "{why}");

        let journal = harness.host.journal().await;
        assert_eq!(
            journal.destroys.len(),
            1,
            "{why}: the instance was destroyed"
        );
        assert!(
            journal.tree_present_at_destroy[0],
            "{why}: the root must still exist when close is called"
        );
        assert!(
            trees(&harness.parent).is_empty(),
            "{why}: the tree must be gone afterwards"
        );

        // The epoch inequality is the assertion that catches a "return to idle"
        // arm: a reused instance would serve the next call from the same tree.
        harness
            .pool
            .run(ask("second"))
            .await
            .expect("second answers");
        let journal = harness.host.journal().await;
        assert_eq!(journal.mints.len(), 2, "{why}");
        assert_ne!(
            (journal.mints[0].slot, journal.mints[0].epoch),
            (journal.mints[1].slot, journal.mints[1].epoch),
            "{why}: a non-delivered turn must not leave its instance serviceable"
        );
        assert_invariants(&harness).await;
    }
}

/// Either half of the sidechain guard refuses on its own.
///
/// This half had no test at all while the production host reported
/// `sidechain_rows: None`: the test beside this one sets a ROW COUNT, so
/// deleting `|| turn.usage.sidechain != Default::default()` from `commit` left
/// the entire suite green while removing the only protection production had.
/// That is a guard whose message promises more than its predicate tests,
/// arriving through the half nobody exercised. Production now counts rows, so
/// both halves are live in a real daemon -- and both are still checked
/// separately here, because a guard whose halves are only ever exercised
/// together is one where either half may already be dead.
#[tokio::test]
async fn sidechain_tokens_alone_refuse_the_turn_even_with_no_row_count() {
    let harness = build(|_| {});
    {
        let mut script = harness.host.script.lock().await;
        // Counted, and counted ZERO. The refusal must come from the tokens.
        script.sidechain_rows = 0;
        script.sidechain_tokens = TokenUsage {
            input_tokens: 3,
            output_tokens: 7,
            ..TokenUsage::default()
        };
    }

    let refusal = harness
        .pool
        .run(ask("hello"))
        .await
        .expect_err("sidechain token usage on a tool-less cell is not a turn pmux commits");
    assert_eq!(refusal.code, ErrorCode::SchemaDrift);
    assert_eq!(
        refusal.details.get("violation").and_then(|v| v.as_str()),
        Some("sidechain_row_on_toolless_cell")
    );
    // The reported count is 0, honestly: the host counted no rows, and the
    // refusal rests on the usage it DID observe rather than on a number nobody
    // established.
    assert_eq!(
        refusal
            .details
            .get("sidechain_rows")
            .and_then(serde_json::Value::as_u64),
        Some(0)
    );
    assert!(trees(&harness.parent).is_empty());
    assert_invariants(&harness).await;
}

/// A host that did not COUNT the sidechain rows cannot commit the turn.
///
/// `HostTurn::sidechain_rows` is an `Option` so a host that cannot count is able
/// to say so rather than fabricate a zero. The pool's half of that bargain is to
/// treat the silence as a FAILED check, and it used to do the opposite:
/// `unwrap_or(0)`, beside a comment claiming the token check made that safe. It
/// did not. The residue was exact -- a sidechain row that carried no usage at
/// all leaves `usage.sidechain` at its default, so both halves of the guard
/// passed and the turn committed with its isolation claim unmade.
///
/// This is that turn, and the two facts that make it the dangerous one are set
/// TOGETHER on purpose: rows are not counted AND the tokens are clean. Neither
/// existing test reaches it.
#[tokio::test]
async fn a_host_that_did_not_count_sidechain_rows_refuses_rather_than_reading_zero() {
    let harness = build(|_| {});
    {
        let mut script = harness.host.script.lock().await;
        script.sidechain_rows_uncounted = true;
        // Clean tokens: the other half of the guard has nothing to fire on, so
        // a `commit` that read `None` as `0` would commit this turn.
        script.sidechain_tokens = TokenUsage::default();
    }

    let refusal = harness
        .pool
        .run(ask("hello"))
        .await
        .expect_err("a turn whose isolation claim was never checked is not one pmux commits");
    assert_eq!(refusal.code, ErrorCode::UnsupportedFeature);
    assert_eq!(
        refusal.details.get("violation").and_then(|v| v.as_str()),
        Some("sidechain_rows_not_counted")
    );
    assert!(
        !refusal.retryable,
        "the same host will answer None again, so nothing is gained by retrying"
    );
    // The instance goes with it, exactly as it does for a positive count.
    assert!(trees(&harness.parent).is_empty());
    assert_invariants(&harness).await;
}

/// Control: a turn with neither rows nor sidechain tokens commits.
///
/// Without this the test above would also pass if `commit` refused every turn.
#[tokio::test]
async fn a_turn_with_no_sidechain_evidence_at_all_commits() {
    let harness = build(|_| {});
    let result = harness
        .pool
        .run(ask("hello"))
        .await
        .expect("a clean turn commits");
    assert_eq!(result.usage.sidechain, TokenUsage::default());
    assert_invariants(&harness).await;
}

#[tokio::test]
async fn a_sidechain_row_on_a_toolless_cell_refuses_rather_than_undercounting() {
    let harness = build(|_| {});
    harness.host.script.lock().await.sidechain_rows = 2;

    let refusal = harness
        .pool
        .run(ask("hello"))
        .await
        .expect_err("a sidechain row on a tool-less cell is not a turn pmux commits");
    assert_eq!(refusal.code, ErrorCode::SchemaDrift);
    assert_eq!(
        refusal.details.get("violation").and_then(|v| v.as_str()),
        Some("sidechain_row_on_toolless_cell")
    );
    // Under-reporting the turn's tokens there would be a wrong answer; refusing
    // is merely bad. And the instance goes with it.
    assert!(trees(&harness.parent).is_empty());
    assert_invariants(&harness).await;
}

#[tokio::test]
async fn a_clear_that_typed_nothing_destroys_a_coherent_instance() {
    let harness = build(|_| {});
    harness.pool.run(ask("hello")).await.expect("answers");
    harness
        .host
        .fail_next_clear(ClearFailure {
            error: ErrorBody::new(ErrorCode::TranscriptUnavailable, "no transcript to watch"),
            clear_not_submitted: true,
            preamble_mismatch: None,
        })
        .await;
    harness.spawner.drain().await;

    let journal = harness.host.journal().await;
    assert_eq!(journal.destroys.len(), 1);
    assert!(trees(&harness.parent).is_empty());
    assert_eq!(
        harness.pool.census().await.halted,
        None,
        "a coherent clear failure is one bad instance, not a pool-wide halt"
    );
    assert_invariants(&harness).await;
}

#[tokio::test]
async fn a_clear_that_may_have_typed_quarantines_and_retains_its_evidence() {
    let temp = tempfile::tempdir().expect("tempdir");
    let retain = temp.path().join("evidence");
    let harness = build(|settings| settings.retain_dir = Some(retain.clone()));

    harness
        .pool
        .run(ask("a secret prompt"))
        .await
        .expect("answers");
    harness
        .host
        .fail_next_clear(ClearFailure {
            error: ErrorBody::new(ErrorCode::TranscriptUnavailable, "rotation never resolved"),
            clear_not_submitted: false,
            preamble_mismatch: None,
        })
        .await;
    harness.spawner.drain().await;

    assert!(trees(&harness.parent).is_empty(), "the tree left the pool");
    let kept: Vec<_> = std::fs::read_dir(&retain)
        .expect("retain dir exists")
        .flatten()
        .collect();
    assert_eq!(kept.len(), 1, "a quarantine keeps its evidence");
    let history = kept[0].path().join("root/history.jsonl");
    assert!(
        std::fs::read_to_string(&history)
            .expect("history retained")
            .contains("a secret prompt"),
        "a quarantine is exactly the case where an operator has something to read"
    );
    assert_invariants(&harness).await;
}

/// **The corpus for the next Claude Code version accumulates from ordinary
/// traffic**, and it holds the measurement and none of the content.
///
/// `docs/version-drift.md` sec.2.2 is why: 178 of the 186 reachable arrivals
/// behind the shipped drain came out of pmux's own PAID campaign directories,
/// and sec.2.1 is why re-analysis cannot replace them -- at a new Claude Code
/// version there are no `cli` turns to read. This is the only proposal in that
/// document that changes the shape of the problem rather than its price, and it
/// only works if it runs on the ordinary teardown path, which is what this
/// asserts.
///
/// The two halves are inseparable and both are here: the mirror keeps the
/// version, the entrypoint, the row kinds and the arrival timestamps -- the
/// whole of what the drain is measured from -- and it does not keep the
/// caller's prompt or the model's answer, which the SAME instance's
/// `history.jsonl` still holds until it is erased four lines later.
#[tokio::test]
async fn ordinary_path_b_traffic_retains_its_own_drain_evidence_and_no_content() {
    let temp = tempfile::tempdir().expect("tempdir");
    let evidence = temp.path().join("path-b-evidence");
    let harness = build(|settings| {
        settings.evidence_dir = Some(evidence.clone());
        // One turn per instance, so the ordinary healthy path ends in a
        // teardown rather than in a still-live slot.
        settings.recycle_turns = 1;
    });

    let secret = "a caller's private prompt about their salary";
    harness.pool.run(ask(secret)).await.expect("answers");
    harness.spawner.drain().await;
    assert!(
        trees(&harness.parent).is_empty(),
        "the instance tree is erased, which is exactly why the mirror had to be taken first"
    );

    let kept: Vec<_> = std::fs::read_dir(&evidence)
        .expect("the evidence directory exists")
        .flatten()
        .map(|entry| entry.path())
        .collect();
    assert_eq!(
        kept.len(),
        1,
        "one instance served one turn and left one mirror"
    );
    let mirrored = std::fs::read_to_string(&kept[0]).expect("the mirror is readable");

    assert!(
        !mirrored.contains(secret),
        "the retained corpus reproduced the caller's prompt: {mirrored}"
    );
    assert!(
        !mirrored.contains("answered:"),
        "the retained corpus reproduced the model's answer: {mirrored}"
    );
    for owed in [
        "\"2.1.220\"",
        "\"cli\"",
        "\"turn_duration\"",
        "\"assistant\"",
        "2026-08-09T10:00:01.150Z",
    ] {
        assert!(
            mirrored.contains(owed),
            "the retained corpus dropped {owed}, which the drain is measured from: {mirrored}"
        );
    }

    // OFF is off. The same ordinary turn, on a pool configured with no
    // evidence directory, against the directory that already holds one mirror:
    // the count must not move. Asserting that some never-configured path does
    // not exist would pass with the feature deleted.
    let disabled = build(|settings| {
        settings.evidence_dir = None;
        settings.recycle_turns = 1;
    });
    disabled.pool.run(ask(secret)).await.expect("answers");
    disabled.spawner.drain().await;
    assert_eq!(
        std::fs::read_dir(&evidence).expect("still there").count(),
        1,
        "a pool with no evidence directory retained something anyway"
    );
    assert_invariants(&harness).await;
    assert_invariants(&disabled).await;
}

/// **EVERY** refusal that says the installed Claude's post-`/clear` preamble
/// moved halts the pool -- not just the one that says `/clear` selected another
/// command.
///
/// The set is DERIVED from `AssertEmptyRefusal::is_a_version_drift_signal`, the
/// one exhaustive classification, rather than written out here. That is the
/// whole change: this test used to name `wrong_local_command` and only
/// `wrong_local_command` halted, while the doc on the predicate that read it
/// already claimed the general thing -- *"pmux's model of the composer no
/// longer matching the installed Claude"* -- which is equally true of a
/// preamble carrying a metadata record type, a system subtype, a row shape or a
/// row count nobody has measured. Each of those quarantined ONE instance and
/// the pool minted the next one straight into the identical drift.
///
/// Re-promotion trigger 4, `docs/version-drift.md` sec.5 P2.
#[tokio::test]
async fn every_preamble_mismatch_halts_the_whole_pool_and_not_only_a_mis_selected_command() {
    let drifted: Vec<&'static str> = AssertEmptyRefusal::ALL
        .iter()
        .filter(|refusal| refusal.is_a_version_drift_signal())
        .map(|refusal| refusal.reason())
        .collect();
    assert!(
        drifted.len() > 1,
        "the drift classification is back to one reason, which is the defect: {drifted:?}"
    );

    for reason in drifted {
        let harness = build(|_| {});
        harness.pool.run(ask("hello")).await.expect("answers");
        harness
            .host
            .fail_next_clear(ClearFailure {
                error: ErrorBody::new(
                    ErrorCode::SchemaDrift,
                    "the cleared preamble is not the one pmux measured",
                ),
                clear_not_submitted: false,
                preamble_mismatch: Some(reason),
            })
            .await;
        harness.spawner.drain().await;

        assert_eq!(
            harness.pool.census().await.halted,
            Some(reason),
            "{reason} is not one bad instance: it is pmux's model of the post-/clear preamble no \
             longer matching Claude, and the halt has to carry WHICH part moved"
        );
        let refusal = harness
            .pool
            .run(ask("next"))
            .await
            .expect_err("a halted pool refuses every checkout");
        assert_eq!(refusal.code, ErrorCode::SchemaDrift);
        assert_eq!(
            refusal.details["repromotion_trigger"].as_str(),
            Some("clear_screen_or_preamble_mismatch"),
            "the refusal must name the trigger an operator has to act on: {}",
            refusal.details
        );
        assert_invariants(&harness).await;
    }
}

/// The other half: a refusal about THIS INSTANCE quarantines it and leaves the
/// pool serving.
///
/// Also derived from the same classification, because a rule that halted on
/// everything would pass the test above and take Path B down on one leaked
/// transcript.
#[tokio::test]
async fn a_refusal_about_one_instance_does_not_halt_the_pool() {
    for refusal in AssertEmptyRefusal::ALL
        .iter()
        .filter(|refusal| !refusal.is_a_version_drift_signal())
    {
        let harness = build(|_| {});
        harness.pool.run(ask("hello")).await.expect("answers");
        harness
            .host
            .fail_next_clear(ClearFailure {
                error: ErrorBody::new(ErrorCode::SchemaDrift, refusal.reason()),
                clear_not_submitted: false,
                preamble_mismatch: None,
            })
            .await;
        harness.spawner.drain().await;

        assert_eq!(
            harness.pool.census().await.halted,
            None,
            "{} is a fact about one instance and must not stop the pool",
            refusal.reason()
        );
        harness
            .pool
            .run(ask("next"))
            .await
            .expect("the pool keeps serving after one instance is quarantined");
        assert_invariants(&harness).await;
    }
}

// ---------------------------------------------------------------------------
// Teardown ordering and leakage
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_unreaped_instance_leaks_its_slot_and_keeps_its_root() {
    let harness = build(|settings| {
        settings.pool_size = 2;
        settings.rss_budget_mb = 2 * 1024;
        settings.recycle_turns = 1;
    });
    harness.host.script.lock().await.never_reaps = true;

    // recycle_turns = 1, so the first clear recycles and the teardown runs.
    harness.pool.run(ask("hello")).await.expect("answers");
    harness.spawner.drain().await;

    let journal = harness.host.journal().await;
    assert_eq!(journal.destroys.len(), 1);
    assert!(
        journal.tree_present_at_destroy[0],
        "nothing on disk is touched before the process is proven reaped"
    );
    assert_eq!(
        trees(&harness.parent).len(),
        1,
        "a root a live process may still be writing to is evidence, not garbage"
    );

    let census = harness.pool.census().await;
    assert_eq!(census.leaked, 1);
    assert_eq!(
        census.capacity, 1,
        "the slot is permanently subtracted from the budget"
    );

    // The pool now behaves like a pool of one: one call succeeds, the next
    // refuses, rather than reusing the slot whose process could not be proven
    // gone.
    harness
        .pool
        .run(ask("one"))
        .await
        .expect("the surviving slot serves");
    let refusal = harness
        .pool
        .run(ask("two"))
        .await
        .expect_err("the leaked slot is not reused");
    assert_eq!(refusal.code, ErrorCode::SessionBusy);
    // The budget the refusal NAMES is the subtracted one, not the configured
    // one. `pool_exhausted` used to be handed `config.pool_size`, so a pool
    // that had permanently lost a slot went on saying "its 2 configured
    // instances" for the rest of the process's life -- an operator sizing a
    // retry off that number is sizing it off capacity that no longer exists.
    assert_eq!(
        refusal
            .details
            .get("budget_instances")
            .and_then(serde_json::Value::as_u64),
        Some(u64::from(census.capacity)),
        "the refusal must name the capacity the census reports: {}",
        refusal.message
    );
    assert_eq!(
        refusal
            .details
            .get("configured_instances")
            .and_then(serde_json::Value::as_u64),
        Some(2),
        "...and still publish what was configured, so the loss is legible"
    );
    assert!(
        refusal.message.contains("1 of 1 usable instance(s)")
            && refusal
                .message
                .contains("against 2 configured before 1 slot(s) leaked permanently"),
        "the message must say the pool is permanently smaller: {}",
        refusal.message
    );
    assert_invariants(&harness).await;
}

#[tokio::test]
async fn destruction_erases_every_residue_channel_under_one_root() {
    let harness = build(|settings| settings.recycle_turns = 1);
    harness
        .pool
        .run(ask("a caller secret"))
        .await
        .expect("answers");

    let journal = harness.host.journal().await;
    let root = journal.served_roots[0].clone();
    // Plant one file per residue channel Claude is known to write, so the
    // assertion below is about the ROOT rather than about a list of names --
    // the list is Claude's to extend and an allowlist of two names is the only
    // form that stays true.
    for channel in [
        "history.jsonl",
        "paste-cache",
        "projects",
        "backups",
        ".claude.json",
        "settings.json",
        "shell-snapshots",
        "debug",
        "cache",
    ] {
        let path = root.join(channel);
        std::fs::create_dir_all(&path).ok();
        std::fs::write(root.join(format!("{channel}.residue")), b"a caller secret").ok();
    }

    harness.spawner.drain().await;
    assert!(
        !root.exists(),
        "one erase of one root destroys every channel at once"
    );
    assert!(trees(&harness.parent).is_empty());
    assert_invariants(&harness).await;
}

// ---------------------------------------------------------------------------
// Recycle
// ---------------------------------------------------------------------------

#[tokio::test]
async fn recycle_bounds_the_turns_one_root_serves_and_then_destroys_it() {
    // A pool of one, so the only instance in play is the one under test: with
    // spare capacity the high-water re-warm would (correctly) mint a second
    // instance of this class, and the mint count would stop being a statement
    // about recycle.
    let harness = build(|settings| {
        settings.pool_size = 1;
        settings.rss_budget_mb = 1024;
        settings.recycle_turns = 3;
    });

    let mut secrets = Vec::new();
    // The first two turns stay below the cap, so one root accumulates both.
    for index in 0..2 {
        let secret = format!("unguessable-secret-{index}-9f3a");
        secrets.push(secret.clone());
        harness.pool.run(ask(&secret)).await.expect("answers");
        harness.spawner.drain().await;
    }

    let journal = harness.host.journal().await;
    let first_root = journal.served_roots[0].clone();
    assert!(
        journal.served_roots.iter().all(|root| *root == first_root),
        "turns below the cap are served by one instance"
    );
    let history = std::fs::read_to_string(first_root.join("history.jsonl")).unwrap_or_default();
    for secret in &secrets {
        assert!(
            history.contains(secret.as_str()),
            "the residue channel this cap bounds is per-root and spans every clear"
        );
    }

    // The third turn reaches the cap, so its clear recycles rather than
    // returning the instance to service, and the root is destroyed with it.
    secrets.push("unguessable-secret-2-9f3a".to_owned());
    harness.pool.run(ask(&secrets[2])).await.expect("answers");
    harness.spawner.drain().await;
    assert!(
        !first_root.exists(),
        "the clear at the cap recycled: the root is destroyed, not held"
    );
    harness.pool.run(ask("fourth")).await.expect("answers");
    harness.spawner.drain().await;
    let journal = harness.host.journal().await;
    assert_eq!(journal.mints.len(), 2, "exactly one re-mint");
    assert_ne!(journal.mints[0].epoch, journal.mints[1].epoch);
    assert_invariants(&harness).await;
}

#[tokio::test]
async fn a_submitted_turn_that_failed_still_advances_the_counter() {
    // The counter is incremented at CHECKOUT, not at check-in, because a prompt
    // reaches `history.jsonl` at submission. A counter incremented at check-in
    // would miscount exactly this turn.
    let harness = build(|settings| settings.recycle_turns = 2);
    harness
        .host
        .fail_next_turn(ErrorBody::new(ErrorCode::TurnTimeout, "deadline"))
        .await;
    let _ = harness.pool.run(ask("submitted then failed")).await;
    harness.spawner.drain().await;

    // The failed turn destroyed its instance, so the counter's effect is
    // observed on a fresh one: two successful turns must recycle.
    harness.pool.run(ask("one")).await.expect("answers");
    harness.spawner.drain().await;
    harness.pool.run(ask("two")).await.expect("answers");
    harness.spawner.drain().await;
    let journal = harness.host.journal().await;
    assert_eq!(
        journal.destroys.len(),
        2,
        "the failed turn destroyed one instance and the cap recycled the next"
    );
    assert_invariants(&harness).await;
}

// ---------------------------------------------------------------------------
// Latency: the caller never waits on the clear
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_caller_has_its_answer_before_the_clear_has_begun() {
    let harness = build(|_| {});
    let result = harness.pool.run(ask("hello")).await.expect("answers");
    assert_eq!(result.text, "answered: hello");

    // The clear is still an undrained future: `run` returned the bytes without
    // waiting on it. A slow clear costs capacity, never latency.
    assert_eq!(harness.spawner.pending(), 1);
    assert!(
        harness.host.journal().await.clears.is_empty(),
        "no clear may have been typed before the caller had its answer"
    );

    harness.spawner.drain().await;
    assert_eq!(harness.host.journal().await.clears.len(), 1);
    assert_invariants(&harness).await;
}

// ---------------------------------------------------------------------------
// Warming
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_operator_declared_warm_set_is_minted_at_boot() {
    let harness = build(|settings| {
        settings.pool_size = 3;
        settings.rss_budget_mb = 3 * 1024;
        settings.warm_set = vec![WarmClassSetting {
            model: "claude-opus-5".to_owned(),
            effort: Some(EffortLevel::High),
            count: 2,
        }];
    });
    harness.pool.start().await.expect("the warm set mints");

    let census = harness.pool.census().await;
    assert_eq!(census.idle, 2, "declared capacity exists before any caller");
    assert_eq!(harness.host.journal().await.mints.len(), 2);

    // A caller of that shape pays no launch.
    harness.pool.run(ask("hello")).await.expect("answers");
    assert_eq!(
        harness.host.journal().await.mints.len(),
        2,
        "a warm caller mints nothing"
    );
    assert_invariants(&harness).await;
}

/// The predicate behind `mint_roots`'s refusal sentence.
///
/// The message an operator gets now says three things: a previous daemon did
/// not shut down cleanly, the pool does not adopt the tree, and **this mint
/// erases that tree as it fails, so a repeated start passes this slot**. The
/// first is context and the second is the rule; the third is a promise about
/// what pmux DID, and it is the one a message must not make without a test.
///
/// It is asserted on the disk rather than on the message, because the message
/// is the thing under suspicion.
#[tokio::test]
async fn a_refused_epoch_tree_is_erased_by_the_start_that_refused_it() {
    let harness = build(|settings| {
        settings.pool_size = 2;
        settings.warm_set = vec![WarmClassSetting {
            model: "claude-opus-5".to_owned(),
            effort: Some(EffortLevel::High),
            count: 1,
        }];
    });

    // Residue in the shape a killed daemon leaves it: slot 0, epoch 0, with a
    // file inside, so "erased" cannot be satisfied by an empty-directory
    // shortcut. Planted through the same owner-only helper the pool creates its
    // parent with, because `Pool::start` refuses a parent that is not 0700
    // before it looks at a single slot.
    plant_residue(&harness.parent, &["0/0/root"]);
    std::fs::write(
        harness.parent.join("0/0/root").join("leftover"),
        b"previous daemon",
    )
    .expect("the residue holds a file");

    let error = harness
        .pool
        .start()
        .await
        .expect_err("a pool never adopts a tree it did not create");
    assert!(
        error.message.contains("did not shut down cleanly")
            && error
                .message
                .contains("never adopts a tree it did not create")
            && error.message.contains("a repeated start passes this slot"),
        "the refusal must name the situation and what it did about it: {}",
        error.message
    );

    assert!(
        trees(&harness.parent).is_empty(),
        "the refusal promised the tree was erased and it is still there: {:?}",
        trees(&harness.parent)
    );
    assert_eq!(
        harness.host.journal().await.mints.len(),
        0,
        "the collision is decided before any child is launched"
    );
    assert_invariants(&harness).await;
}

/// A warm mint that fails partway leaves every instance it DID mint reachable
/// by a drain, and a drain erases all of them.
///
/// This is what bounds the recovery chain, and the bound is not linear without
/// it. MEASURED at the pre-fix shape, real Claude 2.1.226,
/// `--path-b-warm claude-sonnet-5/low=3` SIGTERM'd 2.6 s into its mint: the
/// three abandoned epoch trees took **seven** consecutive refusing restarts
/// before one served. A failed start erases the one tree it collides with and
/// abandons every tree it minted before reaching it, so the leftover set moves
/// `L -> (L \ {min L}) union {0..min L - 1}` -- which shrinks a potential of
/// `sum over i in L of 2^(i+1) - 1` by exactly `min L + 1` per restart, i.e.
/// `2^w - 1` restarts for a warm set of `w` abandoned at the top.
///
/// `NativeService::start` is the caller that closes it: a `start_pool` failure
/// now drains the service before returning, which turns the recurrence into
/// `L -> L \ {min L}`. This test owns the pool half of that -- that a drain
/// after a failed `start` reaches the earlier instances at all.
#[tokio::test]
async fn a_partly_minted_warm_set_is_still_the_pools_to_drain() {
    let harness = build(|settings| {
        settings.pool_size = 3;
        settings.rss_budget_mb = 3 * 1024;
        settings.warm_set = vec![WarmClassSetting {
            model: "claude-opus-5".to_owned(),
            effort: Some(EffortLevel::High),
            count: 3,
        }];
    });

    // The collision is on the THIRD slot, so two instances are minted first --
    // the case that was abandoned, and the reason the chain grew instead of
    // shrinking.
    plant_residue(&harness.parent, &["2/0"]);

    harness
        .pool
        .start()
        .await
        .expect_err("the third mint collides with a tree the pool did not create");

    let journal = harness.host.journal().await;
    assert_eq!(journal.mints.len(), 2, "two instances were minted first");
    let census = harness.pool.census().await;
    assert_eq!(
        census.live, 2,
        "the census must not under-report what the failed start is still holding"
    );
    assert_eq!(
        trees(&harness.parent),
        vec!["0/0".to_owned(), "1/0".to_owned()],
        "the refused tree is gone and the two minted ones are still on disk"
    );

    harness.pool.shutdown().await;
    assert_eq!(
        harness.host.journal().await.destroys.len(),
        2,
        "a drain after a failed start must reach both minted instances"
    );
    assert!(
        trees(&harness.parent).is_empty(),
        "a drained pool leaves no tree for the next start to collide with: {:?}",
        trees(&harness.parent)
    );
}

#[tokio::test]
async fn emptying_a_classes_idle_set_mints_a_replacement_immediately() {
    let harness = build(|settings| {
        settings.pool_size = 3;
        settings.rss_budget_mb = 3 * 1024;
        settings.warm_set = vec![WarmClassSetting {
            model: "claude-opus-5".to_owned(),
            effort: Some(EffortLevel::High),
            count: 1,
        }];
    });
    harness.pool.start().await.expect("the warm set mints");
    assert_eq!(harness.host.journal().await.mints.len(), 1);

    // The single warm instance is checked out, which empties the class's idle
    // set at its high-water mark. A replacement is queued at once, so the NEXT
    // caller of this shape finds one warm.
    harness.pool.run(ask("hello")).await.expect("answers");
    assert!(
        harness.spawner.pending() >= 1,
        "a checkout that emptied a class's idle set queues a re-warm"
    );
    harness.spawner.drain().await;
    assert_eq!(
        harness.host.journal().await.mints.len(),
        2,
        "the high-water mark was restored"
    );
    assert!(harness.pool.census().await.idle >= 1);
    assert_invariants(&harness).await;
}

#[tokio::test]
async fn the_idle_ttl_returns_a_cold_classs_slot_but_never_below_the_warm_floor() {
    let harness = build(|settings| {
        settings.pool_size = 3;
        settings.rss_budget_mb = 3 * 1024;
        settings.instance_idle_ttl_ms = 1_000;
        settings.warm_set = vec![WarmClassSetting {
            model: "claude-opus-5".to_owned(),
            effort: Some(EffortLevel::High),
            count: 1,
        }];
    });
    harness.pool.start().await.expect("the warm set mints");

    // A second, undeclared instance of the same class, so the class sits one
    // above its floor.
    harness.pool.run(ask("hello")).await.expect("answers");
    harness.spawner.drain().await;
    harness.pool.run(ask("world")).await.expect("answers");
    harness.spawner.drain().await;
    let before = harness.pool.census().await.idle;
    assert!(before >= 1);

    harness.clock.advance(10_000);
    harness.pool.sweep_idle().await;

    let after = harness.pool.census().await;
    assert_eq!(
        after.idle, 1,
        "the sweep drains a cold class down to its declared floor and no further"
    );
    assert_eq!(after.live, 1);
    assert_invariants(&harness).await;
}

#[tokio::test]
async fn a_class_with_no_declared_floor_is_swept_to_nothing() {
    let harness = build(|settings| settings.instance_idle_ttl_ms = 1_000);
    harness.pool.run(ask("hello")).await.expect("answers");
    harness.spawner.drain().await;
    assert_eq!(harness.pool.census().await.idle, 1);

    // Before the TTL: nothing moves.
    harness.clock.advance(500);
    harness.pool.sweep_idle().await;
    assert_eq!(
        harness.pool.census().await.idle,
        1,
        "the sweep must not evict inside the TTL window"
    );

    harness.clock.advance(1_000);
    harness.pool.sweep_idle().await;
    let census = harness.pool.census().await;
    assert_eq!(census.idle, 0);
    assert_eq!(census.live, 0, "a cold class returns its slot");
    assert!(trees(&harness.parent).is_empty());
    assert_invariants(&harness).await;
}

// ---------------------------------------------------------------------------
// Admission ordering: a bad request never disturbs an instance
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_refused_request_touches_no_instance() {
    let harness = build(|_| {});
    harness.pool.run(ask("hello")).await.expect("answers");
    harness.spawner.drain().await;
    let before = harness.host.journal().await;

    let cases = [
        (
            RunStatelessRequest {
                model: "claude-opus-5".to_owned(),
                effort: Some(EffortLevel::High),
                prompt: String::new(),
                deadline_unix_ms: None,
            },
            ErrorCode::InvalidConfig,
            "an empty prompt",
        ),
        (
            RunStatelessRequest {
                model: "claude-opus-5".to_owned(),
                effort: Some(EffortLevel::High),
                prompt: "/clear".to_owned(),
                deadline_unix_ms: None,
            },
            ErrorCode::UnsupportedFeature,
            "a caller slash command",
        ),
        (
            RunStatelessRequest {
                model: "claude-haiku-4-5".to_owned(),
                effort: Some(EffortLevel::High),
                prompt: "hello".to_owned(),
                deadline_unix_ms: None,
            },
            ErrorCode::InvalidConfig,
            "a tier the model does not take",
        ),
        (
            RunStatelessRequest {
                model: "claude-invented-9".to_owned(),
                effort: None,
                prompt: "hello".to_owned(),
                deadline_unix_ms: None,
            },
            ErrorCode::InvalidConfig,
            "a model with no class key",
        ),
    ];

    for (request, code, why) in cases {
        let refusal = harness
            .pool
            .run(request)
            .await
            .expect_err("an inadmissible request refuses");
        assert_eq!(refusal.code, code, "{why}");
    }

    let after = harness.host.journal().await;
    assert_eq!(
        after.mints.len(),
        before.mints.len(),
        "no instance was minted"
    );
    assert_eq!(
        after.turns.len(),
        before.turns.len(),
        "no turn was submitted"
    );
    assert_eq!(
        after.destroys.len(),
        before.destroys.len(),
        "a bad request must never evict an idle instance"
    );
    assert_eq!(harness.pool.census().await.idle, 1);
    assert_invariants(&harness).await;
}

// ---------------------------------------------------------------------------
// Shutdown
// ---------------------------------------------------------------------------

#[tokio::test]
async fn shutdown_drains_every_instance_and_erases_every_root() {
    let harness = build(|settings| {
        settings.pool_size = 2;
        settings.rss_budget_mb = 2 * 1024;
    });
    harness.pool.run(ask("one")).await.expect("answers");
    harness.spawner.drain().await;
    harness.pool.run(ask("two")).await.expect("answers");
    harness.spawner.drain().await;

    harness.pool.shutdown().await;
    assert!(
        trees(&harness.parent).is_empty(),
        "every root is erased behind shutdown"
    );
    let census = harness.pool.census().await;
    assert_eq!(census.live, 0);

    let refusal = harness
        .pool
        .run(ask("late"))
        .await
        .expect_err("a shutting-down pool refuses");
    assert_eq!(refusal.code, ErrorCode::DaemonLost);
    assert!(refusal.retryable);
    assert_invariants(&harness).await;
}

/// **Shutdown takes the instances that have just answered, too.**
///
/// The test above drains the spawner first, so every instance is `Idle` by the
/// time `shutdown` runs -- which is why it never saw this. A real daemon is
/// stopped whenever it is stopped, and the pool answers its caller BEFORE it
/// types `/clear`, so the ordinary state at the end of a burst of work is
/// "every instance is `Clearing`". `shutdown` skipped all of them under a
/// comment about callers owed an answer, and their roots survived the daemon.
///
/// MEASURED over the real socket before the fix: one `pmux ask`, then SIGTERM,
/// left `<parent>/0/0/root/projects/pmux-e2e/<id>.jsonl` holding that caller's
/// prompt, beside `.claude.json` and `settings.json` -- with `leaked` still 0
/// and not one line logged.
#[tokio::test]
async fn shutdown_drains_the_instances_that_had_just_answered_and_not_yet_cleared() {
    let harness = build(|settings| {
        settings.pool_size = 2;
        settings.rss_budget_mb = 2 * 1024;
    });
    harness.pool.run(ask("one")).await.expect("answers");
    harness.pool.run(ask("two")).await.expect("answers");
    // Deliberately NOT drained: both callers have their answers and both
    // instances are mid-clear, which is the state this test exists for.
    assert_eq!(harness.spawner.pending(), 2, "two clears are still owed");
    let census = harness.pool.census().await;
    assert_eq!(census.clearing, 2);
    assert_eq!(census.idle, 0);
    assert_eq!(trees(&harness.parent).len(), 2);

    harness.pool.shutdown().await;
    assert!(
        trees(&harness.parent).is_empty(),
        "a clearing instance owes its caller nothing, so its root must not outlive the daemon: {:?}",
        trees(&harness.parent)
    );
    let census = harness.pool.census().await;
    assert_eq!(census.live, 0);
    assert_eq!(census.leaked, 0, "nothing was leaked; it was erased");
    assert_eq!(
        harness.host.journal().await.destroys.len(),
        2,
        "both processes were force-closed rather than abandoned to the daemon's exit"
    );
    assert_invariants(&harness).await;
}

/// A caller still waiting for an answer keeps its instance, which is the ONE
/// thing the wildcard arm was right about.
///
/// The distinction the fix turns on: `CheckedOut` and `Delivering` are owed an
/// answer or a refusal, `Clearing` is owed nothing. Without this test the fix
/// could have drained everything and still looked correct.
#[tokio::test]
async fn shutdown_leaves_the_instance_a_caller_is_still_waiting_on() {
    let harness = build(|settings| {
        settings.pool_size = 1;
        settings.rss_budget_mb = 1024;
    });
    harness.pool.run(ask("one")).await.expect("answers");
    harness.spawner.drain().await;

    // Hold one turn open and shut down underneath it.
    harness.host.hold_next_turn().await;
    let pool = Arc::clone(&harness.pool);
    let held = tokio::spawn(async move { pool.run(ask("two")).await });
    harness.host.await_turn_started().await;
    assert_eq!(harness.pool.census().await.in_flight, 1);

    harness.pool.shutdown().await;
    assert_eq!(
        trees(&harness.parent).len(),
        1,
        "an instance a caller is waiting on keeps its root until that caller is answered"
    );
    assert_eq!(harness.pool.census().await.live, 1);

    harness.host.release_held_turn().await;
    held.await.expect("the held turn task").expect("answers");
}

#[tokio::test]
async fn shutdown_leaves_an_unreaped_root_on_disk_and_reports_it() {
    let harness = build(|settings| {
        settings.pool_size = 1;
        settings.rss_budget_mb = 1024;
    });
    harness.pool.run(ask("one")).await.expect("answers");
    harness.spawner.drain().await;
    harness.host.script.lock().await.never_reaps = true;

    harness.pool.shutdown().await;
    assert_eq!(
        trees(&harness.parent).len(),
        1,
        "a root a live process may still be writing to is evidence, not garbage"
    );
    assert_eq!(harness.pool.census().await.leaked, 1);
}

/// A shutdown that lands INSIDE a launch does not get to call the child
/// imaginary.
///
/// `Pool::mint` releases the lock across `InstanceHost::mint`, and for that
/// whole window the instance is `Warming` with `handle: None`. `shutdown`
/// drains `Warming`, so `destroy` reaches an instance whose handle is absent
/// for a reason its `None` arm did not distinguish: **"no handle yet" is not
/// "no process ever"**. `stateless.rs`'s `ask` takes no start guard, so a
/// SIGTERM arriving during a cold caller's launch is the shipped interleaving,
/// not a contrived one.
///
/// MEASURED before the fix, exactly the probe the review recorded:
/// `mints=1 clears=0 destroys=0 leaked=0 trees=[]`. A child had been launched
/// into a root the pool then erased under it, and the census reported a pool
/// with nothing wrong -- which is the part that matters, because a leak the
/// census names is an operator's problem and a leak it does not is nobody's.
#[tokio::test]
async fn a_shutdown_inside_a_launch_reports_the_child_it_could_not_account_for() {
    let harness = build(|settings| {
        settings.pool_size = 1;
        settings.rss_budget_mb = 1024;
    });

    harness.host.mint_gate.hold().await;
    let pool = Arc::clone(&harness.pool);
    let launching = tokio::spawn(async move { pool.run(ask("one")).await });
    harness.host.mint_gate.await_started().await;

    // The daemon stops while the child is being launched.
    harness.pool.shutdown().await;
    harness.host.mint_gate.release().await;
    let refused = launching
        .await
        .expect("the launching caller's task must not panic");
    assert!(
        refused.is_err(),
        "a caller whose slot was torn down mid-launch is refused, not served"
    );

    let census = harness.pool.census().await;
    assert_eq!(
        census.leaked, 1,
        "the pool minted a process it never got a handle for; a slot whose child is \
         unaccounted for is leaked and the census must say so: {census:?}"
    );
    assert_eq!(census.live, 0);
    assert_eq!(
        census.capacity, 0,
        "a leaked slot is permanently subtracted from the budget"
    );
    assert_eq!(
        trees(&harness.parent).len(),
        1,
        "a root a live Claude may still be writing to is evidence, not garbage: {:?}",
        trees(&harness.parent)
    );

    // ...and the handle that arrived too late is not dropped on the floor. The
    // slot stays leaked either way -- the tree was erased-attempt-free and is
    // being kept -- but a close the host can still act on is strictly better
    // than an orphan nobody ever asks about.
    let journal = harness.host.journal().await;
    assert_eq!(journal.mints.len(), 1);
    assert_eq!(
        journal.destroys.len(),
        1,
        "the late handle is spent closing the child it names"
    );
    assert_invariants(&harness).await;
}

/// A shutdown that lands inside a `/clear` does not take the pool's process
/// down with it.
///
/// `spawn_clear` runs the clear on a task nobody waits on, and `shutdown`
/// drains `Clearing` -- so the slot can be reaped, its tree erased and its
/// entry removed while `InstanceHost::clear` is still inside the host. The
/// resume then indexed the map it had just been emptied of:
///
/// ```text
/// panicked at crates/service/src/pool/mod.rs:1069:52: no entry found for key
/// ```
///
/// A panic in a `pub` type on a reachable interleaving, on a task the daemon
/// spawned and nobody joins. The window being narrow is an argument for not
/// panicking about it.
#[tokio::test]
async fn a_shutdown_inside_a_clear_does_not_panic_the_task_running_it() {
    let harness = build(|settings| {
        settings.pool_size = 1;
        settings.rss_budget_mb = 1024;
    });
    harness.pool.run(ask("one")).await.expect("answers");
    assert_eq!(harness.spawner.pending(), 1, "the clear is still owed");

    harness.host.clear_gate.hold().await;
    let spawner = Arc::clone(&harness.spawner);
    let clearing = tokio::spawn(async move { spawner.drain().await });
    harness.host.clear_gate.await_started().await;
    assert_eq!(harness.pool.census().await.clearing, 1);

    // The daemon stops while `/clear` is still in the host.
    harness.pool.shutdown().await;
    assert!(
        trees(&harness.parent).is_empty(),
        "shutdown drains a clearing instance and erases its root: {:?}",
        trees(&harness.parent)
    );
    harness.host.clear_gate.release().await;
    clearing
        .await
        .expect("a clear that outlives its slot must not panic the task that runs it");

    // ...and the teardown that removed it is the only one: the clear finds no
    // instance to return to service and starts no second destruction.
    let census = harness.pool.census().await;
    assert_eq!(census.live, 0);
    assert_eq!(census.idle, 0);
    assert_eq!(
        census.leaked, 0,
        "the process was reaped and the tree erased"
    );
    assert_eq!(harness.host.journal().await.destroys.len(), 1);
    assert_invariants(&harness).await;
}

// ---------------------------------------------------------------------------
// The idle set is never observably stale
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn an_instance_leaves_the_idle_set_at_the_instant_it_stops_being_idle() {
    // A teardown takes seconds and the pool holds no lock across it, so there
    // is a window between "this instance is on its way out" and "this instance
    // is gone". If the idle set were only tidied at the END of that window, a
    // caller arriving inside it would find the slot, try to check out a
    // `Destroying` process, and get an internal error instead of an answer.
    //
    // Observed from a second task rather than argued about: the sweep runs
    // while an invariant checker polls, and the checker sees the idle set only
    // ever naming instances that are actually idle.
    let harness = build(|settings| {
        settings.pool_size = 3;
        settings.rss_budget_mb = 3 * 1024;
        settings.instance_idle_ttl_ms = 1_000;
    });
    // Three distinct classes, so three instances sit idle across three idle
    // sets and the sweep has to tidy all of them.
    for (model, effort) in [
        ("claude-opus-5", Some(EffortLevel::High)),
        ("claude-sonnet-5", Some(EffortLevel::Low)),
        ("claude-haiku-4-5", None),
    ] {
        harness
            .pool
            .run(RunStatelessRequest {
                model: model.to_owned(),
                effort,
                prompt: "hello".to_owned(),
                deadline_unix_ms: None,
            })
            .await
            .expect("answers");
        harness.spawner.drain().await;
    }
    assert_eq!(harness.pool.census().await.idle, 3);
    harness.clock.advance(5_000);

    let pool = Arc::clone(&harness.pool);
    let checker = tokio::spawn(async move {
        for _ in 0..200 {
            pool.check_invariants()
                .await
                .expect("the idle set must never name an instance that stopped being idle");
            tokio::task::yield_now().await;
        }
    });
    harness.pool.sweep_idle().await;
    checker.await.expect("the invariant checker must not panic");

    assert_eq!(harness.pool.census().await.idle, 0);
    assert_eq!(harness.pool.census().await.live, 0);
    assert_invariants(&harness).await;
}

// ---------------------------------------------------------------------------
// The census, and every invariant across a long sequence
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_invariant_holds_across_a_long_mixed_sequence() {
    // A model test in miniature: interleave checkouts, failures, clears,
    // recycles, TTL sweeps and cold swaps, and assert the whole pool invariant
    // after every step rather than only at the end.
    let harness = build(|settings| {
        settings.pool_size = 3;
        settings.rss_budget_mb = 3 * 1024;
        settings.recycle_turns = 2;
        settings.instance_idle_ttl_ms = 5_000;
    });

    let shapes = [
        ("claude-opus-5", Some(EffortLevel::High)),
        ("claude-opus-5", Some(EffortLevel::Low)),
        ("claude-haiku-4-5", None),
        ("claude-sonnet-5", Some(EffortLevel::Max)),
    ];

    let mut codes: BTreeMap<&str, usize> = BTreeMap::new();
    for step in 0..40_u32 {
        let (model, effort) = shapes[(step as usize) % shapes.len()];
        if step % 7 == 3 {
            harness
                .host
                .fail_next_turn(ErrorBody::new(ErrorCode::TurnTimeout, "deadline"))
                .await;
        }
        if step % 11 == 5 {
            harness
                .host
                .fail_next_clear(ClearFailure {
                    error: ErrorBody::new(ErrorCode::TranscriptUnavailable, "nothing typed"),
                    clear_not_submitted: true,
                    preamble_mismatch: None,
                })
                .await;
        }
        let outcome = harness
            .pool
            .run(RunStatelessRequest {
                model: model.to_owned(),
                effort,
                prompt: format!("step {step}"),
                deadline_unix_ms: None,
            })
            .await;
        *codes
            .entry(match &outcome {
                Ok(_) => "ok",
                Err(error) if error.code == ErrorCode::SessionBusy => "busy",
                Err(_) => "failed",
            })
            .or_default() += 1;
        assert_invariants(&harness).await;

        harness.spawner.drain().await;
        assert_invariants(&harness).await;

        if step % 9 == 8 {
            harness.clock.advance(6_000);
            harness.pool.sweep_idle().await;
            assert_invariants(&harness).await;
        }
        harness.clock.advance(50);
    }

    // The sequence must actually have exercised the interesting arms, or the
    // invariant assertions above proved nothing.
    assert!(codes["ok"] > 10, "{codes:?}");
    assert!(codes.get("failed").copied().unwrap_or(0) >= 5, "{codes:?}");
    assert!(
        harness.host.journal().await.destroys.len() >= 5,
        "the sequence must have torn instances down"
    );

    harness.pool.shutdown().await;
    assert_invariants(&harness).await;
    assert!(
        trees(&harness.parent).is_empty(),
        "the pool leaves nothing behind"
    );
}

/// A re-warm is queued when a checkout leaves a class dry BESIDE A FREE SLOT,
/// and in no other case.
///
/// SURVIVING MUTANTS CLOSED: `mod.rs:690 && -> ||`, `mod.rs:690 < -> <=` and
/// `mod.rs:691 && -> ||` -- all three clauses of `should_rewarm` in
/// `Pool::admit_once`'s rule 1.
///
/// `emptying_a_classes_idle_set_mints_a_replacement_immediately` asserts
/// `pending() >= 1`, and A LOWER BOUND CANNOT SEE A SPURIOUS RE-WARM. All three
/// mutants queue background work the pool did not decide to queue, and all three
/// left that assertion true. The cost is not cosmetic: the `690` pair queues a
/// re-warm against a pool with no free slot -- a task that takes the state lock,
/// re-reads the pool and drops it, once per checkout, forever -- and the `691`
/// mutant re-warms a class that still has an idle instance, which is the
/// high-water-mark rule inverted into "mint on every checkout".
///
/// So the count is EXACT here, and the two pieces of background work `run` can
/// queue are named rather than bounded: the post-answer clear, always, and the
/// re-warm, only when rule 1 decided one.
#[tokio::test]
async fn a_re_warm_is_queued_only_when_a_checkout_leaves_the_class_dry_beside_a_free_slot() {
    // (a) A dry class beside a free slot: the clear, and exactly one re-warm.
    let dry = build(|settings| {
        settings.pool_size = 2;
        settings.rss_budget_mb = 2 * 1024;
        settings.warm_set = vec![WarmClassSetting {
            model: "claude-opus-5".to_owned(),
            effort: Some(EffortLevel::High),
            count: 1,
        }];
    });
    dry.pool.start().await.expect("the warm set mints");
    dry.pool.run(ask("hello")).await.expect("answers");
    assert_eq!(
        dry.spawner.pending(),
        2,
        "a checkout that emptied the class's idle set beside a free slot queues \
         the post-answer clear and one re-warm, and nothing else"
    );
    dry.spawner.drain().await;
    assert_eq!(
        dry.host.journal().await.mints.len(),
        2,
        "the high-water mark was restored"
    );
    assert_invariants(&dry).await;

    // (b) A dry class with NO free slot: there is nothing to re-warm into, so
    // rule 1 does not queue one. Both `690` mutants do.
    let full = build(|settings| {
        settings.pool_size = 1;
        settings.rss_budget_mb = 1024;
        settings.warm_set = vec![WarmClassSetting {
            model: "claude-opus-5".to_owned(),
            effort: Some(EffortLevel::High),
            count: 1,
        }];
    });
    full.pool.start().await.expect("the warm set mints");
    full.pool.run(ask("hello")).await.expect("answers");
    assert_eq!(
        full.spawner.pending(),
        1,
        "the pool is at its budget, so the only background work is the clear"
    );
    full.spawner.drain().await;
    assert_eq!(full.host.journal().await.mints.len(), 1);
    assert_invariants(&full).await;

    // (c) A class still holding an idle instance after the checkout: its
    // high-water mark is intact, so nothing is re-warmed. The `691` mutant
    // queues one anyway -- and that one MINTS, because the slot is free.
    let spare = build(|settings| {
        settings.pool_size = 3;
        settings.rss_budget_mb = 3 * 1024;
        settings.warm_set = vec![WarmClassSetting {
            model: "claude-opus-5".to_owned(),
            effort: Some(EffortLevel::High),
            count: 2,
        }];
    });
    spare.pool.start().await.expect("the warm set mints");
    assert_eq!(spare.host.journal().await.mints.len(), 2);
    spare.pool.run(ask("hello")).await.expect("answers");
    assert_eq!(
        spare.spawner.pending(),
        1,
        "the class still holds an idle instance, so there is no high-water mark \
         to restore"
    );
    spare.spawner.drain().await;
    assert_eq!(
        spare.host.journal().await.mints.len(),
        2,
        "a class above its high-water mark must not mint a third instance"
    );
    assert_invariants(&spare).await;
}

/// A re-warm that lands after the pool stopped minting mints nothing.
///
/// SURVIVING MUTANT CLOSED: `mod.rs:899 && -> ||` in `Pool::spawn_rewarm`. The
/// two facts it conjoins are the two reasons a pool stops minting -- the daemon
/// is going away, and the pool has halted because pmux's model of `/clear` no
/// longer matches the installed Claude -- and `||` makes either one alone
/// sufficient to mint. A re-warm is queued before the work that stops the pool
/// runs, by construction: it is queued during admission and the halt is raised
/// by the clear, so the window is not exotic, it is the ordinary order.
///
/// What it costs is a whole config root, a child process and a `history.jsonl`
/// created after the daemon decided to stop creating them: after shutdown the
/// tree outlives the process that was supposed to erase it, and after a halt it
/// is a child launched into a composer pmux has just admitted it cannot drive.
#[tokio::test]
async fn a_queued_re_warm_that_lands_after_the_pool_stopped_minting_mints_nothing() {
    // (a) Shutdown.
    let closing = build(|settings| {
        settings.pool_size = 2;
        settings.rss_budget_mb = 2 * 1024;
        settings.warm_set = vec![WarmClassSetting {
            model: "claude-opus-5".to_owned(),
            effort: Some(EffortLevel::High),
            count: 1,
        }];
    });
    closing.pool.start().await.expect("the warm set mints");
    closing.pool.run(ask("hello")).await.expect("answers");
    assert_eq!(
        closing.spawner.pending(),
        2,
        "the re-warm this test is about is queued and has not run yet"
    );
    closing.pool.shutdown().await;
    closing.spawner.drain().await;
    assert_eq!(
        closing.host.journal().await.mints.len(),
        1,
        "a re-warm that lands after shutdown must mint nothing: the tree it \
         would create outlives the daemon that would have erased it"
    );
    assert!(trees(&closing.parent).is_empty());

    // (b) A halt. Two warm instances, so the first checkout leaves the class
    // idle (no re-warm) and the second empties it (one re-warm, queued BEHIND
    // the first instance's clear) -- and that first clear is the one that halts
    // the pool. The order is the product's own, not a contrivance.
    let halted = build(|settings| {
        settings.pool_size = 3;
        settings.rss_budget_mb = 3 * 1024;
        settings.warm_set = vec![WarmClassSetting {
            model: "claude-opus-5".to_owned(),
            effort: Some(EffortLevel::High),
            count: 2,
        }];
    });
    halted.pool.start().await.expect("the warm set mints");
    halted.pool.run(ask("first")).await.expect("answers");
    halted.pool.run(ask("second")).await.expect("answers");
    assert_eq!(
        halted.spawner.pending(),
        3,
        "two clears and the one re-warm the second checkout queued"
    );
    halted
        .host
        .fail_next_clear(ClearFailure {
            error: ErrorBody::new(
                ErrorCode::SchemaDrift,
                "the rotation was opened by /compact",
            ),
            clear_not_submitted: false,
            preamble_mismatch: Some("wrong_local_command"),
        })
        .await;
    halted.spawner.drain().await;

    assert_eq!(
        halted.pool.census().await.halted,
        Some("wrong_local_command"),
        "the first clear halts the pool, which is what the queued re-warm then \
         runs into"
    );
    assert_eq!(
        halted.host.journal().await.mints.len(),
        2,
        "a halted pool mints nothing: it has just reported that it cannot drive \
         the composer of the Claude it would launch"
    );
    assert_invariants(&halted).await;
}

/// A mint that fails releases its slot and erases the tree it had created.
///
/// SURVIVING MUTANT CLOSED: `mod.rs:876 Pool::abandon_mint with ()`. Nothing in
/// this tree had ever made a mint fail -- `Script::mint_failures` was read and
/// never written -- so the entire compensation path for a launch that did not
/// happen was untested, and deleting it kept the suite green. With it deleted
/// the instance stays `Reserved` forever: the slot is held by a reservation with
/// no process, `live` counts it, no census clause describes it as anything but
/// reserved, and the roots `mint_roots` had already created stay on disk. A pool
/// of two whose Claude is misconfigured is permanently full after two requests.
#[tokio::test]
async fn a_mint_that_fails_releases_its_slot_and_erases_the_tree_it_had_created() {
    let harness = build(|_| {});
    harness
        .host
        .fail_next_mint(HostFailure::reaped(ErrorBody::new(
            ErrorCode::ClaudeExited,
            "the child exited before it reached a prompt",
        )))
        .await;

    let refusal = harness
        .pool
        .run(ask("hello"))
        .await
        .expect_err("a mint failure is refused, not served");
    assert_eq!(refusal.code, ErrorCode::ClaudeExited);

    let census = harness.pool.census().await;
    assert_eq!(
        census.live, 0,
        "an abandoned mint must not go on holding its slot"
    );
    assert_eq!(
        census.leaked, 0,
        "the host proved the child reaped, so this is a released slot and not a \
         permanent capacity loss"
    );
    assert!(
        trees(&harness.parent).is_empty(),
        "the roots the failed mint had already created are erased"
    );
    assert_invariants(&harness).await;

    // ...and the slot really is usable again, which is the fact `live == 0` is
    // a claim about.
    harness
        .pool
        .run(ask("again"))
        .await
        .expect("the released slot serves the next caller");
    assert_invariants(&harness).await;
}

/// The other half of the same compensation: a mint whose child may have
/// survived leaks its slot and KEEPS its tree.
///
/// SURVIVING MUTANT CLOSED: `mod.rs:876 Pool::abandon_mint with ()`, from the
/// other side. The two arms are opposite decisions about the filesystem, and a
/// deleted `abandon_mint` takes neither: with it gone the slot is neither
/// released nor subtracted, so an operator is never paged about a Claude that
/// may still be running against a root pmux is holding.
#[tokio::test]
async fn a_mint_that_may_have_left_a_child_running_leaks_its_slot_and_keeps_its_tree() {
    let harness = build(|_| {});
    harness
        .host
        .fail_next_mint(HostFailure::possibly_live(ErrorBody::new(
            ErrorCode::ClaudeExited,
            "the launcher was killed after the fork",
        )))
        .await;

    harness
        .pool
        .run(ask("hello"))
        .await
        .expect_err("a mint failure is refused, not served");

    let census = harness.pool.census().await;
    assert_eq!(census.live, 0);
    assert_eq!(
        census.leaked, 1,
        "a child that may still be running costs the slot permanently"
    );
    assert_eq!(
        trees(&harness.parent).len(),
        1,
        "the tree is evidence rather than garbage: deleting a config root out \
         from under a live Claude races that process's own writer"
    );
    assert_invariants(&harness).await;
}

/// Retention is for a quarantine, and for nothing else.
///
/// SURVIVING MUTANTS CLOSED: `mod.rs:1286 == -> !=` -- the `was_quarantined |=
/// next == Quarantined` guard in `Pool::transition_locked` -- plus `mod.rs:1098
/// delete !` and `mod.rs:1109 == -> !=`, both in `Pool::finish_turn`.
///
/// `a_clear_that_may_have_typed_quarantines_and_retains_its_evidence` proves the
/// POSITIVE case and configures a retention directory to do it. Nothing proved
/// the negative, and all three mutants live there: `1286` marks every instance
/// quarantined, so a healthy recycled instance's whole config root -- its
/// `history.jsonl`, its `projects/`, its `.claude.json` -- is moved into the
/// operator's evidence directory and kept forever instead of being erased;
/// `1098` reclassifies a clear that positively typed nothing as one that may
/// have typed, which retains the same tree for no reason and quarantines a
/// coherent instance; and `1109` drops the `BeginDestroy` edge out of the
/// quarantine path, leaving the instance stuck in `Quarantined` holding its slot
/// with `Reaped` refused as an illegal transition out of it.
///
/// So all three cases are here, and the zero readings are paired with the one
/// reading that shows retention works at all.
#[tokio::test]
async fn retention_keeps_a_quarantine_and_nothing_else() {
    let temp = tempfile::tempdir().expect("tempdir");
    let retain = temp.path().join("evidence");
    let harness = build(|settings| {
        settings.retain_dir = Some(retain.clone());
        // One turn per instance, so the ordinary healthy path ends in a
        // teardown this test can watch.
        settings.recycle_turns = 1;
    });

    // 1. A clean turn that reaches the recycle cap: destroyed, nothing kept.
    harness.pool.run(ask("first")).await.expect("answers");
    harness.spawner.drain().await;
    assert_eq!(
        harness.pool.census().await.live,
        0,
        "the recycle cap tears the instance down"
    );
    assert!(trees(&harness.parent).is_empty());
    assert_eq!(
        retained(&retain),
        Vec::<String>::new(),
        "a clean recycle keeps nothing: the retention floor exists so an \
         operator has something to read, and a healthy instance is not evidence"
    );

    // 2. A clear that positively typed nothing: coherent, destroyed, and again
    // nothing kept.
    harness
        .host
        .fail_next_clear(ClearFailure {
            error: ErrorBody::new(ErrorCode::TranscriptUnavailable, "no transcript to watch"),
            clear_not_submitted: true,
            preamble_mismatch: None,
        })
        .await;
    harness.pool.run(ask("second")).await.expect("answers");
    harness.spawner.drain().await;
    assert_eq!(harness.pool.census().await.live, 0);
    assert_eq!(
        retained(&retain),
        Vec::<String>::new(),
        "the driver positively claims nothing was typed, so the instance is \
         coherent and there is nothing for an operator to read"
    );

    // 3. A clear that MAY have typed: quarantined, kept, and the slot released.
    harness
        .host
        .fail_next_clear(ClearFailure {
            error: ErrorBody::new(
                ErrorCode::TranscriptUnavailable,
                "the rotation never resolved",
            ),
            clear_not_submitted: false,
            preamble_mismatch: None,
        })
        .await;
    harness
        .pool
        .run(ask("third, and secret"))
        .await
        .expect("answers");
    harness.spawner.drain().await;
    assert_eq!(
        retained(&retain).len(),
        1,
        "a quarantine keeps exactly its own tree, which is what makes the two \
         zeroes above mean something"
    );
    let census = harness.pool.census().await;
    assert_eq!(
        census.live, 0,
        "a quarantined instance leaves through BeginDestroy and its slot is \
         released; without that edge `Reaped` is refused out of `Quarantined` \
         and the instance holds the slot forever"
    );
    assert_eq!(census.leaked, 0);
    assert!(trees(&harness.parent).is_empty());
    assert_invariants(&harness).await;
}
