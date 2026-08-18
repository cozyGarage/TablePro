use std::sync::Arc;

use tablepro_core::{ConnectOptions, Connection, DriverRegistry, TlsConfig, TlsMode};
use tablepro_ssh::{SshConfig, SshTunnel};
use tablepro_storage::SavedConnection;
use tablepro_transport::TransportError;

use super::database_service::{self, ConnectionMetadata, ReconnectParams};

pub async fn open_saved(
    registry: Arc<DriverRegistry>,
    saved: SavedConnection,
) -> Result<Vec<tablepro_core::TableInfo>, String> {
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
    database_service::instance().add(id, metadata, conn, tunnel, read_only, params);
    Ok(tables)
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

pub fn tls_config(mode: TlsMode) -> TlsConfig {
    tablepro_transport::tls_config(mode)
}
