use std::future::Future;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use pseudomux_protocol::v1::{
    ErrorBody, ErrorCode, MAX_NATIVE_FRAME_BYTES, MAX_SAFE_JSON_INTEGER, NativeFrameAccumulator,
    NativeFrameProgress, PROTOCOL_VERSION, Request, RequestEnvelope, RequestId, ResponseEnvelope,
    ResponseResult, caller_actionable_decode_refusal,
};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::warn;

pub(crate) const MAX_FRAME_BYTES: usize = MAX_NATIVE_FRAME_BYTES;
const DEFAULT_FRAME_READ_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_FRAME_WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const UNKNOWN_REQUEST_ID: RequestId = RequestId::nil();

/// The transport depends only on this Claude-aware v1 dispatch boundary.
/// Production uses `pseudomux_service::NativeService`; tests use a fake.
#[async_trait]
pub(crate) trait RequestDispatcher: Send + Sync + 'static {
    async fn dispatch(&self, request: Request) -> Result<ResponseResult, ErrorBody>;
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ServerLimits {
    pub max_connections: usize,
    pub shutdown_grace: Duration,
    pub frame_read_timeout: Duration,
    pub frame_write_timeout: Duration,
}

impl Default for ServerLimits {
    fn default() -> Self {
        Self {
            max_connections: 64,
            shutdown_grace: Duration::from_secs(5),
            frame_read_timeout: DEFAULT_FRAME_READ_TIMEOUT,
            frame_write_timeout: DEFAULT_FRAME_WRITE_TIMEOUT,
        }
    }
}

/// Serves accepted connections with bounded concurrency until `shutdown` fires.
/// Existing requests receive a grace period; remaining tasks are then aborted.
pub(crate) async fn serve_until<D, F>(
    listener: UnixListener,
    dispatcher: Arc<D>,
    limits: ServerLimits,
    shutdown: F,
) -> io::Result<()>
where
    D: RequestDispatcher + ?Sized,
    F: Future<Output = ()>,
{
    let semaphore = Arc::new(Semaphore::new(limits.max_connections.max(1)));
    let mut tasks = JoinSet::new();
    let mut accept_error = None;
    tokio::pin!(shutdown);

    loop {
        while let Some(result) = tasks.try_join_next() {
            if let Err(error) = result {
                warn!(
                    cancelled = error.is_cancelled(),
                    "pmuxd connection task ended unexpectedly"
                );
            }
        }
        let accepted = tokio::select! {
            biased;
            () = &mut shutdown => break,
            result = listener.accept() => result,
        };
        let accepted = match accepted {
            Ok(accepted) => accepted,
            Err(error) => {
                accept_error = Some(error);
                break;
            }
        };
        let (stream, _) = accepted;
        let permit = tokio::select! {
            biased;
            () = &mut shutdown => break,
            result = Arc::clone(&semaphore).acquire_owned() => {
                result.map_err(|_| io::Error::other("connection semaphore closed"))?
            }
        };
        let dispatcher = Arc::clone(&dispatcher);
        tasks.spawn(async move {
            let _permit = permit;
            if let Err(error) = handle_client_with_limits(stream, dispatcher, limits).await {
                // Never log request payloads or dispatcher errors here. The only
                // unhandled failures are transport errors and their broad kind is
                // sufficient for operations.
                warn!(error_kind = ?error.kind(), "pmuxd client transport ended");
            }
        });
    }

    drop(listener);
    let drain = async {
        while let Some(result) = tasks.join_next().await {
            if let Err(error) = result {
                warn!(
                    cancelled = error.is_cancelled(),
                    "pmuxd connection task ended unexpectedly"
                );
            }
        }
    };
    if tokio::time::timeout(limits.shutdown_grace, drain)
        .await
        .is_err()
    {
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
    }
    match accept_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Handles one connection sequentially: exactly one response is written for
/// every complete frame. Oversized frames receive one compact error and close
/// because their unread body cannot be safely resynchronized.
#[cfg(test)]
async fn handle_client<D>(stream: UnixStream, dispatcher: Arc<D>) -> io::Result<()>
where
    D: RequestDispatcher + ?Sized,
{
    handle_client_with_limits(stream, dispatcher, ServerLimits::default()).await
}

async fn handle_client_with_limits<D>(
    mut stream: UnixStream,
    dispatcher: Arc<D>,
    limits: ServerLimits,
) -> io::Result<()>
where
    D: RequestDispatcher + ?Sized,
{
    loop {
        let payload = match tokio::time::timeout(limits.frame_read_timeout, read_frame(&mut stream))
            .await
            .map_err(|_| {
                io::Error::new(io::ErrorKind::TimedOut, "request frame read timed out")
            })?? {
            FrameRead::Eof => return Ok(()),
            FrameRead::Payload(payload) => payload,
            FrameRead::Oversized(length) => {
                let response = ResponseEnvelope::failure(
                    UNKNOWN_REQUEST_ID,
                    ErrorBody::new(
                        ErrorCode::InvalidConfig,
                        format!("frame length {length} exceeds {MAX_FRAME_BYTES} byte limit"),
                    )
                    .with_details(json!({
                        "maximum_bytes": MAX_FRAME_BYTES,
                        "actual_bytes": length,
                    })),
                );
                write_response_with_timeout(&mut stream, response, limits.frame_write_timeout)
                    .await?;
                return Ok(());
            }
        };

        let response = match decode_request(&payload) {
            Ok(envelope) => {
                let request_id = envelope.request_id;
                match dispatcher.dispatch(envelope.request).await {
                    Ok(result) => ResponseEnvelope::success(request_id, result),
                    Err(error) => ResponseEnvelope::failure(request_id, error),
                }
            }
            Err((request_id, error)) => ResponseEnvelope::failure(request_id, error),
        };
        write_response_with_timeout(&mut stream, response, limits.frame_write_timeout).await?;
    }
}

fn decode_request(payload: &[u8]) -> Result<RequestEnvelope, (RequestId, ErrorBody)> {
    decode_request_with_work(payload).0
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct DecodeWork {
    typed_json_passes: usize,
    recovery_json_passes: usize,
}

fn decode_request_with_work(
    payload: &[u8],
) -> (Result<RequestEnvelope, (RequestId, ErrorBody)>, DecodeWork) {
    let mut work = DecodeWork {
        typed_json_passes: 1,
        recovery_json_passes: 0,
    };
    match serde_json::from_slice::<RequestEnvelope>(payload) {
        Ok(envelope) if envelope.version == PROTOCOL_VERSION => (Ok(envelope), work),
        Ok(envelope) => {
            let error = ErrorBody::new(
                ErrorCode::ProtocolVersionMismatch,
                format!("unsupported protocol version {}", envelope.version),
            )
            .with_details(json!({
                "expected": PROTOCOL_VERSION,
                "actual": envelope.version,
            }));
            (Err((envelope.request_id, error)), work)
        }
        Err(typed_error) => {
            // The typed parse remains authoritative. This recovery parse only
            // classifies the failure and salvages correlation/version fields;
            // it can never accept a frame (including duplicate object keys).
            work.recovery_json_passes = 1;
            let value: Value = match serde_json::from_slice(payload) {
                Ok(value) => value,
                Err(error) => {
                    let error =
                        ErrorBody::new(ErrorCode::InvalidConfig, "request frame is not valid JSON")
                            .with_details(json!({ "category": format!("{:?}", error.classify()) }));
                    return (Err((UNKNOWN_REQUEST_ID, error)), work);
                }
            };
            let request_id = value
                .get("request_id")
                .and_then(Value::as_str)
                .and_then(|value| value.parse().ok())
                .unwrap_or(UNKNOWN_REQUEST_ID);

            let Some(version) = value.get("version").and_then(Value::as_u64) else {
                let error = ErrorBody::new(
                    ErrorCode::ProtocolVersionMismatch,
                    "request version must be an unsigned integer",
                )
                .with_details(json!({ "expected": PROTOCOL_VERSION }));
                return (Err((request_id, error)), work);
            };
            if version != u64::from(PROTOCOL_VERSION) {
                let error = ErrorBody::new(
                    ErrorCode::ProtocolVersionMismatch,
                    format!("unsupported protocol version {version}"),
                )
                .with_details(json!({
                    "expected": PROTOCOL_VERSION,
                    "actual": diagnostic_u64(version),
                }));
                return (Err((request_id, error)), work);
            }

            // CONTENT-FREE BY DEFAULT, AND ACTIONABLE ONLY WHERE pmux WROTE
            // THE SENTENCE. Forwarding a decoder's rendered text wholesale
            // would return the caller's own values: MEASURED,
            // `{"environment":{"set":{"SECRET":42}}}` renders as ``invalid
            // type: integer `42`, expected a string``, and a request frame
            // carries environment values, inline settings and MCP documents,
            // and system prompts. So the classification stays content-free, and
            // `caller_actionable_decode_refusal` returns only the span this
            // protocol crate composed out of field paths -- which is what makes
            // "refused by name" true on the wire rather than only in the type.
            let message = caller_actionable_decode_refusal(&typed_error);
            let error = ErrorBody::new(
                ErrorCode::InvalidConfig,
                message.unwrap_or_else(|| "request does not match protocol v1".to_owned()),
            )
            .with_details(json!({ "category": format!("{:?}", typed_error.classify()) }));
            (Err((request_id, error)), work)
        }
    }
}

enum FrameRead {
    Eof,
    Payload(Vec<u8>),
    Oversized(u32),
}

async fn read_frame<R>(stream: &mut R) -> io::Result<FrameRead>
where
    R: AsyncRead + Unpin,
{
    let mut accumulator = NativeFrameAccumulator::new();
    let mut scratch = [0_u8; 8 * 1024];
    loop {
        // Never read beyond the current frame boundary. A following request
        // remains in the socket for the sequential connection loop.
        let requested = accumulator.remaining_bytes().min(scratch.len());
        let read = stream.read(&mut scratch[..requested]).await?;
        if read == 0 {
            return if accumulator.is_empty() {
                Ok(FrameRead::Eof)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "native request frame ended before its declared length",
                ))
            };
        }
        let (consumed, progress) = accumulator.push(&scratch[..read]);
        if consumed != read {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "native frame accumulator crossed a request boundary",
            ));
        }
        match progress {
            NativeFrameProgress::NeedMore => {}
            NativeFrameProgress::Payload(payload) => return Ok(FrameRead::Payload(payload)),
            NativeFrameProgress::Oversized { advertised_bytes } => {
                return Ok(FrameRead::Oversized(advertised_bytes));
            }
        }
    }
}

fn encode_response_payload(response: ResponseEnvelope) -> io::Result<Vec<u8>> {
    let request_id = response.request_id;
    let mut payload = match serde_json::to_vec(&response) {
        Ok(payload) => payload,
        Err(_) => encode_compact_failure(
            request_id,
            ErrorBody::new(
                ErrorCode::Internal,
                "response could not be serialized within protocol v1",
            ),
        )?,
    };
    if payload.len() > MAX_FRAME_BYTES {
        let actual_bytes = payload.len();
        payload = encode_compact_failure(
            request_id,
            ErrorBody::new(
                ErrorCode::ResultTooLarge,
                "response exceeds transport frame limit",
            )
            .with_details(json!({
                "actual_bytes": diagnostic_usize(actual_bytes),
                "maximum_bytes": diagnostic_usize(MAX_FRAME_BYTES),
            })),
        )?;
    }
    if payload.len() > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bounded response replacement exceeds the native frame limit",
        ));
    }
    Ok(payload)
}

fn encode_compact_failure(request_id: RequestId, error: ErrorBody) -> io::Result<Vec<u8>> {
    serde_json::to_vec(&ResponseEnvelope::failure(request_id, error)).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "invariant compact response could not be serialized",
        )
    })
}

fn diagnostic_u64(value: u64) -> Value {
    if value <= MAX_SAFE_JSON_INTEGER {
        value.into()
    } else {
        value.to_string().into()
    }
}

fn diagnostic_usize(value: usize) -> Value {
    u64::try_from(value).map_or_else(|_| value.to_string().into(), diagnostic_u64)
}

async fn write_response<W>(stream: &mut W, response: ResponseEnvelope) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let payload = encode_response_payload(response)?;
    let length = u32::try_from(payload.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "response frame is too large"))?;
    stream.write_all(&length.to_be_bytes()).await?;
    stream.write_all(&payload).await?;
    stream.flush().await
}

async fn write_response_with_timeout<W>(
    stream: &mut W,
    response: ResponseEnvelope,
    timeout: Duration,
) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    tokio::time::timeout(timeout, write_response(stream, response))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "response frame write timed out"))?
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Poll};

    use proptest::{
        prelude::*,
        test_runner::{Config as ProptestConfig, RngAlgorithm, RngSeed},
    };
    use pseudomux_protocol::v1::{
        Pong, ResponsePayload, RunTurnRequest, SessionGenerationId, TurnLeasePolicy, TurnRequest,
    };
    use tokio::io::ReadBuf;
    use tokio::sync::Notify;

    use super::*;

    const AGENT_ID: &str = "00000000-0000-4000-8000-000000000066";

    fn decode_message(payload: serde_json::Value) -> String {
        let bytes = serde_json::to_vec(&payload).expect("serializes");
        match decode_request(&bytes) {
            Ok(_) => panic!("this frame must be refused"),
            Err((_, error)) => error.message,
        }
    }

    /// A refusal pmux WROTE reaches the caller; one serde produced does not.
    ///
    /// **THIS IS THE DIFFERENCE BETWEEN "refused by name" AND A CLAIM.** The
    /// decoder composes a precise sentence for a start that names both an agent
    /// and a launch field -- and before the marker existed, the transport
    /// flattened it, and every one of them, to "request does not match protocol
    /// v1". MEASURED against a live daemon: the both-modes refusal, the
    /// neither-mode refusal and the zero-version refusal all arrived
    /// indistinguishable from a typo.
    ///
    /// The other direction is why the flattening was there in the first place
    /// and must stay for everything else: serde renders a type mismatch with
    /// the caller's own VALUE in it, and a start frame carries environment
    /// values, inline settings and MCP documents, and system prompts. The
    /// second half of this test is the one that would catch a future "just
    /// forward the decoder's message".
    #[test]
    fn a_decode_refusal_pmux_wrote_reaches_the_caller_and_one_serde_wrote_does_not() {
        let both = decode_message(json!({
            "version": 1,
            "request_id": "00000000-0000-4000-8000-000000000001",
            "method": "start_session",
            "params": {
                "identity": {"mode": "new"},
                "cwd": "/work",
                "agent": {"agent_id": AGENT_ID, "version": 1},
                "terminal": {"rows": 40, "cols": 132},
            },
        }));
        assert!(both.contains("`terminal`"), "{both}");
        assert!(
            both.contains("agent supplies the whole launch policy"),
            "{both}"
        );
        assert!(
            !both.contains("pmux-v1:") && !both.contains(" at line "),
            "neither the marker nor serde's position suffix belongs in a caller's message: {both}"
        );

        let neither = decode_message(json!({
            "version": 1,
            "request_id": "00000000-0000-4000-8000-000000000001",
            "method": "start_session",
            "params": {"identity": {"mode": "new"}, "cwd": "/work"},
        }));
        assert!(
            neither.contains("`claude`") && neither.contains("`agent`"),
            "{neither}"
        );

        let zero = decode_message(json!({
            "version": 1,
            "request_id": "00000000-0000-4000-8000-000000000001",
            "method": "start_session",
            "params": {
                "identity": {"mode": "new"},
                "cwd": "/work",
                "agent": {"agent_id": AGENT_ID, "version": 0},
            },
        }));
        assert!(zero.contains("starts at 1"), "{zero}");

        // ...and the direction that matters more. `SHOULD-NOT-ESCAPE` is a
        // value in the one channel documented as carrying secrets.
        let leaky = decode_message(json!({
            "version": 1,
            "request_id": "00000000-0000-4000-8000-000000000001",
            "method": "start_session",
            "params": {
                "identity": {"mode": "new"},
                "cwd": "/work",
                "claude": {"executable": "/bin/sh"},
                "environment": {"set": {"TOKEN": ["SHOULD-NOT-ESCAPE"]}},
            },
        }));
        assert!(
            !leaky.contains("SHOULD-NOT-ESCAPE"),
            "a caller's own environment value must never come back in a refusal: {leaky}"
        );
        assert_eq!(leaky, "request does not match protocol v1");
    }

    #[derive(Default)]
    struct FakeDispatcher {
        calls: AtomicUsize,
    }

    #[derive(Default)]
    struct UnsafeThenPingDispatcher {
        calls: AtomicUsize,
    }

    #[derive(Default)]
    struct OversizedThenPingDispatcher {
        calls: AtomicUsize,
    }

    struct CountingReader {
        bytes: Vec<u8>,
        offset: usize,
        bytes_read: usize,
        max_chunk: usize,
    }

    impl CountingReader {
        fn framed(payload: &[u8]) -> Self {
            Self::framed_with_chunk(payload, usize::MAX)
        }

        fn framed_with_chunk(payload: &[u8], max_chunk: usize) -> Self {
            let mut bytes = Vec::with_capacity(payload.len() + 4);
            bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
            bytes.extend_from_slice(payload);
            Self::from_bytes_with_chunk(bytes, max_chunk)
        }

        fn from_bytes_with_chunk(bytes: Vec<u8>, max_chunk: usize) -> Self {
            Self {
                bytes,
                offset: 0,
                bytes_read: 0,
                max_chunk: max_chunk.max(1),
            }
        }
    }

    impl AsyncRead for CountingReader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _context: &mut Context<'_>,
            buffer: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            let available = &self.bytes[self.offset..];
            let copied = available.len().min(buffer.remaining()).min(self.max_chunk);
            buffer.put_slice(&available[..copied]);
            self.offset += copied;
            self.bytes_read += copied;
            Poll::Ready(Ok(()))
        }
    }

    #[derive(Default)]
    struct CountingWriter {
        bytes_written: usize,
    }

    impl AsyncWrite for CountingWriter {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _context: &mut Context<'_>,
            buffer: &[u8],
        ) -> Poll<io::Result<usize>> {
            self.bytes_written += buffer.len();
            Poll::Ready(Ok(buffer.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[async_trait]
    impl RequestDispatcher for FakeDispatcher {
        async fn dispatch(&self, request: Request) -> Result<ResponseResult, ErrorBody> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match request {
                Request::Ping => Ok(ResponseResult::Pong(Pong {
                    server_version: "test".to_owned(),
                    protocol_version: PROTOCOL_VERSION,
                })),
                _ => Err(ErrorBody::new(
                    ErrorCode::UnsupportedFeature,
                    "fake only supports ping",
                )),
            }
        }
    }

    #[async_trait]
    impl RequestDispatcher for UnsafeThenPingDispatcher {
        async fn dispatch(&self, request: Request) -> Result<ResponseResult, ErrorBody> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            assert!(matches!(request, Request::Ping));
            if call == 0 {
                return Err(ErrorBody::new(
                    ErrorCode::RecoveryFailed,
                    "synthetic producer returned unsafe diagnostics",
                )
                .with_details(json!({
                    "nested": [MAX_SAFE_JSON_INTEGER + 1, u64::MAX],
                })));
            }
            Ok(ResponseResult::Pong(Pong {
                server_version: "test".to_owned(),
                protocol_version: PROTOCOL_VERSION,
            }))
        }
    }

    #[async_trait]
    impl RequestDispatcher for OversizedThenPingDispatcher {
        async fn dispatch(&self, request: Request) -> Result<ResponseResult, ErrorBody> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            assert!(matches!(request, Request::Ping));
            Ok(ResponseResult::Pong(Pong {
                server_version: if call == 0 {
                    "x".repeat(MAX_FRAME_BYTES)
                } else {
                    "test".to_owned()
                },
                protocol_version: PROTOCOL_VERSION,
            }))
        }
    }

    #[tokio::test]
    async fn native_framing_and_successful_decode_have_deterministic_linear_work() {
        const SMALL: usize = 1_024;
        const LARGE: usize = SMALL * 8;

        async fn read_work(payload_bytes: usize) -> usize {
            let payload = vec![b'x'; payload_bytes];
            let mut reader = CountingReader::framed(&payload);
            let FrameRead::Payload(actual) = read_frame(&mut reader).await.unwrap() else {
                panic!("admitted frame must return its payload")
            };
            assert_eq!(actual, payload);
            assert_eq!(reader.bytes_read, payload_bytes + 4);
            reader.bytes_read
        }

        async fn write_work(value_bytes: usize) -> usize {
            let response = ResponseEnvelope::success(
                RequestId::from_u128(0x5100),
                ResponseResult::Pong(Pong {
                    server_version: "x".repeat(value_bytes),
                    protocol_version: PROTOCOL_VERSION,
                }),
            );
            let payload_bytes = serde_json::to_vec(&response).unwrap().len();
            let mut writer = CountingWriter::default();
            write_response(&mut writer, response).await.unwrap();
            assert_eq!(writer.bytes_written, payload_bytes + 4);
            writer.bytes_written
        }

        fn decode_work(prompt_bytes: usize) -> DecodeWork {
            let request = RequestEnvelope::new(
                RequestId::from_u128(0x5200),
                Request::RunTurn(RunTurnRequest {
                    session_id: RequestId::from_u128(0x5201),
                    generation_id: SessionGenerationId::from_u128(0x5202),
                    turn: TurnRequest {
                        turn_id: RequestId::from_u128(0x5203),
                        prompt: "x".repeat(prompt_bytes),
                        deadline_unix_ms: None,
                        lease: TurnLeasePolicy::default(),
                    },
                }),
            );
            let payload = serde_json::to_vec(&request).unwrap();
            let (decoded, work) = decode_request_with_work(&payload);
            assert_eq!(decoded.unwrap(), request);
            work
        }

        let small_read = read_work(SMALL).await;
        let large_read = read_work(LARGE).await;
        assert_eq!(large_read - 4, (small_read - 4) * 8);

        let small_write = write_work(SMALL).await;
        let large_write = write_work(LARGE).await;
        assert_eq!(large_write - small_write, LARGE - SMALL);

        let expected_decode_work = DecodeWork {
            typed_json_passes: 1,
            recovery_json_passes: 0,
        };
        assert_eq!(decode_work(SMALL), expected_decode_work);
        assert_eq!(decode_work(LARGE), expected_decode_work);
    }

    #[tokio::test]
    async fn production_reader_accepts_every_header_and_payload_fragment_width() {
        let payload = (0_u8..=255).cycle().take(32 * 1024).collect::<Vec<_>>();
        for max_chunk in [1, 2, 3, 4, 7, 31, 4_096, 8_192] {
            let mut reader = CountingReader::framed_with_chunk(&payload, max_chunk);
            let FrameRead::Payload(actual) = read_frame(&mut reader).await.unwrap() else {
                panic!("admitted fragmented frame must return its payload")
            };
            assert_eq!(actual, payload, "fragment width {max_chunk}");
            assert_eq!(reader.bytes_read, payload.len() + 4);
        }
    }

    #[tokio::test]
    async fn production_reader_distinguishes_clean_eof_from_every_truncated_boundary() {
        let mut empty = CountingReader::from_bytes_with_chunk(Vec::new(), 1);
        assert!(matches!(
            read_frame(&mut empty).await.unwrap(),
            FrameRead::Eof
        ));

        let payload = b"complete payload";
        let mut frame = Vec::with_capacity(payload.len() + 4);
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend_from_slice(payload);
        for cut in 1..frame.len() {
            let mut reader = CountingReader::from_bytes_with_chunk(frame[..cut].to_vec(), 2);
            let Err(error) = read_frame(&mut reader).await else {
                panic!("cut {cut} must be reported as a truncated frame")
            };
            assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof, "cut {cut}");
            assert_eq!(reader.bytes_read, cut);
        }

        let mut zero_length = CountingReader::from_bytes_with_chunk(vec![0, 0, 0, 0], 1);
        let FrameRead::Payload(payload) = read_frame(&mut zero_length).await.unwrap() else {
            panic!("a complete zero-length frame must not be classified as EOF")
        };
        assert!(payload.is_empty());
        assert!(matches!(
            read_frame(&mut zero_length).await.unwrap(),
            FrameRead::Eof
        ));
    }

    #[test]
    fn invalid_decode_recovery_is_bounded_to_one_additional_json_pass() {
        const EXPECTED_WORK: DecodeWork = DecodeWork {
            typed_json_passes: 1,
            recovery_json_passes: 1,
        };
        let request_id = RequestId::from_u128(0x5300);

        for padding_bytes in [1_024, 8_192] {
            let payload = serde_json::to_vec(&json!({
                "version": PROTOCOL_VERSION,
                "request_id": request_id,
                "method": "ping",
                "unknown": "x".repeat(padding_bytes),
            }))
            .unwrap();
            let (result, work) = decode_request_with_work(&payload);
            let (actual_request_id, error) = result.unwrap_err();
            assert_eq!(actual_request_id, request_id);
            assert_eq!(error.code, ErrorCode::InvalidConfig);
            assert_eq!(work, EXPECTED_WORK);
        }

        let (result, work) = decode_request_with_work(br#"{"version":1,"request_id":"#);
        let (actual_request_id, error) = result.unwrap_err();
        assert_eq!(actual_request_id, UNKNOWN_REQUEST_ID);
        assert_eq!(error.code, ErrorCode::InvalidConfig);
        assert_eq!(work, EXPECTED_WORK);
    }

    fn native_mutation_config() -> ProptestConfig {
        ProptestConfig {
            max_shrink_iters: 10_000,
            failure_persistence: None,
            rng_algorithm: RngAlgorithm::ChaCha,
            rng_seed: RngSeed::Fixed(0x504d_5558_4652_414d),
            ..ProptestConfig::default()
        }
    }

    proptest! {
        #![proptest_config(native_mutation_config())]

        #[test]
        fn arbitrary_admitted_payloads_have_bounded_decode_recovery_and_responses(
            payload in prop::collection::vec(any::<u8>(), 0..=64 * 1024),
        ) {
            let (decoded, work) = decode_request_with_work(&payload);
            prop_assert_eq!(work.typed_json_passes, 1);
            match decoded {
                Ok(request) => {
                    prop_assert_eq!(work.recovery_json_passes, 0);
                    let encoded = serde_json::to_vec(&request).unwrap();
                    prop_assert!(encoded.len() <= MAX_FRAME_BYTES);
                    prop_assert_eq!(decode_request(&encoded).unwrap(), request);
                }
                Err((request_id, error)) => {
                    prop_assert!(work.recovery_json_passes <= 1);
                    if work.recovery_json_passes == 0 {
                        prop_assert_eq!(error.code, ErrorCode::ProtocolVersionMismatch);
                    }
                    let encoded = encode_response_payload(ResponseEnvelope::failure(
                        request_id,
                        error,
                    ))
                    .unwrap();
                    prop_assert!(encoded.len() <= MAX_FRAME_BYTES);
                    let response: ResponseEnvelope = serde_json::from_slice(&encoded).unwrap();
                    prop_assert_eq!(response.request_id, request_id);
                    prop_assert!(matches!(response.payload, ResponsePayload::Failure(_)));
                }
            }

            if let Ok(response) = serde_json::from_slice::<ResponseEnvelope>(&payload) {
                let request_id = response.request_id;
                let encoded = encode_response_payload(response).unwrap();
                prop_assert!(encoded.len() <= MAX_FRAME_BYTES);
                let decoded: ResponseEnvelope = serde_json::from_slice(&encoded).unwrap();
                prop_assert_eq!(decoded.request_id, request_id);
            }
        }
    }

    #[tokio::test]
    async fn exact_big_endian_frames_support_multiple_requests() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let dispatcher = Arc::new(FakeDispatcher::default());
        let task = tokio::spawn(handle_client(server, Arc::clone(&dispatcher)));
        let first_id = RequestId::from_u128(1);
        let second_id = RequestId::from_u128(2);

        write_test_request(&mut client, &RequestEnvelope::new(first_id, Request::Ping)).await;
        write_test_request(&mut client, &RequestEnvelope::new(second_id, Request::Ping)).await;
        let first = read_test_response(&mut client).await;
        let second = read_test_response(&mut client).await;
        assert_eq!(first.request_id, first_id);
        assert_eq!(second.request_id, second_id);
        assert!(matches!(
            first.payload,
            ResponsePayload::Success(result) if matches!(*result, ResponseResult::Pong(_))
        ));
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 2);
        drop(client);
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn unsupported_version_preserves_valid_request_id_without_dispatch() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let dispatcher = Arc::new(FakeDispatcher::default());
        let task = tokio::spawn(handle_client(server, Arc::clone(&dispatcher)));
        let request_id = RequestId::from_u128(42);
        let payload = serde_json::to_vec(&json!({
            "version": 99,
            "request_id": request_id,
            "method": "ping"
        }))
        .unwrap();
        write_test_frame(&mut client, &payload).await;

        let response = read_test_response(&mut client).await;
        assert_eq!(response.request_id, request_id);
        assert!(matches!(
            response.payload,
            ResponsePayload::Failure(ErrorBody {
                code: ErrorCode::ProtocolVersionMismatch,
                ..
            })
        ));
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 0);
        drop(client);
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn unsafe_unsupported_versions_are_bounded_and_connection_recovers() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let dispatcher = Arc::new(FakeDispatcher::default());
        let task = tokio::spawn(handle_client(server, Arc::clone(&dispatcher)));

        for (index, version) in [MAX_SAFE_JSON_INTEGER, MAX_SAFE_JSON_INTEGER + 1, u64::MAX]
            .into_iter()
            .enumerate()
        {
            let request_id = RequestId::from_u128(0x5400 + index as u128);
            let payload = serde_json::to_vec(&json!({
                "version": version,
                "request_id": request_id,
                "method": "ping",
            }))
            .unwrap();
            write_test_frame(&mut client, &payload).await;
            let response = read_test_response(&mut client).await;
            assert_eq!(response.request_id, request_id);
            let ResponsePayload::Failure(error) = response.payload else {
                panic!("an unsupported version must return a typed failure")
            };
            assert_eq!(error.code, ErrorCode::ProtocolVersionMismatch);
            assert_eq!(
                error.details["actual"],
                if version <= MAX_SAFE_JSON_INTEGER {
                    json!(version)
                } else {
                    json!(version.to_string())
                }
            );
        }
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 0);

        let ping_id = RequestId::from_u128(0x5410);
        write_test_request(&mut client, &RequestEnvelope::new(ping_id, Request::Ping)).await;
        let response = read_test_response(&mut client).await;
        assert_eq!(response.request_id, ping_id);
        assert!(matches!(response.payload, ResponsePayload::Success(_)));
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 1);

        drop(client);
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn unserializable_dispatch_response_is_compacted_and_connection_recovers() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let dispatcher = Arc::new(UnsafeThenPingDispatcher::default());
        let task = tokio::spawn(handle_client(server, Arc::clone(&dispatcher)));

        let unsafe_id = RequestId::from_u128(0x5420);
        write_test_request(&mut client, &RequestEnvelope::new(unsafe_id, Request::Ping)).await;
        let response = read_test_response(&mut client).await;
        assert_eq!(response.request_id, unsafe_id);
        let ResponsePayload::Failure(error) = response.payload else {
            panic!("an unencodable response must become a compact typed failure")
        };
        assert_eq!(error.code, ErrorCode::Internal);
        assert_eq!(
            error.message,
            "response could not be serialized within protocol v1"
        );
        assert!(error.details.is_null());

        let ping_id = RequestId::from_u128(0x5421);
        write_test_request(&mut client, &RequestEnvelope::new(ping_id, Request::Ping)).await;
        let response = read_test_response(&mut client).await;
        assert_eq!(response.request_id, ping_id);
        assert!(matches!(response.payload, ResponsePayload::Success(_)));
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 2);

        drop(client);
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn oversized_dispatch_response_is_compacted_and_connection_recovers() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let dispatcher = Arc::new(OversizedThenPingDispatcher::default());
        let task = tokio::spawn(handle_client(server, Arc::clone(&dispatcher)));

        let oversized_id = RequestId::from_u128(0x5430);
        write_test_request(
            &mut client,
            &RequestEnvelope::new(oversized_id, Request::Ping),
        )
        .await;
        let response = read_test_response(&mut client).await;
        assert_eq!(response.request_id, oversized_id);
        let ResponsePayload::Failure(error) = response.payload else {
            panic!("an oversized response must become a compact typed failure")
        };
        assert_eq!(error.code, ErrorCode::ResultTooLarge);
        assert_eq!(
            error.details["maximum_bytes"],
            diagnostic_usize(MAX_FRAME_BYTES)
        );

        let ping_id = RequestId::from_u128(0x5431);
        write_test_request(&mut client, &RequestEnvelope::new(ping_id, Request::Ping)).await;
        let response = read_test_response(&mut client).await;
        assert_eq!(response.request_id, ping_id);
        assert!(matches!(response.payload, ResponsePayload::Success(_)));
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 2);

        drop(client);
        task.await.unwrap().unwrap();
    }

    #[test]
    fn response_size_diagnostics_never_embed_unsafe_json_integers() {
        assert_eq!(
            diagnostic_u64(MAX_SAFE_JSON_INTEGER),
            json!(MAX_SAFE_JSON_INTEGER)
        );
        assert_eq!(
            diagnostic_u64(MAX_SAFE_JSON_INTEGER + 1),
            json!((MAX_SAFE_JSON_INTEGER + 1).to_string())
        );
        if let Ok(value) = usize::try_from(MAX_SAFE_JSON_INTEGER + 1) {
            assert_eq!(diagnostic_usize(value), json!(value.to_string()));
        }
    }

    #[tokio::test]
    async fn typed_decode_error_preserves_request_id_and_connection_recovers() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let dispatcher = Arc::new(FakeDispatcher::default());
        let task = tokio::spawn(handle_client(server, Arc::clone(&dispatcher)));
        let request_id = RequestId::from_u128(7);
        write_test_frame(
            &mut client,
            &serde_json::to_vec(&json!({
                "version": PROTOCOL_VERSION,
                "request_id": request_id,
                "method": "not_a_method"
            }))
            .unwrap(),
        )
        .await;
        write_test_request(
            &mut client,
            &RequestEnvelope::new(RequestId::from_u128(8), Request::Ping),
        )
        .await;

        let invalid = read_test_response(&mut client).await;
        let valid = read_test_response(&mut client).await;
        assert_eq!(invalid.request_id, request_id);
        assert!(matches!(
            invalid.payload,
            ResponsePayload::Failure(ErrorBody {
                code: ErrorCode::InvalidConfig,
                ..
            })
        ));
        assert!(matches!(valid.payload, ResponsePayload::Success(_)));
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 1);
        drop(client);
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn invalid_utf8_and_empty_json_frames_fail_without_losing_resynchronization() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let dispatcher = Arc::new(FakeDispatcher::default());
        let task = tokio::spawn(handle_client(server, Arc::clone(&dispatcher)));

        for payload in [b"\xff".as_slice(), b"".as_slice()] {
            write_test_frame(&mut client, payload).await;
            let response = read_test_response(&mut client).await;
            assert_eq!(response.request_id, UNKNOWN_REQUEST_ID);
            assert!(matches!(
                response.payload,
                ResponsePayload::Failure(ErrorBody {
                    code: ErrorCode::InvalidConfig,
                    ..
                })
            ));
        }

        let recovered_id = RequestId::from_u128(0x7008);
        write_test_request(
            &mut client,
            &RequestEnvelope::new(recovered_id, Request::Ping),
        )
        .await;
        let recovered = read_test_response(&mut client).await;
        assert_eq!(recovered.request_id, recovered_id);
        assert!(matches!(recovered.payload, ResponsePayload::Success(_)));
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 1);
        drop(client);
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn duplicate_envelope_fields_are_rejected_without_dispatch() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let dispatcher = Arc::new(FakeDispatcher::default());
        let task = tokio::spawn(handle_client(server, Arc::clone(&dispatcher)));
        let request_id = RequestId::from_u128(9);
        let payload = format!(
            r#"{{"version":1,"request_id":"{request_id}","method":"ping","method":"ping"}}"#
        );
        write_test_frame(&mut client, payload.as_bytes()).await;

        let response = read_test_response(&mut client).await;
        assert_eq!(response.request_id, request_id);
        assert!(matches!(
            response.payload,
            ResponsePayload::Failure(ErrorBody {
                code: ErrorCode::InvalidConfig,
                ..
            })
        ));
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 0);
        drop(client);
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn oversized_header_gets_one_error_then_connection_closes() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let dispatcher = Arc::new(FakeDispatcher::default());
        let task = tokio::spawn(handle_client(server, Arc::clone(&dispatcher)));
        client
            .write_all(&((MAX_FRAME_BYTES as u32) + 1).to_be_bytes())
            .await
            .unwrap();

        let response = read_test_response(&mut client).await;
        assert_eq!(response.request_id, UNKNOWN_REQUEST_ID);
        assert!(matches!(
            response.payload,
            ResponsePayload::Failure(ErrorBody {
                code: ErrorCode::InvalidConfig,
                ..
            })
        ));
        let mut byte = [0];
        assert_eq!(client.read(&mut byte).await.unwrap(), 0);
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn oversized_response_is_replaced_by_typed_compact_failure() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let request_id = RequestId::from_u128(77);
        let response = ResponseEnvelope::success(
            request_id,
            ResponseResult::Pong(Pong {
                server_version: "x".repeat(MAX_FRAME_BYTES),
                protocol_version: PROTOCOL_VERSION,
            }),
        );
        let writer = tokio::spawn(async move { write_response(&mut server, response).await });

        let response = read_test_response(&mut client).await;
        assert_eq!(response.request_id, request_id);
        let ResponsePayload::Failure(error) = response.payload else {
            panic!("oversized response must become a typed failure")
        };
        assert_eq!(error.code, ErrorCode::ResultTooLarge);
        assert_eq!(error.details["maximum_bytes"], MAX_FRAME_BYTES);
        assert!(error.details["actual_bytes"].as_u64().unwrap() > MAX_FRAME_BYTES as u64);
        writer.await.unwrap().unwrap();
    }

    #[test]
    fn response_at_the_exact_transport_limit_is_not_replaced() {
        let request_id = RequestId::from_u128(78);
        let empty = ResponseEnvelope::success(
            request_id,
            ResponseResult::Pong(Pong {
                server_version: String::new(),
                protocol_version: PROTOCOL_VERSION,
            }),
        );
        let fixed_bytes = serde_json::to_vec(&empty).unwrap().len();
        let response = ResponseEnvelope::success(
            request_id,
            ResponseResult::Pong(Pong {
                server_version: "x".repeat(MAX_FRAME_BYTES - fixed_bytes),
                protocol_version: PROTOCOL_VERSION,
            }),
        );

        let payload = encode_response_payload(response).unwrap();
        assert_eq!(payload.len(), MAX_FRAME_BYTES);
        let decoded: ResponseEnvelope = serde_json::from_slice(&payload).unwrap();
        assert_eq!(decoded.request_id, request_id);
        assert!(matches!(decoded.payload, ResponsePayload::Success(_)));
    }

    struct ConcurrencyDispatcher {
        current: AtomicUsize,
        maximum: AtomicUsize,
        completed: AtomicUsize,
        notify: Notify,
    }

    #[async_trait]
    impl RequestDispatcher for ConcurrencyDispatcher {
        async fn dispatch(&self, _request: Request) -> Result<ResponseResult, ErrorBody> {
            let current = self.current.fetch_add(1, Ordering::SeqCst) + 1;
            self.maximum.fetch_max(current, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(5)).await;
            self.current.fetch_sub(1, Ordering::SeqCst);
            if self.completed.fetch_add(1, Ordering::SeqCst) + 1 == 6 {
                self.notify.notify_one();
            }
            Ok(ResponseResult::Pong(Pong {
                server_version: "test".to_owned(),
                protocol_version: PROTOCOL_VERSION,
            }))
        }
    }

    #[tokio::test]
    async fn server_bounds_connections_and_drains_on_shutdown() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("pmuxd.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let dispatcher = Arc::new(ConcurrencyDispatcher {
            current: AtomicUsize::new(0),
            maximum: AtomicUsize::new(0),
            completed: AtomicUsize::new(0),
            notify: Notify::new(),
        });
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let server_dispatcher = Arc::clone(&dispatcher);
        let server = tokio::spawn(async move {
            serve_until(
                listener,
                server_dispatcher,
                ServerLimits {
                    max_connections: 2,
                    shutdown_grace: Duration::from_secs(1),
                    ..ServerLimits::default()
                },
                async {
                    let _ = shutdown_rx.await;
                },
            )
            .await
        });

        let mut clients = Vec::new();
        for number in 0..6 {
            let path = path.clone();
            clients.push(tokio::spawn(async move {
                let mut stream = UnixStream::connect(path).await.unwrap();
                write_test_request(
                    &mut stream,
                    &RequestEnvelope::new(RequestId::from_u128(number + 1), Request::Ping),
                )
                .await;
                read_test_response(&mut stream).await
            }));
        }
        dispatcher.notify.notified().await;
        let _ = shutdown_tx.send(());
        for client in clients {
            assert!(matches!(
                client.await.unwrap().payload,
                ResponsePayload::Success(_)
            ));
        }
        server.await.unwrap().unwrap();
        assert!(dispatcher.maximum.load(Ordering::SeqCst) <= 2);
    }

    #[tokio::test]
    async fn partial_request_frames_are_dropped_at_the_read_deadline() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let dispatcher = Arc::new(FakeDispatcher::default());
        let task = tokio::spawn(handle_client_with_limits(
            server,
            Arc::clone(&dispatcher),
            ServerLimits {
                frame_read_timeout: Duration::from_millis(10),
                ..ServerLimits::default()
            },
        ));
        client.write_all(&[0]).await.unwrap();

        let error = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("slow frame task must terminate")
            .unwrap()
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 0);
        let mut byte = [0];
        assert_eq!(client.read(&mut byte).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn unread_response_frames_are_dropped_at_the_write_deadline() {
        let (_client, mut server) = UnixStream::pair().unwrap();
        let response = ResponseEnvelope::success(
            RequestId::from_u128(91),
            ResponseResult::Pong(Pong {
                server_version: "x".repeat(MAX_FRAME_BYTES / 2),
                protocol_version: PROTOCOL_VERSION,
            }),
        );
        let error = write_response_with_timeout(&mut server, response, Duration::from_millis(10))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    async fn write_test_request(stream: &mut UnixStream, request: &RequestEnvelope) {
        write_test_frame(stream, &serde_json::to_vec(request).unwrap()).await;
    }

    async fn write_test_frame(stream: &mut UnixStream, payload: &[u8]) {
        stream
            .write_all(&(payload.len() as u32).to_be_bytes())
            .await
            .unwrap();
        stream.write_all(payload).await.unwrap();
    }

    async fn read_test_response(stream: &mut UnixStream) -> ResponseEnvelope {
        let mut header = [0; 4];
        stream.read_exact(&mut header).await.unwrap();
        let length = u32::from_be_bytes(header) as usize;
        let mut payload = vec![0; length];
        stream.read_exact(&mut payload).await.unwrap();
        serde_json::from_slice(&payload).unwrap()
    }
}
