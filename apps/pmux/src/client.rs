use anyhow::{Context, Result, bail};
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use pseudomux_protocol::{Request, Response, SessionId};
use std::path::PathBuf;
use tokio::net::UnixStream;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

#[derive(Clone)]
pub(crate) struct DaemonClient {
    pub(crate) sockets: Vec<PathBuf>,
}

impl DaemonClient {
    pub(crate) fn new(sockets: Vec<PathBuf>) -> Self {
        Self { sockets }
    }

    pub(crate) async fn send(&self, request: Request) -> Result<Response> {
        if self.sockets.is_empty() {
            bail!("no socket candidates provided; pass --socket or set PSEUDOMUX_SOCKET");
        }
        let payload = serde_json::to_vec(&request)?;
        let mut last_err = None;
        for socket in &self.sockets {
            match UnixStream::connect(socket).await {
                Ok(stream) => {
                    let mut framed = Framed::new(stream, LengthDelimitedCodec::new());
                    framed
                        .send(Bytes::from(payload.clone()))
                        .await
                        .with_context(|| {
                            format!("failed to send request via {}", socket.display())
                        })?;
                    return match framed.next().await {
                        Some(Ok(bytes)) => {
                            let data = bytes.freeze();
                            Ok(serde_json::from_slice(&data)?)
                        }
                        Some(Err(err)) => Err(err.into()),
                        None => bail!("daemon closed connection"),
                    };
                }
                Err(err) => {
                    last_err = Some((socket.clone(), err));
                }
            }
        }
        if let Some((path, err)) = last_err {
            bail!(
                "failed to connect to pmuxd sockets (last {}): {}",
                path.display(),
                err
            );
        }
        bail!("failed to connect to pmuxd sockets")
    }
}

/// Open a raw UDS connection to the daemon (for streaming commands).
pub(crate) async fn connect_to_daemon(sockets: &[PathBuf]) -> Result<UnixStream> {
    if sockets.is_empty() {
        bail!("no socket candidates provided; pass --socket or set PSEUDOMUX_SOCKET");
    }
    let mut last_err = None;
    for socket in sockets {
        match UnixStream::connect(socket).await {
            Ok(stream) => return Ok(stream),
            Err(err) => {
                last_err = Some((socket.clone(), err));
            }
        }
    }
    if let Some((path, err)) = last_err {
        bail!(
            "failed to connect to pmuxd sockets (last {}): {}",
            path.display(),
            err
        );
    }
    bail!("failed to connect to pmuxd sockets")
}

pub(crate) fn expect_ack(resp: Response) -> Result<()> {
    match resp {
        Response::Ack => Ok(()),
        Response::Error { code, message } => bail!("{code}: {message}"),
        other => bail!("unexpected response: {other:?}"),
    }
}

pub(crate) async fn resolve_session(client: &DaemonClient, value: &str) -> Result<SessionId> {
    if let Ok(session) = SessionId::parse_str(value) {
        return Ok(session);
    }

    let prefix = value.trim().to_ascii_lowercase();
    if prefix.is_empty() {
        bail!("session id prefix cannot be empty");
    }

    let resp = client.send(Request::ListSessions).await?;
    let sessions = match resp {
        Response::Sessions { sessions } => sessions,
        Response::Error { code, message } => bail!("{code}: {message}"),
        other => bail!("unexpected response resolving session prefix: {other:?}"),
    };
    let matches: Vec<_> = sessions
        .into_iter()
        .map(|summary| summary.session)
        .filter(|session| session.to_string().starts_with(&prefix))
        .collect();

    match matches.as_slice() {
        [session] => Ok(*session),
        [] => bail!("no session id starts with prefix {value:?}"),
        many => {
            let ids = many
                .iter()
                .map(SessionId::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            bail!("session prefix {value:?} is ambiguous: {ids}")
        }
    }
}
