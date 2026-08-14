//! Lifecycle owner for the private rmux sidecar and launcher broker.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use pseudomux_rmux::{
    ControlPlaneFault, LaunchSpec, OWNER_GRACEFUL_SHUTDOWN_FRAME, RmuxBackend, RmuxBackendConfig,
    TerminalBackend, TerminalBackendError, TerminalLaunch, TerminalSession,
};
use serde::Deserialize;
use tempfile::TempDir;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};

use crate::launch_broker::LaunchBroker;

pub const PINNED_RMUX_VERSION: &str = "0.9.0";

/// Why one private terminal could not be created.
///
/// This type exists because the backend's typed `TerminalBackendError` used to
/// be flattened into `anyhow` on the way out of [`PrivateRuntime::create_terminal`]
/// and then discarded wholesale by its caller. A lost control plane, a rejected
/// launch spec, and a failed process-boundary observation are three different
/// operational situations, and all three surfaced identically as an opaque
/// `RmuxUnavailable` with no record anywhere of which one had happened.
///
/// Keeping the cause typed here is the precondition for every later transport
/// layer being diagnosable at all.
#[derive(Debug, Error)]
pub enum CreateTerminalError {
    /// The sensitive-launch broker refused to register the spec, so no rmux
    /// call was ever attempted.
    #[error("{0}")]
    LaunchRegistration(anyhow::Error),
    /// The private rmux backend failed. The cause is preserved verbatim.
    #[error(transparent)]
    Backend(#[from] TerminalBackendError),
}

impl CreateTerminalError {
    /// Stable, content-free classification for logs and error `details`.
    ///
    /// Deliberately not the error's `Display`: rmux errors can carry session
    /// names, filesystem paths, wait matchers, and rendered screen text, and
    /// this process handles prompts and account state.
    #[must_use]
    pub const fn cause(&self) -> &'static str {
        match self {
            Self::LaunchRegistration(_) => "launch_registration",
            Self::Backend(TerminalBackendError::InvalidLaunch(_)) => "invalid_launch",
            Self::Backend(TerminalBackendError::ControlPlaneLost) => "control_plane_lost",
            Self::Backend(TerminalBackendError::Rmux(_)) => "rmux_operation_failed",
            Self::Backend(TerminalBackendError::ProcessBoundary(_)) => "process_boundary",
        }
    }
}

#[derive(Clone, Debug)]
pub struct PrivateRuntimeConfig {
    pub rmuxd: PathBuf,
    pub launcher: PathBuf,
    pub runtime_parent: Option<PathBuf>,
    pub startup_timeout: Duration,
    pub operation_timeout: Duration,
    pub lease_ttl: Duration,
}

impl PrivateRuntimeConfig {
    /// Resolves companion binaries next to the current pmux executable.
    pub fn from_current_exe() -> Result<Self> {
        let current = std::env::current_exe().context("cannot resolve current executable")?;
        let directory = current
            .parent()
            .context("current executable has no parent directory")?;
        Ok(Self {
            rmuxd: companion_binary(directory, "pmux-rmuxd"),
            launcher: companion_binary(directory, "pmux-launcher"),
            runtime_parent: None,
            startup_timeout: Duration::from_secs(10),
            operation_timeout: Duration::from_secs(10),
            lease_ttl: Duration::from_secs(5),
        })
    }

    fn validate(&self) -> Result<()> {
        for (label, path) in [
            ("pmux-rmuxd", &self.rmuxd),
            ("pmux-launcher", &self.launcher),
        ] {
            if !path.is_absolute() {
                bail!("{label} path must be absolute: {}", path.display());
            }
            if !path.is_file() {
                bail!("{label} binary is unavailable: {}", path.display());
            }
        }
        if self.startup_timeout.is_zero()
            || self.operation_timeout.is_zero()
            || self.lease_ttl.is_zero()
        {
            bail!("runtime timeouts must be non-zero");
        }
        Ok(())
    }
}

/// Owns one private rmux daemon, one launch broker, and all leased sessions.
pub struct PrivateRuntime {
    sidecar: tokio::sync::Mutex<Child>,
    sidecar_shutdown_timeout: Duration,
    broker: LaunchBroker,
    backend: Arc<RmuxBackend>,
    launcher: PathBuf,
    rmux_socket: PathBuf,
    runtime_dir: TempDir,
    /// The deadline every rmux operation is held to, retained so a health
    /// report can measure against the SAME number the runtime enforces rather
    /// than against a constant beside it that can drift from it.
    operation_timeout: Duration,
}

/// Everything `NativeService` asks of the private runtime, and nothing else.
///
/// **THE SEAM.** `NativeService` used to hold an `Arc<PrivateRuntime>`, and a
/// `PrivateRuntime` cannot exist without a real `pmux-rmuxd` sidecar, a real
/// launch broker socket and a completed rmux handshake. That made the service
/// itself unconstructible in a unit test, and the cost was measured rather than
/// guessed: a full-scope mutation run left the completion proof
/// (`wait_for_turn`), the generation fences (`clear_boundary`, `attach`,
/// `close_session_with_state`), the idle reaper, the pool-disclosure filter in
/// `diagnose` and the minified cell's `RequireTested` admission all surviving,
/// every one of them because nothing in the fast suite could reach the method
/// at all. See `evidence/mutation-survivor-register.json`, rows whose
/// `closeable` is `seam`.
///
/// The eight methods here are the entire set `NativeService` calls on it, taken
/// from the call sites rather than from the type: the trait is the interface,
/// so `PrivateRuntime` states each of them once and a double states them again
/// only where a test scripts one.
///
/// A double implementing this trait must be able to *refuse*: see
/// `ScriptedRuntime` below, whose every method answers "nothing scripted this"
/// unless a test said what it should do, and
/// `crates/service/src/native/tests/seam.rs`, which is what it is for.
#[async_trait::async_trait]
pub trait SessionRuntime: Send + Sync + 'static {
    /// Registers a sensitive launch spec and atomically rolls it back on rmux
    /// failure.
    ///
    /// Returns [`CreateTerminalError`] rather than `anyhow::Error` on purpose:
    /// `anyhow::Error::new` here erased the only signal that distinguishes a
    /// lost private control plane from a rejected launch, and the caller then
    /// dropped even that. See [`CreateTerminalError`].
    async fn create_terminal(
        &self,
        session_id: uuid::Uuid,
        rows: u16,
        cols: u16,
        spec: LaunchSpec,
    ) -> Result<Box<dyn TerminalSession>, CreateTerminalError>;

    /// Completes one real request against the private sidecar's dispatch path
    /// and returns the private terminals it reports.
    ///
    /// The whole daemon costs one round trip here, whatever the pool size. See
    /// [`RmuxBackend::probe_request_path`] for why this operation and not a
    /// handshake, a per-session capture, or a lease read.
    async fn probe_request_path(&self) -> Result<BTreeSet<String>, ControlPlaneFault>;

    /// One real launcher-shaped exchange against the launch broker, bounded by
    /// the SAME deadline every rmux operation is held to.
    ///
    /// The deadline is read from the runtime rather than passed in, so the
    /// number a probe waits and the number the runtime enforces cannot drift
    /// apart.
    async fn probe_launch_broker(&self) -> crate::launch_broker::BrokerProbe;

    /// Whether the launch broker is still accepting launcher connections.
    ///
    /// See [`LaunchBroker::is_accepting`] for why this is a task-liveness read
    /// and not a registered spec. [`Self::probe_launch_broker`] is the one that
    /// actually exchanges a frame.
    fn launch_broker_is_accepting(&self) -> bool;

    /// The deadline every rmux operation against the private sidecar is held
    /// to. The performance layer of the health tree measures against this.
    fn operation_timeout(&self) -> Duration;

    fn rmux_socket(&self) -> &Path;

    fn runtime_dir(&self) -> &Path;

    async fn shutdown(&self) -> Result<()>;
}

#[async_trait::async_trait]
impl SessionRuntime for PrivateRuntime {
    async fn create_terminal(
        &self,
        session_id: uuid::Uuid,
        rows: u16,
        cols: u16,
        spec: LaunchSpec,
    ) -> Result<Box<dyn TerminalSession>, CreateTerminalError> {
        let cwd = spec.cwd.clone();
        let token = self
            .broker
            .register(spec)
            .map_err(CreateTerminalError::LaunchRegistration)?;
        let result = self
            .backend
            .create(TerminalLaunch {
                session_id,
                cwd,
                rows,
                cols,
                launch_token: token.clone(),
            })
            .await;
        if result.is_err() {
            self.broker.revoke(&token);
        }
        result.map_err(CreateTerminalError::Backend)
    }

    async fn probe_request_path(&self) -> Result<BTreeSet<String>, ControlPlaneFault> {
        self.backend
            .probe_request_path()
            .await
            .map(|names| names.into_iter().collect())
    }

    async fn probe_launch_broker(&self) -> crate::launch_broker::BrokerProbe {
        self.broker.probe(self.operation_timeout).await
    }

    fn launch_broker_is_accepting(&self) -> bool {
        self.broker.is_accepting()
    }

    fn operation_timeout(&self) -> Duration {
        self.operation_timeout
    }

    fn rmux_socket(&self) -> &Path {
        &self.rmux_socket
    }

    fn runtime_dir(&self) -> &Path {
        self.runtime_dir.path()
    }

    async fn shutdown(&self) -> Result<()> {
        let mut sidecar = self.sidecar.lock().await;
        if let Some(status) = sidecar.try_wait()? {
            return require_successful_sidecar_exit(status);
        }
        // Authenticate an orderly close before dropping the parent-death pipe.
        // Bare EOF remains unforgeable evidence that the owning process died,
        // allowing the surviving sidecar to clean its exact private runtime.
        let graceful_signal = match sidecar.stdin.as_mut() {
            Some(stdin) => stdin
                .write_all(OWNER_GRACEFUL_SHUTDOWN_FRAME)
                .await
                .context("failed to signal graceful private-rmux shutdown"),
            None => Err(anyhow::anyhow!("private-rmux owner pipe is unavailable")),
        };
        drop(sidecar.stdin.take());
        let exit = match tokio::time::timeout(self.sidecar_shutdown_timeout, sidecar.wait()).await {
            Ok(status) => {
                let status = status.context("failed to wait for private rmux sidecar")?;
                require_successful_sidecar_exit(status)
            }
            Err(_) => {
                sidecar
                    .kill()
                    .await
                    .context("failed to force-stop unresponsive private rmux sidecar")?;
                let _ = sidecar.wait().await;
                bail!("private rmux sidecar did not stop within its shutdown deadline")
            }
        };
        exit?;
        graceful_signal
    }
}

impl PrivateRuntime {
    pub async fn start(config: PrivateRuntimeConfig) -> Result<Self> {
        config.validate()?;
        let runtime_dir = match &config.runtime_parent {
            Some(parent) => tempfile::Builder::new().prefix("pmux-").tempdir_in(parent),
            None => tempfile::Builder::new().prefix("pmux-").tempdir(),
        }
        .context("failed to create private runtime directory")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(runtime_dir.path(), std::fs::Permissions::from_mode(0o700))?;
        }

        let rmux_socket = runtime_dir.path().join("rmux.sock");
        let launcher_socket = runtime_dir.path().join("launcher.sock");
        let broker = LaunchBroker::bind(launcher_socket.clone()).await?;

        let mut sidecar_command = Command::new(&config.rmuxd);
        sidecar_command
            .arg("--socket")
            .arg(&rmux_socket)
            .arg("--announce-ready")
            .arg("--owner-stdin")
            .arg("--launcher-socket")
            .arg(&launcher_socket)
            .arg("--shutdown-timeout-ms")
            .arg(config.operation_timeout.as_millis().to_string())
            // The pipe is a kernel-owned parent-death capability. Even SIGKILL
            // closes it, so the private sidecar cannot outlive pmuxd silently.
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped());
        #[cfg(unix)]
        {
            // rmux-server installs process-global signal handling inside the
            // sidecar. Give it a distinct process group so terminal-generated
            // Ctrl-C reaches pmuxd only; the owner pipe remains the sole normal
            // sidecar shutdown authority.
            sidecar_command.process_group(0);
        }
        let mut sidecar = sidecar_command
            .spawn()
            .with_context(|| format!("failed to start {}", config.rmuxd.display()))?;

        let ready_result = wait_for_ready(&mut sidecar, &rmux_socket, config.startup_timeout).await;
        if let Err(error) = ready_result {
            stop_failed_sidecar(&mut sidecar, config.operation_timeout).await;
            return Err(error);
        }

        let backend = match RmuxBackend::configure(RmuxBackendConfig {
            socket: rmux_socket.clone(),
            launcher: config.launcher.clone(),
            launcher_socket,
            operation_timeout: config.operation_timeout,
            lease_ttl: config.lease_ttl,
        }) {
            Ok(backend) => Arc::new(backend),
            Err(error) => {
                stop_failed_sidecar(&mut sidecar, config.operation_timeout).await;
                return Err(
                    anyhow::Error::new(error).context("private rmux backend configuration failed")
                );
            }
        };

        // `RmuxBackend::configure` performs no I/O: every session now mints its
        // own transport, so the backend has nothing daemon-wide left to
        // connect. The `.connect()` it replaced was doing a second job nobody
        // had named -- it was the startup reachability check -- and dropping it
        // silently would have moved "the private socket is unusable" from a
        // failed `PrivateRuntime::start` to a failed first `start_session`,
        // with a healthy sidecar readiness announcement in between. This probe
        // restores that check on a throwaway connection, and it proves more
        // than `.connect()` did: it completes an rmux handshake rather than
        // merely opening the socket.
        //
        // This is the one `ControlPlaneLost` in the process that is genuinely
        // daemon-wide: no session exists yet, so nothing here is scoped to one.
        // That is why the variant's `Display` does not name a session; the
        // terminal mappers add "session" where they have the evidence for it.
        if let Err(error) = backend.probe_control_plane().await {
            stop_failed_sidecar(&mut sidecar, config.operation_timeout).await;
            return Err(anyhow::Error::new(error).context("private rmux control plane is unusable"));
        }

        Ok(Self {
            sidecar: tokio::sync::Mutex::new(sidecar),
            // The sidecar separately bounds rmux shutdown and its final POSIX
            // session reap pass. Leave a small scheduling margin for both.
            sidecar_shutdown_timeout: config
                .operation_timeout
                .saturating_mul(2)
                .saturating_add(Duration::from_secs(1)),
            broker,
            backend,
            launcher: config.launcher,
            rmux_socket,
            runtime_dir,
            operation_timeout: config.operation_timeout,
        })
    }

    pub fn launcher(&self) -> &Path {
        &self.launcher
    }
}

/// A [`SessionRuntime`] that answers what a test scripted and REFUSES anything
/// else by name.
///
/// **The double must be able to fail the guard.** A double that answers every
/// call plausibly makes every guard above it pass whatever the guard says, and
/// a guard that cannot fail is worse than no test at all. So each method here
/// has a refusing default and a scripting method beside it:
///
/// | method | unscripted answer |
/// |---|---|
/// | `create_terminal` | `LaunchRegistration`, "nothing scripted a terminal" |
/// | `probe_request_path` | `Err(ControlPlaneFault::Unreachable)` |
/// | `probe_launch_broker` | `ConnectFailed`, "nothing scripted a probe" |
/// | `launch_broker_is_accepting` | `false` |
/// | `shutdown` | `Ok(())`, counted |
///
/// `runtime_dir` is a real 0700 [`TempDir`] and `rmux_socket` a real path
/// inside it, because the callers that read them (`grant_attach`, the
/// config-isolation seed) do filesystem work with what they are handed and a
/// synthetic path would prove something about a directory that does not exist.
#[cfg(test)]
pub(crate) struct ScriptedRuntime {
    runtime_dir: TempDir,
    rmux_socket: PathBuf,
    operation_timeout: Duration,
    broker_accepting: std::sync::atomic::AtomicBool,
    broker_probe: std::sync::Mutex<Option<crate::launch_broker::BrokerProbe>>,
    control_plane: std::sync::Mutex<Option<Result<BTreeSet<String>, ControlPlaneFault>>>,
    terminals: std::sync::Mutex<
        std::collections::VecDeque<Result<Box<dyn TerminalSession>, CreateTerminalError>>,
    >,
    /// Every `create_terminal` this runtime was asked for, in order, whether or
    /// not anything was scripted for it.
    creations: std::sync::Mutex<Vec<(uuid::Uuid, u16, u16)>>,
    shutdown_error: std::sync::Mutex<Option<String>>,
    shutdowns: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
impl ScriptedRuntime {
    pub(crate) fn new() -> Self {
        let runtime_dir = tempfile::Builder::new()
            .prefix("pmux-scripted-")
            .tempdir()
            .expect("a scripted runtime needs a private directory");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(runtime_dir.path(), std::fs::Permissions::from_mode(0o700))
                .expect("a scripted runtime directory must be owner-only");
        }
        let rmux_socket = runtime_dir.path().join("rmux.sock");
        Self {
            runtime_dir,
            rmux_socket,
            operation_timeout: Duration::from_secs(10),
            broker_accepting: std::sync::atomic::AtomicBool::new(false),
            broker_probe: std::sync::Mutex::new(None),
            control_plane: std::sync::Mutex::new(None),
            terminals: std::sync::Mutex::new(std::collections::VecDeque::new()),
            creations: std::sync::Mutex::new(Vec::new()),
            shutdown_error: std::sync::Mutex::new(None),
            shutdowns: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// The answer one `create_terminal` call gets, queued in call order.
    pub(crate) fn script_terminal(
        &self,
        outcome: Result<Box<dyn TerminalSession>, CreateTerminalError>,
    ) {
        self.terminals.lock().unwrap().push_back(outcome);
    }

    pub(crate) fn script_control_plane(
        &self,
        outcome: Result<BTreeSet<String>, ControlPlaneFault>,
    ) {
        *self.control_plane.lock().unwrap() = Some(outcome);
    }

    pub(crate) fn script_launch_broker(
        &self,
        accepting: bool,
        probe: crate::launch_broker::BrokerProbe,
    ) {
        self.broker_accepting
            .store(accepting, std::sync::atomic::Ordering::SeqCst);
        *self.broker_probe.lock().unwrap() = Some(probe);
    }

    pub(crate) fn script_shutdown_failure(&self, message: &str) {
        *self.shutdown_error.lock().unwrap() = Some(message.to_owned());
    }

    pub(crate) fn creations(&self) -> Vec<(uuid::Uuid, u16, u16)> {
        self.creations.lock().unwrap().clone()
    }

    pub(crate) fn shutdowns(&self) -> usize {
        self.shutdowns.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl SessionRuntime for ScriptedRuntime {
    async fn create_terminal(
        &self,
        session_id: uuid::Uuid,
        rows: u16,
        cols: u16,
        _spec: LaunchSpec,
    ) -> Result<Box<dyn TerminalSession>, CreateTerminalError> {
        self.creations
            .lock()
            .unwrap()
            .push((session_id, rows, cols));
        self.terminals
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| {
                Err(CreateTerminalError::LaunchRegistration(anyhow::anyhow!(
                    "nothing scripted a terminal for session {session_id}"
                )))
            })
    }

    async fn probe_request_path(&self) -> Result<BTreeSet<String>, ControlPlaneFault> {
        self.control_plane
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(Err(ControlPlaneFault::Unreachable))
    }

    async fn probe_launch_broker(&self) -> crate::launch_broker::BrokerProbe {
        self.broker_probe.lock().unwrap().clone().unwrap_or(
            crate::launch_broker::BrokerProbe::ConnectFailed(
                "nothing scripted a launch-broker probe".to_owned(),
            ),
        )
    }

    fn launch_broker_is_accepting(&self) -> bool {
        self.broker_accepting
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    fn operation_timeout(&self) -> Duration {
        self.operation_timeout
    }

    fn rmux_socket(&self) -> &Path {
        &self.rmux_socket
    }

    fn runtime_dir(&self) -> &Path {
        self.runtime_dir.path()
    }

    async fn shutdown(&self) -> Result<()> {
        self.shutdowns
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        match self.shutdown_error.lock().unwrap().clone() {
            Some(message) => bail!("{message}"),
            None => Ok(()),
        }
    }
}

fn require_successful_sidecar_exit(status: std::process::ExitStatus) -> Result<()> {
    if status.success() {
        Ok(())
    } else {
        bail!("private rmux sidecar exited unsuccessfully: {status}")
    }
}

async fn stop_failed_sidecar(sidecar: &mut Child, timeout: Duration) {
    // Startup errors still own an exact child handle. Kill and boundedly wait
    // here instead of relying on Child drop/background reaping.
    let _ = sidecar.start_kill();
    let _ = tokio::time::timeout(timeout, sidecar.wait()).await;
}

impl Drop for PrivateRuntime {
    fn drop(&mut self) {
        // Best-effort asynchronous shutdown: closing the unique owner-pipe
        // writer asks the sidecar to tear down rmux before exiting. Explicit
        // startup-failure and timeout paths still force-kill their child.
        drop(self.sidecar.get_mut().stdin.take());
    }
}

#[derive(Deserialize)]
struct ReadyRecord {
    kind: String,
    rmux_version: String,
    socket: PathBuf,
}

async fn wait_for_ready(
    sidecar: &mut Child,
    expected_socket: &Path,
    timeout: Duration,
) -> Result<()> {
    let stdout = sidecar
        .stdout
        .take()
        .context("pmux-rmuxd stdout unavailable")?;
    let mut line = String::new();
    let bytes = tokio::time::timeout(timeout, BufReader::new(stdout).read_line(&mut line))
        .await
        .context("timed out waiting for private rmux readiness")??;
    if bytes == 0 {
        let status = sidecar.try_wait()?;
        bail!("private rmux sidecar exited before readiness: {status:?}");
    }
    let record: ReadyRecord = serde_json::from_str(line.trim())
        .context("private rmux sidecar emitted invalid readiness data")?;
    if record.kind != "pmux-rmuxd-ready" || record.rmux_version != PINNED_RMUX_VERSION {
        bail!(
            "private rmux version mismatch: expected {}, received {}",
            PINNED_RMUX_VERSION,
            record.rmux_version
        );
    }
    if record.socket != expected_socket {
        bail!(
            "private rmux readiness endpoint mismatch: expected {}, received {}",
            expected_socket.display(),
            record.socket.display()
        );
    }
    Ok(())
}

fn companion_binary(directory: &Path, name: &str) -> PathBuf {
    #[cfg(windows)]
    let name = format!("{name}.exe");
    directory.join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn companion_path_is_scoped_to_binary_directory() {
        let path = companion_binary(Path::new("/opt/pmux/bin"), "pmux-launcher");
        assert!(path.starts_with("/opt/pmux/bin"));
        assert!(
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("pmux-launcher")
        );
    }

    #[test]
    fn an_already_exited_failed_sidecar_is_not_cleanup_proof() {
        let success = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .status()
            .unwrap();
        assert!(require_successful_sidecar_exit(success).is_ok());

        let failed = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 17"])
            .status()
            .unwrap();
        let error = require_successful_sidecar_exit(failed).unwrap_err();
        assert!(error.to_string().contains("unsuccessfully"));
    }
}
