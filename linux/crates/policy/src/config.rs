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
    pub agent_writes: WritePolicy,
    pub agent_allow_ddl: bool,
    pub agent_allow_multi_statement: bool,
    pub human_approve_writes: bool,
    pub human_approve_ddl: bool,
    pub human_approve_unparseable: bool,
    pub human_allow_unaudited_writes: bool,
    pub blast_radius_max_rows: Option<u64>,
    pub mask_agent_results: bool,
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
            human_allow_unaudited_writes: false,
            blast_radius_max_rows: Some(10_000),
            mask_agent_results: true,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EnvPolicyOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_writes: Option<WritePolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_allow_ddl: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_allow_multi_statement: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human_approve_writes: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human_approve_ddl: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human_approve_unparseable: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human_allow_unaudited_writes: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blast_radius_max_rows: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask_agent_results: Option<bool>,
}

impl EnvPolicyOverride {
    fn apply_to(&self, mut policy: EnvPolicy) -> EnvPolicy {
        if let Some(value) = self.agent_writes {
            policy.agent_writes = value;
        }
        if let Some(value) = self.agent_allow_ddl {
            policy.agent_allow_ddl = value;
        }
        if let Some(value) = self.agent_allow_multi_statement {
            policy.agent_allow_multi_statement = value;
        }
        if let Some(value) = self.human_approve_writes {
            policy.human_approve_writes = value;
        }
        if let Some(value) = self.human_approve_ddl {
            policy.human_approve_ddl = value;
        }
        if let Some(value) = self.human_approve_unparseable {
            policy.human_approve_unparseable = value;
        }
        if let Some(value) = self.human_allow_unaudited_writes {
            policy.human_allow_unaudited_writes = value;
        }
        if let Some(value) = self.blast_radius_max_rows {
            policy.blast_radius_max_rows = Some(value);
        }
        if let Some(value) = self.mask_agent_results {
            policy.mask_agent_results = value;
        }
        policy
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
    pub environments: HashMap<String, EnvPolicyOverride>,
    #[serde(default)]
    pub connection_overrides: HashMap<String, EnvPolicyOverride>,
    #[serde(default = "default_mask_rules")]
    pub mask_patterns: Vec<MaskRule>,
}

fn default_mask_rules() -> Vec<MaskRule> {
    crate::mask::DEFAULT_SENSITIVE_PATTERNS
        .iter()
        .map(|pattern| MaskRule {
            pattern: (*pattern).into(),
        })
        .collect()
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            environments: HashMap::new(),
            connection_overrides: HashMap::new(),
            mask_patterns: default_mask_rules(),
        }
    }
}

impl PolicyConfig {
    pub fn for_environment(&self, env: Environment) -> EnvPolicy {
        let defaults = match env {
            Environment::Local | Environment::Dev => EnvPolicy::local_defaults(),
            Environment::Staging => EnvPolicy::staging_defaults(),
            Environment::Prod => EnvPolicy::prod_defaults(),
        };
        self.environments
            .get(env.as_str())
            .map_or(defaults.clone(), |overrides| overrides.apply_to(defaults))
    }

    pub fn for_connection(&self, connection_id: &str, env: Environment) -> EnvPolicy {
        let environment_policy = self.for_environment(env);
        self.connection_overrides
            .get(connection_id)
            .map_or(environment_policy.clone(), |overrides| {
                overrides.apply_to(environment_policy)
            })
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

    #[test]
    fn partial_prod_policy_inherits_secure_defaults() {
        let policy: PolicyConfig = toml::from_str(
            r#"
                [environments.prod]
                human_approve_ddl = false
            "#,
        )
        .unwrap();
        let prod = policy.for_environment(Environment::Prod);
        assert_eq!(prod.agent_writes, WritePolicy::Deny);
        assert!(prod.human_approve_writes);
        assert!(!prod.human_allow_unaudited_writes);
        assert!(!prod.human_approve_ddl);
        assert_eq!(prod.blast_radius_max_rows, Some(1_000));
        assert!(prod.mask_agent_results);
    }

    #[test]
    fn partial_connection_override_inherits_environment_policy() {
        let id = "9ab424f0-76ae-4fec-931f-2346549b6ca3";
        let policy: PolicyConfig = toml::from_str(&format!(
            r#"
                [connection_overrides."{id}"]
                human_approve_ddl = false
            "#
        ))
        .unwrap();
        let prod = policy.for_connection(id, Environment::Prod);
        assert_eq!(prod.agent_writes, WritePolicy::Deny);
        assert!(prod.human_approve_writes);
        assert!(!prod.human_approve_ddl);
    }

    #[test]
    fn local_unaudited_writes_require_explicit_opt_in() {
        let default_policy = PolicyConfig::default();
        assert!(
            !default_policy
                .for_environment(Environment::Local)
                .human_allow_unaudited_writes
        );

        let policy: PolicyConfig = toml::from_str(
            r#"
                [environments.local]
                human_allow_unaudited_writes = true
            "#,
        )
        .unwrap();
        assert!(policy.for_environment(Environment::Local).human_allow_unaudited_writes);
        assert!(!policy.for_environment(Environment::Prod).human_allow_unaudited_writes);
    }

    #[test]
    fn omitted_mask_patterns_keep_sensitive_defaults() {
        let policy: PolicyConfig = toml::from_str("").unwrap();
        assert!(!policy.mask_patterns.is_empty());
    }
}
