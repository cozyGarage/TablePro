use relm4::ComponentController;
use relm4::adw::prelude::*;
use relm4::gtk::glib;
use uuid::Uuid;

use crate::services::workspace_state::{self, ConnectionWorkspaceState, WorkspaceTabRecord};

use super::types::read_workspace_tab_id;
use super::{App, WorkspaceTab};

impl App {
    pub(super) fn persist_workspace_state(&self) {
        if self.persist_pending.get() {
            return;
        }
        self.persist_pending.set(true);
        let pending = self.persist_pending.clone();
        let workspace_tabs = self.workspace_tabs.clone();
        let tab_view = self.workspace_tab_view.clone();
        let connection_id = self.connection_id;
        glib::timeout_add_local_once(std::time::Duration::from_millis(500), move || {
            pending.set(false);
            do_persist_workspace_state(connection_id, &workspace_tabs, tab_view.as_ref());
        });
    }

    pub(super) fn do_persist_workspace_state_now(&self) {
        do_persist_workspace_state(
            self.connection_id,
            &self.workspace_tabs,
            self.workspace_tab_view.as_ref(),
        );
    }
}

fn do_persist_workspace_state(
    connection_id: Option<Uuid>,
    workspace_tabs: &std::rc::Rc<std::cell::RefCell<std::collections::HashMap<Uuid, WorkspaceTab>>>,
    tab_view: Option<&relm4::adw::TabView>,
) {
    let Some(connection_id) = connection_id else {
        return;
    };
    let tabs = workspace_tabs.borrow();
    let Some(tab_view) = tab_view else {
        return;
    };
    let pages = tab_view.pages();
    let n = pages.n_items();
    let active_page = tab_view.selected_page();
    let mut tab_records = Vec::with_capacity(n as usize);
    let mut active_idx = 0;
    for i in 0..n {
        let Some(page) = pages.item(i).and_downcast::<relm4::adw::TabPage>() else {
            continue;
        };
        if active_page.as_ref() == Some(&page) {
            active_idx = i;
        }
        let Some(id) = read_workspace_tab_id(&page) else {
            continue;
        };
        let Some(slot) = tabs.get(&id) else {
            continue;
        };
        tab_records.push(match slot {
            WorkspaceTab::Editor(s) => WorkspaceTabRecord::Editor { query: s.query.clone() },
            WorkspaceTab::Structure(_) => continue,
            WorkspaceTab::Table(s) => {
                if s.table.is_empty() {
                    continue;
                }
                let model = s.browse.model();
                let sort = model.current_sort();
                WorkspaceTabRecord::Table {
                    schema: s.schema.clone(),
                    table: s.table.clone(),
                    mode: workspace_state::PersistedTableMode::Data,
                    offset: model.current_offset(),
                    page_size: model.page_size(),
                    sort_col: sort.map(|(column, _)| column),
                    sort_asc: sort.map(|(_, ascending)| ascending),
                }
            }
        });
    }
    workspace_state::save_connection(
        connection_id,
        ConnectionWorkspaceState {
            tabs: tab_records,
            active_idx,
        },
    );
}
