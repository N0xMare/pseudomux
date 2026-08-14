#![cfg(unix)]

//! Standing cross-cell contamination harness for Path B.
//!
//! Path B's product claim is that any instance can serve any turn, because
//! after `/clear` no instance carries state that distinguishes it from any
//! other. A LEAK is any way caller B observes anything caller A did. Every leak
//! found so far was found by reproduction, never by reading, and three of the
//! four would have been caught here: they all ended with one caller's bytes
//! reachable from another caller's cell.
//!
//! The harness is therefore stated as a resource sweep rather than a scenario.
//! N cells are each given a distinct unguessable secret, run concurrently
//! against ONE daemon, cleared, and then every byte reachable from each cell is
//! searched for every OTHER cell's secret. The channel table below is the
//! explicit part: each named channel is scanned and REPORTED, present or
//! absent, so "we never exercised it" and "it was clean" are distinguishable.
//! Anything in a root that matches no named channel is attributed to
//! [`UNCLASSIFIED_CHANNEL`] and scanned anyway, so a channel Claude invents
//! after this file was written is caught and NAMED rather than skipped.
//!
//! Two lanes, one body:
//!
//! * [`Lane::Double`] drives `pmux-test-claude`. Deterministic, credential-free,
//!   free to run, and the lane CI can afford at N=15.
//! * [`Lane::RealClaude`] drives the operator's actual `claude` behind
//!   `PMUX_CONTAMINATION_REAL_CLAUDE=1`. The double cannot reproduce the TUI's
//!   composer, `history.jsonl` recall, or the paste cache -- which is exactly
//!   how the composer blocker escaped four rounds of double-only testing -- so
//!   the double lane is a regression net, not a proof.
//!
//! The harness returns [`Violation`]s instead of panicking so that its ability
//! to FAIL is itself a test: [`the_harness_reports_contamination_when_two_cells_share_one_configuration_root`]
//! runs the same body under [`Topology::SharedRoot`] and asserts the sweep
//! reports the leak. A contamination harness that cannot fail is worse than
//! none.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pseudomux_client::{ClientError, PmuxClient, exact_environment_snapshot};
use pseudomux_e2e::{
    TEST_ANTHROPIC_SECRET, TEST_ENV_ATTESTATION_MARKER, TEST_ENV_PATCHED_VALUE,
    TEST_ENV_SAFE_CONFIG_VALUE, TEST_ENV_SET_ONLY_VALUE, TEST_PROVIDER_SECRET,
    TEST_SUBSCRIPTION_KEYS, TEST_TRANSPARENT_EXACT_KEYS,
};
use pseudomux_protocol::v1::{
    AuthPolicy, ClaudeLaunchConfig, ClosePolicy, CompatibilityPolicy, ConfigIsolation,
    DisconnectAction, EffortLevel, EnvironmentSpec, ErrorCode, EventPayload, InputTransport,
    LifecycleMode, RetentionPolicy, SessionCell, SessionGenerationId, SessionId, SessionIdentity,
    StartSessionRequest, SubscribeEventsRequest, SystemPromptPolicy, TerminalProfile, TerminalSpec,
    TurnLeasePolicy, TurnOutcome, TurnRequest, TurnResult,
};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// The channel table. Adding a channel is one row.
// ---------------------------------------------------------------------------

/// One place a cell's private Claude configuration root can carry bytes.
///
/// `relative` is matched as a path prefix, so a row names either a single file
/// or a whole subtree without the caller saying which.
#[derive(Clone, Copy, Debug)]
struct Channel {
    name: &'static str,
    relative: &'static str,
}

/// Every channel this harness asserts on, in the cell's own configuration root.
///
/// `history.jsonl` is the row that motivates the table: Claude records EVERY
/// typed prompt there verbatim, it is append-only, `/clear` does not truncate
/// it (it appends `/clear` as a row of its own), and composer recall filters by
/// `project` -- the cwd -- and NOT by session, so it spans `/clear` by
/// construction. A per-session assertion cannot see it.
const ROOT_CHANNELS: &[Channel] = &[
    Channel {
        name: "projects",
        relative: "projects",
    },
    Channel {
        name: "history",
        relative: "history.jsonl",
    },
    Channel {
        name: "paste-cache",
        relative: "paste-cache",
    },
    Channel {
        name: "shell-snapshots",
        relative: "shell-snapshots",
    },
    Channel {
        name: "todos",
        relative: "todos",
    },
    Channel {
        name: "file-history",
        relative: "file-history",
    },
    Channel {
        name: "backups",
        relative: "backups",
    },
    Channel {
        name: "global-config",
        relative: ".claude.json",
    },
    Channel {
        name: "user-settings",
        relative: "settings.json",
    },
    Channel {
        name: "statsig",
        relative: "statsig",
    },
    Channel {
        name: "ide",
        relative: "ide",
    },
    Channel {
        name: "plugins",
        relative: "plugins",
    },
    Channel {
        name: "shell-history",
        relative: "shell-history",
    },
    Channel {
        name: "logs",
        relative: "logs",
    },
    // The four rows below were not written from the specification: they were
    // NAMED by an unclassified-entry report from a real 2.1.220 launch. That is
    // the table working as intended -- an unknown channel is swept first and
    // named second, never skipped.
    Channel {
        name: "cache",
        relative: "cache",
    },
    Channel {
        name: "sessions",
        relative: "sessions",
    },
    Channel {
        name: "credentials",
        relative: ".credentials.json",
    },
    Channel {
        name: "store-db",
        relative: "__store.db",
    },
];

/// Anything in a root that no row above claims. Scanned exactly as hard, and
/// named in the evidence table so a new Claude artifact is visible the first
/// time it appears rather than silently unswept.
const UNCLASSIFIED_CHANNEL: &str = "unclassified-root-entry";

/// Channels that are not files in a root, asserted by the same needle sweep.
const MODEL_ANSWER_CHANNEL: &str = "model-answer";
const OPERATOR_HOME_CHANNEL: &str = "operator-home";
const TRANSCRIPT_INVENTORY_CHANNEL: &str = "transcript-inventory";
const BOUND_TRANSCRIPT_CHANNEL: &str = "bound-transcript";
/// Admission itself, treated as a channel.
///
/// Every leak in this family has arrived through the SECOND DOOR: a plain
/// `environment.set["CLAUDE_CONFIG_DIR"]` naming a live cell's root in a
/// spelling the guard did not recognise. Until this row existed the harness
/// drove every cell through `config_isolation` with an already-canonicalized
/// root, so it never constructed the offending request shape and structurally
/// could not have caught leak 5 or leak 5b.
const SECOND_DOOR_CHANNEL: &str = "plain-env-config-dir";

/// The spellings [`plain_env_spellings`] produces on every platform: identity,
/// trailing slash, doubled separator, `.`, `..` through an existing directory,
/// and `..` through a MISSING one. macOS adds the firmlink alias.
const SECOND_DOOR_MINIMUM_SPELLINGS: usize = 6;

/// Admission's OTHER axis, treated as a channel.
///
/// [`SECOND_DOOR_CHANNEL`] varies the SPELLING of one directory; every leak in
/// that family was the guard failing to recognise a live cell's own root.
/// LEAK 7 is not a spelling. It is the RELATION: eight starts that never named
/// a directory a live minified cell binds, and bound one anyway -- as a parent
/// of it, as a child of it, in the other ROLE (a cwd where the cell has a
/// configuration root), or through `HOME`. The guard compared for IDENTITY, so
/// `R/sub` was not `R` and no incumbent was found.
///
/// LEAK 8 is the same axis one turn further: the relation was asked correctly
/// and asked of the wrong PATH. The containment walk was lexical over the
/// spelling the caller sent, so a spelling whose components are symlinks was
/// walked as though those components were real directories -- and the true
/// ancestors of what the child reaches, which is where a live cell's root sits,
/// were never visited. Two rows here are that shape.
const CONTAINMENT_CHANNEL: &str = "live-cell-containment";

/// The relations [`containment_shapes`] builds. Asserted as a COUNT by
/// [`assert_clean`], for the reason the second door's count is asserted: a
/// probe that quietly stopped constructing the shape would report exactly the
/// same clean sweep as one that constructed it and was refused.
const CONTAINMENT_MINIMUM_SHAPES: usize = 10;

// ---------------------------------------------------------------------------
// Findings
// ---------------------------------------------------------------------------

/// One observation of one cell's bytes from somewhere that must not hold them.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Violation {
    cell: usize,
    channel: String,
    locator: String,
    detail: String,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "cell {} channel {} at {}: {}",
            self.cell, self.channel, self.locator, self.detail
        )
    }
}

/// What one channel was observed to hold, whether or not it was clean.
///
/// Recorded for EVERY channel including the empty ones, because "this channel
/// was clean" and "this channel does not exist in this lane" are different
/// facts and only one of them is evidence.
#[derive(Clone, Debug, Default)]
struct ChannelObservation {
    entries: usize,
    bytes: u64,
}

#[derive(Debug, Default)]
struct Report {
    violations: Vec<Violation>,
    channels: BTreeMap<String, ChannelObservation>,
    /// Per cell, the channels in its OWN root that were observed to hold its
    /// OWN secret.
    ///
    /// This is the anti-vacuity half of the harness, and it is not decoration.
    /// A sweep that finds no foreign secret because it is reading an empty
    /// channel reports exactly the same "clean" as one that is reading a full
    /// one. A channel only counts as tested once this shows the needle IS
    /// findable there.
    positive_controls: BTreeMap<usize, BTreeSet<String>>,
    /// Every path a real Claude wrote into a private root that no row of
    /// [`ROOT_CHANNELS`] claims. Reported by name, so adding the row it wants
    /// is a one-line edit rather than an investigation.
    unclassified: BTreeSet<String>,
    notes: Vec<String>,
    real_turns: usize,
}

impl Report {
    fn observe(&mut self, channel: &str, bytes: u64) {
        let observation = self.channels.entry(channel.to_owned()).or_default();
        observation.entries += 1;
        observation.bytes += bytes;
    }

    fn ensure_channel(&mut self, channel: &str) {
        self.channels.entry(channel.to_owned()).or_default();
    }

    fn control(&mut self, cell: usize, channel: &str) {
        self.positive_controls
            .entry(cell)
            .or_default()
            .insert(channel.to_owned());
    }

    fn violate(&mut self, cell: usize, channel: &str, locator: String, detail: String) {
        self.violations.push(Violation {
            cell,
            channel: channel.to_owned(),
            locator,
            detail,
        });
    }

    fn render(&self, title: &str) -> String {
        let mut text = format!("\n=== {title} ===\n");
        for (channel, observation) in &self.channels {
            let _ = writeln!(
                text,
                "  channel {channel:<24} entries={:<4} bytes={}",
                observation.entries, observation.bytes
            );
        }
        if !self.unclassified.is_empty() {
            let _ = writeln!(
                text,
                "  unclassified root entries (swept, unnamed): {:?}",
                self.unclassified
            );
        }
        for (cell, channels) in &self.positive_controls {
            let _ = writeln!(
                text,
                "  positive control: cell {cell} own secret findable in {channels:?}"
            );
        }
        for note in &self.notes {
            let _ = writeln!(text, "  note: {note}");
        }
        if self.violations.is_empty() {
            let _ = writeln!(text, "  VIOLATIONS: none");
        } else {
            let _ = writeln!(text, "  VIOLATIONS: {}", self.violations.len());
            for violation in &self.violations {
                let _ = writeln!(text, "    {violation}");
            }
        }
        text
    }
}

// ---------------------------------------------------------------------------
// Lanes
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Lane {
    Double,
    RealClaude,
}

/// Everything that differs between the two lanes, and nothing else. The body of
/// the harness never branches on [`Lane`] except through this struct, so the
/// two lanes are the same test.
#[derive(Clone, Debug)]
struct LaneConfig {
    lane: Lane,
    claude: PathBuf,
    claude_version: String,
    transcript_drain_ms: u64,
    model: Option<String>,
    effort: Option<EffortLevel>,
    turn_deadline_ms: u64,
    observer_grace_ms: u64,
    /// The exact launch environment this lane's Claude is entitled to.
    ///
    /// The two lanes differ absolutely here and nowhere else that matters: the
    /// double refuses to launch unless it is handed the published attestation
    /// contract of `pseudomux_e2e` (which is how it proves, on every launch,
    /// that credential and parent-identity names were stripped), while real
    /// Claude needs the operator's actual environment or it cannot
    /// authenticate.
    environment: EnvironmentSpec,
}

impl LaneConfig {
    fn double(binaries: &Binaries, sandbox: &Sandbox) -> Self {
        Self {
            lane: Lane::Double,
            claude: binaries.fake_claude.clone(),
            claude_version: "9.9.9".to_owned(),
            transcript_drain_ms: 50,
            model: Some("test-model".to_owned()),
            effort: None,
            turn_deadline_ms: 60_000,
            observer_grace_ms: 20_000,
            environment: double_environment(sandbox),
        }
    }

    /// The lane that can actually observe a composer, `history.jsonl` recall,
    /// or a paste cache. Everything here is measured from the operator's own
    /// installation rather than assumed: an untested profile is refused by the
    /// daemon before a child exists, so a guessed version would produce a
    /// refusal rather than a false pass.
    fn real_claude() -> Self {
        let claude = std::env::var_os("PMUX_CONTAMINATION_CLAUDE")
            .map(PathBuf::from)
            .unwrap_or_else(|| which("claude").expect("`claude` must be on PATH for the real lane"))
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
        Self {
            lane: Lane::RealClaude,
            claude,
            claude_version,
            transcript_drain_ms: 2_000,
            model: Some("sonnet".to_owned()),
            effort: Some(EffortLevel::Low),
            turn_deadline_ms: 180_000,
            observer_grace_ms: 60_000,
            environment: exact_environment_snapshot().expect("the operator's environment is UTF-8"),
        }
    }

    fn tested_profile(&self) -> String {
        serde_json::json!({
            "claude_version": self.claude_version,
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "terminal_profile": "transparent",
            "input_transport": "sdk",
            "transcript_drain_ms": self.transcript_drain_ms,
        })
        .to_string()
    }

    /// The prompt that plants one cell's secret. Typed into the TUI, so on the
    /// real lane it lands verbatim in that root's `history.jsonl` as well as in
    /// the transcript -- which is the point.
    fn plant_prompt(&self, secret: &str) -> String {
        match self.lane {
            Lane::Double => format!("PMUX_TEST_ECHO:{secret}"),
            Lane::RealClaude => {
                format!("Memorize this token exactly: {secret}. Reply with the single word OK.")
            }
        }
    }

    /// The prompt that asks a cleared cell what it remembers. Deliberately 113
    /// characters on the real lane: it wraps the composer, which is the shape
    /// that could not submit before the Gate 1 reference point was corrected.
    fn recall_prompt(&self) -> String {
        match self.lane {
            Lane::Double => "PMUX_TEST_ECHO:NONE".to_owned(),
            Lane::RealClaude => {
                "List every memorized token from earlier in this conversation, or reply with the single word NONE."
                    .to_owned()
            }
        }
    }

    fn assert_plant_answer(&self, secret: &str, text: &str) {
        match self.lane {
            Lane::Double => assert_eq!(
                text,
                format!("pmux-test-echo:{secret}"),
                "the deterministic double must echo the planted secret back"
            ),
            Lane::RealClaude => assert!(
                !text.trim().is_empty(),
                "a real Claude turn must produce an answer"
            ),
        }
    }
}

fn which(program: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|directory| directory.join(program))
            .find(|candidate| {
                std::fs::metadata(candidate)
                    .map(|metadata| {
                        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                    })
                    .unwrap_or(false)
            })
    })
}

// ---------------------------------------------------------------------------
// Topology: the axis the discrimination proof turns
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Topology {
    /// Path B as shipped: every cell is a minified cell with its own private,
    /// empty, owner-only configuration root and its own cwd, and is cleared
    /// between the plant turn and the recall turn.
    PerCell,
    /// Deliberately broken isolation, used ONLY by the self-test that proves
    /// this harness can fail. Every cell shares one configuration root and one
    /// cwd.
    ///
    /// The cells are Full rather than minified because that is the honest way
    /// to bypass the guard: `admit_config_root` refuses a shared root to a
    /// minified cell in either direction, so a minified SharedRoot run would
    /// fail at admission and prove the GUARD, not the SWEEP. A Full cell is
    /// entitled to share a root, so this run reaches the sweep with two
    /// callers' bytes genuinely reachable from one root -- exactly the shape a
    /// regressed guard would produce -- without patching a single line of
    /// service code.
    SharedRoot,
}

impl Topology {
    fn cell(self) -> SessionCell {
        match self {
            Self::PerCell => SessionCell::Minified,
            Self::SharedRoot => SessionCell::Full,
        }
    }

    /// Only a minified cell may be cleared, so the broken topology cannot be.
    fn clears(self) -> bool {
        self == Self::PerCell
    }
}

// ---------------------------------------------------------------------------
// Binaries and daemon
// ---------------------------------------------------------------------------

struct Binaries {
    pmuxd: PathBuf,
    rmuxd: PathBuf,
    launcher: PathBuf,
    fake_claude: PathBuf,
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
        Self {
            pmuxd: executable("pmuxd"),
            rmuxd: executable("pmux-rmuxd"),
            launcher: executable("pmux-launcher"),
            fake_claude: executable("pmux-test-claude"),
        }
    }
}

/// One daemon serving every cell in a run. Shared on purpose: a leak that only
/// a shared daemon can carry is the interesting one.
struct Daemon {
    child: Child,
    socket: PathBuf,
    stderr_path: PathBuf,
    stopped: bool,
}

impl Daemon {
    async fn start(binaries: &Binaries, sandbox: &Sandbox, lane: &LaneConfig) -> Self {
        let stderr_path = sandbox.root.join("pmuxd.stderr");
        let stderr = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&stderr_path)
            .expect("the daemon stderr file is fresh");
        let child = Command::new(&binaries.pmuxd)
            .arg("serve")
            .arg("--socket")
            .arg(&sandbox.socket)
            .arg("--rmuxd")
            .arg(&binaries.rmuxd)
            .arg("--launcher")
            .arg(&binaries.launcher)
            .arg("--runtime-parent")
            .arg(&sandbox.runtime_parent)
            .arg("--tested-claude-profile")
            .arg(lane.tested_profile())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(stderr))
            .spawn()
            .expect("the candidate daemon starts");
        let mut daemon = Self {
            child,
            socket: sandbox.socket.clone(),
            stderr_path,
            stopped: false,
        };
        let client = PmuxClient::new(&daemon.socket).expect("a client binds the candidate socket");
        for _ in 0..400 {
            if client.ping().await.is_ok() {
                return daemon;
            }
            if let Some(status) = daemon.child.try_wait().unwrap() {
                let diagnostics = std::fs::read_to_string(&daemon.stderr_path).unwrap_or_default();
                panic!("pmuxd exited during startup with {status}: {diagnostics}");
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("pmuxd did not bind its public socket");
    }

    async fn stop(&mut self) {
        if self.stopped {
            return;
        }
        self.signal_terminate();
        for _ in 0..600 {
            if self.child.try_wait().unwrap().is_some() {
                self.stopped = true;
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("the candidate daemon did not stop");
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

impl Drop for Daemon {
    /// A panicking assertion must not leave a daemon, a private rmux sidecar,
    /// or a Claude process behind. SIGTERM is the daemon's graceful shutdown,
    /// which closes every session it owns.
    fn drop(&mut self) {
        if self.stopped {
            return;
        }
        self.signal_terminate();
        for _ in 0..200 {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Owner-only scratch tree. Dropped -- and therefore removed -- even when an
/// assertion panics.
struct Sandbox {
    _temp: TempDir,
    root: PathBuf,
    socket: PathBuf,
    runtime_parent: PathBuf,
    state_root: PathBuf,
    home: PathBuf,
    path_first: PathBuf,
    path_last: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let temp = tempfile::Builder::new()
            .prefix("pmux-contamination-")
            .tempdir_in("/tmp")
            .expect("a scratch root is creatable");
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let root = temp.path().canonicalize().unwrap();
        let runtime_parent = root.join("private");
        let state_root = root.join("state");
        let home = root.join("home");
        let path_first = root.join("path-first");
        let path_last = root.join("path-last");
        for directory in [&runtime_parent, &state_root, &home, &path_first, &path_last] {
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
            path_first,
            path_last,
        }
    }

    /// A fresh EMPTY owner-only directory. pmux refuses a minified cell a root
    /// holding anything beyond its own two seed files, and never creates one.
    fn fresh_directory(&self, label: &str) -> PathBuf {
        let path = self.root.join(label);
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        path.canonicalize().unwrap()
    }
}

/// The launch environment `pmux-test-claude` refuses to start without.
///
/// The double is an attestation harness, not a mock: on every launch it proves
/// from inside the child that the credential names, the provider names and the
/// parent/terminal identity names were removed, that `unset` beat `set` in the
/// snapshot patch order, and that PATH is exactly what the caller declared it
/// should become. That contract is published as constants on `pseudomux_e2e`,
/// and this is the harness's own independent statement of it -- so a policy
/// regression fails the double lane here for the same reason it fails the
/// full-stack lane, rather than being inherited from it.
fn double_environment(sandbox: &Sandbox) -> EnvironmentSpec {
    let path = std::env::join_paths([sandbox.path_first.as_path(), sandbox.path_last.as_path()])
        .unwrap()
        .into_string()
        .unwrap();
    let mut snapshot = BTreeMap::from([
        ("HOME".to_owned(), string(&sandbox.home)),
        (
            "PMUX_TEST_STATE_DIR".to_owned(),
            string(&sandbox.state_root),
        ),
        (
            "PMUX_TEST_ENV_ATTESTATION".to_owned(),
            TEST_ENV_ATTESTATION_MARKER.to_owned(),
        ),
        (
            "PMUX_TEST_CALLER_SAFE_CONFIG".to_owned(),
            TEST_ENV_SAFE_CONFIG_VALUE.to_owned(),
        ),
        ("PMUX_TEST_EXPECTED_PATH".to_owned(), path.clone()),
        ("PATH".to_owned(), path),
        ("TERM".to_owned(), "ambient-terminal".to_owned()),
        (
            "PMUX_TEST_PATCH_ORDER".to_owned(),
            "snapshot-value".to_owned(),
        ),
        (
            "PMUX_TEST_UNSET_ME".to_owned(),
            "must-be-removed".to_owned(),
        ),
    ]);
    for key in TEST_SUBSCRIPTION_KEYS {
        let value = if key.starts_with("ANTHROPIC") {
            TEST_ANTHROPIC_SECRET
        } else {
            TEST_PROVIDER_SECRET
        };
        snapshot.insert((*key).to_owned(), value.to_owned());
    }
    for key in TEST_TRANSPARENT_EXACT_KEYS {
        snapshot.insert((*key).to_owned(), format!("ambient-{key}"));
    }
    EnvironmentSpec {
        snapshot,
        set: BTreeMap::from([
            (
                "PMUX_TEST_PATCH_ORDER".to_owned(),
                TEST_ENV_PATCHED_VALUE.to_owned(),
            ),
            (
                "PMUX_TEST_SET_ONLY".to_owned(),
                TEST_ENV_SET_ONLY_VALUE.to_owned(),
            ),
            (
                "ANTHROPIC_API_KEY".to_owned(),
                TEST_ANTHROPIC_SECRET.to_owned(),
            ),
        ]),
        unset: BTreeSet::from([
            "PMUX_TEST_PATCH_ORDER".to_owned(),
            "PMUX_TEST_UNSET_ME".to_owned(),
            "ANTHROPIC_API_KEY".to_owned(),
        ]),
    }
}

fn string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

// ---------------------------------------------------------------------------
// Cells
// ---------------------------------------------------------------------------

/// One cell and everything it is allowed to be the source of.
#[derive(Clone, Debug)]
struct Cell {
    index: usize,
    secret: String,
    config_root: PathBuf,
    cwd: PathBuf,
    session_id: SessionId,
    generation_id: SessionGenerationId,
    /// The transcript bound after the clear. Nothing at all may appear in it.
    bound_transcript: SessionId,
    /// Whether a `/clear` ran. Only a minified cell may be cleared, so the
    /// broken topology used by the discrimination proof reaches the sweep
    /// without one, and the post-clear emptiness rule must not be claimed of a
    /// transcript that was never rotated.
    cleared: bool,
    /// Every transcript id this cell has ever been bound to, launch included.
    owned_transcripts: BTreeSet<SessionId>,
    plant_answer: String,
    recall_answer: String,
}

impl Cell {
    /// The bytes that identify this cell to a sweep of somebody else's root.
    fn needles(&self) -> Vec<String> {
        let mut needles = vec![self.secret.clone(), self.session_id.to_string()];
        needles.extend(
            self.owned_transcripts
                .iter()
                .map(std::string::ToString::to_string),
        );
        needles.push(self.cwd.to_string_lossy().into_owned());
        needles.sort();
        needles.dedup();
        needles
    }
}

/// A secret no other process can guess and no corpus can contain, in a shape
/// that survives being typed into a TUI, echoed by a model, and stored in JSON.
fn fresh_secret(index: usize) -> String {
    format!(
        "PMUXLEAK{index}Z{}",
        Uuid::new_v4().simple().to_string().to_uppercase()
    )
}

// ---------------------------------------------------------------------------
// The harness body
// ---------------------------------------------------------------------------

async fn run_matrix(cells: usize, lane: Lane, topology: Topology) -> Report {
    assert!(cells >= 2, "contamination needs at least two cells");
    let binaries = Binaries::discover();
    let sandbox = Sandbox::new();
    let lane = match lane {
        Lane::Double => LaneConfig::double(&binaries, &sandbox),
        Lane::RealClaude => LaneConfig::real_claude(),
    };
    let mut report = Report::default();

    // The operator's real configuration is captured BEFORE the daemon exists,
    // so anything the run does to it is inside the observed window.
    let home_before = OperatorHome::capture();

    let mut daemon = Daemon::start(&binaries, &sandbox, &lane).await;
    let client = PmuxClient::new(&sandbox.socket).unwrap();

    // Shared topology deliberately hands every cell the same two directories.
    let shared_root = sandbox.fresh_directory("shared-config-root");
    let shared_cwd = sandbox.fresh_directory("shared-cwd");

    let mut prepared = Vec::new();
    for index in 0..cells {
        let (config_root, cwd) = match topology {
            // The labels carry a fresh uuid because a cell's cwd is one of the
            // needles swept for, and `cwd-1` is a substring of `cwd-10`: at
            // N=15 a positional name would make cell 1 a false positive in
            // cell 10's root.
            Topology::PerCell => (
                sandbox
                    .fresh_directory(&format!("config-root-{index}-{}", Uuid::new_v4().simple())),
                sandbox.fresh_directory(&format!("cwd-{index}-{}", Uuid::new_v4().simple())),
            ),
            Topology::SharedRoot => (shared_root.clone(), shared_cwd.clone()),
        };
        prepared.push((index, fresh_secret(index), config_root, cwd));
    }

    // Phase 1: every cell starts, plants its secret, and clears, CONCURRENTLY
    // against one daemon. Concurrency is not incidental: a leak that lives in
    // daemon-held state rather than on disk is only observable while two cells
    // are alive at once.
    let mut joined = tokio::task::JoinSet::new();
    for (index, secret, config_root, cwd) in prepared {
        let client = PmuxClient::new(&sandbox.socket).unwrap();
        let lane = lane.clone();
        joined.spawn(async move {
            plant(&client, &lane, topology, index, secret, config_root, cwd).await
        });
    }
    let mut cells = Vec::new();
    let mut failures = Vec::new();
    while let Some(result) = joined.join_next().await {
        match result.expect("no cell task panicked") {
            Ok(cell) => cells.push(cell),
            Err(error) => failures.push(error),
        }
    }
    if !failures.is_empty() {
        let _ = daemon.stop().await;
        panic!(
            "cells failed to reach the sweep: {failures:#?}\ndaemon diagnostics:\n{}",
            daemon.diagnostics()
        );
    }
    cells.sort_by_key(|cell| cell.index);

    // Phase 1b: the second door, while every cell is still LIVE. Only under the
    // shipped topology: the broken one deliberately shares a root between
    // ordinary cells, which is a shape an intruder is entitled to join.
    if topology == Topology::PerCell {
        let victim = cells.first().expect("at least two cells").clone();
        probe_the_second_door(&client, &lane, &sandbox, &victim, &mut report).await;
        probe_the_containment_door(&client, &lane, &sandbox, &victim, &mut report).await;
    } else {
        report.ensure_channel(SECOND_DOOR_CHANNEL);
        report.ensure_channel(CONTAINMENT_CHANNEL);
    }

    // Phase 2: every cleared cell is asked, in reverse order, what it recalls.
    // Reverse so the last cell to run is the first to be questioned; a cell
    // that answers from a neighbour's context answers here.
    for cell in cells.iter_mut().rev() {
        let result = run_one_turn(
            &client,
            &lane,
            cell.session_id,
            cell.generation_id,
            &lane.recall_prompt(),
        )
        .await
        .unwrap_or_else(|error| panic!("recall turn for cell {} failed: {error}", cell.index));
        cell.recall_answer = result.text;
    }
    report.real_turns = if lane.lane == Lane::RealClaude {
        cells.len() * 2
    } else {
        0
    };

    // Phase 3: close everything, so the sweep reads a settled filesystem.
    for cell in &cells {
        client
            .close_session(cell.session_id, cell.generation_id, ClosePolicy::Graceful)
            .await
            .unwrap_or_else(|error| panic!("close for cell {} failed: {error}", cell.index));
    }
    daemon.stop().await;

    // Phase 4: the sweep.
    sweep(&cells, &mut report);
    let home_after = OperatorHome::capture();
    home_before.compare(&home_after, &cells, &mut report);

    println!(
        "{}",
        report.render(&format!(
            "cross-cell contamination: {} cells, lane {:?}, topology {topology:?}",
            cells.len(),
            lane.lane
        ))
    );
    report
}

async fn plant(
    client: &PmuxClient,
    lane: &LaneConfig,
    topology: Topology,
    index: usize,
    secret: String,
    config_root: PathBuf,
    cwd: PathBuf,
) -> Result<Cell, String> {
    let session_id = Uuid::new_v4();
    let request = start_request(lane, topology, session_id, &config_root, &cwd);
    let handle = client
        .start_session(request)
        .await
        .map_err(|error| format!("cell {index} start failed: {error}"))?;

    let plant = run_one_turn(
        client,
        lane,
        handle.session_id,
        handle.generation_id,
        &lane.plant_prompt(&secret),
    )
    .await
    .map_err(|error| format!("cell {index} plant turn failed: {error}"))?;
    lane.assert_plant_answer(&secret, &plant.text);

    let mut owned_transcripts = BTreeSet::from([session_id]);
    let cleared = topology.clears();
    let bound_transcript = if cleared {
        let cleared = client
            .clear_session(session_id, handle.generation_id, session_id, None)
            .await
            .map_err(|error| format!("cell {index} clear failed: {error}"))?;
        assert!(cleared.rotated, "a clear that returns a result rotated");
        owned_transcripts.insert(cleared.transcript_session_id);
        cleared.transcript_session_id
    } else {
        session_id
    };

    Ok(Cell {
        index,
        secret,
        config_root,
        cwd,
        session_id,
        generation_id: handle.generation_id,
        bound_transcript,
        cleared,
        owned_transcripts,
        plant_answer: plant.text,
        recall_answer: String::new(),
    })
}

fn start_request(
    lane: &LaneConfig,
    topology: Topology,
    session_id: Uuid,
    config_root: &Path,
    cwd: &Path,
) -> StartSessionRequest {
    StartSessionRequest {
        identity: SessionIdentity::New {
            session_id: Some(session_id),
        },
        cwd: cwd.to_string_lossy().into_owned(),
        agent: None,
        claude: Some(ClaudeLaunchConfig {
            executable: lane.claude.to_string_lossy().into_owned(),
            model: lane.model.clone(),
            effort: lane.effort,
            permission_mode: None,
            allowed_tools: Vec::new(),
            denied_tools: vec!["*".to_owned()],
            settings: Vec::new(),
            mcp_configs: Vec::new(),
            plugin_dirs: Vec::new(),
            system_prompt: SystemPromptPolicy::Replace {
                prompt: "You are a bounded Path B cell. Answer briefly.".to_owned(),
            },
            extra_args: Vec::new(),
        }),
        environment: lane.environment.clone(),
        auth_policy: AuthPolicy::Subscription,
        config_isolation: Some(ConfigIsolation {
            root: config_root.to_string_lossy().into_owned(),
        }),
        terminal: TerminalSpec {
            rows: 24,
            cols: 120,
            profile: TerminalProfile::Transparent,
            input_transport: InputTransport::Sdk,
        },
        lifecycle: LifecycleMode::Transcript,
        retention: RetentionPolicy::Persistent {
            idle_ttl_ms: 1_800_000,
        },
        compatibility: CompatibilityPolicy::RequireTested,
        cell: topology.cell(),
    }
}

// ---------------------------------------------------------------------------
// The second door: a plain `environment.set["CLAUDE_CONFIG_DIR"]`
// ---------------------------------------------------------------------------

/// Every spelling of one directory a caller can put in a plain
/// `environment.set["CLAUDE_CONFIG_DIR"]`, with the fact that makes each row
/// dangerous.
///
/// `resolves_now` records whether `stat` can answer for the row TODAY. It is
/// the axis leak 5b turned on: the `..`-through-missing row is the only one the
/// kernel reports as `NotFound`, and a `NotFound` was being read as proof that
/// the path is not a directory a live cell holds. It is not: `mkdir -p` creates
/// the missing intermediate and then `..` resolves, so the row lands on the
/// live cell's root after all -- which is what Claude's own
/// `CLAUDE_CONFIG_DIR` bootstrap does.
struct Spelling {
    label: &'static str,
    value: String,
    resolves_now: bool,
}

fn plain_env_spellings(root: &Path) -> Vec<Spelling> {
    let name = root
        .file_name()
        .expect("a config root has a file name")
        .to_str()
        .expect("the fixture name is UTF-8")
        .to_owned();
    let parent = root.parent().expect("a config root has a parent");
    let text = |path: &Path| path.to_string_lossy().into_owned();
    let mut rows = vec![
        Spelling {
            label: "identity",
            value: text(root),
            resolves_now: true,
        },
        Spelling {
            label: "trailing slash",
            value: format!("{}/", root.display()),
            resolves_now: true,
        },
        Spelling {
            label: "doubled separator",
            value: format!("{}//{name}", parent.display()),
            resolves_now: true,
        },
        Spelling {
            label: "dot component",
            value: text(&parent.join(".").join(&name)),
            resolves_now: true,
        },
        Spelling {
            label: "dot-dot through an existing directory",
            value: text(&root.join("..").join(&name)),
            resolves_now: true,
        },
        Spelling {
            // LEAK 5b. `NOPE` does not exist, so the kernel resolves
            // left-to-right and answers `NotFound` for a path that lexically
            // names the live root -- and that a `mkdir -p` completes onto it.
            label: "dot-dot through a MISSING directory",
            value: text(
                &parent
                    .join(format!("NOPE-{}", Uuid::new_v4().simple()))
                    .join("..")
                    .join(&name),
            ),
            resolves_now: false,
        },
    ];
    #[cfg(target_os = "macos")]
    {
        // LEAK 5. The APFS firmlink namespace: not a symlink, so
        // `Path::canonicalize` returns it unchanged.
        let canonical = root.canonicalize().expect("the live root resolves");
        let firmlink = Path::new("/System/Volumes/Data").join(
            canonical
                .strip_prefix("/")
                .expect("a canonical path is absolute"),
        );
        if firmlink.is_dir() {
            rows.push(Spelling {
                label: "firmlink alias",
                value: text(&firmlink),
                resolves_now: true,
            });
        }
    }
    rows
}

/// One intruder start per spelling, against a LIVE minified cell's root.
///
/// The request shape is the one `config_isolation` does not go through and the
/// harness never used to construct: an ORDINARY cell, no `config_isolation` at
/// all, and the victim's configuration root handed over in a plain
/// `environment.set["CLAUDE_CONFIG_DIR"]`. Every row must be REFUSED. A row
/// that is admitted is recorded as a violation and its session is closed, so
/// the sweep that follows still runs on a settled filesystem -- and so the
/// physical contamination the admitted child wrote is visible in the victim's
/// own root as well as here.
async fn probe_the_second_door(
    client: &PmuxClient,
    lane: &LaneConfig,
    sandbox: &Sandbox,
    victim: &Cell,
    report: &mut Report,
) {
    report.ensure_channel(SECOND_DOOR_CHANNEL);
    for spelling in plain_env_spellings(&victim.config_root) {
        assert_eq!(
            std::fs::metadata(&spelling.value).is_ok(),
            spelling.resolves_now,
            "{}: the fixture must actually have the resolvability it claims ({})",
            spelling.label,
            spelling.value
        );
        let mut environment = lane.environment.clone();
        environment
            .set
            .insert("CLAUDE_CONFIG_DIR".to_owned(), spelling.value.clone());
        environment.unset.remove("CLAUDE_CONFIG_DIR");
        let intruder_cwd =
            sandbox.fresh_directory(&format!("intruder-cwd-{}", Uuid::new_v4().simple()));
        let mut request = start_request(
            lane,
            Topology::PerCell,
            Uuid::new_v4(),
            &victim.config_root,
            &intruder_cwd,
        );
        // The intruder is an ORDINARY caller with no isolation block: the one
        // shape that reaches `effective_config_root` carrying a spelling
        // nothing canonicalized.
        request.cell = SessionCell::Full;
        request.config_isolation = None;
        request.environment = environment;

        report.observe(SECOND_DOOR_CHANNEL, spelling.value.len() as u64);
        match client.start_session(request).await {
            Err(ClientError::Server(body)) if body.code == ErrorCode::InvalidConfig => {
                report.notes.push(format!(
                    "second door REFUSED [{}] {} -> {}",
                    spelling.label, spelling.value, body.message
                ));
            }
            Err(other) => {
                report.violate(
                    victim.index,
                    SECOND_DOOR_CHANNEL,
                    spelling.value.clone(),
                    format!(
                        "{}: refused, but not by the admission rule; a start that fails for an \
                         unrelated reason proves nothing about the guard: {other}",
                        spelling.label
                    ),
                );
            }
            Ok(handle) => {
                report.violate(
                    victim.index,
                    SECOND_DOOR_CHANNEL,
                    spelling.value.clone(),
                    format!(
                        "{}: ADMITTED against the live minified cell holding {}",
                        spelling.label,
                        victim.config_root.display()
                    ),
                );
                // One turn, so the admission becomes the on-disk fact it really
                // is: the intruder's child writes its own transcript INSIDE the
                // victim's root, where the transcript inventory catches it by
                // name as a second, independent violation. An admission the
                // sweep cannot also see would be a finding this harness had to
                // be told about.
                let _ = run_one_turn(
                    client,
                    lane,
                    handle.session_id,
                    handle.generation_id,
                    &lane.plant_prompt(&fresh_secret(usize::MAX)),
                )
                .await;
                let _ = client
                    .close_session(
                        handle.session_id,
                        handle.generation_id,
                        ClosePolicy::Graceful,
                    )
                    .await;
            }
        }
    }

    // The same door as seen from PATH B's own side. Every row above is an
    // ORDINARY caller, which is the shape that reached the victim; this one is
    // a MINIFIED applicant that brings a perfectly good `config_isolation`
    // block of its own AND an `environment.set["CLAUDE_CONFIG_DIR"]`. It is
    // refused on the CELL, before any directory question is asked, so the
    // spelling in the value never gets a chance to matter.
    let own_root = sandbox.fresh_directory(&format!("second-door-{}", Uuid::new_v4().simple()));
    let own_cwd = sandbox.fresh_directory(&format!("second-door-cwd-{}", Uuid::new_v4().simple()));
    let mut request = start_request(lane, Topology::PerCell, Uuid::new_v4(), &own_root, &own_cwd);
    request
        .environment
        .set
        .insert("CLAUDE_CONFIG_DIR".to_owned(), string(&victim.config_root));
    request.environment.unset.remove("CLAUDE_CONFIG_DIR");
    report.observe(SECOND_DOOR_CHANNEL, 0);
    match client.start_session(request).await {
        Err(ClientError::Server(body)) if body.code == ErrorCode::InvalidConfig => {
            report.notes.push(format!(
                "second door REFUSED [minified cell setting CLAUDE_CONFIG_DIR] -> {}",
                body.message
            ));
        }
        other => report.violate(
            victim.index,
            SECOND_DOOR_CHANNEL,
            string(&victim.config_root),
            format!(
                "a minified cell reached the environment door: {}",
                match other {
                    Ok(_) => "ADMITTED".to_owned(),
                    Err(error) => format!("refused for an unrelated reason: {error}"),
                }
            ),
        ),
    }
}

// ---------------------------------------------------------------------------
// The containment door: a RELATION to a live cell's directories, not a
// spelling of one
// ---------------------------------------------------------------------------

/// Which door of the request one containment shape arrives through.
///
/// Recorded so [`every_containment_relation_really_overlaps_a_directory_the_victim_binds`]
/// can assert that all four are represented. A table that probed the same door
/// eight times would look identical in the report and would prove one eighth
/// as much.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Door {
    /// A plain `environment.set["CLAUDE_CONFIG_DIR"]` on an ORDINARY cell with
    /// no `config_isolation` block: the one spelling of a configuration root
    /// that nothing in pmux canonicalizes.
    PlainEnvConfigRoot,
    /// The request's own `cwd`, which the pre-leak-7 guard compared only
    /// against other sessions' cwds -- so a cwd standing on a live cell's
    /// CONFIGURATION ROOT matched nothing, however many spellings it knew.
    WorkingDirectory,
    /// A `config_isolation` root on a MINIFIED applicant. Path B's own door,
    /// canonicalized, owner-checked, shadow-checked and pristine-checked, and
    /// still inside the victim.
    MinifiedIsolationRoot,
    /// `HOME`, with no `CLAUDE_CONFIG_DIR` anywhere in the request. The
    /// directory the child is handed is `<HOME>/.claude`, which nothing in the
    /// request mentions at all.
    HomeDerivedRoot,
}

/// One relation an intruder can stand in to a live minified cell's directories.
///
/// `binds` and `touches` are stated rather than derived, so the report and the
/// default-suite assertions are about the RELATION -- the thing under test --
/// instead of restating the request.
struct ContainmentRelation {
    label: &'static str,
    door: Door,
    /// The directory this start would really be launched against.
    binds: PathBuf,
    /// The live minified cell's own directory that `binds` overlaps.
    touches: PathBuf,
}

/// The ten relations, every one of them MEASURED as ADMITTED against a live
/// minified cell over the real socket before the rule that refuses it existed:
/// eight before the containment rule (leak 7) and two more before that rule
/// was made to walk what the child REACHES rather than what the caller SPELLED
/// (leak 8).
///
/// Split out of [`containment_shapes`] and kept free of `LaneConfig`,
/// `Sandbox` and `Cell` on purpose: every live lane in this file is
/// `#[ignore]`d, so the only thing `cargo test` can watch is the TABLE, and it
/// can only watch a table it can build without a daemon. This is the same
/// division [`plain_env_spellings`] is under.
///
/// Three rows bind a directory that DOES NOT EXIST -- the absent subdirectory,
/// the `HOME`-derived `<cell root>/.claude`, and the `.claude` under a `HOME`
/// that is a symlink. Those are the rows an identity test cannot see even in
/// principle: there is no inode to compare, and pmux or the child creates the
/// directory afterwards, inside the victim.
///
/// Two rows are LEXICALLY DISJOINT from everything the victim binds and reach
/// inside it anyway, through a symlink. They are leak 8, and they are not
/// spellings of a live cell's directory in the leak-5 sense: nothing resolves
/// them to the victim's ROOT, they resolve to a strict DESCENDANT of it, which
/// is the direction that writes inside the cell.
fn containment_relations(
    victim_root: &Path,
    victim_cwd: &Path,
    ancestor: &Path,
) -> (Vec<ContainmentRelation>, Vec<PathBuf>) {
    // Directories and symlinks the probe creates so a row can stand on them.
    // Returned so the caller can put the victim back the way it found it when
    // the guard holds and nothing was written.
    let mut created = Vec::new();

    let intruder_private_root =
        victim_root.join(format!("intruder-root-{}", Uuid::new_v4().simple()));
    std::fs::create_dir(&intruder_private_root).expect("the probe may create its own root");
    std::fs::set_permissions(
        &intruder_private_root,
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    created.push(intruder_private_root.clone());
    let inside_victim_cwd = victim_cwd.join(format!("intruder-cwd-{}", Uuid::new_v4().simple()));
    std::fs::create_dir(&inside_victim_cwd).expect("the probe may create a cwd");
    created.push(inside_victim_cwd.clone());

    let projects = victim_root.join("projects");

    // LEAK 8's two fixtures. Both are symlinks OUTSIDE everything the victim
    // binds, pointing at a strict DESCENDANT of the victim's configuration
    // root. The target is deliberately not the root itself: a link to the root
    // is caught by the identity arm of the walk (the walk's first element is
    // the path, so `stat` resolves it), and the miss is precisely the link to
    // something UNDER a claimed directory.
    //
    // The first is the link as the FINAL component of the path the request
    // carries; the second has the link in the MIDDLE, because
    // `effective_config_root` appends `.claude` to whatever `HOME` says. A
    // lexical ancestor walk sees `<link>` and then `<link>`'s spelling's
    // parents, and never `projects/`'s real parent -- which is the live cell's
    // root.
    let final_component_link = ancestor.join(format!("leak8-final-{}", Uuid::new_v4().simple()));
    std::os::unix::fs::symlink(&projects, &final_component_link)
        .expect("the probe may create a symlink");
    created.push(final_component_link.clone());
    let home_link = ancestor.join(format!("leak8-home-{}", Uuid::new_v4().simple()));
    std::os::unix::fs::symlink(&projects, &home_link).expect("the probe may create a symlink");
    created.push(home_link.clone());

    let mut rows = vec![
        ContainmentRelation {
            label: "config root NESTED in the live cell root (absent subdir)",
            door: Door::PlainEnvConfigRoot,
            binds: victim_root.join(format!("absent-{}", Uuid::new_v4().simple())),
            touches: victim_root.to_path_buf(),
        },
        ContainmentRelation {
            label: "config root NESTED in the live cell root (existing projects/)",
            door: Door::PlainEnvConfigRoot,
            binds: projects.clone(),
            touches: victim_root.to_path_buf(),
        },
        ContainmentRelation {
            label: "config root is an ANCESTOR of the live cell root",
            door: Door::PlainEnvConfigRoot,
            binds: ancestor.to_path_buf(),
            touches: victim_root.to_path_buf(),
        },
        ContainmentRelation {
            label: "HOME redirected so the effective root lands in the live cell root",
            door: Door::HomeDerivedRoot,
            binds: victim_root.join(".claude"),
            touches: victim_root.to_path_buf(),
        },
        ContainmentRelation {
            label: "cwd IS the live cell's configuration root",
            door: Door::WorkingDirectory,
            binds: victim_root.to_path_buf(),
            touches: victim_root.to_path_buf(),
        },
        ContainmentRelation {
            label: "cwd INSIDE the live cell's configuration root (projects/)",
            door: Door::WorkingDirectory,
            binds: projects,
            touches: victim_root.to_path_buf(),
        },
        ContainmentRelation {
            label: "cwd INSIDE the live cell's workspace",
            door: Door::WorkingDirectory,
            binds: inside_victim_cwd,
            touches: victim_cwd.to_path_buf(),
        },
        ContainmentRelation {
            label: "minified isolation root NESTED in the live cell root",
            door: Door::MinifiedIsolationRoot,
            binds: intruder_private_root,
            touches: victim_root.to_path_buf(),
        },
        ContainmentRelation {
            label: "config root is a SYMLINK to a directory inside the live cell root",
            door: Door::PlainEnvConfigRoot,
            binds: final_component_link,
            touches: victim_root.to_path_buf(),
        },
        ContainmentRelation {
            label: "HOME is a SYMLINK to a directory inside the live cell root",
            door: Door::HomeDerivedRoot,
            binds: home_link.join(".claude"),
            touches: victim_root.to_path_buf(),
        },
    ];
    rows.sort_by_key(|row| row.label);
    (rows, created)
}

/// One start that binds a directory OVERLAPPING one a live minified cell binds,
/// without naming any of them.
struct ContainmentShape {
    label: &'static str,
    /// The directory this start would really be launched against.
    binds: String,
    /// The live minified cell's own directory that `binds` overlaps.
    touches: String,
    request: StartSessionRequest,
}

/// Each relation, turned into the request that delivers it through its door.
fn containment_shapes(
    lane: &LaneConfig,
    sandbox: &Sandbox,
    victim: &Cell,
) -> (Vec<ContainmentShape>, Vec<PathBuf>) {
    let fresh =
        |label: &str| sandbox.fresh_directory(&format!("{label}-{}", Uuid::new_v4().simple()));
    let (relations, created) =
        containment_relations(&victim.config_root, &victim.cwd, &sandbox.root);
    let shapes = relations
        .into_iter()
        .map(|relation| {
            let request = match relation.door {
                Door::PlainEnvConfigRoot => {
                    let mut request = start_request(
                        lane,
                        Topology::PerCell,
                        Uuid::new_v4(),
                        &relation.binds,
                        &fresh("intruder-cwd"),
                    );
                    request.cell = SessionCell::Full;
                    request.config_isolation = None;
                    request
                        .environment
                        .set
                        .insert("CLAUDE_CONFIG_DIR".to_owned(), string(&relation.binds));
                    request.environment.unset.remove("CLAUDE_CONFIG_DIR");
                    request
                }
                Door::WorkingDirectory => {
                    let mut request = start_request(
                        lane,
                        Topology::PerCell,
                        Uuid::new_v4(),
                        &fresh("intruder-root"),
                        &relation.binds,
                    );
                    request.cell = SessionCell::Full;
                    request
                }
                Door::MinifiedIsolationRoot => {
                    let mut request = start_request(
                        lane,
                        Topology::PerCell,
                        Uuid::new_v4(),
                        &relation.binds,
                        &fresh("intruder-cwd"),
                    );
                    request.cell = SessionCell::Minified;
                    request
                }
                Door::HomeDerivedRoot => {
                    // `binds` is `<HOME>/.claude`, so the value the request
                    // actually carries is its parent -- and the request carries
                    // nothing else that names a configuration root.
                    let home = relation
                        .binds
                        .parent()
                        .expect("a HOME-derived root has a parent");
                    let mut request = start_request(
                        lane,
                        Topology::PerCell,
                        Uuid::new_v4(),
                        &fresh("intruder-root"),
                        &fresh("intruder-cwd"),
                    );
                    request.cell = SessionCell::Full;
                    request.config_isolation = None;
                    request.environment.set.remove("CLAUDE_CONFIG_DIR");
                    request.environment.snapshot.remove("CLAUDE_CONFIG_DIR");
                    request
                        .environment
                        .set
                        .insert("HOME".to_owned(), string(home));
                    request
                }
            };
            ContainmentShape {
                label: relation.label,
                binds: string(&relation.binds),
                touches: string(&relation.touches),
                request,
            }
        })
        .collect();
    (shapes, created)
}

/// One intruder start per RELATION, against a LIVE minified cell.
///
/// Every row must be REFUSED with `InvalidConfig`. A row that is admitted is
/// recorded as a violation and given one turn before it is closed, so the
/// physical contamination it wrote shows up in the victim's own root as a
/// second, independent finding rather than only here.
async fn probe_the_containment_door(
    client: &PmuxClient,
    lane: &LaneConfig,
    sandbox: &Sandbox,
    victim: &Cell,
    report: &mut Report,
) {
    report.ensure_channel(CONTAINMENT_CHANNEL);
    let (shapes, created) = containment_shapes(lane, sandbox, victim);
    for shape in shapes {
        report.observe(CONTAINMENT_CHANNEL, shape.binds.len() as u64);
        match client.start_session(shape.request).await {
            Err(ClientError::Server(body)) if body.code == ErrorCode::InvalidConfig => {
                report.notes.push(format!(
                    "containment REFUSED [{}] binds {} which touches {} -> {}",
                    shape.label, shape.binds, shape.touches, body.message
                ));
            }
            Err(other) => report.violate(
                victim.index,
                CONTAINMENT_CHANNEL,
                shape.binds.clone(),
                format!(
                    "{}: refused, but not by the admission rule; a start that fails for an \
                     unrelated reason proves nothing about the guard: {other}",
                    shape.label
                ),
            ),
            Ok(handle) => {
                report.violate(
                    victim.index,
                    CONTAINMENT_CHANNEL,
                    shape.binds.clone(),
                    format!(
                        "{}: ADMITTED -- it binds {} and the live minified cell binds {}",
                        shape.label, shape.binds, shape.touches
                    ),
                );
                let _ = run_one_turn(
                    client,
                    lane,
                    handle.session_id,
                    handle.generation_id,
                    &lane.plant_prompt(&fresh_secret(usize::MAX - 1)),
                )
                .await;
                let _ = client
                    .close_session(
                        handle.session_id,
                        handle.generation_id,
                        ClosePolicy::Graceful,
                    )
                    .await;
            }
        }
    }
    // Left exactly as found when the guard holds: a directory the probe created
    // and nothing wrote into is removed, and one an admitted intruder filled is
    // kept so the sweep can see it.
    for artifact in created {
        remove_probe_artifact(&artifact);
    }
}

/// Undoes one fixture [`containment_relations`] created.
///
/// Split out because the table now creates SYMLINKS as well as directories, and
/// `remove_dir` on a symlink to a directory fails on both platforms pmux
/// targets -- silently, since the caller discards the error. A leak-8 fixture
/// left behind would be a symlink from the sandbox into a live cell's root, and
/// the sweep would then report it as a channel of its own.
///
/// Never follows the link: `symlink_metadata`, and `remove_file` on a symlink
/// removes the LINK. A `remove_dir_all` here could empty a live cell's
/// `projects/`.
fn remove_probe_artifact(path: &Path) {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return;
    };
    let _ = if metadata.file_type().is_symlink() {
        std::fs::remove_file(path)
    } else {
        std::fs::remove_dir(path)
    };
}

async fn run_one_turn(
    client: &PmuxClient,
    lane: &LaneConfig,
    session_id: SessionId,
    generation_id: SessionGenerationId,
    prompt: &str,
) -> Result<TurnResult, String> {
    let turn_id = Uuid::new_v4();
    let request = TurnRequest {
        turn_id,
        prompt: prompt.to_owned(),
        deadline_unix_ms: Some(now_ms() + lane.turn_deadline_ms),
        lease: TurnLeasePolicy {
            on_disconnect: DisconnectAction::Continue,
            heartbeat_timeout_ms: None,
        },
    };
    let deadline = tokio::time::Instant::now()
        + Duration::from_millis(lane.turn_deadline_ms + lane.observer_grace_ms);

    let accepted = loop {
        match client
            .run_turn(session_id, generation_id, request.clone())
            .await
        {
            Ok(accepted) => break accepted,
            Err(ClientError::Server(body))
                if body.code == ErrorCode::SessionBusy && body.retryable =>
            {
                if tokio::time::Instant::now() >= deadline {
                    return Err(format!("turn {turn_id} stayed busy to its deadline"));
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) => return Err(format!("turn {turn_id} submission failed: {error}")),
        }
    };

    let mut after = accepted.next_sequence.saturating_sub(1);
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("turn {turn_id} published no terminal result"));
        }
        let batch = client
            .subscribe_events(SubscribeEventsRequest {
                session_id,
                generation_id,
                after_sequence: after,
                wait_ms: 1_000,
                max_events: 128,
            })
            .await
            .map_err(|error| format!("turn {turn_id} subscription failed: {error}"))?;
        for event in batch.events {
            after = event.sequence;
            match event.event {
                EventPayload::TurnCompleted(result) if result.turn_id == turn_id => {
                    if result.outcome != TurnOutcome::Completed {
                        return Err(format!("turn {turn_id} outcome {:?}", result.outcome));
                    }
                    return Ok(*result);
                }
                EventPayload::TurnFailed(error) if event.turn_id == Some(turn_id) => {
                    return Err(format!("turn {turn_id} failed: {error:?}"));
                }
                _ => {}
            }
        }
        tokio::task::yield_now().await;
    }
}

fn now_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

// ---------------------------------------------------------------------------
// The sweep
// ---------------------------------------------------------------------------

fn sweep(cells: &[Cell], report: &mut Report) {
    for channel in ROOT_CHANNELS {
        report.ensure_channel(channel.name);
    }
    for channel in [
        UNCLASSIFIED_CHANNEL,
        MODEL_ANSWER_CHANNEL,
        OPERATOR_HOME_CHANNEL,
        TRANSCRIPT_INVENTORY_CHANNEL,
        BOUND_TRANSCRIPT_CHANNEL,
    ] {
        report.ensure_channel(channel);
    }

    for cell in cells {
        let foreign: Vec<(usize, String)> = cells
            .iter()
            .filter(|other| other.index != cell.index)
            .flat_map(|other| {
                other
                    .needles()
                    .into_iter()
                    .map(move |needle| (other.index, needle))
            })
            .collect();

        // Every byte reachable from this cell's own configuration root,
        // attributed to a named channel and searched for every other cell's
        // needles. The walk does not follow symlinks: a link OUT of the root is
        // itself a channel, and is reported as one.
        for entry in walk(&cell.config_root) {
            let channel = channel_for(&entry.relative);
            if channel == UNCLASSIFIED_CHANNEL {
                report
                    .unclassified
                    .insert(entry.relative.display().to_string());
            }
            report.observe(channel, entry.bytes.len() as u64);
            if let Some(target) = &entry.symlink_target {
                report.violate(
                    cell.index,
                    channel_for(&entry.relative),
                    entry.relative.display().to_string(),
                    format!(
                        "configuration root contains a symlink to {}",
                        target.display()
                    ),
                );
            }
            let haystack = String::from_utf8_lossy(&entry.bytes);
            if haystack.contains(cell.secret.as_str()) {
                report.control(cell.index, channel_for(&entry.relative));
            }
            for (owner, needle) in &foreign {
                if haystack.contains(needle.as_str())
                    || entry.relative.to_string_lossy().contains(needle.as_str())
                {
                    report.violate(
                        cell.index,
                        channel_for(&entry.relative),
                        entry.relative.display().to_string(),
                        format!("holds cell {owner}'s bytes ({needle})"),
                    );
                }
            }
        }

        // The transcript inventory, by NAME rather than by content: a foreign
        // session file that happened to be empty would pass a content scan.
        let projects = cell.config_root.join("projects");
        for entry in walk(&projects) {
            let Some(name) = entry.relative.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(stem) = name.strip_suffix(".jsonl") else {
                continue;
            };
            report.observe(TRANSCRIPT_INVENTORY_CHANNEL, 0);
            let Ok(transcript) = stem.parse::<Uuid>() else {
                continue;
            };
            if !cell.owned_transcripts.contains(&transcript) {
                report.violate(
                    cell.index,
                    TRANSCRIPT_INVENTORY_CHANNEL,
                    entry.relative.display().to_string(),
                    format!("root holds transcript {transcript}, which this cell never bound"),
                );
            }
        }

        // The bound transcript is held to a stricter rule than every other
        // file: after a clear it must carry NOTHING, not even this cell's own
        // secret. That is the statelessness claim stated as a file.
        let bound = cell
            .cleared
            .then(|| find_transcript(&cell.config_root, cell.bound_transcript))
            .flatten();
        match bound {
            Some((locator, bytes)) => {
                report.observe(BOUND_TRANSCRIPT_CHANNEL, bytes.len() as u64);
                let haystack = String::from_utf8_lossy(&bytes);
                for other in cells {
                    if haystack.contains(other.secret.as_str()) {
                        report.violate(
                            cell.index,
                            BOUND_TRANSCRIPT_CHANNEL,
                            locator.clone(),
                            format!(
                                "the transcript bound after /clear still holds cell {}'s secret",
                                other.index
                            ),
                        );
                    }
                }
            }
            None => report.notes.push(format!(
                "cell {} published no post-clear bound transcript for {} (no clear ran, or the file is absent)",
                cell.index, cell.bound_transcript
            )),
        }

        // The model's own answer. The double cannot make this assertion mean
        // anything; the real lane can, and it is the only channel that reads
        // out of the model rather than off the disk.
        report.observe(MODEL_ANSWER_CHANNEL, cell.recall_answer.len() as u64);
        for other in cells {
            if cell.recall_answer.contains(other.secret.as_str()) {
                report.violate(
                    cell.index,
                    MODEL_ANSWER_CHANNEL,
                    format!("recall answer of cell {}", cell.index),
                    format!(
                        "a cleared cell recalled cell {}'s secret: {:?}",
                        other.index, cell.recall_answer
                    ),
                );
            }
        }
        assert!(
            !cell.plant_answer.is_empty(),
            "every cell answered its plant turn"
        );
    }
}

fn channel_for(relative: &Path) -> &'static str {
    ROOT_CHANNELS
        .iter()
        .find(|channel| {
            let candidate = Path::new(channel.relative);
            relative == candidate || relative.starts_with(candidate)
        })
        .map_or(UNCLASSIFIED_CHANNEL, |channel| channel.name)
}

fn find_transcript(root: &Path, transcript: SessionId) -> Option<(String, Vec<u8>)> {
    let name = format!("{transcript}.jsonl");
    walk(&root.join("projects"))
        .into_iter()
        .find(|entry| entry.relative.file_name().and_then(|value| value.to_str()) == Some(&name))
        .map(|entry| (entry.relative.display().to_string(), entry.bytes))
}

struct Entry {
    relative: PathBuf,
    bytes: Vec<u8>,
    symlink_target: Option<PathBuf>,
}

/// Every regular file and symlink below `root`, relative to it, without
/// following a single link.
fn walk(root: &Path) -> Vec<Entry> {
    let mut entries = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let Ok(listing) = std::fs::read_dir(&directory) else {
            continue;
        };
        for item in listing.flatten() {
            let path = item.path();
            let relative = path
                .strip_prefix(root)
                .expect("walk stays under its root")
                .to_path_buf();
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.is_symlink() {
                let target = std::fs::read_link(&path).unwrap_or_default();
                entries.push(Entry {
                    relative,
                    bytes: target.as_os_str().as_encoded_bytes().to_vec(),
                    symlink_target: Some(target),
                });
            } else if metadata.is_dir() {
                stack.push(path);
            } else if metadata.is_file() {
                entries.push(Entry {
                    relative,
                    bytes: std::fs::read(&path).unwrap_or_default(),
                    symlink_target: None,
                });
            }
        }
    }
    entries
}

// ---------------------------------------------------------------------------
// The operator's own configuration
// ---------------------------------------------------------------------------

/// `~/.claude` and `~/.claude.json` as content digests.
///
/// The requirement is byte-identity across the run. That is asserted, with one
/// named escape hatch: when this harness is itself run from inside a live
/// Claude Code session, that ambient session writes its own transcript into
/// `~/.claude` while the run is in flight, so unconditional byte-identity is
/// unachievable there and setting
/// `PMUX_CONTAMINATION_ALLOW_AMBIENT_HOME_CHURN=1` downgrades the equality to a
/// reported note. The leak assertions -- no secret, no cwd, no session id, no
/// new project directory of ours -- are NEVER downgraded.
struct OperatorHome {
    home: PathBuf,
    digests: BTreeMap<PathBuf, [u8; 32]>,
}

impl OperatorHome {
    fn capture() -> Self {
        let home = PathBuf::from(std::env::var_os("HOME").expect("HOME identifies the operator"));
        let mut digests = BTreeMap::new();
        for base in [home.join(".claude"), home.join(".claude.json")] {
            if base.is_file() {
                digests.insert(PathBuf::from(base.file_name().unwrap()), digest(&base));
            } else {
                for entry in walk(&base) {
                    let mut hasher = Sha256::new();
                    hasher.update(&entry.bytes);
                    digests.insert(
                        Path::new(base.file_name().unwrap()).join(&entry.relative),
                        hasher.finalize().into(),
                    );
                }
            }
        }
        Self { home, digests }
    }

    fn compare(&self, after: &Self, cells: &[Cell], report: &mut Report) {
        let mut changed = Vec::new();
        for (path, digest) in &after.digests {
            if self.digests.get(path) != Some(digest) {
                changed.push(path.clone());
            }
        }
        for path in self.digests.keys() {
            if !after.digests.contains_key(path) {
                changed.push(path.clone());
            }
        }
        changed.sort();
        changed.dedup();
        for digest in after.digests.values() {
            report.observe(OPERATOR_HOME_CHANNEL, digest.len() as u64);
        }

        // The leak assertion, never downgraded: no needle of any cell may
        // appear anywhere in the operator's own configuration.
        for base in [self.home.join(".claude"), self.home.join(".claude.json")] {
            let files: Vec<(String, Vec<u8>)> = if base.is_file() {
                vec![(
                    base.display().to_string(),
                    std::fs::read(&base).unwrap_or_default(),
                )]
            } else {
                walk(&base)
                    .into_iter()
                    .map(|entry| {
                        (
                            base.join(&entry.relative).display().to_string(),
                            entry.bytes,
                        )
                    })
                    .collect()
            };
            for (locator, bytes) in files {
                let haystack = String::from_utf8_lossy(&bytes);
                for cell in cells {
                    for needle in cell.needles() {
                        if haystack.contains(needle.as_str()) || locator.contains(needle.as_str()) {
                            report.violate(
                                cell.index,
                                OPERATOR_HOME_CHANNEL,
                                locator.clone(),
                                format!("the operator's own configuration holds {needle}"),
                            );
                        }
                    }
                }
            }
        }

        if changed.is_empty() {
            return;
        }
        let summary = format!(
            "the operator's configuration changed during the run: {}",
            changed
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        if std::env::var_os("PMUX_CONTAMINATION_ALLOW_AMBIENT_HOME_CHURN").is_some() {
            report.notes.push(format!("{summary} (ambient churn allowed by PMUX_CONTAMINATION_ALLOW_AMBIENT_HOME_CHURN; leak assertions still applied)"));
        } else {
            report.violate(
                usize::MAX,
                OPERATOR_HOME_CHANNEL,
                self.home.display().to_string(),
                summary,
            );
        }
    }
}

fn digest(path: &Path) -> [u8; 32] {
    let mut hasher = Sha256::new();
    let mut file = std::fs::File::open(path).expect("a captured file is readable");
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).unwrap_or(0);
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    hasher.finalize().into()
}

// ---------------------------------------------------------------------------
// Lane (a): deterministic, free, CI-able
// ---------------------------------------------------------------------------

fn assert_clean(report: &Report, cells: usize, lane: Lane) {
    assert!(
        report.violations.is_empty(),
        "cross-cell contamination detected across {cells} cells:\n{}",
        report.render("failing sweep")
    );
    // A sweep that scanned nothing proves nothing.
    let projects = report
        .channels
        .get("projects")
        .expect("the projects channel is always in the table");
    assert!(
        projects.entries >= cells,
        "the sweep must have read at least one transcript per cell: {projects:?}"
    );
    assert!(
        report
            .channels
            .get(BOUND_TRANSCRIPT_CHANNEL)
            .is_some_and(|observation| observation.entries == cells),
        "every cell's post-clear bound transcript must have been read"
    );
    // The second door is asserted by COUNT, not just by "no violations": a
    // probe that stopped constructing the offending request shape would report
    // exactly the same clean sweep as one that constructed it and was refused,
    // which is the failure mode that let leak 5 and leak 5b through this file.
    assert!(
        report
            .channels
            .get(SECOND_DOOR_CHANNEL)
            .is_some_and(|observation| observation.entries >= SECOND_DOOR_MINIMUM_SPELLINGS),
        "the plain-env second door must have been probed with at least \
         {SECOND_DOOR_MINIMUM_SPELLINGS} spellings of a live cell's root: {:?}",
        report.channels.get(SECOND_DOOR_CHANNEL)
    );
    // Same rule, the other axis. The spellings above vary one directory; these
    // vary the RELATION an intruder stands in to it.
    assert!(
        report
            .channels
            .get(CONTAINMENT_CHANNEL)
            .is_some_and(|observation| observation.entries >= CONTAINMENT_MINIMUM_SHAPES),
        "the containment door must have been probed with at least \
         {CONTAINMENT_MINIMUM_SHAPES} relations to a live cell's directories: {:?}",
        report.channels.get(CONTAINMENT_CHANNEL)
    );

    // The channels whose sweep is only meaningful once the needle has been
    // shown to be findable in them. `history` is real-lane only because the
    // double does not write one -- which is precisely why a double-only
    // history assertion would be worthless.
    let required: &[&str] = match lane {
        Lane::Double => &["projects"],
        Lane::RealClaude => &["projects", "history"],
    };
    for index in 0..cells {
        let observed = report.positive_controls.get(&index);
        for channel in required {
            assert!(
                observed.is_some_and(|channels| channels.contains(*channel)),
                "cell {index}: channel {channel} carries no positive control, so a clean sweep of \
                 it proves nothing (observed: {observed:?})"
            );
        }
    }
}

async fn double_lane(cells: usize) {
    let report = run_matrix(cells, Lane::Double, Topology::PerCell).await;
    assert_clean(&report, cells, Lane::Double);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "launches candidate binaries, a private rmux runtime, and credential-free Claude doubles"]
async fn two_minified_cells_share_nothing_through_any_channel() {
    double_lane(2).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "launches candidate binaries, a private rmux runtime, and credential-free Claude doubles"]
async fn five_minified_cells_share_nothing_through_any_channel() {
    double_lane(5).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "launches candidate binaries, a private rmux runtime, and credential-free Claude doubles"]
async fn fifteen_minified_cells_share_nothing_through_any_channel() {
    double_lane(15).await;
}

/// The proof that the sweep above can fail.
///
/// Same body, same channels, same assertions; the only change is that the cells
/// share one configuration root and one cwd. If this test stops failing to be
/// clean, every passing result above is worthless.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "launches candidate binaries, a private rmux runtime, and credential-free Claude doubles"]
async fn the_harness_reports_contamination_when_two_cells_share_one_configuration_root() {
    let report = run_matrix(2, Lane::Double, Topology::SharedRoot).await;
    assert!(
        !report.violations.is_empty(),
        "a shared configuration root must be REPORTED as contamination:\n{}",
        report.render("shared-root sweep")
    );
    let channels = report
        .violations
        .iter()
        .map(|violation| violation.channel.as_str())
        .collect::<BTreeSet<_>>();
    assert!(
        channels.contains("projects"),
        "the shared root's transcripts must be named: {channels:?}"
    );
    assert!(
        channels.contains(TRANSCRIPT_INVENTORY_CHANNEL),
        "a foreign session file must be caught by name as well as by content: {channels:?}"
    );
    println!(
        "{}",
        report.render("discrimination proof (expected to fail)")
    );
}

// ---------------------------------------------------------------------------
// Lane (b): the operator's real Claude
// ---------------------------------------------------------------------------

/// The lane the double cannot stand in for.
///
/// Skipped, loudly, unless `PMUX_CONTAMINATION_REAL_CLAUDE=1`, because it
/// spends real model calls and needs the operator's own credentials.
/// `PMUX_CONTAMINATION_CELLS` sets N; two turns per cell are spent.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "spends real Claude turns against the operator's own credentials"]
async fn real_claude_cells_share_nothing_through_any_channel() {
    if std::env::var_os("PMUX_CONTAMINATION_REAL_CLAUDE").is_none() {
        println!(
            "SKIPPED: set PMUX_CONTAMINATION_REAL_CLAUDE=1 to run the cross-cell contamination \
             harness against the operator's real Claude. The double lane cannot reproduce the \
             TUI composer, history recall, or the paste cache."
        );
        return;
    }
    let cells = std::env::var("PMUX_CONTAMINATION_CELLS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(2);
    let report = run_matrix(cells, Lane::RealClaude, Topology::PerCell).await;
    println!("real Claude turns spent: {}", report.real_turns);
    assert_clean(&report, cells, Lane::RealClaude);
}

// ---------------------------------------------------------------------------
// Unit-level assertions about the harness itself
// ---------------------------------------------------------------------------

#[test]
fn every_named_channel_claims_the_paths_beneath_it_and_nothing_else() {
    assert_eq!(channel_for(Path::new("history.jsonl")), "history");
    assert_eq!(
        channel_for(Path::new("projects/pmux-e2e/x.jsonl")),
        "projects"
    );
    assert_eq!(channel_for(Path::new("paste-cache/a/b")), "paste-cache");
    assert_eq!(channel_for(Path::new(".credentials.json")), "credentials");
    // A name no row claims is attributed to the catch-all rather than to the
    // nearest row, and the sweep reads it anyway.
    assert_eq!(
        channel_for(Path::new("a-channel-claude-has-not-invented-yet/x")),
        UNCLASSIFIED_CHANNEL
    );
    let names = ROOT_CHANNELS
        .iter()
        .map(|channel| channel.name)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        names.len(),
        ROOT_CHANNELS.len(),
        "channel names must be unique"
    );
}

/// The coverage gap that let leak 5b through this file, closed as an assertion
/// rather than as a comment.
///
/// The live lanes above are `#[ignore]`d, so `cargo test` cannot watch them
/// probe the second door. What it CAN watch is the table they probe with, and
/// the two filesystem facts that make one of its rows the leak. Deleting the
/// `..`-through-missing row, or letting the minimum drift below the table, or
/// having either measured fact stop being true, all fail here in the default
/// suite.
#[test]
fn the_plain_env_spellings_include_the_one_the_kernel_reports_as_absent() {
    let parent = tempfile::Builder::new()
        .prefix("pmux-spellings-")
        .tempdir_in("/tmp")
        .expect("a scratch root is creatable");
    let root = parent.path().canonicalize().unwrap().join("rootA");
    std::fs::create_dir(&root).unwrap();

    use std::os::unix::fs::MetadataExt as _;
    let rows = plain_env_spellings(&root);
    assert!(
        rows.len() >= SECOND_DOOR_MINIMUM_SPELLINGS,
        "the table must carry at least the count `assert_clean` requires: {}",
        rows.len()
    );
    let labels = rows
        .iter()
        .map(|row| row.label)
        .collect::<BTreeSet<&'static str>>();
    assert!(
        labels.contains("dot-dot through a MISSING directory"),
        "the leak-5b row is the one this harness could not previously construct: {labels:?}"
    );
    assert_eq!(labels.len(), rows.len(), "spelling labels must be unique");

    // Every row is either resolvable now or is the one that is not, and each
    // row is asserted to be what it says it is. The `resolves_now: false` row
    // carries the whole of leak 5b: the kernel reports it absent, and a
    // recursive create -- what Claude does to its own `CLAUDE_CONFIG_DIR` --
    // completes it onto the directory that was there all along.
    let mut missing_rows = 0;
    for row in &rows {
        assert_eq!(
            std::fs::metadata(&row.value).is_ok(),
            row.resolves_now,
            "{}: {} does not have the resolvability the table claims",
            row.label,
            row.value
        );
        if row.resolves_now {
            // Compared on the INODE, not on the canonical path: leak 5's own
            // fact is that `canonicalize` leaves the firmlink alias alone, so a
            // path comparison here would fail on the row that matters most.
            let aliased = std::fs::metadata(&row.value).unwrap();
            let truth = std::fs::metadata(&root).unwrap();
            assert_eq!(
                (aliased.dev(), aliased.ino()),
                (truth.dev(), truth.ino()),
                "{}: a resolvable row must name the very directory it aliases",
                row.label
            );
        } else {
            missing_rows += 1;
            let inode_before = std::fs::metadata(&root).unwrap().ino();
            std::fs::create_dir_all(&row.value).unwrap();
            assert_eq!(
                std::fs::metadata(&row.value).unwrap().ino(),
                inode_before,
                "{}: a recursive create must land on the EXISTING directory, or this row \
                 is not the leak it is here to reproduce",
                row.label
            );
        }
    }
    assert_eq!(
        missing_rows, 1,
        "exactly one row is the `..`-through-missing shape"
    );
}

/// Whether two directories overlap, on `(dev, ino)`, in either direction.
///
/// Deliberately a SECOND implementation of the ancestry walk, living in the
/// test that checks the table rather than in the service that enforces the
/// rule. `aliases_of` in `native.rs::tests` is duplicated for the same reason:
/// a shared helper would let one deletion silently disarm both the fixture and
/// the rule it is a fixture for.
/// LEAK 8 lived in this helper too, and that is the point of keeping it: the
/// fixture-checker and the rule had the same defect independently, so the rule
/// being fixed does not by itself make this file's claims true. The walk is
/// over [`path_the_child_reaches`], not over the caller's spelling, because a
/// row whose final or middle component is a symlink into the victim overlaps it
/// on the resource while overlapping nothing lexically.
fn overlaps(left: &Path, right: &Path) -> bool {
    fn inode(path: &Path) -> Option<(u64, u64)> {
        use std::os::unix::fs::MetadataExt as _;

        std::fs::metadata(path)
            .ok()
            .map(|metadata| (metadata.dev(), metadata.ino()))
    }
    fn contains(ancestor: &Path, descendant: &Path) -> bool {
        let Some(truth) = inode(ancestor) else {
            return false;
        };
        path_the_child_reaches(descendant)
            .ancestors()
            .any(|prefix| inode(prefix) == Some(truth))
    }
    contains(left, right) || contains(right, left)
}

/// Where a path really lands once the child has created what is missing.
///
/// The longest prefix that exists, canonicalized, with the components that do
/// not exist yet appended verbatim. Those trailing components are what
/// `mkdir -p` will create as ordinary directories, so from the canonical prefix
/// down the lexical chain IS the real ancestor chain.
///
/// A second implementation of `claude_launch`'s own resolution, on purpose and
/// for the same reason [`overlaps`] is: a shared helper would let one deletion
/// disarm both the fixture and the rule the fixture exists to police.
fn path_the_child_reaches(path: &Path) -> PathBuf {
    let mut missing = Vec::new();
    for prefix in path.ancestors() {
        if let Ok(canonical) = prefix.canonicalize() {
            let mut reached = canonical;
            reached.extend(missing.iter().rev());
            return reached;
        }
        match prefix.file_name() {
            Some(name) => missing.push(name.to_owned()),
            None => break,
        }
    }
    path.to_path_buf()
}

/// The coverage gap for LEAKS 7 and 8, closed the only way this file can close
/// it.
///
/// The live lanes are `#[ignore]`d, so `cargo test` never watches
/// [`probe_the_containment_door`] refuse anything. What it CAN watch is the
/// table that probe drives with, and this asserts the five properties that make
/// the table worth driving:
///
/// * every row really does OVERLAP a directory the victim binds, measured on
///   the inode rather than asserted in a doc comment;
/// * no row is merely an ALIAS of one -- that is leak 5's table, and a
///   containment table that decayed into a spelling table would be invisible in
///   the report;
/// * all three relations (nested, ancestor, identical-but-in-the-other-role)
///   and all four doors are present, so the table is not ten copies of one
///   shape;
/// * three rows bind a directory that does not exist yet, which is the case an
///   identity test cannot see even in principle;
/// * TWO ROWS REACH THE VICTIM THROUGH A SYMLINK and are lexically disjoint
///   from it -- leak 8. Without that count a table that quietly lost its
///   symlink rows, or whose symlink rows decayed into ordinary nested ones,
///   would report exactly the same clean sweep.
#[test]
fn every_containment_relation_really_overlaps_a_directory_the_victim_binds() {
    let scratch = tempfile::Builder::new()
        .prefix("pmux-containment-")
        .tempdir_in("/tmp")
        .expect("a scratch root is creatable");
    let ancestor = scratch.path().canonicalize().unwrap();
    let victim_root = ancestor.join("victim-root");
    let victim_cwd = ancestor.join("victim-cwd");
    for directory in [&victim_root, &victim_cwd] {
        std::fs::create_dir(directory).unwrap();
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    // The cell's own transcripts. Two rows stand on this directory because it
    // is the one subdirectory of a live root an intruder can name without
    // guessing anything at all.
    std::fs::create_dir(victim_root.join("projects")).unwrap();

    let (rows, created) = containment_relations(&victim_root, &victim_cwd, &ancestor);
    assert!(
        rows.len() >= CONTAINMENT_MINIMUM_SHAPES,
        "the table must carry at least the count `assert_clean` requires: {}",
        rows.len()
    );
    let labels = rows
        .iter()
        .map(|row| row.label)
        .collect::<BTreeSet<&'static str>>();
    assert_eq!(labels.len(), rows.len(), "relation labels must be unique");
    let doors = rows.iter().map(|row| row.door).collect::<BTreeSet<Door>>();
    assert_eq!(
        doors.len(),
        4,
        "every entry path a directory can be bound through must be represented: {doors:?}"
    );

    let (mut nested, mut ancestors, mut identical, mut absent, mut through_a_link) =
        (0, 0, 0, 0, 0);
    for row in &rows {
        assert!(
            overlaps(&row.binds, &row.touches),
            "{}: {} does not overlap {}, so this row proves nothing about containment",
            row.label,
            row.binds.display(),
            row.touches.display()
        );
        assert!(
            row.touches == victim_root || row.touches == victim_cwd,
            "{}: a row must touch a directory the victim actually binds, not {}",
            row.label,
            row.touches.display()
        );
        // Classified on what the row REACHES, not on how it is spelled: the two
        // leak-8 rows are spelled outside everything the victim binds, and a
        // classifier that read the spelling would file them as ancestors and
        // report a table that carries relations it does not.
        let reached = path_the_child_reaches(&row.binds);
        let same_resource = overlaps(&row.binds, &row.touches)
            && reached.canonicalize().ok() == row.touches.canonicalize().ok();
        if !row.binds.exists() {
            absent += 1;
        }
        if same_resource {
            identical += 1;
        } else if reached.starts_with(&row.touches) {
            nested += 1;
        } else {
            ancestors += 1;
        }
        if !same_resource
            && reached.starts_with(&row.touches)
            && !row.binds.starts_with(&row.touches)
        {
            through_a_link += 1;
        }
    }
    assert!(
        nested >= 1 && ancestors >= 1 && identical >= 1,
        "the table must carry all three relations, or it is a spelling table wearing a \
         containment table's name: nested={nested} ancestors={ancestors} identical={identical}"
    );
    assert_eq!(
        identical, 1,
        "exactly one row is the same directory in the OTHER role; every other row must be a \
         strict containment, because leak 5 already owns the equal-directory axis"
    );
    assert_eq!(
        absent, 3,
        "three rows must bind a directory that does not exist yet -- the case no identity test \
         can see, because there is no inode to compare and the child creates it afterwards \
         inside the victim"
    );
    assert_eq!(
        through_a_link, 2,
        "two rows must reach the victim THROUGH A SYMLINK while being lexically disjoint from \
         everything it binds -- one with the link as the final component and one with it in the \
         middle. That is leak 8, and a lexical ancestor walk over the caller's spelling cannot \
         see either of them"
    );

    for artifact in created {
        remove_probe_artifact(&artifact);
    }
}

#[test]
fn a_secret_is_unguessable_and_survives_being_typed() {
    let first = fresh_secret(0);
    let second = fresh_secret(0);
    assert_ne!(first, second);
    // 8 marker + one index digit + one separator + 32 hex.
    assert_eq!(first.len(), 42);
    assert!(fresh_secret(14).starts_with("PMUXLEAK14Z"));
    assert!(
        first
            .chars()
            .all(|character| character.is_ascii_uppercase() || character.is_ascii_digit()),
        "a secret must survive a TUI composer and a JSON transcript unchanged: {first}"
    );
}
