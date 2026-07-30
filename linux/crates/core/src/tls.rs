use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// TLS verification mode for network drivers.
///
/// `Disabled` sends plaintext. `Prefer` / `Require` encrypt without
/// authenticating the server (legacy / TOFU transition). `VerifyCa`
/// checks the certificate chain. `VerifyFull` also checks the hostname.
/// New network connections default to `VerifyFull`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TlsMode {
    Disabled,
    Prefer,
    Require,
    VerifyCa,
    #[default]
    VerifyFull,
}

impl TlsMode {
    pub fn encrypts(self) -> bool {
        !matches!(self, Self::Disabled)
    }

    pub fn verifies_cert(self) -> bool {
        matches!(self, Self::VerifyCa | Self::VerifyFull)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TlsConfig {
    pub mode: TlsMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_cert: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_cert: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_key: Option<PathBuf>,
    /// Optional SHA-256 fingerprint (hex, lowercase) accepted when the
    /// presented certificate fails normal chain verification. Reuses the
    /// TOFU pattern from SSH known_hosts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_fingerprint: Option<String>,
}

impl TlsConfig {
    pub fn disabled() -> Self {
        Self {
            mode: TlsMode::Disabled,
            ..Default::default()
        }
    }

    pub fn from_legacy_bool(use_tls: bool) -> Self {
        if use_tls {
            Self {
                mode: TlsMode::VerifyFull,
                ..Default::default()
            }
        } else {
            Self::disabled()
        }
    }
}

/// Deployment environment for a saved connection. Drives policy defaults:
/// Prod agents are read-only; human writes on Prod require approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Environment {
    #[default]
    Local,
    Dev,
    Staging,
    Prod,
}

impl Environment {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Dev => "dev",
            Self::Staging => "staging",
            Self::Prod => "prod",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Local => "Local",
            Self::Dev => "Dev",
            Self::Staging => "Staging",
            Self::Prod => "Prod",
        }
    }
}
