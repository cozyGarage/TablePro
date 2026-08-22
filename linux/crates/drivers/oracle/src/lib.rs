use async_trait::async_trait;

use tablepro_core::{ConnectOptions, Connection, DatabaseDriver, DriverError, DriverMaturity};

#[cfg(feature = "odpi")]
mod odpi;

pub struct OracleDriver;

#[async_trait]
impl DatabaseDriver for OracleDriver {
    fn id(&self) -> &'static str {
        "oracle"
    }

    fn display_name(&self) -> &'static str {
        "Oracle"
    }

    fn maturity(&self) -> DriverMaturity {
        DriverMaturity::Experimental
    }

    fn default_port(&self) -> u16 {
        1521
    }

    async fn connect(&self, opts: ConnectOptions) -> Result<Box<dyn Connection>, DriverError> {
        #[cfg(feature = "odpi")]
        {
            return odpi::connect(opts).await;
        }
        #[cfg(not(feature = "odpi"))]
        {
            let _ = opts;
            Err(DriverError::Unsupported(
                "Oracle driver requires the `odpi` feature and Oracle Instant Client on the build/host system".into(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::SecretString;

    #[test]
    fn driver_metadata() {
        let d = OracleDriver;
        assert_eq!(d.id(), "oracle");
        assert_eq!(d.display_name(), "Oracle");
        assert_eq!(d.default_port(), 1521);
        assert_eq!(d.maturity(), DriverMaturity::Experimental);
        assert!(!d.is_file_based());
    }

    #[test]
    fn structure_metadata_is_not_declared_without_a_fetch() {
        let d = OracleDriver;
        let sources = [include_str!("lib.rs"), include_str!("odpi.rs")];
        assert!(!d.supports_index_metadata());
        assert!(!d.supports_foreign_key_metadata());
        for source in sources {
            assert!(!source.contains(&["async fn ", "fetch_indexes("].concat()));
            assert!(!source.contains(&["async fn ", "fetch_foreign_keys("].concat()));
        }
    }

    #[tokio::test]
    async fn connect_without_odpi_feature_returns_unsupported() {
        let d = OracleDriver;
        let result = d
            .connect(ConnectOptions {
                host: "localhost".into(),
                port: 1521,
                database: "ORCL".into(),
                username: "scott".into(),
                password: SecretString::new("tiger".into()),
                ..Default::default()
            })
            .await;
        #[cfg(not(feature = "odpi"))]
        {
            assert!(
                matches!(result, Err(DriverError::Unsupported(_))),
                "expected Unsupported without odpi feature"
            );
        }
        #[cfg(feature = "odpi")]
        {
            let _ = result;
        }
    }
}
