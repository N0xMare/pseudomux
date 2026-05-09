mod client;
mod tools;

use anyhow::Result;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing_subscriber::EnvFilter;

use client::DaemonClient;

#[tokio::main]
async fn main() -> Result<()> {
    // Log to stderr so stdout stays clean for MCP JSON-RPC
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let client = DaemonClient::connect();

    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let reader = BufReader::new(stdin);
    let mut lines = reader.lines();

    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }

        let request: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let err_resp = json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": { "code": -32700, "message": format!("Parse error: {e}") }
                });
                write_response(&mut stdout, &err_resp).await?;
                continue;
            }
        };

        let id = request.get("id").cloned();
        let method = request["method"].as_str().unwrap_or("");

        let response = match method {
            "initialize" => Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "pmux-mcp", "version": "0.1.0" }
                }
            })),
            "notifications/initialized" => None, // no response for notifications
            "tools/list" => Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "tools": tools::tool_definitions() }
            })),
            "tools/call" => {
                let params = &request["params"];
                let tool_name = params["name"].as_str().unwrap_or("");
                let arguments = &params["arguments"];

                match tools::handle_tool(&client, tool_name, arguments).await {
                    Ok(result_text) => Some(json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [{ "type": "text", "text": result_text }]
                        }
                    })),
                    Err(e) => Some(json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [{ "type": "text", "text": e.to_string() }],
                            "isError": true
                        }
                    })),
                }
            }
            _ => {
                if id.is_some() {
                    Some(json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32601, "message": format!("Method not found: {method}") }
                    }))
                } else {
                    None // unknown notification, ignore
                }
            }
        };

        if let Some(resp) = response {
            write_response(&mut stdout, &resp).await?;
        }
    }

    Ok(())
}

async fn write_response(stdout: &mut tokio::io::Stdout, resp: &Value) -> Result<()> {
    let mut out = serde_json::to_string(resp)?;
    out.push('\n');
    stdout.write_all(out.as_bytes()).await?;
    stdout.flush().await?;
    Ok(())
}
