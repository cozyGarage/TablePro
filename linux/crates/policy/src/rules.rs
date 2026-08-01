use serde::{Deserialize, Serialize};
use tablepro_core::Environment;

use crate::classify::{StatementClass, StatementFacts};
use crate::config::EnvPolicy;
use crate::principal::Principal;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Decision {
    Allow {
        rule: String,
    },
    RequireApproval {
        rule: String,
        reason: String,
        preview: Option<String>,
    },
    Deny {
        rule: String,
        message: String,
    },
}

impl Decision {
    pub fn rule_name(&self) -> &str {
        match self {
            Self::Allow { rule } | Self::RequireApproval { rule, .. } | Self::Deny { rule, .. } => rule,
        }
    }

    pub fn is_allow(&self) -> bool {
        matches!(self, Self::Allow { .. })
    }
}

pub fn evaluate(
    principal: &Principal,
    environment: Environment,
    facts: &StatementFacts,
    connection_read_only: bool,
    env_policy: &EnvPolicy,
    estimated_rows: Option<u64>,
) -> Decision {
    if facts.class == StatementClass::Unparseable {
        return decide_unparseable(principal, env_policy);
    }

    if connection_read_only && facts.writes {
        return Decision::Deny {
            rule: "connection_read_only".into(),
            message: "connection is marked read-only; writes are not permitted".into(),
        };
    }

    if facts.is_multi_statement && principal.is_agent() && !env_policy.agent_allow_multi_statement {
        return Decision::Deny {
            rule: "agent_no_multi_statement".into(),
            message: "agents may not run multi-statement scripts".into(),
        };
    }

    if !facts.writes {
        return Decision::Allow {
            rule: "read_allow".into(),
        };
    }

    if principal.is_agent() {
        return evaluate_agent_write(environment, facts, env_policy, estimated_rows);
    }

    evaluate_human_write(environment, facts, env_policy, estimated_rows)
}

fn decide_unparseable(principal: &Principal, env_policy: &EnvPolicy) -> Decision {
    if principal.is_agent() {
        Decision::Deny {
            rule: "fail_closed_unparseable".into(),
            message: "SQL could not be parsed; agents are denied".into(),
        }
    } else if env_policy.human_approve_unparseable {
        Decision::RequireApproval {
            rule: "fail_closed_unparseable".into(),
            reason: "SQL could not be parsed; confirm before running".into(),
            preview: None,
        }
    } else {
        Decision::Allow {
            rule: "unparseable_human_allow".into(),
        }
    }
}

fn evaluate_agent_write(
    environment: Environment,
    facts: &StatementFacts,
    env_policy: &EnvPolicy,
    estimated_rows: Option<u64>,
) -> Decision {
    if env_policy.agent_writes == crate::config::WritePolicy::Deny {
        return Decision::Deny {
            rule: "agent_writes_denied".into(),
            message: format!("agent writes are denied in {}", environment.as_str()),
        };
    }

    if matches!(facts.class, StatementClass::Ddl) && !env_policy.agent_allow_ddl {
        return Decision::Deny {
            rule: "agent_ddl_denied".into(),
            message: "agents may not run DDL".into(),
        };
    }

    if !facts.has_where && matches!(facts.class, StatementClass::Update | StatementClass::Delete) {
        return Decision::Deny {
            rule: "agent_no_unscoped_dml".into(),
            message: "agents may not run UPDATE/DELETE without a WHERE clause".into(),
        };
    }

    if let Some(limit) = env_policy.blast_radius_max_rows
        && matches!(facts.class, StatementClass::Update | StatementClass::Delete)
    {
        match estimated_rows {
            Some(rows) if rows > limit => {
                return Decision::Deny {
                    rule: "blast_radius_exceeded".into(),
                    message: format!("would affect {rows} rows; limit is {limit}"),
                };
            }
            Some(_) => {}
            None => {
                return Decision::Deny {
                    rule: "blast_radius_unknown".into(),
                    message: "could not estimate affected rows; agents are denied when a blast-radius limit is set"
                        .into(),
                };
            }
        }
    }

    match env_policy.agent_writes {
        crate::config::WritePolicy::Allow => Decision::Allow {
            rule: "agent_write_allow".into(),
        },
        crate::config::WritePolicy::Approve => Decision::RequireApproval {
            rule: "agent_write_approve".into(),
            reason: format!("{:?} requires approval for agent writes", facts.class),
            preview: Some(format!("tables: {}", facts.tables.join(", "))),
        },
        crate::config::WritePolicy::Deny => unreachable!(),
    }
}

fn evaluate_human_write(
    environment: Environment,
    facts: &StatementFacts,
    env_policy: &EnvPolicy,
    estimated_rows: Option<u64>,
) -> Decision {
    if !facts.has_where && matches!(facts.class, StatementClass::Update | StatementClass::Delete) {
        return Decision::RequireApproval {
            rule: "human_unscoped_dml".into(),
            reason: "UPDATE/DELETE without WHERE".into(),
            preview: Some(format!("tables: {}", facts.tables.join(", "))),
        };
    }

    if matches!(facts.class, StatementClass::Ddl) && env_policy.human_approve_ddl {
        return Decision::RequireApproval {
            rule: "human_ddl_approve".into(),
            reason: format!("DDL on {}", environment.as_str()),
            preview: Some(format!("tables: {}", facts.tables.join(", "))),
        };
    }

    if let Some(limit) = env_policy.blast_radius_max_rows
        && matches!(facts.class, StatementClass::Update | StatementClass::Delete)
    {
        match estimated_rows {
            Some(rows) if rows > limit => {
                return Decision::RequireApproval {
                    rule: "blast_radius_approve".into(),
                    reason: format!("would affect {rows} rows (limit {limit})"),
                    preview: None,
                };
            }
            Some(_) => {}
            None => {
                return Decision::RequireApproval {
                    rule: "blast_radius_unknown".into(),
                    reason: "could not estimate affected rows; confirm before running".into(),
                    preview: None,
                };
            }
        }
    }

    if env_policy.human_approve_writes {
        return Decision::RequireApproval {
            rule: "human_write_approve".into(),
            reason: format!("writes require approval in {}", environment.as_str()),
            preview: Some(format!("tables: {}", facts.tables.join(", "))),
        };
    }

    Decision::Allow {
        rule: "human_write_allow".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::classify;
    use crate::config::{PolicyConfig, WritePolicy};

    fn env_policy(environment: Environment) -> EnvPolicy {
        PolicyConfig::default().for_environment(environment)
    }

    #[test]
    fn agent_denied_on_prod_write() {
        let facts = classify("DELETE FROM t WHERE id = 1", "postgres");
        let d = evaluate(
            &Principal::Agent {
                token: "tok".into(),
                client: Some("cursor".into()),
                model: None,
            },
            Environment::Prod,
            &facts,
            false,
            &env_policy(Environment::Prod),
            Some(1),
        );
        assert!(matches!(d, Decision::Deny { .. }), "{d:?}");
    }

    #[test]
    fn human_allowed_local_write() {
        let facts = classify("UPDATE t SET a = 1 WHERE id = 1", "postgres");
        let d = evaluate(
            &Principal::human_gui(),
            Environment::Local,
            &facts,
            false,
            &env_policy(Environment::Local),
            Some(1),
        );
        assert!(d.is_allow(), "{d:?}");
    }

    #[test]
    fn read_only_connection_blocks_write() {
        let facts = classify("DELETE FROM t WHERE id = 1", "postgres");
        let d = evaluate(
            &Principal::human_gui(),
            Environment::Local,
            &facts,
            true,
            &env_policy(Environment::Local),
            None,
        );
        assert!(matches!(d, Decision::Deny { ref rule, .. } if rule == "connection_read_only"));
    }

    #[test]
    fn agent_unparseable_denied() {
        let facts = classify("NOT SQL AT ALL !!!", "postgres");
        let d = evaluate(
            &Principal::Agent {
                token: "t".into(),
                client: None,
                model: None,
            },
            Environment::Dev,
            &facts,
            false,
            &env_policy(Environment::Dev),
            None,
        );
        assert!(matches!(d, Decision::Deny { .. }));
    }

    #[test]
    fn agent_denied_when_blast_radius_unknown() {
        let facts = classify("DELETE FROM t WHERE id = 1", "postgres");
        let d = evaluate(
            &Principal::Agent {
                token: "tok".into(),
                client: None,
                model: None,
            },
            Environment::Dev,
            &facts,
            false,
            &env_policy(Environment::Dev),
            None,
        );
        assert!(
            matches!(d, Decision::Deny { ref rule, .. } if rule == "blast_radius_unknown"),
            "{d:?}"
        );
    }

    #[test]
    fn human_requires_approval_when_blast_radius_unknown() {
        let facts = classify("DELETE FROM t WHERE id = 1", "postgres");
        let d = evaluate(
            &Principal::human_gui(),
            Environment::Staging,
            &facts,
            false,
            &env_policy(Environment::Staging),
            None,
        );
        assert!(
            matches!(d, Decision::RequireApproval { ref rule, .. } if rule == "blast_radius_unknown"),
            "{d:?}"
        );
    }

    #[test]
    fn connection_override_policy_is_used_directly() {
        let facts = classify("DELETE FROM t WHERE id = 1", "postgres");
        let mut override_policy = EnvPolicy::prod_defaults();
        override_policy.agent_writes = WritePolicy::Allow;
        let d = evaluate(
            &Principal::Agent {
                token: "tok".into(),
                client: None,
                model: None,
            },
            Environment::Prod,
            &facts,
            false,
            &override_policy,
            Some(1),
        );
        assert!(d.is_allow(), "{d:?}");
    }
}
