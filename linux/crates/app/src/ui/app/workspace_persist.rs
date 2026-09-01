use std::time::Duration;

use relm4::ComponentController;
use relm4::adw::prelude::*;
use relm4::gtk::glib;

use uuid::Uuid;

use crate::services::request_generation;
use crate::services::workspace_state::{self, ConnectionWorkspaceState, WorkspaceTabRecord};

use super::types::read_workspace_tab_id;
use super::{App, WorkspaceTab};

pub(super) const PERSIST_DELAY: Duration = Duration::from_millis(500);

impl App {
    pub(super) fn persist_workspace_state(&self) {
        self.replace_persist_timer();
        let generation = self.persist_generation.get();
        self.persist_pending.set(true);
        let pending = self.persist_pending.clone();
        let timeout = self.persist_timeout.clone();
        let current_generation = self.persist_generation.clone();
        let workspace_tabs = self.workspace_tabs.clone();
        let tab_view = self.workspace_tab_view.clone();
        let connection_id = self.connection_id;
        let id = glib::timeout_add_local_once(PERSIST_DELAY, move || {
            timeout.borrow_mut().take();
            pending.set(false);
            if !request_generation::is_current(generation, current_generation.get()) {
                return;
            }
            do_persist_workspace_state(connection_id, &workspace_tabs, tab_view.as_ref());
        });
        *self.persist_timeout.borrow_mut() = Some(id);
    }

    pub(super) fn cancel_persist_timer(&self) {
        self.replace_persist_timer();
        self.persist_pending.set(false);
    }

    pub(super) fn do_persist_workspace_state_now(&self) {
        self.cancel_persist_timer();
        do_persist_workspace_state(
            self.connection_id,
            &self.workspace_tabs,
            self.workspace_tab_view.as_ref(),
        );
    }

    fn replace_persist_timer(&self) {
        if let Some(id) = self.persist_timeout.borrow_mut().take() {
            id.remove();
        }
        self.persist_generation
            .set(self.persist_generation.get().wrapping_add(1));
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
            WorkspaceTab::Editor(s) => WorkspaceTabRecord::Editor {
                query: workspace_state::bounded_query(&s.query),
            },
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
