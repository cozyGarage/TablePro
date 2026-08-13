use relm4::adw::prelude::*;
use relm4::{adw, gtk};

use super::{StatementOutcome, StatementOutcomeKind};
use crate::ui::grid::{TabGridContext, build_column_view};
use tablepro_core::{DriverError, OperationControl};

pub(crate) fn clear_box(b: &gtk::Box) {
    while let Some(child) = b.first_child() {
        b.remove(&child);
    }
}

pub(crate) enum ScriptRunResult {
    Completed(Vec<StatementOutcome>),
    Cancelled,
    TimedOut,
}

pub(crate) async fn run_statements(
    conn: std::sync::Arc<dyn tablepro_core::Connection>,
    statements: Vec<String>,
    control: &OperationControl,
) -> ScriptRunResult {
    if statements.is_empty() {
        return ScriptRunResult::Completed(Vec::new());
    }
    let mut out = Vec::with_capacity(statements.len());
    let mut aborted = false;
    for sql in statements.into_iter() {
        let preview = sql_preview(&sql);
        if aborted {
            out.push(StatementOutcome {
                sql_preview: preview,
                elapsed_ms: 0,
                kind: StatementOutcomeKind::NotRun,
            });
            continue;
        }
        let started = std::time::Instant::now();
        let kind = match conn.query_controlled(&sql, control).await {
            Ok(qr) => StatementOutcomeKind::Rows(qr),
            Err(DriverError::Cancelled) => return ScriptRunResult::Cancelled,
            Err(DriverError::TimedOut) => return ScriptRunResult::TimedOut,

            Err(e) => {
                aborted = true;
                StatementOutcomeKind::Error(crate::ui::error_text::driver_message(&e))
            }
        };
        out.push(StatementOutcome {
            sql_preview: preview,
            elapsed_ms: started.elapsed().as_millis(),
            kind,
        });
    }
    ScriptRunResult::Completed(out)
}

fn sql_preview(sql: &str) -> String {
    let single_line: String = sql.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.chars().count() > 60 {
        let prefix: String = single_line.chars().take(60).collect();
        format!("{prefix}…")
    } else {
        single_line
    }
}

pub(crate) fn summary_label(n_total: usize, n_ok: usize, total_ms: u128, has_error: bool) -> String {
    if n_total == 1 {
        let ms = total_ms.to_string();
        if has_error {
            crate::tr!("error in {ms} ms").replace("{ms}", &ms)
        } else {
            crate::tr!("done in {ms} ms").replace("{ms}", &ms)
        }
    } else {
        let ok_s = n_ok.to_string();
        let total_s = n_total.to_string();
        let ms = total_ms.to_string();
        let base = crate::tr!("{ok}/{total} statements · {ms} ms")
            .replace("{ok}", &ok_s)
            .replace("{total}", &total_s)
            .replace("{ms}", &ms);
        if has_error {
            format!("{base} · {}", crate::tr!("error"))
        } else {
            base
        }
    }
}

fn build_outcome_widget(o: &StatementOutcome, idx: usize) -> gtk::Widget {
    match &o.kind {
        StatementOutcomeKind::Rows(result) if !result.rows.is_empty() => {
            let (column_view, _selection) = build_column_view(
                result,
                &result.columns,
                "",
                None,
                None,
                None,
                None,
                TabGridContext::default(),
            );
            let scrolled = gtk::ScrolledWindow::builder()
                .child(&column_view)
                .hexpand(true)
                .vexpand(true)
                .build();
            scrolled.upcast()
        }
        StatementOutcomeKind::Rows(_) => {
            let ms = o.elapsed_ms.to_string();
            adw::StatusPage::builder()
                .title(crate::tr!("Statement {n} executed").replace("{n}", &(idx + 1).to_string()))
                .description(crate::tr!("No rows returned · {ms} ms").replace("{ms}", &ms))
                .icon_name("emblem-default-symbolic")
                .vexpand(true)
                .build()
                .upcast()
        }
        StatementOutcomeKind::Error(msg) => adw::StatusPage::builder()
            .title(crate::tr!("Statement {n} failed").replace("{n}", &(idx + 1).to_string()))
            .description(msg)
            .icon_name("dialog-error-symbolic")
            .vexpand(true)
            .build()
            .upcast(),
        StatementOutcomeKind::NotRun => adw::StatusPage::builder()
            .title(crate::tr!("Statement {n} not run").replace("{n}", &(idx + 1).to_string()))
            .description(crate::tr!("Skipped because an earlier statement failed."))
            .icon_name("media-playback-stop-symbolic")
            .vexpand(true)
            .build()
            .upcast(),
    }
}

fn outcome_tab_label(idx: usize, o: &StatementOutcome) -> String {
    match &o.kind {
        StatementOutcomeKind::Rows(qr) => {
            let n_str = qr.rows.len().to_string();
            crate::tr!("Result {n} ({rows})")
                .replace("{n}", &(idx + 1).to_string())
                .replace("{rows}", &n_str)
        }
        StatementOutcomeKind::Error(_) => crate::tr!("Result {n} (error)").replace("{n}", &(idx + 1).to_string()),
        StatementOutcomeKind::NotRun => crate::tr!("Result {n} (skipped)").replace("{n}", &(idx + 1).to_string()),
    }
}

pub(crate) fn render_outcomes(holder: &gtk::Box, outcomes: &[StatementOutcome]) {
    if outcomes.is_empty() {
        let placeholder = adw::StatusPage::builder()
            .title(crate::tr!("Empty query"))
            .description(crate::tr!("Type a SQL statement and press Run."))
            .icon_name("text-x-generic-symbolic")
            .vexpand(true)
            .build();
        holder.append(&placeholder);
        return;
    }
    if outcomes.len() == 1 {
        let widget = build_outcome_widget(&outcomes[0], 0);
        holder.append(&widget);
        return;
    }
    let stack = adw::ViewStack::new();
    for (idx, o) in outcomes.iter().enumerate() {
        let widget = build_outcome_widget(o, idx);
        let icon = match &o.kind {
            StatementOutcomeKind::Rows(_) => "view-grid-symbolic",
            StatementOutcomeKind::Error(_) => "dialog-error-symbolic",
            StatementOutcomeKind::NotRun => "emblem-synchronizing-symbolic",
        };
        let page = stack.add_titled_with_icon(&widget, Some(&format!("r{idx}")), &outcome_tab_label(idx, o), icon);
        if !o.sql_preview.is_empty() {
            widget.set_tooltip_text(Some(&o.sql_preview));
            let _ = page;
        }
    }
    let switcher = adw::ViewSwitcher::builder()
        .stack(&stack)
        .policy(adw::ViewSwitcherPolicy::Wide)
        .build();
    let switcher_holder = gtk::CenterBox::builder()
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(12)
        .margin_end(12)
        .build();
    switcher_holder.set_center_widget(Some(&switcher));
    holder.append(&switcher_holder);
    holder.append(&stack);
    if let Some(err_idx) = outcomes
        .iter()
        .position(|o| matches!(o.kind, StatementOutcomeKind::Error(_)))
    {
        stack.set_visible_child_name(&format!("r{err_idx}"));
    }
}

#[cfg(test)]
mod tests {
    use super::{sql_preview, summary_label};

    #[test]
    fn sql_preview_collapses_whitespace_and_truncates() {
        let preview = sql_preview("SELECT *\n  FROM   users\n  WHERE id = 1");
        assert_eq!(preview, "SELECT * FROM users WHERE id = 1");
    }

    #[test]
    fn sql_preview_appends_ellipsis_when_too_long() {
        let long = "SELECT col1, col2, col3, col4, col5, col6, col7, col8, col9 FROM users WHERE id = 1";
        let preview = sql_preview(long);
        assert!(preview.ends_with('…'));
        assert!(preview.chars().count() <= 61);
    }

    #[test]
    fn summary_label_single_statement_done() {
        let s = summary_label(1, 1, 42, false);
        assert!(s.contains("42"));
        assert!(!s.contains("/"));
    }

    #[test]
    fn summary_label_multi_statement_includes_counts() {
        let s = summary_label(3, 2, 100, true);
        assert!(s.contains("2/3"));
        assert!(s.contains("100"));
    }
}
