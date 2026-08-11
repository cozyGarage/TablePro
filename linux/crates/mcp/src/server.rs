use std::sync::Arc;

use serde_json::{Value as JsonValue, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::bridge::McpBridge;
use crate::tools::{self, TOOL_NAMES};

#[derive(Debug, Clone)]
pub struct McpServerConfig {
    pub bind_host: String,
    pub bind_port: u16,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            bind_host: "127.0.0.1".into(),
            bind_port: 17432,
        }
    }
}

/// Minimal MCP-compatible JSON-RPC loop over stdio. Speaks enough of the
/// protocol for Cursor / Claude Code: `initialize`, `tools/list`,
/// `tools/call`. Auth via `Authorization` in `_meta` or `params.token`.
pub async fn serve_stdio(bridge: Arc<McpBridge>) -> Result<(), String> {
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin);
    let mut stdout = tokio::io::stdout();
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader.read_line(&mut line).await.map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let request: JsonValue = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                write_response(
                    &mut stdout,
                    json!({
                        "jsonrpc": "2.0",
                        "id": null,
                        "error": {"code": -32700, "message": format!("parse error: {e}")}
                    }),
                )
                .await?;
                continue;
            }
        };
        let id = request.get("id").cloned().unwrap_or(JsonValue::Null);
        let method = request.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = request.get("params").cloned().unwrap_or(json!({}));

        let response = match method {
            "initialize" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "tablepro", "version": "0.1.0"}
                }
            }),
            "notifications/initialized" | "initialized" => continue,
            "tools/list" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "tools": TOOL_NAMES.iter().map(|name| json!({
                        "name": name,
                        "description": tool_description(name),
                        "inputSchema": tool_schema(name),
                    })).collect::<Vec<_>>()
                }
            }),
            "tools/call" => match handle_tool_call(&bridge, params).await {
                Ok(content) => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{"type": "text", "text": content.to_string()}],
                        "isError": false
                    }
                }),
                Err(e) => json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{"type": "text", "text": e}],
                        "isError": true
                    }
                }),
            },
            "ping" => json!({"jsonrpc": "2.0", "id": id, "result": {}}),
            other => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": format!("method not found: {other}")}
            }),
        };
        write_response(&mut stdout, response).await?;
    }
    Ok(())
}

async fn handle_tool_call(bridge: &McpBridge, params: JsonValue) -> Result<JsonValue, String> {
    let name = params.get("name").and_then(|v| v.as_str()).ok_or("missing tool name")?;
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    let token_str = params
        .get("token")
        .and_then(|v| v.as_str())
        .or_else(|| params.pointer("/_meta/authorization").and_then(|v| v.as_str()))
        .or_else(|| args.get("token").and_then(|v| v.as_str()))
        .ok_or("missing token (pass params.token or arguments.token)")?;
    let token = bridge.authenticate(token_str)?;
    tools::dispatch(bridge, &token, name, args).await
}

async fn write_response(stdout: &mut tokio::io::Stdout, value: JsonValue) -> Result<(), String> {
    let mut line = serde_json::to_string(&value).map_err(|e| e.to_string())?;
    line.push('\n');
    stdout.write_all(line.as_bytes()).await.map_err(|e| e.to_string())?;
    stdout.flush().await.map_err(|e| e.to_string())
}

fn tool_description(name: &str) -> &'static str {
    match name {
        "list_connections" => "List saved database connections visible to this token",
        "list_tables" => "List tables on a connection",
        "describe_table" => "Describe columns of a table",
        "execute_query" => "Run a read SQL query (writes require tools:write scope and policy approval)",
        "execute_write" => "Run a write with optional transaction preview (preview=true by default)",
        "explain_query" => "Run EXPLAIN on a SQL statement",
        "export_data" => "Run a query and return CSV or JSON",
        _ => "",
    }
}

fn tool_schema(name: &str) -> JsonValue {
    match name {
        "list_connections" => json!({"type": "object", "properties": {}}),
        "list_tables" | "execute_query" | "execute_write" | "explain_query" | "export_data" => json!({
            "type": "object",
            "properties": {
                "connection_id": {"type": "string"},
                "sql": {"type": "string"},
                "preview": {"type": "boolean"},
                "format": {"type": "string"},
                "token": {"type": "string"}
            },
            "required": ["connection_id"]
        }),
        "describe_table" => json!({
            "type": "object",
            "properties": {
                "connection_id": {"type": "string"},
                "schema": {"type": "string"},
                "table": {"type": "string"},
                "token": {"type": "string"}
            },
            "required": ["connection_id", "table"]
        }),
        _ => json!({"type": "object"}),
    }
}

/// Bind a loopback streamable-HTTP endpoint that forwards tool calls to
/// the same bridge. Loopback only by default.
pub async fn serve_streamable_http(bridge: Arc<McpBridge>, config: McpServerConfig) -> Result<(), String> {
    use axum::{Json, Router, routing::post};
    use std::net::SocketAddr;

    if config.bind_host != "127.0.0.1" && config.bind_host != "localhost" && config.bind_host != "::1" {
        return Err("refusing to bind MCP HTTP outside loopback".into());
    }

    let bridge = bridge.clone();
    let app = Router::new().route(
        "/mcp",
        post(move |Json(body): Json<JsonValue>| {
            let bridge = bridge.clone();
            async move {
                let id = body.get("id").cloned().unwrap_or(JsonValue::Null);
                let method = body.get("method").and_then(|m| m.as_str()).unwrap_or("");
                let params = body.get("params").cloned().unwrap_or(json!({}));
                let result = match method {
                    "tools/call" => handle_tool_call(&bridge, params).await,
                    "tools/list" => Ok(json!({
                        "tools": TOOL_NAMES.iter().map(|name| json!({
                            "name": name,
                            "description": tool_description(name),
                            "inputSchema": tool_schema(name),
                        })).collect::<Vec<_>>()
                    })),
                    "initialize" => Ok(json!({
                        "protocolVersion": "2025-11-25",
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "tablepro", "version": "0.1.0"}
                    })),
                    other => Err(format!("unsupported method: {other}")),
                };
                let response = match result {
                    Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
                    Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": e}}),
                };
                Json(response)
            }
        }),
    );

    let addr: SocketAddr = format!("{}:{}", config.bind_host, config.bind_port)
        .parse()
        .map_err(|e| format!("bad bind address: {e}"))?;
    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| e.to_string())?;
    tracing::info!(%addr, "MCP HTTP listening (loopback)");
    axum::serve(listener, app).await.map_err(|e| e.to_string())
}
