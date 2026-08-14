//! Minimal stdin-to-UDS adapter for pmux Hybrid lifecycle hooks.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use clap::{Parser, ValueEnum};
use pseudomux_service::hybrid_hooks::{
    LifecycleEventKind, MAX_HOOK_FRAME_BYTES, send_hook_payload,
};
use serde_json::Value;
#[cfg(test)]
use tokio::io::{AsyncRead, AsyncReadExt};
use uuid::Uuid;

const HOOK_CLIENT_IO_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// Absolute path to the owner-only Hybrid relay socket.
    #[arg(long)]
    socket: PathBuf,

    /// Session UUID the receiving pmux instance assigned.
    #[arg(long)]
    session_id: Uuid,

    /// Claude lifecycle event represented by stdin.
    #[arg(long)]
    event: EventArg,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum EventArg {
    #[value(name = "SessionStart")]
    SessionStart,
    #[value(name = "Stop")]
    Stop,
    #[value(name = "StopFailure")]
    StopFailure,
}

impl From<EventArg> for LifecycleEventKind {
    fn from(event: EventArg) -> Self {
        match event {
            EventArg::SessionStart => Self::SessionStart,
            EventArg::Stop => Self::Stop,
            EventArg::StopFailure => Self::StopFailure,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tokio::time::timeout(HOOK_CLIENT_IO_TIMEOUT, forward_process_stdin(Cli::parse()))
        .await
        .context("Hybrid hook operation timed out")??;
    Ok(())
}

async fn forward_process_stdin(cli: Cli) -> Result<()> {
    ensure!(
        cli.socket.is_absolute(),
        "relay socket path must be absolute"
    );
    let payload = read_nonblocking_process_stdin().await?;
    send_hook_payload(&cli.socket, cli.session_id, cli.event.into(), payload).await
}

#[cfg(test)]
async fn forward(cli: Cli, reader: impl AsyncRead + Unpin) -> Result<()> {
    ensure!(
        cli.socket.is_absolute(),
        "relay socket path must be absolute"
    );
    let payload = read_payload(reader).await?;
    send_hook_payload(&cli.socket, cli.session_id, cli.event.into(), payload).await
}

#[cfg(test)]
async fn read_payload(reader: impl AsyncRead + Unpin) -> Result<Value> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_HOOK_FRAME_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .context("failed to read hook payload from stdin")?;
    decode_payload(&bytes)
}

fn decode_payload(bytes: &[u8]) -> Result<Value> {
    ensure!(!bytes.is_empty(), "hook payload is empty");
    ensure!(
        bytes.len() <= MAX_HOOK_FRAME_BYTES,
        "hook payload exceeds the size limit"
    );
    serde_json::from_slice(bytes).context("hook payload is not valid JSON")
}

#[cfg(unix)]
struct NonblockingStdin {
    fd: std::os::fd::RawFd,
    original_flags: libc::c_int,
}

#[cfg(unix)]
impl NonblockingStdin {
    #[allow(unsafe_code)]
    fn acquire() -> Result<Self> {
        let fd = libc::STDIN_FILENO;
        // SAFETY: `fcntl` inspects and updates flags for the process-owned
        // standard-input descriptor without dereferencing pointers.
        let original_flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if original_flags == -1 {
            return Err(std::io::Error::last_os_error())
                .context("failed to inspect hook stdin flags");
        }
        // SAFETY: the descriptor and flag value were validated by F_GETFL.
        if unsafe { libc::fcntl(fd, libc::F_SETFL, original_flags | libc::O_NONBLOCK) } == -1 {
            return Err(std::io::Error::last_os_error())
                .context("failed to make hook stdin nonblocking");
        }
        Ok(Self { fd, original_flags })
    }

    #[allow(unsafe_code)]
    fn read(&self, buffer: &mut [u8]) -> std::io::Result<usize> {
        // SAFETY: `buffer` is writable for its exact length and stdin remains
        // process-owned for the lifetime of this non-owning wrapper.
        let read = unsafe {
            libc::read(
                self.fd,
                buffer.as_mut_ptr().cast::<libc::c_void>(),
                buffer.len(),
            )
        };
        if read == -1 {
            Err(std::io::Error::last_os_error())
        } else {
            usize::try_from(read)
                .map_err(|_| std::io::Error::other("hook stdin returned an invalid byte count"))
        }
    }
}

#[cfg(unix)]
impl std::os::fd::AsRawFd for NonblockingStdin {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.fd
    }
}

#[cfg(unix)]
impl Drop for NonblockingStdin {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: best-effort restoration targets the same non-owning stdin
        // descriptor captured by `acquire`.
        let _ = unsafe { libc::fcntl(self.fd, libc::F_SETFL, self.original_flags) };
    }
}

#[cfg(unix)]
async fn read_nonblocking_process_stdin() -> Result<Value> {
    let stdin = tokio::io::unix::AsyncFd::new(NonblockingStdin::acquire()?)
        .context("failed to monitor hook stdin")?;
    let mut bytes = Vec::new();
    loop {
        let mut ready = stdin
            .readable()
            .await
            .context("failed to wait for hook stdin")?;
        let remaining = (MAX_HOOK_FRAME_BYTES + 1).saturating_sub(bytes.len());
        let mut chunk = vec![0_u8; remaining.min(8 * 1024)];
        let read = match ready.try_io(|source| source.get_ref().read(&mut chunk)) {
            Ok(result) => result.context("failed to read hook payload from stdin")?,
            Err(_) => continue,
        };
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.len() > MAX_HOOK_FRAME_BYTES {
            break;
        }
    }
    decode_payload(&bytes)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::time::Duration;

    use clap::error::ErrorKind;
    use pseudomux_protocol::v1::LifecycleMode;
    use pseudomux_service::hybrid_hooks::{PreparedLifecycle, prepare_lifecycle};
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;

    fn private_runtime() -> TempDir {
        let runtime = tempfile::tempdir().unwrap();
        fs::set_permissions(runtime.path(), fs::Permissions::from_mode(0o700)).unwrap();
        runtime
    }

    #[test]
    fn all_arguments_are_explicit_and_event_names_are_exact() {
        let missing = Cli::try_parse_from(["pmux-hook"]).unwrap_err().kind();
        assert_eq!(missing, ErrorKind::MissingRequiredArgument);

        let parsed = Cli::try_parse_from([
            "pmux-hook",
            "--socket",
            "/tmp/relay.sock",
            "--session-id",
            "00000000-0000-0000-0000-000000000001",
            "--event",
            "StopFailure",
        ])
        .unwrap();
        assert!(matches!(parsed.event, EventArg::StopFailure));

        assert!(
            Cli::try_parse_from([
                "pmux-hook",
                "--socket",
                "/tmp/relay.sock",
                "--session-id",
                "00000000-0000-0000-0000-000000000001",
                "--event",
                "stop-failure",
            ])
            .is_err()
        );
    }

    #[tokio::test]
    async fn stdin_json_is_bounded_and_validated_before_connecting() {
        assert!(read_payload(&b""[..]).await.is_err());
        assert!(read_payload(&b"{"[..]).await.is_err());
        assert!(
            read_payload(vec![b' '; MAX_HOOK_FRAME_BYTES + 1].as_slice())
                .await
                .is_err()
        );
        assert_eq!(read_payload(&b"{}"[..]).await.unwrap(), json!({}));
    }

    #[tokio::test]
    async fn forwards_one_lifecycle_observation() {
        let runtime = private_runtime();
        let session_id = Uuid::new_v4();
        let mut prepared = prepare_lifecycle(
            &LifecycleMode::Hybrid {
                hook_timeout_ms: 5_000,
            },
            runtime.path(),
            session_id,
            &std::env::current_exe().unwrap(),
            &[],
        )
        .await
        .unwrap();
        let socket = prepared.hybrid().unwrap().socket_path().to_path_buf();
        let transcript = runtime.path().join("transcript.jsonl");
        let payload = serde_json::to_vec(&json!({
            "session_id": session_id,
            "hook_event_name": "Stop",
            "transcript_path": transcript,
            "output": "pmux-hook-private-output",
            "usage": {"marker": "pmux-hook-private-usage"},
        }))
        .unwrap();

        forward(
            Cli {
                socket,
                session_id,
                event: EventArg::Stop,
            },
            payload.as_slice(),
        )
        .await
        .unwrap();

        let observation = tokio::time::timeout(
            Duration::from_secs(1),
            prepared.hybrid_mut().unwrap().recv(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(observation.event(), LifecycleEventKind::Stop);
        assert_eq!(observation.transcript_path(), Some(Path::new(&transcript)));
        let debug = format!("{observation:?}");
        assert!(!debug.contains("pmux-hook-private-output"));
        assert!(!debug.contains("pmux-hook-private-usage"));

        assert!(matches!(prepared, PreparedLifecycle::Hybrid(_)));
    }
}
