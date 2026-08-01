use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tablepro_policy::{ApprovalOutcome, ApprovalRequest, ApprovalSink};

/// GTK approval sink. Posts a modal `adw::AlertDialog` on the glib main
/// context and parks the async caller until the user responds.
pub struct GtkApprovalSink;

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
            dialog.add_response("deny", "Deny");
            dialog.add_response("once", "Approve once");
            dialog.add_response("session", "Approve for session");
            dialog.set_response_appearance("deny", adw::ResponseAppearance::Destructive);
            dialog.set_response_appearance("once", adw::ResponseAppearance::Suggested);
            dialog.set_default_response(Some("deny"));
            dialog.set_close_response("deny");

            dialog.connect_response(None, move |_dlg, response| {
                let outcome = match response {
                    "once" => ApprovalOutcome::AllowOnce,
                    "session" => ApprovalOutcome::AllowSession,
                    _ => ApprovalOutcome::Deny,
                };
                if let Ok(mut guard) = tx_c.lock()
                    && let Some(sender) = guard.take()
                {
                    let _ = sender.send(outcome);
                }
            });

            let parent = gtk::Application::default().active_window();
            match parent {
                Some(win) => dialog.present(Some(&win)),
                None => {
                    let win = gtk::Window::new();
                    dialog.present(Some(&win));
                }
            }
        });

        rx.await.unwrap_or(ApprovalOutcome::Deny)
    }
}
