use relm4::adw::prelude::*;
use relm4::gtk::gio;
use relm4::{ComponentSender, adw, gtk};

use super::{App, AppMsg};

pub(super) fn primary_menu_model() -> gio::Menu {
    let menu = gio::Menu::new();
    let connection_section = gio::Menu::new();
    let disconnect_item = gio::MenuItem::new(Some(&crate::tr!("Disconnect")), Some("win.disconnect"));
    disconnect_item.set_attribute_value("hidden-when", Some(&"action-disabled".to_variant()));
    connection_section.append_item(&disconnect_item);
    menu.append_section(None, &connection_section);
    let query_section = gio::Menu::new();
    query_section.append(Some(&crate::tr!("Open Quickly")), Some("win.open-quickly"));
    query_section.append(Some(&crate::tr!("Save Query as Favorite")), Some("win.save-favorite"));
    menu.append_section(None, &query_section);
    let history_section = gio::Menu::new();
    history_section.append(Some(&crate::tr!("Query History")), Some("win.show-history"));
    history_section.append(Some(&crate::tr!("Server activity")), Some("win.show-activity"));
    history_section.append(Some(&crate::tr!("Explain query")), Some("win.explain-query"));
    menu.append_section(None, &history_section);
    let prefs_section = gio::Menu::new();
    prefs_section.append(Some(&crate::tr!("Preferences")), Some("win.preferences"));
    prefs_section.append(Some(&crate::tr!("New Window")), Some("win.new-window"));
    menu.append_section(None, &prefs_section);
    let app_section = gio::Menu::new();
    app_section.append(Some(&crate::tr!("Keyboard Shortcuts")), Some("win.shortcuts"));
    app_section.append(Some(&crate::tr!("About TablePro")), Some("win.about"));
    app_section.append(Some(&crate::tr!("Quit")), Some("win.quit"));
    menu.append_section(None, &app_section);
    menu
}

pub(super) fn install_window_actions(
    window: &adw::ApplicationWindow,
    sender: ComponentSender<App>,
) -> gio::SimpleAction {
    let group = gio::SimpleActionGroup::new();
    macro_rules! input_action {
        ($name:expr, $msg:expr) => {{
            let s = sender.clone();
            gio::ActionEntry::builder($name)
                .activate(move |_, _, _| s.input($msg))
                .build()
        }};
    }
    let window_for_quit = window.clone();
    let quit = gio::ActionEntry::builder("quit")
        .activate(move |_, _, _| window_for_quit.close())
        .build();
    group.add_action_entries([
        input_action!("shortcuts", AppMsg::ShowShortcuts),
        input_action!("about", AppMsg::ShowAbout),
        quit,
        input_action!("open-editor", AppMsg::NewEditorTab),
        input_action!("disconnect", AppMsg::Disconnect),
        input_action!("close-current", AppMsg::CloseActiveWorkspaceTab),
        input_action!("preferences", AppMsg::ShowPreferences),
        input_action!("new-window", AppMsg::NewWindow),
        input_action!("show-history", AppMsg::ShowHistory),
        input_action!("show-activity", AppMsg::ShowActivity),
        input_action!("explain-query", AppMsg::ExplainActiveQuery),
        input_action!("refresh-page", AppMsg::RefreshPage),
        input_action!("export-csv", AppMsg::ExportCsv),
        input_action!("export-json", AppMsg::ExportJson),
        input_action!("save-changes", AppMsg::SaveActiveBrowseTab),
        input_action!("undo-change", AppMsg::UndoActiveBrowseTab),
        input_action!("redo-change", AppMsg::RedoActiveBrowseTab),
        input_action!("reopen-closed-tab", AppMsg::ReopenClosedTab),
        input_action!("open-filter", AppMsg::ShowFilterDialog),
        input_action!("open-quickly", AppMsg::ShowQuickSwitcher),
        input_action!("save-favorite", AppMsg::SaveQueryAsFavorite),
    ]);
    window.insert_action_group("win", Some(&group));
    let disconnect_action = group
        .lookup_action("disconnect")
        .and_then(|a| a.downcast::<gio::SimpleAction>().ok())
        .expect("disconnect action must be a SimpleAction");
    disconnect_action.set_enabled(false);
    tracing::info!(enabled = disconnect_action.is_enabled(), "registered win.disconnect");
    disconnect_action
}

pub(super) fn install_window_shortcuts(window: &adw::ApplicationWindow) {
    let controller = gtk::ShortcutController::new();
    controller.set_scope(gtk::ShortcutScope::Global);
    for (trigger, action) in [
        ("<Primary>question", "win.shortcuts"),
        ("<Primary>slash", "win.shortcuts"),
        ("<Primary>q", "win.quit"),
        ("<Primary>w", "win.close-current"),
        ("<Primary>e", "win.open-editor"),
        ("<Primary>t", "win.open-editor"),
        ("F5", "win.refresh-page"),
        ("<Primary>f", "win.open-filter"),
        ("<Primary>comma", "win.preferences"),
        ("<Primary>h", "win.show-history"),
        ("<Primary>p", "win.open-quickly"),
        ("<Primary>d", "win.save-favorite"),
        ("<Primary>s", "win.save-changes"),
        ("<Primary>z", "win.undo-change"),
        ("<Primary>y", "win.redo-change"),
        ("<Primary><Shift>z", "win.redo-change"),
        ("<Primary><Shift>t", "win.reopen-closed-tab"),
    ] {
        controller.add_shortcut(make_shortcut(trigger, action));
    }
    window.add_controller(controller);
}

fn make_shortcut(trigger: &str, action: &str) -> gtk::Shortcut {
    gtk::Shortcut::builder()
        .trigger(&gtk::ShortcutTrigger::parse_string(trigger).expect("valid trigger"))
        .action(&gtk::NamedAction::new(action))
        .build()
}

pub(super) fn build_shortcuts_window(parent: &adw::ApplicationWindow) -> gtk::ShortcutsWindow {
    let window = gtk::ShortcutsWindow::builder()
        .modal(true)
        .transient_for(parent)
        .build();
    let section = gtk::ShortcutsSection::builder().section_name("application").build();
    let general = gtk::ShortcutsGroup::builder().title(crate::tr!("General")).build();
    for (accelerator, title) in [
        ("<Primary>e", crate::tr!("Open SQL editor")),
        ("F5", crate::tr!("Refresh table")),
        ("<Primary>comma", crate::tr!("Open Preferences")),
        ("<Primary>h", crate::tr!("Open Query History")),
        ("<Primary>p", crate::tr!("Open Quickly: favorites and open tabs")),
        ("<Primary>d", crate::tr!("Save the editor query as a favorite")),
        ("<Primary>s", crate::tr!("Save pending changes")),
        ("<Primary>z", crate::tr!("Undo pending change")),
        ("<Primary>y", crate::tr!("Redo pending change")),
        ("<Primary>question", crate::tr!("Show keyboard shortcuts")),
        ("<Primary>q", crate::tr!("Quit")),
    ] {
        general.append(&shortcut_entry(accelerator, &title));
    }
    section.append(&general);
    let browse = gtk::ShortcutsGroup::builder().title(crate::tr!("Browse table")).build();
    for (accelerator, title) in [
        ("F2", crate::tr!("Edit focused cell")),
        ("Return", crate::tr!("Edit focused cell")),
        ("Escape", crate::tr!("Cancel edit")),
        ("Tab", crate::tr!("Move to next cell (commits if editing)")),
        ("<Shift>Tab", crate::tr!("Move to previous cell (commits if editing)")),
        ("Left", crate::tr!("Move to previous cell")),
        ("Right", crate::tr!("Move to next cell")),
        ("space", crate::tr!("Toggle boolean cell")),
        ("<Primary>n", crate::tr!("Insert row")),
        ("Delete", crate::tr!("Delete selected row")),
        ("<Primary><Shift>n", crate::tr!("Set focused cell to NULL")),
        ("<Primary>f", crate::tr!("Filter rows")),
        ("<Primary>a", crate::tr!("Select all rows")),
        (
            "<Shift>Pointer_Button1",
            crate::tr!("Extend row selection to clicked row"),
        ),
        (
            "<Primary>Pointer_Button1",
            crate::tr!("Toggle clicked row in selection"),
        ),
        ("Escape", crate::tr!("Clear multi-row selection")),
        ("<Primary>c", crate::tr!("Copy selected rows as TSV")),
        ("Page_Up", crate::tr!("Previous page")),
        ("Page_Down", crate::tr!("Next page")),
        ("<Primary>Home", crate::tr!("Jump to first row of page")),
        ("<Primary>End", crate::tr!("Jump to last row of page")),
        ("<Primary>s", crate::tr!("Save pending edits")),
        ("<Primary>z", crate::tr!("Undo last change")),
        ("<Primary><Shift>z", crate::tr!("Redo last change")),
    ] {
        browse.append(&shortcut_entry(accelerator, &title));
    }
    section.append(&browse);
    let editor = gtk::ShortcutsGroup::builder().title(crate::tr!("SQL editor")).build();
    for (accelerator, title) in [
        ("<Primary>Return", crate::tr!("Run query")),
        ("<Primary><Shift>Return", crate::tr!("Run statement at cursor")),
        ("Escape", crate::tr!("Cancel running query")),
        ("<Primary>slash", crate::tr!("Toggle line comment")),
        ("<Primary>t", crate::tr!("New editor tab")),
        ("<Primary>w", crate::tr!("Close current tab or window")),
        ("<Primary>Tab", crate::tr!("Next editor tab")),
        ("<Primary><Shift>Tab", crate::tr!("Previous editor tab")),
        ("<Primary><Shift>t", crate::tr!("Reopen last closed tab")),
        ("<Primary><Shift>f", crate::tr!("Format SQL")),
    ] {
        editor.append(&shortcut_entry(accelerator, &title));
    }
    section.append(&editor);
    let structure = gtk::ShortcutsGroup::builder()
        .title(crate::tr!("Table structure"))
        .build();
    for (accelerator, title) in [
        ("<Primary>s", crate::tr!("Save pending DDL")),
        ("<Primary>z", crate::tr!("Undo DDL change")),
        ("<Primary><Shift>z", crate::tr!("Redo DDL change")),
    ] {
        structure.append(&shortcut_entry(accelerator, &title));
    }
    section.append(&structure);
    let dialogs = gtk::ShortcutsGroup::builder().title(crate::tr!("Dialogs")).build();
    dialogs.append(&shortcut_entry("Escape", &crate::tr!("Close dialog")));
    section.append(&dialogs);
    window.add_section(&section);
    window
}

fn shortcut_entry(accel: &str, title: &str) -> gtk::ShortcutsShortcut {
    gtk::ShortcutsShortcut::builder()
        .accelerator(accel)
        .title(title)
        .build()
}
