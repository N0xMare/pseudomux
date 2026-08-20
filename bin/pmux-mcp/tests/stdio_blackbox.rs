#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use pseudomux_protocol::v1::{MAX_SAFE_JSON_INTEGER, Request, RequestEnvelope};
use serde_json::{Value, json};

#[path = "../../../tests/support/candidate_binary.rs"]
mod candidate_binary;
use candidate_binary::CandidateBinaries;

const MAX_MCP_FRAME_BYTES: usize = 8 * 1024 * 1024;
const SAFE_JSON_INTEGER_EXCLUSIVE_LIMIT: f64 = 9_007_199_254_740_992.0;
const PROCESS_TIMEOUT: Duration = Duration::from_secs(10);

const UNPUBLISHED_TOOLS: &[&str] = &[
    "start_session",
    "run_turn",
    "inspect_session",
    "cancel_turn",
    "close_session",
    "run_once",
    "subscribe_events",
    "attach_session",
    "create_agent",
    "get_agent",
    "list_agents",
    "update_agent",
];

static NEXT_SANDBOX: AtomicU64 = AtomicU64::new(1);

struct Sandbox {
    root: PathBuf,
    socket: PathBuf,
}

impl Sandbox {
    fn new(label: &str) -> Self {
        let serial = NEXT_SANDBOX.fetch_add(1, Ordering::Relaxed);
        let root =
            PathBuf::from("/tmp").join(format!("pmcp-{}-{serial}-{}", std::process::id(), label));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let socket = root.join("s");
        Self { root, socket }
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn candidates() -> &'static CandidateBinaries {
    static CANDIDATES: OnceLock<CandidateBinaries> = OnceLock::new();
    CANDIDATES.get_or_init(|| {
        CandidateBinaries::discover(
            std::env::var_os("PMUX_TEST_BIN_DIR").map(PathBuf::from),
            [(
                "pmux-mcp".to_owned(),
                PathBuf::from(env!("CARGO_BIN_EXE_pmux-mcp")),
            )],
        )
        .unwrap_or_else(|error| panic!("failed to bind pmux-mcp candidate: {error}"))
    })
}

fn pmux_mcp_binary() -> PathBuf {
    candidates().path("pmux-mcp").to_path_buf()
}

fn assert_candidate_unchanged() {
    candidates()
        .assert_unchanged()
        .unwrap_or_else(|error| panic!("pmux-mcp candidate changed during test: {error}"));
}

enum NativeReply {
    Success {
        kind: &'static str,
        data: Value,
    },
    Error {
        code: &'static str,
        message: &'static str,
        retryable: bool,
        details: Value,
    },
    Malformed(Vec<u8>),
}

struct NativeExchange {
    expected: Request,
    reply: NativeReply,
}

fn spawn_native_server(
    listener: UnixListener,
    exchanges: Vec<NativeExchange>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        for exchange in exchanges {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_native_request(&mut stream);
            assert_eq!(request.version, 1);
            assert_eq!(request.request, exchange.expected);
            match exchange.reply {
                NativeReply::Success { kind, data } => write_native_value(
                    &mut stream,
                    &json!({
                        "version": 1,
                        "request_id": request.request_id,
                        "result": {"type": kind, "data": data},
                    }),
                ),
                NativeReply::Error {
                    code,
                    message,
                    retryable,
                    details,
                } => write_native_value(
                    &mut stream,
                    &json!({
                        "version": 1,
                        "request_id": request.request_id,
                        "error": {
                            "code": code,
                            "message": message,
                            "retryable": retryable,
                            "details": details,
                        },
                    }),
                ),
                NativeReply::Malformed(payload) => write_native_payload(&mut stream, &payload),
            }
        }
    })
}

fn read_native_request(stream: &mut UnixStream) -> RequestEnvelope {
    let mut header = [0_u8; 4];
    stream.read_exact(&mut header).unwrap();
    let mut payload = vec![0; u32::from_be_bytes(header) as usize];
    stream.read_exact(&mut payload).unwrap();
    serde_json::from_slice(&payload).unwrap()
}

fn write_native_value(stream: &mut UnixStream, value: &Value) {
    write_native_payload(stream, &serde_json::to_vec(value).unwrap());
}

fn write_native_payload(stream: &mut UnixStream, payload: &[u8]) {
    stream
        .write_all(&u32::try_from(payload.len()).unwrap().to_be_bytes())
        .unwrap();
    stream.write_all(payload).unwrap();
    stream.flush().unwrap();
}

enum StdoutItem {
    Line(String),
    Error(String),
    Eof,
}

struct McpProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Receiver<StdoutItem>,
    stdout_thread: thread::JoinHandle<()>,
}

impl McpProcess {
    fn start(socket: &Path) -> Self {
        let mut child = Command::new(pmux_mcp_binary())
            .arg("--socket")
            .arg(socket)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, receiver) = mpsc::channel();
        let stdout_thread = thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => {
                        let _ = sender.send(StdoutItem::Eof);
                        return;
                    }
                    Ok(_) => {
                        let _ = sender.send(StdoutItem::Line(line));
                    }
                    Err(error) => {
                        let _ = sender.send(StdoutItem::Error(error.to_string()));
                        return;
                    }
                }
            }
        });
        Self {
            child,
            stdin: Some(stdin),
            stdout: receiver,
            stdout_thread,
        }
    }

    fn send(&mut self, value: &Value) {
        let mut encoded = serde_json::to_vec(value).unwrap();
        encoded.push(b'\n');
        self.send_bytes(&encoded);
    }

    fn send_bytes(&mut self, bytes: &[u8]) {
        let stdin = self.stdin.as_mut().expect("MCP stdin is still open");
        stdin.write_all(bytes).unwrap();
        stdin.flush().unwrap();
    }

    fn close_stdin(&mut self) {
        drop(self.stdin.take());
    }

    fn response(&self) -> Value {
        self.response_with_wire_len().0
    }

    fn response_with_wire_len(&self) -> (Value, usize) {
        match self.stdout.recv_timeout(PROCESS_TIMEOUT).unwrap() {
            StdoutItem::Line(line) => {
                assert!(
                    line.ends_with('\n'),
                    "MCP response was not newline terminated"
                );
                let wire_len = line.len();
                (serde_json::from_str(&line).unwrap(), wire_len)
            }
            StdoutItem::Error(error) => panic!("failed to read MCP stdout: {error}"),
            StdoutItem::Eof => panic!("MCP stdout closed before a response"),
        }
    }

    fn finish(mut self) -> (ExitStatus, String) {
        self.close_stdin();
        let deadline = Instant::now() + PROCESS_TIMEOUT;
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                panic!("pmux-mcp did not exit after stdin EOF");
            }
            thread::sleep(Duration::from_millis(10));
        };

        match self.stdout.recv_timeout(PROCESS_TIMEOUT).unwrap() {
            StdoutItem::Eof => {}
            StdoutItem::Line(line) => panic!("unexpected extra MCP stdout: {line:?}"),
            StdoutItem::Error(error) => panic!("failed to finish MCP stdout: {error}"),
        }
        self.stdout_thread.join().unwrap();
        let mut stderr = String::new();
        self.child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        assert_candidate_unchanged();
        (status, stderr)
    }
}

fn expected_request(method: &str, arguments: &Value) -> Request {
    serde_json::from_value(json!({"method": method, "params": arguments})).unwrap()
}

/// The one provider success shape. Deliberately carries no `session_id`: not even
/// through the envelope does a pool instance get a name.
fn stateless_result() -> Value {
    json!({
        "model": "claude-opus-5",
        "reported_model": "claude-opus-5",
        "effort": "xhigh",
        "text": "four",
        "usage": {
            "main": {
                "input_tokens": 186,
                "output_tokens": 3,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0,
            },
            "sidechain": {
                "input_tokens": 0,
                "output_tokens": 0,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0,
            },
            "combined": {
                "input_tokens": 186,
                "output_tokens": 3,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0,
            },
        },
        "claude_version": "2.1.220",
    })
}

fn initialize(id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "black-box", "version": "1"},
        },
    })
}

fn tool_call(id: u64, name: &str, arguments: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {"name": name, "arguments": arguments},
    })
}

fn run_stateless_arguments() -> Value {
    // Model, effort and prompt. There is nothing else to send: the
    // request DTO is `deny_unknown_fields` and carries no cwd, no
    // configuration root, no system prompt and no session id.
    json!({"model": "claude-opus-5", "effort": "xhigh", "prompt": "what is two plus two"})
}

fn assert_unknown_tool(response: &Value, id: u64, name: &str) {
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], id);
    assert_eq!(response["error"]["code"], -32602);
    assert_eq!(response["error"]["message"], "Unknown tool");
    assert!(
        !response.to_string().contains(name),
        "unknown_tool must not echo the unpublished name: {response}"
    );
}

#[test]
fn real_stdio_enforces_rpc_id_domain_and_recovers_on_one_stream() {
    let sandbox = Sandbox::new("rpc-ids");
    let mut mcp = McpProcess::start(&sandbox.socket);

    for id in [
        Value::Null,
        json!(""),
        json!("9007199254740992"),
        json!("id-\u{1f642}"),
        json!(-(MAX_SAFE_JSON_INTEGER as i64)),
        json!(MAX_SAFE_JSON_INTEGER),
        json!(1.0),
    ] {
        mcp.send(&json!({"jsonrpc": "2.0", "id": id, "method": "ping"}));
        let response = mcp.response();
        assert_eq!(response["id"], id);
        assert_eq!(response["result"], json!({}));
    }

    for id in [
        json!(-(MAX_SAFE_JSON_INTEGER as i64) - 1),
        json!(MAX_SAFE_JSON_INTEGER + 1),
        json!(u64::MAX),
        json!(SAFE_JSON_INTEGER_EXCLUSIVE_LIMIT),
        json!(-SAFE_JSON_INTEGER_EXCLUSIVE_LIMIT),
        json!(1.5),
    ] {
        mcp.send(&json!({"jsonrpc": "2.0", "id": id, "method": "ping"}));
        let response = mcp.response();
        assert!(response["id"].is_null());
        assert_eq!(response["error"]["code"], -32600);
    }

    mcp.send(&json!({
        "jsonrpc": "2.0",
        "id": "after-invalid-ids",
        "method": "ping",
    }));
    let recovered = mcp.response();
    assert_eq!(recovered["id"], "after-invalid-ids");
    assert_eq!(recovered["result"], json!({}));

    let (status, stderr) = mcp.finish();
    assert!(status.success());
    assert!(stderr.is_empty(), "unexpected MCP diagnostics: {stderr}");
}

/// `tools/list` is the provider surface. Unpublished names on `tools/call`
/// are `unknown_tool` and MUST NOT reach the native socket: mapping
/// `start_session` against a listener that never `accept()`s hangs the process.
///
/// The native server is bound only for the one advertised tool, after the
/// unpublished names have already been refused.
#[test]
fn real_stdio_maps_every_advertised_tool_to_an_exact_native_request() {
    let sandbox = Sandbox::new("tools");
    let mut mcp = McpProcess::start(&sandbox.socket);
    mcp.send(&initialize(1));
    let initialized = mcp.response();
    assert_eq!(initialized["id"], 1);
    assert_eq!(initialized["result"]["protocolVersion"], "2025-06-18");

    mcp.send(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {},
    }));
    let listed = mcp.response();
    let names = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec!["run_stateless"],
        "tools/list must expose only run_stateless"
    );

    // No listener is bound. A regression that maps any of these would try
    // UnixStream::connect against a missing path and become
    // transport_unavailable, not unknown_tool; a bound listener that
    // accept()s would hang instead.
    let start = json!({
        "identity": {"mode": "new"},
        "cwd": "/work/project",
        "claude": {"executable": "/opt/claude"},
    });
    for (index, name) in UNPUBLISHED_TOOLS.iter().enumerate() {
        let id = 10 + index as u64;
        let arguments = if *name == "start_session" {
            start.clone()
        } else {
            json!({})
        };
        mcp.send(&tool_call(id, name, &arguments));
        assert_unknown_tool(&mcp.response(), id, name);
    }

    let arguments = run_stateless_arguments();
    let listener = UnixListener::bind(&sandbox.socket).unwrap();
    let server = spawn_native_server(
        listener,
        vec![NativeExchange {
            expected: expected_request("run_stateless", &arguments),
            reply: NativeReply::Success {
                kind: "stateless_result",
                data: stateless_result(),
            },
        }],
    );

    mcp.send(&tool_call(99, "run_stateless", &arguments));
    let response = mcp.response();
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 99);
    assert_eq!(response["result"]["content"], json!([]));
    assert_eq!(
        &response["result"]["structuredContent"],
        &stateless_result()
    );
    assert_eq!(response["result"]["isError"], false);

    let (status, stderr) = mcp.finish();
    server.join().unwrap();
    assert!(status.success());
    assert!(stderr.is_empty(), "unexpected MCP diagnostics: {stderr}");
}

#[test]
fn real_stdio_redacts_native_errors_and_malformed_or_unavailable_peers() {
    let sandbox = Sandbox::new("errors");
    let listener = UnixListener::bind(&sandbox.socket).unwrap();
    let arguments = run_stateless_arguments();
    let expected = expected_request("run_stateless", &arguments);
    let server = spawn_native_server(
        listener,
        vec![
            NativeExchange {
                expected: expected.clone(),
                reply: NativeReply::Error {
                    code: "schema_drift",
                    message: "secret prompt and path",
                    retryable: false,
                    details: json!({"secret": "must-not-escape"}),
                },
            },
            NativeExchange {
                expected: expected.clone(),
                reply: NativeReply::Error {
                    code: "unsupported_feature",
                    message: "secret prompt and path",
                    retryable: false,
                    details: json!({
                        "violation": "path_b_not_enabled",
                        "recommendation": "restart pmuxd with --pool-parent DIR",
                        "attach_token": "must-not-escape",
                    }),
                },
            },
            NativeExchange {
                expected: expected.clone(),
                reply: NativeReply::Malformed(b"{not-json".to_vec()),
            },
            NativeExchange {
                expected,
                reply: NativeReply::Success {
                    kind: "pong",
                    data: json!({"server_version": "test", "protocol_version": 1}),
                },
            },
        ],
    );

    let mut mcp = McpProcess::start(&sandbox.socket);
    mcp.send(&tool_call(1, "run_stateless", &arguments));
    let rejected = mcp.response();
    assert_eq!(rejected["result"]["isError"], true);
    assert_eq!(
        rejected["result"]["structuredContent"]["error"],
        json!({
            "kind": "daemon_rejected",
            "code": "schema_drift",
            "retryable": false,
        })
    );
    let rendered = rejected.to_string();
    assert!(!rendered.contains("secret prompt"));
    assert!(!rendered.contains("must-not-escape"));

    // The same refusal WITH advice: the one detail key a refusal writes for a
    // person to read crosses the real process, and nothing beside it does.
    mcp.send(&tool_call(2, "run_stateless", &arguments));
    let advised = mcp.response();
    assert_eq!(advised["result"]["isError"], true);
    assert_eq!(
        advised["result"]["structuredContent"]["error"],
        json!({
            "kind": "daemon_rejected",
            "code": "unsupported_feature",
            "retryable": false,
            "recommendation": "restart pmuxd with --pool-parent DIR",
        })
    );
    assert!(
        advised["result"]["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("restart pmuxd with --pool-parent DIR")),
        "the advice must reach the channel a model reads: {advised}"
    );
    let rendered = advised.to_string();
    assert!(!rendered.contains("secret prompt"));
    assert!(!rendered.contains("must-not-escape"));

    mcp.send(&tool_call(3, "run_stateless", &arguments));
    let malformed = mcp.response();
    assert_eq!(malformed["result"]["isError"], true);
    assert_eq!(
        malformed["result"]["structuredContent"]["error"]["kind"],
        "invalid_daemon_response"
    );

    mcp.send(&tool_call(4, "run_stateless", &arguments));
    let wrong_result = mcp.response();
    assert_eq!(wrong_result["result"]["isError"], true);
    assert_eq!(
        wrong_result["result"]["structuredContent"]["error"]["kind"],
        "invalid_daemon_response"
    );
    server.join().unwrap();

    // The listener is now gone while the exact endpoint remains. A later call
    // must become a structured transport failure rather than terminating MCP.
    mcp.send(&tool_call(5, "run_stateless", &arguments));
    let unavailable = mcp.response();
    assert_eq!(unavailable["result"]["isError"], true);
    assert_eq!(
        unavailable["result"]["structuredContent"]["error"]["kind"],
        "transport_unavailable"
    );

    let (status, stderr) = mcp.finish();
    assert!(status.success());
    assert!(!stderr.contains("secret prompt"));
    assert!(!stderr.contains("must-not-escape"));
}

#[test]
fn real_stdio_bounds_and_recovers_input_notifications_and_partial_eof() {
    let sandbox = Sandbox::new("frames");
    let mut mcp = McpProcess::start(&sandbox.socket);

    mcp.send(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
        "params": {},
    }));
    mcp.send(&json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}));
    let ping = mcp.response();
    assert_eq!(ping["id"], 1, "the notification emitted a response");
    assert_eq!(ping["result"], json!({}));

    let private_argument = "strict-tool-private-input";
    mcp.send(&tool_call(
        10,
        "run_stateless",
        &json!({
            "model": "claude-opus-5",
            "prompt": "hello",
            "unknown": private_argument,
        }),
    ));
    let invalid_arguments = mcp.response();
    assert_eq!(invalid_arguments["id"], 10);
    assert_eq!(invalid_arguments["result"]["isError"], true);
    assert_eq!(
        invalid_arguments["result"]["structuredContent"]["error"]["kind"],
        "invalid_arguments"
    );
    assert!(!invalid_arguments.to_string().contains(private_argument));

    mcp.send(&tool_call(
        11,
        "start_session",
        &json!({
            "identity": {"mode": "new"},
            "cwd": "/work/project",
            "claude": {"executable": "/opt/claude"},
        }),
    ));
    assert_unknown_tool(&mcp.response(), 11, "start_session");

    mcp.send(&tool_call(12, "private-unknown-tool", &json!({})));
    assert_unknown_tool(&mcp.response(), 12, "private-unknown-tool");

    mcp.send(&json!({
        "jsonrpc": "2.0",
        "id": 13,
        "method": "ping",
        "unexpected": "strict-frame-private-input",
    }));
    let invalid_request = mcp.response();
    assert_eq!(invalid_request["id"], 13);
    assert_eq!(invalid_request["error"]["code"], -32600);
    assert!(
        !invalid_request
            .to_string()
            .contains("strict-frame-private-input")
    );

    mcp.send_bytes(b"{invalid-json\n");
    let malformed = mcp.response();
    assert_eq!(malformed["id"], Value::Null);
    assert_eq!(malformed["error"]["code"], -32700);

    let mut oversized = vec![b'x'; MAX_MCP_FRAME_BYTES + 1];
    oversized.push(b'\n');
    mcp.send_bytes(&oversized);
    let too_large = mcp.response();
    assert_eq!(too_large["id"], Value::Null);
    assert_eq!(too_large["error"]["code"], -32700);
    assert_eq!(
        too_large["error"]["message"],
        "MCP frame exceeds the size limit"
    );

    // Prove that draining an oversized frame preserves the next boundary.
    mcp.send(&json!({"jsonrpc": "2.0", "id": 2, "method": "ping"}));
    assert_eq!(mcp.response()["id"], 2);

    // EOF itself terminates a final non-newline-delimited JSON value.
    mcp.send_bytes(br#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#);
    mcp.close_stdin();
    let partial = mcp.response();
    assert_eq!(partial["id"], 3);
    assert_eq!(partial["result"], json!({}));

    let (status, stderr) = mcp.finish();
    assert!(status.success());
    assert!(stderr.is_empty(), "unexpected MCP diagnostics: {stderr}");
}

#[test]
fn real_stdio_bounds_oversized_output_and_recovers_on_one_stream() {
    const PRIVATE_RESULT_MARKER: &str = "private-oversized-native-result-must-not-escape";
    const PRIVATE_METHOD_MARKER: &str = "private-oversized-method-must-not-escape";

    let sandbox = Sandbox::new("oversized-output");
    let listener = UnixListener::bind(&sandbox.socket).unwrap();
    let arguments = run_stateless_arguments();
    let mut oversized_result = stateless_result();
    oversized_result["text"] = Value::String(format!(
        "{PRIVATE_RESULT_MARKER}/{}",
        "r".repeat(MAX_MCP_FRAME_BYTES / 2)
    ));
    let server = spawn_native_server(
        listener,
        vec![NativeExchange {
            expected: expected_request("run_stateless", &arguments),
            reply: NativeReply::Success {
                kind: "stateless_result",
                data: oversized_result,
            },
        }],
    );

    let mut mcp = McpProcess::start(&sandbox.socket);

    // The MCP request and native response each fit their independent 8 MiB
    // boundaries, but combining their large ID and result would exceed the
    // outbound JSON-RPC boundary. The shipped process must replace that value
    // with one correlated, bounded, redacted error.
    let correlated_id = format!(
        "correlated-oversized-output-{}",
        "i".repeat(MAX_MCP_FRAME_BYTES / 2)
    );
    let request = json!({
        "jsonrpc": "2.0",
        "id": correlated_id,
        "method": "tools/call",
        "params": {"name": "run_stateless", "arguments": arguments},
    });
    assert!(serde_json::to_vec(&request).unwrap().len() <= MAX_MCP_FRAME_BYTES);
    mcp.send(&request);
    let (bounded, wire_len) = mcp.response_with_wire_len();
    assert!(wire_len <= MAX_MCP_FRAME_BYTES + 1);
    assert_eq!(bounded["id"], request["id"]);
    assert_eq!(bounded["error"]["code"], -32603);
    assert_eq!(
        bounded["error"]["message"],
        "MCP response exceeds the size limit"
    );
    assert!(!bounded.to_string().contains(PRIVATE_RESULT_MARKER));

    mcp.send(&json!({
        "jsonrpc": "2.0",
        "id": "after-correlated-oversized-output",
        "method": "ping",
    }));
    let recovered = mcp.response();
    assert_eq!(recovered["id"], "after-correlated-oversized-output");
    assert_eq!(recovered["result"], json!({}));

    // An exactly maximal admitted request ID can make even the correlated
    // fallback exceed the frame. Exercise the compact null-ID fallback through
    // the real stdin/stdout process boundary, without ever echoing the method.
    let request_without_id = json!({
        "jsonrpc": "2.0",
        "id": "",
        "method": PRIVATE_METHOD_MARKER,
    });
    let fixed_len = serde_json::to_vec(&request_without_id).unwrap().len();
    let compact_fallback_id = "j".repeat(MAX_MCP_FRAME_BYTES - fixed_len);
    let compact_fallback_request = json!({
        "jsonrpc": "2.0",
        "id": compact_fallback_id,
        "method": PRIVATE_METHOD_MARKER,
    });
    assert_eq!(
        serde_json::to_vec(&compact_fallback_request).unwrap().len(),
        MAX_MCP_FRAME_BYTES
    );
    mcp.send(&compact_fallback_request);
    let (compact, compact_wire_len) = mcp.response_with_wire_len();
    assert!(compact_wire_len <= MAX_MCP_FRAME_BYTES + 1);
    assert!(compact["id"].is_null());
    assert_eq!(compact["error"]["code"], -32603);
    assert_eq!(
        compact["error"]["message"],
        "MCP response exceeds the size limit"
    );
    assert!(!compact.to_string().contains(PRIVATE_METHOD_MARKER));

    mcp.send(&json!({
        "jsonrpc": "2.0",
        "id": "after-compact-oversized-output",
        "method": "ping",
    }));
    let recovered = mcp.response();
    assert_eq!(recovered["id"], "after-compact-oversized-output");
    assert_eq!(recovered["result"], json!({}));

    let (status, stderr) = mcp.finish();
    server.join().unwrap();
    assert!(status.success());
    assert!(!stderr.contains(PRIVATE_RESULT_MARKER));
    assert!(!stderr.contains(PRIVATE_METHOD_MARKER));
}
