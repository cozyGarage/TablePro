mod audit_journal;
mod connections;
mod error;
pub mod query_history;
mod secrets;

pub use audit_journal::{AuditJournal, AuditJournalRecovery, LegacyJournalRotation, sample_event};
pub use connections::{
    SavedConnection, SavedSshAuth, SavedSshConfig, delete_connection, load_connections, save_connections,
    touch_last_opened,
};
pub use error::StorageError;
pub use secrets::{
    delete_mcp_token, delete_password, delete_ssh_passphrase, delete_ssh_password, load_mcp_token, load_password,
    load_ssh_passphrase, load_ssh_password, store_mcp_token, store_password, store_ssh_passphrase, store_ssh_password,
};
