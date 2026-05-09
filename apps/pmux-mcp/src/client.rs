use anyhow::{Result, bail};
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use pseudomux_protocol::{Request, Response};
use pseudomux_service::socket_candidates;
use std::path::PathBuf;
use tokio::net::UnixStream;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

#[derive(Clone)]
pub struct DaemonClient {
    pub(crate) sockets: Vec<PathBuf>,
}

impl DaemonClient {
    pub fn connect() -> Self {
        Self {
            sockets: socket_candidates(),
        }
    }

    pub async fn send(&self, request: Request) -> Result<Response> {
        if self.sockets.is_empty() {
            bail!("no pmuxd socket candidates found");
        }
        let payload = serde_json::to_vec(&request)?;
        let mut last_err = None;
        for socket in &self.sockets {
            match UnixStream::connect(socket).await {
                Ok(stream) => {
                    let mut framed = Framed::new(stream, LengthDelimitedCodec::new());
                    framed.send(Bytes::from(payload.clone())).await?;
                    return match framed.next().await {
                        Some(Ok(bytes)) => Ok(serde_json::from_slice(&bytes.freeze())?),
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
            bail!("failed to connect to pmuxd ({}): {}", path.display(), err);
        }
        bail!("failed to connect to pmuxd")
    }

    pub async fn connect_stream(&self) -> Result<UnixStream> {
        if self.sockets.is_empty() {
            bail!("no pmuxd socket candidates found");
        }
        let mut last_err = None;
        for socket in &self.sockets {
            match UnixStream::connect(socket).await {
                Ok(stream) => return Ok(stream),
                Err(err) => {
                    last_err = Some((socket.clone(), err));
                }
            }
        }
        if let Some((path, err)) = last_err {
            bail!("failed to connect to pmuxd ({}): {}", path.display(), err);
        }
        bail!("failed to connect to pmuxd")
    }
}
