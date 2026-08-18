//! Fixture harness for the driver TLS tier. Each network driver is connected
//! to a real server holding a privately issued certificate, so its mapping of
//! the shared TLS modes is proven rather than read.

use std::path::PathBuf;

use secrecy::SecretString;
use tablepro_core::{ConnectOptions, TlsConfig, TlsMode};

pub const DRIVER_TLS_ENV: &str = "TABLEPRO_FIXTURE_DRIVER_TLS";

pub fn driver_tls_enabled() -> bool {
    std::env::var(DRIVER_TLS_ENV).map(|value| value == "1").unwrap_or(false)
}

fn env_or(key: &str, fallback: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| fallback.to_string())
}

fn env_port(key: &str, fallback: u16) -> u16 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(fallback)
}

pub struct DriverTlsFixture {
    pub host: String,
    pub mongo_port: u16,
    pub database: String,
    pub username: String,
    pub password: String,
    pub ca_cert: PathBuf,
    pub other_ca_cert: PathBuf,
}

impl DriverTlsFixture {
    pub fn from_env() -> Self {
        let materials = std::env::var("TABLEPRO_DRIVER_TLS_MATERIALS")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/driver-tls/materials")
            });
        Self {
            host: env_or("TABLEPRO_DRIVER_TLS_HOST", "localhost"),
            mongo_port: env_port("TABLEPRO_DRIVER_TLS_MONGO_PORT", 27018),
            database: env_or("TABLEPRO_DRIVER_TLS_DB", "tablepro"),
            username: env_or("TABLEPRO_DRIVER_TLS_USER", "tablepro"),
            password: env_or("TABLEPRO_DRIVER_TLS_PASSWORD", "tablepro"),
            ca_cert: materials.join("ca.crt"),
            other_ca_cert: materials.join("other-ca.crt"),
        }
    }

    pub fn options(&self, port: u16, mode: TlsMode, root_cert: Option<PathBuf>) -> ConnectOptions {
        ConnectOptions {
            host: self.host.clone(),
            port,
            database: self.database.clone(),
            username: self.username.clone(),
            password: SecretString::new(self.password.clone().into()),
            tls: TlsConfig {
                mode,
                root_cert,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    pub fn mongo(&self, mode: TlsMode, root_cert: Option<PathBuf>) -> ConnectOptions {
        self.options(self.mongo_port, mode, root_cert)
    }
}
