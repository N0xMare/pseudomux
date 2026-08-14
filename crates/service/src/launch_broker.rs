//! Owner-only, one-shot process-spec delivery for `pmux-launcher`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use pseudomux_rmux::{
    LAUNCHER_PROTOCOL_VERSION, LaunchSpec, LaunchToken, LauncherRequest, LauncherResponse,
    MAX_LAUNCHER_FRAME_BYTES,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinHandle;

const DEFAULT_TOKEN_TTL: Duration = Duration::from_secs(30);

/// The version [`LaunchBroker::probe`] presents. Derived from the real one so a
/// protocol bump cannot leave the probe accidentally speaking the live version
/// and consuming a capability.
const PROBE_PROTOCOL_VERSION: u16 = LAUNCHER_PROTOCOL_VERSION.wrapping_add(1);

/// The exact rejection code `serve_connection` answers a version it does not
/// speak. Named once so the probe's expectation and the server's answer are the
/// same string.
const UNSUPPORTED_VERSION_CODE: &str = "unsupported_version";

/// What one launcher-shaped exchange against the broker's own endpoint proved.
///
/// Every variant is a POSITIVE statement about what happened, so a health
/// surface reading this cannot report success by falling through. In
/// particular there is no "unknown": a probe that could not complete says which
/// step it died at, because "the broker is fine" and "we never found out" are
/// the two answers this project keeps confusing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrokerProbe {
    /// Connected, wrote a frame, and read back the exact refusal the server
    /// gives a version it does not speak. Accept loop, framing codec and
    /// dispatch all ran.
    Exchanged,
    /// `connect` was refused. This is what `pmux-launcher` meets when the
    /// accept loop has ended: the socket file survives, the listener does not.
    ConnectRefused,
    /// `connect` failed for some other reason, e.g. the endpoint is gone.
    ConnectFailed(String),
    /// The exchange did not finish inside the deadline every rmux operation is
    /// held to.
    TimedOut,
    /// The exchange completed and the answer was not the one the launcher
    /// protocol specifies. The broker is answering, but not with this protocol.
    UnexpectedAnswer(String),
}

impl BrokerProbe {
    /// True only for [`Self::Exchanged`].
    ///
    /// A method rather than a `matches!` at each call site, so the set of
    /// variants that count as success is written ONCE and a variant added later
    /// is not silently folded into it.
    #[must_use]
    pub const fn exchanged(&self) -> bool {
        matches!(self, Self::Exchanged)
    }

    /// One line naming what this probe established, for a health detail string.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Exchanged => "the launch broker accepted a connection, read a launcher frame \
                                and answered it"
                .to_owned(),
            Self::ConnectRefused => "the launch broker refused the connection: its accept loop \
                                     has ended and the listener is gone, so every later session \
                                     start meets ConnectionRefused in pmux-launcher"
                .to_owned(),
            Self::ConnectFailed(error) => {
                format!("the launch broker endpoint could not be connected: {error}")
            }
            Self::TimedOut => "the launch broker accepted a connection and did not complete the \
                               exchange within the deadline every session operation is held to"
                .to_owned(),
            Self::UnexpectedAnswer(detail) => {
                format!("the launch broker answered a launcher frame with {detail}")
            }
        }
    }
}

struct PendingLaunch {
    spec: LaunchSpec,
    expires_at: Instant,
}

struct BrokerState {
    pending: Mutex<HashMap<LaunchToken, PendingLaunch>>,
}

/// Running launch broker. Dropping it aborts the accept loop and removes its socket.
pub struct LaunchBroker {
    socket: PathBuf,
    state: Arc<BrokerState>,
    task: JoinHandle<()>,
}

impl LaunchBroker {
    /// Binds an endpoint inside an existing owner-only runtime directory.
    pub async fn bind(socket: PathBuf) -> Result<Self> {
        validate_endpoint(&socket)?;
        remove_stale_socket(&socket)?;
        let listener = UnixListener::bind(&socket)
            .with_context(|| format!("failed to bind launch broker {}", socket.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
        }

        let state = Arc::new(BrokerState {
            pending: Mutex::new(HashMap::new()),
        });
        let task_state = Arc::clone(&state);
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let state = Arc::clone(&task_state);
                tokio::spawn(async move {
                    let _ = serve_connection(stream, state).await;
                });
            }
        });

        Ok(Self {
            socket,
            state,
            task,
        })
    }

    /// Registers a spec and returns the only capability that can retrieve it.
    pub fn register(&self, spec: LaunchSpec) -> Result<LaunchToken> {
        self.register_with_ttl(spec, DEFAULT_TOKEN_TTL)
    }

    /// Registers with an explicit short TTL, mainly for deterministic tests.
    pub fn register_with_ttl(&self, spec: LaunchSpec, ttl: Duration) -> Result<LaunchToken> {
        spec.validate().map_err(anyhow::Error::msg)?;
        if ttl.is_zero() {
            bail!("launch token TTL must be non-zero");
        }
        let token = LaunchToken::generate();
        let mut pending = self.state.pending.lock().expect("launch broker lock");
        pending.retain(|_, entry| entry.expires_at > Instant::now());
        pending.insert(
            token.clone(),
            PendingLaunch {
                spec,
                expires_at: Instant::now() + ttl,
            },
        );
        Ok(token)
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    /// Whether the accept loop above is still running.
    ///
    /// Free, synchronous, and side-effect-free, which is the whole reason a
    /// health probe uses this and not a registered spec: `register` mints a
    /// real one-use launch capability, and minting a capability that can hand a
    /// process spec to whoever presents it is not a thing to do for a
    /// diagnostic.
    ///
    /// The loop exits on the *first* `accept` error and nothing anywhere
    /// notices. The endpoint keeps looking healthy from the outside -- the
    /// socket file stays, because only [`Drop`] unlinks it -- while the
    /// listener itself is dropped with the loop, so every later
    /// `start_session` meets `ConnectionRefused` in the launcher and the daemon
    /// can no longer start a session at all. MEASURED in
    /// `a_broker_whose_accept_loop_has_ended_reports_itself_not_accepting`,
    /// which also corrected the earlier claim here that such a start would hang
    /// in the handshake: it does not, it is refused. The task is otherwise
    /// ended only by [`Drop`], which also removes the socket, so a finished task
    /// while this broker is alive means exactly one thing.
    #[must_use]
    pub fn is_accepting(&self) -> bool {
        !self.task.is_finished()
    }

    /// Drives one real launcher-shaped exchange against this broker's own
    /// endpoint and reports what it proved.
    ///
    /// WHY THIS EXISTS. [`Self::is_accepting`] is a task-liveness read, and the
    /// health layer built on it said `exercised` -- a word that promises more
    /// than `!task.is_finished()` tests. Everything between the accept and the
    /// answer was unmeasured: the framing codec, the length prefix, the
    /// dispatch, whether an accepted connection is ever served at all. A loop
    /// that accepts and then wedges before `serve_connection` reads its first
    /// byte is indistinguishable from a healthy one to a liveness read, and it
    /// is exactly the shape that hangs `pmux-launcher` inside a session start.
    ///
    /// WHY IT COSTS NO CAPABILITY. It presents `PROBE_PROTOCOL_VERSION`
    /// (private, so not an intra-doc link from public documentation),
    /// which is `LAUNCHER_PROTOCOL_VERSION + 1`, and `serve_connection` answers
    /// a version it does not speak BEFORE it touches the pending map. So the
    /// probe cannot remove a pending entry, cannot expire one, and cannot hand
    /// a process spec to anybody -- which is the reason the old code gave for
    /// not probing at all. The token is generated and never registered; it is
    /// present only because the frame requires the field, and it is never
    /// looked up.
    ///
    /// WHAT IT DOES NOT PROVE, stated so the layer can copy it exactly: that a
    /// REAL token would be honoured. The pending-map lookup is the one step not
    /// on this path, because that lookup consumes a one-use capability and a
    /// diagnostic that spends capabilities is a diagnostic nobody may call
    /// twice.
    pub async fn probe(&self, timeout: Duration) -> BrokerProbe {
        let request = LauncherRequest {
            version: PROBE_PROTOCOL_VERSION,
            token: LaunchToken::generate(),
        };
        let mut stream = match UnixStream::connect(&self.socket).await {
            Ok(stream) => stream,
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                return BrokerProbe::ConnectRefused;
            }
            Err(error) => return BrokerProbe::ConnectFailed(error.to_string()),
        };
        let exchange = async {
            write_frame(&mut stream, &request).await?;
            read_frame::<LauncherResponse>(&mut stream).await
        };
        match tokio::time::timeout(timeout, exchange).await {
            Err(_elapsed) => BrokerProbe::TimedOut,
            Ok(Err(error)) => {
                BrokerProbe::UnexpectedAnswer(format!("an unreadable frame: {error}"))
            }
            Ok(Ok(LauncherResponse::Rejected { version, code }))
                if version == LAUNCHER_PROTOCOL_VERSION && code == UNSUPPORTED_VERSION_CODE =>
            {
                BrokerProbe::Exchanged
            }
            Ok(Ok(LauncherResponse::Rejected { version, code })) => BrokerProbe::UnexpectedAnswer(
                format!("a rejection carrying version {version} and code {code:?}"),
            ),
            // A `Ready` here would mean the broker handed out a launch spec for
            // a version it does not speak and a token nobody registered. It is
            // reported, not ignored, and the spec is not named: a health string
            // is not a place to print a process spec.
            Ok(Ok(LauncherResponse::Ready { version, .. })) => BrokerProbe::UnexpectedAnswer(
                format!("a READY spec for an unregistered token at version {version}"),
            ),
        }
    }

    /// Ends the accept loop the way a fatal `accept` error would, without
    /// dropping the broker.
    ///
    /// TEST-ONLY, AND IT EXISTS BECAUSE THE FALSE CASE HAD NO TEST. Every
    /// deterministic check of [`Self::is_accepting`] used to run against a
    /// broker that was accepting, and `build_diagnosis` takes the answer in as a
    /// `bool` parameter, so nothing ever exercised the reader on a stopped
    /// loop. MEASURED: with the body of `is_accepting` replaced by `true`, the
    /// whole suite still passed -- which made
    /// `RuntimeFinding::LaunchBrokerStopped` the one runtime finding that was
    /// reachable in production and unreachable in test.
    ///
    /// A genuine `accept` error cannot be provoked from safe code -- the socket
    /// file can be unlinked without the listener noticing, and `EMFILE` is
    /// process-global and would poison every other test in the binary -- so this
    /// ends the same task by the same means [`Drop`] does, and then waits until
    /// it has actually stopped. What the caller gets is a live broker, socket
    /// and pending map intact, whose loop is over: exactly the state
    /// `is_accepting` claims to be able to report.
    #[cfg(test)]
    async fn stop_accepting_for_test(&self) {
        self.task.abort();
        // `abort` only requests cancellation. Polling until the join handle
        // reports completion is what makes the assertion that follows a
        // statement about the loop and not about a race with the scheduler.
        while !self.task.is_finished() {
            tokio::task::yield_now().await;
        }
    }

    pub fn revoke(&self, token: &LaunchToken) -> bool {
        self.state
            .pending
            .lock()
            .expect("launch broker lock")
            .remove(token)
            .is_some()
    }
}

impl Drop for LaunchBroker {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_file(&self.socket);
    }
}

async fn serve_connection(mut stream: UnixStream, state: Arc<BrokerState>) -> Result<()> {
    let request: LauncherRequest = read_frame(&mut stream).await?;
    let response = if request.version != LAUNCHER_PROTOCOL_VERSION {
        // Ahead of the pending map on purpose, and `LaunchBroker::probe`
        // depends on it: a frame this branch answers can never consume a
        // one-use capability, which is what makes a real exchange affordable as
        // a diagnostic.
        LauncherResponse::Rejected {
            version: LAUNCHER_PROTOCOL_VERSION,
            code: UNSUPPORTED_VERSION_CODE.into(),
        }
    } else {
        let pending = state
            .pending
            .lock()
            .expect("launch broker lock")
            .remove(&request.token);
        match pending {
            Some(pending) if pending.expires_at > Instant::now() => LauncherResponse::Ready {
                version: LAUNCHER_PROTOCOL_VERSION,
                spec: pending.spec,
            },
            Some(_) => LauncherResponse::Rejected {
                version: LAUNCHER_PROTOCOL_VERSION,
                code: "expired_token".into(),
            },
            None => LauncherResponse::Rejected {
                version: LAUNCHER_PROTOCOL_VERSION,
                code: "unknown_or_used_token".into(),
            },
        }
    };
    write_frame(&mut stream, &response).await
}

async fn read_frame<T: serde::de::DeserializeOwned>(stream: &mut UnixStream) -> Result<T> {
    let length = stream.read_u32().await? as usize;
    if length > MAX_LAUNCHER_FRAME_BYTES {
        bail!("launch request exceeds maximum frame size");
    }
    let mut payload = vec![0_u8; length];
    stream.read_exact(&mut payload).await?;
    serde_json::from_slice(&payload).context("invalid launch request")
}

async fn write_frame<T: serde::Serialize>(stream: &mut UnixStream, value: &T) -> Result<()> {
    let payload = serde_json::to_vec(value)?;
    if payload.len() > MAX_LAUNCHER_FRAME_BYTES {
        bail!("launch response exceeds maximum frame size");
    }
    stream.write_u32(u32::try_from(payload.len())?).await?;
    stream.write_all(&payload).await?;
    stream.flush().await?;
    Ok(())
}

fn validate_endpoint(socket: &Path) -> Result<()> {
    if !socket.is_absolute() {
        bail!("launch broker socket must be absolute");
    }
    let parent = socket
        .parent()
        .context("launch broker socket has no parent")?;
    let metadata = std::fs::metadata(parent)
        .with_context(|| format!("launch broker parent {} is unavailable", parent.display()))?;
    if !metadata.is_dir() {
        bail!("launch broker parent is not a directory");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            bail!("launch broker parent must have mode 0700 or stricter");
        }
    }
    Ok(())
}

fn remove_stale_socket(socket: &Path) -> Result<()> {
    let Ok(metadata) = std::fs::symlink_metadata(socket) else {
        return Ok(());
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        if !metadata.file_type().is_socket() {
            bail!("refusing to remove non-socket path {}", socket.display());
        }
    }
    std::fs::remove_file(socket)
        .with_context(|| format!("failed to remove stale socket {}", socket.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pseudomux_rmux::EnvironmentSnapshot;

    fn spec(root: &Path) -> LaunchSpec {
        LaunchSpec {
            executable: PathBuf::from("/usr/bin/true"),
            args: Vec::new(),
            cwd: root.to_path_buf(),
            environment: EnvironmentSnapshot::default(),
        }
    }

    async fn exchange(socket: &Path, request: &LauncherRequest) -> LauncherResponse {
        let mut stream = UnixStream::connect(socket).await.unwrap();
        write_frame(&mut stream, request).await.unwrap();
        read_frame(&mut stream).await.unwrap()
    }

    #[tokio::test]
    async fn capability_is_one_use() {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let broker = LaunchBroker::bind(dir.path().join("launcher.sock"))
            .await
            .unwrap();
        let token = broker.register(spec(dir.path())).unwrap();
        let request = LauncherRequest {
            version: LAUNCHER_PROTOCOL_VERSION,
            token,
        };

        assert!(matches!(
            exchange(broker.socket_path(), &request).await,
            LauncherResponse::Ready { .. }
        ));
        assert!(matches!(
            exchange(broker.socket_path(), &request).await,
            LauncherResponse::Rejected { code, .. } if code == "unknown_or_used_token"
        ));
    }

    /// A bound broker reports itself as accepting.
    ///
    /// Trivial-looking and load-bearing: the health probe folds this into a
    /// `fail`, so an inverted sense here would report every healthy daemon as
    /// broken. This is HALF the guard and only half -- it pins the `true` case,
    /// and an `is_accepting` that ignored its task and returned `true`
    /// unconditionally would pass it. The `false` case, which is the one the
    /// health probe exists for, is
    /// [`a_broker_whose_accept_loop_has_ended_reports_itself_not_accepting`]
    /// below; that pairing is the whole check and neither half is it alone.
    #[tokio::test]
    async fn a_bound_broker_reports_itself_accepting() {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let broker = LaunchBroker::bind(dir.path().join("launcher.sock"))
            .await
            .unwrap();
        assert!(broker.is_accepting());
        // One completed exchange must not end the loop: the accept loop serves
        // every launcher, and a broker that stopped after the first would take
        // the whole daemon with it on the second session.
        let token = broker.register(spec(dir.path())).unwrap();
        let _ = exchange(
            broker.socket_path(),
            &LauncherRequest {
                version: LAUNCHER_PROTOCOL_VERSION,
                token,
            },
        )
        .await;
        assert!(broker.is_accepting());
    }

    /// A broker whose accept loop has ended says so -- the case the health
    /// probe exists for, which had no test at all.
    ///
    /// WHAT WAS MISSING. `RuntimeFinding::LaunchBrokerStopped` is produced by
    /// `build_diagnosis`, which takes `launch_broker_is_accepting` in as a
    /// `bool`, so the one test that reaches that finding
    /// (`native.rs`'s `a_stopped_launch_broker_is_a_fault_even_when_the_sidecar_answers`)
    /// passes the answer in and never calls the reader. Everything else called
    /// the reader only on a broker that was accepting. MEASURED: with the body
    /// of [`LaunchBroker::is_accepting`] replaced by `true`, the whole suite
    /// passed -- the only runtime finding that was reachable in production and
    /// unreachable in test.
    ///
    /// WHY IT ALSO MEASURES THE FILESYSTEM AND THE CONNECT. A dead broker is
    /// indistinguishable from a live one by looking at its endpoint: the socket
    /// file is still there and still a socket, because only [`Drop`] unlinks
    /// it. What has changed is invisible from the path -- the ended loop takes
    /// the `UnixListener` with it, since the listener is moved into that async
    /// block and dropped when the block ends, so the listening fd is closed and
    /// `connect` is REFUSED. Asserting both together is what makes this a test
    /// of the reader rather than of the tempdir: the cheap check any caller
    /// might reach for first (does the socket exist?) is measured here saying
    /// "healthy" at the same moment `is_accepting` says "stopped".
    ///
    /// MEASURED, and it corrected a doc claim: `connect` returns
    /// `Os { code: 61, kind: ConnectionRefused }`, it does NOT succeed out of
    /// the listen backlog and hang in the handshake, which is what
    /// [`LaunchBroker::is_accepting`] and `pmux`'s `runtime_finding_text` both
    /// used to tell the reader. `pmux-launcher` makes that same `connect` call
    /// at `bin/pmux-launcher/src/main.rs:46` and bails on it, so a start
    /// against a stopped broker fails fast in the pane instead of blocking.
    #[tokio::test]
    async fn a_broker_whose_accept_loop_has_ended_reports_itself_not_accepting() {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let broker = LaunchBroker::bind(dir.path().join("launcher.sock"))
            .await
            .unwrap();
        assert!(broker.is_accepting());

        broker.stop_accepting_for_test().await;

        assert!(
            !broker.is_accepting(),
            "the accept loop has ended and `is_accepting` still reports the broker as healthy; \
             `RuntimeFinding::LaunchBrokerStopped` can then never be produced, and a daemon that \
             cannot start another session reports itself as fine"
        );
        // The broker is alive and only its loop is over. If the socket were
        // gone this test would be observing `Drop` instead, and the health
        // probe would have a cheaper signal available to it than a
        // task-liveness read.
        assert!(
            broker.socket_path().exists(),
            "the endpoint must outlive the accept loop; a stopped broker whose socket had \
             already been removed would be detectable without this reader at all"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileTypeExt;
            assert!(
                std::fs::metadata(broker.socket_path())
                    .unwrap()
                    .file_type()
                    .is_socket(),
                "the endpoint is still a socket after the loop ended, so `pmux-launcher`'s own \
                 pre-connect check passes and the failure is deferred to the connect"
            );
        }

        let connected = UnixStream::connect(broker.socket_path()).await;
        assert_eq!(
            connected.err().map(|error| error.kind()),
            Some(std::io::ErrorKind::ConnectionRefused),
            "a broker whose accept loop ended must refuse connections: the listener is dropped \
             with the loop, so this is what `pmux-launcher` meets at its own connect"
        );
    }

    /// The probe drives a real exchange, and it does not spend the capability
    /// sitting beside it.
    ///
    /// The second half is the load-bearing one. The reason the health surface
    /// read task liveness instead of exchanging was that `register` mints a
    /// one-use launch capability, so a probe that went through the token lookup
    /// could consume one. This asserts the pending entry SURVIVES the probe by
    /// redeeming it afterwards: if the probe had reached the pending map, the
    /// redemption below would come back `unknown_or_used_token`.
    #[tokio::test]
    async fn a_probe_exchanges_a_real_frame_without_spending_a_pending_capability() {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let broker = LaunchBroker::bind(dir.path().join("launcher.sock"))
            .await
            .unwrap();
        let token = broker.register(spec(dir.path())).unwrap();

        assert_eq!(
            broker.probe(Duration::from_secs(5)).await,
            BrokerProbe::Exchanged,
            "a live broker must complete a launcher-shaped exchange, not merely hold a task"
        );

        assert!(
            matches!(
                exchange(
                    broker.socket_path(),
                    &LauncherRequest {
                        version: LAUNCHER_PROTOCOL_VERSION,
                        token,
                    },
                )
                .await,
                LauncherResponse::Ready { .. }
            ),
            "the probe consumed the pending capability it was written not to touch"
        );
        // And the loop is still serving after both exchanges.
        assert!(broker.is_accepting());
        assert!(broker.probe(Duration::from_secs(5)).await.exchanged());
    }

    /// A broker whose accept loop has ended fails the probe, and fails it by
    /// the same means `pmux-launcher` fails.
    ///
    /// The pairing with the test above is the check. Alone, either half passes
    /// against a `probe` that ignores its socket and returns a constant.
    #[tokio::test]
    async fn a_probe_against_a_stopped_broker_is_refused_rather_than_answered() {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let broker = LaunchBroker::bind(dir.path().join("launcher.sock"))
            .await
            .unwrap();
        assert!(broker.probe(Duration::from_secs(5)).await.exchanged());

        broker.stop_accepting_for_test().await;

        let probe = broker.probe(Duration::from_secs(5)).await;
        assert_eq!(
            probe,
            BrokerProbe::ConnectRefused,
            "a stopped broker must fail the probe; its socket file is still present and still a \
             socket, so nothing cheaper than a connect can tell"
        );
        assert!(!probe.exchanged());
        assert!(
            broker.socket_path().exists(),
            "the endpoint outlives the accept loop, which is why the probe has to connect"
        );
    }

    /// The probe speaks a version the server does not, and that is what keeps
    /// it free.
    ///
    /// DERIVED, not restated: it asserts the inequality against
    /// `LAUNCHER_PROTOCOL_VERSION` rather than pinning a literal, so a protocol
    /// bump that happened to collide is caught here instead of in production,
    /// where the collision would be a probe redeeming real capabilities.
    #[test]
    fn the_probe_version_can_never_be_the_live_one() {
        assert_ne!(PROBE_PROTOCOL_VERSION, LAUNCHER_PROTOCOL_VERSION);
    }

    #[tokio::test]
    async fn expired_capability_never_returns_spec() {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let broker = LaunchBroker::bind(dir.path().join("launcher.sock"))
            .await
            .unwrap();
        let token = broker
            .register_with_ttl(spec(dir.path()), Duration::from_millis(1))
            .unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
        let response = exchange(
            broker.socket_path(),
            &LauncherRequest {
                version: LAUNCHER_PROTOCOL_VERSION,
                token,
            },
        )
        .await;
        assert!(matches!(
            response,
            LauncherResponse::Rejected { code, .. } if code == "expired_token"
        ));
    }
}
