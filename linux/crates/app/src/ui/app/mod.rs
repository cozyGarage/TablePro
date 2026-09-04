mod browse;
mod connection;
mod favorites;
mod init_css;
mod init_sidebar;
mod init_window;
mod init_workspace;
mod msg;
mod organization;
mod render;
mod row_ops;
mod schema_index;
mod shortcuts;
mod status_pages;
mod structure;
mod types;
mod workspace_chrome;
mod workspace_close;
mod workspace_persist;
mod workspace_tabs;

use std::sync::Arc;

use relm4::adw::prelude::*;
use relm4::factory::FactoryVecDeque;
use relm4::gtk::{gio, glib};
use relm4::prelude::*;
use relm4::{Controller, adw, gtk};

use tablepro_core::DriverRegistry;
use tablepro_storage::{ConnectionOrganizationIndex, SavedConnection};
use uuid::Uuid;

use super::browse_tab::BrowseTabInput;
use super::connect_dialog::ConnectDialog;
use super::connection_row::ConnectionRow;
use super::editor::build_schema_buffer;
use super::history_dialog::HistoryDialog;
use super::sidebar_row::SidebarRow;
use super::welcome_view::{WelcomeView, WelcomeViewInit, WelcomeViewOutput};
use crate::services::database_service::ConnectionHealth;

pub use msg::AppMsg;
use render::{qualified_label, render_json};
use types::{CLOSED_TABS_CAPACITY, ConnectionTransition, ExportFormat, StatusKind, SwitchDecision};
pub use types::{ClosedTabDescriptor, EditorTabSlot, OpenMode, StructureTabSlot, TableTabSlot, WorkspaceTab};

/// Decrement a tab's pending-save counter in the close-after-save map.
/// Returns `true` if the entry just dropped to zero (the caller should
/// fire `WorkspaceTabClosed`); returns `false` if there's still another
/// in-flight save for that tab, or if the entry was never present.
///
/// Used by both browse `SaveCompletedForTab` and structure
/// `on_structure_save_completed` so a Table tab with both kinds of
/// pending changes only closes after BOTH saves succeed.
pub(super) fn dec_close_after_save(map: &mut std::collections::HashMap<Uuid, u32>, tab_id: &Uuid) -> bool {
    if let Some(count) = map.get_mut(tab_id) {
        *count = count.saturating_sub(1);
        if *count == 0 {
            map.remove(tab_id);
            return true;
        }
    }
    false
}

pub struct App {
    registry: Arc<DriverRegistry>,
    window: adw::ApplicationWindow,
    split_view: adw::OverlaySplitView,
    window_title: adw::WindowTitle,
    sidebar_title: adw::WindowTitle,
    disconnect_action: gio::SimpleAction,
    sidebar_factory: FactoryVecDeque<SidebarRow>,
    sidebar_schemas: std::rc::Rc<std::cell::RefCell<Vec<Option<String>>>>,
    sidebar_kinds: std::rc::Rc<std::cell::RefCell<Vec<crate::ui::sidebar_row::SidebarObjectKind>>>,
    sidebar_views: Vec<tablepro_core::TableInfo>,
    content_holder: adw::ToolbarView,
    toast_overlay: adw::ToastOverlay,
    /// Persistent "Connecting…" toast handle. Held so we can dismiss it
    /// when the connect attempt resolves (success or failure). Native
    /// alternative to a fire-and-forget 2 s toast that disappeared
    /// before the connection actually completed.
    connect_progress_toast: Option<adw::Toast>,
    reconnect_banner: adw::Banner,
    connections_factory: FactoryVecDeque<ConnectionRow>,
    connections_popover: gtk::Popover,
    health_state: Option<ConnectionHealth>,
    row_op_spinner: gtk::Spinner,
    read_only_badge: gtk::Label,
    table_search: gtk::SearchEntry,
    /// Outer Stack inside `content_holder` — swaps between `"empty"`
    /// (AdwStatusPage "Select a table") and `"tabs"` (the unified
    /// AdwTabOverview hosting both Browse and Editor sub-components).
    workspace_outer_stack: gtk::Stack,
    /// AdwTabOverview wrapping the unified AdwTabBar + AdwTabView.
    /// Built lazily on connect; torn down on disconnect.
    workspace_root: Option<adw::TabOverview>,
    workspace_tab_view: Option<adw::TabView>,
    /// Idempotency flag for `ensure_workspace_root`.
    workspace_root_added: std::cell::Cell<bool>,
    /// Per-tab state. Each entry is either a Browse or Editor tab.
    /// HashMap for O(1) tab_id lookup; display order is read from
    /// `tab_view.pages()` since HashMap is unordered.
    workspace_tabs: std::rc::Rc<std::cell::RefCell<std::collections::HashMap<Uuid, WorkspaceTab>>>,
    dialog: Option<Controller<ConnectDialog>>,
    schema_buffer: gtk::TextBuffer,
    favorites: Vec<tablepro_storage::SavedQuery>,
    /// Tables and their columns for schema-aware editor completion.
    schema_index: std::rc::Rc<std::cell::RefCell<crate::ui::editor::SchemaIndex>>,
    /// Tables whose columns are already requested, so cursor movement
    /// does not re-issue the same fetch.
    requested_columns: std::rc::Rc<std::cell::RefCell<std::collections::HashSet<String>>>,
    history_dialog: Option<Controller<HistoryDialog>>,
    welcome_view: Controller<WelcomeView>,
    /// Driver id is connection-wide, not per-tab.
    current_driver_id: Option<String>,
    /// All tables in the current connection — fed into `schema_buffer`
    /// for the editor's autocomplete; not the per-tab columns.
    table_names: Vec<String>,
    /// Read-only flag is connection-wide; fanned out to every BrowseTab
    /// when toggled.
    read_only: bool,
    /// Default page size for newly-opened browse tabs (from preferences).
    /// Per-tab page size lives on each BrowseTab.
    default_page_size: u64,
    saved_connections: Vec<SavedConnection>,
    /// Groups, tags and favourite flags for the saved connections,
    /// loaded from the sidecar file beside `connections.json`. Held in
    /// the model so the welcome view can be re-arranged without another
    /// disk read on every keystroke in the filter box.
    connection_organization: ConnectionOrganizationIndex,
    connected: bool,
    /// The connection this window owns. Activation is process-wide and
    /// additive, so a window must resolve its own connection rather than
    /// whichever one was focused most recently.
    connection_id: Option<Uuid>,
    /// Serialized connection-switch state. The candidate is fully validated
    /// before it can replace the active connection.
    connection_transition: ConnectionTransition,
    prepared_connection: Option<crate::services::connection_service::PreparedConnection>,
    switch_saves_pending: std::collections::HashMap<Uuid, u32>,
    /// Audit safety state captured before cancelling running work for a
    /// connection switch. If cancellation makes a previously safe state
    /// ambiguous, the candidate is discarded and the old workspace remains.
    switch_cancel_audit_was_disabled: Option<bool>,
    /// Tabs the user picked "Save" on in a close-confirmation dialog,
    /// counted by remaining saves before the close fires. A Table tab
    /// with both browse-dirty AND structure-dirty dispatches two saves
    /// (one of each kind) so its entry starts at 2 — the first
    /// completion decrements to 1 (no close yet), the second decrements
    /// to 0 and finally fires `WorkspaceTabClosed`. A `SaveFailed`
    /// removes the entry entirely (abort all close intents for that
    /// tab so the user can fix the error and retry). See
    /// `dec_close_after_save` for the decrement helper.
    close_after_save: std::rc::Rc<std::cell::RefCell<std::collections::HashMap<Uuid, u32>>>,
    /// Set when the user picked "Save" on the *window*-close dialog.
    /// While true, the last `SaveCompletedForTab` that empties
    /// `close_after_save` triggers `window.close()`. A `SaveFailed`
    /// while in this state aborts the window-close intent. `Rc<Cell>`
    /// so the close-request handler closure can mutate it from outside
    /// `App::update`.
    close_window_after_save: std::rc::Rc<std::cell::Cell<bool>>,
    /// Count of in-flight transaction commits (a tab clicked Save
    /// and is awaiting `execute_in_transaction`). Incremented by
    /// `on_execute_browse_transaction` at dispatch, decremented by
    /// `SaveCompletedForTab` and `SaveFailedForTab`. Window-close
    /// blocks while this is > 0 so an async transaction never commits
    /// after the tab / window has been torn down.
    in_flight_saves: std::rc::Rc<std::cell::Cell<usize>>,
    /// Structure tabs currently mid-DDL-transaction. A second Ctrl+S
    /// while a Save is still in flight would dispatch a parallel
    /// transaction and potentially commit twice; this set lets the
    /// dispatch path short-circuit. Cleared on
    /// `StructureSaveCompleted` / `StructureSaveFailed`.
    structure_saves_in_flight: std::rc::Rc<std::cell::RefCell<std::collections::HashSet<Uuid>>>,
    /// Debounce flag for `persist_workspace_state`. Active tabs fire
    /// `WorkspaceTabsChanged` on every selection / drag-reorder /
    /// page-size change / state-changed event; without coalescing,
    /// each one triggers a load-modify-write of the entire
    /// connections JSON. This flag stays `true` while a 500ms timer
    /// is pending; subsequent persist requests in the window no-op.
    persist_pending: std::rc::Rc<std::cell::Cell<bool>>,
    persist_timeout: std::rc::Rc<std::cell::RefCell<Option<glib::SourceId>>>,
    persist_generation: std::rc::Rc<std::cell::Cell<u64>>,
    /// LIFO stack of recently-closed tab descriptors for Ctrl+Shift+T
    /// reopen. Capped at `CLOSED_TABS_CAPACITY`; the oldest entry is
    /// dropped when a new one is pushed against a full stack. Cleared
    /// on disconnect — descriptors reference the active connection's
    /// tables, so reopening across connections would target the wrong
    /// schema. Editor descriptors round-trip the buffer text;
    /// dirty tabs lose their pending row/DDL edits because the
    /// trackers have already been cleared by the close path.
    closed_tabs_stack: std::rc::Rc<std::cell::RefCell<std::collections::VecDeque<ClosedTabDescriptor>>>,
    /// Extra ApplicationWindows spawned via New Window. Kept alive so
    /// closing the primary window does not drop them mid-use. Removing an
    /// entry (see `AppMsg::ExtraWindowClosed`) drops its `Controller`,
    /// which is what actually triggers that window's `Component::shutdown`.
    extra_windows: Vec<(usize, Controller<App>)>,
    /// Source for the once-a-second health poll, so `shutdown` can remove
    /// it instead of leaving it firing forever after this window closes.
    poll_health_source: Option<glib::SourceId>,
    /// Source for the hourly history prune, same reasoning.
    history_prune_source: Option<glib::SourceId>,
}

impl App {
    /// The active driver id, or "postgres" if no connection is active.
    ///
    /// Single fallback site (was duplicated at 7 call sites). The
    /// tracing::warn! makes the latent bug visible if anything ever
    /// asks for the driver id without an active connection — today
    /// that would silently corrupt SQL quoting on non-Postgres drivers.
    pub(super) fn driver_id(&self) -> &str {
        match self.current_driver_id.as_deref() {
            Some(id) => id,
            None => {
                tracing::warn!("driver_id called without active connection; falling back to postgres");
                "postgres"
            }
        }
    }
}

#[relm4::component(pub)]
impl SimpleComponent for App {
    type Init = Arc<DriverRegistry>;
    type Input = AppMsg;
    type Output = ();

    view! {
        #[name = "window"]
        adw::ApplicationWindow {
            set_title: Some("TablePro"),
            set_default_width: 1200,
            set_default_height: 760,

            adw::ToolbarView {
                #[name = "header_bar"]
                add_top_bar = &adw::HeaderBar {
                    #[name = "window_title"]
                    #[wrap(Some)]
                    set_title_widget = &adw::WindowTitle {
                        set_title: "TablePro",
                    },

                    // Two distinct affordances → two distinct buttons.
                    // SplitButton would imply they're variants of the
                    // same action, but "new connection" and "open
                    // saved" are semantically different (matches GNOME
                    // Files' "New" + "History" pattern, not the
                    // SplitButton-as-Save-with-format pattern).
                    #[name = "new_connection_button"]
                    pack_start = &gtk::Button {
                        set_icon_name: "list-add-symbolic",
                        set_tooltip_text: Some(crate::tr!("New connection").as_str()),
                        connect_clicked => AppMsg::OpenConnect,
                    },

                    #[name = "saved_connections_button"]
                    pack_start = &gtk::MenuButton {
                        set_icon_name: "document-open-symbolic",
                        set_tooltip_text: Some(crate::tr!("Open saved connection").as_str()),

                        #[wrap(Some)]
                        #[name = "connections_popover"]
                        set_popover = &gtk::Popover {},
                    },

                    #[name = "read_only_badge"]
                    pack_end = &gtk::Label {
                        set_visible: false,
                        set_label: &crate::tr!("Read-only"),
                        set_margin_end: 6,
                        add_css_class: "warning",
                        add_css_class: "caption-heading",
                    },

                    #[name = "row_op_spinner"]
                    pack_end = &gtk::Spinner {
                        set_visible: false,
                        set_margin_end: 6,
                        set_tooltip_text: Some(crate::tr!("Saving…").as_str()),
                    },

                    #[name = "primary_menu_button"]
                    pack_end = &gtk::MenuButton {
                        set_icon_name: "open-menu-symbolic",
                        set_tooltip_text: Some(crate::tr!("Main menu").as_str()),
                    },
                },

                #[wrap(Some)]
                #[name = "split_view"]
                set_content = &adw::OverlaySplitView {
                    set_min_sidebar_width: 220.0,
                    set_max_sidebar_width: 280.0,
                    set_show_sidebar: false,

                    // Sidebar wrapped in its own AdwToolbarView so it can
                    // carry a sidebar-local AdwHeaderBar with a search
                    // toggle — same structure GNOME Files uses for its
                    // Places sidebar. Window-decoration buttons live on
                    // the outer (main) header bar already, so we hide
                    // them here.
                    #[wrap(Some)]
                    #[name = "sidebar_root"]
                    set_sidebar = &adw::ToolbarView {
                        #[name = "sidebar_header"]
                        add_top_bar = &adw::HeaderBar {
                            set_show_start_title_buttons: false,
                            set_show_end_title_buttons: false,

                            #[wrap(Some)]
                            #[name = "sidebar_title"]
                            set_title_widget = &adw::WindowTitle {
                                set_title: &crate::tr!("Tables"),
                            },

                            #[name = "table_search_toggle"]
                            pack_end = &gtk::ToggleButton {
                                set_icon_name: "system-search-symbolic",
                                set_tooltip_text: Some(crate::tr!("Search tables").as_str()),
                            },
                        },

                        #[wrap(Some)]
                        set_content = &gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,

                            #[name = "table_search_bar"]
                            gtk::SearchBar {
                                set_show_close_button: true,
                                set_search_mode: false,

                                #[wrap(Some)]
                                #[name = "table_search"]
                                set_child = &gtk::SearchEntry {
                                    set_placeholder_text: Some(crate::tr!("Filter tables…").as_str()),
                                    set_hexpand: true,
                                },
                            },

                            #[name = "sidebar_scroll"]
                            gtk::ScrolledWindow {
                                set_hscrollbar_policy: gtk::PolicyType::Never,
                                set_vexpand: true,
                            },
                        },
                    },

                    #[wrap(Some)]
                    #[name = "toast_overlay"]
                    set_content = &adw::ToastOverlay {
                        #[wrap(Some)]
                        #[name = "content_holder"]
                        set_child = &adw::ToolbarView {
                            #[name = "reconnect_banner"]
                            add_top_bar = &adw::Banner {
                                set_revealed: false,
                                set_use_markup: false,
                                set_button_label: Some(crate::tr!("Retry").as_str()),
                            },
                            // Content is set imperatively at the end of
                            // init() — show_welcome_page swaps in the
                            // WelcomeView for the disconnected state,
                            // and on_connected swaps in the workspace
                            // tab strip on connect. The previously-
                            // inlined "Connect to a database" StatusPage
                            // here was dead UI: built once, replaced
                            // immediately, never seen.
                        },
                    },
                },
            },
        }
    }

    fn init(registry: Self::Init, root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let widgets = view_output!();

        init_css::install_pending_change_css();

        let window_handles = init_window::install_window_lifecycle(&widgets.window, &widgets.split_view, &sender);

        let sidebar = init_sidebar::build_sidebar(&widgets, &sender);

        let workspace_chrome = init_workspace::build_workspace_chrome(&widgets, &sender);

        let disconnect_action = shortcuts::install_window_actions(&widgets.window, sender.clone());
        shortcuts::install_window_shortcuts(&widgets.window);
        widgets
            .primary_menu_button
            .set_menu_model(Some(&shortcuts::primary_menu_model()));

        let welcome_view =
            WelcomeView::builder()
                .launch(WelcomeViewInit)
                .forward(sender.input_sender(), |out| match out {
                    WelcomeViewOutput::OpenConnect => AppMsg::OpenConnect,
                    WelcomeViewOutput::ImportUrl => AppMsg::ImportConnectionUrl,
                    WelcomeViewOutput::OpenSaved(saved) => AppMsg::OpenSaved(saved),
                    WelcomeViewOutput::ToggleFavorite(id) => AppMsg::ToggleConnectionFavorite(id),
                    WelcomeViewOutput::Organize(saved) => AppMsg::OrganizeConnection(saved),
                    WelcomeViewOutput::Delete(id) => AppMsg::DeleteConnection(id),
                });

        let mut model = App {
            registry,
            window: root.clone(),
            split_view: widgets.split_view.clone(),
            window_title: widgets.window_title.clone(),
            sidebar_title: widgets.sidebar_title.clone(),
            disconnect_action,
            sidebar_factory: sidebar.factory,
            sidebar_schemas: sidebar.schemas,
            sidebar_kinds: sidebar.kinds,
            sidebar_views: Vec::new(),
            content_holder: widgets.content_holder.clone(),
            toast_overlay: widgets.toast_overlay.clone(),
            connect_progress_toast: None,
            reconnect_banner: widgets.reconnect_banner.clone(),
            connections_factory: workspace_chrome.connections_factory,
            connections_popover: widgets.connections_popover.clone(),
            health_state: None,
            row_op_spinner: widgets.row_op_spinner.clone(),
            read_only_badge: widgets.read_only_badge.clone(),
            table_search: widgets.table_search.clone(),
            workspace_outer_stack: workspace_chrome.outer_stack,
            workspace_root: None,
            workspace_tab_view: None,
            workspace_root_added: std::cell::Cell::new(false),
            workspace_tabs: std::rc::Rc::new(std::cell::RefCell::new(std::collections::HashMap::new())),
            dialog: None,
            schema_buffer: build_schema_buffer(),
            favorites: Vec::new(),
            schema_index: std::rc::Rc::new(std::cell::RefCell::new(crate::ui::editor::SchemaIndex::default())),
            requested_columns: std::rc::Rc::new(std::cell::RefCell::new(std::collections::HashSet::new())),
            history_dialog: None,
            welcome_view,
            current_driver_id: None,
            table_names: Vec::new(),
            read_only: false,
            default_page_size: crate::services::preferences::load().default_page_size,
            saved_connections: Vec::new(),
            connection_organization: ConnectionOrganizationIndex::default(),
            connected: false,
            connection_id: None,
            connection_transition: ConnectionTransition::Idle,
            prepared_connection: None,
            switch_saves_pending: std::collections::HashMap::new(),
            switch_cancel_audit_was_disabled: None,
            close_after_save: window_handles.close_after_save,
            close_window_after_save: window_handles.close_window_after_save,
            in_flight_saves: window_handles.in_flight_saves,
            structure_saves_in_flight: std::rc::Rc::new(std::cell::RefCell::new(std::collections::HashSet::new())),
            persist_pending: std::rc::Rc::new(std::cell::Cell::new(false)),
            persist_timeout: std::rc::Rc::new(std::cell::RefCell::new(None)),
            persist_generation: std::rc::Rc::new(std::cell::Cell::new(0)),
            closed_tabs_stack: std::rc::Rc::new(std::cell::RefCell::new(std::collections::VecDeque::with_capacity(
                CLOSED_TABS_CAPACITY,
            ))),
            extra_windows: Vec::new(),
            poll_health_source: None,
            history_prune_source: None,
        };
        sender.input(AppMsg::ReloadConnections);
        model.show_welcome_page(sender.clone());

        widgets
            .new_connection_button
            .update_property(&[gtk::accessible::Property::Label("New connection")]);
        widgets
            .saved_connections_button
            .update_property(&[gtk::accessible::Property::Label("Open saved connection")]);
        widgets
            .primary_menu_button
            .update_property(&[gtk::accessible::Property::Label("Main menu")]);

        let banner_sender = sender.clone();
        widgets.reconnect_banner.connect_button_clicked(move |_| {
            banner_sender.input(AppMsg::RefreshPage);
        });

        let poll_sender = sender.clone();
        model.poll_health_source = Some(glib::timeout_add_seconds_local(1, move || {
            poll_sender.input(AppMsg::PollHealth);
            glib::ControlFlow::Continue
        }));

        model.history_prune_source = Some(glib::timeout_add_seconds_local(3600, || {
            let retention = crate::services::preferences::load().history_retention_days;
            relm4::spawn(async move {
                if let Err(e) = tablepro_storage::query_history::prune_older_than(retention).await {
                    tracing::warn!(error = %e, "history prune failed");
                }
            });
            glib::ControlFlow::Continue
        }));

        model.load_favorites(sender.clone());
        model.load_connection_organization(sender.clone());

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::OpenConnect => self.on_open_connect(sender),
            AppMsg::ConnectionPrepared(prepared) => self.on_connection_prepared(prepared, sender),
            AppMsg::ConnectionPrepareFailed(message) => self.on_connection_prepare_failed(message),
            AppMsg::ConnectionSwitchDecision(decision) => self.on_connection_switch_decision(decision, sender),
            AppMsg::Disconnect => self.on_disconnect(sender),
            AppMsg::ForceDisconnect => self.do_disconnect(sender),
            AppMsg::DialogClosed => self.on_connect_dialog_closed(),
            AppMsg::SelectTable {
                schema,
                name,
                open_mode,
            } => self.on_select_table(schema, name, open_mode, sender),
            AppMsg::ColumnsLoaded(tab_id, columns) => self.on_browse_columns_loaded(tab_id, columns, sender),
            AppMsg::FetchBrowseForeignKeys(tab_id) => self.fetch_browse_foreign_keys(tab_id, sender),
            AppMsg::ForeignKeysLoaded(tab_id, foreign_keys) => self.on_browse_foreign_keys_loaded(tab_id, foreign_keys),
            AppMsg::RowsLoaded(tab_id, offset, result) => self.on_browse_rows_loaded(tab_id, offset, result),
            AppMsg::LoadFailed(tab_id, msg) => self.on_browse_load_failed(tab_id, msg),
            AppMsg::RowCountLoaded(tab_id, request, count) => self.on_browse_row_count_loaded(tab_id, request, count),
            AppMsg::RowCountFailed(tab_id, request) => self.on_browse_row_count_failed(tab_id, request),
            AppMsg::FetchBrowsePage(tab_id) => self.fetch_browse_page(tab_id, sender),
            AppMsg::FetchBrowseColumns(tab_id) => self.fetch_browse_columns(tab_id, sender),
            AppMsg::FetchBrowseRowCount(tab_id) => self.fetch_browse_row_count(tab_id, sender),
            AppMsg::WorkspaceTabsChanged => self.on_workspace_tabs_changed(),
            AppMsg::WorkspaceSchemaWordsChanged => self.rebuild_schema_buffer(),
            AppMsg::ExecuteBrowseTransaction {
                tab_id,
                statements,
                sources,
            } => {
                self.on_execute_browse_transaction(tab_id, statements, sources, sender);
            }
            AppMsg::SaveCompletedForTab(tab_id, warning) => {
                self.set_row_op_in_flight(false);
                self.in_flight_saves.set(self.in_flight_saves.get().saturating_sub(1));
                // GNOME HIG toast pattern: confirm one-shot events.
                // Concurrency warning takes precedence (it implicitly
                // confirms the save *and* explains the partial-match);
                // otherwise the plain "Saved" reads as a successful
                // commit. No Undo button — the transaction has already
                // committed; users have explicit Ctrl+Z before Save.
                // 4s timeout (vs the default 5) so the success toast
                // doesn't hang around long after the user has moved on.
                let msg = warning.unwrap_or_else(|| crate::tr!("Saved"));
                let toast = adw::Toast::builder().title(msg).timeout(4).build();
                self.toast_overlay.add_toast(toast);
                self.dispatch_to_tab(tab_id, BrowseTabInput::SaveCompleted);
                // If the user picked Save in a close-confirmation
                // dialog, fire the close now that the commit succeeded.
                // The counter ensures a tab with BOTH browse-dirty and
                // structure-dirty waits for both saves before closing.
                let drained = dec_close_after_save(&mut self.close_after_save.borrow_mut(), &tab_id);
                if drained {
                    sender.input(AppMsg::WorkspaceTabClosed(tab_id));
                }
                // If we're in a window-close-Save-all flow and the map
                // just drained, the window can finally close.
                if self.close_window_after_save.get() && self.close_after_save.borrow().is_empty() {
                    self.close_window_after_save.set(false);
                    self.window.close();
                }
                self.connection_switch_save_succeeded(tab_id, sender);
            }
            AppMsg::SaveFailedForTab(tab_id, message) => {
                self.set_row_op_in_flight(false);
                self.in_flight_saves.set(self.in_flight_saves.get().saturating_sub(1));
                // Abort any close-after-save intent: the commit failed,
                // so we keep the tab open and let the user see the
                // error and retry. Window-close intent is also cleared.
                self.close_after_save.borrow_mut().remove(&tab_id);
                self.close_window_after_save.set(false);
                self.dispatch_to_tab(tab_id, BrowseTabInput::SaveFailed(message));
                self.connection_switch_save_failed();
            }
            AppMsg::FlashErrorRowForTab(tab_id, source) => {
                self.dispatch_to_tab(tab_id, BrowseTabInput::FlashErrorRow(source));
            }
            AppMsg::SaveActiveBrowseTab => {
                if let Some(id) = self.selected_browse_tab_id() {
                    self.dispatch_to_tab(id, BrowseTabInput::CommitSave);
                }
            }
            AppMsg::SaveActiveBrowseTabById(id) => {
                self.dispatch_to_tab(id, BrowseTabInput::CommitSave);
            }
            AppMsg::UndoActiveBrowseTab => {
                // Ctrl+Z routes to BrowseTab undo only. Structure
                // editing follows the snapshot+diff model — DDL undo
                // is a session-level Discard, not a per-keystroke
                // history. Inside a Structure-mode Entry, the native
                // `gtk::Text` undo handles per-field text revert.
                if let Some(id) = self.selected_browse_tab_id() {
                    self.dispatch_to_tab(id, BrowseTabInput::Undo);
                }
            }
            AppMsg::RedoActiveBrowseTab => {
                if let Some(id) = self.selected_browse_tab_id() {
                    self.dispatch_to_tab(id, BrowseTabInput::Redo);
                }
            }
            AppMsg::WorkspaceTabClosed(id) => self.close_workspace_tab_by_id(id, sender),
            AppMsg::CloseOtherWorkspaceTabs(id) => self.close_other_workspace_tabs(id, sender),
            AppMsg::CloseWorkspaceTabsToRight(id) => self.close_workspace_tabs_to_right(id, sender),
            AppMsg::CloseActiveWorkspaceTab => self.close_active_workspace_tab(sender),
            AppMsg::ShowAlert { title, body } => self.show_error_alert(&title, &body),
            AppMsg::ShowToast(msg) => self.show_toast(&msg),
            AppMsg::BrowseTabDirtyChanged(tab_id, dirty) => self.refresh_browse_tab_dirty(tab_id, dirty),
            AppMsg::NewTableTab { schema } => self.on_new_table_tab(schema, sender),
            AppMsg::EditStructureTab { schema, table } => self.on_edit_structure_tab(schema, table, sender),
            AppMsg::ShowCreateTableForExisting { schema, table } => self.on_show_create_table(schema, table, sender),
            AppMsg::ShowCreateTableLoaded { sql } => self.append_editor_tab(Some(sql), sender),
            AppMsg::DropTablePrompt { schema, table } => self.on_drop_table_prompt(schema, table, sender),
            AppMsg::DropTableConfirmed { schema, table } => self.on_drop_table_confirmed(schema, table, sender),
            AppMsg::DropTableSucceeded { schema, table } => self.on_drop_table_succeeded(schema, table, sender),
            AppMsg::ExecuteStructureTransaction { tab_id, statements } => {
                self.on_execute_structure_transaction(tab_id, statements, sender)
            }
            AppMsg::SaveActiveStructureTabById(id) => self.save_structure_tab_by_id(id, sender),
            AppMsg::StructureSaveCompleted { tab_id, new_table_name } => {
                self.on_structure_save_completed(tab_id, new_table_name, sender.clone());
                self.connection_switch_save_succeeded(tab_id, sender);
            }
            AppMsg::StructureSaveFailed(tab_id, message) => {
                self.on_structure_save_failed(tab_id, message);
                self.connection_switch_save_failed();
            }
            AppMsg::FetchStructureData { tab_id } => self.on_fetch_structure_data(tab_id, sender),
            AppMsg::StructureDataLoaded {
                tab_id,
                columns,
                indexes,
                fks,
            } => self.on_structure_data_loaded(tab_id, columns, indexes, fks),
            AppMsg::StructureLoadFailed { tab_id, message } => self.on_structure_load_failed(tab_id, message),
            AppMsg::StructureTabDirtyChanged(tab_id, dirty) => self.refresh_structure_tab_dirty(tab_id, dirty),
            AppMsg::SchemaChanged { schema, table } => self.on_schema_changed(schema, table, sender),
            AppMsg::TablesReloaded(tables) => self.on_tables_reloaded(tables),
            AppMsg::RowOpStarted => self.set_row_op_in_flight(true),
            AppMsg::ReloadConnections => self.on_reload_connections(sender),
            AppMsg::ConnectionsLoaded(connections) => {
                let conns = connections;
                self.on_connections_loaded(&conns, sender);
            }
            AppMsg::NewEditorTab => self.append_editor_tab(None, sender),
            AppMsg::EditorTabRunStateChanged(id, running) => {
                self.on_editor_tab_run_state_changed(id, running);
                self.continue_connection_switch(sender);
            }
            AppMsg::EditorTabQueryChanged(id, text) => self.on_editor_tab_query_changed(id, text),
            AppMsg::EditorNeedsColumns(tables) => self.on_editor_needs_columns(tables, sender),
            AppMsg::SchemaColumnsFetched(table, columns) => self.on_schema_columns_fetched(table, columns),
            AppMsg::FavoritesLoaded(favorites) => self.on_favorites_loaded(favorites),
            AppMsg::PersistFavorite(favorite) => self.on_persist_favorite(favorite, sender),
            AppMsg::FavoriteSaved => self.show_toast(&crate::tr!("Saved as favorite")),
            AppMsg::FavoriteSaveFailed(reason) => self.show_toast(&reason),
            AppMsg::SaveQueryAsFavorite => self.on_save_query_as_favorite(sender),
            AppMsg::ShowQuickSwitcher => self.on_show_quick_switcher(sender),
            AppMsg::QuickSwitcherChose(target) => self.on_quick_switcher_chose(target, sender),
            AppMsg::ShowHistory => self.on_show_history(sender),
            AppMsg::OpenHistoryQuery(text) => {
                if self.connected {
                    self.append_editor_tab(Some(text), sender);
                } else {
                    self.show_toast(&crate::tr!("Connect to a database first to run SQL."));
                }
            }
            AppMsg::ReplaceActiveTabQuery(text) => {
                if self.connected {
                    self.on_replace_active_tab_query(text, sender);
                } else {
                    self.show_toast(&crate::tr!("Connect to a database first to run SQL."));
                }
            }
            AppMsg::PollHealth => self.on_poll_health(),
            AppMsg::RefreshPage => self.on_refresh_active_tab(),
            AppMsg::ShowShortcuts => self.on_show_shortcuts(),
            AppMsg::ShowAbout => self.on_show_about(),
            AppMsg::ShowActivity => {
                crate::ui::activity_dialog::present(self.window.upcast_ref::<gtk::Window>(), self.connection_id);
            }
            AppMsg::ExplainActiveQuery => self.on_explain_active_query(),
            AppMsg::ShowPreferences => super::preferences::present(&self.window),
            AppMsg::NewWindow => {
                let ctrl = App::builder().launch(self.registry.clone()).detach();
                // Only the window relm4 starts the application with is
                // presented for us. A window spawned here stays unmapped
                // unless it joins the application and is presented.
                if let Some(application) = self.window.application() {
                    ctrl.widget().set_application(Some(&application));
                }
                ctrl.widget().present();
                // Dropping a Controller is what actually runs the child's
                // Component::shutdown (which closes its connection and
                // cancels its timers), but nothing does that when the user
                // just closes the window -- the GTK widget is destroyed
                // while this Vec keeps the Controller alive forever. Watch
                // for that destruction and remove our own entry so the
                // Controller drops.
                let key = ctrl.widget().as_ptr() as usize;
                let destroy_sender = sender.clone();
                ctrl.widget().connect_destroy(move |_| {
                    destroy_sender.input(AppMsg::ExtraWindowClosed(key));
                });
                self.extra_windows.push((key, ctrl));
            }
            AppMsg::ExtraWindowClosed(key) => {
                self.extra_windows.retain(|(k, _)| *k != key);
            }
            AppMsg::ExportCsv => self.on_export(ExportFormat::Csv),
            AppMsg::ExportJson => self.on_export(ExportFormat::Json),
            AppMsg::CopyToClipboard(text) => self.on_copy_to_clipboard(text),
            AppMsg::CopyRowAsInsert { tab_id, row_position } => self.on_copy_row_as_insert(tab_id, row_position),
            AppMsg::DeleteConnection(id) => self.on_delete_connection(id, sender),
            AppMsg::ConnectionOrganizationLoaded(index) => self.on_connection_organization_loaded(index),
            AppMsg::ToggleConnectionFavorite(id) => self.on_toggle_connection_favorite(id, sender),
            AppMsg::OrganizeConnection(saved) => self.on_organize_connection(saved, sender),
            AppMsg::SetConnectionOrganization(id, organization) => {
                self.on_set_connection_organization(id, organization, sender)
            }
            AppMsg::ConnectionOrganizationRejected => self.on_connection_organization_rejected(),
            AppMsg::ImportConnectionUrl => self.on_import_connection_url(sender),
            AppMsg::ImportConnectionUrlText(url) => self.on_import_connection_url_text(url, sender),
            AppMsg::ImportConnectionUrlSucceeded(name) => self.on_import_connection_url_succeeded(name),
            AppMsg::ImportConnectionUrlFailed => self.on_import_connection_url_failed(),
            AppMsg::OpenSaved(saved) => self.on_open_saved(saved, sender),
            AppMsg::ReopenClosedTab => self.on_reopen_closed_tab(sender),
            AppMsg::ShowFilterDialog => self.on_show_filter_dialog(),
        }
    }

    /// Runs once for every window, including the primary one on full
    /// application shutdown (relm4 guarantees this). Closing a window's
    /// GTK widget alone does not release what it owns: the DatabaseService
    /// entry (server session, SSH tunnel, reconnect-monitor task) stays
    /// registered under this window's connection id, and the health-poll
    /// and history-prune timers keep firing on the process-wide main
    /// context, until this runs (H12).
    fn shutdown(&mut self, _widgets: &mut Self::Widgets, _output: relm4::Sender<Self::Output>) {
        if let Some(id) = self.connection_id.take() {
            crate::services::database_service::instance().close(id);
            crate::services::window_registry::unregister(id);
        }
        if let Some(source) = self.poll_health_source.take() {
            source.remove();
        }
        if let Some(source) = self.history_prune_source.take() {
            source.remove();
        }
    }
}
