//! Headless TablePro agent daemon. Serves MCP over stdio or loopback HTTP
//! with a required policy file and no GTK dependency.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use clap::{Parser, ValueEnum};
use drivers_clickhouse::ClickhouseDriver;
#[cfg(feature = "duckdb")]
use drivers_duckdb::DuckdbDriver;
use drivers_mongodb::MongodbDriver;
use drivers_mssql::MssqlDriver;
use drivers_mysql::MysqlDriver;
#[cfg(feature = "odpi")]
use drivers_oracle::OracleDriver;
use drivers_postgres::PgDriver;
use drivers_redis::RedisDriver;
use drivers_sqlite::SqliteDriver;
use tablepro_core::{AuthMode, ConnectOptions, Connection, DriverRegistry, TlsConfig};
use tablepro_mcp::{
    ConnectionProvider, McpBridge, McpServerConfig, TokenPermissions, TokenStore, serve_stdio, serve_streamable_http,
};
use tablepro_policy::{
    ApprovalOutcome, ApprovalRequest, ApprovalSink, AuditState, DenyApprovalSink, GuardContext, PolicyConfig,
    PolicyGuard, Principal, load_from_path,
};
use tablepro_storage::{AuditJournal, SavedConnection, load_connections, load_password};
use uuid::Uuid;

#[derive(Parser, Debug)]
#[command(name = "tablepro-agentd", about = "Headless TablePro MCP agent daemon")]
struct Args {
    /// Path to policy.toml (required).
    #[arg(long)]
    policy: PathBuf,

    /// Transport: stdio (default) or http.
    #[arg(long, default_value = "stdio")]
    transport: Transport,

    /// HTTP bind port (loopback only).
    #[arg(long, default_value_t = 17432)]
    port: u16,

    /// Approval strategy when policy requires approval.
    #[arg(long, default_value = "deny")]
    approval: ApprovalMode,

    /// Issue a new read-write token and print it, then exit.
    #[arg(long)]
    issue_token: Option<String>,

    #[arg(long, requires = "issue_token")]
    connection: Vec<Uuid>,
}

#[derive(Clone, Debug, ValueEnum)]
enum Transport {
    Stdio,
    Http,
}

#[derive(Clone, Debug, ValueEnum)]
enum ApprovalMode {
    Deny,
    Tty,
}

struct TtyApprovalSink;

#[async_trait]
impl ApprovalSink for TtyApprovalSink {
    async fn request(&self, req: ApprovalRequest) -> ApprovalOutcome {
        use std::io::{Write, stdin, stdout};
        let _ = writeln!(
            stdout(),
            "\n[tablepro-agentd] approval required\n  connection: {}\n  rule: {}\n  reason: {}\n  sql: {}\nApprove? [y/N] ",
            req.connection_name,
            req.rule,
            req.reason,
            req.sql
        );
        let _ = stdout().flush();
        let mut line = String::new();
        match stdin().read_line(&mut line) {
            Ok(_) if line.trim().eq_ignore_ascii_case("y") => ApprovalOutcome::AllowOnce,
            _ => ApprovalOutcome::Deny,
        }
    }
}

struct DaemonProvider {
    registry: Arc<DriverRegistry>,
    policy: Arc<PolicyConfig>,
    audit: Arc<dyn tablepro_policy::AuditSink>,
    audit_state: Arc<AuditState>,
    approval: Arc<dyn tablepro_policy::ApprovalSink>,
}

#[async_trait]
impl ConnectionProvider for DaemonProvider {
    async fn list_saved_connections(&self) -> Result<Vec<SavedConnection>, String> {
        load_connections().await.map_err(|e| e.to_string())
    }

    async fn connection(&self, connection_id: Uuid, principal: Principal) -> Result<Arc<dyn Connection>, String> {
        let saved = load_connections()
            .await
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|c| c.id == connection_id)
            .ok_or_else(|| format!("connection {connection_id} not found"))?;

        let driver = self
            .registry
            .get(&saved.driver_id)
            .ok_or_else(|| format!("driver {} not registered", saved.driver_id))?;
        let password = match saved.auth_mode {
            AuthMode::Password => load_password(saved.id)
                .await
                .ok()
                .flatten()
                .unwrap_or_else(|| secrecy::SecretString::new(String::new().into())),
            AuthMode::Kerberos => secrecy::SecretString::new(String::new().into()),
        };
        if saved.auth_mode == AuthMode::Kerberos && !driver.supports_integrated_auth() {
            return Err(format!(
                "the {} driver does not support Windows (Kerberos) authentication",
                driver.display_name()
            ));
        }

        let opts = ConnectOptions {
            host: saved.host.clone(),
            port: saved.port,
            database: saved.database.clone(),
            username: saved.username.clone(),
            password,
            tls: TlsConfig {
                mode: saved.effective_tls_mode(),
                ..Default::default()
            },
            auth_mode: saved.auth_mode,
            service_endpoint: None,
            forwarded_socket_dir: None,
        };
        let raw = driver.connect(opts).await.map_err(|e| e.to_string())?;
        let ctx = GuardContext {
            connection_id: saved.id,
            connection_name: saved.name.clone(),
            driver_id: saved.driver_id.clone(),
            environment: saved.environment,
            read_only: saved.read_only,
            principal,
            policy: self.policy.clone(),
            approval: self.approval.clone(),
            audit: self.audit.clone(),
            audit_state: self.audit_state.clone(),
        };
        Ok(Arc::new(PolicyGuard::new(Arc::from(raw), ctx)) as Arc<dyn Connection>)
    }
}

fn validate_token_connections(requested: &[Uuid], saved: &[SavedConnection]) -> Result<(), String> {
    if requested.is_empty() {
        return Err("--issue-token requires at least one --connection UUID".into());
    }
    let saved_ids: HashSet<Uuid> = saved.iter().map(|connection| connection.id).collect();
    if let Some(unknown) = requested.iter().find(|id| !saved_ids.contains(id)) {
        return Err(format!("connection {unknown} not found"));
    }
    Ok(())
}

fn build_registry() -> DriverRegistry {
    let mut r = DriverRegistry::new();
    r.register(Arc::new(ClickhouseDriver));
    #[cfg(feature = "duckdb")]
    r.register(Arc::new(DuckdbDriver));
    r.register(Arc::new(MongodbDriver));
    r.register(Arc::new(MssqlDriver));
    r.register(Arc::new(MysqlDriver));
    #[cfg(feature = "odpi")]
    r.register(Arc::new(OracleDriver));
    r.register(Arc::new(PgDriver));
    r.register(Arc::new(RedisDriver));
    r.register(Arc::new(SqliteDriver));
    r
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();
    if !args.policy.exists() {
        eprintln!("policy file not found: {}", args.policy.display());
        std::process::exit(2);
    }
    let policy = Arc::new(load_from_path(&args.policy).map_err(|e| e.to_string())?);

    let tokens = Arc::new(TokenStore::open_default().map_err(|e| e.to_string())?);
    if let Some(name) = args.issue_token {
        let saved = load_connections().await?;
        validate_token_connections(&args.connection, &saved)?;
        let (_meta, plain) = tokens.issue(name, TokenPermissions::ReadWrite, args.connection, None)?;
        println!("{plain}");
        return Ok(());
    }

    let approval: Arc<dyn tablepro_policy::ApprovalSink> = match args.approval {
        ApprovalMode::Deny => Arc::new(DenyApprovalSink),
        ApprovalMode::Tty => Arc::new(TtyApprovalSink),
    };
    let journal =
        AuditJournal::open_default().map_err(|error| format!("required audit journal unavailable: {error}"))?;
    if journal.recovery().recovered_unresolved_operations() {
        return Err(format!(
            "refusing to start with {} unresolved audit operation(s)",
            journal.recovery().recovered_operation_ids().len()
        )
        .into());
    }
    let audit: Arc<dyn tablepro_policy::AuditSink> = Arc::new(journal);

    let provider = Arc::new(DaemonProvider {
        registry: Arc::new(build_registry()),
        policy,
        audit,
        audit_state: Arc::new(AuditState::new()),
        approval,
    });
    let bridge = Arc::new(McpBridge::new(provider, tokens));

    match args.transport {
        Transport::Stdio => serve_stdio(bridge).await?,
        Transport::Http => {
            serve_streamable_http(
                bridge,
                McpServerConfig {
                    bind_host: "127.0.0.1".into(),
                    bind_port: args.port,
                },
            )
            .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tablepro_core::{AuthMode, Environment};

    fn saved_connection(id: Uuid) -> SavedConnection {
        SavedConnection {
            id,
            name: "test".into(),
            driver_id: "postgres".into(),
            host: "localhost".into(),
            port: 5432,
            database: "postgres".into(),
            username: "postgres".into(),
            use_tls: false,
            tls_mode: None,
            read_only: false,
            auth_mode: AuthMode::Password,
            environment: Environment::Local,
            ssh: None,
            last_opened_at: None,
        }
    }

    #[test]
    fn token_connections_require_at_least_one_saved_connection() {
        let id = Uuid::new_v4();
        let saved = vec![saved_connection(id)];

        assert!(validate_token_connections(&[], &saved).is_err());
        assert!(validate_token_connections(&[Uuid::new_v4()], &saved).is_err());
        assert!(validate_token_connections(&[id], &saved).is_ok());
    }
}
