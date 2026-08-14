use std::collections::{HashMap, HashSet};
use std::fs::Metadata;
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::Parser;
use pseudomux_rmux::{
    OWNER_GRACEFUL_SHUTDOWN_FRAME, OwnedProcessBoundary, PROCESS_OBSERVATION_POLL_INTERVAL,
    try_reap_exited_child,
};
use rmux_sdk::{PaneProcessState, Rmux};
use rmux_server::{DaemonConfig, ServerDaemon};
use tokio::io::AsyncReadExt;

const PINNED_RMUX_VERSION: &str = "0.9.0";
const DEFAULT_SHUTDOWN_TIMEOUT_MS: u64 = 10_000;
const CHILD_REAPER_POLL_INTERVAL: Duration = Duration::from_millis(200);
const STALE_CHILD_GRACE: Duration = Duration::from_secs(2);

#[derive(Debug, Parser)]
#[command(name = "pmux-rmuxd", version, about = "Private rmux sidecar for pmuxd")]
struct Args {
    /// Owner-only local endpoint selected by pmuxd.
    #[arg(long)]
    socket: PathBuf,

    /// Emit one JSON readiness record on stdout after binding.
    #[arg(long)]
    announce_ready: bool,

    /// Exit when the owning pmuxd closes stdin (including on SIGKILL).
    #[arg(long)]
    owner_stdin: bool,

    /// Exact sibling launcher endpoint used to clean a private runtime only
    /// after ungraceful owner loss.
    #[arg(long, requires = "owner_stdin", hide = true)]
    launcher_socket: Option<PathBuf>,

    /// Bound for rmux shutdown and the following process-session reap proof.
    #[arg(long, default_value_t = DEFAULT_SHUTDOWN_TIMEOUT_MS, hide = true)]
    shutdown_timeout_ms: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if args.shutdown_timeout_ms == 0 {
        bail!("--shutdown-timeout-ms must be non-zero");
    }
    validate_socket_parent(&args.socket)?;
    let mut owner_cleanup = args
        .launcher_socket
        .as_deref()
        .map(|launcher| OwnerRuntimeCleanup::capture(&args.socket, launcher))
        .transpose()?;

    // DaemonConfig::new deliberately leaves user config and web sharing disabled.
    // pmuxd chooses a private endpoint and always connects to it explicitly.
    let server = ServerDaemon::new(DaemonConfig::new(args.socket.clone()))
        .bind()
        .await
        .with_context(|| {
            format!(
                "failed to bind private rmux endpoint {}",
                args.socket.display()
            )
        })?;
    if let Some(cleanup) = owner_cleanup.as_mut() {
        cleanup.capture_rmux_socket(&args.socket)?;
    }

    if args.announce_ready {
        println!(
            "{}",
            serde_json::json!({
                "kind": "pmux-rmuxd-ready",
                "rmux_version": PINNED_RMUX_VERSION,
                "socket": server.socket_path(),
            })
        );
    }

    if args.owner_stdin {
        let child_reaper = tokio::spawn(reap_stale_untracked_children(args.socket.clone()));
        let owner_exit = wait_for_owner_exit().await?;
        let shutdown_timeout = Duration::from_millis(args.shutdown_timeout_ms);
        shutdown_after_owner_exit(server, &args.socket, shutdown_timeout, child_reaper).await?;
        if owner_exit == OwnerExit::Lost
            && let Some(cleanup) = owner_cleanup
        {
            cleanup.remove_after_owner_loss().await?;
        }
        Ok(())
    } else {
        server.wait().await.context("private rmux daemon failed")
    }
}

use std::time::Duration;

async fn shutdown_after_owner_exit(
    server: rmux_server::ServerHandle,
    socket: &Path,
    timeout: Duration,
    child_reaper: tokio::task::JoinHandle<()>,
) -> Result<()> {
    // Capture every pane's stable POSIX session while the control plane still
    // exposes its process identity. This sidecar survives an owning pmuxd
    // SIGKILL and is therefore the last ordinary cleanup authority.
    let mut boundaries = capture_process_boundaries(socket, timeout).await;
    // Keep the established periodic observer alive until the final boundary
    // snapshot has completed. This avoids creating an observer-free gap
    // immediately after owner EOF.
    let shutdown_result = match boundaries.as_mut() {
        Ok(boundaries) => observe_server_shutdown(server, boundaries, timeout).await,
        Err(_) => tokio::time::timeout(timeout, server.shutdown())
            .await
            .context("timed out shutting down private rmux daemon")?
            .context("failed to shut down private rmux daemon"),
    };
    child_reaper.abort();
    let _ = child_reaper.await;

    let reap_result = match boundaries {
        // Once ServerHandle::shutdown has completed, rmux no longer owns the
        // direct child wait handles. Keep collecting exited direct children
        // while the POSIX-boundary fallback terminates any survivors. This is
        // required on platforms where an exited pane can otherwise remain as
        // a sidecar-owned zombie after rmux's background teardown completes.
        Ok(boundaries) if shutdown_result.is_ok() => {
            reap_boundaries_after_server_shutdown(boundaries, timeout).await
        }
        Ok(boundaries) => reap_boundaries(boundaries, timeout).await,
        Err(error) => Err(error),
    };
    shutdown_result.context("failed to shut down private rmux daemon after owner exit")?;
    reap_result.context("failed to confirm private pane process cleanup after owner exit")
}

async fn reap_boundaries_after_server_shutdown(
    boundaries: Vec<OwnedProcessBoundary>,
    timeout: Duration,
) -> Result<()> {
    let parent_pid = std::process::id();
    let mut child_reaper = tokio::spawn(async move {
        let mut interval = tokio::time::interval(PROCESS_OBSERVATION_POLL_INTERVAL);
        loop {
            interval.tick().await;
            let children = tokio::task::spawn_blocking(move || direct_child_pids(parent_pid))
                .await
                .context("post-shutdown direct-child observer task failed")?
                .context("post-shutdown direct-child observation failed")?;
            for pid in children {
                try_reap_exited_child(pid)
                    .with_context(|| format!("failed to collect exited direct child {pid}"))?;
            }
        }
        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    });
    let boundary_reap = reap_boundaries(boundaries, timeout);
    tokio::pin!(boundary_reap);
    let result = tokio::select! {
        result = &mut boundary_reap => result,
        result = &mut child_reaper => {
            result.context("post-shutdown direct-child reaper task failed")??;
            bail!("post-shutdown direct-child reaper stopped unexpectedly")
        }
    };
    child_reaper.abort();
    let _ = child_reaper.await;
    result
}

async fn capture_process_boundaries(
    socket: &Path,
    timeout: Duration,
) -> Result<Vec<OwnedProcessBoundary>> {
    let rmux = Rmux::builder()
        .unix_socket(socket)
        .default_timeout(timeout)
        .connect()
        .await
        .context("failed to connect cleanup observer to private rmux daemon")?;
    let panes = rmux
        .find_panes()
        .all()
        .await
        .context("failed to enumerate private rmux panes before shutdown")?;
    let mut boundaries = Vec::with_capacity(panes.len());
    for pane in panes {
        let pid = match pane.process {
            PaneProcessState::Running { pid: Some(pid) } => pid,
            PaneProcessState::Exited => continue,
            PaneProcessState::Running { pid: None } | PaneProcessState::Unknown => {
                bail!(
                    "private pane {} had no observable process identity before shutdown",
                    pane.pane_id.as_u32()
                )
            }
            _ => bail!(
                "private pane {} reported an unsupported process state before shutdown",
                pane.pane_id.as_u32()
            ),
        };
        let mut boundary = OwnedProcessBoundary::capture(pid)
            .context("private pane process boundary capture failed")?
            .context("private pane process exited during boundary capture")?;
        // Seed descendant tracking before teardown begins.
        boundary
            .observe()
            .await
            .context("private pane descendant capture failed")?;
        if !boundaries
            .iter()
            .any(|known: &OwnedProcessBoundary| known.session_id() == boundary.session_id())
        {
            boundaries.push(boundary);
        }
    }
    Ok(boundaries)
}

async fn observe_server_shutdown(
    server: rmux_server::ServerHandle,
    boundaries: &mut [OwnedProcessBoundary],
    timeout: Duration,
) -> Result<()> {
    let shutdown = server.shutdown();
    tokio::pin!(shutdown);
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        tokio::select! {
            result = &mut shutdown => {
                return result.context("private rmux server shutdown failed");
            }
            () = tokio::time::sleep(PROCESS_OBSERVATION_POLL_INTERVAL) => {
                for boundary in boundaries.iter_mut() {
                    boundary
                        .observe()
                        .await
                        .context("process observation failed during private rmux shutdown")?;
                }
                if tokio::time::Instant::now() >= deadline {
                    bail!("private rmux server shutdown exceeded its deadline");
                }
            }
        }
    }
}

async fn reap_boundaries(boundaries: Vec<OwnedProcessBoundary>, timeout: Duration) -> Result<()> {
    let mut tasks = tokio::task::JoinSet::new();
    for mut boundary in boundaries {
        tasks.spawn(async move {
            let session_id = boundary.session_id();
            let reaped = boundary.force_reap(timeout).await?;
            Ok::<_, pseudomux_rmux::ProcessBoundaryError>((session_id, reaped))
        });
    }
    while let Some(result) = tasks.join_next().await {
        let (session_id, reaped) = result
            .context("private process reaper task failed")?
            .context("private process boundary observation failed")?;
        if !reaped {
            bail!(
                "POSIX session {session_id} was not positively reaped; an observed escape or surviving member invalidated cleanup proof"
            );
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OwnerExit {
    Graceful,
    Lost,
}

async fn wait_for_owner_exit() -> Result<OwnerExit> {
    let mut stdin = tokio::io::stdin();
    let mut buffer = [0_u8; 64];
    let mut payload = Vec::with_capacity(OWNER_GRACEFUL_SHUTDOWN_FRAME.len());
    let mut overflowed = false;
    loop {
        let read = stdin
            .read(&mut buffer)
            .await
            .context("failed to monitor owner pipe")?;
        if read == 0 {
            return Ok(classify_owner_exit(&payload, overflowed));
        }
        if payload.len() + read <= OWNER_GRACEFUL_SHUTDOWN_FRAME.len() {
            payload.extend_from_slice(&buffer[..read]);
        } else {
            overflowed = true;
        }
    }
}

fn classify_owner_exit(payload: &[u8], overflowed: bool) -> OwnerExit {
    if !overflowed && payload == OWNER_GRACEFUL_SHUTDOWN_FRAME {
        OwnerExit::Graceful
    } else {
        OwnerExit::Lost
    }
}

/// rmux 0.9 terminates removed pane processes on a background thread. On some
/// Unix exits (observed on macOS), that bounded thread can drop the final child
/// handle without collecting a zombie. Reap only direct children that have not
/// been advertised by any live pane for a full grace window. `waitpid(WNOHANG)`
/// never affects a still-running process, while the advertisement check avoids
/// stealing an exit status from rmux's ordinary lifecycle path.
async fn reap_stale_untracked_children(socket: PathBuf) {
    let rmux = loop {
        match Rmux::builder()
            .unix_socket(&socket)
            .default_timeout(Duration::from_secs(1))
            .connect()
            .await
        {
            Ok(rmux) => break rmux,
            Err(_) => tokio::time::sleep(CHILD_REAPER_POLL_INTERVAL).await,
        }
    };
    let parent_pid = std::process::id();
    let mut first_untracked = HashMap::<i32, tokio::time::Instant>::new();
    let mut interval = tokio::time::interval(CHILD_REAPER_POLL_INTERVAL);

    loop {
        interval.tick().await;
        let panes = match rmux.find_panes().all().await {
            Ok(panes) => panes,
            Err(_) => continue,
        };
        let active_pids = panes
            .into_iter()
            .filter_map(|pane| match pane.process {
                PaneProcessState::Running { pid: Some(pid) } => i32::try_from(pid).ok(),
                _ => None,
            })
            .collect::<HashSet<_>>();
        let children =
            match tokio::task::spawn_blocking(move || direct_child_pids(parent_pid)).await {
                Ok(Ok(children)) => children,
                Ok(Err(_)) | Err(_) => continue,
            };
        let now = tokio::time::Instant::now();
        first_untracked.retain(|pid, _| children.contains(pid) && !active_pids.contains(pid));

        for pid in children {
            if active_pids.contains(&pid) {
                first_untracked.remove(&pid);
                continue;
            }
            let first_seen = first_untracked.entry(pid).or_insert(now);
            if now.duration_since(*first_seen) >= STALE_CHILD_GRACE {
                let _ = try_reap_exited_child(pid);
            }
        }
    }
}

fn direct_child_pids(parent_pid: u32) -> std::io::Result<HashSet<i32>> {
    let output = Command::new("/bin/ps")
        .args(["-axo", "pid=,ppid="])
        .output()?;
    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "/bin/ps exited with {}",
            output.status
        )));
    }
    let output = String::from_utf8(output.stdout)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let parent_pid = i32::try_from(parent_pid)
        .map_err(|_| std::io::Error::other("sidecar PID is out of range"))?;
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(parse_process_row)
        .collect::<std::io::Result<Vec<_>>>()
        .map(|rows| {
            rows.into_iter()
                .filter_map(|(pid, ppid)| (ppid == parent_pid).then_some(pid))
                .collect()
        })
}

fn parse_process_row(line: &str) -> std::io::Result<(i32, i32)> {
    let mut fields = line.split_whitespace();
    let pid = fields.next().and_then(|value| value.parse::<i32>().ok());
    let ppid = fields.next().and_then(|value| value.parse::<i32>().ok());
    if fields.next().is_some() || pid.is_none_or(|pid| pid <= 0) || ppid.is_none_or(|pid| pid < 0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("unrecognized /bin/ps process row: {line:?}"),
        ));
    }
    Ok((
        pid.expect("positive pid was checked"),
        ppid.expect("non-negative ppid was checked"),
    ))
}

fn validate_socket_parent(socket: &Path) -> Result<()> {
    if !socket.is_absolute() {
        bail!("--socket must be an absolute path");
    }
    let parent = socket
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .context("--socket must have a parent directory")?;
    let metadata = std::fs::metadata(parent)
        .with_context(|| format!("socket parent {} does not exist", parent.display()))?;
    if !metadata.is_dir() {
        bail!("socket parent {} is not a directory", parent.display());
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mode = metadata.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            bail!(
                "socket parent {} must not be accessible by group or other users (mode {mode:o})",
                parent.display()
            );
        }
    }

    Ok(())
}

const MAX_OWNER_CLEANUP_NODES: usize = 4_096;
const MAX_OWNER_CLEANUP_DEPTH: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OwnedNodeKind {
    Directory,
    File,
    Socket,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OwnedNodeIdentity {
    device: u64,
    inode: u64,
    owner: u32,
    mode: u32,
    kind: OwnedNodeKind,
}

impl OwnedNodeIdentity {
    fn capture(path: &Path) -> Result<Self> {
        let metadata = std::fs::symlink_metadata(path).with_context(|| {
            format!(
                "could not inspect owned runtime artifact {}",
                path.display()
            )
        })?;
        Self::from_metadata(path, &metadata)
    }

    fn from_metadata(path: &Path, metadata: &Metadata) -> Result<Self> {
        let file_type = metadata.file_type();
        let kind = if file_type.is_dir() {
            OwnedNodeKind::Directory
        } else if file_type.is_file() {
            OwnedNodeKind::File
        } else if file_type.is_socket() {
            OwnedNodeKind::Socket
        } else {
            bail!(
                "owned runtime contained an unsupported artifact type at {}",
                path.display()
            );
        };
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            owner: metadata.uid(),
            mode: metadata.permissions().mode() & 0o7777,
            kind,
        })
    }

    fn require_at(self, path: &Path) -> Result<()> {
        let observed = Self::capture(path)?;
        if observed != self {
            bail!(
                "owned runtime artifact identity changed before cleanup at {}",
                path.display()
            );
        }
        Ok(())
    }
}

struct OwnerRuntimeCleanup {
    runtime_dir: PathBuf,
    runtime_identity: OwnedNodeIdentity,
    launcher_socket: PathBuf,
    launcher_identity: OwnedNodeIdentity,
    rmux_socket: PathBuf,
    rmux_identity: Option<OwnedNodeIdentity>,
}

impl OwnerRuntimeCleanup {
    fn capture(rmux_socket: &Path, launcher_socket: &Path) -> Result<Self> {
        if !rmux_socket.is_absolute() || !launcher_socket.is_absolute() {
            bail!("owner cleanup endpoints must be absolute");
        }
        let runtime_dir = rmux_socket
            .parent()
            .context("private rmux socket has no runtime directory")?;
        if launcher_socket.parent() != Some(runtime_dir) {
            bail!("private rmux and launcher sockets must share one runtime directory");
        }
        let runtime_identity = OwnedNodeIdentity::capture(runtime_dir)?;
        if runtime_identity.kind != OwnedNodeKind::Directory {
            bail!("private runtime identity is not a directory");
        }
        let mode = std::fs::symlink_metadata(runtime_dir)?.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            bail!("private runtime must remain inaccessible to group and other users");
        }
        let launcher_identity = OwnedNodeIdentity::capture(launcher_socket)?;
        if launcher_identity.kind != OwnedNodeKind::Socket
            || launcher_identity.owner != runtime_identity.owner
            || launcher_identity.device != runtime_identity.device
        {
            bail!("launcher endpoint is outside the exact private runtime identity");
        }
        Ok(Self {
            runtime_dir: runtime_dir.to_path_buf(),
            runtime_identity,
            launcher_socket: launcher_socket.to_path_buf(),
            launcher_identity,
            rmux_socket: rmux_socket.to_path_buf(),
            rmux_identity: None,
        })
    }

    fn capture_rmux_socket(&mut self, rmux_socket: &Path) -> Result<()> {
        if rmux_socket.parent() != Some(self.runtime_dir.as_path()) {
            bail!("bound rmux endpoint changed private runtime directory");
        }
        let identity = OwnedNodeIdentity::capture(rmux_socket)?;
        if identity.kind != OwnedNodeKind::Socket
            || identity.owner != self.runtime_identity.owner
            || identity.device != self.runtime_identity.device
        {
            bail!("bound rmux endpoint is outside the exact private runtime identity");
        }
        self.rmux_identity = Some(identity);
        Ok(())
    }

    async fn remove_after_owner_loss(self) -> Result<()> {
        match std::fs::symlink_metadata(&self.runtime_dir) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error).context("could not re-inspect private runtime"),
            Ok(_) => self.runtime_identity.require_at(&self.runtime_dir)?,
        }

        match std::fs::symlink_metadata(&self.launcher_socket) {
            Ok(_) => {
                self.launcher_identity.require_at(&self.launcher_socket)?;
                match tokio::time::timeout(
                    Duration::from_millis(250),
                    tokio::net::UnixStream::connect(&self.launcher_socket),
                )
                .await
                {
                    Ok(Ok(_)) => {
                        bail!("launcher endpoint still had a live listener after owner loss")
                    }
                    Ok(Err(error))
                        if matches!(
                            error.kind(),
                            io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                        ) => {}
                    Ok(Err(error)) => {
                        return Err(error)
                            .context("launcher endpoint liveness was ambiguous after owner loss");
                    }
                    Err(_) => bail!("launcher endpoint liveness check timed out after owner loss"),
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("could not re-inspect launcher endpoint"),
        }
        if let Some(rmux_identity) = self.rmux_identity {
            match std::fs::symlink_metadata(&self.rmux_socket) {
                Ok(_) => rmux_identity.require_at(&self.rmux_socket)?,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).context("could not re-inspect rmux endpoint"),
            }
        }

        // Preflight the complete exact tree before deleting any node. This
        // deliberately rejects symlinks, devices, cross-device mounts,
        // ownership changes, permissive modes, excessive depth, and excessive
        // node counts. Every deletion is then identity-revalidated and uses
        // remove_file/remove_dir only; no recursive broad removal occurs.
        let mut removal = Vec::new();
        collect_owned_tree(&self.runtime_dir, self.runtime_identity, 0, &mut removal)?;
        for node in removal {
            node.identity.require_at(&node.path)?;
            match node.identity.kind {
                OwnedNodeKind::Directory => std::fs::remove_dir(&node.path),
                OwnedNodeKind::File | OwnedNodeKind::Socket => std::fs::remove_file(&node.path),
            }
            .with_context(|| {
                format!(
                    "failed to remove exact runtime artifact {}",
                    node.path.display()
                )
            })?;
        }
        Ok(())
    }
}

struct RemovalNode {
    path: PathBuf,
    identity: OwnedNodeIdentity,
}

fn collect_owned_tree(
    directory: &Path,
    expected: OwnedNodeIdentity,
    depth: usize,
    removal: &mut Vec<RemovalNode>,
) -> Result<()> {
    if depth > MAX_OWNER_CLEANUP_DEPTH {
        bail!("private runtime cleanup depth exceeded its bound");
    }
    expected.require_at(directory)?;
    let mut entries = std::fs::read_dir(directory)
        .context("could not enumerate exact private runtime")?
        .collect::<io::Result<Vec<_>>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        if removal.len() >= MAX_OWNER_CLEANUP_NODES {
            bail!("private runtime cleanup node count exceeded its bound");
        }
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        let identity = OwnedNodeIdentity::from_metadata(&path, &metadata)?;
        if identity.owner != expected.owner || identity.device != expected.device {
            bail!("private runtime artifact crossed its owner/device boundary");
        }
        if metadata.permissions().mode() & 0o077 != 0 {
            bail!("private runtime artifact became accessible to group or other users");
        }
        if identity.kind == OwnedNodeKind::Directory {
            collect_owned_tree(&path, identity, depth + 1, removal)?;
        } else {
            removal.push(RemovalNode { path, identity });
        }
    }
    removal.push(RemovalNode {
        path: directory.to_path_buf(),
        identity: expected,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UnixListener;

    #[test]
    fn relative_socket_is_rejected() {
        let error = validate_socket_parent(Path::new("relative/rmux.sock")).unwrap_err();
        assert!(error.to_string().contains("absolute"));
    }

    #[test]
    fn private_temp_parent_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        validate_socket_parent(&dir.path().join("rmux.sock")).unwrap();
    }

    #[test]
    fn process_rows_are_parsed_strictly() {
        assert_eq!(parse_process_row(" 123  42").unwrap(), (123, 42));
        assert!(parse_process_row("123").is_err());
        assert!(parse_process_row("123 42 extra").is_err());
        assert!(parse_process_row("0 42").is_err());
        assert!(parse_process_row("123 -1").is_err());
    }

    #[test]
    fn graceful_owner_frame_is_exact_and_bare_eof_is_loss() {
        assert_eq!(
            classify_owner_exit(OWNER_GRACEFUL_SHUTDOWN_FRAME, false),
            OwnerExit::Graceful
        );
        assert_eq!(classify_owner_exit(&[], false), OwnerExit::Lost);
        assert_eq!(
            classify_owner_exit(b"pmux-rmux-owner-graceful-v1", false),
            OwnerExit::Lost
        );
        assert_eq!(
            classify_owner_exit(OWNER_GRACEFUL_SHUTDOWN_FRAME, true),
            OwnerExit::Lost
        );
    }

    #[tokio::test]
    async fn exact_owner_runtime_cleanup_removes_only_the_captured_tree() {
        let parent = tempfile::tempdir().unwrap();
        let runtime = parent.path().join("pmux-exact");
        std::fs::create_dir(&runtime).unwrap();
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
        let launcher_path = runtime.join("launcher.sock");
        let rmux_path = runtime.join("rmux.sock");
        let launcher = UnixListener::bind(&launcher_path).unwrap();
        std::fs::set_permissions(&launcher_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let mut cleanup = OwnerRuntimeCleanup::capture(&rmux_path, &launcher_path).unwrap();
        let rmux = UnixListener::bind(&rmux_path).unwrap();
        std::fs::set_permissions(&rmux_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        cleanup.capture_rmux_socket(&rmux_path).unwrap();
        let sensitive = runtime.join("launch-test-random");
        std::fs::create_dir(&sensitive).unwrap();
        std::fs::set_permissions(&sensitive, std::fs::Permissions::from_mode(0o700)).unwrap();
        let material = sensitive.join("settings-0000.json");
        std::fs::write(&material, b"private").unwrap();
        std::fs::set_permissions(&material, std::fs::Permissions::from_mode(0o600)).unwrap();
        drop(rmux);
        std::fs::remove_file(&rmux_path).unwrap();
        drop(launcher);

        cleanup.remove_after_owner_loss().await.unwrap();
        assert!(!runtime.exists());
        assert!(parent.path().exists());
    }

    #[tokio::test]
    async fn owner_runtime_cleanup_preserves_identity_replacement_for_diagnostics() {
        let parent = tempfile::tempdir().unwrap();
        let runtime = parent.path().join("pmux-replaced");
        std::fs::create_dir(&runtime).unwrap();
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
        let launcher_path = runtime.join("launcher.sock");
        let rmux_path = runtime.join("rmux.sock");
        let launcher = UnixListener::bind(&launcher_path).unwrap();
        std::fs::set_permissions(&launcher_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let cleanup = OwnerRuntimeCleanup::capture(&rmux_path, &launcher_path).unwrap();
        drop(launcher);
        std::fs::remove_file(&launcher_path).unwrap();
        std::fs::write(&launcher_path, b"do-not-remove").unwrap();
        std::fs::set_permissions(&launcher_path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let error = cleanup.remove_after_owner_loss().await.unwrap_err();
        assert!(error.to_string().contains("identity changed"));
        assert_eq!(std::fs::read(&launcher_path).unwrap(), b"do-not-remove");
        assert!(runtime.exists());
    }
}
