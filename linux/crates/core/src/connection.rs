use async_trait::async_trait;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};

use crate::error::DriverError;
use crate::operation::{OperationControl, run_controlled};
use crate::query::{ColumnInfo, ExecResult, ForeignKeyInfo, IndexInfo, QueryResult, TableInfo, Value};
use crate::tls::TlsConfig;
use crate::transaction::Transaction;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    #[default]
    Password,
    Kerberos,
}

#[derive(Debug, Clone)]
pub struct ConnectOptions {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: String,
    pub password: SecretString,
    pub tls: TlsConfig,
    pub auth_mode: AuthMode,
    pub service_endpoint: Option<(String, u16)>,
}

impl ConnectOptions {
    pub fn service_address(&self) -> (&str, u16) {
        self.service_endpoint
            .as_ref()
            .map_or((self.host.as_str(), self.port), |(host, port)| (host.as_str(), *port))
    }
}

impl Default for ConnectOptions {
    fn default() -> Self {
        Self {
            host: "localhost".to_string(),
            port: 0,
            database: String::new(),
            username: String::new(),
            password: SecretString::new(String::new().into()),
            tls: TlsConfig::disabled(),
            auth_mode: AuthMode::Password,
            service_endpoint: None,
        }
    }
}

#[async_trait]
pub trait Connection: Send + Sync {
    async fn list_tables(&self) -> Result<Vec<TableInfo>, DriverError>;
    async fn fetch_columns(&self, schema: Option<&str>, table: &str) -> Result<Vec<ColumnInfo>, DriverError>;
    async fn fetch_rows(
        &self,
        schema: Option<&str>,
        table: &str,
        offset: u64,
        limit: u64,
    ) -> Result<QueryResult, DriverError>;
    async fn query(&self, sql: &str) -> Result<QueryResult, DriverError>;
    async fn query_controlled(&self, sql: &str, control: &OperationControl) -> Result<QueryResult, DriverError> {
        run_controlled(self.query(sql), control).await
    }
    /// Parameterised SELECT. Bound `Value`s are passed through to the
    /// driver's prepare/bind path (sqlx::query::bind for the built-in
    /// drivers). Default impl delegates to `query` when params is
    /// empty, so legacy callers compile unchanged; drivers that
    /// support real parameter binding override.
    async fn query_params(&self, sql: &str, params: &[Value]) -> Result<QueryResult, DriverError> {
        if params.is_empty() {
            self.query(sql).await
        } else {
            Err(DriverError::Internal(
                "query_params is not implemented for this driver".into(),
            ))
        }
    }
    async fn query_params_controlled(
        &self,
        sql: &str,
        params: &[Value],
        control: &OperationControl,
    ) -> Result<QueryResult, DriverError> {
        run_controlled(self.query_params(sql, params), control).await
    }
    async fn execute(&self, sql: &str) -> Result<ExecResult, DriverError>;
    async fn execute_controlled(&self, sql: &str, control: &OperationControl) -> Result<ExecResult, DriverError> {
        run_controlled(self.execute(sql), control).await
    }
    async fn execute_params(&self, sql: &str, params: &[Value]) -> Result<ExecResult, DriverError>;
    async fn execute_params_controlled(
        &self,
        sql: &str,
        params: &[Value],
        control: &OperationControl,
    ) -> Result<ExecResult, DriverError> {
        run_controlled(self.execute_params(sql, params), control).await
    }
    /// Run a sequence of parameterised statements inside a single
    /// database transaction. Rolls back automatically if any
    /// statement errors; returns `DriverError::Transaction` with the
    /// failing statement's index. Returns one `rows_affected` value
    /// per successful statement, in order. Used by the inline-edit
    /// changeset Save flow so all pending row inserts / updates /
    /// deletes commit atomically.
    async fn execute_in_transaction(&self, statements: &[(String, Vec<Value>)]) -> Result<Vec<u64>, DriverError>;
    /// Indexes defined on `table`. Implementations may include the
    /// implicit primary-key index with `primary = true` so the UI can
    /// render it as read-only. Default returns empty so existing
    /// drivers compile before they're filled in.
    async fn fetch_indexes(&self, _schema: Option<&str>, _table: &str) -> Result<Vec<IndexInfo>, DriverError> {
        Ok(Vec::new())
    }
    /// Foreign-key constraints declared on `table`. Default returns
    /// empty for the same reason as `fetch_indexes`.
    async fn fetch_foreign_keys(
        &self,
        _schema: Option<&str>,
        _table: &str,
    ) -> Result<Vec<ForeignKeyInfo>, DriverError> {
        Ok(Vec::new())
    }
    /// Open an interactive transaction for preview-then-commit flows.
    /// Default returns `Unsupported` so drivers adopt incrementally.
    async fn begin(&self) -> Result<Box<dyn Transaction>, DriverError> {
        Err(DriverError::Unsupported(
            "begin is not implemented for this driver".into(),
        ))
    }
    /// Best-effort server version string (e.g. `PostgreSQL 16.3`). Default
    /// returns `None` when the driver has not implemented detection.
    async fn server_version(&self) -> Result<Option<String>, DriverError> {
        Ok(None)
    }
    async fn ping(&self) -> Result<(), DriverError>;
    async fn close(self: Box<Self>) -> Result<(), DriverError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_address_uses_dial_endpoint_for_direct_connections() {
        let options = ConnectOptions {
            host: "sql.corp.example".into(),
            port: 1433,
            ..Default::default()
        };

        assert_eq!(options.service_address(), ("sql.corp.example", 1433));
    }

    #[test]
    fn service_address_preserves_service_identity_through_tunnels() {
        let options = ConnectOptions {
            host: "127.0.0.1".into(),
            port: 54321,
            service_endpoint: Some(("sql.corp.example".into(), 1433)),
            ..Default::default()
        };

        assert_eq!(options.service_address(), ("sql.corp.example", 1433));
    }
}
