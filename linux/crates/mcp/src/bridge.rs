use std::sync::Arc;

use async_trait::async_trait;
use tablepro_core::{Connection, ExecResult, QueryResult, TableInfo};
use tablepro_policy::Principal;
use tablepro_storage::SavedConnection;
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

pub struct McpBridge {
    provider: Arc<dyn ConnectionProvider>,
    tokens: Arc<TokenStore>,
    rate_limiter: RateLimiter,
    pub max_rows: u64,
    pub query_timeout_secs: u64,
}

impl McpBridge {
    pub fn new(provider: Arc<dyn ConnectionProvider>, tokens: Arc<TokenStore>) -> Self {
        Self {
            provider,
            tokens,
            rate_limiter: RateLimiter::new(120),
            max_rows: 500,
            query_timeout_secs: 30,
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

    pub async fn list_tables(&self, token: &McpToken, connection_id: Uuid) -> Result<Vec<TableInfo>, String> {
        self.with_connection(token, connection_id, |conn| {
            Box::pin(async move { conn.list_tables().await.map_err(|e| e.to_string()) })
        })
        .await
    }

    pub async fn execute_query(&self, token: &McpToken, connection_id: Uuid, sql: &str) -> Result<QueryResult, String> {
        authorize_scopes(token.permissions, McpScope::ToolsRead)?;
        if sql_looks_like_write(sql) {
            authorize_scopes(token.permissions, McpScope::ToolsWrite)?;
        }
        let max_rows = self.max_rows;
        let timeout = self.query_timeout_secs;
        let sql = sql.to_string();
        self.with_connection(token, connection_id, move |conn| {
            Box::pin(async move {
                let fut = conn.query(&sql);
                let result = tokio::time::timeout(std::time::Duration::from_secs(timeout), fut)
                    .await
                    .map_err(|_| "query timed out".to_string())?
                    .map_err(|e| e.to_string())?;
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
                if preview {
                    let mut tx = conn.begin().await.map_err(|e| e.to_string())?;
                    let result = tokio::time::timeout(std::time::Duration::from_secs(timeout), tx.execute(&sql))
                        .await
                        .map_err(|_| "query timed out".to_string())?
                        .map_err(|e| e.to_string())?;
                    // Leave open for caller to commit via separate path —
                    // for the first cut we roll back after reporting.
                    let rows = result.rows_affected;
                    tx.rollback().await.map_err(|e| e.to_string())?;
                    Ok(WriteOutcome::Preview {
                        rows_affected: rows,
                        rolled_back: true,
                    })
                } else {
                    let result = tokio::time::timeout(std::time::Duration::from_secs(timeout), conn.execute(&sql))
                        .await
                        .map_err(|_| "query timed out".to_string())?
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
pub enum WriteOutcome {
    Preview { rows_affected: u64, rolled_back: bool },
    Committed { rows_affected: u64 },
}

fn sql_looks_like_write(sql: &str) -> bool {
    let facts = tablepro_policy::classify(sql, "postgres");
    facts.writes
}
