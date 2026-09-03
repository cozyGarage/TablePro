//! Statement classification, policy decisions, and the connection guard
//! that every consumer (GUI, MCP, agentd) must pass through.

mod approval;
mod audit;
mod blast_radius;
mod classify;
mod config;
mod guard;
mod mask;
mod principal;
mod rules;
mod sensitive_projection;

pub use approval::{ApprovalOutcome, ApprovalRequest, ApprovalSink, AutoApproveSink, DenyApprovalSink};
pub use audit::{
    AuditApprovalOutcome, AuditError, AuditErrorCategory, AuditEvent, AuditOperationClass, AuditPreviewState,
    AuditRecordPhase, AuditSink, AuditState, AuditTerminalStatus, AuditTransactionOutcome, NullAuditSink,
};
pub use blast_radius::{BlastRadiusResult, BlastRadiusRewrite, count_sql_for_mutation};
pub use classify::{StatementClass, StatementFacts, classify, statement_requires_write_capability};
pub use config::{EnvPolicy, MaskRule, PolicyConfig, WritePolicy, load_from_path, load_policy, policy_path};
pub use guard::{GuardContext, PolicyGuard};
pub use mask::{DEFAULT_SENSITIVE_PATTERNS, apply_masking, column_is_sensitive};
pub use principal::Principal;
pub use rules::{Decision, evaluate};
