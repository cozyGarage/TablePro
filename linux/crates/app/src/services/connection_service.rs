use std::sync::Arc;

use tablepro_core::{ConnectOptions, Connection, DriverRegistry};
use tablepro_ssh::{SshConfig, SshTunnel};
use tablepro_storage::SavedConnection;
use tablepro_transport::TransportError;

use super::database_service::{self, ConnectionMetadata, ReconnectParams};

/// A connection that has authenticated and completed its initial metadata
/// query, but has not replaced the active application connection yet.
/// Keeping preparation separate from activation makes failed switches leave
/// the existing workspace and connection untouched.
pub struct PreparedConnection {
    pub tables: Vec<tablepro_core::TableInfo>,
    pub driver_id: String,
    id: uuid::Uuid,
    metadata: ConnectionMetadata,
    connection: Box<dyn Connection>,
    tunnel: Option<SshTunnel>,
    read_only: bool,
    params: ReconnectParams,
}

impl std::fmt::Debug for PreparedConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedConnection")
            .field("id", &self.id)
            .field("driver_id", &self.driver_id)
            .field("table_count", &self.tables.len())
            .finish_non_exhaustive()
    }
}

impl PreparedConnection {
    pub(crate) fn new(
        tables: Vec<tablepro_core::TableInfo>,
        driver_id: String,
        metadata: ConnectionMetadata,
        connection: Box<dyn Connection>,
        tunnel: Option<SshTunnel>,
        params: ReconnectParams,
    ) -> Self {
        Self {
            tables,
            driver_id,
            id: metadata.id,
            read_only: metadata.read_only,
            metadata,
            connection,
            tunnel,
            params,
        }
    }

    pub fn activate(self) -> (Vec<tablepro_core::TableInfo>, String) {
        database_service::instance().activate_exclusive(
            self.id,
            self.metadata,
            self.connection,
            self.tunnel,
            self.read_only,
            self.params,
        );
        (self.tables, self.driver_id)
    }
}

pub async fn open_saved(registry: Arc<DriverRegistry>, saved: SavedConnection) -> Result<PreparedConnection, String> {
    let driver = registry
        .get(&saved.driver_id)
        .ok_or_else(|| format!("driver {} not registered", saved.driver_id))?;
    let id = saved.id;
    let environment = saved.environment;
    let read_only = saved.read_only;

    let ssh_hops = tablepro_transport::saved_ssh_chain(&saved).await.map_err(message)?;
    let opts = tablepro_transport::connect_options_for(&saved).await.map_err(message)?;

    let (conn, tunnel) = establish(&*driver, opts.clone(), ssh_hops.clone()).await?;
    let server_version = conn.server_version().await.ok().flatten();
    let tables = conn.list_tables().await.map_err(|e| format!("list_tables: {e}"))?;
    let metadata = ConnectionMetadata {
        id,
        name: saved.name.clone(),
        driver_id: saved.driver_id.clone(),
        environment,
        read_only,
        server_version,
    };
    let params = ReconnectParams {
        driver,
        opts,
        ssh: ssh_hops,
    };
    Ok(PreparedConnection::new(
        tables,
        saved.driver_id,
        metadata,
        conn,
        tunnel,
        params,
    ))
}

pub async fn establish(
    driver: &dyn tablepro_core::DatabaseDriver,
    opts: ConnectOptions,
    ssh: Option<Vec<SshConfig>>,
) -> Result<(Box<dyn Connection>, Option<SshTunnel>), String> {
    tablepro_transport::establish(driver, opts, ssh).await.map_err(message)
}

fn message(error: TransportError) -> String {
    match error {
        TransportError::Driver(error) => crate::ui::error_text::driver_message(&error),
        TransportError::IntegratedAuthUnsupported(driver_name) => {
            crate::tr!("The {driver} driver does not support Windows (Kerberos) authentication.")
                .replace("{driver}", &driver_name)
        }
        other => other.to_string(),
    }
}
