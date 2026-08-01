//! Tool catalogue. Names are stable; MCP clients bind to these strings.

pub const TOOL_NAMES: &[&str] = &[
    "list_connections",
    "list_tables",
    "describe_table",
    "execute_query",
    "execute_write",
    "explain_query",
    "search_query_history",
    "export_data",
];

use serde_json::{Value as JsonValue, json};
use uuid::Uuid;

use crate::bridge::{McpBridge, WriteOutcome};
use crate::tokens::McpToken;

pub async fn dispatch(bridge: &McpBridge, token: &McpToken, name: &str, args: JsonValue) -> Result<JsonValue, String> {
    match name {
        "list_connections" => {
            let list = bridge.list_connections(token).await?;
            Ok(json!(
                list.into_iter()
                    .map(|c| json!({
                        "id": c.id,
                        "name": c.name,
                        "driver_id": c.driver_id,
                        "environment": c.environment,
                        "read_only": c.read_only,
                    }))
                    .collect::<Vec<_>>()
            ))
        }
        "list_tables" => {
            let id = parse_uuid(&args, "connection_id")?;
            let tables = bridge.list_tables(token, id).await?;
            Ok(json!(
                tables
                    .into_iter()
                    .map(|t| json!({"schema": t.schema, "name": t.name}))
                    .collect::<Vec<_>>()
            ))
        }
        "describe_table" => {
            let id = parse_uuid(&args, "connection_id")?;
            let table = args
                .get("table")
                .and_then(|v| v.as_str())
                .ok_or("missing table")?
                .to_string();
            let schema = args.get("schema").and_then(|v| v.as_str()).map(|s| s.to_string());
            bridge
                .with_connection(token, id, |conn| {
                    Box::pin(async move {
                        let cols = conn
                            .fetch_columns(schema.as_deref(), &table)
                            .await
                            .map_err(|e| e.to_string())?;
                        Ok(json!(
                            cols.into_iter()
                                .map(|c| json!({
                                    "name": c.name,
                                    "data_type": c.data_type,
                                    "nullable": c.nullable,
                                    "primary_key": c.primary_key,
                                }))
                                .collect::<Vec<_>>()
                        ))
                    })
                })
                .await
        }
        "execute_query" => {
            let id = parse_uuid(&args, "connection_id")?;
            let sql = args
                .get("sql")
                .and_then(|v| v.as_str())
                .ok_or("missing sql")?
                .to_string();
            let result = bridge.execute_query(token, id, &sql).await?;
            Ok(json!({
                "columns": result.columns.iter().map(|c| &c.name).collect::<Vec<_>>(),
                "rows": result.rows.iter().map(|r| {
                    r.iter().map(value_to_json).collect::<Vec<_>>()
                }).collect::<Vec<_>>(),
                "truncated": result.truncated,
            }))
        }
        "execute_write" => {
            let id = parse_uuid(&args, "connection_id")?;
            let sql = args
                .get("sql")
                .and_then(|v| v.as_str())
                .ok_or("missing sql")?
                .to_string();
            let preview = args.get("preview").and_then(|v| v.as_bool()).unwrap_or(true);
            let outcome = bridge.execute_write(token, id, &sql, preview).await?;
            Ok(match outcome {
                WriteOutcome::Preview {
                    rows_affected,
                    rolled_back,
                } => json!({
                    "preview": true,
                    "rows_affected": rows_affected,
                    "rolled_back": rolled_back,
                    "message": "Write previewed inside a transaction and rolled back. Re-call with preview=false after approval to commit.",
                }),
                WriteOutcome::Committed { rows_affected } => json!({
                    "preview": false,
                    "rows_affected": rows_affected,
                    "committed": true,
                }),
            })
        }
        "explain_query" => {
            let id = parse_uuid(&args, "connection_id")?;
            let sql = args
                .get("sql")
                .and_then(|v| v.as_str())
                .ok_or("missing sql")?
                .to_string();
            let explain = format!("EXPLAIN {sql}");
            let result = bridge.execute_query(token, id, &explain).await?;
            Ok(json!({
                "plan_rows": result.rows.iter().map(|r| {
                    r.iter().map(value_to_json).collect::<Vec<_>>()
                }).collect::<Vec<_>>(),
            }))
        }
        "search_query_history" => {
            let q = args.get("query").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let filter = tablepro_storage::query_history::SearchFilter {
                needle: if q.is_empty() { None } else { Some(q) },
                limit: 50,
                ..Default::default()
            };
            let hits = tablepro_storage::query_history::search(filter)
                .await
                .map_err(|e| e.to_string())?;
            Ok(json!(
                hits.into_iter()
                    .map(|h| json!({
                        "query": h.query,
                        "connection_id": h.connection_id,
                        "executed_at": chrono::DateTime::<chrono::Utc>::from(h.executed_at).to_rfc3339(),
                        "was_successful": h.success,
                    }))
                    .collect::<Vec<_>>()
            ))
        }
        "export_data" => {
            let id = parse_uuid(&args, "connection_id")?;
            let sql = args
                .get("sql")
                .and_then(|v| v.as_str())
                .ok_or("missing sql")?
                .to_string();
            let format = args.get("format").and_then(|v| v.as_str()).unwrap_or("json");
            let result = bridge.execute_query(token, id, &sql).await?;
            match format {
                "csv" => {
                    let mut out = String::new();
                    out.push_str(
                        &result
                            .columns
                            .iter()
                            .map(|c| c.name.clone())
                            .collect::<Vec<_>>()
                            .join(","),
                    );
                    out.push('\n');
                    for row in &result.rows {
                        let cells: Vec<String> = row.iter().map(|v| format!("{}", value_to_json(v))).collect();
                        out.push_str(&cells.join(","));
                        out.push('\n');
                    }
                    Ok(json!({"format": "csv", "content": out}))
                }
                _ => Ok(json!({
                    "format": "json",
                    "columns": result.columns.iter().map(|c| &c.name).collect::<Vec<_>>(),
                    "rows": result.rows.iter().map(|r| {
                        r.iter().map(value_to_json).collect::<Vec<_>>()
                    }).collect::<Vec<_>>(),
                })),
            }
        }
        other => Err(format!("unknown tool: {other}")),
    }
}

fn parse_uuid(args: &JsonValue, key: &str) -> Result<Uuid, String> {
    let s = args
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("missing {key}"))?;
    Uuid::parse_str(s).map_err(|e| e.to_string())
}

fn value_to_json(v: &tablepro_core::Value) -> JsonValue {
    match v {
        tablepro_core::Value::Null => JsonValue::Null,
        tablepro_core::Value::Bool(b) => json!(b),
        tablepro_core::Value::Int(i) => json!(i),
        tablepro_core::Value::Float(f) => json!(f),
        tablepro_core::Value::Text(s) => json!(s),
        tablepro_core::Value::Bytes(b) => json!(format!("\\x{}", hex::encode(b))),
        tablepro_core::Value::Date(d) => json!(d.to_string()),
        tablepro_core::Value::Time(t) => json!(t.to_string()),
        tablepro_core::Value::DateTime(d) => json!(d.to_string()),
        tablepro_core::Value::TimestampTz(d) => json!(d.to_rfc3339()),
        tablepro_core::Value::Decimal(d) => json!(d.to_string()),
        tablepro_core::Value::Uuid(u) => json!(u.to_string()),
        tablepro_core::Value::Json(j) => j.clone(),
    }
}
