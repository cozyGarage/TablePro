//! In-process MCP server for the GTK app. Agents talk to the same
//! policy-gated connections the UI uses.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use tablepro_core::Connection;
use tablepro_mcp::{ConnectionProvider, McpBridge, TokenPermissions, TokenStore};
use tablepro_policy::Principal;
use tablepro_storage::{SavedConnection, load_connections, store_mcp_token};
use uuid::Uuid;

use super::database_service;

static BRIDGE: OnceLock<Arc<McpBridge>> = OnceLock::new();

struct AppConnectionProvider;

#[async_trait]
impl ConnectionProvider for AppConnectionProvider {
    async fn list_saved_connections(&self) -> Result<Vec<SavedConnection>, String> {
        load_connections().await.map_err(|e| e.to_string())
    }

    async fn connection(&self, connection_id: Uuid, principal: Principal) -> Result<Arc<dyn Connection>, String> {
        let svc = database_service::instance();
        let conn = match &principal {
            Principal::Human { .. } => svc.get(connection_id),
            _ => svc.handle(connection_id, principal),
        };
        conn.ok_or_else(|| format!("connection {connection_id} is not open in the app"))
    }
}

/// Start the loopback MCP HTTP server.
pub fn start_background() -> Option<Arc<McpBridge>> {
    if !database_service::instance().audit_available() {
        tracing::error!("MCP server disabled because the required audit journal is unavailable");
        return None;
    }
    let tokens = match TokenStore::open_default() {
        Ok(t) => Arc::new(t),
        Err(e) => {
            tracing::warn!(error = %e, "MCP token store unavailable");
            return None;
        }
    };
    let bridge = Arc::new(McpBridge::new(Arc::new(AppConnectionProvider), tokens));
    let _ = BRIDGE.set(bridge.clone());
    let bridge_http = bridge.clone();
    let config = tablepro_mcp::McpServerConfig::default();
    tracing::info!(host = %config.bind_host, port = config.bind_port, "MCP HTTP server starting");
    std::thread::Builder::new()
        .name("tablepro-mcp".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::warn!(error = %e, "MCP runtime unavailable; MCP HTTP server not started");
                    return;
                }
            };
            rt.block_on(async move {
                if let Err(e) = tablepro_mcp::serve_streamable_http(bridge_http, config).await {
                    tracing::warn!(error = %e, "MCP HTTP server stopped");
                }
            });
        })
        .ok();
    Some(bridge)
}

pub fn bridge() -> Option<Arc<McpBridge>> {
    BRIDGE.get().cloned()
}

/// Issue a token, store plaintext in libsecret, return plaintext once.
pub async fn issue_token(
    name: String,
    permissions: TokenPermissions,
    connection_allowlist: Vec<Uuid>,
) -> Result<(Uuid, String), String> {
    let bridge = bridge().ok_or_else(|| "MCP server is not running".to_string())?;
    let (meta, plaintext) = bridge
        .tokens()
        .issue(name.clone(), permissions, connection_allowlist, None)?;
    if let Err(e) = store_mcp_token(meta.id, &plaintext, &format!("TablePro MCP: {name}")).await {
        tracing::warn!(error = %e, "failed to store MCP token in libsecret");
    }
    Ok((meta.id, plaintext))
}

pub fn revoke_token(id: Uuid) -> Result<(), String> {
    let bridge = bridge().ok_or_else(|| "MCP server is not running".to_string())?;
    bridge.tokens().revoke(id)?;
    relm4::spawn(async move {
        if let Err(e) = tablepro_storage::delete_mcp_token(id).await {
            tracing::warn!(error = %e, "failed to delete MCP token from libsecret");
        }
    });
    Ok(())
}

pub fn list_tokens() -> Vec<tablepro_mcp::McpToken> {
    bridge().map(|b| b.tokens().list()).unwrap_or_default()
}
