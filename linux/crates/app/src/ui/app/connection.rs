use relm4::adw::prelude::*;
use relm4::{Component, ComponentController, ComponentSender, adw};

use tablepro_core::TableInfo;
use tablepro_storage::SavedConnection;
use uuid::Uuid;

use crate::services::database_service::{ConnectionHealth, ConnectionMetadata};
use crate::services::{connection_service, database_service};
use crate::ui::connect_dialog::{ConnectDialog, ConnectDialogInit, ConnectDialogOutput};

use super::{App, AppMsg, ConnectionTransition, SwitchDecision};

impl App {
    /// The policy-guarded connection this window owns, or `None` when the
    /// window is disconnected. Never resolves through the process-wide focus,
    /// so another window's connection can never answer for this one.
    pub(super) fn window_connection(&self) -> Option<std::sync::Arc<dyn tablepro_core::Connection>> {
        let id = self.connection_id?;
        database_service::instance().get(id)
    }

    pub(super) fn window_metadata(&self) -> Option<ConnectionMetadata> {
        let id = self.connection_id?;
        database_service::instance().metadata(id)
    }

    pub(super) fn on_open_connect(&mut self, sender: ComponentSender<Self>) {
        if self.connection_transition != ConnectionTransition::Idle || self.dialog.is_some() {
            self.show_toast(&crate::tr!("A connection change is already in progress."));
            return;
        }
        self.connection_transition = ConnectionTransition::Connecting;
        let dialog = ConnectDialog::builder()
            .launch(ConnectDialogInit {
                registry: self.registry.clone(),
            })
            .forward(sender.input_sender(), |out| match out {
                ConnectDialogOutput::Prepared(prepared) => AppMsg::ConnectionPrepared(prepared),
                ConnectDialogOutput::Closed => AppMsg::DialogClosed,
            });
        dialog.widget().present(Some(&self.window));
        self.dialog = Some(dialog);
    }

    pub(super) fn on_connected(
        &mut self,
        activated: connection_service::ActivatedConnection,
        sender: ComponentSender<Self>,
    ) {
        let connection_service::ActivatedConnection { id, tables, driver_id } = activated;
        self.dismiss_loading_page();
        self.dialog = None;
        self.connected = true;
        self.connection_id = Some(id);
        self.current_driver_id = Some(driver_id.clone());
        self.read_only = self.window_metadata().map(|m| m.read_only).unwrap_or(false);
        self.read_only_badge.set_visible(self.read_only);
        self.split_view.set_show_sidebar(true);
        self.disconnect_action.set_enabled(true);
        self.table_search.set_text("");
        // Build the unified workspace tab tree (Browse + Editor share one
        // strip). Empty state shows "Select a table" until the user opens
        // a tab via sidebar click or Ctrl+T.
        self.ensure_workspace_root(sender.clone());
        self.content_holder.set_content(Some(&self.workspace_outer_stack));
        self.table_names = tables.iter().map(|t| t.name.clone()).collect();
        tracing::info!(driver = %driver_id, table_count = tables.len(), "workspace ready");
        self.repopulate_sidebar(&tables);
        self.rebuild_schema_buffer();
        self.refresh_window_title();
        // Restore tabs (browse + editor) persisted from the prior session
        // for this connection.
        if let Some(connection_id) = self.connection_id {
            self.restore_workspace_tabs(connection_id, sender.clone());
            // Stamp `last_opened_at = now()` then reload connections so
            // the popover + welcome view re-sort with the fresh
            // timestamp. Sequencing matters: ReloadConnections reads
            // the JSON; firing it before the touch lands would render
            // the previous ordering until the next reload.
            let sender_for_touch = sender.clone();
            relm4::spawn(async move {
                if let Err(e) = tablepro_storage::touch_last_opened(connection_id).await {
                    tracing::warn!(error = %e, "touch_last_opened failed");
                }
                sender_for_touch.input(AppMsg::ReloadConnections);
            });
            return;
        }
        sender.input(AppMsg::ReloadConnections);
    }

    pub(super) fn on_disconnect(&mut self, sender: ComponentSender<Self>) {
        if self.connection_transition != ConnectionTransition::Idle {
            self.show_toast(&crate::tr!(
                "Finish or cancel the connection change before disconnecting."
            ));
            return;
        }
        // Block disconnect when any tab has pending changes. The
        // teardown below clears all tracker registries, so dropping
        // the connection mid-edit silently destroys the user's work.
        // Confirm via an AlertDialog mirroring the window-close-with-
        // pending and F5-with-pending paths.
        let has_pending = self.window_has_pending();
        if has_pending {
            let dialog = adw::AlertDialog::new(
                Some(&crate::tr!("Discard pending changes?")),
                Some(&crate::tr!(
                    "Disconnecting will close every tab and drop every unsaved row edit and DDL change."
                )),
            );
            dialog.add_response("cancel", &crate::tr!("Cancel"));
            dialog.add_response("discard", &crate::tr!("Discard and disconnect"));
            dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
            dialog.set_default_response(Some("cancel"));
            dialog.set_close_response("cancel");
            let sender_for_resp = sender.clone();
            dialog.connect_response(None, move |dlg, response| {
                dlg.close();
                if response == "discard" {
                    sender_for_resp.input(AppMsg::ForceDisconnect);
                }
            });
            dialog.present(Some(&self.window));
            return;
        }
        self.do_disconnect(sender);
    }

    /// Skip the dirty check and tear the connection down. Reachable
    /// either from the AlertDialog "Discard and disconnect" branch
    /// or from a clean `Disconnect` when no tracker has pending
    /// changes.
    pub(super) fn do_disconnect(&mut self, sender: ComponentSender<Self>) {
        // Invalidate any candidate left behind by a stale dialog callback.
        // A late preparation result is rejected unless the transition is
        // still `Connecting`, so it cannot reconnect after this teardown.
        self.prepared_connection = None;
        self.switch_saves_pending.clear();
        self.switch_cancel_audit_was_disabled = None;
        self.connection_transition = ConnectionTransition::Idle;
        // Persist + tear down workspace tabs before dropping the
        // connection (persist needs the active connection_id).
        self.teardown_workspace_tabs();
        // Drop reopen-stack entries — they reference tables in the
        // connection we're about to release. Reopening one against a
        // different connection would target a non-existent table.
        self.clear_closed_tabs_stack();
        if let Some(id) = self.connection_id.take() {
            database_service::instance().close(id);
        }
        self.schema_buffer.set_text(crate::ui::editor::SQL_KEYWORDS);
        self.current_driver_id = None;
        self.read_only = false;
        self.read_only_badge.set_visible(false);
        self.connected = false;
        self.split_view.set_show_sidebar(false);
        self.disconnect_action.set_enabled(false);
        self.refresh_window_title();
        self.table_search.set_text("");
        self.sidebar_schemas.borrow_mut().clear();
        self.sidebar_factory.guard().clear();
        self.show_welcome_page(sender);
        tracing::info!("disconnected");
    }

    pub(super) fn on_reload_connections(&self, sender: ComponentSender<Self>) {
        let sender_clone = sender.clone();
        sender.command(move |_, shutdown| {
            shutdown
                .register(async move {
                    if let Ok(connections) = tablepro_storage::load_connections().await {
                        sender_clone.input(AppMsg::ConnectionsLoaded(connections));
                    }
                })
                .drop_on_shutdown()
        });
    }

    pub(super) fn on_connections_loaded(&mut self, connections: &[SavedConnection], sender: ComponentSender<Self>) {
        self.saved_connections = connections.to_vec();
        let mut guard = self.connections_factory.guard();
        guard.clear();
        for saved in connections {
            guard.push_back(crate::ui::connection_row::ConnectionRowInit {
                saved: saved.clone(),
                organization: self.connection_organization.get(saved.id),
            });
        }
        drop(guard);
        let _ = self
            .welcome_view
            .sender()
            .send(crate::ui::welcome_view::WelcomeViewInput::SetConnections(
                self.saved_connections.clone(),
            ));
        self.prune_connection_organization(sender.clone());
        if !self.connected {
            self.show_welcome_page(sender);
        }
    }

    pub(super) fn on_poll_health(&mut self) {
        let current = self
            .connection_id
            .and_then(|id| database_service::instance().health(id));
        if current != self.health_state {
            self.refresh_health_banner(current.clone());
            self.health_state = current;
        }
    }

    pub(super) fn on_delete_connection(&self, id: Uuid, sender: ComponentSender<Self>) {
        // Connection deletion wipes the saved entry and ALL associated
        // keyring credentials (db password, SSH password, SSH passphrase).
        // Irreversible (no Undo can recover keyring entries) so we
        // confirm unconditionally — the previous `confirm_destructive`
        // preference gate let users skip it, but per HIG (and GNOME
        // Files' bookmark-delete behaviour) destructive keyring writes
        // need confirmation regardless of preferences.
        let connection_name = self
            .saved_connections
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| crate::tr!("this connection"));
        let title = crate::tr!("Delete {name}?").replace("{name}", &connection_name);
        let body = crate::tr!(
            "The saved entry and any stored passwords will be removed from your keyring. This cannot be undone."
        );
        let dialog = adw::AlertDialog::new(Some(&title), Some(&body));
        dialog.add_response("cancel", &crate::tr!("Cancel"));
        dialog.add_response("delete", &crate::tr!("Delete"));
        dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");

        let sender_for_response = sender;
        dialog.connect_response(None, move |dialog, response| {
            dialog.close();
            if response != "delete" {
                return;
            }
            execute_delete_connection(id, sender_for_response.clone());
        });
        dialog.present(Some(&self.window));
    }

    pub(super) fn on_open_saved(&mut self, saved: SavedConnection, sender: ComponentSender<Self>) {
        if self.connection_transition != ConnectionTransition::Idle {
            self.show_toast(&crate::tr!("A connection change is already in progress."));
            return;
        }
        self.connections_popover.popdown();
        self.connection_transition = ConnectionTransition::Connecting;
        self.set_loading_page(
            &crate::tr!("Connecting…"),
            &crate::tr!("Opening {name}").replace("{name}", &saved.name),
        );
        let registry = self.registry.clone();
        let timeout_secs = crate::services::operation_control::configured_timeout_secs();
        let sender_clone = sender.clone();
        sender.command(move |_, shutdown| {
            shutdown
                .register(async move {
                    match connection_service::open_saved(registry, saved, timeout_secs).await {
                        Ok(prepared) => sender_clone.input(AppMsg::ConnectionPrepared(Box::new(prepared))),
                        Err(e) => sender_clone.input(AppMsg::ConnectionPrepareFailed(e)),
                    }
                })
                .drop_on_shutdown()
        });
    }

    pub(super) fn on_connection_prepared(
        &mut self,
        prepared: Box<connection_service::PreparedConnection>,
        sender: ComponentSender<Self>,
    ) {
        if self.connection_transition != ConnectionTransition::Connecting {
            tracing::warn!("discarding stale prepared connection");
            return;
        }
        if self.prepared_connection.is_some() {
            tracing::warn!("discarding duplicate prepared connection");
            return;
        }
        self.dismiss_loading_page();
        self.prepared_connection = Some(*prepared);
        self.connection_transition = ConnectionTransition::AwaitingDecision;
        self.continue_connection_switch(sender);
    }

    pub(super) fn on_connection_prepare_failed(&mut self, message: String) {
        if self.connection_transition != ConnectionTransition::Connecting {
            tracing::warn!(error = %message, "discarding stale connection failure");
            return;
        }
        self.connection_transition = ConnectionTransition::Idle;
        self.prepared_connection = None;
        self.switch_cancel_audit_was_disabled = None;
        self.dismiss_loading_page();
        tracing::warn!(error = %message, "candidate connection failed");
        self.set_status_page(super::StatusKind::Error, &crate::tr!("Connection failed"), &message);
    }

    pub(super) fn on_connect_dialog_closed(&mut self) {
        self.dialog = None;
        if self.connection_transition == ConnectionTransition::Connecting && self.prepared_connection.is_none() {
            self.connection_transition = ConnectionTransition::Idle;
            self.switch_cancel_audit_was_disabled = None;
        }
    }

    /// Advance a prepared connection only when the old workspace is safe to
    /// release. Called after preparation and every editor/save completion.
    pub(super) fn continue_connection_switch(&mut self, sender: ComponentSender<Self>) {
        if self.prepared_connection.is_none() {
            return;
        }
        match self.connection_transition {
            ConnectionTransition::WaitingForRuns if self.any_editor_running() => return,
            ConnectionTransition::WaitingForSaves if !self.switch_saves_pending.is_empty() => return,
            ConnectionTransition::Connecting | ConnectionTransition::Idle => return,
            _ => {}
        }

        if self.connection_transition == ConnectionTransition::WaitingForRuns {
            let was_disabled = self.switch_cancel_audit_was_disabled.take().unwrap_or(false);
            if !was_disabled && database_service::instance().governed_writes_disabled() {
                self.prepared_connection = None;
                self.switch_saves_pending.clear();
                self.connection_transition = ConnectionTransition::Idle;
                self.show_toast(&crate::tr!(
                    "Connection switch stopped because a running operation has an unknown outcome."
                ));
                return;
            }
        }

        if self.any_editor_running() {
            self.connection_transition = ConnectionTransition::AwaitingDecision;
            let dialog = adw::AlertDialog::new(
                Some(&crate::tr!("Cancel running queries and switch?")),
                Some(&crate::tr!(
                    "TablePro will wait for every running query to report cancellation before changing connections."
                )),
            );
            dialog.add_response("stay", &crate::tr!("Stay"));
            dialog.add_response("cancel-runs", &crate::tr!("Cancel queries and switch"));
            dialog.set_response_appearance("cancel-runs", adw::ResponseAppearance::Destructive);
            dialog.set_default_response(Some("stay"));
            dialog.set_close_response("stay");
            let response_sender = sender.clone();
            dialog.connect_response(None, move |dialog, response| {
                dialog.close();
                response_sender.input(AppMsg::ConnectionSwitchDecision(if response == "cancel-runs" {
                    SwitchDecision::CancelRuns
                } else {
                    SwitchDecision::Stay
                }));
            });
            dialog.present(Some(&self.window));
            return;
        }

        let has_pending = self.window_has_pending();
        if has_pending {
            self.connection_transition = ConnectionTransition::AwaitingDecision;
            let dialog = adw::AlertDialog::new(
                Some(&crate::tr!("Save changes before switching?")),
                Some(&crate::tr!(
                    "Pending row and structure changes belong to the current connection."
                )),
            );
            dialog.add_response("stay", &crate::tr!("Stay"));
            dialog.add_response("discard", &crate::tr!("Discard and switch"));
            dialog.add_response("save", &crate::tr!("Save and switch"));
            dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
            dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
            dialog.set_default_response(Some("save"));
            dialog.set_close_response("stay");
            let response_sender = sender.clone();
            dialog.connect_response(None, move |dialog, response| {
                dialog.close();
                let decision = match response {
                    "discard" => SwitchDecision::DiscardChanges,
                    "save" => SwitchDecision::SaveChanges,
                    _ => SwitchDecision::Stay,
                };
                response_sender.input(AppMsg::ConnectionSwitchDecision(decision));
            });
            dialog.present(Some(&self.window));
            return;
        }

        self.activate_prepared_connection(sender);
    }

    pub(super) fn on_connection_switch_decision(&mut self, decision: SwitchDecision, sender: ComponentSender<Self>) {
        match decision {
            SwitchDecision::Stay => {
                self.prepared_connection = None;
                self.switch_saves_pending.clear();
                self.switch_cancel_audit_was_disabled = None;
                self.connection_transition = ConnectionTransition::Idle;
                self.show_toast(&crate::tr!("Connection switch cancelled."));
            }
            SwitchDecision::CancelRuns => {
                self.switch_cancel_audit_was_disabled = Some(database_service::instance().governed_writes_disabled());
                self.connection_transition = ConnectionTransition::WaitingForRuns;
                self.cancel_all_editor_runs();
                self.continue_connection_switch(sender);
            }
            SwitchDecision::DiscardChanges => {
                for id in self.window_pending_browse_tabs() {
                    crate::services::change_tracker::with_tab(id, |tracker| tracker.clear());
                }
                for id in self.window_pending_structure_tabs() {
                    crate::services::structure_tracker::with_tab(id, |tracker| tracker.clear());
                }
                self.activate_prepared_connection(sender);
            }
            SwitchDecision::SaveChanges => {
                let browse = self.window_pending_browse_tabs();
                let structure = self.window_pending_structure_tabs();
                self.switch_saves_pending.clear();
                for id in browse.iter().copied() {
                    *self.switch_saves_pending.entry(id).or_insert(0) += 1;
                }
                for id in structure.iter().copied() {
                    *self.switch_saves_pending.entry(id).or_insert(0) += 1;
                }
                self.connection_transition = ConnectionTransition::WaitingForSaves;
                for id in browse {
                    sender.input(AppMsg::SaveActiveBrowseTabById(id));
                }
                for id in structure {
                    sender.input(AppMsg::SaveActiveStructureTabById(id));
                }
                self.continue_connection_switch(sender);
            }
        }
    }

    fn activate_prepared_connection(&mut self, sender: ComponentSender<Self>) {
        let Some(prepared) = self.prepared_connection.take() else {
            return;
        };
        if self.connected {
            // Persist while the old connection id is still active. Only then
            // dispose the old tab controllers and install the candidate.
            self.teardown_workspace_tabs();
            self.clear_closed_tabs_stack();
        }
        // Activation is additive, so this window releases the connection it is
        // replacing. Closing before activation keeps reconnecting to the same
        // saved connection correct: the identifier is reused, not cancelled.
        if let Some(previous) = self.connection_id.take() {
            database_service::instance().close(previous);
        }
        let activated = prepared.activate();
        self.connection_transition = ConnectionTransition::Idle;
        self.switch_saves_pending.clear();
        self.switch_cancel_audit_was_disabled = None;
        self.on_connected(activated, sender);
    }

    pub(super) fn connection_switch_save_succeeded(&mut self, tab_id: Uuid, sender: ComponentSender<Self>) {
        if self.connection_transition != ConnectionTransition::WaitingForSaves {
            return;
        }
        if let Some(remaining) = self.switch_saves_pending.get_mut(&tab_id) {
            *remaining = remaining.saturating_sub(1);
            if *remaining == 0 {
                self.switch_saves_pending.remove(&tab_id);
            }
        }
        self.continue_connection_switch(sender);
    }

    pub(super) fn connection_switch_save_failed(&mut self) {
        if self.connection_transition != ConnectionTransition::WaitingForSaves {
            return;
        }
        self.switch_saves_pending.clear();
        self.prepared_connection = None;
        self.switch_cancel_audit_was_disabled = None;
        self.connection_transition = ConnectionTransition::Idle;
        self.show_toast(&crate::tr!(
            "Connection switch stopped because changes could not be saved."
        ));
    }

    pub(super) fn repopulate_sidebar(&mut self, tables: &[TableInfo]) {
        {
            let mut schemas = self.sidebar_schemas.borrow_mut();
            schemas.clear();
            schemas.extend(tables.iter().map(|t| t.schema.clone()));
        }
        let mut guard = self.sidebar_factory.guard();
        guard.clear();
        for table in tables {
            guard.push_back(table.clone());
        }
        drop(guard);
        self.sidebar_factory.widget().invalidate_headers();
    }

    /// Surfaces connection health via `adw::Banner` only when degraded —
    /// healthy/disconnected states show no chrome, matching GNOME apps that
    /// reserve banners for "abnormal, user-actionable" situations (Files
    /// uses the same pattern for unmounted volumes).
    pub(super) fn refresh_health_banner(&self, health: Option<ConnectionHealth>) {
        match health {
            Some(ConnectionHealth::Reconnecting { attempt }) => {
                self.reconnect_banner.set_title(
                    &crate::tr!("Connection lost — reconnecting (attempt {n}, will keep retrying)")
                        .replace("{n}", &attempt.to_string()),
                );
                self.reconnect_banner.set_revealed(true);
            }
            _ => self.reconnect_banner.set_revealed(false),
        }
    }
}

/// Performs the actual disk + keyring teardown for a saved connection.
/// Extracted from `on_delete_connection` so the confirm-yes branch and
/// the prefs-disabled branch share one implementation.
fn execute_delete_connection(id: Uuid, sender: ComponentSender<App>) {
    let sender_clone = sender.clone();
    sender.command(move |_, shutdown| {
        shutdown
            .register(async move {
                let _ = tablepro_storage::delete_connection(id).await;
                let _ = tablepro_storage::delete_password(id).await;
                let _ = tablepro_storage::delete_ssh_password(id).await;
                let _ = tablepro_storage::delete_ssh_passphrase(id).await;
                sender_clone.input(AppMsg::ReloadConnections);
            })
            .drop_on_shutdown()
    });
}
