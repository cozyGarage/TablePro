mod audit_journal;
mod connection_organization;
mod connection_url;
mod connections;
mod error;
mod favorites;
mod file_access;
pub mod query_history;
mod secrets;

pub use audit_journal::{AuditJournal, AuditJournalRecovery, LegacyJournalRotation, sample_event};
pub use connection_organization::{
    ConnectionOrganization, ConnectionOrganizationIndex, MAX_LABEL_LEN, MAX_ORGANIZED_CONNECTIONS,
    MAX_TAGS_PER_CONNECTION, arrange_connections, connection_matches_filter, load_organization, save_organization,
};
pub use connection_url::{ParsedConnectionUrl, parse_connection_url};
pub use connections::{
    SavedConnection, SavedSshAuth, SavedSshConfig, delete_connection, load_connections, save_connections,
    touch_last_opened,
};
pub use error::StorageError;
pub use favorites::{
    SavedQuery, delete_favorite, load_favorites, matches_filter, rank_favorites, save_favorite, touch_favorite,
};
pub use secrets::{
    delete_mcp_token, delete_password, delete_ssh_passphrase, delete_ssh_password, load_mcp_token, load_password,
    load_ssh_passphrase, load_ssh_password, store_mcp_token, store_password, store_ssh_passphrase, store_ssh_password,
};
