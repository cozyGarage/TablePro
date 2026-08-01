use relm4::ComponentSender;

use super::*;

impl BrowseTab {
    pub(super) fn handle_duplicate_row(&mut self, row_position: u32, sender: ComponentSender<Self>) {
        if self.current_columns.is_empty() {
            return;
        }
        // Read the source row through the live RowObject at
        // `row_position` — NOT by indexing
        // `current_result.rows` directly. The grid's
        // row_position reflects the user's current sort and
        // any prepended drafts; raw `rows` is fetch order. A
        // sort would otherwise hand us the wrong row's cells.
        let Some(source) = self.row_object_at(row_position) else {
            return;
        };
        let source_cells = source.cells_clone();
        // Clone source values; blank columns whose value is
        // owned by the database (PK, identity / serial,
        // generated). The duplicate is meant to be a *new*
        // row — inheriting the source's identity would either
        // collide on save or pre-fill nonsense.
        let values: Vec<Value> = self
            .current_columns
            .iter()
            .enumerate()
            .map(|(i, col)| {
                if col.primary_key || col.is_auto_increment || col.is_generated {
                    Value::Null
                } else {
                    source_cells.get(i).cloned().unwrap_or(Value::Null)
                }
            })
            .collect();
        let key_opt = crate::services::change_tracker::with_tab(self.tab_id, |t| t.track_insert(values.clone()));
        let Some(key) = key_opt else {
            return;
        };
        let crate::services::change_tracker::RowKey::Draft(draft_id) = key else {
            return;
        };
        if let Some(store) = self.list_store() {
            let draft_row = crate::ui::row_object::RowObject::new_draft(draft_id, values);
            store.insert(0, &draft_row);
        }
        self.refresh_inner_stack_visibility();
        // Scroll + focus deferred via FocusInsertedDraft — see
        // the matching note in the `InsertRow` arm above. Same
        // `bounds.y == 0` assertion fires if `cv.scroll_to`
        // lands in the same tick as a stack flip.
        sender.input(BrowseTabInput::FocusInsertedDraft);
    }

    pub(super) fn handle_insert_row(&mut self, sender: ComponentSender<Self>) {
        if self.current_columns.is_empty() {
            return;
        }
        // Inline draft: track in the changeset (returns a
        // RowKey::Draft(N) handle), then prepend a fresh
        // RowObject tagged with the same draft id to the
        // grid's ListStore. The new row appears at the top
        // with a green tint and editable cells; user fills
        // them inline and clicks Save to commit.
        let default_values: Vec<Value> = self.current_columns.iter().map(|_| Value::Null).collect();
        let key_opt =
            crate::services::change_tracker::with_tab(self.tab_id, |t| t.track_insert(default_values.clone()));
        let Some(key) = key_opt else {
            return;
        };
        let crate::services::change_tracker::RowKey::Draft(draft_id) = key else {
            return;
        };
        if let Some(store) = self.list_store() {
            let draft_row = crate::ui::row_object::RowObject::new_draft(draft_id, default_values);
            store.insert(0, &draft_row);
        }
        // The empty-state status page hides the column view —
        // if we were sitting on it (zero persisted rows on the
        // first page) the draft we just appended would be
        // invisible. Re-derive the inner stack visibility now
        // that there's a draft to show.
        self.refresh_inner_stack_visibility();
        // Scroll + focus are deferred via FocusInsertedDraft.
        // Calling `cv.scroll_to(...)` synchronously here used
        // to crash with
        //   Gtk-ERROR gtk_list_base_update_adjustments:
        //   assertion failed: (bounds.y == 0)
        // when the empty → grid flip and the scroll landed in
        // the same tick: the ColumnView's ScrolledWindow had
        // no realized adjustments to update. FocusInsertedDraft
        // runs from `glib::idle_add_local_once`, after GTK has
        // finished allocating the now-visible grid, so the
        // adjustments are valid by then.
        sender.input(BrowseTabInput::FocusInsertedDraft);
    }

    pub(super) fn handle_delete_selected_row(&mut self, sender: ComponentSender<Self>) {
        // Toolbar Delete now marks the selected rows for
        // pending deletion (red strikethrough via tracker
        // overlay). User reviews + clicks Save to commit, or
        // Discard / Ctrl+Z to revert. Replaces the previous
        // confirm-dialog-then-immediate-DELETE flow.
        let Some(selection) = self.current_selection.as_ref() else {
            return;
        };
        let positions = selected_positions(selection);
        if positions.is_empty() {
            return;
        }
        let pk_indices: Vec<usize> = self
            .current_columns
            .iter()
            .enumerate()
            .filter(|(_, c)| c.primary_key)
            .map(|(i, _)| i)
            .collect();
        // Partition the selection into persisted rows (need a
        // PK to build a RowKey) and drafts (in-memory only,
        // discarded by id). We resolve through the selection
        // model rather than `result.rows[pos]` directly
        // because `pos` is in selection-model space, which
        // includes prepended drafts. A pure-draft selection
        // is valid (the user can bulk-discard pending
        // inserts before saving).
        let Some(model) = selection.model() else {
            return;
        };
        let mut snapshot: Vec<(crate::services::change_tracker::RowKey, Vec<Value>)> = Vec::new();
        let mut draft_ids: Vec<u64> = Vec::new();
        let mut had_persisted_row = false;
        for pos in &positions {
            let Some(item) = model.item(*pos) else { continue };
            let Ok(row_obj) = item.downcast::<crate::ui::row_object::RowObject>() else {
                continue;
            };
            if let Some(draft_id) = row_obj.draft_id() {
                draft_ids.push(draft_id);
                continue;
            }
            had_persisted_row = true;
            let cells = row_obj.cells_clone();
            if let Some(pair) = build_persisted_row_key(cells, &pk_indices) {
                snapshot.push(pair);
            }
        }
        // PK gate only fires when persisted rows are involved.
        // Pure-draft selections sail through to discard.
        if had_persisted_row && pk_indices.is_empty() {
            let _ = sender.output(BrowseTabOutput::ShowSelectionAlert {
                title: crate::tr!("Cannot delete"),
                body: crate::tr!("This table has no primary key — editing is disabled."),
            });
            return;
        }
        if snapshot.is_empty() && draft_ids.is_empty() {
            return;
        }
        let count = snapshot.len() + draft_ids.len();
        let tab_id = self.tab_id;
        let selection_for_commit = selection.clone();
        let commit_delete = move |snapshot: Vec<(crate::services::change_tracker::RowKey, Vec<Value>)>,
                                  draft_ids: Vec<u64>| {
            crate::services::change_tracker::with_tab(tab_id, |t| {
                for (key, row) in snapshot {
                    t.track_delete(key, row);
                }
                for id in draft_ids {
                    t.discard_draft(id);
                }
            });
            // Clear the multi-row selection so the
            // "{n} selected" badge disappears and the Delete
            // button tooltip resets. Without this the bitset
            // still contains the now-strikethrough rows
            // (they remain in the model until Save). Spreadsheet
            // convention: a bulk action ends the selection it
            // operated on.
            selection_for_commit.unselect_all();
        };
        if count >= BULK_DELETE_CONFIRM_THRESHOLD {
            // Dialog parent is any descendant of the toplevel
            // window — adw::AlertDialog walks up to find the
            // window. The inner_stack is always parented to
            // the BrowseTab's root toolbar, so it resolves
            // correctly while the tab is visible.
            let title = crate::tr!("Delete {n} rows?").replace("{n}", &count.to_string());
            let body = crate::tr!("These rows will be marked for deletion. They aren't removed until you click Save.");
            let dialog = adw::AlertDialog::new(Some(&title), Some(&body));
            dialog.add_response("cancel", &crate::tr!("Cancel"));
            dialog.add_response("delete", &crate::tr!("Delete"));
            dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
            dialog.set_default_response(Some("cancel"));
            dialog.set_close_response("cancel");
            let pending = std::cell::RefCell::new(Some((snapshot, draft_ids)));
            dialog.connect_response(None, move |dlg, response| {
                dlg.close();
                if response == "delete"
                    && let Some((s, d)) = pending.borrow_mut().take()
                {
                    commit_delete(s, d);
                }
            });
            dialog.present(Some(&self.inner_stack));
        } else {
            commit_delete(snapshot, draft_ids);
        }
    }

    pub(super) fn handle_grid_cell_edited(
        &mut self,
        row_position: u32,
        col_index: usize,
        new_value: String,
        sender: ComponentSender<Self>,
    ) {
        // Cell edits route through the per-tab change tracker
        // so the user can review / Save / Discard a batch.
        //
        // Empty input on a nullable column becomes Value::Null
        // (canonical SQL convention for "user cleared the
        // cell"). Non-empty input is parsed against the
        // column's data_type so every native type binds at
        // its declared kind instead of falling back to text.
        //
        // On parse failure the edit is rejected: a toast
        // explains why and `refresh_row` forces a re-bind so
        // the cell goes back to the canonical pre-edit
        // display. The tracker stays untouched so a single
        // bad keystroke can't sneak into the batch.
        //
        // GtkText handles multi-line clipboard paste
        // inconsistently across GTK builds — some embed
        // literal newlines, some strip them silently.
        // Collapse newlines / carriage returns to spaces
        // here so a paste-induced multi-line value never
        // reaches the SQL layer. JSON columns aren't
        // normalised: they need real newlines.
        let normalized = match self
            .current_columns
            .get(col_index)
            .map(|c| classify_type(&c.data_type.to_ascii_lowercase()))
        {
            Some(TypeKind::Json) => new_value,
            _ => normalize_single_line_input(&new_value),
        };
        let col = self.current_columns.get(col_index);
        let new = match parse_input_for_column(&normalized, col) {
            Ok(v) => v,
            Err(message) => {
                let _ = sender.output(BrowseTabOutput::ShowToast(message));
                self.refresh_row(row_position);
                return;
            }
        };
        let row_obj = self.row_object_at(row_position);
        if let Some(row_obj) = &row_obj
            && let Some(draft_id) = row_obj.draft_id()
        {
            // Draft row — mutate the tracker's draft buffer
            // directly. The RowObject's own cells are also
            // updated so the grid's display reflects the
            // pending value without waiting for re-fetch.
            crate::services::change_tracker::with_tab(self.tab_id, |t| {
                t.track_draft_cell_edit(draft_id, col_index, new.clone());
            });
            row_obj.set_cell(col_index, new);
            return;
        }
        let Some((key, row)) = self.row_key_at(row_position) else {
            return;
        };
        let original = row[col_index].clone();
        crate::services::change_tracker::with_tab(self.tab_id, |t| {
            t.track_cell_edit(key, col_index, original, new);
        });
    }

    pub(super) fn handle_grid_set_cell_null(&mut self, row_position: u32, col_index: usize) {
        let Some((key, row)) = self.row_key_at(row_position) else {
            return;
        };
        let original = row[col_index].clone();
        crate::services::change_tracker::with_tab(self.tab_id, |t| {
            t.track_cell_edit(key, col_index, original, Value::Null);
        });
    }

    pub(super) fn handle_grid_delete_row(&mut self, row_position: u32) {
        let Some((key, row)) = self.row_key_at(row_position) else {
            return;
        };
        crate::services::change_tracker::with_tab(self.tab_id, |t| {
            t.track_delete(key, row);
        });
    }

    pub(super) fn handle_grid_copy_row_as_insert(&mut self, row_position: u32, sender: ComponentSender<Self>) {
        let _ = sender.output(BrowseTabOutput::CopyRowAsInsert { row_position });
    }

    pub(super) fn handle_grid_copy_to_clipboard(&mut self, text: String, sender: ComponentSender<Self>) {
        let _ = sender.output(BrowseTabOutput::CopyToClipboard(text));
    }

    pub(super) fn handle_copy_selected_rows_as_tsv(&mut self, sender: ComponentSender<Self>) {
        let Some(selection) = self.current_selection.as_ref() else {
            return;
        };
        let positions = selected_positions(selection);
        if positions.is_empty() {
            return;
        }
        let model = match selection.model() {
            Some(m) => m,
            None => return,
        };
        let mut rows: Vec<String> = Vec::with_capacity(positions.len());
        for pos in &positions {
            let Some(item) = model.item(*pos) else { continue };
            let Ok(row) = item.downcast::<crate::ui::row_object::RowObject>() else {
                continue;
            };
            let cells = row.cells_clone();
            let line: Vec<String> = cells
                .iter()
                .map(|v| escape_tsv_cell(&crate::ui::grid::value_to_display_text(v)))
                .collect();
            rows.push(line.join("\t"));
        }
        if rows.is_empty() {
            return;
        }
        let tsv = rows.join("\n");
        let _ = sender.output(BrowseTabOutput::CopyToClipboard(tsv));
    }

    pub(super) fn handle_paste_not_supported(&mut self, sender: ComponentSender<Self>) {
        let _ = sender.output(BrowseTabOutput::ShowToast(crate::tr!(
            "Pasting rows isn't supported yet"
        )));
    }

    pub(super) fn handle_select_all_rows(&mut self) {
        if let Some(selection) = self.current_selection.as_ref() {
            let n = selection.n_items();
            if n > 0 {
                selection.select_all();
            }
        }
    }

    pub(super) fn handle_go_to_first_row(&mut self) {
        let Some(cv) = self.current_column_view.as_ref() else {
            return;
        };
        let n = self.current_selection.as_ref().map(|s| s.n_items()).unwrap_or(0);
        if n == 0 {
            return;
        }
        cv.scroll_to(
            0,
            None,
            gtk::ListScrollFlags::FOCUS | gtk::ListScrollFlags::SELECT,
            None,
        );
    }

    pub(super) fn handle_go_to_last_row(&mut self) {
        let Some(cv) = self.current_column_view.as_ref() else {
            return;
        };
        let n = self.current_selection.as_ref().map(|s| s.n_items()).unwrap_or(0);
        if n == 0 {
            return;
        }
        cv.scroll_to(
            n - 1,
            None,
            gtk::ListScrollFlags::FOCUS | gtk::ListScrollFlags::SELECT,
            None,
        );
    }

    pub(super) fn handle_commit_save(&mut self, sender: ComponentSender<Self>) {
        let columns = self.current_columns.clone();
        let driver_id = self.driver_id.clone();
        let schema = self.schema.clone();
        let table = self.table.clone();
        let result = crate::services::change_tracker::with_tab_ref(self.tab_id, |t| {
            t.materialize(&driver_id, schema.as_deref(), &table, &columns)
        });
        match result {
            Some(Ok((statements, sources))) if !statements.is_empty() => {
                // Disable both buttons for the duration of the
                // in-flight transaction. SaveCompleted /
                // SaveFailed re-enable them. This prevents a
                // double-click firing two transactions and
                // matches GNOME's standard "in-progress action"
                // affordance (busy spinner + disabled control).
                self.save_button.set_sensitive(false);
                self.discard_button.set_sensitive(false);
                // SaveCompleted will refetch the page; capture
                // the focused row's PK now so it can be re-
                // selected after the reload.
                self.capture_focus_for_restore();
                let _ = sender.output(BrowseTabOutput::ExecuteTransaction { statements, sources });
            }
            Some(Ok(_)) => {
                // Nothing to save (tracker empty) — refresh bar.
                self.refresh_pending_bar(0);
            }
            Some(Err(e)) => {
                let _ = sender.output(BrowseTabOutput::ShowSelectionAlert {
                    title: crate::tr!("Cannot save"),
                    body: format!("{e}"),
                });
            }
            None => {}
        }
    }

    pub(super) fn handle_discard_all(&mut self, sender: ComponentSender<Self>) {
        crate::services::change_tracker::with_tab(self.tab_id, |t| t.clear());
        let _ = sender.output(BrowseTabOutput::FetchPage);
    }

    pub(super) fn handle_pending_count_changed(&mut self, n: usize, sender: ComponentSender<Self>) {
        self.refresh_pending_bar(n);
        // Tell the App so it can prefix the tab title with the
        // GNOME-Text-Editor "•" dot for dirty buffers. Only on
        // real transitions (empty ↔ non-empty) so a count
        // change like 2 → 3 doesn't re-rewrite the tab title.
        let dirty = n > 0;
        if dirty != self.was_dirty.get() {
            self.was_dirty.set(dirty);
            let _ = sender.output(BrowseTabOutput::DirtyChanged(dirty));
        }
        // Note: row re-binds are driven by the parallel
        // ChangedRows event, not from here. PendingCountChanged
        // fires on every tracker mutation so re-binding the
        // viewport here would be wasteful — most edits affect
        // exactly one row and ChangedRows hits only that row.
    }

    pub(super) fn handle_changed_rows(&mut self, keys: Vec<crate::services::change_tracker::RowKey>) {
        // Walk the model once per key. For typical interactive
        // edits (one cell at a time) this is O(n) per keystroke
        // where n = visible row count — bounded and cheap.
        // Bulk operations (Discard) emit one ChangedRows per
        // op via undo unwind, again bounded.
        for key in &keys {
            if let Some(pos) = self.find_row_position_by_key(key) {
                self.refresh_row_at(pos);
            }
        }
    }

    pub(super) fn handle_save_completed(&mut self, sender: ComponentSender<Self>) {
        crate::services::change_tracker::with_tab(self.tab_id, |t| t.clear());
        self.refresh_pending_bar(0);
        self.save_button.set_sensitive(true);
        self.discard_button.set_sensitive(true);
        let _ = sender.output(BrowseTabOutput::FetchPage);
        let _ = sender.output(BrowseTabOutput::FetchRowCount);
    }

    pub(super) fn handle_save_failed(&mut self, message: String, sender: ComponentSender<Self>) {
        self.save_button.set_sensitive(true);
        self.discard_button.set_sensitive(true);
        let _ = sender.output(BrowseTabOutput::ShowSelectionAlert {
            title: crate::tr!("Save failed"),
            body: message,
        });
    }

    pub(super) fn handle_flash_error_row(&self, source: crate::services::change_tracker::StatementSource) {
        self.flash_error_row(&source);
    }

    pub(super) fn handle_focus_inserted_draft(&self) {
        self.focus_inserted_draft();
    }

    pub(super) fn handle_undo(&mut self) {
        use crate::services::change_tracker::UndoOp;
        let op = match crate::services::change_tracker::with_tab(self.tab_id, |t| t.undo()) {
            Some(Some(op)) => op,
            _ => return,
        };
        match op {
            UndoOp::CellEdit {
                row_key,
                col,
                prev_value,
                ..
            } => {
                // Drafts hold the post-edit value in
                // RowObject.cells (mirrored at edit time);
                // reverting the visible value requires
                // mutating the cell back. Persisted rows
                // never mutate RowObject — the tracker is
                // the single source of truth and
                // connect_bind queries
                // `current_cell_value` to overlay the
                // pending edit. set_cell on a persisted
                // row is a harmless no-op (the cell
                // already holds the original).
                if let Some(pos) = self.find_row_position_by_key(&row_key)
                    && let Some(row_obj) = self.row_object_at(pos)
                {
                    row_obj.set_cell(col, prev_value);
                }
            }
            UndoOp::Insert { draft_id, .. } => {
                // The draft RowObject is still in the
                // ListStore (prepended by InsertRow). Walk
                // the store, find the row whose draft_id
                // matches, remove it. The subsequent
                // ChangedRows event for `Draft(id)` is a
                // no-op once the row is gone.
                if let Some(store) = self.list_store() {
                    let n = store.n_items();
                    for i in 0..n {
                        if let Some(obj) = store.item(i)
                            && let Ok(row) = obj.downcast::<crate::ui::row_object::RowObject>()
                            && row.draft_id() == Some(draft_id)
                        {
                            store.remove(i);
                            break;
                        }
                    }
                }
            }
            UndoOp::Delete { row_key, .. } => {
                // No RowObject mutation needed — the row was
                // never visually removed; it stayed in the
                // ListStore with a strikethrough overlay
                // applied at bind time. The tracker's
                // emit_changed → items_changed re-bind drops
                // the strikethrough automatically because
                // `row_state` returns Clean once the deletes
                // entry is gone.
                let _ = row_key;
            }
        }
    }

    pub(super) fn handle_redo(&mut self) {
        use crate::services::change_tracker::UndoOp;
        let op = match crate::services::change_tracker::with_tab(self.tab_id, |t| t.redo()) {
            Some(Some(op)) => op,
            _ => return,
        };
        match op {
            UndoOp::CellEdit {
                row_key,
                col,
                new_value,
                ..
            } => {
                if let Some(pos) = self.find_row_position_by_key(&row_key)
                    && let Some(row_obj) = self.row_object_at(pos)
                {
                    row_obj.set_cell(col, new_value);
                }
            }
            UndoOp::Insert { draft_id, values } => {
                // Re-add the draft RowObject. Match the
                // original insert path: prepend at position 0.
                if let Some(store) = self.list_store() {
                    let draft_row = crate::ui::row_object::RowObject::new_draft(draft_id, values);
                    store.insert(0, &draft_row);
                }
            }
            UndoOp::Delete { .. } => {}
        }
    }
}
