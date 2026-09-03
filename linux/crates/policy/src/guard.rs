use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::Utc;
use sha2::{Digest, Sha256};
use tablepro_core::{
    ColumnInfo, Connection, DriverError, Environment, ExecResult, ForeignKeyInfo, IndexInfo, OperationControl,
    QueryResult, TableInfo, Transaction, Value, check_pre_dispatch,
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

const BLAST_RADIUS_TIMEOUT: Duration = Duration::from_secs(30);

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

    async fn authorize(
        &self,
        sql: &str,
        _is_params_exec: bool,
        control: Option<&OperationControl>,
    ) -> Result<Authorization, DriverError> {
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
            self.estimate_blast_radius(sql, &facts, control).await?
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

    async fn estimate_blast_radius(
        &self,
        sql: &str,
        facts: &StatementFacts,
        control: Option<&OperationControl>,
    ) -> Result<Option<u64>, DriverError> {
        let rewrite = match count_sql_for_mutation(sql, &self.ctx.driver_id) {
            None => return Ok(None),
            Some(crate::blast_radius::BlastRadiusRewrite::Known(rows)) => return Ok(Some(rows)),
            Some(crate::blast_radius::BlastRadiusRewrite::CountQuery(rewrite)) => rewrite,
        };
        let operation = self.metadata_operation("BLAST RADIUS ESTIMATE", facts.tables.clone());
        self.record_intent(&operation).await.map_err(|error| {
            DriverError::PolicyDenied(format!(
                "operation denied because audit intent could not be persisted: {error}"
            ))
        })?;
        let start = Instant::now();
        let owned_control;
        let control = match control {
            Some(control) => control,
            None => {
                owned_control = OperationControl::with_timeout(BLAST_RADIUS_TIMEOUT);
                &owned_control
            }
        };
        let result = self.inner.query_controlled(&rewrite.count_sql, control).await;
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
                        terminal_status: controlled_error_terminal_status(error),
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
        Err(DriverError::PolicyDenied(format!(
            "operation denied because audit recording failed: {error}"
        )))
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
        self.mask_result_for_sql(None, result)
    }

    /// Like `mask_result`, but when `sql` is available it also consults the
    /// parsed statement's projection so an alias or wrapping expression
    /// cannot hide a sensitive source column from the result-set-name match.
    fn mask_result_for_sql(&self, sql: Option<&str>, result: QueryResult) -> QueryResult {
        if !self.should_mask() {
            return result;
        }
        let patterns = self.ctx.policy.effective_mask_patterns();
        let sensitive_positions =
            sql.and_then(|sql| crate::sensitive_projection::sensitive_projection(sql, &self.ctx.driver_id, &patterns));
        apply_masking(result, &patterns, sensitive_positions.as_deref())
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

    async fn audit_controlled_read_result<T>(
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
                let terminal_status = controlled_error_terminal_status(error);
                self.record_outcome(
                    operation,
                    AuditOutcome {
                        terminal_status,
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
                let (terminal_status, transaction_outcome, disables_governed_writes) = transaction_error_outcome(error);
                if disables_governed_writes {
                    self.ctx.audit_state.disable_governed_writes();
                }
                self.record_outcome(
                    operation,
                    AuditOutcome {
                        terminal_status,
                        transaction_outcome,
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
                let (terminal_status, disables_governed_writes) = write_error_outcome(error);
                if disables_governed_writes {
                    self.ctx.audit_state.disable_governed_writes();
                }
                self.record_outcome(
                    operation,
                    AuditOutcome {
                        terminal_status,
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

fn controlled_error_terminal_status(error: &DriverError) -> AuditTerminalStatus {
    match error {
        DriverError::Cancelled => AuditTerminalStatus::Cancelled,
        DriverError::TimedOut => AuditTerminalStatus::TimedOut,
        DriverError::OperationOutcomeUnknown { .. } => AuditTerminalStatus::Unknown,
        _ => AuditTerminalStatus::Failed,
    }
}

fn write_error_outcome(error: &DriverError) -> (AuditTerminalStatus, bool) {
    match error {
        DriverError::Cancelled => (AuditTerminalStatus::Cancelled, false),
        DriverError::TimedOut => (AuditTerminalStatus::TimedOut, false),
        DriverError::OperationOutcomeUnknown { .. } => (AuditTerminalStatus::Unknown, true),
        _ if is_ambiguous_post_dispatch(error) => (AuditTerminalStatus::Unknown, true),
        _ => (AuditTerminalStatus::Failed, false),
    }
}

fn transaction_error_outcome(error: &DriverError) -> (AuditTerminalStatus, AuditTransactionOutcome, bool) {
    match error {
        DriverError::Cancelled => (AuditTerminalStatus::Cancelled, AuditTransactionOutcome::Pending, false),
        DriverError::TimedOut => (AuditTerminalStatus::TimedOut, AuditTransactionOutcome::Pending, false),
        _ => (AuditTerminalStatus::Unknown, AuditTransactionOutcome::Unknown, true),
    }
}

fn is_ambiguous_post_dispatch(error: &DriverError) -> bool {
    match error {
        DriverError::ConnectionRefused
        | DriverError::Disconnected
        | DriverError::AuthFailed
        | DriverError::IntegratedAuth(_)
        | DriverError::Tls(_)
        | DriverError::Internal(_)
        | DriverError::OperationOutcomeUnknown { .. } => true,
        DriverError::Transaction { source, .. } => is_ambiguous_post_dispatch(source),
        DriverError::Query { .. }
        | DriverError::ReadOnly
        | DriverError::PolicyDenied(_)
        | DriverError::Unsupported(_)
        | DriverError::Cancelled
        | DriverError::TimedOut => false,
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
        DriverError::Cancelled => AuditErrorCategory::Cancelled,
        DriverError::TimedOut => AuditErrorCategory::Timeout,
        DriverError::OperationOutcomeUnknown { .. } => AuditErrorCategory::Unknown,
        DriverError::Transaction { .. } => AuditErrorCategory::Transaction,
    }
}

mod connection;

#[cfg(test)]
#[path = "guard_tests.rs"]
mod guard_tests;
