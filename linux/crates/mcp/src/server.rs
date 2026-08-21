use std::sync::Arc;

use axum::http::{HeaderMap, Uri, header::ORIGIN};
use serde_json::{Value as JsonValue, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

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

const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const DISCARD_CHUNK_BYTES: u64 = 8 * 1024;

async fn discard_rest_of_line<R>(reader: &mut R) -> Result<(), String>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    let mut chunk = Vec::new();
    loop {
        chunk.clear();
        let read = reader
            .take(DISCARD_CHUNK_BYTES)
            .read_until(b'\n', &mut chunk)
            .await
            .map_err(|e| e.to_string())?;
        if read == 0 || chunk.last() == Some(&b'\n') {
            return Ok(());
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
        let n = (&mut reader)
            .take(MAX_REQUEST_BYTES as u64 + 1)
            .read_line(&mut line)
            .await
            .map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        if n > MAX_REQUEST_BYTES && !line.ends_with('\n') {
            discard_rest_of_line(&mut reader).await?;
            write_response(
                &mut stdout,
                json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": {"code": -32600, "message": format!("request exceeds {MAX_REQUEST_BYTES} bytes")}
                }),
            )
            .await?;
            continue;
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

fn origin_is_allowed(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(ORIGIN) else {
        return true;
    };
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    if origin.is_empty() {
        return true;
    }
    let Ok(uri) = origin.parse::<Uri>() else {
        return false;
    };
    if !matches!(uri.scheme_str(), Some("http" | "https")) {
        return false;
    }
    matches!(
        uri.host().map(str::to_ascii_lowercase).as_deref(),
        Some("localhost" | "127.0.0.1" | "[::1]" | "claude.ai" | "app.cursor.com")
    )
}

/// Bind a loopback streamable-HTTP endpoint that forwards tool calls to
/// the same bridge. Loopback only by default.
pub async fn serve_streamable_http(bridge: Arc<McpBridge>, config: McpServerConfig) -> Result<(), String> {
    use axum::response::IntoResponse;
    use axum::{Json, Router, http::StatusCode, routing::post};
    use std::net::SocketAddr;

    if config.bind_host != "127.0.0.1" && config.bind_host != "localhost" && config.bind_host != "::1" {
        return Err("refusing to bind MCP HTTP outside loopback".into());
    }

    let bridge = bridge.clone();
    let app = Router::new().route(
        "/mcp",
        post(move |headers: HeaderMap, Json(body): Json<JsonValue>| {
            let bridge = bridge.clone();
            async move {
                if !origin_is_allowed(&headers) {
                    return (
                        StatusCode::FORBIDDEN,
                        Json(json!({
                            "jsonrpc": "2.0",
                            "id": null,
                            "error": {"code": -32000, "message": "forbidden origin"}
                        })),
                    )
                        .into_response();
                }
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
                Json(response).into_response()
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers_with_origin(origin: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(ORIGIN, HeaderValue::from_str(origin).unwrap());
        headers
    }

    #[tokio::test]
    async fn discarding_an_oversized_line_resyncs_on_the_next_request() {
        let oversized = "x".repeat(DISCARD_CHUNK_BYTES as usize * 3);
        let payload = format!("{oversized}\n{{\"id\":1}}\n");
        let mut reader = BufReader::new(std::io::Cursor::new(payload.into_bytes()));

        discard_rest_of_line(&mut reader).await.unwrap();

        let mut next = String::new();
        reader.read_line(&mut next).await.unwrap();
        assert_eq!(next.trim(), "{\"id\":1}");
    }

    #[tokio::test]
    async fn discarding_stops_at_end_of_input() {
        let mut reader = BufReader::new(std::io::Cursor::new(b"no newline here".to_vec()));
        discard_rest_of_line(&mut reader).await.unwrap();
        let mut next = String::new();
        assert_eq!(reader.read_line(&mut next).await.unwrap(), 0);
    }

    #[test]
    fn native_requests_without_origin_are_allowed() {
        assert!(origin_is_allowed(&HeaderMap::new()));
    }

    #[test]
    fn trusted_browser_origins_are_allowed() {
        for origin in [
            "http://localhost:3000",
            "http://127.0.0.1:8080",
            "http://[::1]:8080",
            "https://claude.ai",
            "https://app.cursor.com",
        ] {
            assert!(origin_is_allowed(&headers_with_origin(origin)), "{origin}");
        }
    }

    #[test]
    fn untrusted_browser_origins_are_rejected() {
        for origin in [
            "https://example.com",
            "https://cursor.com",
            "https://evil.claude.ai",
            "file:///tmp/request",
            "null",
        ] {
            assert!(!origin_is_allowed(&headers_with_origin(origin)), "{origin}");
        }
    }
}
