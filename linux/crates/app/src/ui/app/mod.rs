mod browse;
mod connection;
mod favorites;
mod msg;
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

use tablepro_core::{DriverRegistry, QueryResult, Value};
use tablepro_storage::SavedConnection;
use uuid::Uuid;

use super::browse_tab::BrowseTabInput;
use super::connect_dialog::ConnectDialog;
use super::connection_row::{ConnectionRow, ConnectionRowOutput};
use super::editor::build_schema_buffer;
use super::history_dialog::HistoryDialog;
use super::sidebar_row::{SidebarRow, SidebarRowOutput};
use super::welcome_view::{WelcomeView, WelcomeViewInit, WelcomeViewOutput};
use crate::services::database_service::ConnectionHealth;

pub use msg::AppMsg;
use types::{CLOSED_TABS_CAPACITY, ExportFormat, StatusKind};
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
    connected: bool,
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
    /// closing the primary window does not drop them mid-use.
    extra_windows: Vec<Controller<App>>,
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

        // Custom CSS for pending-changeset visual states. Native
        // Adwaita classes (.warning, .success, .error) don't compose
        // cleanly on grid cells (background colour washes the row);
        // these rules use the same accent-tinted alpha approach
        // GNOME Builder uses for diff markers.
        if let Some(display) = gtk::gdk::Display::default() {
            let provider = gtk::CssProvider::new();
            provider.load_from_string(
                ".tp-cell-modified {\
                    background: alpha(@warning_color, 0.18);\
                }\
                .tp-row-pending-insert {\
                    background: alpha(@success_color, 0.16);\
                }\
                .tp-row-pending-delete {\
                    text-decoration: line-through;\
                    color: alpha(@error_color, 0.7);\
                    background: alpha(@error_color, 0.10);\
                }\
                /* NULL sentinel: italic only — opacity already comes\
                   from the `dim-label` Adwaita class added alongside.\
                */\
                label.tp-null-sentinel {\
                    font-style: italic;\
                }\
                /* Cell focus ring. GtkColumnView's default focus chevron\
                   on cells is a 1px outline that disappears against the\
                   selected-row highlight. A 2px inset accent ring is the\
                   spreadsheet-standard focus-cell signal. Selectors are\
                   explicit to avoid stacking on `GtkCheckButton`, which\
                   already paints its own focus indicator.\
                */\
                columnview > listview > row > cell:focus-within > label,\
                columnview > listview > row > cell:focus-within > .tp-cell-editor {\
                    box-shadow: inset 0 0 0 2px @accent_color;\
                    border-radius: 2px;\
                }\
                /* One-shot flash on the row that produced a failing\
                   commit statement. Animation fades the red overlay\
                   to transparent over ~1.8s; the bind callback\
                   re-applies the class until the BrowseTab clears\
                   tracker.error_row. No leftmost ribbon — the row's\
                   background already turns red via the animation,\
                   matching the pending-state row tints which are\
                   themselves background-only (no extra gutter).\
                */\
                @keyframes tp-flash-error {\
                    0%   { background: alpha(@error_color, 0.55); }\
                    100% { background: alpha(@error_color, 0); }\
                }\
                .tp-row-leftmost-error-flash {\
                    animation: tp-flash-error 1.8s ease-out;\
                }",
            );
            gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        }

        let restored = crate::services::window_state::load();
        widgets.window.set_default_size(restored.width, restored.height);
        if restored.maximized {
            widgets.window.maximize();
        }
        // Window-close handler. Three responsibilities: persist window
        // size + maximize state, intercept close when any tab has
        // unsaved edits with a Cancel | Discard | Save dialog, and
        // route Save through the same SaveCompletedForTab plumbing as
        // a per-tab close so failures abort cleanly.
        let force_close: std::rc::Rc<std::cell::Cell<bool>> = std::rc::Rc::new(std::cell::Cell::new(false));
        let force_close_for_close = force_close.clone();
        let close_after_save_for_close: std::rc::Rc<std::cell::RefCell<std::collections::HashMap<Uuid, u32>>> =
            std::rc::Rc::new(std::cell::RefCell::new(std::collections::HashMap::new()));
        let close_window_after_save_for_close: std::rc::Rc<std::cell::Cell<bool>> =
            std::rc::Rc::new(std::cell::Cell::new(false));
        let in_flight_saves: std::rc::Rc<std::cell::Cell<usize>> = std::rc::Rc::new(std::cell::Cell::new(0));
        let close_after_save_handle = close_after_save_for_close.clone();
        let close_window_after_save_handle = close_window_after_save_for_close.clone();
        let in_flight_saves_handle = in_flight_saves.clone();
        let in_flight_saves_for_close = in_flight_saves.clone();
        let close_request_input_sender = sender.input_sender().clone();
        widgets.window.connect_close_request(move |w| {
            // If a Save is mid-flight (async transaction running), block
            // the close until it resolves. Without this, the completion
            // handler would dispatch SaveCompleted to a tab that's
            // already gone — the transaction commits in the background
            // with no UI feedback.
            if !force_close_for_close.get() && in_flight_saves_for_close.get() > 0 {
                let dialog = adw::AlertDialog::new(
                    Some(&crate::tr!("Saving in progress")),
                    Some(&crate::tr!(
                        "Waiting for pending saves to finish before closing the window."
                    )),
                );
                dialog.set_can_close(false);
                dialog.present(Some(w));
                let dialog_for_poll = dialog.clone();
                let window_for_poll = w.clone();
                let force_close_for_poll = force_close_for_close.clone();
                let in_flight_for_poll = in_flight_saves_for_close.clone();
                glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
                    if in_flight_for_poll.get() == 0 {
                        dialog_for_poll.close();
                        force_close_for_poll.set(true);
                        window_for_poll.close();
                        glib::ControlFlow::Break
                    } else {
                        glib::ControlFlow::Continue
                    }
                });
                return glib::Propagation::Stop;
            }
            // Already-confirmed close path (set by the dialog handler
            // below) — skip the guard, save state, allow close.
            // Browse + Structure tabs share the dirty-state guard:
            // either source of pending changes triggers the dialog.
            let has_pending = crate::services::change_tracker::any_pending_globally()
                || crate::services::structure_tracker::any_pending_globally();
            if !force_close_for_close.get() && has_pending {
                // Plural-form heading matches the per-tab dialog's
                // tone — factual GNOME HIG language rather than the
                // colloquial "throws them away" the body used to
                // carry. Per-tab dialog stays specific ("Save changes
                // to {name}"); window close groups across N tabs so
                // it stays generic.
                let dialog = adw::AlertDialog::new(None, None);
                dialog.set_heading(Some(&crate::tr!("Save changes before closing?")));
                dialog.set_body(&crate::tr!(
                    "One or more tabs have unsaved changes. They will be permanently lost if you discard them."
                ));
                dialog.add_response("cancel", &crate::tr!("Cancel"));
                dialog.add_response("discard", &crate::tr!("Discard"));
                dialog.add_response("save", &crate::tr!("Save"));
                dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
                dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
                dialog.set_default_response(Some("save"));
                dialog.set_close_response("cancel");
                let force_close_for_resp = force_close_for_close.clone();
                let window_for_resp = w.clone();
                let close_after_save_for_resp = close_after_save_for_close.clone();
                let close_window_after_save_for_resp = close_window_after_save_for_close.clone();
                let input_sender_for_resp = close_request_input_sender.clone();
                dialog.connect_response(None, move |dlg, response| {
                    dlg.close();
                    match response {
                        "discard" => {
                            for tab_id in crate::services::change_tracker::pending_tabs() {
                                crate::services::change_tracker::with_tab(tab_id, |t| t.clear());
                            }
                            for tab_id in crate::services::structure_tracker::pending_tabs() {
                                crate::services::structure_tracker::with_tab(tab_id, |t| t.clear());
                            }
                            force_close_for_resp.set(true);
                            // Re-fire close_request — guard sees the flag,
                            // saves window state, returns Proceed.
                            window_for_resp.close();
                        }
                        "save" => {
                            // Commit each dirty tab. Browse tabs go through
                            // SaveActiveBrowseTabById; Structure tabs need
                            // ExecuteStructureTransaction with materialized
                            // statements. close_after_save tracks both kinds;
                            // the SaveCompletedForTab / StructureSaveCompleted
                            // handlers in App::update close the window once
                            // the set drains. Any SaveFailed aborts.
                            let browse_tabs: Vec<Uuid> = crate::services::change_tracker::pending_tabs();
                            let structure_tabs: Vec<Uuid> = crate::services::structure_tracker::pending_tabs();
                            // Counter increments — a Table tab listed in both
                            // sets bumps to 2 so the window close waits for
                            // BOTH the browse save and the structure save.
                            {
                                let mut map = close_after_save_for_resp.borrow_mut();
                                for id in browse_tabs.iter().copied() {
                                    *map.entry(id).or_insert(0) += 1;
                                }
                                for id in structure_tabs.iter().copied() {
                                    *map.entry(id).or_insert(0) += 1;
                                }
                            }
                            close_window_after_save_for_resp.set(true);
                            for id in browse_tabs {
                                let _ = input_sender_for_resp.send(AppMsg::SaveActiveBrowseTabById(id));
                            }
                            for id in structure_tabs {
                                let _ = input_sender_for_resp.send(AppMsg::SaveActiveStructureTabById(id));
                            }
                        }
                        _ => {} // Cancel: do nothing, stay open.
                    }
                });
                dialog.present(Some(w));
                return glib::Propagation::Stop;
            }
            let (width, height) = if w.is_maximized() {
                (w.default_width(), w.default_height())
            } else {
                (w.width(), w.height())
            };
            crate::services::window_state::save(crate::services::window_state::WindowState {
                width,
                height,
                maximized: w.is_maximized(),
            });
            glib::Propagation::Proceed
        });

        let breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            600.0,
            adw::LengthUnit::Sp,
        ));
        breakpoint.add_setter(&widgets.split_view, "collapsed", Some(&true.into()));
        widgets.window.add_breakpoint(breakpoint);

        let sidebar_schemas: std::rc::Rc<std::cell::RefCell<Vec<Option<String>>>> =
            std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));

        let sidebar_factory: FactoryVecDeque<SidebarRow> = FactoryVecDeque::builder()
            .launch(
                gtk::ListBox::builder()
                    .selection_mode(gtk::SelectionMode::Single)
                    .activate_on_single_click(true)
                    .css_classes(["navigation-sidebar"])
                    .build(),
            )
            .forward(sender.input_sender(), |out| match out {
                // Plain click + Enter activation route through the parent
                // ListBox's `row-activated` signal (wired below), which is
                // the only signal that fires for both mouse and keyboard.
                // The factory only carries the Ctrl+click / right-click
                // "open in new tab" path.
                SidebarRowOutput::OpenInNewTab { schema, name } => AppMsg::SelectTable {
                    schema,
                    name,
                    open_mode: OpenMode::NewTab,
                },
                SidebarRowOutput::EditStructure { schema, name } => AppMsg::EditStructureTab { schema, table: name },
                SidebarRowOutput::ShowCreateTable { schema, name } => {
                    AppMsg::ShowCreateTableForExisting { schema, table: name }
                }
                SidebarRowOutput::DropTable { schema, name } => AppMsg::DropTablePrompt { schema, table: name },
            });

        let sidebar_listbox = sidebar_factory.widget();
        widgets.sidebar_scroll.set_child(Some(sidebar_listbox));

        // Plain click + Enter on focused row → SwitchOrAppend. This is
        // the single source of truth for sidebar activation; per-row
        // keybinding signals (gtk::ListBoxRow::activate) only fire on
        // Enter and would miss mouse clicks.
        let schemas_for_activate = sidebar_schemas.clone();
        let activate_sender = sender.clone();
        sidebar_listbox.connect_row_activated(move |_, row| {
            let name = row.widget_name().to_string();
            let idx = row.index() as usize;
            let schema = schemas_for_activate.borrow().get(idx).cloned().unwrap_or(None);
            activate_sender.input(AppMsg::SelectTable {
                schema,
                name,
                open_mode: OpenMode::SwitchOrAppend,
            });
        });

        let search_for_filter = widgets.table_search.clone();
        let schemas_for_filter = sidebar_schemas.clone();
        sidebar_listbox.set_filter_func(move |row| {
            let query = search_for_filter.text().to_lowercase();
            if query.is_empty() {
                return true;
            }
            // SidebarRow stashes its table name in widget-name; same
            // identifier is read by sync_sidebar_selection. Search
            // also matches the row's schema (when present) so a query
            // for "auth" surfaces every table in the auth schema, and
            // the qualified `schema.table` form so users with
            // multi-schema connections can disambiguate by typing the
            // dotted name they see in the tab title.
            let table_name = row.widget_name().to_lowercase();
            if table_name.contains(&query) {
                return true;
            }
            let schemas = schemas_for_filter.borrow();
            let idx = row.index() as usize;
            let Some(schema) = schemas.get(idx).and_then(|s| s.as_deref()) else {
                return false;
            };
            let schema_lc = schema.to_lowercase();
            schema_lc.contains(&query) || format!("{schema_lc}.{table_name}").contains(&query)
        });
        let listbox_for_invalidate = sidebar_listbox.clone();
        widgets.table_search.connect_search_changed(move |_| {
            listbox_for_invalidate.invalidate_filter();
        });
        widgets.table_search_bar.connect_entry(&widgets.table_search);
        widgets
            .table_search_bar
            .set_key_capture_widget(Some(&widgets.sidebar_root));

        // Empty-state placeholder. Shown by GtkListBox when no row is
        // visible — covers both "the database has zero tables" and
        // "the search filtered everything out". Without this, the
        // sidebar renders as a blank surface and reads as broken.
        // AdwStatusPage `.compact` is the documented empty-state
        // widget for narrow containers (matches GNOME Files's
        // sidebar-empty look).
        let sidebar_placeholder = adw::StatusPage::builder()
            .icon_name("view-list-symbolic")
            .title(crate::tr!("No tables"))
            .description(crate::tr!(
                "Nothing matches the current search, or this connection has no tables yet."
            ))
            .build();
        sidebar_placeholder.add_css_class("compact");
        sidebar_listbox.set_placeholder(Some(&sidebar_placeholder));

        // Two-way bind the sidebar header's search toggle to the SearchBar.
        // Click toggle → SearchBar reveals + entry focuses; press Esc →
        // SearchBar hides → toggle deactivates.
        widgets
            .table_search_toggle
            .bind_property("active", &widgets.table_search_bar, "search-mode-enabled")
            .bidirectional()
            .sync_create()
            .build();

        let schemas_for_header = sidebar_schemas.clone();
        let sender_for_header = sender.clone();
        sidebar_listbox.set_header_func(move |row, before| {
            let schemas = schemas_for_header.borrow();
            let total_distinct: std::collections::BTreeSet<&str> =
                schemas.iter().filter_map(|s| s.as_deref()).collect();
            // Postgres-style multi-schema connections render a header
            // per schema with a "+" button for "New Table…". Single-
            // schema connections (MySQL / SQLite) get one header
            // anchored to "main" / database-name with the same "+"
            // affordance — the visual cue matters even when there's
            // only one schema in the list.
            let multi_schema = total_distinct.len() >= 2;
            let idx = row.index();
            let current = schemas.get(idx as usize).cloned().flatten();
            let prev_idx = before.map(|b| b.index());
            let prev = prev_idx.and_then(|i| schemas.get(i as usize)).cloned().flatten();
            let needs = match (&current, &prev) {
                (Some(c), Some(p)) => c != p,
                (Some(_), None) => true,
                (None, None) => before.is_none() && !multi_schema,
                (None, Some(_)) => false,
            };
            if !needs {
                row.set_header(gtk::Widget::NONE);
                return;
            }
            let header_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(6)
                .margin_top(12)
                .margin_bottom(6)
                .margin_start(12)
                // Match the row body's `margin_end: 12` so the "+"
                // button sits flush with where row content ends — the
                // previous 6px pulled it inward of the row label edge
                // and read as a misaligned column.
                .margin_end(12)
                .build();
            let label_text = current
                .as_deref()
                .map(|s| s.to_string())
                .unwrap_or_else(|| crate::tr!("Tables"));
            let label = gtk::Label::builder()
                .label(&label_text)
                .xalign(0.0)
                .hexpand(true)
                .build();
            // GtkPlacesSidebar section-header typography: small + bold
            // + ~55% alpha. `.heading` (libadwaita's "emphasized body")
            // combined with `.dim-label` rendered as bold-dim at body
            // size — too loud for a section divider. `.caption-heading`
            // is the small-bold variant the toolkit ships for exactly
            // this purpose.
            label.add_css_class("caption-heading");
            label.add_css_class("dim-label");
            header_box.append(&label);
            // "+" button: emit NewTableTab carrying this schema. Flat
            // styling matches GNOME Files' inline-add buttons; the
            // tooltip clarifies the destination ("New Table in …")
            // so the user understands what the schema scoping means.
            let new_table_button = gtk::Button::builder()
                .icon_name("list-add-symbolic")
                .tooltip_text(match current.as_deref() {
                    Some(s) => crate::tr!("New Table in {schema}…").replace("{schema}", s),
                    None => crate::tr!("New Table…"),
                })
                .valign(gtk::Align::Center)
                .build();
            new_table_button.add_css_class("flat");
            let sender_for_button = sender_for_header.clone();
            let schema_for_button = current.clone();
            new_table_button.connect_clicked(move |_| {
                sender_for_button.input(AppMsg::NewTableTab {
                    schema: schema_for_button.clone(),
                });
            });
            header_box.append(&new_table_button);
            row.set_header(Some(&header_box));
        });

        let connections_factory: FactoryVecDeque<ConnectionRow> = FactoryVecDeque::builder()
            .launch(
                gtk::ListBox::builder()
                    .selection_mode(gtk::SelectionMode::None)
                    .css_classes(["boxed-list"])
                    .build(),
            )
            .forward(sender.input_sender(), |out| match out {
                ConnectionRowOutput::Open(saved) => AppMsg::OpenSaved(saved),
                ConnectionRowOutput::Delete(id) => AppMsg::DeleteConnection(id),
            });

        // The SplitButton's tooltip already labels the popover, so we drop
        // the in-popover "Saved Connections" header that previously sat
        // above the list. Explicit width_request prevents AdwSplitButton's
        // narrow dropdown trigger from constraining the popover width
        // (which produced mid-word hyphenation of connection names).
        let popover_content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(0)
            .margin_top(6)
            .margin_bottom(6)
            .margin_start(6)
            .margin_end(6)
            .width_request(320)
            .build();

        let scroll = gtk::ScrolledWindow::builder()
            .child(connections_factory.widget())
            .min_content_width(320)
            .min_content_height(120)
            .max_content_height(400)
            .propagate_natural_height(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        popover_content.append(&scroll);
        widgets.connections_popover.set_child(Some(&popover_content));

        // Workspace outer stack: swaps between an empty StatusPage
        // ("Select a table") when no tabs are open and the unified
        // AdwTabOverview hosting both Browse and Editor tabs. The
        // tab tree itself is built lazily on connect via
        // `ensure_workspace_root` in app/workspace_tabs.rs.
        let workspace_outer_stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .build();
        // CTA button parented inside the empty-state status page so
        // the "open editor" affordance is reachable with the mouse —
        // without it the only path was the keyboard shortcut and the
        // tab-bar "+", and the tab bar is hidden in this empty state.
        let workspace_empty_cta = gtk::Button::builder()
            .label(crate::tr!("Open SQL editor"))
            .action_name("win.open-editor")
            .halign(gtk::Align::Center)
            .build();
        workspace_empty_cta.add_css_class("suggested-action");
        workspace_empty_cta.add_css_class("pill");
        let workspace_empty_page = adw::StatusPage::builder()
            .icon_name(StatusKind::Info.icon())
            .title(crate::tr!("Select a table"))
            .description(crate::tr!(
                "Pick a table from the sidebar, or use the button below (Ctrl+T)."
            ))
            .child(&workspace_empty_cta)
            .build();
        workspace_outer_stack.add_named(&workspace_empty_page, Some("empty"));
        workspace_outer_stack.set_visible_child_name("empty");

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
                    WelcomeViewOutput::OpenSaved(saved) => AppMsg::OpenSaved(saved),
                    WelcomeViewOutput::Delete(id) => AppMsg::DeleteConnection(id),
                });

        let model = App {
            registry,
            window: root.clone(),
            split_view: widgets.split_view.clone(),
            window_title: widgets.window_title.clone(),
            sidebar_title: widgets.sidebar_title.clone(),
            disconnect_action,
            sidebar_factory,
            sidebar_schemas,
            content_holder: widgets.content_holder.clone(),
            toast_overlay: widgets.toast_overlay.clone(),
            connect_progress_toast: None,
            reconnect_banner: widgets.reconnect_banner.clone(),
            connections_factory,
            connections_popover: widgets.connections_popover.clone(),
            health_state: None,
            row_op_spinner: widgets.row_op_spinner.clone(),
            read_only_badge: widgets.read_only_badge.clone(),
            table_search: widgets.table_search.clone(),
            workspace_outer_stack,
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
            connected: false,
            close_after_save: close_after_save_handle,
            close_window_after_save: close_window_after_save_handle,
            in_flight_saves: in_flight_saves_handle,
            structure_saves_in_flight: std::rc::Rc::new(std::cell::RefCell::new(std::collections::HashSet::new())),
            persist_pending: std::rc::Rc::new(std::cell::Cell::new(false)),
            closed_tabs_stack: std::rc::Rc::new(std::cell::RefCell::new(std::collections::VecDeque::with_capacity(
                CLOSED_TABS_CAPACITY,
            ))),
            extra_windows: Vec::new(),
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
        glib::timeout_add_seconds_local(1, move || {
            poll_sender.input(AppMsg::PollHealth);
            glib::ControlFlow::Continue
        });

        glib::timeout_add_seconds_local(3600, || {
            let retention = crate::services::preferences::load().history_retention_days;
            relm4::spawn(async move {
                if let Err(e) = tablepro_storage::query_history::prune_older_than(retention).await {
                    tracing::warn!(error = %e, "history prune failed");
                }
            });
            glib::ControlFlow::Continue
        });

        model.load_favorites(sender.clone());

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::OpenConnect => self.on_open_connect(sender),
            AppMsg::Connected { tables, driver_id } => self.on_connected(tables, driver_id, sender),
            AppMsg::Disconnect => self.on_disconnect(sender),
            AppMsg::ForceDisconnect => self.do_disconnect(sender),
            AppMsg::DialogClosed => self.dialog = None,
            AppMsg::SelectTable {
                schema,
                name,
                open_mode,
            } => self.on_select_table(schema, name, open_mode, sender),
            AppMsg::ColumnsLoaded(tab_id, columns) => self.on_browse_columns_loaded(tab_id, columns),
            AppMsg::RowsLoaded(tab_id, offset, result) => self.on_browse_rows_loaded(tab_id, offset, result),
            AppMsg::LoadFailed(tab_id, msg) => self.on_browse_load_failed(tab_id, msg),
            AppMsg::RowCountLoaded(tab_id, count) => self.on_browse_row_count_loaded(tab_id, count),
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
                self.on_structure_save_completed(tab_id, new_table_name, sender)
            }
            AppMsg::StructureSaveFailed(tab_id, message) => self.on_structure_save_failed(tab_id, message),
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
            AppMsg::EditorTabRunStateChanged(id, running) => self.on_editor_tab_run_state_changed(id, running),
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
                crate::ui::activity_dialog::present(self.window.upcast_ref::<gtk::Window>());
            }
            AppMsg::ExplainActiveQuery => self.on_explain_active_query(),
            AppMsg::ShowPreferences => super::preferences::present(&self.window),
            AppMsg::NewWindow => {
                let ctrl = App::builder().launch(self.registry.clone()).detach();
                self.extra_windows.push(ctrl);
            }
            AppMsg::ExportCsv => self.on_export(ExportFormat::Csv),
            AppMsg::ExportJson => self.on_export(ExportFormat::Json),
            AppMsg::CopyToClipboard(text) => self.on_copy_to_clipboard(text),
            AppMsg::CopyRowAsInsert { tab_id, row_position } => self.on_copy_row_as_insert(tab_id, row_position),
            AppMsg::DeleteConnection(id) => self.on_delete_connection(id, sender),
            AppMsg::OpenSaved(saved) => self.on_open_saved(saved, sender),
            AppMsg::ReopenClosedTab => self.on_reopen_closed_tab(sender),
            AppMsg::ShowFilterDialog => self.on_show_filter_dialog(),
        }
    }
}

fn render_json(result: &QueryResult) -> Vec<u8> {
    let cols: Vec<&str> = result.columns.iter().map(|c| c.name.as_str()).collect();
    let rows: Vec<serde_json::Value> = result
        .rows
        .iter()
        .map(|row| {
            let mut obj = serde_json::Map::new();
            for (i, col) in cols.iter().enumerate() {
                let v = row.get(i).cloned().unwrap_or(Value::Null);
                obj.insert((*col).to_string(), value_to_json(&v));
            }
            serde_json::Value::Object(obj)
        })
        .collect();
    serde_json::to_vec_pretty(&rows).unwrap_or_default()
}

fn value_to_json(v: &Value) -> serde_json::Value {
    use serde_json::Value as J;
    match v {
        Value::Null => J::Null,
        Value::Bool(b) => J::Bool(*b),
        Value::Int(i) => J::from(*i),
        Value::Float(f) => J::from(*f),
        Value::Text(s) => J::String(s.clone()),
        Value::Bytes(b) => J::String(format!("<{} bytes>", b.len())),
        Value::Date(d) => J::String(d.to_string()),
        Value::Time(t) => J::String(t.to_string()),
        Value::DateTime(dt) => J::String(dt.to_string()),
        Value::TimestampTz(ts) => J::String(ts.to_rfc3339()),
        Value::Decimal(d) => J::String(d.to_string()),
        Value::Uuid(u) => J::String(u.to_string()),
        Value::Json(j) => j.clone(),
    }
}

fn qualified_label(schema: Option<&str>, table: &str) -> String {
    match schema {
        Some(s) => format!("{s}.{table}"),
        None => table.to_string(),
    }
}
