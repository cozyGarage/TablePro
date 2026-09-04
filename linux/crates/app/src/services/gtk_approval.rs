use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tablepro_policy::{ApprovalOutcome, ApprovalRequest, ApprovalSink};

/// GTK approval sink. Posts a modal `adw::AlertDialog` on the glib main
/// context and parks the async caller until the user responds.
pub struct GtkApprovalSink;

const DENY_RESPONSE: &str = "deny";
const APPROVE_ONCE_RESPONSE: &str = "once";
const CLOSE_RESPONSE: &str = DENY_RESPONSE;

/// The window that actually owns the triggering connection wins over
/// whichever window last had focus, which wins over any other visible
/// window, which wins over anything still on screen at all.
fn preferred_approval_window<T>(
    registered_and_visible: Option<T>,
    active: Option<T>,
    any_visible: Option<T>,
    listed_toplevel: Option<T>,
) -> Option<T> {
    registered_and_visible.or(active).or(any_visible).or(listed_toplevel)
}

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

            // Prefer the window that actually owns the connection this
            // statement runs on -- active_window() answers with whatever
            // window last had focus, which in a multi-window session can
            // be a different connection than the one being approved.
            let application = gtk::Application::default();
            let window = preferred_approval_window(
                crate::services::window_registry::window_for(req.connection_id).filter(|window| window.is_visible()),
                application.active_window(),
                application.windows().into_iter().find(|window| window.is_visible()),
                gtk::Window::list_toplevels()
                    .into_iter()
                    .filter_map(|widget| widget.downcast::<gtk::Window>().ok())
                    .find(|window| window.is_visible()),
            );
            if let Some(window) = window {
                dialog.present(Some(&window));
                return;
            }
            if let Ok(mut guard) = tx.lock()
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

    #[test]
    fn the_connections_own_visible_window_wins_over_a_different_active_window() {
        assert_eq!(
            preferred_approval_window(Some("owner"), Some("last-focused"), Some("other"), Some("toplevel")),
            Some("owner")
        );
    }

    #[test]
    fn an_unregistered_or_hidden_connection_falls_back_to_the_active_window() {
        assert_eq!(
            preferred_approval_window(None, Some("last-focused"), Some("other"), Some("toplevel")),
            Some("last-focused")
        );
    }

    #[test]
    fn no_active_window_falls_back_to_any_other_visible_window() {
        assert_eq!(
            preferred_approval_window(None, None, Some("other"), Some("toplevel")),
            Some("other")
        );
    }

    #[test]
    fn nothing_visible_falls_back_to_a_listed_toplevel() {
        assert_eq!(
            preferred_approval_window(None, None, None, Some("toplevel")),
            Some("toplevel")
        );
    }

    #[test]
    fn no_window_anywhere_denies_by_finding_none() {
        assert_eq!(preferred_approval_window::<&str>(None, None, None, None), None);
    }
}
