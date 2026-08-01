use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tablepro_core::Environment;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WritePolicy {
    Allow,
    #[default]
    Approve,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvPolicy {
    #[serde(default)]
    pub agent_writes: WritePolicy,
    #[serde(default)]
    pub agent_allow_ddl: bool,
    #[serde(default)]
    pub agent_allow_multi_statement: bool,
    #[serde(default)]
    pub human_approve_writes: bool,
    #[serde(default)]
    pub human_approve_ddl: bool,
    #[serde(default = "default_true")]
    pub human_approve_unparseable: bool,
    #[serde(default)]
    pub blast_radius_max_rows: Option<u64>,
    #[serde(default = "default_true")]
    pub mask_agent_results: bool,
}

fn default_true() -> bool {
    true
}

impl Default for EnvPolicy {
    fn default() -> Self {
        Self {
            agent_writes: WritePolicy::Approve,
            agent_allow_ddl: false,
            agent_allow_multi_statement: false,
            human_approve_writes: false,
            human_approve_ddl: false,
            human_approve_unparseable: true,
            blast_radius_max_rows: Some(10_000),
            mask_agent_results: true,
        }
    }
}

impl EnvPolicy {
    pub fn local_defaults() -> Self {
        Self {
            agent_writes: WritePolicy::Approve,
            human_approve_writes: false,
            human_approve_ddl: false,
            blast_radius_max_rows: Some(100_000),
            ..Self::default()
        }
    }

    pub fn prod_defaults() -> Self {
        Self {
            agent_writes: WritePolicy::Deny,
            agent_allow_ddl: false,
            human_approve_writes: true,
            human_approve_ddl: true,
            blast_radius_max_rows: Some(1_000),
            mask_agent_results: true,
            ..Self::default()
        }
    }

    pub fn staging_defaults() -> Self {
        Self {
            agent_writes: WritePolicy::Approve,
            human_approve_writes: false,
            human_approve_ddl: true,
            blast_radius_max_rows: Some(5_000),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaskRule {
    /// Glob matched against column names (case-insensitive).
    pub pattern: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyConfig {
    #[serde(default)]
    pub environments: HashMap<String, EnvPolicy>,
    #[serde(default)]
    pub connection_overrides: HashMap<String, EnvPolicy>,
    #[serde(default)]
    pub mask_patterns: Vec<MaskRule>,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        let mut environments = HashMap::new();
        environments.insert("local".into(), EnvPolicy::local_defaults());
        environments.insert("dev".into(), EnvPolicy::local_defaults());
        environments.insert("staging".into(), EnvPolicy::staging_defaults());
        environments.insert("prod".into(), EnvPolicy::prod_defaults());
        Self {
            environments,
            connection_overrides: HashMap::new(),
            mask_patterns: crate::mask::DEFAULT_SENSITIVE_PATTERNS
                .iter()
                .map(|p| MaskRule { pattern: (*p).into() })
                .collect(),
        }
    }
}

impl PolicyConfig {
    pub fn for_environment(&self, env: Environment) -> EnvPolicy {
        self.environments
            .get(env.as_str())
            .cloned()
            .unwrap_or_else(|| match env {
                Environment::Local | Environment::Dev => EnvPolicy::local_defaults(),
                Environment::Staging => EnvPolicy::staging_defaults(),
                Environment::Prod => EnvPolicy::prod_defaults(),
            })
    }

    pub fn for_connection(&self, connection_id: &str, env: Environment) -> EnvPolicy {
        self.connection_overrides
            .get(connection_id)
            .cloned()
            .unwrap_or_else(|| self.for_environment(env))
    }
}

pub fn policy_path() -> Result<PathBuf, String> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| {
                let mut p = PathBuf::from(h);
                p.push(".config");
                p
            })
        })
        .ok_or_else(|| "neither XDG_CONFIG_HOME nor HOME is set".to_string())?;
    Ok(base.join("tablepro").join("policy.toml"))
}

pub fn load_policy() -> PolicyConfig {
    match policy_path() {
        Ok(path) if path.exists() => load_from_path(&path).unwrap_or_default(),
        _ => PolicyConfig::default(),
    }
}

pub fn load_from_path(path: &Path) -> Result<PolicyConfig, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    toml::from_str(&text).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_prod_denies_agent_writes() {
        let p = PolicyConfig::default();
        let env = p.for_environment(Environment::Prod);
        assert_eq!(env.agent_writes, WritePolicy::Deny);
        assert!(env.human_approve_writes);
    }

    #[test]
    fn round_trip_toml() {
        let p = PolicyConfig::default();
        let text = toml::to_string_pretty(&p).unwrap();
        let back: PolicyConfig = toml::from_str(&text).unwrap();
        assert_eq!(back.for_environment(Environment::Prod).agent_writes, WritePolicy::Deny);
    }
}
