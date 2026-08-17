use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tablepro_policy::{ApprovalOutcome, ApprovalRequest, ApprovalSink};

/// GTK approval sink. Posts a modal `adw::AlertDialog` on the glib main
/// context and parks the async caller until the user responds.
pub struct GtkApprovalSink;

const DENY_RESPONSE: &str = "deny";
const APPROVE_ONCE_RESPONSE: &str = "once";
const CLOSE_RESPONSE: &str = DENY_RESPONSE;

fn outcome_for_response(response: &str) -> ApprovalOutcome {
    if response == APPROVE_ONCE_RESPONSE {
        return ApprovalOutcome::AllowOnce;
    }
    ApprovalOutcome::Deny
}

#[async_trait]
impl ApprovalSink for GtkApprovalSink {
    async fn request(&self, req: ApprovalRequest) -> ApprovalOutcome {
        let (tx, rx) = tokio::sync::oneshot::channel::<ApprovalOutcome>();
        let tx = Arc::new(Mutex::new(Some(tx)));

        let heading = format!("Approve write: {}", req.connection_name);
        let targets = if req.facts.tables.is_empty() {
            "(none detected)".to_string()
        } else {
            req.facts.tables.join(", ")
        };
        let mut body = format!(
            "Rule: {}\n{}\n\nPrincipal: {}\nEnvironment: {}\nClass: {:?}\nTargets: {}\n\n{}",
            req.rule,
            req.reason,
            req.principal.label(),
            req.environment.display_name(),
            req.facts.class,
            targets,
            req.sql
        );
        if let Some(preview) = &req.preview {
            body.push_str("\n\n");
            body.push_str(preview);
        }
        if let Some(rows) = req.estimated_rows {
            body.push_str(&format!("\n\nExact affected-row count: {rows}"));
        }

        let heading_c = heading.clone();
        let body_c = body.clone();
        let tx_c = tx.clone();
        glib::MainContext::default().invoke(move || {
            use relm4::adw::prelude::*;
            use relm4::{adw, gtk};

            let dialog = adw::AlertDialog::builder().heading(&heading_c).body(&body_c).build();
            dialog.add_response(DENY_RESPONSE, "Deny");
            dialog.add_response(APPROVE_ONCE_RESPONSE, "Approve once");
            dialog.set_response_appearance(DENY_RESPONSE, adw::ResponseAppearance::Destructive);
            dialog.set_response_appearance(APPROVE_ONCE_RESPONSE, adw::ResponseAppearance::Suggested);
            dialog.set_default_response(Some(DENY_RESPONSE));
            dialog.set_close_response(CLOSE_RESPONSE);

            dialog.connect_response(None, move |_dlg, response| {
                let outcome = outcome_for_response(response);
                if let Ok(mut guard) = tx_c.lock()
                    && let Some(sender) = guard.take()
                {
                    let _ = sender.send(outcome);
                }
            });

            if let Some(window) = gtk::Application::default().active_window() {
                dialog.present(Some(&window));
            } else if let Ok(mut guard) = tx.lock()
                && let Some(sender) = guard.take()
            {
                let _ = sender.send(ApprovalOutcome::Deny);
            }
        });

        rx.await.unwrap_or(ApprovalOutcome::Deny)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dismissed_approval_denies() {
        assert_eq!(outcome_for_response(CLOSE_RESPONSE), ApprovalOutcome::Deny);
    }

    #[test]
    fn unexpected_approval_response_denies() {
        assert_eq!(outcome_for_response("unexpected"), ApprovalOutcome::Deny);
    }

    #[test]
    fn approve_once_response_allows_one_operation() {
        assert_eq!(outcome_for_response(APPROVE_ONCE_RESPONSE), ApprovalOutcome::AllowOnce);
    }
}
