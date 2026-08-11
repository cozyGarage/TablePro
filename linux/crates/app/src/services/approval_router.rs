use std::sync::Arc;

use async_trait::async_trait;
use tablepro_policy::{ApprovalOutcome, ApprovalRequest, ApprovalSink, Principal};

pub struct ApprovalRouter {
    human: Arc<dyn ApprovalSink>,
    agent: Arc<dyn ApprovalSink>,
}

impl ApprovalRouter {
    pub fn new(human: Arc<dyn ApprovalSink>, agent: Arc<dyn ApprovalSink>) -> Self {
        Self { human, agent }
    }
}

#[async_trait]
impl ApprovalSink for ApprovalRouter {
    async fn request(&self, request: ApprovalRequest) -> ApprovalOutcome {
        let sink = match &request.principal {
            Principal::Human { .. } => &self.human,
            Principal::Agent { .. } => &self.agent,
        };
        sink.request(request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tablepro_core::Environment;
    use tablepro_policy::{Principal, StatementFacts};

    struct CountingSink {
        calls: Arc<AtomicUsize>,
        outcome: ApprovalOutcome,
    }

    #[async_trait]
    impl ApprovalSink for CountingSink {
        async fn request(&self, _request: ApprovalRequest) -> ApprovalOutcome {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.outcome
        }
    }

    fn request(principal: Principal) -> ApprovalRequest {
        ApprovalRequest {
            principal,
            environment: Environment::Prod,
            connection_name: "production".into(),
            sql: "DELETE FROM jobs WHERE id = 1".into(),
            facts: StatementFacts::unparseable("test"),
            rule: "test".into(),
            reason: "test".into(),
            preview: None,
            estimated_rows: None,
        }
    }

    #[tokio::test]
    async fn routes_humans_and_agents_to_distinct_sinks() {
        let human_calls = Arc::new(AtomicUsize::new(0));
        let agent_calls = Arc::new(AtomicUsize::new(0));
        let router = ApprovalRouter::new(
            Arc::new(CountingSink {
                calls: human_calls.clone(),
                outcome: ApprovalOutcome::AllowOnce,
            }),
            Arc::new(CountingSink {
                calls: agent_calls.clone(),
                outcome: ApprovalOutcome::Deny,
            }),
        );

        assert_eq!(
            router.request(request(Principal::human_gui())).await,
            ApprovalOutcome::AllowOnce
        );
        assert_eq!(
            router
                .request(request(Principal::Agent {
                    token: "token".into(),
                    client: None,
                    model: None,
                }))
                .await,
            ApprovalOutcome::Deny
        );
        assert_eq!(human_calls.load(Ordering::SeqCst), 1);
        assert_eq!(agent_calls.load(Ordering::SeqCst), 1);
    }
}
