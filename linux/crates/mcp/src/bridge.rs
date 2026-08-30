use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use tablepro_core::sql_dialect::quote_ident;
use tablepro_core::{
    ColumnInfo, Connection, ExecResult, ForeignKeyInfo, IndexInfo, OperationControl, QueryResult, TableInfo, Value,
    check_pre_dispatch,
};
use tablepro_policy::Principal;
use tablepro_storage::SavedConnection;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::auth::{McpScope, authorize_scopes};
use crate::rate_limit::RateLimiter;
use crate::tokens::{McpToken, TokenStore};

/// Supplies policy-gated connections. Implementations must never return a
/// raw driver connection that bypasses [`tablepro_policy::PolicyGuard`].
#[async_trait]
pub trait ConnectionProvider: Send + Sync {
    async fn list_saved_connections(&self) -> Result<Vec<SavedConnection>, String>;
    async fn connection(&self, connection_id: Uuid, principal: Principal) -> Result<Arc<dyn Connection>, String>;
}

const MAX_IDENTIFIER_BYTES: usize = 256;

pub struct McpBridge {
    provider: Arc<dyn ConnectionProvider>,
    tokens: Arc<TokenStore>,
    rate_limiter: RateLimiter,
    pub max_rows: u64,
    pub query_timeout_secs: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct McpLimits {
    pub requests_per_minute: u32,
    pub max_rows: u64,
    pub query_timeout_secs: u64,
}

impl Default for McpLimits {
    fn default() -> Self {
        Self {
            requests_per_minute: 120,
            max_rows: 500,
            query_timeout_secs: 30,
        }
    }
}

impl McpBridge {
    pub fn new(provider: Arc<dyn ConnectionProvider>, tokens: Arc<TokenStore>) -> Self {
        Self::with_limits(provider, tokens, McpLimits::default())
    }

    pub fn with_limits(provider: Arc<dyn ConnectionProvider>, tokens: Arc<TokenStore>, limits: McpLimits) -> Self {
        Self {
            provider,
            tokens,
            rate_limiter: RateLimiter::new(limits.requests_per_minute),
            max_rows: limits.max_rows,
            query_timeout_secs: limits.query_timeout_secs,
        }
    }

    pub fn tokens(&self) -> &TokenStore {
        &self.tokens
    }

    pub fn authenticate(&self, bearer: &str) -> Result<McpToken, String> {
        let token = bearer.strip_prefix("Bearer ").unwrap_or(bearer).trim();
        self.tokens.authenticate(token)
    }

    pub fn check_rate(&self, token: &McpToken) -> Result<(), String> {
        self.rate_limiter.check(&token.id.to_string())
    }

    pub fn ensure_connection_allowed(&self, token: &McpToken, connection_id: Uuid) -> Result<(), String> {
        if token.connection_allowlist.is_empty() {
            return Err("token has an empty connection allowlist".into());
        }
        if token.connection_allowlist.contains(&connection_id) {
            Ok(())
        } else {
            Err("token is not allowed to access this connection".into())
        }
    }

    pub async fn list_connections(&self, token: &McpToken) -> Result<Vec<SavedConnection>, String> {
        authorize_scopes(token.permissions, McpScope::ToolsRead)?;
        self.check_rate(token)?;
        if token.connection_allowlist.is_empty() {
            return Ok(Vec::new());
        }
        let mut list = self.provider.list_saved_connections().await?;
        list.retain(|c| token.connection_allowlist.contains(&c.id));
        Ok(list)
    }

    pub async fn with_connection<F, T>(&self, token: &McpToken, connection_id: Uuid, f: F) -> Result<T, String>
    where
        F: FnOnce(
                Arc<dyn Connection>,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, String>> + Send>>
            + Send,
        T: Send,
    {
        authorize_scopes(token.permissions, McpScope::ToolsRead)?;
        self.check_rate(token)?;
        self.ensure_connection_allowed(token, connection_id)?;
        let principal = Principal::Agent {
            token: token.id.to_string(),
            client: Some(token.name.clone()),
            model: None,
        };
        let conn = self.provider.connection(connection_id, principal).await?;
        f(conn).await
    }

    pub(crate) async fn driver_id_for(&self, token: &McpToken, connection_id: Uuid) -> Result<String, String> {
        self.ensure_connection_allowed(token, connection_id)?;
        self.provider
            .list_saved_connections()
            .await?
            .into_iter()
            .find(|c| c.id == connection_id)
            .map(|c| c.driver_id)
            .ok_or_else(|| format!("connection {connection_id} not found"))
    }

    pub async fn list_tables(&self, token: &McpToken, connection_id: Uuid) -> Result<Vec<TableInfo>, String> {
        let timeout = self.query_timeout_secs;
        self.with_connection(token, connection_id, move |conn| {
            Box::pin(async move {
                let control = operation_control(timeout);
                conn.list_tables_controlled(&control).await.map_err(|e| e.to_string())
            })
        })
        .await
    }

    pub async fn describe_table(
        &self,
        token: &McpToken,
        connection_id: Uuid,
        schema: Option<String>,
        table: String,
    ) -> Result<Vec<ColumnInfo>, String> {
        let table = validated_identifier(&table)?;
        let schema = match schema {
            Some(schema) => Some(validated_identifier(&schema)?),
            None => None,
        };
        let timeout = self.query_timeout_secs;
        self.with_connection(token, connection_id, move |conn| {
            Box::pin(async move {
                let control = operation_control(timeout);
                conn.fetch_columns_controlled(schema.as_deref(), &table, &control)
                    .await
                    .map_err(|e| e.to_string())
            })
        })
        .await
    }

    pub async fn execute_query(&self, token: &McpToken, connection_id: Uuid, sql: &str) -> Result<QueryResult, String> {
        authorize_scopes(token.permissions, McpScope::ToolsRead)?;
        let driver_id = self.driver_id_for(token, connection_id).await?;
        if sql_looks_like_write(sql, &driver_id) {
            authorize_scopes(token.permissions, McpScope::ToolsWrite)?;
        }
        let max_rows = self.max_rows;
        let timeout = self.query_timeout_secs;
        let sql = sql.to_string();
        self.with_connection(token, connection_id, move |conn| {
            Box::pin(async move {
                let control = operation_control(timeout);
                let result = conn.query_controlled(&sql, &control).await.map_err(|e| e.to_string())?;
                let mut result = result;
                if result.rows.len() as u64 > max_rows {
                    result.rows.truncate(max_rows as usize);
                    result.truncated = true;
                }
                Ok(result)
            })
        })
        .await
    }

    pub async fn table_schema(
        &self,
        token: &McpToken,
        connection_id: Uuid,
        schema: Option<String>,
        table: String,
    ) -> Result<TableSchema, String> {
        let table = validated_identifier(&table)?;
        let schema = match schema {
            Some(schema) => Some(validated_identifier(&schema)?),
            None => None,
        };
        let timeout = self.query_timeout_secs;
        self.with_connection(token, connection_id, move |conn| {
            Box::pin(async move {
                let control = operation_control(timeout);
                let columns = conn
                    .fetch_columns_controlled(schema.as_deref(), &table, &control)
                    .await
                    .map_err(|e| e.to_string())?;
                check_pre_dispatch(&control).map_err(|e| e.to_string())?;
                let indexes = conn
                    .fetch_indexes_controlled(schema.as_deref(), &table, &control)
                    .await
                    .map_err(|e| e.to_string())?;
                check_pre_dispatch(&control).map_err(|e| e.to_string())?;
                let foreign_keys = conn
                    .fetch_foreign_keys_controlled(schema.as_deref(), &table, &control)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(TableSchema {
                    columns,
                    indexes,
                    foreign_keys,
                })
            })
        })
        .await
    }

    pub async fn count_rows(
        &self,
        token: &McpToken,
        connection_id: Uuid,
        schema: Option<String>,
        table: String,
    ) -> Result<u64, String> {
        let driver_id = self.driver_id_for(token, connection_id).await?;
        let sql = count_statement(&driver_id, schema.as_deref(), &table)?;
        let result = self.execute_query(token, connection_id, &sql).await?;
        let value = result
            .rows
            .first()
            .and_then(|row| row.first())
            .ok_or("the engine returned no count row")?;
        count_from_value(value)
    }

    pub async fn browse_table(
        &self,
        token: &McpToken,
        connection_id: Uuid,
        schema: Option<String>,
        table: String,
        offset: u64,
        limit: u64,
    ) -> Result<QueryResult, String> {
        let table = validated_identifier(&table)?;
        let schema = match schema {
            Some(schema) => Some(validated_identifier(&schema)?),
            None => None,
        };
        let capped = limit.clamp(1, self.max_rows);
        let timeout = self.query_timeout_secs;
        self.with_connection(token, connection_id, move |conn| {
            Box::pin(async move {
                let control = operation_control(timeout);
                let mut result = conn
                    .fetch_rows_controlled(schema.as_deref(), &table, offset, capped, &control)
                    .await
                    .map_err(|e| e.to_string())?;
                if result.rows.len() as u64 > capped {
                    result.rows.truncate(capped as usize);
                    result.truncated = true;
                }
                if limit > capped {
                    result.truncated = true;
                }
                Ok(result)
            })
        })
        .await
    }

    pub async fn execute_write(
        &self,
        token: &McpToken,
        connection_id: Uuid,
        sql: &str,
        preview: bool,
    ) -> Result<WriteOutcome, String> {
        authorize_scopes(token.permissions, McpScope::ToolsWrite)?;
        let timeout = self.query_timeout_secs;
        let sql = sql.to_string();
        self.with_connection(token, connection_id, move |conn| {
            Box::pin(async move {
                let control = operation_control(timeout);
                if preview {
                    let mut tx = conn.begin().await.map_err(|e| e.to_string())?;
                    let result = tx.execute_controlled(&sql, &control).await;
                    tx.rollback().await.map_err(|e| e.to_string())?;
                    let result = result.map_err(|e| e.to_string())?;
                    Ok(WriteOutcome::Preview {
                        rows_affected: result.rows_affected,
                        rolled_back: true,
                    })
                } else {
                    let result = conn
                        .execute_controlled(&sql, &control)
                        .await
                        .map_err(|e| e.to_string())?;
                    Ok(WriteOutcome::Committed {
                        rows_affected: result.rows_affected,
                    })
                }
            })
        })
        .await
    }

    pub async fn execute_write_commit(
        &self,
        token: &McpToken,
        connection_id: Uuid,
        sql: &str,
    ) -> Result<ExecResult, String> {
        match self.execute_write(token, connection_id, sql, false).await? {
            WriteOutcome::Committed { rows_affected } => Ok(ExecResult { rows_affected }),
            WriteOutcome::Preview { .. } => Err("unexpected preview outcome".into()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TableSchema {
    pub columns: Vec<ColumnInfo>,
    pub indexes: Vec<IndexInfo>,
    pub foreign_keys: Vec<ForeignKeyInfo>,
}

#[derive(Debug, Clone)]
pub enum WriteOutcome {
    Preview { rows_affected: u64, rolled_back: bool },
    Committed { rows_affected: u64 },
}

fn validated_identifier(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("identifier must not be empty".into());
    }
    if trimmed.len() > MAX_IDENTIFIER_BYTES {
        return Err(format!("identifier exceeds {MAX_IDENTIFIER_BYTES} bytes"));
    }
    if trimmed.chars().any(char::is_control) {
        return Err("identifier must not contain control characters".into());
    }
    Ok(trimmed.to_string())
}

fn count_statement(driver_id: &str, schema: Option<&str>, table: &str) -> Result<String, String> {
    let table = quote_ident(driver_id, &validated_identifier(table)?);
    let target = match schema {
        Some(schema) => format!("{}.{}", quote_ident(driver_id, &validated_identifier(schema)?), table),
        None => table,
    };
    Ok(format!("SELECT COUNT(*) AS row_count FROM {target}"))
}

fn count_from_value(value: &Value) -> Result<u64, String> {
    match value {
        Value::Int(count) if *count >= 0 => Ok(*count as u64),
        Value::Decimal(count) => count
            .to_string()
            .parse::<u64>()
            .map_err(|_| "the engine returned a count this tool cannot read".to_string()),
        _ => Err("the engine returned a count this tool cannot read".into()),
    }
}

fn operation_control(timeout_secs: u64) -> OperationControl {
    OperationControl::new(
        CancellationToken::new(),
        Some(Instant::now() + Duration::from_secs(timeout_secs)),
    )
}

fn sql_looks_like_write(sql: &str, driver_id: &str) -> bool {
    let facts = tablepro_policy::classify(sql, driver_id);
    facts.writes
}

#[cfg(test)]
mod tests {
    use super::{McpLimits, count_from_value, count_statement, sql_looks_like_write, validated_identifier};
    use tablepro_core::Value;

    #[test]
    fn a_count_statement_quotes_the_target_for_the_engine() {
        assert_eq!(
            count_statement("postgres", Some("public"), "items").unwrap(),
            "SELECT COUNT(*) AS row_count FROM \"public\".\"items\""
        );
        assert_eq!(
            count_statement("mysql", None, "items").unwrap(),
            "SELECT COUNT(*) AS row_count FROM `items`"
        );
        assert_eq!(
            count_statement("mssql", None, "it]ems").unwrap(),
            "SELECT COUNT(*) AS row_count FROM [it]]ems]"
        );
    }

    #[test]
    fn a_hostile_identifier_cannot_escape_its_quotes() {
        let sql = count_statement("postgres", None, "items\"; TRUNCATE users --").unwrap();
        assert_eq!(
            sql,
            "SELECT COUNT(*) AS row_count FROM \"items\"\"; TRUNCATE users --\""
        );
        assert!(validated_identifier("").is_err());
        assert!(validated_identifier("  ").is_err());
        assert!(validated_identifier("items\nitems").is_err());
        assert!(validated_identifier("items\0").is_err());
        assert!(validated_identifier(&"x".repeat(1024)).is_err());
    }

    #[test]
    fn only_a_non_negative_integer_count_is_accepted() {
        assert_eq!(count_from_value(&Value::Int(7)).unwrap(), 7);
        assert_eq!(count_from_value(&Value::Decimal(12.into())).unwrap(), 12);
        assert!(count_from_value(&Value::Int(-1)).is_err());
        assert!(count_from_value(&Value::Text("7".into())).is_err());
        assert!(count_from_value(&Value::Null).is_err());
    }

    #[test]
    fn the_default_limits_are_the_values_the_bridge_has_always_used() {
        let limits = McpLimits::default();
        assert_eq!(limits.requests_per_minute, 120);
        assert_eq!(limits.max_rows, 500);
        assert_eq!(limits.query_timeout_secs, 30);
    }

    #[test]
    fn only_a_provable_read_skips_the_write_scope_check() {
        for sql in ["SELECT 1", "SELECT id FROM t WHERE id = 1"] {
            assert!(!sql_looks_like_write(sql, "postgres"), "{sql}");
        }
        for sql in [
            "DELETE FROM t WHERE id = 1",
            "CREATE TABLE t (id int)",
            "SELECT pg_read_file('/etc/passwd')",
            "COPY t TO PROGRAM 'sh'",
            "this is not sql at all",
            "",
        ] {
            assert!(sql_looks_like_write(sql, "postgres"), "{sql}");
        }
    }
}
