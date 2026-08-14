//! Short-lived one-use proxy capabilities for interactive terminal attachment.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail, ensure};
use rmux_client::AttachTransition;
use rmux_sdk::SessionName;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::oneshot;
use uuid::Uuid;

use pseudomux_protocol::v1::{ErrorCode, MAX_SAFE_JSON_INTEGER};

const MAX_TOKEN_BYTES: usize = 128;
const AUTH_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AttachTimeError {
    CurrentTimeUnavailable,
    TtlOutOfRange,
    ExpiryOutOfRange,
}

impl AttachTimeError {
    pub(crate) const fn protocol_code(self) -> ErrorCode {
        match self {
            Self::CurrentTimeUnavailable => ErrorCode::RecoveryFailed,
            Self::TtlOutOfRange | Self::ExpiryOutOfRange => ErrorCode::InvalidConfig,
        }
    }
}

impl std::fmt::Display for AttachTimeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::CurrentTimeUnavailable => {
                "current time is outside protocol-v1's safe timestamp domain"
            }
            Self::TtlOutOfRange => "attach capability TTL is outside protocol-v1's safe domain",
            Self::ExpiryOutOfRange => {
                "attach capability expiry is outside protocol-v1's safe timestamp domain"
            }
        })
    }
}

impl std::error::Error for AttachTimeError {}

/// Public data used to construct a protocol attach capability.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachGrant {
    pub endpoint: PathBuf,
    pub token: String,
    pub expires_at_ms: u64,
}

/// Resolves when the one-use capability expires or its attached stream closes.
pub struct AttachCompletion {
    completed: oneshot::Receiver<AttachCompletionOutcome>,
}

/// Whether an attach capability could have mutated the Claude TUI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttachCompletionOutcome {
    /// No authenticated private-rmux attachment was established.
    Unused,
    /// An authenticated rmux attachment was established, or completion became
    /// uncertain after the proxy task started. Reconciliation is mandatory.
    PotentiallyMutated,
}

impl AttachCompletion {
    pub async fn wait(self) -> AttachCompletionOutcome {
        // Sender loss is ambiguous: fail closed as though input may have
        // reached the authenticated private rmux stream.
        self.completed
            .await
            .unwrap_or(AttachCompletionOutcome::PotentiallyMutated)
    }
}

/// Binds a one-use endpoint without exposing the private rmux socket or target.
pub async fn grant_attach(
    runtime_dir: &Path,
    rmux_socket: &Path,
    private_session_name: String,
    ttl: Duration,
) -> Result<(AttachGrant, AttachCompletion)> {
    ensure!(
        runtime_dir.is_absolute(),
        "runtime directory must be absolute"
    );
    ensure!(rmux_socket.is_absolute(), "rmux socket must be absolute");
    ensure!(!ttl.is_zero(), "attach capability TTL must be non-zero");

    // Validate the public timestamp before binding. A rejected configuration
    // must not leave a capability socket behind.
    let expires_at_ms = checked_attach_expiry_ms(unix_now_ms()?, ttl)?;

    let token = Uuid::new_v4().simple().to_string();
    let endpoint = runtime_dir.join(format!("attach-{}.sock", &token[..16]));
    let listener = UnixListener::bind(&endpoint).context("failed to bind attach capability")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(error) =
            std::fs::set_permissions(&endpoint, std::fs::Permissions::from_mode(0o600))
        {
            let _ = std::fs::remove_file(&endpoint);
            return Err(error).context("failed to secure attach capability");
        }
    }

    let task_endpoint = endpoint.clone();
    let expected_token = token.clone();
    let rmux_socket = rmux_socket.to_path_buf();
    let (completed, completion) = oneshot::channel();
    tokio::spawn(async move {
        let mut outcome = AttachCompletionOutcome::Unused;
        let accepted = tokio::time::timeout(ttl, listener.accept()).await;
        if let Ok(Ok((mut client, _))) = accepted
            && authenticate(&mut client, &expected_token).await.is_ok()
            && let Ok((rmux_stream, initial_bytes)) =
                open_rmux_attach(rmux_socket, private_session_name).await
        {
            // From this point onward, EOF and proxy errors are ambiguous with
            // respect to bytes already delivered to the terminal.
            outcome = AttachCompletionOutcome::PotentiallyMutated;
            let _ = proxy_attach(client, rmux_stream, initial_bytes).await;
        }
        remove_owned_socket(&task_endpoint);
        let _ = completed.send(outcome);
    });

    Ok((
        AttachGrant {
            endpoint,
            token,
            expires_at_ms,
        },
        AttachCompletion {
            completed: completion,
        },
    ))
}

async fn authenticate(stream: &mut UnixStream, expected_token: &str) -> Result<()> {
    tokio::time::timeout(AUTH_TIMEOUT, async {
        let length = stream.read_u32().await? as usize;
        ensure!(
            length > 0 && length <= MAX_TOKEN_BYTES,
            "invalid attach token frame"
        );
        let mut token = vec![0; length];
        stream.read_exact(&mut token).await?;
        ensure!(
            constant_time_equal(&token, expected_token.as_bytes()),
            "attach token mismatch"
        );
        Result::<()>::Ok(())
    })
    .await
    .context("attach authentication timed out")??;
    Ok(())
}

async fn open_rmux_attach(
    rmux_socket: PathBuf,
    private_session_name: String,
) -> Result<(std::os::unix::net::UnixStream, Vec<u8>)> {
    tokio::task::spawn_blocking(move || {
        let target = SessionName::new(private_session_name)
            .map_err(|error| anyhow!("invalid private rmux target: {error}"))?;
        let connection = rmux_client::connect(&rmux_socket)
            .context("failed to connect to private rmux for attach")?;
        match connection.begin_attach(target)? {
            AttachTransition::Upgraded(upgrade) => Ok(upgrade.into_parts()),
            AttachTransition::Rejected(_) => bail!("private rmux rejected attach"),
        }
    })
    .await
    .context("private rmux attach worker failed")?
}

async fn proxy_attach(
    client: UnixStream,
    rmux_stream: std::os::unix::net::UnixStream,
    initial_bytes: Vec<u8>,
) -> Result<()> {
    rmux_stream
        .set_nonblocking(true)
        .context("failed to configure private attach stream")?;
    let rmux =
        UnixStream::from_std(rmux_stream).context("failed to import private attach stream")?;

    let (mut client_reader, mut client_writer) = client.into_split();
    let (mut rmux_reader, mut rmux_writer) = rmux.into_split();

    // An authenticated attach may already have delivered public input when the
    // consumer closes its read side (or disappears entirely). Keep the two
    // directions independently owned: failure to return terminal output must
    // never cancel client -> rmux forwarding. Conversely, keep draining rmux
    // output after a public write failure so the private server cannot become
    // backpressured before it consumes the final input frame.
    let client_to_rmux = async {
        let copy_result = tokio::io::copy(&mut client_reader, &mut rmux_writer).await;
        let shutdown_result = rmux_writer.shutdown().await;
        copy_result?;
        shutdown_result?;
        io::Result::Ok(())
    };
    let rmux_to_client =
        forward_attach_output(&mut rmux_reader, &mut client_writer, &initial_bytes);

    tokio::pin!(client_to_rmux);
    tokio::pin!(rmux_to_client);
    tokio::select! {
        // Prefer a simultaneously ready input result so an authenticated-input
        // failure remains the primary diagnostic. A clean client input EOF
        // still waits for the private server's final output and detach.
        biased;
        input_result = &mut client_to_rmux => {
            input_result.context("failed to forward authenticated attach input")?;
            rmux_to_client
                .await
                .context("failed to forward private attach output")?;
        }
        output_result = &mut rmux_to_client => {
            // Completion of this future always means the private output side
            // reached EOF or failed. The rmux attach is no longer usable, so
            // cancel a still-blocked public-input read rather than stranding
            // the capability reservation indefinitely.
            output_result.context("failed to forward private attach output")?;
        }
    }
    Ok(())
}

async fn forward_attach_output<R, W>(
    reader: &mut R,
    writer: &mut W,
    initial_bytes: &[u8],
) -> io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut output_error = None;
    if !initial_bytes.is_empty() {
        if let Err(error) = writer.write_all(initial_bytes).await {
            output_error = Some(error);
        } else if let Err(error) = writer.flush().await {
            output_error = Some(error);
        }
    }

    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let bytes_read = reader.read(&mut buffer).await?;
        if bytes_read == 0 {
            break;
        }
        if output_error.is_none()
            && let Err(error) = writer.write_all(&buffer[..bytes_read]).await
        {
            output_error = Some(error);
        }
    }

    if output_error.is_none()
        && let Err(error) = writer.shutdown().await
    {
        output_error = Some(error);
    }

    match output_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn constant_time_equal(actual: &[u8], expected: &[u8]) -> bool {
    let mut difference = actual.len() ^ expected.len();
    for index in 0..actual.len().max(expected.len()) {
        let left = actual.get(index).copied().unwrap_or(0);
        let right = expected.get(index).copied().unwrap_or(0);
        difference |= usize::from(left ^ right);
    }
    difference == 0
}

fn remove_owned_socket(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        let Ok(metadata) = std::fs::symlink_metadata(path) else {
            return;
        };
        #[allow(unsafe_code)]
        let uid = unsafe { libc::geteuid() };
        if metadata.uid() == uid && metadata.file_type().is_socket() {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn unix_now_ms() -> std::result::Result<u64, AttachTimeError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AttachTimeError::CurrentTimeUnavailable)?;
    u64::try_from(elapsed.as_millis())
        .ok()
        .filter(|millis| *millis <= MAX_SAFE_JSON_INTEGER)
        .ok_or(AttachTimeError::CurrentTimeUnavailable)
}

fn checked_attach_expiry_ms(
    now_ms: u64,
    ttl: Duration,
) -> std::result::Result<u64, AttachTimeError> {
    if now_ms > MAX_SAFE_JSON_INTEGER {
        return Err(AttachTimeError::CurrentTimeUnavailable);
    }
    let ttl_ms = u64::try_from(ttl.as_millis())
        .ok()
        .filter(|millis| *millis <= MAX_SAFE_JSON_INTEGER)
        .ok_or(AttachTimeError::TtlOutOfRange)?;
    now_ms
        .checked_add(ttl_ms)
        .filter(|expires_at_ms| *expires_at_ms <= MAX_SAFE_JSON_INTEGER)
        .ok_or(AttachTimeError::ExpiryOutOfRange)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_comparison_includes_length_and_every_byte() {
        assert!(constant_time_equal(b"same", b"same"));
        assert!(!constant_time_equal(b"same", b"samf"));
        assert!(!constant_time_equal(b"same", b"same-longer"));
    }

    #[test]
    fn attach_expiry_is_checked_against_the_protocol_timestamp_domain() {
        assert_eq!(
            checked_attach_expiry_ms(MAX_SAFE_JSON_INTEGER - 1, Duration::from_millis(1)),
            Ok(MAX_SAFE_JSON_INTEGER)
        );
        assert_eq!(
            checked_attach_expiry_ms(MAX_SAFE_JSON_INTEGER, Duration::from_millis(1)),
            Err(AttachTimeError::ExpiryOutOfRange)
        );
        assert_eq!(
            checked_attach_expiry_ms(MAX_SAFE_JSON_INTEGER + 1, Duration::from_millis(1)),
            Err(AttachTimeError::CurrentTimeUnavailable)
        );
        assert_eq!(
            checked_attach_expiry_ms(0, Duration::MAX),
            Err(AttachTimeError::TtlOutOfRange)
        );
    }

    #[tokio::test]
    async fn authentication_is_framed_and_one_token_only() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let writer = tokio::spawn(async move {
            client.write_u32(5).await.unwrap();
            client.write_all(b"token").await.unwrap();
        });
        authenticate(&mut server, "token").await.unwrap();
        writer.await.unwrap();
    }

    #[tokio::test]
    async fn closed_output_peer_cannot_cancel_already_authenticated_input() {
        let (proxy_client, mut public_peer) = UnixStream::pair().unwrap();
        let (proxy_rmux, mut rmux_peer) = UnixStream::pair().unwrap();
        let payload = b"complete authenticated attach input".to_vec();

        public_peer.write_all(&payload).await.unwrap();
        public_peer.shutdown().await.unwrap();
        drop(public_peer);

        let proxy_rmux = proxy_rmux.into_std().unwrap();
        let rmux_peer_task = tokio::spawn(async move {
            let mut received = Vec::new();
            rmux_peer.read_to_end(&mut received).await.unwrap();
            rmux_peer.shutdown().await.unwrap();
            received
        });

        let proxy_result = tokio::time::timeout(
            Duration::from_secs(1),
            proxy_attach(proxy_client, proxy_rmux, b"unread initial output".to_vec()),
        )
        .await
        .expect("proxy must finish after both half-closes");
        let received = tokio::time::timeout(Duration::from_secs(1), rmux_peer_task)
            .await
            .expect("private peer must observe attach input and EOF")
            .unwrap();

        assert!(
            proxy_result.is_err(),
            "the closed public output peer must remain observable as an error"
        );
        assert_eq!(received, payload);
    }

    #[tokio::test]
    async fn proxy_preserves_initial_output_and_both_stream_directions() {
        let (proxy_client, mut public_peer) = UnixStream::pair().unwrap();
        let (proxy_rmux, mut rmux_peer) = UnixStream::pair().unwrap();
        let initial_output = b"initial attach output".to_vec();
        let public_input = b"public attach input".to_vec();
        let private_output = b"private attach output".to_vec();

        let expected_initial = initial_output.clone();
        let sent_public_input = public_input.clone();
        let public_peer_task = tokio::spawn(async move {
            let mut initial = vec![0_u8; expected_initial.len()];
            public_peer.read_exact(&mut initial).await.unwrap();
            public_peer.write_all(&sent_public_input).await.unwrap();
            public_peer.shutdown().await.unwrap();
            let mut streamed = Vec::new();
            public_peer.read_to_end(&mut streamed).await.unwrap();
            (initial, streamed)
        });

        let sent_private_output = private_output.clone();
        let rmux_peer_task = tokio::spawn(async move {
            let mut received = Vec::new();
            rmux_peer.read_to_end(&mut received).await.unwrap();
            rmux_peer.write_all(&sent_private_output).await.unwrap();
            rmux_peer.shutdown().await.unwrap();
            received
        });

        let proxy_result = tokio::time::timeout(
            Duration::from_secs(1),
            proxy_attach(
                proxy_client,
                proxy_rmux.into_std().unwrap(),
                initial_output.clone(),
            ),
        )
        .await
        .expect("proxy must finish after both peers half-close");
        let (observed_initial, observed_private_output) = public_peer_task.await.unwrap();
        let observed_public_input = rmux_peer_task.await.unwrap();

        proxy_result.unwrap();
        assert_eq!(observed_initial, initial_output);
        assert_eq!(observed_private_output, private_output);
        assert_eq!(observed_public_input, public_input);
    }

    #[tokio::test]
    async fn private_output_eof_releases_proxy_with_public_input_still_open() {
        let (proxy_client, mut public_peer) = UnixStream::pair().unwrap();
        let (proxy_rmux, mut rmux_peer) = UnixStream::pair().unwrap();

        // Half-close only the private output direction. The public peer and
        // private input reader deliberately remain open, so an unconditional
        // join would wait forever for another public input byte.
        rmux_peer.shutdown().await.unwrap();
        tokio::time::timeout(
            Duration::from_secs(1),
            proxy_attach(proxy_client, proxy_rmux.into_std().unwrap(), Vec::new()),
        )
        .await
        .expect("private output EOF must release the attach reservation")
        .unwrap();

        let mut trailing = [0_u8; 1];
        assert_eq!(public_peer.read(&mut trailing).await.unwrap(), 0);
        drop(rmux_peer);
    }

    #[tokio::test]
    async fn unused_capability_reports_completion_when_it_expires() {
        let runtime = tempfile::TempDir::new().unwrap();
        let (grant, completion) = grant_attach(
            runtime.path(),
            &runtime.path().join("private-rmux.sock"),
            "private-session".to_owned(),
            Duration::from_millis(5),
        )
        .await
        .unwrap();
        let outcome = tokio::time::timeout(Duration::from_secs(1), completion.wait())
            .await
            .expect("capability completion must resolve after its TTL");
        assert_eq!(outcome, AttachCompletionOutcome::Unused);
        assert!(!grant.endpoint.exists());
    }
}
