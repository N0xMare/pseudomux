//! Loopback Messages listener: pin header, release, catalog.
//!
//! Harnesses POST `/v1/messages` themselves (Anthropic Messages). This client
//! does not.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use serde_json::Value;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

pub const CONVERSATION_HEADER: &str = "x-pmux-conversation";
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum MessagesError {
    #[error("{0}")]
    InvalidConfig(String),
    #[error("messages HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub struct MessagesClient {
    host: String,
    port: u16,
    api_key: String,
    read_timeout: Duration,
    max_response_bytes: usize,
}

impl MessagesClient {
    pub fn new(base_url: impl AsRef<str>) -> Result<Self, MessagesError> {
        let raw = base_url.as_ref().trim().trim_end_matches('/');
        let without_scheme = raw.strip_prefix("http://").ok_or_else(|| {
            MessagesError::InvalidConfig("Messages URL must be http://HOST:PORT".into())
        })?;
        let hostport = without_scheme.split('/').next().unwrap_or(without_scheme);
        let (host, port) = parse_loopback_hostport(hostport)?;
        Ok(Self {
            host,
            port,
            api_key: "pmux".to_owned(),
            read_timeout: EXCHANGE_TIMEOUT,
            max_response_bytes: MAX_RESPONSE_BYTES,
        })
    }

    #[must_use]
    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        let key = key.into();
        let trimmed = key.trim();
        if !trimmed.is_empty() {
            self.api_key = trimmed.to_owned();
        }
        self
    }

    pub fn conversation_header(id: &str) -> Result<(&'static str, String), MessagesError> {
        Ok((CONVERSATION_HEADER, path_safe_conversation_id(id)?))
    }

    pub async fn release(&self, conversation_id: &str) -> Result<(), MessagesError> {
        let id = path_safe_conversation_id(conversation_id)?;
        let path = format!("/v1/conversations/{id}/release");
        let (_status, _body) = self.exchange("POST", &path, "").await?;
        Ok(())
    }

    pub async fn models(&self) -> Result<Value, MessagesError> {
        let (_status, body) = self.exchange("GET", "/v1/models", "").await?;
        Ok(serde_json::from_str(&body)?)
    }

    pub async fn capabilities(&self) -> Result<Value, MessagesError> {
        let (_status, body) = self.exchange("GET", "/v1/capabilities", "").await?;
        Ok(serde_json::from_str(&body)?)
    }

    async fn exchange(
        &self,
        method: &str,
        path: &str,
        body: &str,
    ) -> Result<(u16, String), MessagesError> {
        refuse_header_value(&self.api_key)?;
        let work = async {
            let mut stream = TcpStream::connect((self.host.as_str(), self.port)).await?;
            let mut request = format!(
                "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nx-api-key: {}\r\nAuthorization: Bearer {}\r\n",
                self.host_header(),
                self.api_key,
                self.api_key
            );
            if !body.is_empty() {
                request.push_str(&format!("Content-Length: {}\r\n", body.len()));
            }
            request.push_str("\r\n");
            request.push_str(body);
            stream.write_all(request.as_bytes()).await?;
            stream.shutdown().await?;
            let buf = read_bounded(&mut stream, self.max_response_bytes).await?;
            let text = String::from_utf8_lossy(&buf);
            let (head, rest) = text.split_once("\r\n\r\n").ok_or_else(|| {
                MessagesError::InvalidConfig(format!("no HTTP header terminator in {text:?}"))
            })?;
            let status = head
                .lines()
                .next()
                .unwrap_or("")
                .split_whitespace()
                .nth(1)
                .and_then(|code| code.parse().ok())
                .unwrap_or(0);
            if status != 200 {
                return Err(MessagesError::Http {
                    status,
                    body: rest.to_owned(),
                });
            }
            Ok((status, rest.to_owned()))
        };
        match timeout(self.read_timeout, work).await {
            Ok(result) => result,
            Err(_) => {
                Err(io::Error::new(io::ErrorKind::TimedOut, "Messages HTTP timed out").into())
            }
        }
    }

    fn host_header(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

fn refuse_header_value(value: &str) -> Result<(), MessagesError> {
    if value.contains(['\r', '\n', '\0']) {
        return Err(MessagesError::InvalidConfig(
            "api_key contains CR, LF, or NUL".into(),
        ));
    }
    Ok(())
}

async fn read_bounded(stream: &mut TcpStream, maximum: usize) -> io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Ok(buf);
        }
        if buf.len().saturating_add(n) > maximum {
            return Err(io::Error::other(format!(
                "Messages HTTP response exceeds {maximum} bytes"
            )));
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

fn is_allowed_messages_ip(ip: IpAddr) -> bool {
    ip == IpAddr::V4(Ipv4Addr::LOCALHOST) || ip == IpAddr::V6(Ipv6Addr::LOCALHOST)
}

fn path_safe_conversation_id(id: &str) -> Result<String, MessagesError> {
    let id = id.trim();
    if id.is_empty() {
        return Err(MessagesError::InvalidConfig(
            "conversation id must not be empty".into(),
        ));
    }
    if id.contains(['/', '?', '#']) || id.chars().any(char::is_whitespace) {
        return Err(MessagesError::InvalidConfig(
            "conversation id is not path-safe".into(),
        ));
    }
    Ok(id.to_owned())
}

fn parse_loopback_hostport(hostport: &str) -> Result<(String, u16), MessagesError> {
    if let Ok(addr) = hostport.parse::<SocketAddr>() {
        if !is_allowed_messages_ip(addr.ip()) {
            return Err(MessagesError::InvalidConfig(
                "Messages client is loopback-only".into(),
            ));
        }
        let host = match addr.ip() {
            IpAddr::V4(ip) => ip.to_string(),
            IpAddr::V6(ip) => ip.to_string(),
        };
        return Ok((host, addr.port()));
    }
    // `localhost` is a name. Keep it and connect via ToSocketAddrs.
    let Some((host, port)) = hostport.rsplit_once(':') else {
        return Err(MessagesError::InvalidConfig(format!(
            "cannot parse {hostport}"
        )));
    };
    if !host.eq_ignore_ascii_case("localhost") {
        return Err(MessagesError::InvalidConfig(format!(
            "cannot parse {hostport}"
        )));
    }
    let port = port.parse::<u16>().map_err(|error| {
        MessagesError::InvalidConfig(format!("cannot parse {hostport}: {error}"))
    })?;
    Ok((host.to_owned(), port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_http_url_parses() {
        assert!(MessagesClient::new("http://127.0.0.1:8765").is_ok());
        assert!(MessagesClient::new("http://localhost:8765").is_ok());
        assert!(MessagesClient::new("http://[::1]:8765").is_ok());
        assert!(MessagesClient::new("https://127.0.0.1:8765").is_err());
        assert!(MessagesClient::new("http://192.168.1.4:8765").is_err());
        let Err(other_loopback) = MessagesClient::new("http://127.0.0.2:8765") else {
            panic!("127.0.0.2 must be refused");
        };
        assert!(
            other_loopback.to_string().contains("loopback-only"),
            "{other_loopback}"
        );
    }

    #[test]
    fn conversation_header_is_the_pin() {
        let (name, value) = MessagesClient::conversation_header(" abc ").unwrap();
        assert_eq!(name, CONVERSATION_HEADER);
        assert_eq!(value, "abc");
        let empty = MessagesClient::conversation_header("  ").unwrap_err();
        assert!(empty.to_string().contains("must not be empty"), "{empty}");
        let blank = MessagesClient::conversation_header("").unwrap_err();
        assert!(blank.to_string().contains("must not be empty"), "{blank}");
        for id in ["a/b", "a b", "a?b", "a#b"] {
            let err = MessagesClient::conversation_header(id).unwrap_err();
            assert!(err.to_string().contains("path-safe"), "{id}: {err}");
        }
    }

    #[test]
    fn localhost_is_kept_as_a_name() {
        let client = MessagesClient::new("http://localhost:8765").unwrap();
        assert_eq!(client.host, "localhost");
        assert_eq!(client.port, 8765);
        assert_eq!(client.host_header(), "localhost:8765");
    }

    #[test]
    fn with_api_key_trims_like_the_other_clients() {
        let client = MessagesClient::new("http://127.0.0.1:8765")
            .unwrap()
            .with_api_key(" k ");
        assert_eq!(client.api_key, "k");
        let blank = MessagesClient::new("http://127.0.0.1:8765")
            .unwrap()
            .with_api_key("  ");
        assert_eq!(blank.api_key, "pmux");
    }

    #[tokio::test]
    async fn release_refuses_empty_whitespace_and_path_unsafe_ids() {
        let client = MessagesClient::new("http://127.0.0.1:1").unwrap();
        for id in ["", "  ", "\t"] {
            let err = client.release(id).await.unwrap_err();
            assert!(
                err.to_string().contains("must not be empty"),
                "{id:?}: {err}"
            );
        }
        for id in ["a/b", "a b", "a?b", "a#b"] {
            let err = client.release(id).await.unwrap_err();
            assert!(err.to_string().contains("path-safe"), "{id}: {err}");
        }
    }

    #[tokio::test]
    async fn release_puts_path_safe_ids_in_the_path_verbatim() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let mut seen = Vec::new();
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let head = read_http_head(&mut stream).await;
                let line = head.lines().next().unwrap_or("").to_owned();
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                    )
                    .await
                    .unwrap();
                seen.push(line);
            }
            seen
        });
        let client = MessagesClient::new(format!("http://127.0.0.1:{port}")).unwrap();
        client.release("a!b").await.unwrap();
        client.release("100%").await.unwrap();
        assert_eq!(
            server.await.unwrap(),
            [
                "POST /v1/conversations/a!b/release HTTP/1.1",
                "POST /v1/conversations/100%/release HTTP/1.1",
            ]
        );
    }

    async fn read_http_head(stream: &mut TcpStream) -> String {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            let n = stream.read(&mut chunk).await.unwrap();
            assert_ne!(n, 0, "eof before HTTP header terminator");
            buf.extend_from_slice(&chunk[..n]);
            if buf.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
            assert!(buf.len() < 16 * 1024, "HTTP head too large");
        }
        String::from_utf8_lossy(&buf).into_owned()
    }

    #[tokio::test]
    async fn api_key_with_cr_lf_or_nul_is_refused_before_headers() {
        for key in ["a\nb", "a\rb", "a\0b", "a\r\nb"] {
            let err = MessagesClient::new("http://127.0.0.1:1")
                .unwrap()
                .with_api_key(key)
                .models()
                .await
                .unwrap_err();
            assert!(err.to_string().contains("CR, LF, or NUL"), "{key:?}: {err}");
        }
    }

    #[tokio::test]
    async fn exchange_times_out_when_the_peer_sends_nothing() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        let mut client = MessagesClient::new(format!("http://127.0.0.1:{port}")).unwrap();
        client.read_timeout = Duration::from_millis(50);
        let err = client.models().await.unwrap_err();
        assert!(err.to_string().contains("timed out"), "{err}");
        server.abort();
    }

    #[tokio::test]
    async fn exchange_refuses_an_oversized_response() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let _ = read_http_head(&mut stream).await;
            let body = vec![b'x'; 64];
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            stream.write_all(&body).await.unwrap();
        });
        let mut client = MessagesClient::new(format!("http://127.0.0.1:{port}")).unwrap();
        client.max_response_bytes = 16;
        let err = client.models().await.unwrap_err();
        assert!(err.to_string().contains("exceeds"), "{err}");
        let _ = server.await;
    }
}
