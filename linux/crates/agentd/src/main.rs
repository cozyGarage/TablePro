//! Headless TablePro agent process. Serves MCP on demand over stdio with a
//! required policy file and no GTK dependency.

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
use tablepro_agentd::DaemonProvider;
use tablepro_core::DriverRegistry;
use tablepro_mcp::{McpBridge, TokenPermissions, TokenStore, serve_stdio};
use tablepro_policy::{ApprovalOutcome, ApprovalRequest, ApprovalSink, AuditState, DenyApprovalSink, load_from_path};
use tablepro_storage::{AuditJournal, SavedConnection, load_connections};
use uuid::Uuid;

#[derive(Parser, Debug)]
#[command(name = "tablepro-agentd", about = "Headless TablePro MCP agent daemon")]
struct Args {
    /// Path to policy.toml (required).
    #[arg(long)]
    policy: PathBuf,

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
    tablepro_transport::install_crypto_provider();
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

    let provider = Arc::new(DaemonProvider::new(
        Arc::new(build_registry()),
        policy,
        audit,
        Arc::new(AuditState::new()),
        approval,
    ));
    let bridge = Arc::new(McpBridge::new(provider, tokens));

    serve_stdio(bridge).await?;
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
            socket_dir: None,
            database: "postgres".into(),
            username: "postgres".into(),
            use_tls: false,
            tls_mode: None,
            tls_root_cert: None,
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
