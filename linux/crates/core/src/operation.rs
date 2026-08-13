use std::future::Future;

use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::DriverError;

#[derive(Clone, Debug)]
pub struct OperationControl {
    cancellation_token: CancellationToken,
    deadline: Option<Instant>,
}

impl OperationControl {
    pub fn new(cancellation_token: CancellationToken, deadline: Option<Instant>) -> Self {
        Self {
            cancellation_token,
            deadline,
        }
    }

    pub fn cancellation_token(&self) -> &CancellationToken {
        &self.cancellation_token
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }
}

pub(crate) async fn run_controlled<T, F>(operation: F, control: &OperationControl) -> Result<T, DriverError>
where
    F: Future<Output = Result<T, DriverError>>,
{
    if control.cancellation_token.is_cancelled() {
        return Err(DriverError::Cancelled);
    }

    if control.deadline.is_some_and(|deadline| deadline <= Instant::now()) {
        return Err(DriverError::TimedOut);
    }

    match control.deadline {
        Some(deadline) => {
            tokio::select! {
                biased;
                _ = control.cancellation_token.cancelled() => Err(unknown_outcome(DriverError::Cancelled)),
                _ = tokio::time::sleep_until(deadline) => Err(unknown_outcome(DriverError::TimedOut)),
                result = operation => result,
            }
        }
        None => {
            tokio::select! {
                biased;
                _ = control.cancellation_token.cancelled() => Err(unknown_outcome(DriverError::Cancelled)),
                result = operation => result,
            }
        }
    }
}

fn unknown_outcome(interruption: DriverError) -> DriverError {
    DriverError::OperationOutcomeUnknown {
        source: Box::new(interruption),
    }
}

#[cfg(test)]
mod tests {
    use std::future::pending;
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn returns_completed_operation_result() {
        let control = OperationControl::new(CancellationToken::new(), None);

        let result = run_controlled(async { Ok::<_, DriverError>(42) }, &control).await;

        assert_eq!(result.expect("operation should complete"), 42);
    }

    #[tokio::test]
    async fn reports_pre_dispatch_cancellation() {
        let token = CancellationToken::new();
        token.cancel();
        let control = OperationControl::new(token, None);

        let error = run_controlled(pending::<Result<(), DriverError>>(), &control)
            .await
            .expect_err("operation should be interrupted");

        assert!(matches!(error, DriverError::Cancelled));
    }

    #[tokio::test]
    async fn reports_pre_dispatch_timeout() {
        let deadline = Instant::now() - Duration::from_millis(1);
        let control = OperationControl::new(CancellationToken::new(), Some(deadline));

        let error = run_controlled(pending::<Result<(), DriverError>>(), &control)
            .await
            .expect_err("operation should be interrupted");

        assert!(matches!(error, DriverError::TimedOut));
    }

    #[tokio::test]
    async fn cancellation_wins_when_both_controls_are_ready() {
        let token = CancellationToken::new();
        token.cancel();
        let control = OperationControl::new(token, Some(Instant::now()));

        let error = run_controlled(pending::<Result<(), DriverError>>(), &control)
            .await
            .expect_err("operation should be interrupted");

        assert!(matches!(error, DriverError::Cancelled));
    }
}
