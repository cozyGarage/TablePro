use relm4::adw::prelude::*;
use relm4::gtk::gio;
use relm4::{ComponentController, ComponentSender, adw, gtk};

use tablepro_core::{ColumnInfo, KEYSET_OFFSET_THRESHOLD, QueryResult, keyset_order_by, keyset_where_clause};
use uuid::Uuid;

use crate::services::database_service;
use crate::ui::browse_tab::BrowseTabInput;

use super::{App, AppMsg, ExportFormat, OpenMode, render_json};

impl App {
    /// Sidebar click — routes via OpenMode (smart switch / new tab).
    pub(super) fn on_select_table(
        &mut self,
        schema: Option<String>,
        name: String,
        open_mode: OpenMode,
        sender: ComponentSender<Self>,
    ) {
        self.dispatch_select_table(schema, name, open_mode, sender);
    }

    /// Fire the SELECT * query for a specific browse tab. Result goes to
    /// the same tab via `AppMsg::RowsLoaded(tab_id, ...)`. Composes the
    /// SELECT from the tab's current sort + filter + pagination state.
    /// Filter and sort are server-side; the row window is rendered by
    /// `sql_dialect::build_order_and_pagination` because the syntax is
    /// dialect-specific. Past `KEYSET_OFFSET_THRESHOLD`, sequential Next
    /// uses a primary-key seek when PKs and a cursor are available.
    pub(super) fn fetch_browse_page(&self, tab_id: Uuid, sender: ComponentSender<Self>) {
        let (schema, table, offset, limit, sort, filter, columns, driver_id, keyset_cursor) = {
            let tabs = self.workspace_tabs.borrow();
            let Some(controller) = tabs.get(&tab_id).and_then(|t| t.browse_controller()) else {
                return;
            };
            let model = controller.model();
            (
                model.schema().map(str::to_owned),
                model.table().to_string(),
                model.current_offset(),
                model.page_size(),
                model.current_sort(),
                model.current_filter().clone(),
                model.columns().to_vec(),
                model.driver_id().to_string(),
                model.keyset_cursor().map(|v| v.to_vec()),
            )
        };

        let Some(conn) = database_service::instance().active() else {
            sender.input(AppMsg::LoadFailed(Some(tab_id), "no active connection".into()));
            return;
        };
        let order_by = resolved_order_by(&driver_id, &columns, sort);

        let where_result = tablepro_core::build_filter_where(&driver_id, &columns, &filter);
        let (where_sql, mut params) = match where_result {
            Ok(Some((sql, p))) => (Some(sql), p),
            Ok(None) => (None, Vec::new()),
            Err(e) => {
                sender.input(AppMsg::ShowToast(format!("{e}")));
                return;
            }
        };

        let pk_names: Vec<String> = columns
            .iter()
            .filter(|c| c.primary_key)
            .map(|c| c.name.clone())
            .collect();
        let use_keyset = offset >= KEYSET_OFFSET_THRESHOLD
            && sort.is_none()
            && !pk_names.is_empty()
            && keyset_cursor.as_ref().is_some_and(|c| c.len() == pk_names.len());

        let sender_clone = sender.clone();
        sender.command(move |_, shutdown| {
            shutdown
                .register(async move {
                    let result = if use_keyset {
                        let cursor = keyset_cursor.expect("guarded by use_keyset");
                        let qualified = match &schema {
                            Some(s) => format!(
                                "{}.{}",
                                tablepro_core::sql_dialect::quote_ident(&driver_id, s),
                                tablepro_core::sql_dialect::quote_ident(&driver_id, &table)
                            ),
                            None => tablepro_core::sql_dialect::quote_ident(&driver_id, &table),
                        };
                        let mut sql = format!("SELECT * FROM {qualified}");
                        let mut clauses: Vec<String> = Vec::new();
                        if let Some(w) = &where_sql {
                            clauses.push(w.clone());
                        }
                        let pk_refs: Vec<&str> = pk_names.iter().map(String::as_str).collect();
                        match keyset_where_clause(&driver_id, &pk_refs, &cursor, params.len()) {
                            Ok((ks, ks_params)) => {
                                clauses.push(ks);
                                params.extend(ks_params);
                            }
                            Err(e) => {
                                sender_clone.input(AppMsg::LoadFailed(Some(tab_id), e.to_string()));
                                return;
                            }
                        }
                        if !clauses.is_empty() {
                            sql.push_str(" WHERE ");
                            sql.push_str(&clauses.join(" AND "));
                        }
                        let order_sql = keyset_order_by(&driver_id, &pk_refs);
                        let order_inner = order_sql
                            .trim()
                            .strip_prefix("ORDER BY")
                            .map(str::trim)
                            .filter(|s| !s.is_empty());
                        sql.push_str(&tablepro_core::sql_dialect::build_order_and_pagination(
                            &driver_id,
                            order_inner,
                            limit,
                            0,
                        ));
                        conn.query_params(&sql, &params).await
                    } else if where_sql.is_none() && order_by.is_none() {
                        conn.fetch_rows(schema.as_deref(), &table, offset, limit).await
                    } else {
                        let qualified = match &schema {
                            Some(s) => format!(
                                "{}.{}",
                                tablepro_core::sql_dialect::quote_ident(&driver_id, s),
                                tablepro_core::sql_dialect::quote_ident(&driver_id, &table)
                            ),
                            None => tablepro_core::sql_dialect::quote_ident(&driver_id, &table),
                        };
                        let mut sql = format!("SELECT * FROM {qualified}");
                        if let Some(w) = &where_sql {
                            sql.push_str(" WHERE ");
                            sql.push_str(w);
                        }
                        sql.push_str(&tablepro_core::sql_dialect::build_order_and_pagination(
                            &driver_id,
                            order_by.as_deref(),
                            limit,
                            offset,
                        ));
                        conn.query_params(&sql, &params).await
                    };
                    match result {
                        Ok(query_result) => sender_clone.input(AppMsg::RowsLoaded(tab_id, offset, query_result)),
                        Err(e) => sender_clone.input(AppMsg::LoadFailed(
                            Some(tab_id),
                            crate::ui::error_text::driver_message(&e),
                        )),
                    }
                })
                .drop_on_shutdown()
        });
    }

    pub(super) fn fetch_browse_columns(&self, tab_id: Uuid, sender: ComponentSender<Self>) {
        let (schema, table) = {
            let tabs = self.workspace_tabs.borrow();
            let Some(controller) = tabs.get(&tab_id).and_then(|t| t.browse_controller()) else {
                return;
            };
            let model = controller.model();
            (model.schema().map(str::to_owned), model.table().to_string())
        };

        let Some(conn) = database_service::instance().active() else {
            return;
        };
        let sender_clone = sender.clone();
        sender.command(move |_, shutdown| {
            shutdown
                .register(async move {
                    match conn.fetch_columns(schema.as_deref(), &table).await {
                        Ok(columns) => sender_clone.input(AppMsg::ColumnsLoaded(tab_id, columns)),
                        Err(error) => sender_clone.input(AppMsg::LoadFailed(
                            Some(tab_id),
                            crate::ui::error_text::driver_message(&error),
                        )),
                    }
                })
                .drop_on_shutdown()
        });
    }

    pub(super) fn fetch_browse_row_count(&self, tab_id: Uuid, sender: ComponentSender<Self>) {
        let (schema, table, filter, columns, driver_id) = {
            let tabs = self.workspace_tabs.borrow();
            let Some(controller) = tabs.get(&tab_id).and_then(|t| t.browse_controller()) else {
                return;
            };
            let model = controller.model();
            (
                model.schema().map(str::to_owned),
                model.table().to_string(),
                model.current_filter().clone(),
                model.columns().to_vec(),
                model.driver_id().to_string(),
            )
        };

        let Some(conn) = database_service::instance().active() else {
            return;
        };

        let (where_sql, params) = match tablepro_core::build_filter_where(&driver_id, &columns, &filter) {
            Ok(Some((sql, p))) => (Some(sql), p),
            _ => (None, Vec::new()),
        };

        let sender_clone = sender.clone();
        sender.command(move |_, shutdown| {
            shutdown
                .register(async move {
                    let qualified = match schema {
                        Some(s) => format!(
                            "{}.{}",
                            tablepro_core::sql_dialect::quote_ident(&driver_id, &s),
                            tablepro_core::sql_dialect::quote_ident(&driver_id, &table)
                        ),
                        None => tablepro_core::sql_dialect::quote_ident(&driver_id, &table),
                    };
                    let mut sql = format!("SELECT COUNT(*) FROM {qualified}");
                    if let Some(w) = &where_sql {
                        sql.push_str(" WHERE ");
                        sql.push_str(w);
                    }
                    let qr_result = if where_sql.is_some() {
                        conn.query_params(&sql, &params).await
                    } else {
                        conn.query(&sql).await
                    };
                    if let Ok(qr) = qr_result
                        && let Some(row) = qr.rows.first()
                        && let Some(value) = row.first()
                    {
                        let count = match value {
                            tablepro_core::Value::Int(i) if *i >= 0 => Some(*i as u64),
                            tablepro_core::Value::Float(f) if *f >= 0.0 && f.is_finite() => Some(*f as u64),
                            tablepro_core::Value::Decimal(d) => d.to_string().parse::<u64>().ok(),
                            _ => None,
                        };
                        if let Some(count) = count {
                            sender_clone.input(AppMsg::RowCountLoaded(tab_id, count));
                        }
                    }
                })
                .drop_on_shutdown()
        });
    }

    pub(super) fn on_browse_columns_loaded(
        &self,
        tab_id: Uuid,
        columns: Vec<ColumnInfo>,
        sender: ComponentSender<Self>,
    ) {
        self.dispatch_to_tab(tab_id, BrowseTabInput::ColumnsLoaded(columns));
        // Page order depends on the primary-key metadata, and filters depend
        // on column types. Start both queries only after ColumnsLoaded has
        // updated the tab model.
        sender.input(AppMsg::FetchBrowseRowCount(tab_id));
        sender.input(AppMsg::FetchBrowsePage(tab_id));
    }

    pub(super) fn on_browse_rows_loaded(&self, tab_id: Uuid, offset: u64, result: QueryResult) {
        self.dispatch_to_tab(tab_id, BrowseTabInput::RowsLoaded { offset, result });
    }

    pub(super) fn on_browse_row_count_loaded(&self, tab_id: Uuid, count: u64) {
        self.dispatch_to_tab(tab_id, BrowseTabInput::RowCountLoaded(count));
    }

    pub(super) fn on_browse_load_failed(&mut self, tab_id: Option<Uuid>, msg: String) {
        match tab_id {
            Some(id) => self.dispatch_to_tab(id, BrowseTabInput::ShowError(msg)),
            None => {
                tracing::warn!(error = %msg, "app-level load failed");
                self.dismiss_loading_page();
                self.set_status_page(super::StatusKind::Error, &crate::tr!("Failed"), &msg);
            }
        }
    }

    pub(super) fn on_export(&self, format: ExportFormat) {
        let Some((schema, table)) = self.selected_browse_slot_table() else {
            self.show_toast(&crate::tr!("Nothing to export"));
            return;
        };
        let Some(active_id) = self.selected_browse_tab_id() else {
            self.show_toast(&crate::tr!("Nothing to export"));
            return;
        };

        let result = {
            let tabs = self.workspace_tabs.borrow();
            tabs.get(&active_id)
                .and_then(|t| t.browse_controller())
                .and_then(|c| c.model().snapshot())
        };
        let Some(result) = result else {
            self.show_toast(&crate::tr!("Nothing to export"));
            return;
        };
        if matches!(format, ExportFormat::Csv) {
            self.on_export_csv_page(schema, table, result);
            return;
        }
        let table_label = match &schema {
            Some(s) => format!("{s}.{table}"),
            None => table.clone(),
        };
        let suggested = format!("{table_label}.json");
        let filter = gtk::FileFilter::new();
        filter.set_name(Some(&crate::tr!("JSON files")));
        filter.add_mime_type("application/json");
        filter.add_suffix("json");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder()
            .title(crate::tr!("Export current page as JSON"))
            .modal(true)
            .initial_name(&suggested)
            .default_filter(&filter)
            .filters(&filters)
            .build();
        let parent = self.window.clone();
        let parent_for_alert = parent.clone();
        let toast_overlay = self.toast_overlay.clone();
        dialog.save(Some(&parent), gtk::gio::Cancellable::NONE, move |outcome| {
            let Ok(file) = outcome else { return };
            let Some(path) = file.path() else { return };
            let bytes = render_json(&result);
            match std::fs::write(&path, bytes) {
                Ok(()) => toast_overlay.add_toast(relm4::adw::Toast::new(
                    &crate::tr!("Exported the current page to {path}").replace("{path}", &path.display().to_string()),
                )),
                Err(e) => {
                    let alert = adw::AlertDialog::new(
                        Some(&crate::tr!("Couldn't export")),
                        Some(
                            &crate::tr!("Writing {path} failed: {error}")
                                .replace("{path}", &path.display().to_string())
                                .replace("{error}", &e.to_string()),
                        ),
                    );
                    alert.add_response("close", &crate::tr!("Close"));
                    alert.set_default_response(Some("close"));
                    alert.set_close_response("close");
                    alert.present(Some(&parent_for_alert));
                }
            }
        });
    }

    fn on_export_csv_page(&self, schema: Option<String>, table: String, result: QueryResult) {
        let table_label = match &schema {
            Some(s) => format!("{s}.{table}"),
            None => table.clone(),
        };
        let suggested = format!("{table_label}.csv");
        let filter = gtk::FileFilter::new();
        filter.set_name(Some(&crate::tr!("CSV files")));
        filter.add_mime_type("text/csv");
        filter.add_suffix("csv");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder()
            .title(crate::tr!("Export current page as CSV"))
            .modal(true)
            .initial_name(&suggested)
            .default_filter(&filter)
            .filters(&filters)
            .build();
        let parent = self.window.clone();
        let parent_for_alert = parent.clone();
        let toast_overlay = self.toast_overlay.clone();
        dialog.save(Some(&parent), gtk::gio::Cancellable::NONE, move |outcome| {
            let Ok(file) = outcome else { return };
            let Some(path) = file.path() else { return };
            let write_result = (|| {
                let mut output = std::fs::File::create(&path).map_err(|error| error.to_string())?;
                tablepro_core::export::write_csv_header(&mut output, &result.columns)
                    .map_err(|error| error.to_string())?;
                for row in &result.rows {
                    tablepro_core::export::write_csv_row(&mut output, row).map_err(|error| error.to_string())?;
                }
                Ok::<_, String>(result.rows.len())
            })();
            match write_result {
                Ok(n) => toast_overlay.add_toast(relm4::adw::Toast::new(
                    &crate::tr!("Exported {n} rows from the current page to {path}")
                        .replace("{n}", &n.to_string())
                        .replace("{path}", &path.display().to_string()),
                )),
                Err(error) => {
                    let alert = adw::AlertDialog::new(
                        Some(&crate::tr!("Couldn't export")),
                        Some(
                            &crate::tr!("Writing {path} failed: {error}")
                                .replace("{path}", &path.display().to_string())
                                .replace("{error}", &error),
                        ),
                    );
                    alert.add_response("close", &crate::tr!("Close"));
                    alert.set_default_response(Some("close"));
                    alert.set_close_response("close");
                    alert.present(Some(&parent_for_alert));
                }
            }
        });
    }

    /// Ctrl+F / Filter button — toggle the inline filter strip on
    /// the active Browse tab. Strip lives inside the tab (always
    /// constructed at init), so this is just a reveal flip.
    pub(super) fn on_show_filter_dialog(&self) {
        let Some(id) = self.selected_browse_tab_id() else {
            self.show_toast(&crate::tr!("Open a table to filter rows."));
            return;
        };
        self.dispatch_to_tab(id, BrowseTabInput::ToggleFilterStrip);
    }

    pub(super) fn on_refresh_active_tab(&self) {
        let Some(id) = self.selected_browse_tab_id() else {
            return;
        };
        let dirty = crate::services::change_tracker::with_tab_ref(id, |tr| tr.has_pending()).unwrap_or(false);
        if !dirty {
            self.dispatch_to_tab(id, BrowseTabInput::Refresh);
            return;
        }
        let dialog = adw::AlertDialog::new(
            Some(&crate::tr!("Discard pending changes?")),
            Some(&crate::tr!(
                "Refreshing reloads the table from the database and drops every unsaved edit on this tab."
            )),
        );
        dialog.add_response("cancel", &crate::tr!("Cancel"));
        dialog.add_response("discard", &crate::tr!("Discard and refresh"));
        dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let workspace_tabs = self.workspace_tabs.clone();
        dialog.connect_response(None, move |dlg: &adw::AlertDialog, response: &str| {
            dlg.close();
            if response == "discard" {
                crate::services::change_tracker::with_tab(id, |t| t.clear());
                if let Some(controller) = workspace_tabs.borrow().get(&id).and_then(|t| t.browse_controller()) {
                    let _ = controller.sender().send(BrowseTabInput::Refresh);
                }
            }
        });
        dialog.present(Some(&self.window));
    }
}

fn resolved_order_by(driver_id: &str, columns: &[ColumnInfo], sort: Option<(usize, bool)>) -> Option<String> {
    let mut terms = Vec::new();
    let selected = sort.and_then(|(index, ascending)| {
        columns.get(index).map(|column| {
            let direction = if ascending { "ASC" } else { "DESC" };
            terms.push(format!(
                "{} {direction}",
                tablepro_core::sql_dialect::quote_ident(driver_id, &column.name)
            ));
            column.name.as_str()
        })
    });
    for column in columns.iter().filter(|column| column.primary_key) {
        if selected == Some(column.name.as_str()) {
            continue;
        }
        terms.push(format!(
            "{} ASC",
            tablepro_core::sql_dialect::quote_ident(driver_id, &column.name)
        ));
    }
    (!terms.is_empty()).then(|| terms.join(", "))
}

#[cfg(test)]
mod tests {
    use super::resolved_order_by;
    use tablepro_core::ColumnInfo;

    fn column(name: &str, primary_key: bool) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            data_type: "text".into(),
            nullable: false,
            primary_key,
            is_auto_increment: false,
            default_value: None,
            is_generated: false,
        }
    }

    #[test]
    fn default_order_uses_every_primary_key_column() {
        let columns = vec![column("tenant", true), column("name", false), column("id", true)];
        assert_eq!(
            resolved_order_by("postgres", &columns, None).as_deref(),
            Some("\"tenant\" ASC, \"id\" ASC")
        );
    }

    #[test]
    fn explicit_sort_appends_primary_key_tie_breakers() {
        let columns = vec![column("tenant", true), column("name", false), column("id", true)];
        assert_eq!(
            resolved_order_by("postgres", &columns, Some((1, false))).as_deref(),
            Some("\"name\" DESC, \"tenant\" ASC, \"id\" ASC")
        );
    }

    #[test]
    fn sorted_primary_key_is_not_duplicated() {
        let columns = vec![column("tenant", true), column("id", true)];
        assert_eq!(
            resolved_order_by("postgres", &columns, Some((0, false))).as_deref(),
            Some("\"tenant\" DESC, \"id\" ASC")
        );
    }

    #[test]
    fn table_without_pk_or_sort_has_no_promised_order() {
        assert_eq!(resolved_order_by("postgres", &[column("name", false)], None), None);
    }
}
