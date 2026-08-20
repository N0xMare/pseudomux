#![cfg(unix)]
//! **The stateless pool under concurrent load, over the real socket.**
//!
//! Everything the pool claims about concurrency was, before this file, a
//! single-threaded unit-test assertion against an in-process fake host. Four
//! concurrent callers is the most that had ever run against a real daemon. This
//! is the wave harness: N callers, several classes at once, one daemon, one
//! `pmux-test-claude` per instance, real PTYs, real transcripts.
//!
//! # What each wave is allowed to conclude
//!
//! The double does not render an Ink frame, so nothing here establishes
//! anything about real TUI geometry. What it does establish is everything the
//! *pool* decides: admission, class routing, checkout, `/clear`, recycle,
//! refusal at the cap, slot accounting and teardown. Those are decided from
//! bookkeeping and from the transcript, and both are real here.
//!
//! # The one wrong-answer path, and how it is proven closed
//!
//! `StatelessResult::model` is what pmux ASKED for -- it is the class key,
//! copied out of the request path. Asserting it equals the requested model
//! proves nothing about which process answered: a pool that handed every
//! `opus/max` call to a `haiku` instance would still publish `claude-opus-5`
//! in every result. Fungibility is therefore proven from the CHILD side, by
//! joining two files the children write:
//!
//! - `prompts.jsonl` -- one row per accepted submission, carrying the prompt
//!   and the writing process's `cwd`.
//! - `launches.jsonl` -- one row per launch, carrying the full argv and the
//!   same `cwd`.
//!
//! `cwd` is `<parent>/<slot>/<epoch>/cwd`, which exactly one process owns for
//! its whole life, so the join is exact. The argv is read whole; no summary of
//! the class key is restated anywhere in this file's evidence path.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use pseudomux_client::{ClientError, PmuxClient};
use pseudomux_e2e::{
    TEST_ENV_ATTESTATION_MARKER, TEST_ENV_PATCHED_VALUE, TEST_ENV_SAFE_CONFIG_VALUE,
    TEST_ENV_SET_ONLY_VALUE,
};
use pseudomux_protocol::v1::{
    EffortLevel, ErrorBody, ErrorCode, HealthLayerName, LayerFinding, ProbeOutcome,
    RunStatelessRequest, StatelessResult,
};
use serde_json::Value;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::Barrier;
use tokio::task::JoinSet;
use uuid::Uuid;

/// The exact version the double reports, and therefore the only version a
/// promoted compatibility cell may name for this lane.
const DOUBLE_VERSION: &str = "9.9.9";
/// A token no corpus can contain, so the one textually unique prompt in the
/// `/clear` residue measurement can be found in exactly one transcript.
const FILLER_MARKER: &str = "PMUXFILLERMARKERZQ7";
/// A pool instance may not be swept out from under a wave. Ten minutes.
const IDLE_TTL_MS: u64 = 600_000;
/// One stateless turn against the double is milliseconds of work; the ceiling
/// exists so a wedged instance returns its slot inside a test's lifetime rather
/// than the daemon's ten-minute default.
const TURN_TIMEOUT_MS: u64 = 120_000;
/// The one variable the real lane is gated on, named once so that the gate, the
/// skip message and every `#[ignore]` message can be checked against the same
/// string rather than three copies of it.
const REAL_CLAUDE_VARIABLE: &str = "PMUX_POOL_REAL_CLAUDE";

// ---------------------------------------------------------------------------
// Classes
// ---------------------------------------------------------------------------

/// One `(model, effort)` shape a caller may ask for, and the argv it must
/// produce.
///
/// `spelling` is deliberately not always the canonical name: an alias and the
/// canonical id must resolve to ONE class, and a caller who shouts must not
/// partition the pool. `expected_model` and `expected_effort` are what argv has
/// to carry, and they are the only place in this file that names an argv value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ClassSpec {
    spelling: &'static str,
    effort: Option<EffortLevel>,
    expected_model: &'static str,
    expected_effort: Option<&'static str>,
}

/// Four classes across three models, including the two hardest cases: two
/// classes that differ ONLY in effort (one argv token apart, same executable,
/// same model) and one model that renders no `--effort` at all.
const CLASSES: &[ClassSpec] = &[
    ClassSpec {
        spelling: "sonnet",
        effort: Some(EffortLevel::Low),
        expected_model: "claude-sonnet-5",
        expected_effort: Some("low"),
    },
    ClassSpec {
        spelling: "claude-sonnet-5",
        effort: Some(EffortLevel::High),
        expected_model: "claude-sonnet-5",
        expected_effort: Some("high"),
    },
    ClassSpec {
        spelling: "haiku",
        effort: None,
        expected_model: "claude-haiku-4-5",
        expected_effort: None,
    },
    ClassSpec {
        spelling: "OPUS",
        effort: Some(EffortLevel::Medium),
        expected_model: "claude-opus-5",
        expected_effort: Some("medium"),
    },
];

impl ClassSpec {
    fn label(self) -> String {
        match self.expected_effort {
            Some(effort) => format!("{}/{effort}", self.expected_model),
            None => self.expected_model.to_owned(),
        }
    }

    /// The `--pool-warm` declaration for this class, in the operator's own
    /// spelling grammar.
    fn warm(self, count: u32) -> String {
        match self.effort {
            Some(effort) => format!(
                "{}/{}={count}",
                self.spelling,
                serde_json::to_value(effort)
                    .expect("an effort level serializes")
                    .as_str()
                    .expect("an effort level is a string")
            ),
            None => format!("{}={count}", self.spelling),
        }
    }
}

// ---------------------------------------------------------------------------
// Binaries and sandbox
// ---------------------------------------------------------------------------

struct Binaries {
    pmuxd: PathBuf,
    rmuxd: PathBuf,
    launcher: PathBuf,
    double: PathBuf,
    mcp: PathBuf,
}

impl Binaries {
    fn discover() -> Self {
        let directory = std::env::var_os("PMUX_E2E_BIN_DIR")
            .or_else(|| std::env::var_os("PMUX_TEST_BIN_DIR"))
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::current_exe()
                    .expect("the integration-test executable has a path")
                    .parent()
                    .and_then(Path::parent)
                    .expect("the integration-test executable has a candidate directory")
                    .to_path_buf()
            })
            .canonicalize()
            .expect("the candidate binary directory must resolve");
        let executable = |name: &str| {
            let path = directory.join(name);
            let metadata = std::fs::metadata(&path)
                .unwrap_or_else(|error| panic!("candidate {name} is missing: {error}"));
            assert!(metadata.is_file(), "candidate {name} is not a file");
            assert_ne!(
                metadata.permissions().mode() & 0o111,
                0,
                "candidate {name} is not executable"
            );
            path
        };
        let binaries = Self {
            pmuxd: executable("pmuxd"),
            rmuxd: executable("pmux-rmuxd"),
            launcher: executable("pmux-launcher"),
            double: executable("pmux-test-claude"),
            mcp: executable("pmux-mcp"),
        };
        binaries.assert_not_stale();
        binaries
    }

    /// Every candidate binary is at least as new as every source CARGO says it
    /// is built from.
    ///
    /// WHY THIS EXISTS, MEASURED. `cargo test -p pseudomux-e2e --test
    /// pool_concurrency` rebuilds this crate and the libraries it links, and
    /// does NOT rebuild `target/debug/pmuxd`, `pmux-rmuxd`, `pmux-launcher`,
    /// `pmux-mcp` or `pmux-test-claude` -- those are bin targets of other
    /// packages. So every wave in this file runs against whatever daemon was
    /// last built, and a source change that has not been compiled is simply not
    /// under test.
    ///
    /// This was measured, not reasoned about: a deliberate mutation to
    /// `stateless.rs` that makes `Pool::commit` refuse EVERY turn was verified
    /// by running the live MCP wave, and the wave passed -- 1 passed, 0 failed
    /// -- because it drove the previous daemon. A delete-the-check campaign
    /// against these targets was, until this guard, unable to fail.
    ///
    /// The dependency set is READ FROM CARGO, not guessed: `<binary>.d` beside
    /// each candidate is the depinfo cargo itself wrote, listing every file
    /// that binary was compiled from. A hand-rolled rule -- "newer than
    /// anything under `crates/` and `bin/`" -- was tried first and is wrong in
    /// both directions: it marks `pmux-rmuxd` stale for an edit to
    /// `crates/service/src/stateless.rs`, which it does not link, and cannot be
    /// cleared by rebuilding, because a rebuild does not touch the mtime of a
    /// binary whose own inputs did not change.
    fn assert_not_stale(&self) {
        for candidate in [
            &self.pmuxd,
            &self.rmuxd,
            &self.launcher,
            &self.double,
            &self.mcp,
        ] {
            let built = std::fs::metadata(candidate)
                .and_then(|data| data.modified())
                .unwrap_or_else(|error| {
                    panic!("candidate {} has no mtime: {error}", candidate.display())
                });
            let depinfo = candidate.with_extension("d");
            let listing = std::fs::read_to_string(&depinfo).unwrap_or_else(|error| {
                panic!(
                    "candidate {} ships no cargo depinfo at {}, so whether it is stale cannot be \
                     established: {error}",
                    candidate.display(),
                    depinfo.display()
                )
            });
            let sources: Vec<&str> = listing
                .lines()
                .filter_map(|line| line.split_once(": "))
                .flat_map(|(_, dependencies)| dependencies.split_whitespace())
                .collect();
            assert!(
                !sources.is_empty(),
                "the depinfo at {} lists no sources, so 'nothing is stale' says nothing",
                depinfo.display()
            );
            for source in sources {
                let Ok(modified) = std::fs::metadata(source).and_then(|data| data.modified())
                else {
                    continue;
                };
                assert!(
                    built >= modified,
                    "candidate {} is older than {source}, which cargo says it is built from, so \
                     this wave would run against a binary that no longer matches the source; run \
                     `cargo build --workspace --tests` first",
                    candidate.display(),
                );
            }
        }
    }
}

/// Owner-only scratch tree, removed on drop even when an assertion panics.
struct Sandbox {
    _temp: TempDir,
    root: PathBuf,
    socket: PathBuf,
    runtime_parent: PathBuf,
    state_root: PathBuf,
    home: PathBuf,
    pool_parent: PathBuf,
    path_dir: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let temp = tempfile::Builder::new()
            .prefix("pmux-pool-wave-")
            .tempdir_in("/tmp")
            .expect("a scratch root is creatable");
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let root = temp.path().canonicalize().unwrap();
        let runtime_parent = root.join("private");
        let state_root = root.join("state");
        let home = root.join("home");
        let pool_parent = root.join("pool");
        let path_dir = root.join("path");
        for directory in [&runtime_parent, &state_root, &home, &pool_parent, &path_dir] {
            std::fs::create_dir(directory).unwrap();
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self {
            socket: root.join("pmux.sock"),
            _temp: temp,
            root,
            runtime_parent,
            state_root,
            home,
            pool_parent,
            path_dir,
        }
    }

    /// The exact environment `pmuxd` is started under, and therefore -- through
    /// the pool's own snapshot and `build_environment`'s allowlist -- the exact
    /// environment every instance's child is launched under.
    ///
    /// This is the whole reason the double works as a pool instance: the
    /// `PMUX_` prefix is inherited verbatim, so the attestation contract the
    /// double refuses to start without can be handed to it through the DAEMON's
    /// environment. Nothing on the wire contributes a byte of it.
    fn daemon_environment(&self) -> BTreeMap<String, String> {
        let path = self.path_dir.to_string_lossy().into_owned();
        BTreeMap::from([
            ("PATH".to_owned(), path.clone()),
            ("PMUX_TEST_EXPECTED_PATH".to_owned(), path),
            ("HOME".to_owned(), string(&self.home)),
            ("TMPDIR".to_owned(), string(&self.root)),
            ("TERM".to_owned(), "xterm-256color".to_owned()),
            ("PMUX_TEST_STATE_DIR".to_owned(), string(&self.state_root)),
            (
                "PMUX_TEST_ENV_ATTESTATION".to_owned(),
                TEST_ENV_ATTESTATION_MARKER.to_owned(),
            ),
            (
                "PMUX_TEST_PATCH_ORDER".to_owned(),
                TEST_ENV_PATCHED_VALUE.to_owned(),
            ),
            (
                "PMUX_TEST_SET_ONLY".to_owned(),
                TEST_ENV_SET_ONLY_VALUE.to_owned(),
            ),
            (
                "PMUX_TEST_CALLER_SAFE_CONFIG".to_owned(),
                TEST_ENV_SAFE_CONFIG_VALUE.to_owned(),
            ),
        ])
    }
}

fn string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

// ---------------------------------------------------------------------------
// The daemon
// ---------------------------------------------------------------------------

/// Everything a wave may vary about the pool it runs against.
#[derive(Clone, Debug)]
struct PoolOptions {
    pool_size: u32,
    recycle_turns: u32,
    warm: Vec<String>,
}

impl PoolOptions {
    fn sized(pool_size: u32) -> Self {
        Self {
            pool_size,
            recycle_turns: 50,
            warm: Vec::new(),
        }
    }

    fn warming(mut self, class: ClassSpec, count: u32) -> Self {
        self.warm.push(class.warm(count));
        self
    }

    fn recycling_every(mut self, turns: u32) -> Self {
        self.recycle_turns = turns;
        self
    }
}

/// Everything that differs between the deterministic lane and the real one,
/// and nothing else.
///
/// The waves themselves never branch on the lane: they take a socket and a set
/// of callers. What a lane decides is which executable the pool launches, which
/// compatibility cell admits it, how long a turn may take, and what environment
/// the daemon runs under -- and the last one is the whole reason the double can
/// be a pool instance at all, since its attestation contract arrives through
/// `PMUX_*` names the allowlist inherits verbatim.
struct Lane {
    claude: PathBuf,
    claude_version: String,
    transcript_drain_ms: u64,
    turn_timeout_ms: u64,
    environment: BTreeMap<String, String>,
    /// Whether the daemon is started with `--tested-claude-profile`.
    ///
    /// `false` is the deployment every operator who is not us has: nothing on
    /// argv admits a compatibility cell, so the ONLY thing that can admit one
    /// is pmux's own promoted set. A lane that runs with it off is testing
    /// promotion; a lane that runs with it on is testing everything else.
    operator_profile: bool,
}

impl Lane {
    fn double(binaries: &Binaries, sandbox: &Sandbox) -> Self {
        Self {
            claude: binaries.double.clone(),
            claude_version: DOUBLE_VERSION.to_owned(),
            transcript_drain_ms: 50,
            turn_timeout_ms: TURN_TIMEOUT_MS,
            environment: sandbox.daemon_environment(),
            // The double reports 9.9.9, which pmux does not and must never
            // promote, so this lane can only run on an operator profile.
            operator_profile: true,
        }
    }

    /// The operator's own installed Claude, measured rather than assumed.
    ///
    /// The version is read from the binary because an unmatched compatibility
    /// cell is refused before a child exists, so a guessed version would
    /// produce a refusal rather than a false pass. The environment is the
    /// operator's real one: a pool instance authenticates from it, and the
    /// live probe that established this found `needs_login` under an empty
    /// snapshot.
    ///
    /// `None`, loudly, when [`REAL_CLAUDE_VARIABLE`] is unset, exactly as
    /// `cross_cell_contamination.rs:2258` does for its own real lane. This used
    /// to `.expect(..)` -- while the doc comment on [`real_wave`] said the lane
    /// was "`#[ignore]`d AND gated on `PMUX_POOL_REAL_CLAUDE`", which is a
    /// promise of a gate where the code had an assertion. `--include-ignored`
    /// without `PMUX_POOL_REAL_CLAUDE` used to panic all five real waves
    /// (MEASURED as five identical panics). `tools/dev/check.sh --push` unsets
    /// that variable so the waves skip.
    ///
    /// Nothing below this point changed: with the variable set, every one of
    /// the five runs exactly as it did before.
    fn real() -> Option<Self> {
        let Some(named) = std::env::var_os(REAL_CLAUDE_VARIABLE) else {
            println!(
                "SKIPPED: set {REAL_CLAUDE_VARIABLE} to the Claude executable to run the pool's \
                 real lane. The double cannot reproduce a real Ink frame under concurrency, so \
                 the deterministic waves establish nothing about it."
            );
            return None;
        };
        let claude = PathBuf::from(named)
            .canonicalize()
            .expect("the real Claude executable must resolve");
        let output = Command::new(&claude)
            .arg("--version")
            .stdin(Stdio::null())
            .output()
            .expect("`claude --version` must run");
        assert!(output.status.success(), "`claude --version` failed");
        let reported = String::from_utf8(output.stdout).expect("`claude --version` emits UTF-8");
        let claude_version = reported
            .split_whitespace()
            .next()
            .expect("`claude --version` reports a version")
            .to_owned();
        Some(Self {
            claude,
            claude_version,
            // Only read when `operator_profile` is on. The promotion lane below
            // turns it off, and then the drain is whatever pmux promoted for
            // this identity -- which is the number under test.
            transcript_drain_ms: 2_000,
            turn_timeout_ms: 300_000,
            environment: std::env::vars().collect(),
            operator_profile: true,
        })
    }

    /// The real lane as an unprivileged operator gets it: no compatibility
    /// flag on argv at all.
    fn promoted() -> Option<Self> {
        Some(Self {
            operator_profile: false,
            ..Self::real()?
        })
    }
}

struct PoolDaemon {
    child: Child,
    socket: PathBuf,
    stderr_path: PathBuf,
    stopped: bool,
}

impl PoolDaemon {
    async fn start(binaries: &Binaries, sandbox: &Sandbox, options: &PoolOptions) -> Self {
        Self::start_on(binaries, sandbox, options, &Lane::double(binaries, sandbox)).await
    }

    async fn start_on(
        binaries: &Binaries,
        sandbox: &Sandbox,
        options: &PoolOptions,
        lane: &Lane,
    ) -> Self {
        let mut daemon = Self::spawn(binaries, sandbox, options, lane);
        let client = PmuxClient::new(&daemon.socket).expect("a client binds the candidate socket");
        // Generous: a warm set of fifteen instances is fifteen real TUI
        // launches before the socket answers.
        for _ in 0..2_400 {
            if client.ping().await.is_ok() {
                return daemon;
            }
            if let Some(status) = daemon.child.try_wait().unwrap() {
                panic!(
                    "pmuxd exited during startup with {status}: {}",
                    daemon.diagnostics()
                );
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("pmuxd did not bind its public socket");
    }

    /// Start the daemon and return the instant the child exists, WITHOUT
    /// waiting for it to serve.
    ///
    /// Separate from [`Self::start_on`] because the readiness wait is exactly
    /// what a test of the startup window cannot do: the window closes when the
    /// socket begins answering.
    fn spawn(binaries: &Binaries, sandbox: &Sandbox, options: &PoolOptions, lane: &Lane) -> Self {
        let stderr_path = sandbox.root.join("pmuxd.stderr");
        let stderr = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&stderr_path)
            .expect("the daemon stderr file opens");
        let profile = serde_json::json!({
            "claude_version": lane.claude_version,
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "terminal_profile": "transparent",
            "input_transport": "sdk",
            "transcript_drain_ms": lane.transcript_drain_ms,
        })
        .to_string();

        let mut command = Command::new(&binaries.pmuxd);
        command
            .arg("serve")
            .arg("--socket")
            .arg(&sandbox.socket)
            .arg("--rmuxd")
            .arg(&binaries.rmuxd)
            .arg("--launcher")
            .arg(&binaries.launcher)
            .arg("--runtime-parent")
            .arg(&sandbox.runtime_parent);
        if lane.operator_profile {
            command.arg("--tested-claude-profile").arg(profile);
        }
        command
            .arg("--pool-parent")
            .arg(&sandbox.pool_parent)
            .arg("--pool-claude")
            .arg(&lane.claude)
            .arg("--pool-size")
            .arg(options.pool_size.to_string())
            .arg("--pool-recycle-turns")
            .arg(options.recycle_turns.to_string())
            .arg("--pool-idle-ttl-ms")
            .arg(IDLE_TTL_MS.to_string())
            .arg("--pool-turn-timeout-ms")
            .arg(lane.turn_timeout_ms.to_string());
        for warm in &options.warm {
            command.arg("--pool-warm").arg(warm);
        }
        command.env_clear();
        for (key, value) in &lane.environment {
            command.env(key, value);
        }
        let child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(stderr))
            .spawn()
            .expect("the candidate daemon starts");

        Self {
            child,
            socket: sandbox.socket.clone(),
            stderr_path,
            stopped: false,
        }
    }

    /// Wait for the child to exit and report exactly how it did.
    ///
    /// The whole status, not a boolean: `143` and `0` are the two answers this
    /// file's startup-window test exists to tell apart, and on Unix they differ
    /// in `code()` being `None` versus `Some(0)`.
    async fn wait_for_exit(&mut self) -> std::process::ExitStatus {
        for _ in 0..1_200 {
            if let Some(status) = self.child.try_wait().expect("the child is waitable") {
                self.stopped = true;
                return status;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("the candidate daemon did not exit: {}", self.diagnostics());
    }

    fn client(&self) -> PmuxClient {
        PmuxClient::new(&self.socket).expect("a client binds the candidate socket")
    }

    async fn stop(&mut self) {
        if self.stopped {
            return;
        }
        self.signal_terminate();
        for _ in 0..1_200 {
            if self.child.try_wait().unwrap().is_some() {
                self.stopped = true;
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("the candidate daemon did not stop: {}", self.diagnostics());
    }

    fn signal_terminate(&self) {
        let _ = Command::new("/bin/kill")
            .arg("-TERM")
            .arg(self.child.id().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    fn diagnostics(&self) -> String {
        std::fs::read_to_string(&self.stderr_path).unwrap_or_default()
    }
}

impl Drop for PoolDaemon {
    fn drop(&mut self) {
        if self.stopped {
            return;
        }
        self.signal_terminate();
        for _ in 0..400 {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---------------------------------------------------------------------------
// One caller
// ---------------------------------------------------------------------------

/// A secret no other process can guess and no corpus can contain, in a shape
/// that survives being typed into a TUI and stored in JSON.
fn fresh_secret(index: usize) -> String {
    format!(
        "PMUXWAVE{index}Z{}",
        Uuid::new_v4().simple().to_string().to_uppercase()
    )
}

#[derive(Clone, Debug)]
struct Caller {
    index: usize,
    class: ClassSpec,
    secret: String,
}

impl Caller {
    fn prompt(&self) -> String {
        format!("PMUX_TEST_ECHO:{}", self.secret)
    }

    fn expected_answer(&self) -> String {
        format!("pmux-test-echo:{}", self.secret)
    }
}

#[derive(Debug)]
enum Answer {
    Served(Box<StatelessResult>),
    Refused(Box<ErrorBody>),
    Transport(String),
}

#[derive(Debug)]
struct Outcome {
    caller: Caller,
    answer: Answer,
    elapsed: Duration,
}

/// Fire every caller at once and collect what each one got.
///
/// A `Barrier` rather than "spawn and hope": the point of a wave is that the
/// requests are simultaneous, and tasks spawned in a loop are not. Every task
/// builds its own client, because one connection per request is what the
/// shipped client does.
async fn wave(socket: &Path, callers: Vec<Caller>) -> Vec<Outcome> {
    wave_with(socket, callers, &[]).await
}

/// The same wave, with the prompt overridden per caller.
///
/// The real lane needs prompts a model can act on rather than the double's
/// `PMUX_TEST_ECHO:` protocol, and it needs them fired through the SAME barrier
/// and the same client: a second copy of this function is a second definition
/// of "concurrent".
async fn wave_with(
    socket: &Path,
    callers: Vec<Caller>,
    overrides: &[(usize, String)],
) -> Vec<Outcome> {
    let overrides: BTreeMap<usize, String> = overrides.iter().cloned().collect();
    let barrier = Arc::new(Barrier::new(callers.len()));
    let mut tasks = JoinSet::new();
    for caller in callers {
        let socket = socket.to_path_buf();
        let barrier = Arc::clone(&barrier);
        let prompt = overrides
            .get(&caller.index)
            .cloned()
            .unwrap_or_else(|| caller.prompt());
        tasks.spawn(async move {
            let client = PmuxClient::new(&socket).expect("a client binds the candidate socket");
            let request = RunStatelessRequest {
                model: caller.class.spelling.to_owned(),
                effort: caller.class.effort,
                prompt,
                deadline_unix_ms: None,
            };
            barrier.wait().await;
            let started = Instant::now();
            let answer = match client.run_stateless(request).await {
                Ok(result) => Answer::Served(Box::new(result)),
                Err(ClientError::Server(body)) => Answer::Refused(Box::new(body)),
                Err(other) => Answer::Transport(other.to_string()),
            };
            Outcome {
                caller,
                answer,
                elapsed: started.elapsed(),
            }
        });
    }
    let mut outcomes = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        outcomes.push(joined.expect("a wave task must not panic"));
    }
    outcomes.sort_by_key(|outcome| outcome.caller.index);
    outcomes
}

// ---------------------------------------------------------------------------
// The child-side evidence
// ---------------------------------------------------------------------------

/// What the children wrote, joined into one thing a claim can be read off.
///
/// `prompts.jsonl` says which process received which prompt; `launches.jsonl`
/// says which argv that process was launched with. The join key is `cwd`,
/// which is `<parent>/<slot>/<epoch>/cwd` and belongs to exactly one process.
struct ChildEvidence {
    /// `cwd` -> the whole argv, verbatim.
    argv_by_cwd: BTreeMap<String, Vec<String>>,
    /// prompt text -> every `cwd` that received it.
    receivers_by_prompt: BTreeMap<String, Vec<String>>,
    /// `cwd` -> every prompt that process received, in arrival order.
    prompts_by_cwd: BTreeMap<String, Vec<String>>,
    launch_count: usize,
}

impl ChildEvidence {
    fn read(state_root: &Path) -> Self {
        let mut argv_by_cwd = BTreeMap::new();
        let mut launch_count = 0;
        for row in read_jsonl(&state_root.join("launches.jsonl")) {
            let cwd = row["cwd"].as_str().expect("a launch row names its cwd");
            let argv = row["argv"]
                .as_array()
                .expect("a launch row carries its argv")
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .expect("an argv entry is a string")
                        .to_owned()
                })
                .collect::<Vec<_>>();
            launch_count += 1;
            assert!(
                argv_by_cwd.insert(cwd.to_owned(), argv).is_none(),
                "two processes claimed the same instance cwd {cwd}, so the join key is not a key"
            );
        }
        let mut receivers_by_prompt: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut prompts_by_cwd: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for row in read_jsonl(&state_root.join("prompts.jsonl")) {
            let cwd = row["cwd"]
                .as_str()
                .expect("a prompt row names its cwd")
                .to_owned();
            let prompt = row["prompt"]
                .as_str()
                .expect("a prompt row carries its prompt")
                .to_owned();
            receivers_by_prompt
                .entry(prompt.clone())
                .or_default()
                .push(cwd.clone());
            prompts_by_cwd.entry(cwd).or_default().push(prompt);
        }
        Self {
            argv_by_cwd,
            receivers_by_prompt,
            prompts_by_cwd,
            launch_count,
        }
    }

    /// The cwd of the one process that received `prompt`, or a description of
    /// why there is no such process.
    fn sole_receiver(&self, prompt: &str) -> Result<&String, String> {
        let receivers = self
            .receivers_by_prompt
            .get(prompt)
            .ok_or_else(|| "no child process recorded receiving it".to_owned())?;
        if receivers.len() != 1 {
            return Err(format!(
                "{} different instances received it: {receivers:?}",
                receivers.len()
            ));
        }
        Ok(&receivers[0])
    }

    /// The argv of the one process that received `prompt`.
    fn sole_receiver_argv(&self, prompt: &str) -> Result<&[String], String> {
        let cwd = self.sole_receiver(prompt)?;
        self.argv_by_cwd
            .get(cwd)
            .map(Vec::as_slice)
            .ok_or_else(|| format!("no launch row exists for {cwd}"))
    }
}

fn read_jsonl(path: &Path) -> Vec<Value> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("the double writes canonical JSON lines"))
        .collect()
}

/// The value of `flag` in an argv, or `None` when the flag is absent.
///
/// Reads the argv the child was actually launched with. There is no second
/// table here mapping a class to a token: the expectation lives on
/// [`ClassSpec`] and the observation lives in the process's own argv.
fn argv_value<'a>(argv: &'a [String], flag: &str) -> Option<&'a str> {
    argv.iter()
        .position(|argument| argument == flag)
        .and_then(|index| argv.get(index + 1))
        .map(String::as_str)
}

// ---------------------------------------------------------------------------
// The report
// ---------------------------------------------------------------------------

/// Every violation a wave found, gathered rather than asserted one at a time.
///
/// A wave that fails should say everything that was wrong with it, not the
/// first thing: at fifteen concurrent callers the first failure is rarely the
/// informative one.
#[derive(Default)]
struct Report {
    title: String,
    violations: Vec<String>,
    notes: Vec<String>,
}

impl Report {
    fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            ..Self::default()
        }
    }

    fn check(&mut self, held: bool, violation: impl FnOnce() -> String) {
        if !held {
            self.violations.push(violation());
        }
    }

    fn note(&mut self, note: impl Into<String>) {
        self.notes.push(note.into());
    }

    fn assert_clean(self) {
        if self.violations.is_empty() {
            println!("[{}]", self.title);
            for note in &self.notes {
                println!("  {note}");
            }
            return;
        }
        let mut rendered = format!("[{}] {} violation(s):\n", self.title, self.violations.len());
        for violation in &self.violations {
            rendered.push_str("  VIOLATION ");
            rendered.push_str(violation);
            rendered.push('\n');
        }
        for note in &self.notes {
            rendered.push_str("  ");
            rendered.push_str(note);
            rendered.push('\n');
        }
        panic!("{rendered}");
    }
}

// ---------------------------------------------------------------------------
// The claims, each checked over a whole wave
// ---------------------------------------------------------------------------

/// Every call returned its OWN answer or a refusal, and never a wrong answer.
fn claim_no_wrong_answer(report: &mut Report, outcomes: &[Outcome], tolerate_transport: bool) {
    let foreign: BTreeSet<&str> = outcomes
        .iter()
        .map(|outcome| outcome.caller.secret.as_str())
        .collect();
    for outcome in outcomes {
        match &outcome.answer {
            Answer::Served(result) => {
                report.check(result.text == outcome.caller.expected_answer(), || {
                    format!(
                        "caller {} asked for {} and got {:?}, not its own answer {:?}",
                        outcome.caller.index,
                        outcome.caller.class.label(),
                        result.text,
                        outcome.caller.expected_answer()
                    )
                });
                for other in &foreign {
                    if *other == outcome.caller.secret {
                        continue;
                    }
                    report.check(!result.text.contains(other), || {
                        format!(
                            "caller {}'s answer carried another caller's secret {other}",
                            outcome.caller.index
                        )
                    });
                }
                // The wire's own account of the class, which is necessary and
                // NOT sufficient: it is copied from the request path, so it
                // cannot see a mis-routed instance. The child-side join is what
                // closes that, in `claim_fungibility`.
                report.check(result.model == outcome.caller.class.expected_model, || {
                    format!(
                        "caller {} asked for {} and the result named model {:?}",
                        outcome.caller.index,
                        outcome.caller.class.label(),
                        result.model
                    )
                });
                let published = result.effort.map(|effort| {
                    serde_json::to_value(effort)
                        .expect("an effort level serializes")
                        .as_str()
                        .expect("an effort level is a string")
                        .to_owned()
                });
                report.check(
                    published.as_deref() == outcome.caller.class.expected_effort,
                    || {
                        format!(
                            "caller {} asked for {} and the result named effort {published:?}",
                            outcome.caller.index,
                            outcome.caller.class.label()
                        )
                    },
                );
            }
            Answer::Refused(_) => {}
            Answer::Transport(error) => {
                // Not a wrong answer, but not a refusal either: the caller was
                // left without a decision. Tolerated only where the test itself
                // destroyed the transport, and counted even there.
                let message = format!(
                    "caller {} got neither an answer nor a server refusal: {error}",
                    outcome.caller.index
                );
                if tolerate_transport {
                    report.note(format!("TOLERATED {message}"));
                } else {
                    report.violations.push(message);
                }
            }
        }
    }
}

/// A call for one class was never answered by an instance of another.
///
/// Read from the child side. The wire cannot answer this question.
fn claim_fungibility(report: &mut Report, outcomes: &[Outcome], evidence: &ChildEvidence) {
    for outcome in outcomes {
        if !matches!(outcome.answer, Answer::Served(_)) {
            continue;
        }
        let argv = match evidence.sole_receiver_argv(&outcome.caller.prompt()) {
            Ok(argv) => argv,
            Err(reason) => {
                report.violations.push(format!(
                    "caller {}'s prompt cannot be attributed to one instance: {reason}",
                    outcome.caller.index
                ));
                continue;
            }
        };
        let class = outcome.caller.class;
        report.check(
            argv_value(argv, "--model") == Some(class.expected_model),
            || {
                format!(
                    "caller {} asked for {} and was answered by a process launched {:?}",
                    outcome.caller.index,
                    class.label(),
                    argv_value(argv, "--model")
                )
            },
        );
        report.check(
            argv_value(argv, "--effort") == class.expected_effort,
            || {
                format!(
                "caller {} asked for {} and was answered by a process launched with effort {:?}",
                outcome.caller.index,
                class.label(),
                argv_value(argv, "--effort")
            )
            },
        );
    }
}

/// Cross-call statelessness, in exactly the three forms this can observe.
///
/// 1. Each caller's prompt was submitted to AT MOST ONE instance, and to at
///    least one when the caller was answered. A prompt delivered twice is a
///    second cell holding a caller's bytes.
/// 2. Every transcript **that survives to the end of the run** carries at most
///    one caller's secret. Not "every transcript ever written": the pool erases
///    an instance's whole tree when it recycles or destroys it, and a file that
///    is gone cannot be read. What that leaves uncovered is a transcript that
///    briefly held two callers' prompts and was then erased -- which claim 1
///    already excludes, since two prompts in one transcript means two
///    submissions to one bound transcript.
/// 3. No answer carries another caller's secret. That one lives in
///    [`claim_no_wrong_answer`], beside the answer it is about.
///
/// The residue the pool keeps BY DESIGN is not asserted against: `/clear`
/// abandons the previous transcript in place rather than truncating it
/// (path-b.md sec.10), so one instance's ROOT accumulates one file per caller
/// it has served until the instance is recycled. That is why claim 2 is
/// per-file and not per-root -- a per-root assertion would fail against
/// correct, documented behaviour.
fn claim_statelessness(
    report: &mut Report,
    outcomes: &[Outcome],
    evidence: &ChildEvidence,
    pool_parent: &Path,
) {
    let secrets: BTreeMap<&str, usize> = outcomes
        .iter()
        .map(|outcome| (outcome.caller.secret.as_str(), outcome.caller.index))
        .collect();

    // Each prompt reached at most one instance, exactly once.
    for outcome in outcomes {
        let prompt = outcome.caller.prompt();
        let receivers = evidence
            .receivers_by_prompt
            .get(&prompt)
            .map_or(0, Vec::len);
        let expected = usize::from(matches!(outcome.answer, Answer::Served(_)));
        report.check(receivers <= 1, || {
            format!(
                "caller {}'s prompt was submitted to {receivers} instances",
                outcome.caller.index
            )
        });
        report.check(receivers >= expected, || {
            format!(
                "caller {} was answered but no instance recorded receiving its prompt",
                outcome.caller.index
            )
        });
    }

    // Every transcript still on disk carries at most one caller's secret. The
    // BOUND transcript of a turn is the one the model can read; `/clear`
    // abandons the previous one in place, which is the documented residue
    // channel (path-b.md sec.10), so this is asserted per FILE and not per root.
    for file in walk_files(pool_parent) {
        if file
            .extension()
            .is_none_or(|extension| extension != "jsonl")
        {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        let carried: Vec<usize> = secrets
            .iter()
            .filter(|(secret, _)| text.contains(**secret))
            .map(|(_, index)| *index)
            .collect();
        report.check(carried.len() <= 1, || {
            format!(
                "{} carries the secrets of callers {carried:?}",
                file.display()
            )
        });
    }
}

/// Every refusal uses a code both shipped clients already know.
///
/// Both shipped clients hard-reject an unknown code and lose the WHOLE frame,
/// so this is checked over every refusal a wave produced and not only over the
/// capacity ones: a fault refusal -- a killed child, a lost sidecar, a turn
/// that ran out of time -- arrives through the same wire.
///
/// **The ten below are NOT the four `pool::refusal` commits to**, and the
/// sentence here used to say they were. Four of them are that module's, pinned
/// against its own public surface by
/// `pool::refusal::tests::every_pool_refusal_uses_a_code_both_shipped_clients_already_know`
/// over a census derived from the module source. The other six are the fault
/// codes a wave reaches through the session and driver layers, and they belong
/// to no single module, which is why they are enumerated here and cannot be
/// derived from one. Keep the two statements apart: narrowing this list to four
/// would make the check fail on a healthy wave, and pointing the module's
/// four-code claim at this list would let the pool grow a fifth unnoticed.
fn claim_every_refusal_is_a_known_code(report: &mut Report, outcomes: &[Outcome]) {
    const ADMITTED: &[ErrorCode] = &[
        // The four `pool::refusal` commits to.
        ErrorCode::SessionBusy,
        ErrorCode::UnsupportedFeature,
        ErrorCode::SchemaDrift,
        ErrorCode::DaemonLost,
        // Fault codes that reach a caller through the session and driver
        // layers rather than through the pool's refusal module.
        ErrorCode::ClaudeExited,
        ErrorCode::TurnTimeout,
        ErrorCode::PromptNotAcknowledged,
        ErrorCode::TranscriptUnavailable,
        ErrorCode::RecoveryFailed,
        ErrorCode::Internal,
    ];
    for outcome in outcomes {
        let Answer::Refused(body) = &outcome.answer else {
            continue;
        };
        report.check(ADMITTED.contains(&body.code), || {
            format!(
                "caller {} was refused with {:?}, which is not a code this pool committed to: {}",
                outcome.caller.index, body.code, body.message
            )
        });
    }
}

/// A refusal at the cap names the budget, and every number it names is true.
///
/// Applies to CAPACITY refusals only, identified by the violation they publish
/// rather than by their code: a killed child also refuses with a code, and it
/// carries no census to check.
fn claim_refusal_names_a_true_budget(
    report: &mut Report,
    outcomes: &[Outcome],
    pool_size: u32,
) -> usize {
    let mut refusals = 0;
    for outcome in outcomes {
        let Answer::Refused(body) = &outcome.answer else {
            continue;
        };
        let violation = body.details.get("violation").and_then(Value::as_str);
        if !matches!(violation, Some("pool_exhausted" | "reclaimed_slot_leaked")) {
            continue;
        }
        refusals += 1;
        report.check(body.code == ErrorCode::SessionBusy, || {
            format!(
                "caller {} was refused with {:?}, not the capacity refusal",
                outcome.caller.index, body.code
            )
        });
        report.check(body.retryable, || {
            format!(
                "caller {}'s capacity refusal is not marked retryable",
                outcome.caller.index
            )
        });
        let number = |name: &str| body.details.get(name).and_then(Value::as_u64);
        let configured = number("configured_instances");
        let budget = number("budget_instances");
        let leaked = number("leaked");
        let live = number("live");
        let in_flight = number("in_flight");
        let idle = number("idle");
        let reserved = number("reserved");
        let tearing_down = number("tearing_down");
        report.check(configured == Some(u64::from(pool_size)), || {
            format!("the refusal named {configured:?} configured instances, not {pool_size}")
        });
        report.check(
            budget == configured.zip(leaked).map(|(size, lost)| size - lost),
            || format!("budget {budget:?} != configured {configured:?} - leaked {leaked:?}"),
        );
        report.check(live <= budget, || {
            format!("the refusal named {live:?} live against a budget of {budget:?}")
        });
        // The census clause is the operator-facing half, and its parts must add
        // up to the whole it claims: every slot the pool is holding is in
        // exactly one of the six counted states.
        let clearing = number("clearing");
        let leased = number("leased");
        let parts = [in_flight, clearing, idle, leased, reserved, tearing_down]
            .into_iter()
            .sum::<Option<u64>>();
        report.check(parts == live, || {
            format!(
                "the refusal's states sum to {parts:?} but it claims {live:?} live: {}",
                body.message
            )
        });
        // Every clause the message prints is one of the numbers the details
        // blob publishes, and vice versa: an operator reading the sentence and
        // a client branching on the JSON must not be told different things.
        for (count, phrase) in [
            (in_flight, "serving a turn"),
            (clearing, "clearing between turns, with no caller waiting"),
            (idle, "idle"),
            (leased, "holding a conversation lease"),
            (reserved, "reserved or warming"),
            (tearing_down, "in teardown"),
        ] {
            let clause = format!("{} {phrase}", count.unwrap_or_default());
            report.check(body.message.contains(&clause), || {
                format!(
                    "the refusal publishes {clause:?} in its details and not in its message: {}",
                    body.message
                )
            });
        }
        report.check(
            body.details
                .get("requested_class")
                .and_then(|class| class.get("model"))
                .and_then(Value::as_str)
                == Some(outcome.caller.class.expected_model),
            || {
                format!(
                    "caller {} asked for {} and the refusal named class {:?}",
                    outcome.caller.index,
                    outcome.caller.class.label(),
                    body.details.get("requested_class")
                )
            },
        );
        let census = format!(
            "{} of {} usable instance(s) are live",
            live.unwrap_or_default(),
            budget.unwrap_or_default()
        );
        report.check(body.message.contains(&census), || {
            format!(
                "the refusal message does not name its budget: {}",
                body.message
            )
        });
        report.check(body.message.contains("nothing is queued"), || {
            format!(
                "the refusal must say there is no queue, or a caller will wait: {}",
                body.message
            )
        });
    }
    refusals
}

/// The pool's own census, over the wire, after the wave has quiesced.
async fn census(client: &PmuxClient) -> Value {
    let diagnosis = client.diagnose().await.expect("the daemon answers a probe");
    diagnosis
        .layer(HealthLayerName::Pool)
        .expect("a Path B daemon reports its pool layer")
        .evidence
        .clone()
}

/// The pool layer's own verdict on itself: the finding and the outcome it rolls
/// up to.
async fn pool_verdict(client: &PmuxClient) -> (LayerFinding, ProbeOutcome) {
    let layer = client
        .diagnose()
        .await
        .expect("the daemon answers a probe")
        .layer(HealthLayerName::Pool)
        .expect("a Path B daemon reports its pool layer")
        .clone();
    (layer.finding, layer.outcome)
}

/// Every slot taken was returned or is accounted for as leaked.
///
/// `disrupted` separates two very different claims. The pool's own bookkeeping
/// must close in EVERY run. Whether the sidecar still holds a terminal for
/// every registered instance is a different question, and killing a child is
/// exactly how it comes apart: the pool has no liveness sampler over idle
/// instances, so a killed child leaves an instance in the idle set whose pane
/// is gone. That costs the next caller of that class a refused turn -- never a
/// wrong answer -- and the daemon is required to SAY so rather than roll up
/// healthy, which is the part asserted here.
async fn claim_slot_accounting(
    report: &mut Report,
    client: &PmuxClient,
    pool_size: u32,
    disrupted: bool,
) -> Value {
    let evidence = quiesced_census(client).await;
    let number = |name: &str| evidence.get(name).and_then(Value::as_u64);
    report.check(number("pool_size") == Some(u64::from(pool_size)), || {
        format!("the pool layer reports pool_size {:?}", number("pool_size"))
    });
    report.check(
        number("capacity")
            == number("pool_size")
                .zip(number("leaked"))
                .map(|(size, lost)| size - lost),
        || format!("capacity != pool_size - leaked: {evidence}"),
    );
    report.check(number("live") <= number("capacity"), || {
        format!("more live instances than capacity: {evidence}")
    });
    report.check(number("in_flight") == Some(0), || {
        format!("a quiesced pool still reports work in flight: {evidence}")
    });
    report.check(number("clearing") == Some(0), || {
        format!("a quiesced pool is still clearing: {evidence}")
    });
    // The whole of the census closes: every slot the pool says it holds is in
    // exactly one of the six counted states. Summed from the layer's own
    // numbers rather than from a subset, because a subset that adds up is the
    // check that misses the state nobody thought to include.
    let counted = [
        "in_flight",
        "clearing",
        "idle",
        "leased",
        "reserved",
        "tearing_down",
    ]
    .into_iter()
    .map(number)
    .sum::<Option<u64>>();
    report.check(counted == number("live"), || {
        format!("the pool layer's states sum to {counted:?}, not its live count: {evidence}")
    });
    report.check(number("halted").is_none(), || {
        format!("the pool halted during the wave: {evidence}")
    });

    // The sidecar's own count, which is the only statement here that is not the
    // pool's bookkeeping agreeing with itself.
    let present = number("instance_terminals_present");
    let registered = number("registered_instances");
    report.check(present <= registered, || {
        format!("the sidecar reports more instance terminals than the pool registered: {evidence}")
    });
    if present == registered {
        return evidence;
    }
    if !disrupted {
        report.violations.push(format!(
            "the sidecar does not hold a terminal for every registered instance, and nothing \
             in this run killed one: {evidence}"
        ));
        return evidence;
    }
    // A pool that ended the run holding NOTHING has no instance whose process
    // could be gone, and that -- not `present != registered` -- is what the
    // claim below is about. This predicate was wider than its own message: at
    // `registered_instances: 0` beside `instance_terminals_present: null`,
    // `None != Some(0)` walked a pool holding no instances into an assertion
    // about a deficit with no subject. The pool layer answers
    // `nothing_to_exercise` there and is right to -- its one non-self-referential
    // question is whether the sidecar still holds a terminal for the instances
    // the pool believes in, and it believes in none -- and its detail claims
    // nothing about the sidecar.
    //
    // MEASURED, and the reason this state is new: once a refused caller waits
    // for a clearing slot instead of being turned away, the disrupted round
    // uses every slot, so every instance meets the dead sidecar on its next
    // operation and every one is destroyed. Before that, callers were refused
    // at the cap (5 to 15 of them per run) and the pool ended holding idle
    // instances whose panes were gone -- which is exactly the state the sentence
    // above describes, and it is still asserted whenever it occurs.
    //
    // The claim is not dropped here, it MOVES to the layer that owns it. The
    // sidecar really is dead, so the DAEMON must not roll up green; the layers
    // that ask about the sidecar itself are `control_plane` and
    // `private_runtime`, and `DaemonDiagnosis::outcome` folds every one of them
    // plus every layer that is missing.
    if registered == Some(0) {
        let diagnosis = client.diagnose().await.expect("the daemon answers a probe");
        let rolled = diagnosis.outcome();
        report.check(rolled != ProbeOutcome::Pass, || {
            format!(
                "every pool instance was destroyed and the control-plane probe did not answer, \
                 and the daemon still reports itself healthy: {:?}",
                diagnosis
                    .layers
                    .iter()
                    .map(|layer| (layer.layer, layer.finding, layer.outcome))
                    .collect::<Vec<_>>()
            )
        });
        report.note(format!(
            "after the kill the pool held no instance at all, so the terminal deficit has no \
             subject; the daemon rolled up as {rolled:?}"
        ));
        return evidence;
    }
    // A deficit after a kill is expected. Reporting it as healthy is not: this
    // is the whole value of the layer, and a `pass` here would mean the daemon
    // rolls up green while holding an idle instance whose process is gone.
    //
    // The assertion is on the OUTCOME rather than on a list of admitted
    // findings: a counted deficit is `faulted` and a probe that never answered
    // is `not_established`, and the property that matters -- and the only one
    // that cannot be broken by adding a finding -- is that neither is `pass`.
    let (finding, outcome) = pool_verdict(client).await;
    report.check(outcome != ProbeOutcome::Pass, || {
        format!(
            "the sidecar holds {present:?} terminal(s) for {registered:?} registered \
             instance(s) and the pool layer rolls up as {outcome:?} ({finding:?}): {evidence}"
        )
    });
    report.note(format!(
        "after the kill the pool held {registered:?} registered instance(s) against \
         {present:?} sidecar terminal(s), and the daemon reported {finding:?}/{outcome:?}"
    ));
    evidence
}

/// Poll the census until every live instance is idle.
///
/// `live == idle`, DERIVED, rather than a list of the counters that must be
/// zero. `PoolCensus::live` is the number of live instances and `idle` is the
/// size of the published idle sets, so their equality says "no instance is
/// anywhere else" without naming where else is -- and `InstanceState` has
/// seven states across six buckets, of which this used to name four by hand.
///
/// The hand-written form was `["in_flight", "clearing", "reserved",
/// "tearing_down"]` summed to zero. It is the bug class in a test: the doc
/// promised a quiesced pool and the predicate tested four counters somebody
/// remembered. A bucket added to `CensusBucket` -- the enum that exists
/// precisely because a state was once counted by no clause -- would be absent
/// from that list, and every wave that waits for quiescence would have gone on
/// reading a census that was still moving.
///
/// Waiting on `in_flight` alone was the original defect and is worth keeping on
/// the record: the pool answers its caller BEFORE it types `/clear`, so a
/// wave's last answer arrives with every instance still clearing.
async fn quiesced_census(client: &PmuxClient) -> Value {
    let mut evidence = census(client).await;
    for _ in 0..600 {
        let live = evidence.get("live").and_then(Value::as_u64);
        let idle = evidence.get("idle").and_then(Value::as_u64);
        // `Some(_) == Some(_)`: an absent counter is not quiescence. Two
        // `unwrap_or_default()`s here would read a census that stopped
        // publishing either field as a perfectly quiet pool.
        if live.is_some() && live == idle {
            return evidence;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        evidence = census(client).await;
    }
    evidence
}

/// Every `<pool parent>/<slot>/<epoch>` directory currently on disk, sorted.
///
/// One entry per instance a mint has REACHED, not per instance it finished:
/// `mint_roots` creates the epoch tree before the child is launched, which is
/// what makes it the cheapest observable proof that a warm mint has begun.
fn epoch_trees(pool_parent: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(slots) = std::fs::read_dir(pool_parent) else {
        return found;
    };
    for slot in slots.flatten().filter(|slot| slot.path().is_dir()) {
        let Ok(epochs) = std::fs::read_dir(slot.path()) else {
            continue;
        };
        found.extend(
            epochs
                .flatten()
                .map(|epoch| epoch.path())
                .filter(|path| path.is_dir()),
        );
    }
    found.sort();
    found
}

fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => stack.push(path),
                Ok(kind) if kind.is_file() => found.push(path),
                _ => {}
            }
        }
    }
    found
}

// ---------------------------------------------------------------------------
// The waves
// ---------------------------------------------------------------------------

/// One wave of `concurrency` callers spread round-robin across every class.
///
/// `first_index` keeps caller indices unique across the rounds of one run, so
/// a violation names one call and not one position in a round.
fn callers(first_index: usize, concurrency: usize) -> Vec<Caller> {
    (0..concurrency)
        .map(|offset| {
            let index = first_index + offset;
            Caller {
                index,
                class: CLASSES[index % CLASSES.len()],
                secret: fresh_secret(index),
            }
        })
        .collect()
}

/// One run of the battery: `rounds` back-to-back waves of `concurrency`
/// callers against one daemon.
#[derive(Clone, Debug)]
struct WavePlan {
    concurrency: usize,
    rounds: usize,
    pool: PoolOptions,
    /// Whether this plan is expected to make instances serve more than one
    /// caller. See [`claim_reuse_was_exercised`] for why it is not optional.
    expect_reuse: bool,
    /// Whether this plan is expected to refuse at least one call.
    expect_refusal: bool,
    /// Whether this plan is expected to destroy and re-mint at least one slot.
    expect_recycle: bool,
    /// What is done to the last round while it is running.
    disruption: Option<Disruption>,
    /// Whether a call may end without a decision. Only true where this test is
    /// itself what destroyed the transport.
    expect_disruption: bool,
}

impl WavePlan {
    fn new(concurrency: usize, rounds: usize, pool: PoolOptions) -> Self {
        let recycle_turns = pool.recycle_turns;
        Self {
            concurrency,
            rounds,
            pool,
            // DERIVED, not declared. Reuse needs a second round to reuse in
            // AND a recycle cap above one: at `recycle_turns == 1` every
            // instance is destroyed after the turn it served, so no instance
            // can ever serve a second caller and demanding it would fail a
            // correct pool. Deriving it means a plan cannot quietly stop
            // exercising routing by having its cap lowered.
            expect_reuse: rounds > 1 && recycle_turns > 1,
            expect_refusal: false,
            expect_recycle: false,
            disruption: None,
            expect_disruption: false,
        }
    }

    fn expecting_refusals(mut self) -> Self {
        self.expect_refusal = true;
        self
    }

    /// A plan whose capacity is too small for any instance to be reused.
    ///
    /// An explicit waiver rather than a silently weakened default, and it is
    /// used exactly once: fifteen simultaneous callers against two slots serve
    /// two and refuse thirteen, and the next round arrives while both of those
    /// two are still clearing, so it refuses fifteen more. That IS the
    /// behaviour under test. Reuse and routing are proven by the plans that
    /// have the capacity for them.
    fn where_reuse_is_impossible(mut self) -> Self {
        self.expect_reuse = false;
        self
    }

    fn expecting_recycle(mut self) -> Self {
        self.expect_recycle = true;
        self
    }

    fn disrupted_by(mut self, disruption: Disruption) -> Self {
        self.disruption = Some(disruption);
        self.expect_disruption = true;
        // The disrupted round's callers may be answered by a process that is
        // about to die, so reuse is proven by the rounds before it.
        self.expect_reuse = self.rounds > 2;
        self
    }

    fn title(&self) -> String {
        format!(
            "{} concurrent x {} rounds, {} classes, pool {}, recycle {}",
            self.concurrency,
            self.rounds,
            CLASSES.len(),
            self.pool.pool_size,
            self.pool.recycle_turns
        )
    }
}

/// What the whole run produced, so a claim can be read over all of it at once.
struct RunResult {
    outcomes: Vec<Outcome>,
    evidence: ChildEvidence,
    round_walls: Vec<Duration>,
    /// Processes this run actually signalled. Zero means the disruption never
    /// happened, which is a failed test and not a passed one.
    killed: usize,
    /// Instances the daemon itself reported as `Clearing` at the moment a
    /// mid-clear kill was fired. Zero means the window was missed.
    clearing_at_kill: u64,
}

impl RunResult {
    fn served(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|outcome| matches!(outcome.answer, Answer::Served(_)))
            .count()
    }

    fn refused(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|outcome| matches!(outcome.answer, Answer::Refused(_)))
            .count()
    }

    fn transport_failures(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|outcome| matches!(outcome.answer, Answer::Transport(_)))
            .count()
    }
}

/// **The coverage claim, and it is not optional.**
///
/// Every fungibility statement in this file is vacuous over a pool that mints
/// one fresh instance per caller: a pool that never reuses an instance cannot
/// mis-route one, so "no call for one class was answered by an instance of
/// another" is true of it for a reason that has nothing to do with routing.
/// The first four waves in this file did exactly that -- 15 callers, 15
/// launches -- and reported a fungibility pass that tested nothing.
///
/// So the exercise is asserted, not hoped for: the run must have made at least
/// one instance serve two different callers, and where the plan says a slot
/// should have been recycled or a call refused, that too is asserted.
fn claim_reuse_was_exercised(
    report: &mut Report,
    plan: &WavePlan,
    run: &RunResult,
    capacity_refusals: usize,
) {
    let shared = run
        .evidence
        .prompts_by_cwd
        .values()
        .filter(|prompts| {
            prompts
                .iter()
                .filter(|prompt| prompt.starts_with("PMUX_TEST_ECHO:"))
                .count()
                > 1
        })
        .count();
    if plan.expect_reuse {
        report.check(shared > 0, || {
            format!(
                "no instance served two callers ({} launches for {} served calls), so every \
                 fungibility check in this run passed vacuously",
                run.evidence.launch_count,
                run.served()
            )
        });
    }
    if plan.expect_refusal {
        report.check(capacity_refusals > 0, || {
            format!(
                "the plan expects the cap to be reached and {capacity_refusals} capacity \
                 refusal(s) were produced, so the refusal checks passed vacuously"
            )
        });
    }
    // What "the disruption was actually exercised" means differs by what was
    // killed, and asking the same question of all three would either demand a
    // refusal a mid-clear kill cannot produce or accept a mid-turn kill that
    // did nothing.
    if let Some(disruption) = plan.disruption {
        if disruption.affects_a_caller() {
            report.check(
                run.refused() > capacity_refusals || run.transport_failures() > 0,
                || {
                    "the kill was aimed at callers' turns and every call still came back \
                     clean, so nothing about the disrupted path was exercised"
                        .to_owned()
                },
            );
        }
        if matches!(disruption, Disruption::ChildrenMidClear(_)) {
            report.check(run.clearing_at_kill > 0, || {
                "the kill fired without the daemon ever reporting an instance as clearing, so \
                 this run killed idle instances and proved nothing about the clear window"
                    .to_owned()
            });
            report.note(format!(
                "the daemon reported {} instance(s) clearing at the moment of the kill",
                run.clearing_at_kill
            ));
        }
    }
    if plan.expect_recycle {
        let epochs: BTreeSet<u64> = run
            .evidence
            .argv_by_cwd
            .keys()
            .filter_map(|cwd| epoch_of(Path::new(cwd)))
            .collect();
        report.check(epochs.len() > 1, || {
            format!(
                "the plan expects a recycle and every launch was at epoch {epochs:?}, so the \
                 recycle checks passed vacuously"
            )
        });
    }
    report.note(format!(
        "{} instance(s) served more than one caller; {} launch(es) for {} served call(s)",
        shared,
        run.evidence.launch_count,
        run.served()
    ));
}

/// Every distinct refusal sentence the run produced, so the report shows what
/// an operator would actually read rather than only that a check passed.
fn distinct_refusals(outcomes: &[Outcome]) -> BTreeSet<String> {
    outcomes
        .iter()
        .filter_map(|outcome| match &outcome.answer {
            Answer::Refused(body) => Some(body.message.clone()),
            _ => None,
        })
        .collect()
}

/// The epoch component of `<parent>/<slot>/<epoch>/cwd`.
fn epoch_of(cwd: &Path) -> Option<u64> {
    cwd.parent()?.file_name()?.to_str()?.parse().ok()
}

// ---------------------------------------------------------------------------
// Disruption
// ---------------------------------------------------------------------------

/// Something a wave has done to it while it is running.
///
/// The pool's whole design is resolved against one asymmetry -- returning
/// before the work is done is unacceptable, refusing is merely bad -- and the
/// only way to test that resolution is to take the work away mid-flight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Disruption {
    /// SIGKILL live Claude children while their turns are in flight.
    ChildrenMidTurn(usize),
    /// SIGKILL live Claude children in the window after their callers have
    /// their answers and before `/clear` has finished -- the state
    /// `spawn_clear` creates, and the one a pool under load spends most of its
    /// slots in.
    ChildrenMidClear(usize),
    /// SIGKILL the private rmux sidecar. Every pane in the daemon dies at once.
    Sidecar,
}

impl Disruption {
    /// Whether the daemon is expected to be able to serve again afterwards.
    const fn recovers(self) -> bool {
        match self {
            Self::ChildrenMidTurn(_) | Self::ChildrenMidClear(_) => true,
            // A daemon that has lost its private rmux sidecar does not grow a
            // new one. Every call is refused with `DaemonLost`, which is the
            // fail-closed posture the whole product is built on, and a
            // supervisor replaces the daemon. Asserting recovery here would be
            // asserting a behaviour pmux deliberately does not have.
            Self::Sidecar => false,
        }
    }

    /// Whether at least one CALLER must have been left without an answer.
    const fn affects_a_caller(self) -> bool {
        match self {
            Self::ChildrenMidTurn(_) | Self::Sidecar => true,
            // Killing an instance that has already answered affects no caller
            // by construction, so demanding a refusal here would demand the
            // wrong thing. What is proven instead is that the kill really
            // landed while instances were clearing -- see `clearing_at_kill`.
            Self::ChildrenMidClear(_) => false,
        }
    }
}

/// Every live pid the double has ever reported, newest first.
fn live_child_pids(state_root: &Path) -> Vec<i32> {
    let mut pids: Vec<i32> = read_jsonl(&state_root.join("launches.jsonl"))
        .iter()
        .filter_map(|row| row["pid"].as_i64())
        .filter_map(|pid| i32::try_from(pid).ok())
        .collect();
    pids.reverse();
    pids.dedup();
    pids.retain(|pid| is_alive(*pid));
    pids
}

fn is_alive(pid: i32) -> bool {
    Command::new("/bin/kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn sigkill(pid: i32) -> bool {
    Command::new("/bin/kill")
        .arg("-KILL")
        .arg(pid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// The private rmux sidecar this sandbox's daemon started.
///
/// Matched on argv[0], not on "the line mentions pmux-rmuxd": the DAEMON's own
/// command line names both `--rmuxd .../pmux-rmuxd` and `--runtime-parent
/// <sandbox>`, so a substring filter selects the daemon as well and the
/// "sidecar" test becomes a SIGKILL of pmuxd. That is a different experiment
/// with a different expected outcome, and it is the one this filter was
/// silently running.
///
/// The runtime parent is still required, so a developer's unrelated pmux daemon
/// on the same host is never a candidate.
fn sidecar_pids(sandbox: &Sandbox) -> Vec<i32> {
    let needle = string(&sandbox.runtime_parent);
    let output = Command::new("/bin/ps")
        .args(["-ax", "-o", "pid=,command="])
        .stdin(Stdio::null())
        .output()
        .expect("ps runs");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.contains(&needle))
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?;
            let executable = fields.next()?;
            Path::new(executable)
                .file_name()
                .filter(|name| *name == "pmux-rmuxd")
                .and_then(|_| pid.parse().ok())
        })
        .collect()
}

/// Kill things while `wave` is in flight, and report what was actually killed.
///
/// Returns the number of processes signalled. It is checked by the caller: a
/// disruption test that killed nothing is a test that proved nothing, and at
/// these speeds "the window closed before I got there" is the likely outcome
/// rather than an impossible one.
async fn disrupt(disruption: Disruption, sandbox: &Sandbox) -> usize {
    match disruption {
        Disruption::ChildrenMidTurn(count) | Disruption::ChildrenMidClear(count) => {
            live_child_pids(&sandbox.state_root)
                .into_iter()
                .take(count)
                .filter(|pid| sigkill(*pid))
                .count()
        }
        Disruption::Sidecar => sidecar_pids(sandbox)
            .into_iter()
            .filter(|pid| sigkill(*pid))
            .count(),
    }
}

async fn execute(plan: &WavePlan, sandbox: &Sandbox) -> RunResult {
    let mut outcomes = Vec::new();
    let mut round_walls = Vec::new();
    let mut killed = 0;
    let mut clearing_at_kill = 0;
    for round in 0..plan.rounds {
        let started = Instant::now();
        let batch = callers(round * plan.concurrency, plan.concurrency);
        let socket = sandbox.socket.clone();
        let mut produced = match plan.disruption {
            // Nothing to interleave with.
            None => wave(&socket, batch).await,
            // The turn window. The wave is spawned, given long enough to have
            // submitted, and then has its children taken away underneath it.
            Some(Disruption::ChildrenMidTurn(count)) if round + 1 == plan.rounds => {
                let running = tokio::spawn(async move { wave(&socket, batch).await });
                tokio::time::sleep(Duration::from_millis(400)).await;
                killed += disrupt(Disruption::ChildrenMidTurn(count), sandbox).await;
                running.await.expect("the wave task must not panic")
            }
            // The sidecar, on the same schedule: it is the harder case, because
            // it takes every pane at once rather than one instance's child.
            Some(Disruption::Sidecar) if round + 1 == plan.rounds => {
                let running = tokio::spawn(async move { wave(&socket, batch).await });
                tokio::time::sleep(Duration::from_millis(400)).await;
                killed += disrupt(Disruption::Sidecar, sandbox).await;
                running.await.expect("the wave task must not panic")
            }
            // The clear window. It is entered the instant a caller is answered
            // and lasts milliseconds, so the kill is not fired on a timer: a
            // watcher polls the pool's own census and fires the moment the
            // daemon itself reports instances clearing. What that count was at
            // the moment of the kill is recorded and asserted, so a run whose
            // window closed first fails rather than passing as "mid-clear".
            Some(Disruption::ChildrenMidClear(count)) if round + 1 == plan.rounds => {
                let watcher_socket = socket.clone();
                let watcher = tokio::spawn(async move {
                    let client = PmuxClient::new(&watcher_socket).expect("a client binds");
                    for _ in 0..2_000 {
                        let clearing = census(&client)
                            .await
                            .get("clearing")
                            .and_then(Value::as_u64)
                            .unwrap_or(0);
                        if clearing > 0 {
                            return clearing;
                        }
                    }
                    0
                });
                let produced = wave(&socket, batch).await;
                clearing_at_kill = watcher.await.expect("the census watcher must not panic");
                killed += disrupt(Disruption::ChildrenMidClear(count), sandbox).await;
                produced
            }
            Some(_) => wave(&socket, batch).await,
        };
        round_walls.push(started.elapsed());
        outcomes.append(&mut produced);
    }
    RunResult {
        outcomes,
        evidence: ChildEvidence::read(&sandbox.state_root),
        round_walls,
        killed,
        clearing_at_kill,
    }
}

/// The whole battery, over one plan.
async fn run_plan(plan: WavePlan) {
    let binaries = Binaries::discover();
    let sandbox = Sandbox::new();
    let mut daemon = PoolDaemon::start(&binaries, &sandbox, &plan.pool).await;
    let client = daemon.client();

    let run = execute(&plan, &sandbox).await;

    let mut report = Report::new(plan.title());
    claim_no_wrong_answer(&mut report, &run.outcomes, plan.expect_disruption);
    claim_every_refusal_is_a_known_code(&mut report, &run.outcomes);
    claim_fungibility(&mut report, &run.outcomes, &run.evidence);
    claim_statelessness(
        &mut report,
        &run.outcomes,
        &run.evidence,
        &sandbox.pool_parent,
    );
    let capacity_refusals =
        claim_refusal_names_a_true_budget(&mut report, &run.outcomes, plan.pool.pool_size);
    claim_reuse_was_exercised(&mut report, &plan, &run, capacity_refusals);
    if plan.disruption.is_some() {
        report.check(run.killed > 0, || {
            "the plan names a disruption and no process was signalled, so every claim about \
             the disrupted path passed vacuously"
                .to_owned()
        });
    }
    let census = claim_slot_accounting(
        &mut report,
        &client,
        plan.pool.pool_size,
        plan.disruption.is_some(),
    )
    .await;

    // A disrupted daemon must be able to serve again -- or, where pmux has
    // deliberately chosen not to recover, must refuse every call rather than
    // answer one. Both are checked, because only a call made after the dust
    // settles tells "the pool refused correctly" apart from "the pool is dead",
    // and a pool that answered here after losing its sidecar would be answering
    // from something pmux does not believe exists.
    if let Some(disruption) = plan.disruption {
        let recovery = wave(&sandbox.socket, callers(100_000, 2)).await;
        claim_no_wrong_answer(&mut report, &recovery, !disruption.recovers());
        claim_every_refusal_is_a_known_code(&mut report, &recovery);
        let served = recovery
            .iter()
            .filter(|outcome| matches!(outcome.answer, Answer::Served(_)))
            .count();
        let rendered = recovery
            .iter()
            .map(|outcome| format!("{:?}", outcome.answer))
            .collect::<Vec<_>>();
        if disruption.recovers() {
            report.check(served > 0, || {
                format!("the pool never recovered: {rendered:?}")
            });
        } else {
            report.check(served == 0, || {
                format!("a daemon with no sidecar answered a call: {rendered:?}")
            });
        }
        report.note(format!("after the dust settled: {rendered:?}"));
    }

    report.check(run.served() > 0, || {
        "no caller was served at all, so nothing about routing was exercised".to_owned()
    });
    report.note(format!(
        "served {}, refused {} ({capacity_refusals} at the cap), no-decision {}, killed {}, \
         rounds {:?}",
        run.served(),
        run.refused(),
        run.transport_failures(),
        run.killed,
        run.round_walls
    ));
    for message in distinct_refusals(&run.outcomes) {
        report.note(format!("refusal: {message}"));
    }
    report.note(format!("census {census}"));
    report.assert_clean();

    daemon.stop().await;
    assert!(
        !sandbox.socket.exists(),
        "a clean shutdown removes the socket"
    );
    assert_pool_parent_drained(&sandbox.pool_parent);
}

/// A clean shutdown erases every instance tree it could prove reaped.
///
/// The slot directories themselves outlive their epochs by design, so what is
/// asserted is that no EPOCH directory survives -- that is the tree holding a
/// caller's transcripts, history and paste cache.
fn assert_pool_parent_drained(pool_parent: &Path) {
    // The place is checked before the emptiness. `walk_files` answers "no
    // files" for a directory that does not exist and for one it cannot read,
    // so the assertion below is satisfied by a pool parent that was never
    // created, was removed, or was misspelled by a caller -- three ways for
    // every wave in this file to prove nothing about shutdown while reporting
    // that it did.
    assert!(
        pool_parent.is_dir(),
        "the pool parent {} is not a readable directory, so 'it holds no files' is not a          statement about shutdown",
        pool_parent.display()
    );
    let survivors: Vec<PathBuf> = walk_files(pool_parent);
    assert!(
        survivors.is_empty(),
        "a clean shutdown left {} file(s) under the pool parent: {:?}",
        survivors.len(),
        survivors
    );
}

// -- the four wave sizes, each running long enough to reuse an instance -----

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "launches a real Path B daemon, a private rmux sidecar and one deterministic Claude per pool instance"]
async fn two_concurrent_callers_across_four_classes() {
    run_plan(WavePlan::new(2, 4, PoolOptions::sized(4))).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "launches a real Path B daemon, a private rmux sidecar and one deterministic Claude per pool instance"]
async fn five_concurrent_callers_across_four_classes() {
    run_plan(WavePlan::new(5, 3, PoolOptions::sized(8))).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore = "launches a real Path B daemon, a private rmux sidecar and one deterministic Claude per pool instance"]
async fn eight_concurrent_callers_across_four_classes() {
    run_plan(WavePlan::new(8, 3, PoolOptions::sized(8))).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "launches a real Path B daemon, a private rmux sidecar and one deterministic Claude per pool instance"]
async fn fifteen_concurrent_callers_across_four_classes() {
    run_plan(WavePlan::new(15, 2, PoolOptions::sized(15))).await;
}

// -- the cap ---------------------------------------------------------------

/// Fifteen callers against two slots: most of them must be refused, and every
/// number the refusal names must be true.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "launches a real Path B daemon, a private rmux sidecar and one deterministic Claude per pool instance"]
async fn fifteen_concurrent_callers_against_two_slots_are_refused_with_a_true_budget() {
    run_plan(
        WavePlan::new(15, 2, PoolOptions::sized(2))
            .expecting_refusals()
            .where_reuse_is_impossible(),
    )
    .await;
}

/// Eight callers against three slots, which is the cold-swap regime: four
/// classes cannot all be resident, so admission rule 3 has to evict an idle
/// instance of another class rather than refuse while holding one.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore = "launches a real Path B daemon, a private rmux sidecar and one deterministic Claude per pool instance"]
async fn eight_concurrent_callers_against_three_slots_cold_swap_rather_than_starve() {
    run_plan(WavePlan::new(8, 3, PoolOptions::sized(3)).expecting_refusals()).await;
}

// -- recycle under load ----------------------------------------------------

/// Drive past the recycle threshold while calls are in flight.
///
/// `recycle_turns 2` against 8 concurrent callers over 4 rounds means every
/// instance is destroyed and re-minted mid-run, repeatedly, while other calls
/// are being admitted, checked out and cleared.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore = "launches a real Path B daemon, a private rmux sidecar and one deterministic Claude per pool instance"]
async fn eight_concurrent_callers_drive_past_the_recycle_threshold_in_flight() {
    run_plan(WavePlan::new(8, 4, PoolOptions::sized(6).recycling_every(2)).expecting_recycle())
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "launches a real Path B daemon, a private rmux sidecar and one deterministic Claude per pool instance"]
async fn fifteen_concurrent_callers_drive_past_the_recycle_threshold_in_flight() {
    run_plan(WavePlan::new(15, 3, PoolOptions::sized(15).recycling_every(1)).expecting_recycle())
        .await;
}

// -- things killed mid-flight ----------------------------------------------

/// Three of eight children killed while their turns are in flight.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore = "launches a real Path B daemon, a private rmux sidecar and one deterministic Claude per pool instance"]
async fn eight_concurrent_callers_survive_children_killed_mid_turn() {
    run_plan(
        WavePlan::new(8, 3, PoolOptions::sized(8)).disrupted_by(Disruption::ChildrenMidTurn(3)),
    )
    .await;
}

/// Five of fifteen children killed while their turns are in flight, which is
/// where the interleavings are richest: some are mid-mint, some mid-turn, some
/// already clearing from the round before.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "launches a real Path B daemon, a private rmux sidecar and one deterministic Claude per pool instance"]
async fn fifteen_concurrent_callers_survive_children_killed_mid_turn() {
    run_plan(
        WavePlan::new(15, 3, PoolOptions::sized(15)).disrupted_by(Disruption::ChildrenMidTurn(5)),
    )
    .await;
}

/// Children killed in the post-answer `/clear` window.
///
/// The instance is not serving anyone, so nothing is owed to a caller -- what
/// must hold is that the pool destroys it rather than proving it clean, and
/// that the slot comes back or is named as leaked.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "launches a real Path B daemon, a private rmux sidecar and one deterministic Claude per pool instance"]
async fn fifteen_concurrent_callers_survive_children_killed_mid_clear() {
    run_plan(
        WavePlan::new(15, 3, PoolOptions::sized(15)).disrupted_by(Disruption::ChildrenMidClear(6)),
    )
    .await;
}

/// The private rmux sidecar killed under fifteen concurrent callers.
///
/// Every pane in the daemon dies at once. Nothing here may return a wrong
/// answer, and the daemon must still be able to answer at all afterwards.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "launches a real Path B daemon, a private rmux sidecar and one deterministic Claude per pool instance"]
async fn fifteen_concurrent_callers_survive_the_sidecar_being_killed() {
    run_plan(WavePlan::new(15, 3, PoolOptions::sized(15)).disrupted_by(Disruption::Sidecar)).await;
}

// -- a declared warm floor -------------------------------------------------

/// A declared warm set is minted at boot and is what the first wave lands on.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore = "launches a real Path B daemon, a private rmux sidecar and one deterministic Claude per pool instance"]
async fn a_declared_warm_floor_serves_the_first_wave_without_minting() {
    let binaries = Binaries::discover();
    let sandbox = Sandbox::new();
    let warm = CLASSES[0];
    let cold = CLASSES[2];
    let options = PoolOptions::sized(8).warming(warm, 4);
    let mut daemon = PoolDaemon::start(&binaries, &sandbox, &options).await;

    let booted = ChildEvidence::read(&sandbox.state_root);
    let mut report = Report::new("a declared warm floor of 4 is minted at boot");
    report.check(booted.launch_count == 4, || {
        format!(
            "the operator declared 4 warm instances and {} were minted at boot",
            booted.launch_count
        )
    });
    for argv in booted.argv_by_cwd.values() {
        report.check(
            argv_value(argv, "--model") == Some(warm.expected_model),
            || format!("a boot mint was launched {:?}", argv_value(argv, "--model")),
        );
    }

    // Four callers of the declared class land on the warm set; four of an
    // undeclared class must each pay a mint. Both are measured, because the
    // difference between them is the only thing a warm floor buys.
    let warm_wave = wave(
        &sandbox.socket,
        (0..4)
            .map(|index| Caller {
                index,
                class: warm,
                secret: fresh_secret(index),
            })
            .collect(),
    )
    .await;
    let after_warm = ChildEvidence::read(&sandbox.state_root);
    // Every one of the four was answered by a process that already existed
    // before the wave. This is the claim, and it is NOT "no further mint
    // happened": emptying a class's idle set starts a background re-warm by
    // design, so counting launches measures the re-warm rather than the floor
    // and reports a correct pool as broken. What the floor buys is that these
    // callers did not WAIT for a launch.
    let boot_cwds: BTreeSet<&String> = booted.argv_by_cwd.keys().collect();
    for outcome in &warm_wave {
        match after_warm.sole_receiver(&outcome.caller.prompt()) {
            Ok(cwd) => report.check(boot_cwds.contains(cwd), || {
                format!(
                    "caller {} of the declared class was answered by {cwd}, which the floor did \
                     not mint at boot",
                    outcome.caller.index
                )
            }),
            Err(reason) => report.violations.push(format!(
                "caller {} cannot be attributed to one instance: {reason}",
                outcome.caller.index
            )),
        }
    }
    report.note(format!(
        "the declared floor minted {} instance(s) at boot; {} launch(es) existed after the warm \
         wave, the surplus being the high-water-mark re-warm",
        booted.launch_count, after_warm.launch_count
    ));
    let cold_wave = wave(
        &sandbox.socket,
        (0..4)
            .map(|index| Caller {
                index: 100 + index,
                class: cold,
                secret: fresh_secret(100 + index),
            })
            .collect(),
    )
    .await;
    let after_cold = ChildEvidence::read(&sandbox.state_root);

    let outcomes: Vec<Outcome> = warm_wave.into_iter().chain(cold_wave).collect();
    claim_no_wrong_answer(&mut report, &outcomes, false);
    claim_fungibility(&mut report, &outcomes, &after_cold);
    let warm_latency = outcomes
        .iter()
        .filter(|outcome| outcome.caller.class == warm)
        .map(|outcome| outcome.elapsed)
        .max()
        .unwrap_or_default();
    let cold_latency = outcomes
        .iter()
        .filter(|outcome| outcome.caller.class == cold)
        .map(|outcome| outcome.elapsed)
        .max()
        .unwrap_or_default();
    report.note(format!(
        "warm class {} slowest {warm_latency:?}; cold class {} slowest {cold_latency:?}",
        warm.label(),
        cold.label()
    ));
    report.assert_clean();

    daemon.stop().await;
    assert_pool_parent_drained(&sandbox.pool_parent);
}

// -- the startup window ----------------------------------------------------

/// Stopping a daemon that is still minting its warm set is a graceful shutdown.
///
/// # The window this exists for
///
/// `shutdown_signal()` used to be an `async fn` called in argument position at
/// `serve_until`, and an `async fn` runs none of its body until it is first
/// polled. `tokio::signal::unix::signal` -- the call that installs the
/// disposition -- therefore ran AFTER `NativeService::start` had minted the
/// entire declared warm set. For the width of that mint, SIGTERM was the
/// kernel's. MEASURED at that shape against real Claude 2.1.226,
/// `--path-b-warm claude-sonnet-5/low=3`, signal 2.6 s in:
///
/// ```text
/// exit 143  |  trees 0/0 1/0 2/0  |  socket PRESENT  |  daemon log: 1 line
/// ```
///
/// The one line was the raw startup `writeln!`. Every `tracing` record was
/// buffered behind a `tracing_appender::non_blocking` writer whose `WorkerGuard`
/// never dropped, so the daemon that left three trees said nothing about
/// having existed -- and the next start refused on one of them with an empty
/// log to explain it.
///
/// # How the signal is aimed, and why `ping` is not the instrument
///
/// The window is bounded structurally: `Pool::start` mints every declared warm
/// instance before it returns, and `serve_until` runs after it. So an observed
/// epoch-tree count of `1` against a declared warm set of `3` IS "startup has
/// begun and has not finished", read from the disk before the signal is sent
/// rather than inferred from the outcome after it.
///
/// `ping` was tried first and is exactly the wrong probe, which is worth
/// recording because it looks like the obvious one. `bind_socket` binds the
/// listener BEFORE `NativeService::start`, so a client's `connect` lands in the
/// kernel backlog and its request sits there un-accepted; the call does not
/// fail, it BLOCKS THROUGH the window and returns `Ok` on the far side of it.
/// Measured: three loop iterations, the third one's `ping` answering `Ok` with
/// the tree count still `0`, and `3` on the next read.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "launches a real Path B daemon, a private rmux sidecar and one deterministic Claude per pool instance"]
async fn a_sigterm_during_the_warm_mint_shuts_down_gracefully_and_leaves_no_tree() {
    const DECLARED_WARM: usize = 3;

    let binaries = Binaries::discover();
    let sandbox = Sandbox::new();
    let options = PoolOptions::sized(4).warming(CLASSES[0], DECLARED_WARM as u32);
    let mut daemon = PoolDaemon::spawn(
        &binaries,
        &sandbox,
        &options,
        &Lane::double(&binaries, &sandbox),
    );

    let mut trees_at_signal = 0;
    for _ in 0..2_400 {
        assert!(
            daemon.child.try_wait().expect("waitable").is_none(),
            "pmuxd exited before it was signalled: {}",
            daemon.diagnostics()
        );
        let trees = epoch_trees(&sandbox.pool_parent).len();
        if trees > 0 && trees < DECLARED_WARM {
            trees_at_signal = trees;
            daemon.signal_terminate();
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert!(
        trees_at_signal > 0 && trees_at_signal < DECLARED_WARM,
        "the loop never caught the daemon between its first and last warm mint, so nothing here \
         is a statement about the startup window: {}",
        daemon.diagnostics()
    );

    let status = daemon.wait_for_exit().await;
    assert_eq!(
        status.code(),
        Some(0),
        "a daemon signalled inside its startup window must exit gracefully, not be killed by the \
         kernel ({status}): {}",
        daemon.diagnostics()
    );
    assert_pool_parent_drained(&sandbox.pool_parent);
    assert!(
        !sandbox.socket.exists(),
        "the socket guard never ran, so {} is stale",
        sandbox.socket.display()
    );

    // The log is the half that makes the residue diagnosable, and it is the
    // half exit 143 destroyed: both records below are emitted before the
    // signal is acted on and both live in the non-blocking appender's buffer
    // until the `WorkerGuard` drops.
    let log = sandbox.root.join("logs").join("pmuxd.log");
    let text = std::fs::read_to_string(&log).unwrap_or_else(|error| {
        panic!("the daemon log at {} is unreadable: {error}", log.display())
    });
    for record in ["pmuxd protocol v1 listening", "pmuxd stopped"] {
        assert!(
            text.contains(record),
            "the daemon log does not carry {record:?}, so a stopped daemon left nothing to read: \
             {text}"
        );
    }
}

// ---------------------------------------------------------------------------
// The real lane
// ---------------------------------------------------------------------------

/// **What the double cannot model: a real Ink frame under concurrency.**
///
/// Every screen-shape defect this codebase has found came from the gap between
/// the double's PTY and Claude's actual renderer, so the deterministic waves
/// above establish nothing about it. This lane runs REAL concurrent turns and
/// measures the one thing a pool exists to change: what a caller waits when the
/// class is already warm versus when it has to be minted.
///
/// It costs real model turns -- `2 * concurrency` of them -- so it is
/// `#[ignore]`d AND gated on `PMUX_POOL_REAL_CLAUDE` naming the executable.
/// Model and effort are `sonnet` / `low` and are not configurable here.
///
/// The two waves run against ONE daemon: the first pays every mint, and the
/// second runs after the pool has quiesced, so it is a checkout of instances
/// that are already idle. Waiting for quiescence is what makes the second wave
/// a warm measurement rather than a race against the post-answer clears.
async fn real_wave(concurrency: usize) {
    let Some(lane) = Lane::real() else { return };
    let binaries = Binaries::discover();
    let sandbox = Sandbox::new();
    let version = lane.claude_version.clone();
    let options = PoolOptions::sized(u32::try_from(concurrency).expect("a small pool"));
    let mut daemon = PoolDaemon::start_on(&binaries, &sandbox, &options, &lane).await;
    let client = daemon.client();

    let mut report = Report::new(format!(
        "REAL claude {version}: {concurrency} concurrent callers, sonnet/low"
    ));
    let ask = |first: usize| -> Vec<Caller> {
        (0..concurrency)
            .map(|offset| Caller {
                index: first + offset,
                class: CLASSES[0],
                secret: fresh_secret(first + offset),
            })
            .collect()
    };

    let cold = real_turns(&sandbox.socket, ask(0)).await;
    let quiesced = quiesced_census(&client).await;
    let warm = real_turns(&sandbox.socket, ask(1_000)).await;

    for (label, outcomes) in [("cold", &cold), ("warm", &warm)] {
        for outcome in outcomes {
            match &outcome.answer {
                Answer::Served(result) => {
                    // A real model is asked to echo one unguessable token. The
                    // answer is trimmed but not otherwise interpreted: this
                    // lane measures concurrency and latency, and turning it
                    // into a model-behaviour assertion would make a wording
                    // change look like a pool defect.
                    report.check(result.text.contains(&outcome.caller.secret), || {
                        format!(
                            "{label} caller {} asked for its own token and got {:?}",
                            outcome.caller.index, result.text
                        )
                    });
                    for other in outcomes {
                        if other.caller.index != outcome.caller.index {
                            report.check(!result.text.contains(&other.caller.secret), || {
                                format!(
                                    "{label} caller {}'s answer carried caller {}'s token",
                                    outcome.caller.index, other.caller.index
                                )
                            });
                        }
                    }
                    report.check(result.model == CLASSES[0].expected_model, || {
                        format!("{label}: the result named model {:?}", result.model)
                    });
                    report.check(result.claude_version == version, || {
                        format!(
                            "{label}: the result named claude {:?}, not the probed {version}",
                            result.claude_version
                        )
                    });
                }
                other => report.violations.push(format!(
                    "{label} caller {} was not served: {other:?}",
                    outcome.caller.index
                )),
            }
        }
    }
    report.check(
        quiesced.get("idle").and_then(Value::as_u64) == Some(concurrency as u64),
        || {
            format!(
                "the pool did not hold {concurrency} idle instance(s) between the waves, so the \
                 second wave was not a warm measurement: {quiesced}"
            )
        },
    );

    report.note(format!("cold {}", latency_line(&cold)));
    report.note(format!("warm {}", latency_line(&warm)));
    report.note(format!(
        "{} real model turns spent",
        cold.len() + warm.len()
    ));
    report.assert_clean();

    daemon.stop().await;
    assert_pool_parent_drained(&sandbox.pool_parent);
}

/// One wave whose prompt is written for a model rather than for the double.
async fn real_turns(socket: &Path, callers: Vec<Caller>) -> Vec<Outcome> {
    let prompts: Vec<(usize, String)> = callers
        .iter()
        .map(|caller| {
            (
                caller.index,
                format!(
                    "Reply with exactly the token {} and nothing else.",
                    caller.secret
                ),
            )
        })
        .collect();
    wave_with(socket, callers, &prompts).await
}

fn latency_line(outcomes: &[Outcome]) -> String {
    let mut millis: Vec<u128> = outcomes
        .iter()
        .map(|outcome| outcome.elapsed.as_millis())
        .collect();
    millis.sort_unstable();
    format!(
        "n={} min={}ms median={}ms max={}ms",
        millis.len(),
        millis.first().copied().unwrap_or_default(),
        millis.get(millis.len() / 2).copied().unwrap_or_default(),
        millis.last().copied().unwrap_or_default(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "SPENDS REAL MODEL TURNS: set PMUX_POOL_REAL_CLAUDE to the Claude executable"]
async fn two_concurrent_real_callers() {
    real_wave(2).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
#[ignore = "SPENDS REAL MODEL TURNS: set PMUX_POOL_REAL_CLAUDE to the Claude executable"]
async fn five_concurrent_real_callers() {
    real_wave(5).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "SPENDS REAL MODEL TURNS: set PMUX_POOL_REAL_CLAUDE to the Claude executable"]
async fn eight_concurrent_real_callers() {
    real_wave(8).await;
}

/// The real lane's gate is a gate, and not an assertion wearing one's message.
///
/// NOT `#[ignore]`d, on purpose: this is the check that the five ignored waves
/// can be enumerated and entered by `--include-ignored` -- which is exactly how
/// `tools/dev/check.sh --push` runs this crate -- without spending a model
/// turn and without panicking. It discriminates in the direction that
/// matters, because on every gate host the variable is unset, so a reintroduced
/// `.expect(..)` fails HERE, in one deterministic test, rather than five cells
/// and nine minutes later as five identical panics.
///
/// Stated as an equality rather than as `is_none()` so it is not vacuous on an
/// operator's own machine either: where the variable IS set the lane must
/// produce an instance, so neither arm can quietly become "always skip".
#[test]
fn the_real_lane_is_gated_on_its_variable_rather_than_asserting_it() {
    let declared = std::env::var_os(REAL_CLAUDE_VARIABLE).is_some();
    // The patterns carry a condition rather than `Some(_)` so that what is
    // asserted is a USABLE lane, not merely a present one: a lane whose version
    // is empty was not measured off the binary, and a promoted lane that kept
    // `operator_profile` on is the operator lane under another name.
    assert_eq!(
        matches!(Lane::real(), Some(lane) if !lane.claude_version.is_empty()),
        declared,
        "the real lane must yield a measured instance exactly when \
         {REAL_CLAUDE_VARIABLE} is set",
    );
    assert_eq!(
        matches!(Lane::promoted(), Some(lane) if !lane.operator_profile),
        declared,
        "the promoted lane must yield an unprivileged instance exactly when \
         {REAL_CLAUDE_VARIABLE} is set",
    );
}

/// Every real-lane test names the variable that gates it, and reaches the lane
/// through the gate.
///
/// Derived from this file's own source rather than restated, because the defect
/// being closed was a MESSAGE that promised a gate the code did not have: the
/// `#[ignore]` text and the doc comment on [`real_wave`] both said the lane was
/// gated on `PMUX_POOL_REAL_CLAUDE`, and `Lane::real` asserted it instead. A
/// message is the only thing an operator reads before deciding what to set, so
/// the two are compared here rather than trusted to stay in step.
#[test]
fn every_real_lane_test_names_and_reaches_its_gate() {
    let source = include_str!("pool_concurrency.rs");
    // A line of this file that is neither a comment nor an attribute. Doc
    // comments name the lane and the variable freely, and the `#[ignore]`
    // messages checked below are attributes that quote the variable on purpose;
    // neither is a place the lane can be constructed or the variable read.
    let code = |line: &&str| {
        let trimmed = line.trim_start();
        !trimmed.starts_with("//") && !trimmed.starts_with("#[")
    };
    // The needles are assembled rather than written whole, so that the three
    // lines below -- which are themselves code -- do not contain the strings
    // they search for. Spelled literally, this test reported ITSELF as an
    // ungated construction, which is a true statement about the file and a
    // useless one about the lane.
    let real = concat!("Lane::", "real()");
    let promoted = concat!("Lane::", "promoted()");
    let inherited = concat!("Self::", "real()");

    let promises: Vec<&str> = source
        .lines()
        .filter(|line| line.trim_start().starts_with("#[ignore"))
        .filter(|line| line.contains("REAL MODEL TURNS"))
        .collect();
    assert!(
        !promises.is_empty(),
        "found no real-lane #[ignore] message; this derivation is broken, not satisfied",
    );
    for promise in &promises {
        assert!(
            promise.contains(REAL_CLAUDE_VARIABLE),
            "an #[ignore] promises real model turns without naming \
             {REAL_CLAUDE_VARIABLE}: {promise}",
        );
    }

    // Every construction of the real lane is BOUND through the gate: `Some(..)`
    // on the caller's side or `?` on the constructor's. A bare
    // `let lane = Lane::real();` matches neither, which is the exact line this
    // file carried at `:2299`, `:2698` and `:2783`.
    let constructions: Vec<&str> = source
        .lines()
        .filter(code)
        .filter(|line| line.contains(real) || line.contains(promoted) || line.contains(inherited))
        .collect();
    assert!(
        !constructions.is_empty(),
        "found no real-lane construction; this derivation is broken, not satisfied",
    );
    for construction in &constructions {
        assert!(
            construction.contains("Some(") || construction.contains('?'),
            "a real-lane construction is not bound through its gate: {construction}",
        );
    }

    // The variable reaches the code through the constant and nowhere else, so
    // the `#[ignore]` messages above are compared against the string the lane
    // truly reads rather than against a second copy of it.
    let literals = source
        .lines()
        .filter(code)
        .filter(|line| line.contains(REAL_CLAUDE_VARIABLE))
        .count();
    assert_eq!(
        literals, 1,
        "{REAL_CLAUDE_VARIABLE} appears as a literal in {literals} code lines; \
         exactly one -- the constant -- may spell it",
    );
}

// ---------------------------------------------------------------------------
// The MCP front end, against a live daemon
// ---------------------------------------------------------------------------

/// One `pmux-mcp` process speaking JSON-RPC over its own stdio.
///
/// Deliberately the real binary over real pipes rather than the library: the
/// thing that had never been established is that `pmux-mcp` and `pmuxd`
/// interoperate at all, and every part of that -- argv, the socket connect, the
/// frame the adapter writes, the native reply it decodes -- lives outside the
/// library.
struct McpClient {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    stdout: tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
    next_id: u64,
}

impl McpClient {
    async fn start(binaries: &Binaries, socket: &Path) -> Self {
        let mut child = tokio::process::Command::new(&binaries.mcp)
            .arg("--socket")
            .arg(socket)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("the candidate pmux-mcp starts");
        let stdin = child.stdin.take().expect("pmux-mcp stdin is piped");
        let stdout = child.stdout.take().expect("pmux-mcp stdout is piped");
        let mut client = Self {
            child,
            stdin,
            stdout: tokio::io::BufReader::new(stdout).lines(),
            next_id: 1,
        };
        let initialized = client
            .call(
                "initialize",
                serde_json::json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "pmux-pool-wave", "version": "0"},
                }),
            )
            .await;
        assert_eq!(
            initialized["result"]["protocolVersion"], "2025-06-18",
            "pmux-mcp did not complete the MCP handshake: {initialized}"
        );
        client.notify("notifications/initialized").await;
        client
    }

    async fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let mut line = serde_json::to_vec(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .expect("a request serializes");
        line.push(b'\n');
        self.stdin.write_all(&line).await.expect("stdin accepts");
        self.stdin.flush().await.expect("stdin flushes");
        let response = tokio::time::timeout(
            Duration::from_millis(TURN_TIMEOUT_MS + 60_000),
            self.stdout.next_line(),
        )
        .await
        .expect("pmux-mcp answered inside the turn budget")
        .expect("pmux-mcp stdout is readable")
        .expect("pmux-mcp closed stdout without answering");
        let value: Value = serde_json::from_str(&response).expect("pmux-mcp emits one JSON object");
        assert_eq!(value["id"], id, "pmux-mcp answered a different request");
        value
    }

    async fn notify(&mut self, method: &str) {
        let mut line = serde_json::to_vec(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": {},
        }))
        .expect("a notification serializes");
        line.push(b'\n');
        self.stdin.write_all(&line).await.expect("stdin accepts");
        self.stdin.flush().await.expect("stdin flushes");
    }

    /// One `tools/call`, returning the whole envelope so a test can read either
    /// the structured result or the typed error out of it.
    async fn tool(&mut self, name: &str, arguments: Value) -> Value {
        self.call(
            "tools/call",
            serde_json::json!({ "name": name, "arguments": arguments }),
        )
        .await
    }

    async fn stop(mut self) {
        drop(self.stdin);
        let status = tokio::time::timeout(Duration::from_secs(20), self.child.wait())
            .await
            .expect("pmux-mcp exits when its stdin closes")
            .expect("pmux-mcp is waitable");
        assert!(
            status.success(),
            "pmux-mcp exited with {status} after a clean stdin close"
        );
    }
}

/// **The MCP front end serves a real turn against a real daemon.**
///
/// `run_stateless` was covered by a blackbox test against a scripted native
/// server and by a schema-drift test, and by nothing that had ever put a real
/// `pmuxd` on the other end of the socket. Those two prove the adapter's
/// framing and its request shape; neither can catch an adapter and a daemon
/// that disagree, because in both the daemon is the test.
///
/// What this adds, and only this can: the socket connect, a real pool checkout
/// behind it, and the DECODE of a real `stateless_result` -- including the
/// fields the scripted double never had to produce in the order the daemon
/// produces them.
///
/// The answer is read from the CHILD side as well. `structuredContent.text` is
/// the adapter's word for what happened; `prompts.jsonl` is the process's, and
/// the two are joined here so a front end that fabricated a plausible answer
/// without ever reaching a pool instance would fail.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "launches a real Path B daemon, a private rmux sidecar and one deterministic Claude per pool instance"]
async fn the_mcp_front_end_runs_a_stateless_turn_against_a_live_daemon() {
    let binaries = Binaries::discover();
    let sandbox = Sandbox::new();
    let mut daemon = PoolDaemon::start(&binaries, &sandbox, &PoolOptions::sized(2)).await;
    let mut mcp = McpClient::start(&binaries, &sandbox.socket).await;
    let mut report = Report::new("MCP run_stateless against a live pmuxd");

    // The tool is advertised by the adapter that is actually connected.
    let listed = mcp.call("tools/list", serde_json::json!({})).await;
    let advertised: BTreeSet<String> = listed["result"]["tools"]
        .as_array()
        .expect("tools/list returns an array")
        .iter()
        .map(|tool| tool["name"].as_str().expect("a tool has a name").to_owned())
        .collect();
    report.check(advertised.contains("run_stateless"), || {
        format!("run_stateless is not advertised: {advertised:?}")
    });

    let caller = Caller {
        index: 0,
        class: CLASSES[0],
        secret: fresh_secret(0),
    };
    let prompt = caller.prompt();
    let answered = mcp
        .tool(
            "run_stateless",
            serde_json::json!({
                "model": CLASSES[0].spelling,
                "effort": "low",
                "prompt": prompt,
            }),
        )
        .await;
    report.check(answered.get("error").is_none(), || {
        format!("run_stateless failed: {answered}")
    });
    let structured = &answered["result"]["structuredContent"];
    report.check(
        structured["text"]
            .as_str()
            .is_some_and(|text| text.contains(&caller.expected_answer())),
        || format!("the MCP answer did not carry the caller's own token: {structured}"),
    );
    report.check(structured["model"] == CLASSES[0].expected_model, || {
        format!("the MCP answer named model {}", structured["model"])
    });
    report.check(structured["claude_version"] == DOUBLE_VERSION, || {
        format!(
            "the MCP answer named claude {}",
            structured["claude_version"]
        )
    });
    report.check(
        answered["result"]["isError"].as_bool() != Some(true),
        || format!("a served turn was reported as an MCP error: {answered}"),
    );

    // The child side. A front end that answered without reaching an instance
    // leaves no row here, and this is the only evidence in the test that a
    // process ran at all.
    let evidence = ChildEvidence::read(&sandbox.state_root);
    match evidence.sole_receiver_argv(&prompt) {
        Ok(argv) => {
            report.check(
                argv_value(argv, "--model") == Some(CLASSES[0].expected_model),
                || format!("the process that answered was launched with argv {argv:?}"),
            );
            report.check(
                argv_value(argv, "--effort") == CLASSES[0].expected_effort,
                || format!("the process that answered was launched with argv {argv:?}"),
            );
        }
        Err(reason) => report.violations.push(format!(
            "the MCP prompt reached no single instance: {reason}"
        )),
    }

    // And a refusal survives the adapter as a typed MCP error rather than as a
    // fabricated success. `run_stateless` names no resource, so the reachable
    // refusal is an inadmissible class.
    let refused = mcp
        .tool(
            "run_stateless",
            serde_json::json!({
                "model": "definitely-not-a-model",
                "effort": "low",
                "prompt": "unreachable",
            }),
        )
        .await;
    report.check(
        refused.get("error").is_some() || refused["result"]["isError"] == Value::Bool(true),
        || format!("an inadmissible model was not reported as an error: {refused}"),
    );

    report.assert_clean();
    mcp.stop().await;
    daemon.stop().await;
    assert_pool_parent_drained(&sandbox.pool_parent);
}

// ---------------------------------------------------------------------------
// Promotion, and what `/clear` leaves in the model's context
// ---------------------------------------------------------------------------

/// **Path B serves a real turn with nothing on `pmuxd` argv admitting it.**
///
/// Every other real-lane test in this file, and every session that has ever
/// driven Path B, passes `--tested-claude-profile`. That flag is the difference
/// between a product and a private capability: without it
/// `require_tested_for_minified_cell` refuses every mint, so Path B worked for
/// whoever knew the flag and refused for everyone else.
///
/// This lane turns the flag OFF. The only thing that can admit the mint is
/// `compatibility::PROMOTED_PROFILES`, so a served turn here is the promotion
/// working end to end -- through `pmuxd`'s own argv parsing, the pool's
/// `RequireTested` mint, and the real Claude the operator has installed.
///
/// It also reads the drain back off the wire. A promoted cell carries a
/// MEASURED `transcript_drain_ms`, and the one number that would prove nothing
/// was measured is the untested fallback appearing in its place.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "SPENDS REAL MODEL TURNS: set PMUX_POOL_REAL_CLAUDE to the Claude executable"]
async fn a_promoted_profile_serves_a_real_turn_with_no_operator_flag() {
    let Some(lane) = Lane::promoted() else { return };
    let binaries = Binaries::discover();
    let sandbox = Sandbox::new();
    let version = lane.claude_version.clone();
    let mut daemon = PoolDaemon::start_on(&binaries, &sandbox, &PoolOptions::sized(1), &lane).await;
    let client = daemon.client();
    let mut report = Report::new(format!(
        "PROMOTED profile, no --tested-claude-profile on argv, claude {version}"
    ));

    // The health tree first: the compatibility layer has a subject here (there
    // IS a pool) and nothing on argv gave it one.
    let diagnosis = client.diagnose().await.expect("the daemon answers doctor");
    let compatibility = diagnosis
        .layer(HealthLayerName::CompatibilityProfile)
        .expect("the compatibility layer is reported");
    report.check(compatibility.finding == LayerFinding::Exercised, || {
        format!(
            "a pool daemon with a promoted cell reported {:?}: {}",
            compatibility.finding, compatibility.detail
        )
    });

    let caller = Caller {
        index: 0,
        class: CLASSES[0],
        secret: fresh_secret(0),
    };
    let outcomes = real_turns(&sandbox.socket, vec![caller.clone()]).await;
    match &outcomes[0].answer {
        Answer::Served(result) => {
            report.check(result.text.contains(&caller.secret), || {
                format!("the promoted cell answered {:?}", result.text)
            });
            report.check(result.claude_version == version, || {
                format!("the answer named claude {:?}", result.claude_version)
            });
            report.note(format!(
                "served in {}ms by claude {}",
                outcomes[0].elapsed.as_millis(),
                result.claude_version
            ));
        }
        other => report.violations.push(format!(
            "a promoted profile did not admit a real turn: {other:?}"
        )),
    }

    report.assert_clean();
    daemon.stop().await;
    assert_pool_parent_drained(&sandbox.pool_parent);
}

/// **What `/clear` costs the caller, measured turn by turn on one instance.**
///
/// A stateless product's whole claim is that turn N+1 does not see turn N. An
/// unexplained constant in the model's context is exactly where a violation of
/// that claim would hide, and one had been observed and left unexplained:
/// `input_tokens` steps once at the first `/clear` and then never moves.
///
/// This measures it rather than restating it. One instance, `recycle_turns`
/// high enough that the pool clears rather than recycles between every turn, N
/// sequential callers, and the per-turn usage read back off the wire.
///
/// The claims are stated as three separate checks because they fail
/// differently:
///
/// 1. There IS a step, at the FIRST clear. (If there is none, the premise is
///    gone and everything below is vacuous.)
/// 2. The step does not GROW. Turns 2..N follow two, three, ... clears, and
///    residue that accumulated would show as a rising sequence. A constant is
///    per-clear; a ramp is a leak.
/// 3. A prompt roughly five times longer moves `input_tokens` by roughly its
///    own size. This is the control that says `input_tokens` is a live
///    measurement of THIS turn's input and not a stale value copied forward --
///    which is the other thing a flat sequence could mean.
///
/// The transcripts are copied out before shutdown, because the pool erases
/// every root it can prove reaped and the rows that explain the step live in
/// them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "SPENDS REAL MODEL TURNS: set PMUX_POOL_REAL_CLAUDE to the Claude executable"]
async fn the_context_a_cleared_instance_carries_is_constant_across_clears() {
    let Some(lane) = Lane::promoted() else { return };
    let binaries = Binaries::discover();
    let sandbox = Sandbox::new();
    let version = lane.claude_version.clone();
    // One instance, so every turn after the first follows a `/clear` on the
    // SAME process. `recycle_turns` well above the turn count keeps it that way:
    // a recycle would mint a fresh instance and reset the measurement.
    let options = PoolOptions::sized(1).recycling_every(64);
    let mut daemon = PoolDaemon::start_on(&binaries, &sandbox, &options, &lane).await;
    let mut report = Report::new(format!("/clear residue on claude {version}, sonnet/low"));

    // ~450 tokens of filler, generated rather than pasted so the file stays
    // readable. It is deliberately dull, repetitive prose: the turn is measured
    // for its token count, not its answer.
    let filler = format!(
        "{FILLER_MARKER} {}",
        "The quick brown fox jumps over the lazy dog while the patient tortoise considers the \
         merits of a slower pace. "
            .repeat(24)
    );
    let short = "Reply with exactly the word OK and nothing else.";
    // Single line: the prompt is typed into a TUI as one bracketed paste, and a
    // newline in it is a different injection path than the one every other turn
    // here takes.
    let long = format!("{filler} {short}");
    let prompts: Vec<(&str, String)> = vec![
        ("t1-cold", short.to_owned()),
        ("t2-after-1-clear", short.to_owned()),
        ("t3-after-2-clears", short.to_owned()),
        ("t4-after-3-clears", short.to_owned()),
        ("t5-long-prompt", long),
        ("t6-after-5-clears", short.to_owned()),
    ];

    let mut measured: Vec<(&str, u64, u64, u64, u64)> = Vec::new();
    let quiescer = daemon.client();
    for (label, prompt) in &prompts {
        // Wait for the pool to be idle before each turn. `spawn_clear` answers
        // the caller BEFORE `/clear` is typed, so a sequential caller that
        // submits the instant it has bytes meets a pool whose only slot is
        // `Clearing` and is refused -- measured here, exactly as the census now
        // renders it. Waiting is what makes each turn a checkout of a cleared
        // instance rather than a race with the clear that follows the previous
        // one.
        let quiesced = quiesced_census(&quiescer).await;
        assert_eq!(
            quiesced.get("clearing").and_then(Value::as_u64),
            Some(0),
            "{label} would have raced the previous turn's clear: {quiesced}"
        );
        let client = PmuxClient::new(&sandbox.socket).expect("a client binds the socket");
        let result = client
            .run_stateless(RunStatelessRequest {
                model: CLASSES[0].spelling.to_owned(),
                effort: CLASSES[0].effort,
                prompt: prompt.clone(),
                deadline_unix_ms: None,
            })
            .await
            .unwrap_or_else(|error| panic!("{label} was not served: {error:?}"));
        let usage = result.usage.main;
        measured.push((
            label,
            usage.input_tokens,
            usage.output_tokens,
            usage.cache_creation_input_tokens,
            usage.cache_read_input_tokens,
        ));
        report.note(format!(
            "{label}: input={} output={} cache_creation={} cache_read={} prompt_chars={}",
            usage.input_tokens,
            usage.output_tokens,
            usage.cache_creation_input_tokens,
            usage.cache_read_input_tokens,
            prompt.len(),
        ));
    }

    let transcripts = copy_pool_transcripts(&sandbox.pool_parent, &sandbox.root.join("evidence"));
    report.note(format!(
        "{} transcript(s) copied before shutdown",
        transcripts.len()
    ));

    let cold = measured[0].1;
    let after_first = measured[1].1;
    let step = i64::try_from(after_first).unwrap() - i64::try_from(cold).unwrap();
    report.note(format!(
        "cold={cold} after-one-clear={after_first}; step at the first /clear: {step} input token(s)"
    ));
    report.check(step > 0, || {
        format!("no step was observed at the first clear: {measured:?}")
    });
    // The STEP is noted, not asserted. Measured across three runs on one host
    // the cleared value was 326 every time and the cold value was 194, 194 and
    // 171 -- so the invariant is the cleared context, not the difference. What
    // varies at cold start is whatever Claude Code puts in a fresh session's
    // context and `/clear` then replaces; the number this product depends on is
    // the one below, which does not move.

    // Claim 2: flat across further clears. Every short-prompt turn that follows
    // a clear is compared against the FIRST one, so a ramp of any slope fails
    // rather than only a jump.
    for entry in [measured[2], measured[3], measured[5]] {
        report.check(entry.1 == after_first, || {
            format!(
                "{} carries {} input token(s) against {after_first} after the first clear: the \
                 residue is not constant, so it accumulates",
                entry.0, entry.1
            )
        });
    }

    // Claim 3: the turn's whole input tracks the prompt.
    //
    // Over the THREE counters, not `input_tokens` alone. `input_tokens` is only
    // the part of the request that fell after the last cache breakpoint, so a
    // prompt large enough for Claude Code to place one moves into
    // `cache_creation_input_tokens` and `input_tokens` COLLAPSES. Measured:
    // a 2690-character prompt reported `input=2 cache_creation=1214` against
    // `input=326 cache_creation=0` for a 48-character one. Reading
    // `input_tokens` alone there says a five-times-longer prompt cost 162 times
    // FEWER tokens, which is how a flat `input_tokens` under a long filler
    // prompt came to look like evidence of a stale reading.
    let total = |entry: (&str, u64, u64, u64, u64)| entry.1 + entry.3 + entry.4;
    let long_turn = measured[4];
    let long_prompt_chars = prompts[4].1.len();
    let short_prompt_chars = prompts[1].1.len();
    report.note(format!(
        "total input (input + cache_creation + cache_read): cold={} after-one-clear={} long={}",
        total(measured[0]),
        total(measured[1]),
        total(long_turn)
    ));
    report.check(total(long_turn) > total(measured[1]) + 100, || {
        format!(
            "a prompt of {long_prompt_chars} characters reported {} total input token(s) against \
             {} for a {short_prompt_chars}-character one; the reported usage is not measuring \
             this turn's input",
            total(long_turn),
            total(measured[1])
        )
    });

    // Claim 4: the residue is IDENTIFIED, not merely bounded.
    //
    // The step is a number until something says which bytes it is. `/clear`
    // rotates the transcript, and the successor opens with the rows Claude Code
    // writes for a local command: a `<local-command-caveat>` meta user row, the
    // `<command-name>/clear</command-name>` user row, and a
    // `local_command` system row carrying `<local-command-stdout>`. Those three
    // messages are the whole difference between the cold context and every
    // cleared one, and this reads them off the instance's own transcripts.
    let mut rotated = 0_usize;
    let mut residue_chars = 0_usize;
    let mut carried_prompts = 0_usize;
    for (index, transcript) in transcripts.iter().enumerate() {
        let rows = read_jsonl(transcript);
        let caveat = rows.iter().find(|row| {
            row["type"] == "user"
                && row["isMeta"] == Value::Bool(true)
                && row["message"]["content"]
                    .as_str()
                    .is_some_and(|text| text.starts_with("<local-command-caveat>"))
        });
        let command = rows.iter().find(|row| {
            row["type"] == "user"
                && row["message"]["content"]
                    .as_str()
                    .is_some_and(|text| text.starts_with("<command-name>/clear</command-name>"))
        });
        let stdout = rows.iter().find(|row| {
            row["subtype"] == "local_command"
                && row["content"]
                    .as_str()
                    .is_some_and(|text| text.starts_with("<local-command-stdout>"))
        });
        let carried = [caveat.is_some(), command.is_some(), stdout.is_some()];
        if carried.iter().all(|present| *present) {
            rotated += 1;
            let chars = [
                caveat.and_then(|row| row["message"]["content"].as_str()),
                command.and_then(|row| row["message"]["content"].as_str()),
                stdout.and_then(|row| row["content"].as_str()),
            ]
            .into_iter()
            .flatten()
            .map(str::len)
            .sum::<usize>();
            residue_chars = residue_chars.max(chars);
        } else {
            report.check(carried == [false, false, false], || {
                format!(
                    "transcript {index} carries only part of the /clear preamble: \
                     caveat={} command={} stdout={}",
                    carried[0], carried[1], carried[2]
                )
            });
        }
        // AND NOTHING ELSE SURVIVES. Structural, not textual: five of the six
        // prompts here are the SAME string, chosen so their token counts are
        // comparable, so "does this file contain another caller's prompt" is
        // not a question their text can answer. What it can answer is how many
        // caller prompts are in the file at all -- a transcript that carried a
        // previous turn would have two. This is the stateless claim itself, and
        // it is the reason an unexplained constant in the context was worth
        // chasing.
        let caller_prompts = rows
            .iter()
            .filter(|row| {
                row["type"] == "user"
                    && row["isMeta"] != Value::Bool(true)
                    && row["message"]["content"]
                        .as_str()
                        .is_some_and(|text| !text.starts_with("<command-name>"))
            })
            .count();
        report.check(caller_prompts <= 1, || {
            format!("transcript {index} carries {caller_prompts} caller prompts, not at most one")
        });
        carried_prompts += caller_prompts;
    }
    // The one textually unique prompt in the run appears in exactly one
    // transcript. The structural check above cannot see a copy that landed in
    // a DIFFERENT file, and this one can.
    // Compared against the PARSED row content, not the file's bytes: the
    // transcript is JSON, so a prompt containing a newline is escaped in the
    // file and a raw substring search finds nothing -- which is a check that
    // passes for the wrong reason in one direction and fails for the wrong
    // reason in the other.
    let unique_marker = FILLER_MARKER;
    let carrying = transcripts
        .iter()
        .filter(|transcript| {
            read_jsonl(transcript).iter().any(|row| {
                row["message"]["content"]
                    .as_str()
                    .is_some_and(|text| text.contains(unique_marker))
            })
        })
        .count();
    report.check(carrying == 1, || {
        format!("the one unique prompt in this run appears in {carrying} transcript(s), not one")
    });
    report.check(carried_prompts == prompts.len(), || {
        format!(
            "{carried_prompts} caller prompt(s) are spread across {} transcript(s) for {} turns",
            transcripts.len(),
            prompts.len()
        )
    });
    report.note(format!(
        "{rotated} rotated transcript(s) carry the /clear preamble; its three messages are \
         {residue_chars} characters"
    ));
    report.check(rotated >= 2, || {
        format!(
            "only {rotated} of {} transcript(s) carried the /clear preamble, so the residue was \
             not identified",
            transcripts.len()
        )
    });

    report.assert_clean();
    daemon.stop().await;
    assert_pool_parent_drained(&sandbox.pool_parent);
}

/// Copies every transcript under the pool parent aside, returning the copies.
///
/// Called BEFORE shutdown, which erases every root it can prove reaped. The
/// copies land inside the sandbox, so they go with it: nothing here is meant to
/// outlive the test, and a caller's prompt is in them.
fn copy_pool_transcripts(pool_parent: &Path, destination: &Path) -> Vec<PathBuf> {
    std::fs::create_dir_all(destination).expect("the evidence directory is creatable");
    let mut taken = Vec::new();
    for file in walk_files(pool_parent) {
        if file.extension().and_then(|value| value.to_str()) != Some("jsonl") {
            continue;
        }
        let relative = file
            .strip_prefix(pool_parent)
            .expect("a walked file is under the parent")
            .to_string_lossy()
            .replace('/', "_");
        let copy = destination.join(relative);
        if std::fs::copy(&file, &copy).is_ok() {
            taken.push(copy);
        }
    }
    taken.sort();
    taken
}
