use async_trait::async_trait;

use crate::connection::{ConnectOptions, Connection};
use crate::error::DriverError;

/// How complete a driver is for day-to-day use.
///
/// `Stable` drivers support browse, SQL (or the engine's query dialect),
/// and the common write paths. `Experimental` drivers connect and browse
/// but may lack parameter binding, interactive transactions, or full
/// write coverage. See `docs/driver-maturity.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DriverMaturity {
    #[default]
    Stable,
    Experimental,
}

impl DriverMaturity {
    pub fn label(self) -> &'static str {
        match self {
            Self::Stable => "Stable",
            Self::Experimental => "Experimental",
        }
    }
}

#[async_trait]
pub trait DatabaseDriver: Send + Sync {
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn default_port(&self) -> u16;

    fn maturity(&self) -> DriverMaturity {
        DriverMaturity::Stable
    }

    fn is_file_based(&self) -> bool {
        false
    }

    /// Whether a multi-statement DDL batch can roll back as a unit, so
    /// the structure editor's Save runs through
    /// `Connection::execute_in_transaction` instead of statement by
    /// statement. MySQL commits implicitly on every DDL statement: the
    /// transaction would end after the first one and a later failure
    /// would leave the earlier statements applied, which is worse than
    /// not opening one at all.
    fn ddl_is_transactional(&self) -> bool {
        false
    }

    /// Whether `ExecResult::rows_affected` carries a real count for
    /// UPDATE and DELETE. The inline-edit Save path reads a zero count
    /// as an optimistic-concurrency conflict, so a driver that cannot
    /// produce one must say so or every successful save reports a lost
    /// update. ClickHouse applies both as asynchronous mutations and
    /// returns no row count for either.
    fn reports_rows_affected(&self) -> bool {
        true
    }

    fn supports_integrated_auth(&self) -> bool {
        false
    }

    fn supports_local_socket(&self) -> bool {
        false
    }

    /// File name a forwarded Unix socket must use for this driver to
    /// dial it while verifying TLS against the original service
    /// hostname. `None` means the driver has no socket transport and an
    /// SSH tunnel must forward TCP instead.
    fn forwarded_socket_name(&self, _service_port: u16) -> Option<String> {
        None
    }

    async fn connect(&self, opts: ConnectOptions) -> Result<Box<dyn Connection>, DriverError>;
}
