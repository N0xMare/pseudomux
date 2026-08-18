//! Stdio MCP adapter for the native pmux protocol.

#![cfg(unix)]

mod tools;

use std::io;
use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use clap::Parser;
use pseudomux_client::PmuxClient;
use pseudomux_protocol::v1::MAX_SAFE_JSON_INTEGER;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};

const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const SUPPORTED_MCP_VERSIONS: &[&str] = &[
    "2024-11-05",
    "2025-03-26",
    "2025-06-18",
    MCP_PROTOCOL_VERSION,
];
const MAX_MCP_FRAME_BYTES: usize = 8 * 1024 * 1024;
const SAFE_JSON_INTEGER_EXCLUSIVE_LIMIT: f64 = 9_007_199_254_740_992.0;

#[derive(Debug, Parser)]
#[command(version, about = "Native protocol-v1 pmux MCP adapter")]
struct Cli {
    /// Exact pmuxd Unix socket. No discovery or daemon startup is performed.
    #[arg(long, env = "PMUX_SOCKET")]
    socket: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RpcRequest {
    jsonrpc: String,
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InitializeParams {
    #[serde(rename = "protocolVersion")]
    protocol_version: String,
    #[serde(rename = "capabilities")]
    _capabilities: Value,
    #[serde(rename = "clientInfo")]
    _client_info: Implementation,
    #[serde(rename = "_meta", default)]
    _meta: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Implementation {
    #[serde(rename = "name")]
    _name: String,
    #[serde(rename = "version")]
    _version: String,
    #[serde(rename = "title", default)]
    _title: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListToolsParams {
    #[serde(default)]
    cursor: Option<String>,
    #[serde(rename = "_meta", default)]
    _meta: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CallToolParams {
    name: String,
    #[serde(default = "empty_object")]
    arguments: Value,
    #[serde(rename = "_meta", default)]
    _meta: Option<Value>,
}

fn empty_object() -> Value {
    json!({})
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    ensure!(
        cli.socket.is_absolute(),
        "--socket/PMUX_SOCKET must be an absolute path"
    );
    let client = PmuxClient::new(cli.socket).context("invalid explicit pmuxd socket")?;
    serve(
        &client,
        BufReader::new(tokio::io::stdin()),
        tokio::io::stdout(),
    )
    .await
}

async fn serve<R, W>(client: &PmuxClient, mut reader: R, mut writer: W) -> Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    loop {
        match read_mcp_frame(&mut reader)
            .await
            .context("failed to read MCP stdin")?
        {
            FrameRead::Eof => return Ok(()),
            FrameRead::TooLarge => {
                write_response(
                    &mut writer,
                    &rpc_error(Value::Null, -32700, "MCP frame exceeds the size limit"),
                )
                .await?;
            }
            FrameRead::Frame(frame) if frame.iter().all(u8::is_ascii_whitespace) => {}
            FrameRead::Frame(frame) => {
                let response = match serde_json::from_slice::<Value>(&frame) {
                    Ok(raw) => process_value(client, raw).await,
                    Err(_) => Some(rpc_error(Value::Null, -32700, "Parse error")),
                };
                if let Some(response) = response {
                    write_response(&mut writer, &response).await?;
                }
            }
        }
    }
}

async fn process_value(client: &PmuxClient, raw: Value) -> Option<Value> {
    let has_id = raw.get("id").is_some();
    let fallback_id = raw
        .get("id")
        .filter(|id| valid_rpc_id(id))
        .cloned()
        .unwrap_or(Value::Null);
    let request: RpcRequest = match serde_json::from_value(raw) {
        Ok(request) => request,
        Err(_) => return Some(rpc_error(fallback_id, -32600, "Invalid Request")),
    };
    if request.jsonrpc != "2.0"
        || request.method.is_empty()
        || request.id.as_ref().is_some_and(|id| !valid_rpc_id(id))
    {
        return Some(rpc_error(fallback_id, -32600, "Invalid Request"));
    }

    if !has_id {
        // JSON-RPC notifications never receive a response. The initialized
        // notification needs no local state because this adapter is stateless.
        return None;
    }
    let id = request.id.unwrap_or(Value::Null);
    let params = request.params.unwrap_or_else(empty_object);
    match request.method.as_str() {
        "initialize" => {
            let params: InitializeParams = match serde_json::from_value(params) {
                Ok(params) => params,
                Err(_) => return Some(rpc_error(id, -32602, "Invalid initialize parameters")),
            };
            let negotiated = negotiate_version(&params.protocol_version);
            Some(rpc_success(
                id,
                json!({
                    "protocolVersion": negotiated,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {
                        "name": "pmux-mcp",
                        "version": env!("CARGO_PKG_VERSION")
                    },
                    "instructions": "This server is a thin mapper to one explicit pmuxd protocol-v1 socket. run_stateless is (model, effort, prompt) -> text + usage. The caller names no resource."
                }),
            ))
        }
        "ping" => Some(rpc_success(id, json!({}))),
        "tools/list" => {
            let params: ListToolsParams = match serde_json::from_value(params) {
                Ok(params) => params,
                Err(_) => return Some(rpc_error(id, -32602, "Invalid tools/list parameters")),
            };
            if params.cursor.is_some() {
                return Some(rpc_error(id, -32602, "Tool list cursor is not supported"));
            }
            Some(rpc_success(
                id,
                json!({"tools": tools::published_tool_definitions()}),
            ))
        }
        "tools/call" => {
            let params: CallToolParams = match serde_json::from_value(params) {
                Ok(params) => params,
                Err(_) => return Some(rpc_error(id, -32602, "Invalid tools/call parameters")),
            };
            match tools::handle_tool(client, &params.name, &params.arguments).await {
                Ok(result) => Some(rpc_success(id, result)),
                Err(error) if error.is_unknown_tool() => {
                    Some(rpc_error(id, -32602, "Unknown tool"))
                }
                Err(error) => Some(rpc_success(id, error.result())),
            }
        }
        _ => Some(rpc_error(id, -32601, "Method not found")),
    }
}

fn negotiate_version(requested: &str) -> &str {
    SUPPORTED_MCP_VERSIONS
        .iter()
        .copied()
        .find(|version| *version == requested)
        .unwrap_or(MCP_PROTOCOL_VERSION)
}

fn valid_rpc_id(value: &Value) -> bool {
    match value {
        Value::String(_) | Value::Null => true,
        Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                return (-(MAX_SAFE_JSON_INTEGER as i64)..=MAX_SAFE_JSON_INTEGER as i64)
                    .contains(&value);
            }
            if let Some(value) = number.as_u64() {
                return value <= MAX_SAFE_JSON_INTEGER;
            }
            number.as_f64().is_some_and(|value| {
                value.is_finite()
                    && value.fract() == 0.0
                    && value.abs() < SAFE_JSON_INTEGER_EXCLUSIVE_LIMIT
            })
        }
        Value::Bool(_) | Value::Array(_) | Value::Object(_) => false,
    }
}

fn outbound_json_numbers_are_safe(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) => true,
        Value::Array(values) => values.iter().all(outbound_json_numbers_are_safe),
        Value::Object(values) => values.values().all(outbound_json_numbers_are_safe),
        Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                return (-(MAX_SAFE_JSON_INTEGER as i64)..=MAX_SAFE_JSON_INTEGER as i64)
                    .contains(&value);
            }
            if let Some(value) = number.as_u64() {
                return value <= MAX_SAFE_JSON_INTEGER;
            }
            number.as_f64().is_some_and(|value| {
                value.is_finite()
                    && (value.fract() != 0.0 || value.abs() < SAFE_JSON_INTEGER_EXCLUSIVE_LIMIT)
            })
        }
    }
}

fn rpc_success(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn rpc_error(id: Value, code: i32, message: &'static str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message}
    })
}

async fn write_response<W: AsyncWrite + Unpin>(writer: &mut W, response: &Value) -> Result<()> {
    let id = response
        .get("id")
        .filter(|id| valid_rpc_id(id))
        .cloned()
        .unwrap_or(Value::Null);
    let mut encoded = if outbound_json_numbers_are_safe(response) {
        serde_json::to_vec(response).context("failed to encode MCP response")?
    } else {
        serde_json::to_vec(&rpc_error(
            id.clone(),
            -32603,
            "MCP response contains an unsafe integer",
        ))
        .context("failed to encode bounded MCP error response")?
    };
    if encoded.len() > MAX_MCP_FRAME_BYTES {
        encoded = serde_json::to_vec(&rpc_error(
            id.clone(),
            -32603,
            "MCP response exceeds the size limit",
        ))
        .context("failed to encode bounded MCP error response")?;
    }
    if encoded.len() > MAX_MCP_FRAME_BYTES {
        encoded = serde_json::to_vec(&rpc_error(
            Value::Null,
            -32603,
            "MCP response exceeds the size limit",
        ))
        .context("failed to encode compact MCP error response")?;
    }
    ensure!(
        encoded.len() <= MAX_MCP_FRAME_BYTES,
        "compact MCP error response exceeds the size limit"
    );
    encoded.push(b'\n');
    writer
        .write_all(&encoded)
        .await
        .context("failed to write MCP stdout")?;
    writer.flush().await.context("failed to flush MCP stdout")
}

#[derive(Debug, PartialEq, Eq)]
enum FrameRead {
    Eof,
    Frame(Vec<u8>),
    TooLarge,
}

/// Read and drain one newline-delimited MCP frame without unbounded allocation.
async fn read_mcp_frame<R: AsyncBufRead + Unpin>(reader: &mut R) -> io::Result<FrameRead> {
    let mut frame = Vec::new();
    let mut too_large = false;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if too_large {
                Ok(FrameRead::TooLarge)
            } else if frame.is_empty() {
                Ok(FrameRead::Eof)
            } else {
                Ok(FrameRead::Frame(frame))
            };
        }

        let newline = available.iter().position(|byte| *byte == b'\n');
        let content_len = newline.unwrap_or(available.len());
        if !too_large {
            if frame.len().saturating_add(content_len) > MAX_MCP_FRAME_BYTES {
                too_large = true;
                frame.clear();
            } else {
                frame.extend_from_slice(&available[..content_len]);
            }
        }
        let consumed = newline.map_or(available.len(), |index| index + 1);
        reader.consume(consumed);

        if newline.is_some() {
            if too_large {
                return Ok(FrameRead::TooLarge);
            }
            if frame.last() == Some(&b'\r') {
                frame.pop();
            }
            return Ok(FrameRead::Frame(frame));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_uses_one_explicit_socket() {
        let cli = Cli::try_parse_from(["pmux-mcp", "--socket", "/tmp/pmux.sock"]).unwrap();
        assert_eq!(cli.socket, PathBuf::from("/tmp/pmux.sock"));
    }

    #[test]
    fn protocol_negotiation_echoes_supported_versions() {
        assert_eq!(negotiate_version("2024-11-05"), "2024-11-05");
        assert_eq!(negotiate_version("unknown-future"), MCP_PROTOCOL_VERSION);
    }

    #[test]
    fn rpc_ids_are_verbatim_strings_null_or_integral_signed_safe_numbers() {
        for valid in [
            Value::Null,
            json!(""),
            json!("9007199254740992"),
            json!("id-\u{1f642}"),
            json!(-(MAX_SAFE_JSON_INTEGER as i64)),
            json!(MAX_SAFE_JSON_INTEGER),
            json!(1.0),
        ] {
            assert!(valid_rpc_id(&valid), "expected valid ID: {valid}");
        }
        for invalid in [
            json!(-(MAX_SAFE_JSON_INTEGER as i64) - 1),
            json!(MAX_SAFE_JSON_INTEGER + 1),
            json!(u64::MAX),
            json!(SAFE_JSON_INTEGER_EXCLUSIVE_LIMIT),
            json!(-SAFE_JSON_INTEGER_EXCLUSIVE_LIMIT),
            json!(1.5),
            json!(true),
            json!([]),
            json!({}),
        ] {
            assert!(!valid_rpc_id(&invalid), "expected invalid ID: {invalid}");
        }
    }

    #[test]
    fn outbound_number_preflight_recurses_through_arrays_and_objects() {
        assert!(outbound_json_numbers_are_safe(&json!({
            "bounds": [-(MAX_SAFE_JSON_INTEGER as i64), MAX_SAFE_JSON_INTEGER],
            "fraction": 1.5,
        })));
        assert!(!outbound_json_numbers_are_safe(&json!({
            "nested": [{"unsafe": MAX_SAFE_JSON_INTEGER + 1}],
        })));
        assert!(!outbound_json_numbers_are_safe(&json!({
            "nested": [{"unsafe": -(MAX_SAFE_JSON_INTEGER as i64) - 1}],
        })));
        assert!(!outbound_json_numbers_are_safe(&json!({
            "nested": [{"unsafe": SAFE_JSON_INTEGER_EXCLUSIVE_LIMIT}],
        })));
    }

    #[tokio::test]
    async fn bounded_reader_drains_an_oversized_line() {
        let mut input = vec![b'x'; MAX_MCP_FRAME_BYTES + 1];
        input.extend_from_slice(b"\n{}\n");
        let mut reader = BufReader::new(input.as_slice());
        assert_eq!(
            read_mcp_frame(&mut reader).await.unwrap(),
            FrameRead::TooLarge
        );
        assert_eq!(
            read_mcp_frame(&mut reader).await.unwrap(),
            FrameRead::Frame(b"{}".to_vec())
        );
    }

    #[tokio::test]
    async fn oversized_output_is_replaced_before_writing_a_frame() {
        let response = rpc_success(
            json!(17),
            json!({"structuredContent": {"text": "x".repeat(MAX_MCP_FRAME_BYTES)}}),
        );
        let mut output = Vec::new();
        write_response(&mut output, &response).await.unwrap();
        assert!(output.len() <= MAX_MCP_FRAME_BYTES + 1);
        let decoded: Value = serde_json::from_slice(&output[..output.len() - 1]).unwrap();
        assert_eq!(decoded["id"], 17);
        assert_eq!(decoded["error"]["code"], -32603);
        assert_eq!(
            decoded["error"]["message"],
            "MCP response exceeds the size limit"
        );
    }

    #[tokio::test]
    async fn oversized_string_id_uses_a_compact_null_correlated_fallback() {
        let response = rpc_success(json!("x".repeat(MAX_MCP_FRAME_BYTES)), json!({}));
        let mut output = Vec::new();
        write_response(&mut output, &response).await.unwrap();
        assert!(output.len() <= MAX_MCP_FRAME_BYTES + 1);
        let decoded: Value = serde_json::from_slice(&output[..output.len() - 1]).unwrap();
        assert!(decoded["id"].is_null());
        assert_eq!(decoded["error"]["code"], -32603);
        assert_eq!(
            decoded["error"]["message"],
            "MCP response exceeds the size limit"
        );
    }

    #[tokio::test]
    async fn unsafe_nested_output_is_replaced_with_a_correlated_safe_error() {
        let response = rpc_success(
            json!("opaque-id"),
            json!({"nested": [{"unsafe": u64::MAX}]}),
        );
        let mut output = Vec::new();
        write_response(&mut output, &response).await.unwrap();
        assert!(output.len() <= MAX_MCP_FRAME_BYTES + 1);
        let decoded: Value = serde_json::from_slice(&output[..output.len() - 1]).unwrap();
        assert_eq!(decoded["id"], "opaque-id");
        assert_eq!(decoded["error"]["code"], -32603);
        assert_eq!(
            decoded["error"]["message"],
            "MCP response contains an unsafe integer"
        );
        assert!(!decoded.to_string().contains(&u64::MAX.to_string()));
    }

    #[tokio::test]
    async fn invalid_numeric_request_ids_are_uncorrelated_protocol_errors() {
        let client = PmuxClient::new("/definitely/missing/pmux.sock").unwrap();
        for id in [
            json!(-(MAX_SAFE_JSON_INTEGER as i64) - 1),
            json!(MAX_SAFE_JSON_INTEGER + 1),
            json!(u64::MAX),
            json!(1.5),
        ] {
            let response = process_value(
                &client,
                json!({"jsonrpc": "2.0", "id": id, "method": "ping"}),
            )
            .await
            .unwrap();
            assert!(response["id"].is_null());
            assert_eq!(response["error"]["code"], -32600);
        }
    }

    #[tokio::test]
    async fn initialize_and_tool_list_do_not_contact_pmuxd() {
        let client = PmuxClient::new("/definitely/missing/pmux.sock").unwrap();
        let initialized = process_value(
            &client,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "test", "version": "1"}
                }
            }),
        )
        .await
        .unwrap();
        assert_eq!(initialized["result"]["protocolVersion"], "2025-06-18");

        let listed = process_value(
            &client,
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        )
        .await
        .unwrap();
        // Compared against the definition list rather than a literal. The
        // literal was `8`, and what this test is actually about is that
        // neither call touched the socket -- so the count was a number that had
        // to be edited every time a tool was added, for no reason connected to
        // what the test proves.
        assert_eq!(
            listed["result"]["tools"].as_array().unwrap().len(),
            crate::tools::published_tool_definitions().len()
        );
    }

    #[tokio::test]
    async fn unknown_tools_are_protocol_errors_without_name_echo() {
        let client = PmuxClient::new("/definitely/missing/pmux.sock").unwrap();
        let response = process_value(
            &client,
            json!({
                "jsonrpc": "2.0",
                "id": "call",
                "method": "tools/call",
                "params": {"name": "secret-legacy-tool", "arguments": {}}
            }),
        )
        .await
        .unwrap();
        assert_eq!(response["error"]["code"], -32602);
        assert!(!response.to_string().contains("secret-legacy-tool"));
    }

    #[tokio::test]
    async fn notifications_are_silent_but_null_request_ids_are_correlated() {
        let client = PmuxClient::new("/definitely/missing/pmux.sock").unwrap();
        assert!(
            process_value(
                &client,
                json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            )
            .await
            .is_none()
        );
        let response = process_value(
            &client,
            json!({"jsonrpc": "2.0", "id": null, "method": "ping"}),
        )
        .await
        .unwrap();
        assert!(response["id"].is_null());
        assert_eq!(response["result"], json!({}));
    }
}
