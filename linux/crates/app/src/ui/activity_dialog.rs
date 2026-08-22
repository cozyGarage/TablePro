//! Activity / monitoring dialog. Runs driver-specific activity SQL through
//! the policy-gated connection owned by the calling window and shows the
//! result grid.

use relm4::adw::prelude::*;
use relm4::{adw, gtk};

use tablepro_core::{ActivityQuery, ActivityUnsupported, Value, activity_kinds, activity_sql, parse_session_id};

use crate::services::database_service;
use crate::tr;

pub fn present(parent: &gtk::Window, connection_id: Option<uuid::Uuid>) {
    let Some(connection) = connection_id else {
        let alert = adw::AlertDialog::new(
            Some(&tr!("No active connection")),
            Some(&tr!("Open a connection before viewing server activity.")),
        );
        alert.add_response("ok", &tr!("OK"));
        alert.present(Some(parent));
        return;
    };
    let Some(meta) = database_service::instance().metadata(connection) else {
        let alert = adw::AlertDialog::new(
            Some(&tr!("No active connection")),
            Some(&tr!("Open a connection before viewing server activity.")),
        );
        alert.add_response("ok", &tr!("OK"));
        alert.present(Some(parent));
        return;
    };

    let dialog = adw::Dialog::builder().title(tr!("Server activity")).build();
    dialog.set_content_width(900);
    dialog.set_content_height(560);

    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    toolbar.add_top_bar(&header);

    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let button_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .margin_start(12)
        .margin_end(12)
        .margin_top(8)
        .margin_bottom(8)
        .build();

    let scrolled = gtk::ScrolledWindow::builder().vexpand(true).hexpand(true).build();
    let text = gtk::TextView::builder()
        .editable(false)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::None)
        .build();
    scrolled.set_child(Some(&text));

    let status = gtk::Label::builder()
        .label(tr!("Choose a query above."))
        .xalign(0.0)
        .margin_start(12)
        .margin_end(12)
        .margin_bottom(8)
        .build();
    status.add_css_class("dim-label");

    let driver_id = meta.driver_id.clone();
    let actions = [
        ("Sessions", ActivityQuery::Sessions),
        ("Blocking locks", ActivityQuery::BlockingLocks),
        ("Long-running", ActivityQuery::LongRunning),
        ("Replication lag", ActivityQuery::ReplicationLag),
    ];
    let supported = activity_kinds(&driver_id);
    for (label, kind) in actions {
        let btn = gtk::Button::with_label(label);
        let text_buf = text.buffer();
        let status_l = status.clone();
        let driver = driver_id.clone();
        if !supported.contains(&kind) {
            btn.set_sensitive(false);
            btn.set_tooltip_text(Some(&unsupported_text(&driver_id)));
        }
        btn.connect_clicked(move |_| {
            let sql = match activity_sql(&driver, kind, None) {
                Ok(sql) => sql,
                Err(reason) => {
                    status_l.set_text(&reason_text(reason));
                    return;
                }
            };
            let Some(conn) = database_service::instance().get(connection) else {
                status_l.set_text(&tr!("Connection closed."));
                return;
            };
            status_l.set_text(&tr!("Running…"));
            let text_buf = text_buf.clone();
            let status_l = status_l.clone();
            let timeout_secs = crate::services::operation_control::configured_timeout_secs();
            glib::spawn_future_local(async move {
                let control = crate::services::operation_control::bounded(timeout_secs);
                match conn.query_controlled(&sql, &control).await {
                    Ok(result) => {
                        let cols: Vec<String> = result.columns.iter().map(|c| c.name.clone()).collect();
                        let rendered = render_result(&cols, &result.rows);
                        let n = result.rows.len();
                        text_buf.set_text(&rendered);
                        status_l.set_text(&format!("{n} rows"));
                    }
                    Err(e) => {
                        text_buf.set_text(&e.to_string());
                        status_l.set_text(&tr!("Query failed"));
                    }
                }
            });
        });
        button_row.append(&btn);
    }

    let kill_entry = gtk::Entry::builder()
        .placeholder_text(tr!("Session id to kill"))
        .width_chars(12)
        .build();
    let kill_btn = gtk::Button::with_label(&tr!("Kill"));
    kill_btn.add_css_class("destructive-action");
    if !supported.contains(&ActivityQuery::KillSession) {
        kill_entry.set_sensitive(false);
        kill_btn.set_sensitive(false);
        kill_btn.set_tooltip_text(Some(&unsupported_text(&driver_id)));
    }
    let text_buf = text.buffer();
    let status_l = status.clone();
    let driver = driver_id.clone();
    let kill_entry_c = kill_entry.clone();
    kill_btn.connect_clicked(move |_| {
        let id = match parse_session_id(&kill_entry_c.text()) {
            Some(id) => id,
            None => {
                status_l.set_text(&tr!("Session id must be a positive integer."));
                return;
            }
        };
        let sql = match activity_sql(&driver, ActivityQuery::KillSession, Some(id)) {
            Ok(sql) => sql,
            Err(reason) => {
                status_l.set_text(&reason_text(reason));
                return;
            }
        };
        let Some(conn) = database_service::instance().get(connection) else {
            return;
        };
        let text_buf = text_buf.clone();
        let status_l = status_l.clone();
        let timeout_secs = crate::services::operation_control::configured_timeout_secs();
        glib::spawn_future_local(async move {
            let control = crate::services::operation_control::bounded(timeout_secs);
            match conn.execute_controlled(&sql, &control).await {
                Ok(r) => {
                    text_buf.set_text(&format!("Kill issued; rows_affected={}", r.rows_affected));
                    status_l.set_text(&tr!("Done"));
                }
                Err(e) => {
                    text_buf.set_text(&e.to_string());
                    status_l.set_text(&tr!("Kill failed"));
                }
            }
        });
    });
    button_row.append(&kill_entry);
    button_row.append(&kill_btn);

    box_.append(&button_row);
    box_.append(&scrolled);
    box_.append(&status);
    toolbar.set_content(Some(&box_));
    dialog.set_child(Some(&toolbar));
    dialog.present(Some(parent));
}

/// The dialog offers every activity view, so a button the engine cannot
/// answer has to say which of the two reasons applies rather than a bare
/// "not supported".
fn reason_text(reason: ActivityUnsupported) -> String {
    match reason {
        ActivityUnsupported::Engine => tr!("This engine has no server activity views."),
        ActivityUnsupported::Query => tr!("This engine does not report this kind of activity."),
        ActivityUnsupported::MissingSessionId => tr!("Session id must be a positive integer."),
    }
}

fn unsupported_text(driver_id: &str) -> String {
    if activity_kinds(driver_id).is_empty() {
        reason_text(ActivityUnsupported::Engine)
    } else {
        reason_text(ActivityUnsupported::Query)
    }
}

fn render_result(columns: &[String], rows: &[Vec<Value>]) -> String {
    let mut out = String::new();
    out.push_str(&columns.join("\t"));
    out.push('\n');
    for row in rows {
        let cells: Vec<String> = row.iter().map(value_to_string).collect();
        out.push_str(&cells.join("\t"));
        out.push('\n');
    }
    out
}

fn value_to_string(v: &Value) -> String {
    match v {
        Value::Null => "NULL".into(),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Text(s) => s.replace(['\t', '\n'], " "),
        Value::Bytes(b) => format!("\\x{} bytes", b.len()),
        Value::Date(d) => d.to_string(),
        Value::Time(t) => t.to_string(),
        Value::DateTime(dt) => dt.to_string(),
        Value::TimestampTz(ts) => ts.to_rfc3339(),
        Value::Decimal(d) => d.to_string(),
        Value::Uuid(u) => u.to_string(),
        Value::Json(j) => j.to_string(),
    }
}
