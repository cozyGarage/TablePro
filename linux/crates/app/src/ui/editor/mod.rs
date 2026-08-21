mod completion;
mod outcomes;
mod schema;
mod sql_text;

use std::time::SystemTime;

use relm4::adw::prelude::*;
use relm4::gtk::glib;
use relm4::prelude::*;
use relm4::{adw, gtk};
use sourceview5::prelude::*;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use tablepro_core::{OperationControl, QueryResult};
use tablepro_storage::query_history::{self, NewEntry, Outcome};

use crate::services::database_service::{self, ConnectionMetadata};

pub use completion::{SchemaIndex, candidate_words, referenced_tables, table_key};
pub use schema::{SQL_KEYWORDS, build_schema_buffer, derive_tab_label, update_schema_buffer};

use outcomes::{ScriptRunResult, clear_box, render_outcomes, run_statements, summary_label};
use schema::{apply_editor_font_size, apply_editor_scheme};
use sql_text::{split_sql_statements, statement_at_cursor, toggle_line_comment};

pub struct SqlEditor {
    source_view: sourceview5::View,
    run_button: gtk::Button,
    cancel_button: gtk::Button,
    running_spinner: gtk::Spinner,
    results_holder: gtk::Box,
    status: gtk::Label,
    cancel_token: Option<CancellationToken>,
    executing_sql: Option<String>,
    executing_metadata: Option<ConnectionMetadata>,
    executing_started_at: Option<SystemTime>,
    connection_id: Option<Uuid>,
}

pub struct SqlEditorInit {
    pub schema_buffer: gtk::TextBuffer,
    pub schema_index: std::rc::Rc<std::cell::RefCell<SchemaIndex>>,
    pub initial_query: Option<String>,
    pub connection_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct StatementOutcome {
    pub sql_preview: String,
    pub elapsed_ms: u128,
    pub kind: StatementOutcomeKind,
}

#[derive(Debug, Clone)]
pub enum StatementOutcomeKind {
    Rows(QueryResult),
    Error(String),
    NotRun,
}

#[derive(Debug)]
pub enum SqlEditorInput {
    Run,
    RunWithParameters {
        sql: String,
        values: std::collections::HashMap<String, tablepro_core::Value>,
    },
    Cancel,
    ShowOutcomes(Vec<StatementOutcome>),
    ShowCancelled,
    ShowTimedOut(u32),
    ReplaceQuery(String),
    Format,
    RunAtCursor,
    ToggleLineComment,
    Explain,
}

#[derive(Debug)]
pub enum SqlEditorOutput {
    RunStateChanged(bool),
    QueryChanged(String),
    NeedColumns(Vec<String>),
}

#[relm4::component(pub)]
impl SimpleComponent for SqlEditor {
    type Init = SqlEditorInit;
    type Input = SqlEditorInput;
    type Output = SqlEditorOutput;

    view! {
        adw::ToolbarView {
            add_top_bar = &gtk::Box {
                set_orientation: gtk::Orientation::Horizontal,
                set_spacing: 8,
                set_margin_top: 8,
                set_margin_bottom: 8,
                set_margin_start: 8,
                set_margin_end: 8,

                gtk::Box {
                    set_hexpand: true,
                },

                #[name = "cursor_info"]
                gtk::Label {
                    set_halign: gtk::Align::End,
                    add_css_class: "dim-label",
                    add_css_class: "monospace",
                    set_margin_end: 8,
                },

                #[name = "running_spinner"]
                gtk::Spinner {
                    set_visible: false,
                    set_spinning: true,
                    set_size_request: (20, 20),
                },

                #[name = "status"]
                gtk::Label {
                    set_halign: gtk::Align::End,
                    add_css_class: "dim-label",
                },

                #[name = "cancel_button"]
                gtk::Button {
                    set_label: &crate::tr!("Cancel"),
                    set_tooltip_text: Some(crate::tr!("Cancel running query (Esc)").as_str()),
                    set_visible: false,
                    add_css_class: "flat",
                    connect_clicked => SqlEditorInput::Cancel,
                },

                #[name = "explain_button"]
                gtk::Button {
                    set_label: &crate::tr!("Explain"),
                    set_tooltip_text: Some(crate::tr!("Explain query plan").as_str()),
                    add_css_class: "flat",
                    connect_clicked => SqlEditorInput::Explain,
                },

                #[name = "run_button"]
                gtk::Button {
                    set_label: &crate::tr!("Run"),
                    set_tooltip_text: Some(crate::tr!("Run query (Ctrl+Return)").as_str()),
                    add_css_class: "suggested-action",
                    connect_clicked => SqlEditorInput::Run,
                },
            },

            #[wrap(Some)]
            set_content = &gtk::Paned {
                set_orientation: gtk::Orientation::Vertical,
                set_position: 280,
                set_vexpand: true,
                set_hexpand: true,

                #[wrap(Some)]
                set_start_child = &gtk::ScrolledWindow {
                    set_min_content_height: 200,

                    #[wrap(Some)]
                    #[name = "source_view"]
                    set_child = &sourceview5::View {
                        set_show_line_numbers: true,
                        set_monospace: true,
                        set_auto_indent: true,
                        set_highlight_current_line: true,
                        set_tab_width: 4,
                        set_top_margin: 8,
                        set_bottom_margin: 8,
                        set_left_margin: 8,
                        set_right_margin: 8,
                    },
                },

                #[wrap(Some)]
                #[name = "results_holder"]
                set_end_child = &gtk::Box {
                    set_orientation: gtk::Orientation::Vertical,
                },
            },
        }
    }

    fn init(init: Self::Init, _root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let widgets = view_output!();

        let lang_manager = sourceview5::LanguageManager::default();
        let initial_text = init.initial_query.unwrap_or_else(|| "SELECT 1;".to_string());
        if let Some(lang) = lang_manager.language("sql") {
            let buffer = sourceview5::Buffer::with_language(&lang);
            buffer.set_text(&initial_text);
            widgets.source_view.set_buffer(Some(&buffer));
        } else {
            widgets.source_view.buffer().set_text(&initial_text);
        }
        apply_editor_scheme(&widgets.source_view);
        let view_for_theme = widgets.source_view.clone();
        adw::StyleManager::default().connect_dark_notify(move |_| {
            apply_editor_scheme(&view_for_theme);
        });

        let font_size = crate::services::preferences::load().editor_font_size;
        apply_editor_font_size(&widgets.source_view, font_size);

        let provider = sourceview5::CompletionWords::new(Some("SQL"));
        provider.register(&init.schema_buffer);
        if let Ok(view_buffer) = widgets.source_view.buffer().downcast::<sourceview5::Buffer>() {
            provider.register(&view_buffer);
        }
        let completion = widgets.source_view.completion();
        completion.add_provider(&provider);

        let cursor_info = widgets.cursor_info.clone();
        let view_for_cursor = widgets.source_view.clone();
        let update_cursor = move || {
            let buffer = view_for_cursor.buffer();
            let mark = buffer.get_insert();
            let iter = buffer.iter_at_mark(&mark);
            let line = iter.line() + 1;
            let col = iter.line_offset() + 1;
            cursor_info.set_label(&format!("Ln {line}, Col {col}"));
        };
        update_cursor();
        widgets
            .source_view
            .buffer()
            .connect_cursor_position_notify(move |_| update_cursor());

        let refresh_completion = build_completion_refresh(
            widgets.source_view.clone(),
            init.schema_buffer.clone(),
            init.schema_index.clone(),
            sender.clone(),
        );
        refresh_completion();
        let refresh_on_cursor = refresh_completion.clone();
        widgets
            .source_view
            .buffer()
            .connect_cursor_position_notify(move |_| refresh_on_cursor());

        let view_for_change = widgets.source_view.clone();
        let sender_for_change = sender.clone();
        let refresh_on_change = refresh_completion.clone();
        widgets.source_view.buffer().connect_changed(move |_| {
            let buffer = view_for_change.buffer();
            let (start, end) = buffer.bounds();
            let text = buffer.text(&start, &end, false).to_string();
            let _ = sender_for_change.output(SqlEditorOutput::QueryChanged(text));
            refresh_on_change();
        });

        let run_shortcut = gtk::Shortcut::builder()
            .trigger(&crate::ui::shortcut::parse("<Primary>Return"))
            .action(&gtk::CallbackAction::new({
                let sender = sender.clone();
                move |_, _| {
                    sender.input(SqlEditorInput::Run);
                    glib::Propagation::Stop
                }
            }))
            .build();
        let cancel_shortcut = gtk::Shortcut::builder()
            .trigger(&crate::ui::shortcut::parse("Escape"))
            .action(&gtk::CallbackAction::new({
                let sender = sender.clone();
                move |_, _| {
                    sender.input(SqlEditorInput::Cancel);
                    glib::Propagation::Stop
                }
            }))
            .build();
        let format_shortcut = gtk::Shortcut::builder()
            .trigger(&crate::ui::shortcut::parse("<Primary><Shift>f"))
            .action(&gtk::CallbackAction::new({
                let sender = sender.clone();
                move |_, _| {
                    sender.input(SqlEditorInput::Format);
                    glib::Propagation::Stop
                }
            }))
            .build();
        let run_at_cursor_shortcut = gtk::Shortcut::builder()
            .trigger(&crate::ui::shortcut::parse("<Primary><Shift>Return"))
            .action(&gtk::CallbackAction::new({
                let sender = sender.clone();
                move |_, _| {
                    sender.input(SqlEditorInput::RunAtCursor);
                    glib::Propagation::Stop
                }
            }))
            .build();
        let toggle_comment_shortcut = gtk::Shortcut::builder()
            .trigger(&crate::ui::shortcut::parse("<Primary>slash"))
            .action(&gtk::CallbackAction::new({
                let sender = sender.clone();
                move |_, _| {
                    sender.input(SqlEditorInput::ToggleLineComment);
                    glib::Propagation::Stop
                }
            }))
            .build();
        let controller = gtk::ShortcutController::new();
        controller.add_shortcut(run_shortcut);
        controller.add_shortcut(cancel_shortcut);
        controller.add_shortcut(format_shortcut);
        controller.add_shortcut(run_at_cursor_shortcut);
        controller.add_shortcut(toggle_comment_shortcut);
        widgets.source_view.add_controller(controller);

        let drop_target = gtk::DropTarget::new(gtk::gio::File::static_type(), gtk::gdk::DragAction::COPY);
        let view_for_drop = widgets.source_view.clone();
        drop_target.connect_drop(move |_, value, _, _| {
            if let Ok(file) = value.get::<gtk::gio::File>()
                && let Some(path) = file.path()
                && let Ok(text) = std::fs::read_to_string(&path)
            {
                let buffer = view_for_drop.buffer();
                let (start, end) = buffer.bounds();
                let existing_empty = buffer.text(&start, &end, false).trim().is_empty();
                if existing_empty {
                    buffer.set_text(&text);
                } else {
                    buffer.insert_at_cursor(&text);
                }
                return true;
            }
            false
        });
        widgets.source_view.add_controller(drop_target);

        let model = SqlEditor {
            source_view: widgets.source_view.clone(),
            run_button: widgets.run_button.clone(),
            cancel_button: widgets.cancel_button.clone(),
            running_spinner: widgets.running_spinner.clone(),
            results_holder: widgets.results_holder.clone(),
            status: widgets.status.clone(),
            cancel_token: None,
            executing_sql: None,
            executing_metadata: None,
            executing_started_at: None,
            connection_id: init.connection_id,
        };
        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            SqlEditorInput::Run => {
                let buffer = self.source_view.buffer();
                let (start, end) = buffer.bounds();
                let sql = buffer.text(&start, &end, false).to_string();
                let trimmed = sql.trim().to_string();
                if trimmed.is_empty() {
                    self.status.set_label(&crate::tr!("empty query"));
                    return;
                }
                self.begin_run(trimmed, sender);
            }

            SqlEditorInput::RunWithParameters { sql, values } => {
                self.execute_sql(sql, values, sender);
            }

            SqlEditorInput::ToggleLineComment => {
                toggle_line_comment(&self.source_view.buffer());
            }

            SqlEditorInput::Explain => {
                let buffer = self.source_view.buffer();
                let text = if let Some((a, b)) = buffer.selection_bounds() {
                    buffer.text(&a, &b, true).to_string()
                } else {
                    let (start, end) = buffer.bounds();
                    buffer.text(&start, &end, true).to_string()
                };
                if let Some(window) = self.source_view.root().and_then(|r| r.downcast::<gtk::Window>().ok()) {
                    crate::ui::explain_dialog::present(&window, self.connection_id, &text);
                }
            }

            SqlEditorInput::RunAtCursor => {
                let buffer = self.source_view.buffer();
                let (start, end) = buffer.bounds();
                let sql = buffer.text(&start, &end, false).to_string();
                let cursor_chars = buffer.iter_at_mark(&buffer.get_insert()).offset() as usize;
                let cursor_byte: usize = sql.chars().take(cursor_chars).map(char::len_utf8).sum();
                let Some(statement) = statement_at_cursor(&sql, cursor_byte) else {
                    self.status.set_label(&crate::tr!("No statement at cursor"));
                    return;
                };
                self.begin_run(statement, sender);
            }

            SqlEditorInput::Cancel => {
                if let Some(token) = self.cancel_token.take() {
                    token.cancel();
                }
            }

            SqlEditorInput::ShowOutcomes(outcomes) => {
                self.cancel_token = None;
                self.run_button.set_sensitive(true);
                self.cancel_button.set_visible(false);
                self.running_spinner.set_visible(false);
                let _ = sender.output(SqlEditorOutput::RunStateChanged(false));

                let total_ms: u128 = outcomes.iter().map(|o| o.elapsed_ms).sum();
                let n_total = outcomes.len();
                let n_ok = outcomes
                    .iter()
                    .filter(|o| matches!(o.kind, StatementOutcomeKind::Rows(_)))
                    .count();
                let first_error = outcomes.iter().find_map(|o| match &o.kind {
                    StatementOutcomeKind::Error(msg) => Some(msg.clone()),
                    _ => None,
                });

                let total_rows: i64 = outcomes
                    .iter()
                    .filter_map(|o| match &o.kind {
                        StatementOutcomeKind::Rows(qr) => Some(qr.rows.len() as i64),
                        _ => None,
                    })
                    .sum();
                let history_outcome = match &first_error {
                    Some(msg) => Outcome::Error(msg.clone()),
                    None => Outcome::Success,
                };
                let rows_for_history = if total_rows > 0 { Some(total_rows) } else { None };
                self.record_history(total_ms as i64, rows_for_history, history_outcome);

                self.status
                    .set_label(&summary_label(n_total, n_ok, total_ms, first_error.is_some()));
                clear_box(&self.results_holder);
                render_outcomes(&self.results_holder, &outcomes);
            }

            SqlEditorInput::ShowCancelled => {
                self.cancel_token = None;
                self.run_button.set_sensitive(true);
                self.cancel_button.set_visible(false);
                self.running_spinner.set_visible(false);
                let _ = sender.output(SqlEditorOutput::RunStateChanged(false));
                let elapsed = self
                    .executing_started_at
                    .and_then(|t| SystemTime::now().duration_since(t).ok())
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                self.record_history(elapsed, None, Outcome::Cancelled);
                self.status.set_label(&crate::tr!("cancelled"));
                clear_box(&self.results_holder);
                let cancelled_page = adw::StatusPage::builder()
                    .title(crate::tr!("Query cancelled"))
                    .description(crate::tr!("The running query was stopped."))
                    .icon_name("process-stop-symbolic")
                    .vexpand(true)
                    .build();
                self.results_holder.append(&cancelled_page);
            }

            SqlEditorInput::ShowTimedOut(secs) => {
                self.cancel_token = None;
                self.run_button.set_sensitive(true);
                self.cancel_button.set_visible(false);
                self.running_spinner.set_visible(false);
                let _ = sender.output(SqlEditorOutput::RunStateChanged(false));
                let elapsed = self
                    .executing_started_at
                    .and_then(|t| SystemTime::now().duration_since(t).ok())
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                let secs_str = secs.to_string();
                let reason =
                    crate::tr!("Query exceeded the {n}s timeout configured in Preferences.").replace("{n}", &secs_str);
                self.record_history(elapsed, None, Outcome::Error(reason.clone()));
                self.status.set_label(&crate::tr!("timed out"));
                clear_box(&self.results_holder);
                let page = adw::StatusPage::builder()
                    .title(crate::tr!("Query timed out"))
                    .description(&reason)
                    .icon_name("dialog-warning-symbolic")
                    .vexpand(true)
                    .build();
                self.results_holder.append(&page);
            }

            SqlEditorInput::ReplaceQuery(text) => {
                self.source_view.buffer().set_text(&text);
            }

            SqlEditorInput::Format => {
                let buffer = self.source_view.buffer();
                let (start, end) = buffer.bounds();
                let text = buffer.text(&start, &end, false).to_string();
                if text.trim().is_empty() {
                    return;
                }
                let opts = sqlformat::FormatOptions {
                    indent: sqlformat::Indent::Spaces(4),
                    uppercase: Some(true),
                    lines_between_queries: 2,
                    ..sqlformat::FormatOptions::default()
                };
                let formatted = sqlformat::format(&text, &sqlformat::QueryParams::None, &opts);
                if formatted == text {
                    return;
                }
                buffer.set_text(&formatted);
            }
        }
    }
}

impl SqlEditor {
    fn connection(&self) -> Option<std::sync::Arc<dyn tablepro_core::Connection>> {
        database_service::instance().get(self.connection_id?)
    }

    fn metadata(&self) -> Option<ConnectionMetadata> {
        database_service::instance().metadata(self.connection_id?)
    }

    fn begin_run(&mut self, sql: String, sender: ComponentSender<Self>) {
        let driver_id = self.metadata().map(|metadata| metadata.driver_id).unwrap_or_default();
        let names = crate::services::query_parameters::statement_names(&sql, &driver_id);
        if names.is_empty() {
            self.execute_sql(sql, std::collections::HashMap::new(), sender);
            return;
        }
        let Some(window) = self
            .source_view
            .root()
            .and_then(|root| root.downcast::<gtk::Window>().ok())
        else {
            self.status
                .set_label(&crate::tr!("Cannot ask for parameter values without a window"));
            return;
        };
        crate::ui::parameters_dialog::present(&window, &names, move |values| {
            sender.input(SqlEditorInput::RunWithParameters {
                sql: sql.clone(),
                values,
            });
        });
    }

    fn execute_sql(
        &mut self,
        trimmed: String,
        parameter_values: std::collections::HashMap<String, tablepro_core::Value>,
        sender: ComponentSender<Self>,
    ) {
        let conn = match self.connection() {
            Some(c) => c,
            None => {
                self.status.set_label(&crate::tr!("no active connection"));
                return;
            }
        };

        if let Some(prev) = self.cancel_token.take() {
            prev.cancel();
        }
        let token = CancellationToken::new();
        self.cancel_token = Some(token.clone());

        self.run_button.set_sensitive(false);
        self.cancel_button.set_visible(true);
        self.running_spinner.set_visible(true);
        self.status.set_label(&crate::tr!("Running…"));
        clear_box(&self.results_holder);
        let _ = sender.output(SqlEditorOutput::RunStateChanged(true));

        self.executing_sql = Some(trimmed.clone());
        self.executing_metadata = self.metadata();
        self.executing_started_at = Some(SystemTime::now());

        let timeout_secs = crate::services::preferences::load().query_timeout_secs;
        let driver_id = self
            .executing_metadata
            .as_ref()
            .map(|metadata| metadata.driver_id.clone())
            .unwrap_or_default();
        let sender_clone = sender.clone();
        sender.command(move |_, shutdown| {
            shutdown
                .register(async move {
                    let statements = split_sql_statements(&trimmed);
                    let deadline = (timeout_secs > 0)
                        .then(|| tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_secs as u64));
                    let control = OperationControl::new(token, deadline);
                    let msg = match run_statements(conn, statements, &driver_id, &parameter_values, &control).await {
                        ScriptRunResult::Cancelled => SqlEditorInput::ShowCancelled,
                        ScriptRunResult::TimedOut => SqlEditorInput::ShowTimedOut(timeout_secs),
                        ScriptRunResult::Completed(outcomes) => {
                            let total_ms: u128 = outcomes.iter().map(|o| o.elapsed_ms).sum();
                            let n_ok = outcomes
                                .iter()
                                .filter(|o| matches!(o.kind, StatementOutcomeKind::Rows(_)))
                                .count();
                            let n_err = outcomes
                                .iter()
                                .filter(|o| matches!(o.kind, StatementOutcomeKind::Error(_)))
                                .count();
                            tracing::info!(n_ok, n_err, total_ms, "script run complete");
                            SqlEditorInput::ShowOutcomes(outcomes)
                        }
                    };
                    sender_clone.input(msg);
                })
                .drop_on_shutdown()
        });
    }

    fn record_history(&mut self, duration_ms: i64, rows_affected: Option<i64>, outcome: Outcome) {
        let (Some(query), Some(metadata), Some(started_at)) = (
            self.executing_sql.take(),
            self.executing_metadata.take(),
            self.executing_started_at.take(),
        ) else {
            return;
        };
        let entry = NewEntry {
            query,
            driver_id: metadata.driver_id,
            connection_id: metadata.id,
            connection_name: metadata.name,
            executed_at: started_at,
            duration_ms: Some(duration_ms),
            rows_affected,
            outcome,
        };
        relm4::spawn(async move {
            if let Err(e) = query_history::record(entry).await {
                tracing::warn!(error = %e, "history record failed");
            }
        });
    }
}

fn build_completion_refresh(
    view: sourceview5::View,
    schema_buffer: gtk::TextBuffer,
    schema_index: std::rc::Rc<std::cell::RefCell<SchemaIndex>>,
    sender: ComponentSender<SqlEditor>,
) -> std::rc::Rc<dyn Fn()> {
    std::rc::Rc::new(move || {
        let buffer = view.buffer();
        let (start, end) = buffer.bounds();
        let sql = buffer.text(&start, &end, false).to_string();
        let cursor_chars = buffer.iter_at_mark(&buffer.get_insert()).offset() as usize;
        let cursor_byte: usize = sql.chars().take(cursor_chars).map(char::len_utf8).sum();
        let Ok(index) = schema_index.try_borrow() else {
            return;
        };
        let words = candidate_words(&sql, cursor_byte, &index);
        let missing: Vec<String> = referenced_tables(&sql, cursor_byte)
            .into_iter()
            .filter(|table| !index.knows_columns(table))
            .collect();
        drop(index);
        update_schema_buffer(&schema_buffer, &words);
        if !missing.is_empty() {
            let _ = sender.output(SqlEditorOutput::NeedColumns(missing));
        }
    })
}
