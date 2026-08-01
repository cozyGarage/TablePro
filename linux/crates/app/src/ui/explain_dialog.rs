//! EXPLAIN plan dialog. Runs `EXPLAIN <sql>` through the policy-gated
//! active connection and shows the textual plan.

use gtk::prelude::IsA;
use relm4::adw::prelude::*;
use relm4::{adw, gtk};

use crate::services::database_service;
use crate::tr;

pub fn present(parent: &impl IsA<gtk::Window>, sql: &str) {
    let trimmed = sql.trim();
    if trimmed.is_empty() {
        let toast_parent = parent.clone().upcast::<gtk::Window>();
        let dialog = adw::AlertDialog::new(
            Some(&tr!("Explain query")),
            Some(&tr!("The editor is empty. Type a statement to explain.")),
        );
        dialog.add_response("close", &tr!("Close"));
        dialog.set_close_response("close");
        dialog.present(Some(&toast_parent));
        return;
    }

    let Some(conn) = database_service::instance().active() else {
        return;
    };

    let explain_sql = if trimmed.to_ascii_uppercase().starts_with("EXPLAIN") {
        trimmed.to_string()
    } else {
        format!("EXPLAIN {trimmed}")
    };

    let window = adw::Window::builder()
        .title(tr!("Explain plan"))
        .transient_for(parent)
        .modal(true)
        .default_width(720)
        .default_height(480)
        .build();

    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    toolbar.add_top_bar(&header);

    let scrolled = gtk::ScrolledWindow::builder().hexpand(true).vexpand(true).build();
    let view = gtk::TextView::builder()
        .editable(false)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::WordChar)
        .left_margin(12)
        .right_margin(12)
        .top_margin(12)
        .bottom_margin(12)
        .build();
    view.buffer().set_text(&tr!("Running EXPLAIN…"));
    scrolled.set_child(Some(&view));
    toolbar.set_content(Some(&scrolled));
    window.set_content(Some(&toolbar));
    window.present();

    let buffer = view.buffer();
    glib::spawn_future_local(async move {
        let text = match conn.query(&explain_sql).await {
            Ok(result) => format_explain_result(&result),
            Err(e) => format!("{e}"),
        };
        buffer.set_text(&text);
    });
}

fn format_explain_result(result: &tablepro_core::QueryResult) -> String {
    if result.rows.is_empty() {
        return String::from("(empty plan)");
    }
    let mut lines = Vec::with_capacity(result.rows.len());
    for row in &result.rows {
        let cells: Vec<String> = row.iter().map(|v| crate::ui::grid::value_to_display_text(v)).collect();
        if cells.len() == 1 {
            lines.push(cells.into_iter().next().unwrap_or_default());
        } else {
            lines.push(cells.join(" | "));
        }
    }
    lines.join("\n")
}
