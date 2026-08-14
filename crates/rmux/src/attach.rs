use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::Path;

use thiserror::Error;

const MAX_TOKEN_BYTES: usize = 128;

#[derive(Debug, Error)]
pub enum AttachCapabilityError {
    #[error("invalid attach capability: {0}")]
    Invalid(String),
    #[error("attach capability transport failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("interactive rmux attachment failed: {0}")]
    Rmux(#[from] rmux_client::ClientError),
}

/// Consumes a pmux one-use attach grant and drives the caller's terminal.
///
/// The endpoint is a private proxy, not the rmux daemon socket, and the token
/// is sent in one bounded frame before raw attach bytes begin.
pub fn attach_capability_terminal(
    endpoint: &Path,
    token: &str,
) -> Result<(), AttachCapabilityError> {
    let stream = connect_attach_capability(endpoint, token)?;
    rmux_client::attach_terminal(stream)?;
    Ok(())
}

/// Authenticates to an attach proxy and returns its raw attach stream.
pub fn connect_attach_capability(
    endpoint: &Path,
    token: &str,
) -> Result<UnixStream, AttachCapabilityError> {
    if !endpoint.is_absolute() {
        return Err(AttachCapabilityError::Invalid(
            "endpoint must be absolute".into(),
        ));
    }
    if token.is_empty() || token.len() > MAX_TOKEN_BYTES || !token.is_ascii() {
        return Err(AttachCapabilityError::Invalid(
            "token must be bounded non-empty ASCII".into(),
        ));
    }
    let mut stream = UnixStream::connect(endpoint)?;
    let length = u32::try_from(token.len())
        .map_err(|_| AttachCapabilityError::Invalid("token is too large".into()))?;
    stream.write_all(&length.to_be_bytes())?;
    stream.write_all(token.as_bytes())?;
    stream.flush()?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::net::UnixListener;
    use tempfile::TempDir;

    #[test]
    fn client_writes_exact_bounded_token_frame() {
        let root = TempDir::new().unwrap();
        let endpoint = root.path().join("attach.sock");
        let listener = UnixListener::bind(&endpoint).unwrap();
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut header = [0; 4];
            stream.read_exact(&mut header).unwrap();
            let mut token = vec![0; u32::from_be_bytes(header) as usize];
            stream.read_exact(&mut token).unwrap();
            token
        });
        let stream = connect_attach_capability(&endpoint, "one-use-token").unwrap();
        drop(stream);
        assert_eq!(thread.join().unwrap(), b"one-use-token");
    }

    #[test]
    fn client_rejects_relative_endpoint_and_unbounded_token() {
        assert!(connect_attach_capability(Path::new("relative.sock"), "token").is_err());
        assert!(connect_attach_capability(Path::new("/tmp/nope"), &"x".repeat(129)).is_err());
    }
}
