use relm4::{ComponentController, ComponentSender};

use super::{App, AppMsg};

impl App {
    pub(super) fn on_editor_needs_columns(&self, tables: Vec<String>, sender: ComponentSender<Self>) {
        let Some(conn) = self.window_connection() else {
            return;
        };
        let pending: Vec<crate::ui::editor::SchemaRequest> = {
            let mut index = self.schema_index.borrow_mut();
            if index.sync_connection(&conn) {
                self.requested_columns.borrow_mut().clear();
            }
            let mut requested = self.requested_columns.borrow_mut();
            tables
                .into_iter()
                .filter(|table| !index.knows_columns(table))
                .filter(|table| requested.insert(crate::ui::editor::table_key(table)))
                .map(|table| index.request(table))
                .collect()
        };
        if pending.is_empty() {
            return;
        };
        let timeout_secs = crate::services::operation_control::configured_timeout_secs();
        let sender_clone = sender.clone();
        sender.command(move |_, shutdown| {
            shutdown
                .register(async move {
                    for request in pending {
                        let (schema, table) = split_table_reference(&request.table);
                        let control = crate::services::operation_control::bounded(timeout_secs);
                        let result = conn
                            .fetch_columns_controlled(schema.as_deref(), &table, &control)
                            .await
                            .map(|columns| columns.into_iter().map(|column| column.name).collect())
                            .map_err(|_| ());
                        sender_clone.input(AppMsg::SchemaColumnsFetched(request, result));
                    }
                })
                .drop_on_shutdown()
        });
    }

    pub(super) fn on_schema_columns_fetched(
        &self,
        request: crate::ui::editor::SchemaRequest,
        columns: Result<Vec<String>, ()>,
    ) {
        let key = crate::ui::editor::table_key(&request.table);
        let mut index = self.schema_index.borrow_mut();
        if !index.accepts(&request) {
            return;
        }
        match columns {
            Ok(columns) => index.set_columns(&request.table, columns),
            Err(()) => {
                self.requested_columns.borrow_mut().remove(&key);
            }
        }
    }

    pub(super) fn rebuild_schema_buffer(&self) {
        if let Some(connection) = self.window_connection()
            && self.schema_index.borrow_mut().sync_connection(&connection)
        {
            self.requested_columns.borrow_mut().clear();
        }
        let mut words: Vec<String> = self.table_names.clone();
        let tabs = self.workspace_tabs.borrow();
        for tab in tabs.values() {
            if let Some(controller) = tab.browse_controller() {
                for col in controller.model().columns() {
                    words.push(col.name.clone());
                }
            }
        }
        words.sort_unstable();
        words.dedup();
        crate::ui::editor::update_schema_buffer(&self.schema_buffer, &words);

        let mut index = self.schema_index.borrow_mut();
        index.tables = self.table_names.clone();
        for tab in tabs.values() {
            if let Some(controller) = tab.browse_controller() {
                let model = controller.model();
                let columns: Vec<String> = model.columns().iter().map(|column| column.name.clone()).collect();
                if columns.is_empty() {
                    continue;
                }
                let reference = match model.schema() {
                    Some(schema) => format!("{schema}.{}", model.table()),
                    None => model.table().to_string(),
                };
                index.set_columns(&reference, columns);
            }
        }
    }
}

fn split_table_reference(reference: &str) -> (Option<String>, String) {
    let cleaned = reference.replace(['"', '`', '[', ']'], "");
    match cleaned.rsplit_once('.') {
        Some((schema, table)) if !schema.is_empty() && !table.is_empty() => {
            (Some(schema.to_string()), table.to_string())
        }
        _ => (None, cleaned),
    }
}

#[cfg(test)]
mod tests {
    use super::split_table_reference;

    #[test]
    fn a_qualified_reference_splits_into_schema_and_table() {
        assert_eq!(
            split_table_reference("public.users"),
            (Some("public".to_string()), "users".to_string())
        );
        assert_eq!(
            split_table_reference("\"public\".\"Users\""),
            (Some("public".to_string()), "Users".to_string())
        );
    }

    #[test]
    fn a_bare_reference_has_no_schema() {
        assert_eq!(split_table_reference("users"), (None, "users".to_string()));
        assert_eq!(split_table_reference("`orders`"), (None, "orders".to_string()));
    }

    #[test]
    fn a_trailing_dot_is_not_a_schema_split() {
        assert_eq!(split_table_reference("users."), (None, "users.".to_string()));
    }
}
