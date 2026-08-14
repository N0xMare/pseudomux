#![allow(
    unsafe_code,
    reason = "the harness signals only the exact child identity it spawned"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use pseudomux_client::PmuxClient;
use pseudomux_e2e::{
    TEST_ANTHROPIC_SECRET, TEST_CLAUDE_VERSION, TEST_ENV_ATTESTATION_MARKER,
    TEST_ENV_PATCHED_VALUE, TEST_ENV_SAFE_CONFIG_VALUE, TEST_ENV_SET_ONLY_VALUE,
    TEST_PROVIDER_SECRET, TEST_SUBSCRIPTION_KEYS, TEST_TRANSPARENT_EXACT_KEYS,
};
use pseudomux_protocol::v1::{
    AuthPolicy, ClaudeLaunchConfig, CompatibilityPolicy, DisconnectAction, EnvironmentSpec,
    EventPayload, InputTransport, LifecycleMode, RetentionPolicy, SessionCell, SessionHandle,
    SessionId, SessionIdentity, SessionState, StartSessionRequest, SystemPromptPolicy,
    TerminalProfile, TerminalSpec, TurnAccepted, TurnLeasePolicy, TurnOutcome, TurnRequest,
    TurnResult,
};
use serde_json::Value;
use tempfile::TempDir;
use tokio::time::Instant;
use uuid::Uuid;

use super::{
    CandidateFiles, ProcessIdentity, ProcessResources, find_direct_child, process_resources,
    runtime_entries, set_owner_only, wait_for_process_absence,
};

const PROCESS_TIMEOUT: Duration = Duration::from_secs(15);
const TEST_PROFILE_DRAIN_MS: u64 = 50;

/// Exact-candidate, owner-only public-daemon harness for credential-free L5 tests.
///
/// The fake Claude is a tracked foreground PTY executable. Production pmux,
/// not this harness, decides transcript and completion semantics.
pub struct ActualDaemon {
    candidates: CandidateFiles,
    root: TempDir,
    root_path: PathBuf,
    socket: PathBuf,
    runtime_parent: PathBuf,
    cwd: PathBuf,
    config_root: PathBuf,
    state_root: PathBuf,
    path_first: PathBuf,
    path_last: PathBuf,
    shim_dir: PathBuf,
    client: PmuxClient,
    child: Child,
    daemon_identity: ProcessIdentity,
    sidecar_identity: ProcessIdentity,
    stderr_path: PathBuf,
    stopped: bool,
}

impl ActualDaemon {
    pub async fn start() -> Result<Self> {
        let candidates = CandidateFiles::discover(&[
            "pmuxd",
            "pmux-rmuxd",
            "pmux-launcher",
            "pmux-test-claude",
        ])?;
        candidates.assert_unchanged()?;

        let root = tempfile::Builder::new()
            .prefix("pmux-l5-daemon-")
            .tempdir_in("/tmp")?;
        set_owner_only(root.path())?;
        let root_path = root.path().canonicalize()?;
        let socket = root_path.join("pmux.sock");
        let runtime_parent = root_path.join("private");
        let cwd = root_path.join("workspace");
        let config_root = root_path.join("config");
        let state_root = root_path.join("state");
        let path_first = root_path.join("path-first");
        let path_last = root_path.join("path-last");
        let shim_dir = root_path.join("parent-tmux-shim");
        for directory in [
            &runtime_parent,
            &cwd,
            &config_root,
            &state_root,
            &path_first,
            &path_last,
            &shim_dir,
        ] {
            std::fs::create_dir(directory)?;
            set_owner_only(directory)?;
        }

        let stderr_path = root_path.join("pmuxd.stderr");
        let stderr = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&stderr_path)?;
        std::fs::set_permissions(&stderr_path, std::fs::Permissions::from_mode(0o600))?;
        let profile = serde_json::json!({
            "claude_version": TEST_CLAUDE_VERSION,
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "terminal_profile": "transparent",
            "input_transport": "sdk",
            "transcript_drain_ms": TEST_PROFILE_DRAIN_MS,
        });
        let mut child = Command::new(candidates.path("pmuxd"))
            .arg("serve")
            .arg("--socket")
            .arg(&socket)
            .arg("--rmuxd")
            .arg(candidates.path("pmux-rmuxd"))
            .arg("--launcher")
            .arg(candidates.path("pmux-launcher"))
            .arg("--runtime-parent")
            .arg(&runtime_parent)
            .arg("--tested-claude-profile")
            .arg(profile.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(stderr))
            .spawn()
            .context("failed to spawn exact pmuxd candidate")?;
        let daemon_marker = candidates
            .path("pmuxd")
            .file_name()
            .and_then(|name| name.to_str())
            .context("pmuxd candidate has no UTF-8 file name")?;
        let daemon_identity = ProcessIdentity::capture(child.id(), daemon_marker)?;
        let client = PmuxClient::new(&socket)?;
        let startup_deadline = Instant::now() + PROCESS_TIMEOUT;
        loop {
            if client.ping().await.is_ok() {
                break;
            }
            if let Some(status) = child.try_wait()? {
                bail!(
                    "pmuxd exited during startup with {status}: {}",
                    std::fs::read_to_string(&stderr_path).unwrap_or_default()
                );
            }
            ensure!(
                Instant::now() < startup_deadline,
                "pmuxd did not become ready before the exact startup deadline"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        let sidecar_marker = candidates
            .path("pmux-rmuxd")
            .file_name()
            .and_then(|name| name.to_str())
            .context("pmux-rmuxd candidate has no UTF-8 file name")?;
        let sidecar_pid = loop {
            match find_direct_child(child.id(), &[sidecar_marker]) {
                Ok(pid) => break pid,
                Err(_) if Instant::now() < startup_deadline => {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                Err(error) => return Err(error.context("exact private sidecar was not observed")),
            }
        };
        let sidecar_identity = ProcessIdentity::capture(sidecar_pid, sidecar_marker)?;
        candidates.assert_unchanged()?;

        Ok(Self {
            candidates,
            root,
            root_path,
            socket,
            runtime_parent,
            cwd,
            config_root,
            state_root,
            path_first,
            path_last,
            shim_dir,
            client,
            child,
            daemon_identity,
            sidecar_identity,
            stderr_path,
            stopped: false,
        })
    }

    pub fn client(&self) -> PmuxClient {
        self.client.clone()
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    pub fn daemon_resources(&self) -> Result<ProcessResources> {
        process_resources(&self.daemon_identity)
    }

    pub fn start_request(&self, session_id: SessionId) -> StartSessionRequest {
        StartSessionRequest {
            identity: SessionIdentity::New {
                session_id: Some(session_id),
            },
            cwd: self.cwd.to_string_lossy().into_owned(),
            agent: None,
            claude: Some(ClaudeLaunchConfig {
                executable: self
                    .candidates
                    .path("pmux-test-claude")
                    .to_string_lossy()
                    .into_owned(),
                model: Some("test-model".to_owned()),
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
            environment: self.environment(),
            auth_policy: AuthPolicy::Subscription,
            config_isolation: None,
            terminal: TerminalSpec {
                rows: 24,
                cols: 120,
                profile: TerminalProfile::Transparent,
                input_transport: InputTransport::Sdk,
            },
            lifecycle: LifecycleMode::Transcript,
            retention: RetentionPolicy::Persistent {
                idle_ttl_ms: 60_000,
            },
            compatibility: CompatibilityPolicy::RequireTested,
            cell: SessionCell::Full,
        }
    }

    pub fn turn(prompt: impl Into<String>) -> TurnRequest {
        TurnRequest {
            turn_id: Uuid::new_v4(),
            prompt: prompt.into(),
            deadline_unix_ms: Some(now_ms() + 30_000),
            lease: TurnLeasePolicy {
                on_disconnect: DisconnectAction::Continue,
                heartbeat_timeout_ms: None,
            },
        }
    }

    pub async fn wait_for_result(
        &self,
        handle: &SessionHandle,
        accepted: &TurnAccepted,
    ) -> Result<TurnResult> {
        let mut after_sequence = accepted.next_sequence.saturating_sub(1);
        for _ in 0..60 {
            let batch = self
                .client
                .subscribe_events(pseudomux_protocol::v1::SubscribeEventsRequest {
                    session_id: handle.session_id,
                    generation_id: handle.generation_id,
                    after_sequence,
                    wait_ms: 1_000,
                    max_events: 128,
                })
                .await?;
            ensure!(batch.replay_gap.is_none(), "unexpected replay gap");
            for event in batch.events {
                after_sequence = event.sequence;
                match event.event {
                    EventPayload::TurnCompleted(result) if result.turn_id == accepted.turn_id => {
                        ensure!(result.outcome == TurnOutcome::Completed);
                        return Ok(*result);
                    }
                    EventPayload::TurnFailed(error) if event.turn_id == Some(accepted.turn_id) => {
                        bail!("turn failed unexpectedly with {:?}", error.code);
                    }
                    _ => {}
                }
            }
        }
        bail!("turn {} did not complete", accepted.turn_id)
    }

    pub async fn wait_for_ready(&self, handle: &SessionHandle) -> Result<()> {
        let deadline = Instant::now() + PROCESS_TIMEOUT;
        loop {
            let snapshot = self
                .client
                .inspect_session(handle.session_id, handle.generation_id)
                .await?;
            if snapshot.state == SessionState::Ready && snapshot.active_turn_id.is_none() {
                return Ok(());
            }
            ensure!(
                Instant::now() < deadline,
                "session {} did not return to ready",
                handle.session_id
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    pub fn transcript_path(&self, session_id: SessionId) -> PathBuf {
        self.config_root
            .join("projects/pmux-e2e")
            .join(format!("{session_id}.jsonl"))
    }

    pub async fn launched_processes(
        &self,
        expected: usize,
    ) -> Result<BTreeMap<SessionId, ProcessIdentity>> {
        let launches = self.state_root.join("launches.jsonl");
        let deadline = Instant::now() + PROCESS_TIMEOUT;
        loop {
            if let Ok(contents) = std::fs::read_to_string(&launches) {
                let values = contents
                    .lines()
                    .map(serde_json::from_str::<Value>)
                    .collect::<serde_json::Result<Vec<_>>>();
                if let Ok(values) = values
                    && values.len() == expected
                {
                    let marker = self
                        .candidates
                        .path("pmux-test-claude")
                        .file_name()
                        .and_then(|name| name.to_str())
                        .context("fake Claude candidate has no UTF-8 file name")?;
                    let mut identities = BTreeMap::new();
                    for value in values {
                        let session_id = value
                            .get("session_id")
                            .and_then(Value::as_str)
                            .context("launch record omitted session_id")?
                            .parse::<Uuid>()?;
                        let pid = value
                            .get("pid")
                            .and_then(Value::as_u64)
                            .and_then(|pid| u32::try_from(pid).ok())
                            .context("launch record omitted a bounded pid")?;
                        ensure!(
                            identities
                                .insert(session_id, ProcessIdentity::capture(pid, marker)?)
                                .is_none(),
                            "duplicate interactive launch for session {session_id}"
                        );
                    }
                    return Ok(identities);
                }
            }
            ensure!(
                Instant::now() < deadline,
                "did not observe exactly {expected} interactive launches"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    pub async fn assert_processes_absent(
        &self,
        identities: &BTreeMap<SessionId, ProcessIdentity>,
    ) -> Result<()> {
        for identity in identities.values() {
            wait_for_process_absence(identity, PROCESS_TIMEOUT).await?;
        }
        Ok(())
    }

    pub async fn stop(&mut self) -> Result<()> {
        ensure!(!self.stopped, "daemon harness was stopped twice");
        self.candidates.assert_unchanged()?;
        self.daemon_identity.signal(libc::SIGTERM)?;
        let deadline = Instant::now() + PROCESS_TIMEOUT;
        let status = loop {
            if let Some(status) = self.child.try_wait()? {
                break status;
            }
            ensure!(Instant::now() < deadline, "pmuxd did not stop in time");
            tokio::time::sleep(Duration::from_millis(25)).await;
        };
        ensure!(
            status.success(),
            "pmuxd shutdown failed with {status}: {}",
            std::fs::read_to_string(&self.stderr_path).unwrap_or_default()
        );
        wait_for_process_absence(&self.daemon_identity, PROCESS_TIMEOUT).await?;
        wait_for_process_absence(&self.sidecar_identity, PROCESS_TIMEOUT).await?;
        ensure!(!self.socket.exists(), "public socket survived shutdown");
        ensure!(
            runtime_entries(&self.runtime_parent)?.is_empty(),
            "private runtime entries survived shutdown"
        );
        self.candidates.assert_unchanged()?;
        self.stopped = true;
        Ok(())
    }

    fn environment(&self) -> EnvironmentSpec {
        let path_with_shim = std::env::join_paths([
            self.path_first.as_path(),
            self.shim_dir.as_path(),
            self.path_last.as_path(),
        ])
        .expect("test PATH components are valid")
        .into_string()
        .expect("test PATH is UTF-8");
        let expected_path =
            std::env::join_paths([self.path_first.as_path(), self.path_last.as_path()])
                .expect("expected test PATH components are valid")
                .into_string()
                .expect("expected test PATH is UTF-8");
        let mut snapshot = BTreeMap::from([
            (
                "HOME".to_owned(),
                self.root_path.to_string_lossy().into_owned(),
            ),
            (
                "CLAUDE_CONFIG_DIR".to_owned(),
                self.config_root.to_string_lossy().into_owned(),
            ),
            (
                "PMUX_TEST_STATE_DIR".to_owned(),
                self.state_root.to_string_lossy().into_owned(),
            ),
            (
                "PMUX_TEST_ENV_ATTESTATION".to_owned(),
                TEST_ENV_ATTESTATION_MARKER.to_owned(),
            ),
            (
                "PMUX_TEST_CALLER_SAFE_CONFIG".to_owned(),
                TEST_ENV_SAFE_CONFIG_VALUE.to_owned(),
            ),
            ("PMUX_TEST_EXPECTED_PATH".to_owned(), expected_path),
            ("PATH".to_owned(), path_with_shim),
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
            snapshot.insert(
                (*key).to_owned(),
                if key.starts_with("ANTHROPIC") {
                    TEST_ANTHROPIC_SECRET
                } else {
                    TEST_PROVIDER_SECRET
                }
                .to_owned(),
            );
        }
        for key in TEST_TRANSPARENT_EXACT_KEYS {
            snapshot.insert(
                (*key).to_owned(),
                if *key == "TMUX_PROGRAM" {
                    self.shim_dir.join("tmux").to_string_lossy().into_owned()
                } else {
                    format!("ambient-{key}")
                },
            );
        }
        snapshot.extend([
            ("RMUX_TEST_BOUNDARY".to_owned(), "must-strip".to_owned()),
            ("TMUX_TEST_BOUNDARY".to_owned(), "must-strip".to_owned()),
            (
                "CLAUDE_AGENT_SDK_TEST_BOUNDARY".to_owned(),
                "must-strip".to_owned(),
            ),
            (
                "CLAUDE_CODE_SDK_TEST_BOUNDARY".to_owned(),
                "must-strip".to_owned(),
            ),
        ]);
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
}

impl Drop for ActualDaemon {
    fn drop(&mut self) {
        if self.stopped {
            return;
        }
        if self.daemon_identity.assert_running().is_ok() {
            let _ = self.daemon_identity.signal(libc::SIGKILL);
        }
        let _ = self.child.wait();
        if self.sidecar_identity.assert_running().is_ok() {
            let _ = self.sidecar_identity.signal(libc::SIGKILL);
        }
        for _ in 0..200 {
            if !self.sidecar_identity.is_present().unwrap_or(false) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_millis()
        .try_into()
        .expect("current time fits protocol v1")
}
