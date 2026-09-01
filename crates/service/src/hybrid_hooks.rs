//! Opt-in Claude lifecycle hooks and their owner-only local relay.
//!
//! Transcript mode never calls into this module. Hybrid mode composes the three
//! lifecycle hooks that can corroborate session state, while transcript parsing
//! remains the sole output and usage authority.

use std::fmt;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use pseudomux_protocol::v1::{ConfigSource, LifecycleMode, MAX_SAFE_JSON_INTEGER};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Semaphore, mpsc, oneshot};
use tokio::task::{JoinHandle, JoinSet};
use uuid::Uuid;

pub const HOOK_RELAY_PROTOCOL_VERSION: u16 = 1;
pub const MAX_HOOK_FRAME_BYTES: usize = 64 * 1024;
pub const MAX_SETTINGS_BYTES: usize = 1024 * 1024;
const MAX_HOOK_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const MAX_RELAY_CONNECTIONS: usize = 16;

/// The only hook events pmux adds in Hybrid mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LifecycleEventKind {
    SessionStart,
    Stop,
    StopFailure,
}

impl LifecycleEventKind {
    const ALL: [Self; 3] = [Self::SessionStart, Self::Stop, Self::StopFailure];

    const fn as_str(self) -> &'static str {
        match self {
            Self::SessionStart => "SessionStart",
            Self::Stop => "Stop",
            Self::StopFailure => "StopFailure",
        }
    }
}

/// Corroborating lifecycle data. It deliberately has no output, content, or usage fields.
#[derive(Clone, PartialEq, Eq)]
pub struct LifecycleObservation {
    sequence: u64,
    session_id: Uuid,
    event: LifecycleEventKind,
    transcript_path: Option<PathBuf>,
    failure_observed: bool,
}

impl LifecycleObservation {
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    pub const fn session_id(&self) -> Uuid {
        self.session_id
    }

    pub const fn event(&self) -> LifecycleEventKind {
        self.event
    }

    pub fn transcript_path(&self) -> Option<&Path> {
        self.transcript_path.as_deref()
    }

    pub const fn failure_observed(&self) -> bool {
        self.failure_observed
    }
}

impl fmt::Debug for LifecycleObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LifecycleObservation")
            .field("sequence", &self.sequence)
            .field("event", &self.event)
            .field("session_id", &self.session_id)
            .field("has_transcript_path", &self.transcript_path.is_some())
            .field("failure_observed", &self.failure_observed)
            .finish()
    }
}

/// Lifecycle preparation result. Transcript mode retains caller settings unchanged.
pub enum PreparedLifecycle {
    Transcript,
    Hybrid(HybridLifecycle),
}

impl PreparedLifecycle {
    /// Returns the settings arguments the Claude launch must use.
    ///
    /// Hybrid mode returns only the generated semantic composition so caller
    /// documents and injected hooks are each applied exactly once.
    pub fn launch_settings(&self, caller_settings: &[ConfigSource]) -> Vec<ConfigSource> {
        match self {
            Self::Transcript => caller_settings.to_vec(),
            Self::Hybrid(hybrid) => vec![ConfigSource::File {
                path: hybrid.settings_path.to_string_lossy().into_owned(),
            }],
        }
    }

    pub fn hybrid(&self) -> Option<&HybridLifecycle> {
        match self {
            Self::Transcript => None,
            Self::Hybrid(hybrid) => Some(hybrid),
        }
    }

    pub fn hybrid_mut(&mut self) -> Option<&mut HybridLifecycle> {
        match self {
            Self::Transcript => None,
            Self::Hybrid(hybrid) => Some(hybrid),
        }
    }

    pub fn into_hybrid(self) -> Option<HybridLifecycle> {
        match self {
            Self::Transcript => None,
            Self::Hybrid(hybrid) => Some(hybrid),
        }
    }
}

impl fmt::Debug for PreparedLifecycle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transcript => formatter.write_str("PreparedLifecycle::Transcript"),
            Self::Hybrid(_) => formatter.write_str("PreparedLifecycle::Hybrid(<redacted>)"),
        }
    }
}

/// Running owner-only lifecycle relay plus its generated settings file.
pub struct HybridLifecycle {
    session_id: Uuid,
    settings_path: PathBuf,
    settings_identity: FileIdentity,
    socket_path: PathBuf,
    socket_identity: FileIdentity,
    receiver: mpsc::Receiver<LifecycleObservation>,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

impl HybridLifecycle {
    pub const fn session_id(&self) -> Uuid {
        self.session_id
    }

    pub fn settings_path(&self) -> &Path {
        &self.settings_path
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub async fn recv(&mut self) -> Option<LifecycleObservation> {
        self.receiver.recv().await
    }

    pub fn try_recv(
        &mut self,
    ) -> std::result::Result<LifecycleObservation, mpsc::error::TryRecvError> {
        self.receiver.try_recv()
    }

    /// Cooperatively closes the listener, aborts and joins every accepted
    /// connection, and returns only after the relay task is quiescent.
    pub async fn shutdown(mut self) {
        self.request_shutdown();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }

    fn request_shutdown(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

impl fmt::Debug for HybridLifecycle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HybridLifecycle")
            .field("session_id", &self.session_id)
            .field("settings_path", &"<redacted>")
            .field("socket_path", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl Drop for HybridLifecycle {
    fn drop(&mut self) {
        // Explicit NativeService paths use `shutdown` above. Drop remains a
        // last-resort cancellation boundary for preparation failures or runtime
        // teardown where awaiting is impossible.
        if let Some(task) = self.task.take() {
            task.abort();
        }
        remove_if_same_file(&self.socket_path, &self.socket_identity);
        remove_if_same_file(&self.settings_path, &self.settings_identity);
    }
}

/// Device and inode of an artifact we created, plus, where the platform allows
/// it, an open handle that keeps that inode allocated.
///
/// Inode numbers are recycled: on ext4 a file unlinked and immediately
/// recreated at the same path usually lands on the very same inode, which would
/// make a same-user replacement indistinguishable from our own artifact and get
/// it deleted by cleanup. Holding a handle keeps the kernel from freeing the
/// inode number for as long as we might still act on it.
struct FileIdentity {
    device: u64,
    inode: u64,
    pin: Option<std::fs::File>,
}

impl FileIdentity {
    fn capture(path: &Path) -> Result<Self> {
        let pin = pin_inode(path);
        let metadata = match pin.as_ref() {
            Some(pin) => pin.metadata(),
            None => std::fs::symlink_metadata(path),
        }
        .context("failed to inspect a Hybrid runtime artifact")?;
        Self::new(&metadata, pin)
    }

    fn from_file(file: &std::fs::File) -> Result<Self> {
        let metadata = file
            .metadata()
            .context("failed to inspect a Hybrid runtime artifact")?;
        Self::new(&metadata, file.try_clone().ok())
    }

    fn new(metadata: &std::fs::Metadata, pin: Option<std::fs::File>) -> Result<Self> {
        ensure!(
            metadata.uid() == effective_uid(),
            "Hybrid runtime artifact is owned by another user"
        );
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            pin,
        })
    }

    fn matches(&self, metadata: &std::fs::Metadata) -> bool {
        let (device, inode) = match self.pin.as_ref().and_then(|pin| pin.metadata().ok()) {
            Some(pinned) => (pinned.dev(), pinned.ino()),
            None => (self.device, self.inode),
        };
        metadata.dev() == device && metadata.ino() == inode
    }
}

/// Opens a handle that pins `path`'s inode without opening the file for I/O.
///
/// `O_PATH` is Linux-only, and a bound Unix socket cannot be opened for I/O
/// anywhere, so platforms without it fall back to the recorded device and
/// inode alone.
#[cfg(target_os = "linux")]
fn pin_inode(path: &Path) -> Option<std::fs::File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_NOFOLLOW)
        .open(path)
        .ok()
}

#[cfg(not(target_os = "linux"))]
fn pin_inode(_path: &Path) -> Option<std::fs::File> {
    None
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HookRelayEnvelope {
    version: u16,
    event: LifecycleEventKind,
    session_id: Uuid,
    payload: Value,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HookRelayResponse {
    version: u16,
    accepted: bool,
    code: String,
}

/// Prepares transcript-only or opt-in Hybrid lifecycle behavior.
///
/// `hook_client` is an absolute executable implementing the command contract
/// emitted into the settings document. It receives `--socket`, `--session-id`,
/// and `--event`; Claude's hook JSON remains on stdin.
pub async fn prepare_lifecycle(
    mode: &LifecycleMode,
    runtime_dir: &Path,
    session_id: Uuid,
    hook_client: &Path,
    caller_settings: &[ConfigSource],
) -> Result<PreparedLifecycle> {
    let LifecycleMode::Hybrid { hook_timeout_ms } = mode else {
        return Ok(PreparedLifecycle::Transcript);
    };

    let hook_timeout = Duration::from_millis(*hook_timeout_ms);
    ensure!(
        !hook_timeout.is_zero() && hook_timeout <= MAX_HOOK_TIMEOUT,
        "Hybrid hook timeout is outside the supported range"
    );
    validate_private_runtime_dir(runtime_dir)?;
    validate_hook_client(hook_client)?;

    let compact_session_id = session_id.simple().to_string();
    let artifact_key = &compact_session_id[..16];
    let socket_path = runtime_dir.join(format!("hh-{artifact_key}.sock"));
    let settings_path = runtime_dir.join(format!("hh-{artifact_key}.json"));
    ensure_path_absent(&socket_path, "Hybrid relay socket")?;
    ensure_path_absent(&settings_path, "Hybrid settings file")?;

    let mut document = compose_caller_settings(caller_settings)?;
    append_lifecycle_hooks(
        &mut document,
        hook_client,
        &socket_path,
        session_id,
        hook_timeout,
    )?;
    let encoded = serde_json::to_vec(&document).context("failed to encode Hybrid settings")?;
    ensure!(
        encoded.len() <= MAX_SETTINGS_BYTES,
        "composed Hybrid settings exceed the size limit"
    );

    let listener = UnixListener::bind(&socket_path).context("failed to bind Hybrid hook relay")?;
    if let Err(error) =
        std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))
    {
        remove_owned_file(&socket_path);
        return Err(error).context("failed to secure Hybrid hook relay");
    }
    let socket_identity = match FileIdentity::capture(&socket_path) {
        Ok(identity) => identity,
        Err(error) => {
            remove_owned_file(&socket_path);
            return Err(error);
        }
    };
    let settings_identity = match write_private_file(&settings_path, &encoded) {
        Ok(identity) => identity,
        Err(error) => {
            remove_if_same_file(&socket_path, &socket_identity);
            remove_owned_file(&settings_path);
            return Err(error);
        }
    };

    let (sender, receiver) = mpsc::channel(32);
    let sequence = Arc::new(AtomicU64::new(0));
    let semaphore = Arc::new(Semaphore::new(MAX_RELAY_CONNECTIONS));
    let (shutdown, shutdown_requested) = oneshot::channel();
    let task = tokio::spawn(run_relay(
        listener,
        session_id,
        sender,
        sequence,
        semaphore,
        hook_timeout,
        shutdown_requested,
    ));

    Ok(PreparedLifecycle::Hybrid(HybridLifecycle {
        session_id,
        settings_path,
        settings_identity,
        socket_path,
        socket_identity,
        receiver,
        shutdown: Some(shutdown),
        task: Some(task),
    }))
}

/// Sends one parsed Claude hook payload to a prepared relay.
///
/// This is the implementation primitive for the small hook-client executable;
/// it is not a turn result or transcript transport.
pub async fn send_hook_payload(
    socket: &Path,
    session_id: Uuid,
    event: LifecycleEventKind,
    payload: Value,
) -> Result<()> {
    let envelope = HookRelayEnvelope {
        version: HOOK_RELAY_PROTOCOL_VERSION,
        event,
        session_id,
        payload,
    };
    let mut stream = UnixStream::connect(socket)
        .await
        .context("failed to connect to Hybrid hook relay")?;
    write_frame(&mut stream, &envelope).await?;
    let response: HookRelayResponse = read_frame(&mut stream).await?;
    ensure!(
        response.version == HOOK_RELAY_PROTOCOL_VERSION,
        "Hybrid hook relay returned an unsupported version"
    );
    ensure!(response.accepted, "Hybrid hook relay rejected the event");
    Ok(())
}

async fn run_relay(
    listener: UnixListener,
    session_id: Uuid,
    sender: mpsc::Sender<LifecycleObservation>,
    sequence: Arc<AtomicU64>,
    semaphore: Arc<Semaphore>,
    io_timeout: Duration,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => break,
            completed = connections.join_next(), if !connections.is_empty() => {
                let _ = completed;
            }
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else {
                    break;
                };
                let Ok(permit) = Arc::clone(&semaphore).try_acquire_owned() else {
                    continue;
                };
                let sender = sender.clone();
                let sequence = Arc::clone(&sequence);
                connections.spawn(async move {
                    let _permit = permit;
                    let _ = tokio::time::timeout(
                        io_timeout,
                        serve_connection(stream, session_id, sender, sequence),
                    )
                    .await;
                });
            }
        }
    }

    // Every connection operation is cancellation-safe Tokio I/O over a bounded
    // 64 KiB frame. Abort first, then join every child so relay shutdown cannot
    // return with a task still owning a stream, semaphore permit, or payload.
    connections.abort_all();
    while connections.join_next().await.is_some() {}
}

async fn serve_connection(
    mut stream: UnixStream,
    expected_session_id: Uuid,
    sender: mpsc::Sender<LifecycleObservation>,
    sequence: Arc<AtomicU64>,
) -> Result<()> {
    let envelope: HookRelayEnvelope = read_frame(&mut stream).await?;
    let observation = validate_envelope(envelope, expected_session_id);
    let response = match observation {
        Ok(mut observation) => {
            let Some(next_sequence) = next_lifecycle_sequence(sequence.as_ref()) else {
                let response = HookRelayResponse {
                    version: HOOK_RELAY_PROTOCOL_VERSION,
                    accepted: false,
                    code: "sequence_exhausted".into(),
                };
                write_frame(&mut stream, &response).await?;
                return Ok(());
            };
            observation.sequence = next_sequence;
            if sender.send(observation).await.is_err() {
                HookRelayResponse {
                    version: HOOK_RELAY_PROTOCOL_VERSION,
                    accepted: false,
                    code: "receiver_closed".into(),
                }
            } else {
                HookRelayResponse {
                    version: HOOK_RELAY_PROTOCOL_VERSION,
                    accepted: true,
                    code: "accepted".into(),
                }
            }
        }
        Err(_) => HookRelayResponse {
            version: HOOK_RELAY_PROTOCOL_VERSION,
            accepted: false,
            code: "invalid_event".into(),
        },
    };
    write_frame(&mut stream, &response).await
}

fn next_lifecycle_sequence(sequence: &AtomicU64) -> Option<u64> {
    sequence
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current
                .checked_add(1)
                .filter(|next| *next <= MAX_SAFE_JSON_INTEGER)
        })
        .ok()
        .and_then(|previous| previous.checked_add(1))
}

fn validate_envelope(
    envelope: HookRelayEnvelope,
    expected_session_id: Uuid,
) -> Result<LifecycleObservation> {
    ensure!(
        envelope.version == HOOK_RELAY_PROTOCOL_VERSION,
        "unsupported Hybrid hook protocol version"
    );
    ensure!(
        envelope.session_id == expected_session_id,
        "Hybrid hook session mismatch"
    );
    let payload = envelope
        .payload
        .as_object()
        .context("Hybrid hook payload must be an object")?;
    let expected_session = expected_session_id.to_string();
    ensure!(
        payload.get("session_id").and_then(Value::as_str) == Some(expected_session.as_str()),
        "Hybrid hook payload session mismatch"
    );
    ensure!(
        payload.get("hook_event_name").and_then(Value::as_str) == Some(envelope.event.as_str()),
        "Hybrid hook payload event mismatch"
    );

    let transcript_path = payload
        .get("transcript_path")
        .and_then(Value::as_str)
        .map(PathBuf::from);
    if let Some(path) = &transcript_path {
        ensure!(
            path.is_absolute(),
            "Hybrid transcript path must be absolute"
        );
    }
    Ok(LifecycleObservation {
        sequence: 0,
        session_id: expected_session_id,
        event: envelope.event,
        transcript_path,
        failure_observed: envelope.event == LifecycleEventKind::StopFailure,
    })
}

fn compose_caller_settings(sources: &[ConfigSource]) -> Result<Value> {
    let mut composed = Value::Object(Map::new());
    for source in sources {
        let incoming = match source {
            ConfigSource::Inline { document } => document.clone(),
            ConfigSource::File { path } => read_settings_file(Path::new(path))?,
        };
        ensure!(
            incoming.is_object(),
            "each settings source must contain a JSON object"
        );
        merge_settings(&mut composed, incoming, &mut Vec::new())?;
    }
    Ok(composed)
}

fn read_settings_file(path: &Path) -> Result<Value> {
    ensure!(path.is_absolute(), "settings source path must be absolute");
    let path = path
        .canonicalize()
        .context("failed to canonicalize a settings source")?;
    let file = std::fs::File::open(&path).context("failed to read a settings source")?;
    let metadata = file
        .metadata()
        .context("failed to inspect a settings source")?;
    ensure!(metadata.is_file(), "settings source must be a regular file");
    ensure!(
        metadata.len() <= MAX_SETTINGS_BYTES as u64,
        "settings source exceeds the size limit"
    );
    let mut bytes = Vec::new();
    file.take(MAX_SETTINGS_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .context("failed to read a settings source")?;
    ensure!(
        bytes.len() <= MAX_SETTINGS_BYTES,
        "settings source exceeds the size limit"
    );
    serde_json::from_slice(&bytes).context("settings source is not valid JSON")
}

fn merge_settings(target: &mut Value, incoming: Value, path: &mut Vec<String>) -> Result<()> {
    match (target, incoming) {
        (Value::Object(target), Value::Object(incoming)) => {
            for (key, value) in incoming {
                if let Some(existing) = target.get_mut(&key) {
                    path.push(key);
                    merge_settings(existing, value, path)?;
                    path.pop();
                } else {
                    target.insert(key, value);
                }
            }
            Ok(())
        }
        (Value::Array(target), Value::Array(mut incoming))
            if path.first().is_some_and(|v| v == "hooks") =>
        {
            target.append(&mut incoming);
            Ok(())
        }
        (target, incoming) if *target == incoming => Ok(()),
        _ => bail!("settings sources conflict and cannot be composed safely"),
    }
}

fn append_lifecycle_hooks(
    document: &mut Value,
    hook_client: &Path,
    socket_path: &Path,
    session_id: Uuid,
    timeout: Duration,
) -> Result<()> {
    let root = document
        .as_object_mut()
        .context("composed settings root must be an object")?;
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .context("settings hooks value must be an object")?;
    let timeout_seconds = u64::try_from(timeout.as_millis().div_ceil(1_000))?;

    for event in LifecycleEventKind::ALL {
        let entries = hooks
            .entry(event.as_str())
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .context("lifecycle hook settings must be arrays")?;
        entries.push(json!({
            "hooks": [{
                "type": "command",
                "command": hook_command(hook_client, socket_path, session_id, event)?,
                "timeout": timeout_seconds,
            }]
        }));
    }
    Ok(())
}

fn hook_command(
    hook_client: &Path,
    socket_path: &Path,
    session_id: Uuid,
    event: LifecycleEventKind,
) -> Result<String> {
    let executable = hook_client
        .to_str()
        .context("hook client path must be UTF-8")?;
    let socket = socket_path
        .to_str()
        .context("hook relay path must be UTF-8")?;
    Ok(format!(
        "{} --socket {} --session-id {} --event {}",
        shell_quote(executable),
        shell_quote(socket),
        shell_quote(&session_id.to_string()),
        shell_quote(event.as_str()),
    ))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

async fn read_frame<T: serde::de::DeserializeOwned>(stream: &mut UnixStream) -> Result<T> {
    let length = stream.read_u32().await? as usize;
    ensure!(length > 0, "Hybrid hook frame is empty");
    ensure!(
        length <= MAX_HOOK_FRAME_BYTES,
        "Hybrid hook frame exceeds the size limit"
    );
    let mut payload = vec![0_u8; length];
    stream.read_exact(&mut payload).await?;
    serde_json::from_slice(&payload).context("Hybrid hook frame is invalid")
}

async fn write_frame<T: Serialize>(stream: &mut UnixStream, value: &T) -> Result<()> {
    let payload = serde_json::to_vec(value).context("failed to encode Hybrid hook frame")?;
    ensure!(
        payload.len() <= MAX_HOOK_FRAME_BYTES,
        "Hybrid hook frame exceeds the size limit"
    );
    stream.write_u32(u32::try_from(payload.len())?).await?;
    stream.write_all(&payload).await?;
    stream.flush().await?;
    Ok(())
}

fn validate_private_runtime_dir(runtime_dir: &Path) -> Result<()> {
    ensure!(
        runtime_dir.is_absolute(),
        "private runtime directory must be absolute"
    );
    let metadata = std::fs::symlink_metadata(runtime_dir)
        .context("private runtime directory is unavailable")?;
    ensure!(
        metadata.file_type().is_dir() && !metadata.file_type().is_symlink(),
        "private runtime path must be a directory"
    );
    ensure!(
        metadata.uid() == effective_uid(),
        "private runtime directory is owned by another user"
    );
    ensure!(
        metadata.permissions().mode() & 0o077 == 0,
        "private runtime directory must have mode 0700 or stricter"
    );
    Ok(())
}

fn validate_hook_client(hook_client: &Path) -> Result<()> {
    ensure!(
        hook_client.is_absolute(),
        "hook client must be an absolute path"
    );
    let metadata = std::fs::metadata(hook_client).context("hook client is unavailable")?;
    ensure!(metadata.is_file(), "hook client must be a regular file");
    ensure!(
        metadata.permissions().mode() & 0o111 != 0,
        "hook client must be executable"
    );
    Ok(())
}

fn ensure_path_absent(path: &Path, label: &str) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => bail!("{label} already exists"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("failed to inspect Hybrid runtime path"),
    }
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<FileIdentity> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .context("failed to create Hybrid settings")?;
    let identity = FileIdentity::from_file(&file).context("failed to inspect Hybrid settings")?;
    let write_result = file
        .write_all(bytes)
        .context("failed to write Hybrid settings")
        .and_then(|()| file.sync_all().context("failed to sync Hybrid settings"));
    if let Err(error) = write_result {
        drop(file);
        remove_if_same_file(path, &identity);
        return Err(error);
    }
    Ok(identity)
}

fn remove_owned_file(path: &Path) {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return;
    };
    if metadata.uid() != effective_uid() {
        return;
    }
    if metadata.file_type().is_file() || metadata.file_type().is_socket() {
        let _ = std::fs::remove_file(path);
    }
}

fn remove_if_same_file(path: &Path, identity: &FileIdentity) {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return;
    };
    if metadata.uid() != effective_uid() || !identity.matches(&metadata) {
        return;
    }
    if metadata.file_type().is_file() || metadata.file_type().is_socket() {
        let _ = std::fs::remove_file(path);
    }
}

#[allow(unsafe_code)]
fn effective_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and does not dereference pointers.
    unsafe { libc::geteuid() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn transcript_mode_does_not_touch_inputs() {
        let runtime = Path::new("/definitely/missing/private/runtime");
        let result = prepare_lifecycle(
            &LifecycleMode::Transcript,
            runtime,
            Uuid::new_v4(),
            Path::new("/missing/hook-client"),
            &[ConfigSource::File {
                path: "/missing/settings.json".into(),
            }],
        )
        .await
        .unwrap();
        assert!(matches!(result, PreparedLifecycle::Transcript));
    }

    #[test]
    fn hook_merge_is_additive_and_conflicts_fail_closed() {
        let mut target = json!({"hooks": {"Stop": [{"hooks": [{"command": "original"}]}]}});
        merge_settings(
            &mut target,
            json!({"hooks": {"Stop": [{"hooks": [{"command": "second"}]}]}}),
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(target["hooks"]["Stop"].as_array().unwrap().len(), 2);

        let mut conflict = json!({"theme": "dark"});
        assert!(merge_settings(&mut conflict, json!({"theme": "light"}), &mut Vec::new()).is_err());
    }

    #[test]
    fn settings_files_require_absolute_regular_paths() {
        assert!(read_settings_file(Path::new("relative-settings.json")).is_err());

        let directory = tempfile::tempdir().unwrap();
        assert!(read_settings_file(directory.path()).is_err());

        let settings = directory.path().join("settings.json");
        std::fs::write(&settings, "{}").unwrap();
        assert_eq!(read_settings_file(&settings).unwrap(), json!({}));
    }

    #[test]
    fn quoting_preserves_each_shell_argument() {
        assert_eq!(shell_quote("a'b"), "'a'\"'\"'b'");
    }

    #[test]
    fn lifecycle_sequence_fails_closed_at_protocol_safe_max() {
        let sequence = AtomicU64::new(MAX_SAFE_JSON_INTEGER - 1);
        assert_eq!(
            next_lifecycle_sequence(&sequence),
            Some(MAX_SAFE_JSON_INTEGER)
        );
        assert_eq!(next_lifecycle_sequence(&sequence), None);
        assert_eq!(sequence.load(Ordering::Relaxed), MAX_SAFE_JSON_INTEGER);
    }
}
