use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use chrono::Utc;
use sha2::{Digest, Sha256};
use tablepro_core::{
    ColumnInfo, Connection, DriverError, Environment, ExecResult, ForeignKeyInfo, IndexInfo, QueryResult, TableInfo,
    Transaction, Value,
};
use uuid::Uuid;

use crate::approval::{ApprovalOutcome, ApprovalRequest, ApprovalSink};
use crate::audit::{
    AuditApprovalOutcome, AuditError, AuditErrorCategory, AuditEvent, AuditOperationClass, AuditPreviewState,
    AuditRecordPhase, AuditSink, AuditState, AuditTerminalStatus, AuditTransactionOutcome,
};
use crate::blast_radius::count_sql_for_mutation;
use crate::classify::{StatementFacts, classify};
use crate::config::PolicyConfig;
use crate::mask::apply_masking;
use crate::principal::Principal;
use crate::rules::{Decision, evaluate_categorical, evaluate_eligible_write};

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
    pub audit_state: Arc<AuditState>,
}

#[derive(Clone)]
pub struct PolicyGuard {
    inner: Arc<dyn Connection>,
    ctx: GuardContext,
}

struct PolicyTransaction {
    guard: PolicyGuard,
    inner: Box<dyn Transaction>,
    batch_id: Uuid,
}

struct Authorization {
    decision: Decision,
    facts: StatementFacts,
    approval_outcome: AuditApprovalOutcome,
    preview_state: AuditPreviewState,
}

struct AuditOperation<'a> {
    operation_id: Uuid,
    batch_id: Option<Uuid>,
    sql: &'a str,
    class: AuditOperationClass,
    targets: Vec<String>,
    decision_rule: String,
    approval_outcome: AuditApprovalOutcome,
    preview_state: AuditPreviewState,
}

struct AuditOutcome {
    terminal_status: AuditTerminalStatus,
    transaction_outcome: AuditTransactionOutcome,
    rows_affected: Option<u64>,
    error_category: Option<AuditErrorCategory>,
    duration_ms: u64,
}

impl PolicyGuard {
    pub fn new(inner: Arc<dyn Connection>, ctx: GuardContext) -> Self {
        Self { inner, ctx }
    }

    pub fn context(&self) -> &GuardContext {
        &self.ctx
    }

    async fn authorize(&self, sql: &str, _is_params_exec: bool) -> Result<Authorization, DriverError> {
        let facts = classify(sql, &self.ctx.driver_id);
        let env_policy = self
            .ctx
            .policy
            .for_connection(&self.ctx.connection_id.to_string(), self.ctx.environment);
        if let Some(decision) = evaluate_categorical(
            &self.ctx.principal,
            self.ctx.environment,
            &facts,
            self.ctx.read_only,
            &env_policy,
        ) {
            return self.resolve_authorization(sql, facts, decision, None).await;
        }

        self.require_governed_write_available()?;
        let estimated_rows = if facts.contains_mutating_dml && env_policy.blast_radius_max_rows.is_some() {
            self.estimate_blast_radius(sql, &facts).await?
        } else {
            None
        };
        let decision = evaluate_eligible_write(
            &self.ctx.principal,
            self.ctx.environment,
            &facts,
            &env_policy,
            estimated_rows,
        );
        self.resolve_authorization(sql, facts, decision, estimated_rows).await
    }

    async fn resolve_authorization(
        &self,
        sql: &str,
        facts: StatementFacts,
        decision: Decision,
        estimated_rows: Option<u64>,
    ) -> Result<Authorization, DriverError> {
        match decision {
            Decision::Allow { .. } => Ok(Authorization {
                decision,
                facts,
                approval_outcome: AuditApprovalOutcome::NotRequired,
                preview_state: AuditPreviewState::NotRequested,
            }),
            Decision::Deny { ref message, .. } => {
                let operation = self.operation(
                    sql,
                    None,
                    &facts,
                    &decision,
                    AuditApprovalOutcome::NotRequired,
                    AuditPreviewState::NotRequested,
                );
                let audit_result = self
                    .record_outcome(
                        &operation,
                        AuditOutcome {
                            terminal_status: AuditTerminalStatus::Denied,
                            transaction_outcome: AuditTransactionOutcome::NotApplicable,
                            rows_affected: None,
                            error_category: Some(AuditErrorCategory::Policy),
                            duration_ms: 0,
                        },
                    )
                    .await;
                self.handle_non_execution_audit_failure(audit_result)?;
                Err(DriverError::PolicyDenied(message.clone()))
            }
            Decision::RequireApproval {
                ref rule,
                ref reason,
                ref preview,
            } => {
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
                let preview_state = match preview {
                    Some(value) => AuditPreviewState::Available(value.clone()),
                    None => AuditPreviewState::Unavailable,
                };
                match outcome {
                    ApprovalOutcome::AllowOnce => Ok(Authorization {
                        decision: Decision::Allow {
                            rule: format!("{rule}:approved"),
                        },
                        facts,
                        approval_outcome: AuditApprovalOutcome::Approved,
                        preview_state,
                    }),
                    ApprovalOutcome::Deny => {
                        let message = format!("approval denied: {reason}");
                        let operation = self.operation(
                            sql,
                            None,
                            &facts,
                            &decision,
                            AuditApprovalOutcome::Denied,
                            preview_state,
                        );
                        let audit_result = self
                            .record_outcome(
                                &operation,
                                AuditOutcome {
                                    terminal_status: AuditTerminalStatus::Denied,
                                    transaction_outcome: AuditTransactionOutcome::NotApplicable,
                                    rows_affected: None,
                                    error_category: Some(AuditErrorCategory::Policy),
                                    duration_ms: 0,
                                },
                            )
                            .await;
                        self.handle_non_execution_audit_failure(audit_result)?;
                        Err(DriverError::PolicyDenied(message))
                    }
                }
            }
        }
    }

    async fn estimate_blast_radius(&self, sql: &str, facts: &StatementFacts) -> Result<Option<u64>, DriverError> {
        let Some(rewrite) = count_sql_for_mutation(sql, &self.ctx.driver_id) else {
            return Ok(None);
        };
        let operation = self.metadata_operation("BLAST RADIUS ESTIMATE", facts.tables.clone());
        self.record_intent(&operation).await.map_err(|error| {
            DriverError::PolicyDenied(format!(
                "operation denied because audit intent could not be persisted: {error}"
            ))
        })?;
        let start = Instant::now();
        let result = self.inner.query(&rewrite.count_sql).await;
        let rows = result.as_ref().ok().map(|value| value.rows.len() as u64);
        let audit_result = match &result {
            Ok(_) => {
                self.record_outcome(
                    &operation,
                    AuditOutcome {
                        terminal_status: AuditTerminalStatus::Succeeded,
                        transaction_outcome: AuditTransactionOutcome::NotApplicable,
                        rows_affected: rows,
                        error_category: None,
                        duration_ms: start.elapsed().as_millis() as u64,
                    },
                )
                .await
            }
            Err(error) => {
                self.record_outcome(
                    &operation,
                    AuditOutcome {
                        terminal_status: AuditTerminalStatus::Failed,
                        transaction_outcome: AuditTransactionOutcome::NotApplicable,
                        rows_affected: None,
                        error_category: Some(error_category(error)),
                        duration_ms: start.elapsed().as_millis() as u64,
                    },
                )
                .await
            }
        };
        audit_result.map_err(|error| {
            DriverError::PolicyDenied(format!(
                "operation denied because blast-radius audit outcome could not be persisted: {error}"
            ))
        })?;

        let Ok(result) = result else {
            return Ok(None);
        };
        let Some(cell) = result.rows.first().and_then(|row| row.first()) else {
            return Ok(None);
        };
        Ok(match cell {
            Value::Int(value) => u64::try_from(*value).ok(),
            Value::Decimal(value) => value.to_string().parse().ok(),
            Value::Text(value) => value.parse().ok(),
            _ => None,
        })
    }

    fn operation<'a>(
        &self,
        sql: &'a str,
        batch_id: Option<Uuid>,
        facts: &StatementFacts,
        decision: &Decision,
        approval_outcome: AuditApprovalOutcome,
        preview_state: AuditPreviewState,
    ) -> AuditOperation<'a> {
        AuditOperation {
            operation_id: Uuid::new_v4(),
            batch_id,
            sql,
            class: AuditOperationClass::from_statement(facts.class, facts.writes),
            targets: facts.tables.clone(),
            decision_rule: decision.rule_name().to_string(),
            approval_outcome,
            preview_state,
        }
    }

    fn metadata_operation<'a>(&self, operation: &'a str, targets: Vec<String>) -> AuditOperation<'a> {
        AuditOperation {
            operation_id: Uuid::new_v4(),
            batch_id: None,
            sql: operation,
            class: AuditOperationClass::Read,
            targets,
            decision_rule: "metadata_read".into(),
            approval_outcome: AuditApprovalOutcome::NotRequired,
            preview_state: AuditPreviewState::NotRequested,
        }
    }

    fn commit_operation(batch_id: Uuid) -> AuditOperation<'static> {
        AuditOperation {
            operation_id: Uuid::new_v4(),
            batch_id: Some(batch_id),
            sql: "COMMIT",
            class: AuditOperationClass::TransactionCommit,
            targets: Vec::new(),
            decision_rule: "transaction_commit".into(),
            approval_outcome: AuditApprovalOutcome::NotRequired,
            preview_state: AuditPreviewState::NotRequested,
        }
    }

    async fn record_intent(&self, operation: &AuditOperation<'_>) -> Result<(), AuditError> {
        self.record(
            operation,
            AuditRecordPhase::Intent,
            AuditTerminalStatus::Pending,
            if matches!(
                operation.class,
                AuditOperationClass::TransactionCommit | AuditOperationClass::TransactionRollback
            ) {
                AuditTransactionOutcome::Pending
            } else {
                AuditTransactionOutcome::NotApplicable
            },
            None,
            None,
            None,
        )
        .await
    }

    async fn record_outcome(&self, operation: &AuditOperation<'_>, outcome: AuditOutcome) -> Result<(), AuditError> {
        self.record(
            operation,
            AuditRecordPhase::Outcome,
            outcome.terminal_status,
            outcome.transaction_outcome,
            outcome.rows_affected,
            outcome.error_category,
            Some(outcome.duration_ms),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn record(
        &self,
        operation: &AuditOperation<'_>,
        phase: AuditRecordPhase,
        terminal_status: AuditTerminalStatus,
        transaction_outcome: AuditTransactionOutcome,
        rows_affected: Option<u64>,
        error_category: Option<AuditErrorCategory>,
        duration_ms: Option<u64>,
    ) -> Result<(), AuditError> {
        let sql_hash = hex::encode(Sha256::digest(operation.sql.as_bytes()));
        let principal = sanitized_principal(&self.ctx.principal);
        let error = error_category.map(sanitized_error_detail);
        self.ctx
            .audit
            .record(AuditEvent {
                timestamp: Utc::now(),
                operation_id: operation.operation_id,
                batch_id: operation.batch_id,
                phase,
                principal,
                connection_id: self.ctx.connection_id,
                connection_name: self.ctx.connection_name.clone(),
                environment: self.ctx.environment,
                driver_id: self.ctx.driver_id.clone(),
                operation_class: operation.class,
                redacted_sql: "[REDACTED]".into(),
                sql_hash,
                targets: operation.targets.clone(),
                decision_rule: operation.decision_rule.clone(),
                approval_outcome: operation.approval_outcome,
                preview_state: operation.preview_state.clone(),
                terminal_status,
                transaction_outcome,
                error_category,
                error,
                rows_affected,
                duration_ms,
            })
            .await
    }

    fn require_governed_write_available(&self) -> Result<(), DriverError> {
        if self.ctx.audit_state.governed_writes_disabled() {
            return Err(DriverError::PolicyDenied(
                "governed writes are disabled because a prior audit outcome could not be persisted".into(),
            ));
        }
        Ok(())
    }

    async fn prepare_governed_read(&self, operation: &AuditOperation<'_>) -> Result<(), DriverError> {
        if !self.ctx.principal.is_agent() && matches!(self.ctx.environment, Environment::Local | Environment::Dev) {
            return Ok(());
        }
        self.record_intent(operation).await.map_err(|error| {
            DriverError::PolicyDenied(format!(
                "operation denied because audit intent could not be persisted: {error}"
            ))
        })
    }

    fn allows_unaudited_local_write(&self) -> bool {
        !self.ctx.principal.is_agent()
            && matches!(self.ctx.environment, Environment::Local | Environment::Dev)
            && self
                .ctx
                .policy
                .for_connection(&self.ctx.connection_id.to_string(), self.ctx.environment)
                .human_allow_unaudited_writes
    }

    fn intent_is_required(&self) -> bool {
        !self.allows_unaudited_local_write()
    }

    fn handle_intent_failure(&self, result: Result<(), AuditError>) -> Result<(), DriverError> {
        let Err(error) = result else {
            return Ok(());
        };
        if self.intent_is_required() {
            return Err(DriverError::PolicyDenied(format!(
                "operation denied because audit intent could not be persisted: {error}"
            )));
        }
        tracing::warn!(error = %error, "audit intent could not be persisted; local human write is continuing");
        Ok(())
    }

    fn handle_non_execution_audit_failure(&self, result: Result<(), AuditError>) -> Result<(), DriverError> {
        let Err(error) = result else {
            return Ok(());
        };
        if self.ctx.principal.is_agent() {
            return Err(DriverError::PolicyDenied(format!(
                "operation denied because audit recording failed: {error}"
            )));
        }
        tracing::warn!(error = %error, "audit outcome could not be persisted");
        Ok(())
    }

    fn handle_read_audit_failure(&self, result: Result<(), AuditError>) -> Result<(), DriverError> {
        let Err(error) = result else {
            return Ok(());
        };
        if self.ctx.principal.is_agent() {
            return Err(DriverError::Internal(format!(
                "audit recording failed after read execution: {error}"
            )));
        }
        tracing::warn!(error = %error, "audit outcome could not be persisted after read execution");
        Ok(())
    }

    fn handle_write_outcome_failure(&self, result: Result<(), AuditError>) -> Result<(), DriverError> {
        let Err(error) = result else {
            return Ok(());
        };
        self.ctx.audit_state.disable_governed_writes();
        Err(DriverError::Internal(format!(
            "audit outcome could not be persisted; the operation may have succeeded and further governed writes are disabled: {error}"
        )))
    }

    fn should_mask(&self) -> bool {
        self.ctx.principal.is_agent()
            && self
                .ctx
                .policy
                .for_connection(&self.ctx.connection_id.to_string(), self.ctx.environment)
                .mask_agent_results
    }

    fn mask_result(&self, result: QueryResult) -> QueryResult {
        if !self.should_mask() {
            return result;
        }
        let patterns = self
            .ctx
            .policy
            .mask_patterns
            .iter()
            .map(|rule| rule.pattern.clone())
            .collect::<Vec<_>>();
        apply_masking(result, &patterns)
    }

    async fn audit_read_result<T>(
        &self,
        operation: &AuditOperation<'_>,
        start: Instant,
        result: &Result<T, DriverError>,
        rows: Option<u64>,
    ) -> Result<(), DriverError> {
        let audit_result = match result {
            Ok(_) => {
                self.record_outcome(
                    operation,
                    AuditOutcome {
                        terminal_status: AuditTerminalStatus::Succeeded,
                        transaction_outcome: AuditTransactionOutcome::NotApplicable,
                        rows_affected: rows,
                        error_category: None,
                        duration_ms: start.elapsed().as_millis() as u64,
                    },
                )
                .await
            }
            Err(error) => {
                self.record_outcome(
                    operation,
                    AuditOutcome {
                        terminal_status: AuditTerminalStatus::Failed,
                        transaction_outcome: AuditTransactionOutcome::NotApplicable,
                        rows_affected: None,
                        error_category: Some(error_category(error)),
                        duration_ms: start.elapsed().as_millis() as u64,
                    },
                )
                .await
            }
        };
        self.handle_read_audit_failure(audit_result)
    }

    async fn audit_transaction_result<T>(
        &self,
        operation: &AuditOperation<'_>,
        start: Instant,
        result: &Result<T, DriverError>,
        rows: Option<u64>,
    ) -> Result<(), DriverError> {
        let audit_result = match result {
            Ok(_) => {
                self.record_outcome(
                    operation,
                    AuditOutcome {
                        terminal_status: AuditTerminalStatus::Succeeded,
                        transaction_outcome: AuditTransactionOutcome::NotApplicable,
                        rows_affected: rows,
                        error_category: None,
                        duration_ms: start.elapsed().as_millis() as u64,
                    },
                )
                .await
            }
            Err(error) => {
                self.ctx.audit_state.disable_governed_writes();
                self.record_outcome(
                    operation,
                    AuditOutcome {
                        terminal_status: AuditTerminalStatus::Unknown,
                        transaction_outcome: AuditTransactionOutcome::Unknown,
                        rows_affected: None,
                        error_category: Some(error_category(error)),
                        duration_ms: start.elapsed().as_millis() as u64,
                    },
                )
                .await
            }
        };
        if result.is_err() || operation.class.is_write() {
            return self.handle_write_outcome_failure(audit_result);
        }
        self.handle_read_audit_failure(audit_result)
    }

    async fn audit_write_result<T>(
        &self,
        operation: &AuditOperation<'_>,
        start: Instant,
        result: &Result<T, DriverError>,
        rows: Option<u64>,
    ) -> Result<(), DriverError> {
        let audit_result = match result {
            Ok(_) => {
                self.record_outcome(
                    operation,
                    AuditOutcome {
                        terminal_status: AuditTerminalStatus::Succeeded,
                        transaction_outcome: AuditTransactionOutcome::NotApplicable,
                        rows_affected: rows,
                        error_category: None,
                        duration_ms: start.elapsed().as_millis() as u64,
                    },
                )
                .await
            }
            Err(error) => {
                let ambiguous = is_ambiguous_post_dispatch(error);
                if ambiguous {
                    self.ctx.audit_state.disable_governed_writes();
                }
                self.record_outcome(
                    operation,
                    AuditOutcome {
                        terminal_status: if ambiguous {
                            AuditTerminalStatus::Unknown
                        } else {
                            AuditTerminalStatus::Failed
                        },
                        transaction_outcome: AuditTransactionOutcome::NotApplicable,
                        rows_affected: None,
                        error_category: Some(error_category(error)),
                        duration_ms: start.elapsed().as_millis() as u64,
                    },
                )
                .await
            }
        };
        self.handle_write_outcome_failure(audit_result)
    }
}

fn sanitized_principal(principal: &Principal) -> Principal {
    match principal {
        Principal::Human { session } => Principal::Human {
            session: session.clone(),
        },
        Principal::Agent { token, client, model } => Principal::Agent {
            token: format!("sha256:{}", &hex::encode(Sha256::digest(token.as_bytes()))[..16]),
            client: client.clone(),
            model: model.clone(),
        },
    }
}

fn sanitized_error_detail(category: AuditErrorCategory) -> String {
    match category {
        AuditErrorCategory::Policy => "policy_denied",
        AuditErrorCategory::Audit => "audit_error",
        AuditErrorCategory::Connection => "connection_error",
        AuditErrorCategory::Authentication => "authentication_error",
        AuditErrorCategory::Tls => "tls_error",
        AuditErrorCategory::Query => "query_error",
        AuditErrorCategory::ReadOnly => "read_only",
        AuditErrorCategory::Unsupported => "unsupported",
        AuditErrorCategory::Internal => "internal_error",
        AuditErrorCategory::Transaction => "transaction_error",
        AuditErrorCategory::Cancelled => "cancelled",
        AuditErrorCategory::Timeout => "timeout",
        AuditErrorCategory::Unknown => "unknown_error",
    }
    .into()
}

fn is_ambiguous_post_dispatch(error: &DriverError) -> bool {
    match error {
        DriverError::ConnectionRefused
        | DriverError::Disconnected
        | DriverError::AuthFailed
        | DriverError::IntegratedAuth(_)
        | DriverError::Tls(_)
        | DriverError::Internal(_) => true,
        DriverError::Transaction { source, .. } => is_ambiguous_post_dispatch(source),
        DriverError::Query { .. }
        | DriverError::ReadOnly
        | DriverError::PolicyDenied(_)
        | DriverError::Unsupported(_) => false,
    }
}

fn error_category(error: &DriverError) -> AuditErrorCategory {
    match error {
        DriverError::ConnectionRefused | DriverError::Disconnected => AuditErrorCategory::Connection,
        DriverError::AuthFailed | DriverError::IntegratedAuth(_) => AuditErrorCategory::Authentication,
        DriverError::Tls(_) => AuditErrorCategory::Tls,
        DriverError::Query { .. } => AuditErrorCategory::Query,
        DriverError::ReadOnly => AuditErrorCategory::ReadOnly,
        DriverError::PolicyDenied(_) => AuditErrorCategory::Policy,
        DriverError::Unsupported(_) => AuditErrorCategory::Unsupported,
        DriverError::Internal(_) => AuditErrorCategory::Internal,
        DriverError::Transaction { .. } => AuditErrorCategory::Transaction,
    }
}

#[async_trait]
impl Connection for PolicyGuard {
    async fn list_tables(&self) -> Result<Vec<TableInfo>, DriverError> {
        let operation = self.metadata_operation("LIST TABLES", Vec::new());
        self.prepare_governed_read(&operation).await?;
        let start = Instant::now();
        let result = self.inner.list_tables().await;
        let rows = result.as_ref().ok().map(|tables| tables.len() as u64);
        self.audit_read_result(&operation, start, &result, rows).await?;
        result
    }

    async fn fetch_columns(&self, schema: Option<&str>, table: &str) -> Result<Vec<ColumnInfo>, DriverError> {
        let target = schema.map_or_else(|| table.to_string(), |schema| format!("{schema}.{table}"));
        let operation = self.metadata_operation("FETCH COLUMNS", vec![target]);
        self.prepare_governed_read(&operation).await?;
        let start = Instant::now();
        let result = self.inner.fetch_columns(schema, table).await;
        let rows = result.as_ref().ok().map(|columns| columns.len() as u64);
        self.audit_read_result(&operation, start, &result, rows).await?;
        result
    }

    async fn fetch_rows(
        &self,
        schema: Option<&str>,
        table: &str,
        offset: u64,
        limit: u64,
    ) -> Result<QueryResult, DriverError> {
        let target = schema.map_or_else(|| table.to_string(), |schema| format!("{schema}.{table}"));
        let operation = self.metadata_operation("FETCH ROWS", vec![target]);
        self.prepare_governed_read(&operation).await?;
        let start = Instant::now();
        let result = self
            .inner
            .fetch_rows(schema, table, offset, limit)
            .await
            .map(|rows| self.mask_result(rows));
        let row_count = result.as_ref().ok().map(|rows| rows.rows.len() as u64);
        self.audit_read_result(&operation, start, &result, row_count).await?;
        result
    }

    async fn query(&self, sql: &str) -> Result<QueryResult, DriverError> {
        let authorization = self.authorize(sql, false).await?;
        if authorization.facts.writes {
            return self
                .execute_query_write(sql, None, authorization, |inner| inner.query(sql))
                .await;
        }
        let operation = self.operation(
            sql,
            None,
            &authorization.facts,
            &authorization.decision,
            authorization.approval_outcome,
            authorization.preview_state,
        );
        self.prepare_governed_read(&operation).await?;
        let start = Instant::now();
        let result = self.inner.query(sql).await.map(|value| self.mask_result(value));
        let rows = result.as_ref().ok().map(|value| value.rows.len() as u64);
        self.audit_read_result(&operation, start, &result, rows).await?;
        result
    }

    async fn query_params(&self, sql: &str, params: &[Value]) -> Result<QueryResult, DriverError> {
        let authorization = self.authorize(sql, true).await?;
        if authorization.facts.writes {
            return self
                .execute_query_write(sql, None, authorization, |inner| inner.query_params(sql, params))
                .await;
        }
        let operation = self.operation(
            sql,
            None,
            &authorization.facts,
            &authorization.decision,
            authorization.approval_outcome,
            authorization.preview_state,
        );
        self.prepare_governed_read(&operation).await?;
        let start = Instant::now();
        let result = self
            .inner
            .query_params(sql, params)
            .await
            .map(|value| self.mask_result(value));
        let rows = result.as_ref().ok().map(|value| value.rows.len() as u64);
        self.audit_read_result(&operation, start, &result, rows).await?;
        result
    }

    async fn execute(&self, sql: &str) -> Result<ExecResult, DriverError> {
        let authorization = self.authorize(sql, false).await?;
        self.execute_write(sql, None, authorization, |inner| inner.execute(sql))
            .await
    }

    async fn execute_params(&self, sql: &str, params: &[Value]) -> Result<ExecResult, DriverError> {
        let authorization = self.authorize(sql, true).await?;
        self.execute_write(sql, None, authorization, |inner| inner.execute_params(sql, params))
            .await
    }

    async fn execute_in_transaction(&self, statements: &[(String, Vec<Value>)]) -> Result<Vec<u64>, DriverError> {
        let combined = statements
            .iter()
            .map(|(sql, _)| sql.trim().trim_end_matches(';'))
            .collect::<Vec<_>>()
            .join(";\n");
        let authorization = self.authorize(&combined, true).await?;
        self.require_governed_write_available()?;
        let batch_id = Uuid::new_v4();
        let operation = self.operation(
            &combined,
            Some(batch_id),
            &authorization.facts,
            &authorization.decision,
            authorization.approval_outcome,
            authorization.preview_state,
        );
        self.handle_intent_failure(self.record_intent(&operation).await)?;
        let mut pending_write = self.ctx.audit_state.pending_write();
        let start = Instant::now();
        let result = self.inner.execute_in_transaction(statements).await;
        let rows = result.as_ref().ok().map(|values| values.iter().sum());
        self.audit_transaction_result(&operation, start, &result, rows).await?;
        pending_write.disarm();
        result
    }

    async fn fetch_indexes(&self, schema: Option<&str>, table: &str) -> Result<Vec<IndexInfo>, DriverError> {
        let target = schema.map_or_else(|| table.to_string(), |schema| format!("{schema}.{table}"));
        let operation = self.metadata_operation("FETCH INDEXES", vec![target]);
        self.prepare_governed_read(&operation).await?;
        let start = Instant::now();
        let result = self.inner.fetch_indexes(schema, table).await;
        let rows = result.as_ref().ok().map(|indexes| indexes.len() as u64);
        self.audit_read_result(&operation, start, &result, rows).await?;
        result
    }

    async fn fetch_foreign_keys(&self, schema: Option<&str>, table: &str) -> Result<Vec<ForeignKeyInfo>, DriverError> {
        let target = schema.map_or_else(|| table.to_string(), |schema| format!("{schema}.{table}"));
        let operation = self.metadata_operation("FETCH FOREIGN KEYS", vec![target]);
        self.prepare_governed_read(&operation).await?;
        let start = Instant::now();
        let result = self.inner.fetch_foreign_keys(schema, table).await;
        let rows = result.as_ref().ok().map(|keys| keys.len() as u64);
        self.audit_read_result(&operation, start, &result, rows).await?;
        result
    }

    async fn begin(&self) -> Result<Box<dyn Transaction>, DriverError> {
        let inner = self.inner.begin().await?;
        Ok(Box::new(PolicyTransaction {
            guard: self.clone(),
            inner,
            batch_id: Uuid::new_v4(),
        }))
    }

    async fn server_version(&self) -> Result<Option<String>, DriverError> {
        let operation = self.metadata_operation("SERVER VERSION", Vec::new());
        self.prepare_governed_read(&operation).await?;
        let start = Instant::now();
        let result = self.inner.server_version().await;
        self.audit_read_result(&operation, start, &result, None).await?;
        result
    }

    async fn ping(&self) -> Result<(), DriverError> {
        self.inner.ping().await
    }

    async fn close(self: Box<Self>) -> Result<(), DriverError> {
        Ok(())
    }
}

impl PolicyGuard {
    async fn execute_write<'a, F, Fut>(
        &'a self,
        sql: &'a str,
        batch_id: Option<Uuid>,
        authorization: Authorization,
        execute: F,
    ) -> Result<ExecResult, DriverError>
    where
        F: FnOnce(&'a Arc<dyn Connection>) -> Fut,
        Fut: std::future::Future<Output = Result<ExecResult, DriverError>>,
    {
        self.require_governed_write_available()?;
        let operation = self.operation(
            sql,
            batch_id,
            &authorization.facts,
            &authorization.decision,
            authorization.approval_outcome,
            authorization.preview_state,
        );
        self.handle_intent_failure(self.record_intent(&operation).await)?;
        let mut pending_write = self.ctx.audit_state.pending_write();
        let start = Instant::now();
        let result = execute(&self.inner).await;
        let rows = result.as_ref().ok().map(|value| value.rows_affected);
        self.audit_write_result(&operation, start, &result, rows).await?;
        pending_write.disarm();
        result
    }

    async fn execute_query_write<'a, F, Fut>(
        &'a self,
        sql: &'a str,
        batch_id: Option<Uuid>,
        authorization: Authorization,
        execute: F,
    ) -> Result<QueryResult, DriverError>
    where
        F: FnOnce(&'a Arc<dyn Connection>) -> Fut,
        Fut: std::future::Future<Output = Result<QueryResult, DriverError>>,
    {
        self.require_governed_write_available()?;
        let operation = self.operation(
            sql,
            batch_id,
            &authorization.facts,
            &authorization.decision,
            authorization.approval_outcome,
            authorization.preview_state,
        );
        self.handle_intent_failure(self.record_intent(&operation).await)?;
        let mut pending_write = self.ctx.audit_state.pending_write();
        let start = Instant::now();
        let result = execute(&self.inner).await.map(|value| self.mask_result(value));
        let rows = result.as_ref().ok().map(|value| value.rows.len() as u64);
        self.audit_write_result(&operation, start, &result, rows).await?;
        pending_write.disarm();
        result
    }
}

#[async_trait]
impl Transaction for PolicyTransaction {
    async fn query(&mut self, sql: &str) -> Result<QueryResult, DriverError> {
        let authorization = self.guard.authorize(sql, false).await?;
        if authorization.facts.writes {
            self.guard.require_governed_write_available()?;
            let operation = self.guard.operation(
                sql,
                Some(self.batch_id),
                &authorization.facts,
                &authorization.decision,
                authorization.approval_outcome,
                authorization.preview_state,
            );
            self.guard
                .handle_intent_failure(self.guard.record_intent(&operation).await)?;
            let mut pending_write = self.guard.ctx.audit_state.pending_write();
            let start = Instant::now();
            let result = self.inner.query(sql).await.map(|value| self.guard.mask_result(value));
            let rows = result.as_ref().ok().map(|value| value.rows.len() as u64);
            self.guard
                .audit_transaction_result(&operation, start, &result, rows)
                .await?;
            pending_write.disarm();
            return result;
        }
        let operation = self.guard.operation(
            sql,
            Some(self.batch_id),
            &authorization.facts,
            &authorization.decision,
            authorization.approval_outcome,
            authorization.preview_state,
        );
        self.guard.prepare_governed_read(&operation).await?;
        let start = Instant::now();
        let result = self.inner.query(sql).await.map(|value| self.guard.mask_result(value));
        let rows = result.as_ref().ok().map(|value| value.rows.len() as u64);
        self.guard
            .audit_transaction_result(&operation, start, &result, rows)
            .await?;
        result
    }

    async fn query_params(&mut self, sql: &str, params: &[Value]) -> Result<QueryResult, DriverError> {
        let authorization = self.guard.authorize(sql, true).await?;
        if authorization.facts.writes {
            self.guard.require_governed_write_available()?;
            let operation = self.guard.operation(
                sql,
                Some(self.batch_id),
                &authorization.facts,
                &authorization.decision,
                authorization.approval_outcome,
                authorization.preview_state,
            );
            self.guard
                .handle_intent_failure(self.guard.record_intent(&operation).await)?;
            let mut pending_write = self.guard.ctx.audit_state.pending_write();
            let start = Instant::now();
            let result = self
                .inner
                .query_params(sql, params)
                .await
                .map(|value| self.guard.mask_result(value));
            let rows = result.as_ref().ok().map(|value| value.rows.len() as u64);
            self.guard
                .audit_transaction_result(&operation, start, &result, rows)
                .await?;
            pending_write.disarm();
            return result;
        }
        let operation = self.guard.operation(
            sql,
            Some(self.batch_id),
            &authorization.facts,
            &authorization.decision,
            authorization.approval_outcome,
            authorization.preview_state,
        );
        self.guard.prepare_governed_read(&operation).await?;
        let start = Instant::now();
        let result = self
            .inner
            .query_params(sql, params)
            .await
            .map(|value| self.guard.mask_result(value));
        let rows = result.as_ref().ok().map(|value| value.rows.len() as u64);
        self.guard
            .audit_transaction_result(&operation, start, &result, rows)
            .await?;
        result
    }

    async fn execute(&mut self, sql: &str) -> Result<ExecResult, DriverError> {
        let authorization = self.guard.authorize(sql, false).await?;
        self.guard.require_governed_write_available()?;
        let operation = self.guard.operation(
            sql,
            Some(self.batch_id),
            &authorization.facts,
            &authorization.decision,
            authorization.approval_outcome,
            authorization.preview_state,
        );
        self.guard
            .handle_intent_failure(self.guard.record_intent(&operation).await)?;
        let mut pending_write = self.guard.ctx.audit_state.pending_write();
        let start = Instant::now();
        let result = self.inner.execute(sql).await;
        let rows = result.as_ref().ok().map(|value| value.rows_affected);
        self.guard
            .audit_transaction_result(&operation, start, &result, rows)
            .await?;
        pending_write.disarm();
        result
    }

    async fn execute_params(&mut self, sql: &str, params: &[Value]) -> Result<ExecResult, DriverError> {
        let authorization = self.guard.authorize(sql, true).await?;
        self.guard.require_governed_write_available()?;
        let operation = self.guard.operation(
            sql,
            Some(self.batch_id),
            &authorization.facts,
            &authorization.decision,
            authorization.approval_outcome,
            authorization.preview_state,
        );
        self.guard
            .handle_intent_failure(self.guard.record_intent(&operation).await)?;
        let mut pending_write = self.guard.ctx.audit_state.pending_write();
        let start = Instant::now();
        let result = self.inner.execute_params(sql, params).await;
        let rows = result.as_ref().ok().map(|value| value.rows_affected);
        self.guard
            .audit_transaction_result(&operation, start, &result, rows)
            .await?;
        pending_write.disarm();
        result
    }

    async fn commit(self: Box<Self>) -> Result<(), DriverError> {
        self.guard.require_governed_write_available()?;
        let operation = PolicyGuard::commit_operation(self.batch_id);
        self.guard.record_intent(&operation).await.map_err(|error| {
            DriverError::PolicyDenied(format!(
                "commit denied because audit intent could not be persisted: {error}"
            ))
        })?;
        let mut pending_write = self.guard.ctx.audit_state.pending_write();
        let start = Instant::now();
        let result = self.inner.commit().await;
        let (status, transaction_outcome, category) = match &result {
            Ok(()) => (AuditTerminalStatus::Succeeded, AuditTransactionOutcome::Committed, None),
            Err(error) if is_ambiguous_post_dispatch(error) => {
                self.guard.ctx.audit_state.disable_governed_writes();
                (
                    AuditTerminalStatus::Unknown,
                    AuditTransactionOutcome::Unknown,
                    Some(error_category(error)),
                )
            }
            Err(error) => (
                AuditTerminalStatus::Failed,
                AuditTransactionOutcome::Failed,
                Some(error_category(error)),
            ),
        };
        let audit_result = self
            .guard
            .record_outcome(
                &operation,
                AuditOutcome {
                    terminal_status: status,
                    transaction_outcome,
                    rows_affected: None,
                    error_category: category,
                    duration_ms: start.elapsed().as_millis() as u64,
                },
            )
            .await;
        self.guard.handle_write_outcome_failure(audit_result)?;
        pending_write.disarm();
        result
    }

    async fn rollback(self: Box<Self>) -> Result<(), DriverError> {
        let operation = AuditOperation {
            operation_id: Uuid::new_v4(),
            batch_id: Some(self.batch_id),
            sql: "ROLLBACK",
            class: AuditOperationClass::TransactionRollback,
            targets: Vec::new(),
            decision_rule: "transaction_rollback".into(),
            approval_outcome: AuditApprovalOutcome::NotRequired,
            preview_state: AuditPreviewState::NotRequested,
        };
        self.guard.record_intent(&operation).await.map_err(|error| {
            DriverError::PolicyDenied(format!(
                "rollback denied because audit intent could not be persisted: {error}"
            ))
        })?;
        let mut pending_write = self.guard.ctx.audit_state.pending_write();
        let start = Instant::now();
        let result = self.inner.rollback().await;
        let (status, transaction_outcome, category) = match &result {
            Ok(()) => (
                AuditTerminalStatus::Succeeded,
                AuditTransactionOutcome::RolledBack,
                None,
            ),
            Err(error) if is_ambiguous_post_dispatch(error) => {
                self.guard.ctx.audit_state.disable_governed_writes();
                (
                    AuditTerminalStatus::Unknown,
                    AuditTransactionOutcome::Unknown,
                    Some(error_category(error)),
                )
            }
            Err(error) => (
                AuditTerminalStatus::Failed,
                AuditTransactionOutcome::Failed,
                Some(error_category(error)),
            ),
        };
        let audit_result = self
            .guard
            .record_outcome(
                &operation,
                AuditOutcome {
                    terminal_status: status,
                    transaction_outcome,
                    rows_affected: None,
                    error_category: category,
                    duration_ms: start.elapsed().as_millis() as u64,
                },
            )
            .await;
        self.guard.handle_write_outcome_failure(audit_result)?;
        pending_write.disarm();
        result
    }
}

#[cfg(test)]
#[path = "guard_tests.rs"]
mod guard_tests;
