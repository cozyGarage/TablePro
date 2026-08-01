use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tablepro_core::{
    ColumnInfo, Connection, DriverError, Environment, ExecResult, ForeignKeyInfo, IndexInfo, QueryResult, TableInfo,
    Transaction, Value,
};
use uuid::Uuid;

use crate::approval::{ApprovalOutcome, ApprovalRequest, ApprovalSink};
use crate::blast_radius::count_sql_for_mutation;
use crate::classify::{StatementClass, classify};
use crate::config::PolicyConfig;
use crate::mask::apply_masking;
use crate::principal::Principal;
use crate::rules::{Decision, evaluate};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    pub timestamp: DateTime<Utc>,
    pub principal: Principal,
    pub connection_id: Uuid,
    pub connection_name: String,
    pub environment: Environment,
    pub driver_id: String,
    pub sql: String,
    pub decision_rule: String,
    pub decision_kind: String,
    pub rows_affected: Option<u64>,
    pub duration_ms: u64,
    pub error: Option<String>,
}

#[async_trait]
pub trait AuditSink: Send + Sync {
    async fn record(&self, event: AuditEvent);
}

pub struct NullAuditSink;

#[async_trait]
impl AuditSink for NullAuditSink {
    async fn record(&self, _event: AuditEvent) {}
}

#[derive(Clone)]
pub struct GuardContext {
    pub connection_id: Uuid,
    pub connection_name: String,
    pub driver_id: String,
    pub environment: Environment,
    pub read_only: bool,
    pub principal: Principal,
    pub policy: Arc<PolicyConfig>,
    pub approval: Arc<dyn ApprovalSink>,
    pub audit: Arc<dyn AuditSink>,
}

/// Connection wrapper that classifies SQL, evaluates policy, optionally
/// asks for approval, masks agent results, and journals every decision.
pub struct PolicyGuard {
    inner: Arc<dyn Connection>,
    ctx: GuardContext,
}

impl PolicyGuard {
    pub fn new(inner: Arc<dyn Connection>, ctx: GuardContext) -> Self {
        Self { inner, ctx }
    }

    pub fn context(&self) -> &GuardContext {
        &self.ctx
    }

    pub fn inner(&self) -> Arc<dyn Connection> {
        self.inner.clone()
    }

    async fn authorize(&self, sql: &str, is_params_exec: bool) -> Result<Decision, DriverError> {
        let facts = classify(sql, &self.ctx.driver_id);
        let env_policy = self
            .ctx
            .policy
            .for_connection(&self.ctx.connection_id.to_string(), self.ctx.environment);

        let estimated_rows = if facts.writes
            && matches!(facts.class, StatementClass::Update | StatementClass::Delete)
            && env_policy.blast_radius_max_rows.is_some()
        {
            self.estimate_blast_radius(sql).await
        } else {
            None
        };

        let decision = evaluate(
            &self.ctx.principal,
            self.ctx.environment,
            &facts,
            self.ctx.read_only,
            &self.ctx.policy,
            estimated_rows,
        );

        // Connection-level override for EnvPolicy fields used above:
        // re-evaluate with connection override by temporarily swapping —
        // evaluate already uses PolicyConfig::for_environment; apply
        // connection override by building a one-shot config when present.
        let decision = if self
            .ctx
            .policy
            .connection_overrides
            .contains_key(&self.ctx.connection_id.to_string())
        {
            let mut cfg = (*self.ctx.policy).clone();
            if let Some(over) = self
                .ctx
                .policy
                .connection_overrides
                .get(&self.ctx.connection_id.to_string())
            {
                cfg.environments
                    .insert(self.ctx.environment.as_str().into(), over.clone());
            }
            evaluate(
                &self.ctx.principal,
                self.ctx.environment,
                &facts,
                self.ctx.read_only,
                &cfg,
                estimated_rows,
            )
        } else {
            decision
        };

        let _ = is_params_exec;

        match &decision {
            Decision::Allow { .. } => Ok(decision),
            Decision::Deny { message, .. } => {
                self.journal(sql, &decision, None, 0, Some(message.clone())).await;
                Err(DriverError::PolicyDenied(message.clone()))
            }
            Decision::RequireApproval { rule, reason, preview } => {
                let outcome = self
                    .ctx
                    .approval
                    .request(ApprovalRequest {
                        principal: self.ctx.principal.clone(),
                        environment: self.ctx.environment,
                        connection_name: self.ctx.connection_name.clone(),
                        sql: sql.to_string(),
                        facts: facts.clone(),
                        rule: rule.clone(),
                        reason: reason.clone(),
                        preview: preview.clone(),
                        estimated_rows,
                    })
                    .await;
                match outcome {
                    ApprovalOutcome::AllowOnce | ApprovalOutcome::AllowSession => Ok(Decision::Allow {
                        rule: format!("{rule}:approved"),
                    }),
                    ApprovalOutcome::Deny => {
                        let msg = format!("approval denied: {reason}");
                        self.journal(sql, &decision, None, 0, Some(msg.clone())).await;
                        Err(DriverError::PolicyDenied(msg))
                    }
                }
            }
        }
    }

    async fn estimate_blast_radius(&self, sql: &str) -> Option<u64> {
        let rewrite = count_sql_for_mutation(sql, &self.ctx.driver_id)?;
        let result = self.inner.query(&rewrite.count_sql).await.ok()?;
        let cell = result.rows.first()?.first()?;
        match cell {
            Value::Int(n) => Some(*n as u64),
            Value::Decimal(d) => d.to_string().parse().ok(),
            Value::Text(s) => s.parse().ok(),
            _ => None,
        }
    }

    async fn journal(
        &self,
        sql: &str,
        decision: &Decision,
        rows_affected: Option<u64>,
        duration_ms: u64,
        error: Option<String>,
    ) {
        let kind = match decision {
            Decision::Allow { .. } => "allow",
            Decision::RequireApproval { .. } => "require_approval",
            Decision::Deny { .. } => "deny",
        };
        self.ctx
            .audit
            .record(AuditEvent {
                timestamp: Utc::now(),
                principal: self.ctx.principal.clone(),
                connection_id: self.ctx.connection_id,
                connection_name: self.ctx.connection_name.clone(),
                environment: self.ctx.environment,
                driver_id: self.ctx.driver_id.clone(),
                sql: sql.to_string(),
                decision_rule: decision.rule_name().to_string(),
                decision_kind: kind.into(),
                rows_affected,
                duration_ms,
                error,
            })
            .await;
    }

    fn should_mask(&self) -> bool {
        if !self.ctx.principal.is_agent() {
            return false;
        }
        self.ctx
            .policy
            .for_connection(&self.ctx.connection_id.to_string(), self.ctx.environment)
            .mask_agent_results
    }

    fn mask_result(&self, result: QueryResult) -> QueryResult {
        if !self.should_mask() {
            return result;
        }
        let patterns: Vec<String> = self
            .ctx
            .policy
            .mask_patterns
            .iter()
            .map(|r| r.pattern.clone())
            .collect();
        apply_masking(result, &patterns)
    }
}

#[async_trait]
impl Connection for PolicyGuard {
    async fn list_tables(&self) -> Result<Vec<TableInfo>, DriverError> {
        self.inner.list_tables().await
    }

    async fn fetch_columns(&self, schema: Option<&str>, table: &str) -> Result<Vec<ColumnInfo>, DriverError> {
        self.inner.fetch_columns(schema, table).await
    }

    async fn fetch_rows(
        &self,
        schema: Option<&str>,
        table: &str,
        offset: u64,
        limit: u64,
    ) -> Result<QueryResult, DriverError> {
        let result = self.inner.fetch_rows(schema, table, offset, limit).await?;
        Ok(self.mask_result(result))
    }

    async fn query(&self, sql: &str) -> Result<QueryResult, DriverError> {
        let decision = self.authorize(sql, false).await?;
        let start = Instant::now();
        match self.inner.query(sql).await {
            Ok(result) => {
                let result = self.mask_result(result);
                self.journal(
                    sql,
                    &decision,
                    Some(result.rows.len() as u64),
                    start.elapsed().as_millis() as u64,
                    None,
                )
                .await;
                Ok(result)
            }
            Err(e) => {
                self.journal(
                    sql,
                    &decision,
                    None,
                    start.elapsed().as_millis() as u64,
                    Some(e.to_string()),
                )
                .await;
                Err(e)
            }
        }
    }

    async fn query_params(&self, sql: &str, params: &[Value]) -> Result<QueryResult, DriverError> {
        let decision = self.authorize(sql, true).await?;
        let start = Instant::now();
        match self.inner.query_params(sql, params).await {
            Ok(result) => {
                let result = self.mask_result(result);
                self.journal(
                    sql,
                    &decision,
                    Some(result.rows.len() as u64),
                    start.elapsed().as_millis() as u64,
                    None,
                )
                .await;
                Ok(result)
            }
            Err(e) => {
                self.journal(
                    sql,
                    &decision,
                    None,
                    start.elapsed().as_millis() as u64,
                    Some(e.to_string()),
                )
                .await;
                Err(e)
            }
        }
    }

    async fn execute(&self, sql: &str) -> Result<ExecResult, DriverError> {
        let decision = self.authorize(sql, false).await?;
        let start = Instant::now();
        match self.inner.execute(sql).await {
            Ok(result) => {
                self.journal(
                    sql,
                    &decision,
                    Some(result.rows_affected),
                    start.elapsed().as_millis() as u64,
                    None,
                )
                .await;
                Ok(result)
            }
            Err(e) => {
                self.journal(
                    sql,
                    &decision,
                    None,
                    start.elapsed().as_millis() as u64,
                    Some(e.to_string()),
                )
                .await;
                Err(e)
            }
        }
    }

    async fn execute_params(&self, sql: &str, params: &[Value]) -> Result<ExecResult, DriverError> {
        let decision = self.authorize(sql, true).await?;
        let start = Instant::now();
        match self.inner.execute_params(sql, params).await {
            Ok(result) => {
                self.journal(
                    sql,
                    &decision,
                    Some(result.rows_affected),
                    start.elapsed().as_millis() as u64,
                    None,
                )
                .await;
                Ok(result)
            }
            Err(e) => {
                self.journal(
                    sql,
                    &decision,
                    None,
                    start.elapsed().as_millis() as u64,
                    Some(e.to_string()),
                )
                .await;
                Err(e)
            }
        }
    }

    async fn execute_in_transaction(&self, statements: &[(String, Vec<Value>)]) -> Result<Vec<u64>, DriverError> {
        for (sql, _) in statements {
            self.authorize(sql, true).await?;
        }
        let start = Instant::now();
        let combined = statements
            .iter()
            .map(|(s, _)| s.as_str())
            .collect::<Vec<_>>()
            .join(";\n");
        let decision = Decision::Allow {
            rule: "transaction_batch".into(),
        };
        match self.inner.execute_in_transaction(statements).await {
            Ok(rows) => {
                let total: u64 = rows.iter().sum();
                self.journal(
                    &combined,
                    &decision,
                    Some(total),
                    start.elapsed().as_millis() as u64,
                    None,
                )
                .await;
                Ok(rows)
            }
            Err(e) => {
                self.journal(
                    &combined,
                    &decision,
                    None,
                    start.elapsed().as_millis() as u64,
                    Some(e.to_string()),
                )
                .await;
                Err(e)
            }
        }
    }

    async fn fetch_indexes(&self, schema: Option<&str>, table: &str) -> Result<Vec<IndexInfo>, DriverError> {
        self.inner.fetch_indexes(schema, table).await
    }

    async fn fetch_foreign_keys(&self, schema: Option<&str>, table: &str) -> Result<Vec<ForeignKeyInfo>, DriverError> {
        self.inner.fetch_foreign_keys(schema, table).await
    }

    async fn begin(&self) -> Result<Box<dyn Transaction>, DriverError> {
        // Opening a transaction is itself a control action; agents on
        // Prod are already denied writes, and begin alone is allowed.
        self.inner.begin().await
    }

    async fn server_version(&self) -> Result<Option<String>, DriverError> {
        self.inner.server_version().await
    }

    async fn ping(&self) -> Result<(), DriverError> {
        self.inner.ping().await
    }

    async fn close(self: Box<Self>) -> Result<(), DriverError> {
        // Arc-backed: closing via PolicyGuard is a no-op; lifecycle is
        // owned by DatabaseService.
        Ok(())
    }
}
