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

    /// Issue a new token and print it, then exit.
    #[arg(long)]
    issue_token: Option<String>,

    #[arg(long, requires = "issue_token")]
    connection: Vec<Uuid>,

    /// Scope for --issue-token. Defaults to the least privilege that is useful.
    #[arg(long, requires = "issue_token", default_value = "read-only")]
    permissions: TokenScope,

    /// Expire the issued token after this many days.
    #[arg(long, requires = "issue_token")]
    expires_days: Option<u32>,

    /// Write the issued token here with owner-only permissions instead of to stdout.
    #[arg(long, requires = "issue_token")]
    token_file: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum TokenScope {
    ReadOnly,
    ReadWrite,
}

impl From<TokenScope> for TokenPermissions {
    fn from(scope: TokenScope) -> Self {
        match scope {
            TokenScope::ReadOnly => TokenPermissions::ReadOnly,
            TokenScope::ReadWrite => TokenPermissions::ReadWrite,
        }
    }
}

#[derive(Clone, Debug, ValueEnum)]
enum ApprovalMode {
    Deny,
    Tty,
}

struct TtyApprovalSink;

const MAX_TTY_ANSWER_BYTES: u64 = 64;

#[async_trait]
impl ApprovalSink for TtyApprovalSink {
    async fn request(&self, req: ApprovalRequest) -> ApprovalOutcome {
        let prompt = format!(
            "\n[tablepro-agentd] approval required\n  connection: {}\n  rule: {}\n  reason: {}\n  sql: {}\nApprove? [y/N] ",
            req.connection_name, req.rule, req.reason, req.sql
        );
        tokio::task::spawn_blocking(move || ask_on_controlling_terminal(&prompt))
            .await
            .unwrap_or(ApprovalOutcome::Deny)
    }
}

fn ask_on_controlling_terminal(prompt: &str) -> ApprovalOutcome {
    use std::io::{BufRead, BufReader, Read, Write};

    let terminal = match std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty") {
        Ok(terminal) => terminal,
        Err(error) => {
            tracing::warn!(%error, "no controlling terminal for approval; denying");
            return ApprovalOutcome::Deny;
        }
    };
    let mut writer = match terminal.try_clone() {
        Ok(writer) => writer,
        Err(error) => {
            tracing::warn!(%error, "controlling terminal is not writable; denying");
            return ApprovalOutcome::Deny;
        }
    };
    if writer.write_all(prompt.as_bytes()).is_err() || writer.flush().is_err() {
        return ApprovalOutcome::Deny;
    }

    let mut answer = String::new();
    let mut reader = BufReader::new(terminal).take(MAX_TTY_ANSWER_BYTES);
    match reader.read_line(&mut answer) {
        Ok(_) if answer.trim().eq_ignore_ascii_case("y") => ApprovalOutcome::AllowOnce,
        _ => ApprovalOutcome::Deny,
    }
}

fn write_token_file(path: &std::path::Path, plaintext: &str) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut handle = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    handle
        .write_all(plaintext.as_bytes())
        .map_err(|e| format!("{}: {e}", path.display()))?;
    handle.sync_all().map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(())
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
        tracing::error!(path = %args.policy.display(), "policy file not found");
        std::process::exit(2);
    }
    let policy = Arc::new(load_from_path(&args.policy).map_err(|e| e.to_string())?);

    let tokens = Arc::new(TokenStore::open_default().map_err(|e| e.to_string())?);
    if let Some(name) = args.issue_token {
        let saved = load_connections().await?;
        validate_token_connections(&args.connection, &saved)?;
        let expires_at = match args.expires_days {
            Some(days) => Some(
                chrono::Utc::now()
                    .checked_add_signed(chrono::TimeDelta::days(i64::from(days)))
                    .ok_or("--expires-days is too large")?,
            ),
            None => None,
        };
        let (_meta, plain) = tokens.issue(name, args.permissions.into(), args.connection, expires_at)?;
        match args.token_file {
            Some(path) => write_token_file(&path, &plain)?,
            None => println!("{plain}"),
        }
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
    fn an_issued_token_is_read_only_unless_asked_otherwise() {
        let id = Uuid::new_v4();
        let args = Args::parse_from([
            "tablepro-agentd",
            "--policy",
            "policy.toml",
            "--issue-token",
            "agent",
            "--connection",
            &id.to_string(),
        ]);
        assert!(matches!(args.permissions, TokenScope::ReadOnly));
        assert!(args.expires_days.is_none());
        assert!(args.token_file.is_none());

        let elevated = Args::parse_from([
            "tablepro-agentd",
            "--policy",
            "policy.toml",
            "--issue-token",
            "agent",
            "--connection",
            &id.to_string(),
            "--permissions",
            "read-write",
            "--expires-days",
            "7",
        ]);
        assert!(matches!(elevated.permissions, TokenScope::ReadWrite));
        assert_eq!(elevated.expires_days, Some(7));
    }

    #[test]
    fn a_token_file_is_written_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("token");
        write_token_file(&path, "secret").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "token file mode was {mode:o}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "secret");
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
