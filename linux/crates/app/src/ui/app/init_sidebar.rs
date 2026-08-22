use std::cell::RefCell;
use std::rc::Rc;

use relm4::adw::prelude::*;
use relm4::factory::FactoryVecDeque;
use relm4::prelude::*;
use relm4::{adw, gtk};

use super::{App, AppMsg, AppWidgets, OpenMode};
use crate::ui::sidebar_row::{SidebarRow, SidebarRowOutput};

/// The sidebar's factory plus the parallel schema list its filter,
/// header and selection code index by row position.
pub(super) struct SidebarParts {
    pub(super) factory: FactoryVecDeque<SidebarRow>,
    pub(super) schemas: Rc<RefCell<Vec<Option<String>>>>,
}

pub(super) fn build_sidebar(widgets: &AppWidgets, sender: &ComponentSender<App>) -> SidebarParts {
    let sidebar_schemas: Rc<RefCell<Vec<Option<String>>>> = Rc::new(RefCell::new(Vec::new()));

    let sidebar_factory: FactoryVecDeque<SidebarRow> = FactoryVecDeque::builder()
        .launch(
            gtk::ListBox::builder()
                .selection_mode(gtk::SelectionMode::Single)
                .activate_on_single_click(true)
                .css_classes(["navigation-sidebar"])
                .build(),
        )
        .forward(sender.input_sender(), |out| match out {
            SidebarRowOutput::Open { schema, name } => AppMsg::SelectTable {
                schema,
                name,
                open_mode: OpenMode::SwitchOrAppend,
            },
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
        let total_distinct: std::collections::BTreeSet<&str> = schemas.iter().filter_map(|s| s.as_deref()).collect();
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
    SidebarParts {
        factory: sidebar_factory,
        schemas: sidebar_schemas,
    }
}
