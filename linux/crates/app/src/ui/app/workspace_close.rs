use relm4::adw::prelude::*;
use relm4::{ComponentSender, adw};
use uuid::Uuid;

use super::App;
use super::types::read_workspace_tab_id;

impl App {
    pub(super) fn close_active_workspace_tab(&mut self, sender: ComponentSender<Self>) {
        let Some(tab_view) = self.workspace_tab_view.as_ref() else {
            self.window.close();
            return;
        };
        let Some(page) = tab_view.selected_page() else {
            self.window.close();
            return;
        };
        tab_view.close_page(&page);
        let _ = sender;
    }

    pub(super) fn close_other_workspace_tabs(&mut self, keep_id: Uuid, _sender: ComponentSender<Self>) {
        let Some(tab_view) = self.workspace_tab_view.clone() else {
            return;
        };
        let pages = tab_view.pages();
        let mut targets = Vec::with_capacity(pages.n_items() as usize);
        for index in 0..pages.n_items() {
            let Some(page) = pages.item(index).and_downcast::<adw::TabPage>() else {
                continue;
            };
            if read_workspace_tab_id(&page) != Some(keep_id) {
                targets.push(page);
            }
        }
        for page in targets {
            tab_view.close_page(&page);
        }
    }

    pub(super) fn close_workspace_tabs_to_right(&mut self, anchor_id: Uuid, _sender: ComponentSender<Self>) {
        let Some(tab_view) = self.workspace_tab_view.clone() else {
            return;
        };
        let pages = tab_view.pages();
        let Some(anchor_index) = (0..pages.n_items()).find(|&index| {
            pages
                .item(index)
                .and_downcast::<adw::TabPage>()
                .is_some_and(|page| read_workspace_tab_id(&page) == Some(anchor_id))
        }) else {
            return;
        };
        let mut targets = Vec::new();
        for index in (anchor_index + 1)..pages.n_items() {
            if let Some(page) = pages.item(index).and_downcast::<adw::TabPage>() {
                targets.push(page);
            }
        }
        for page in targets {
            tab_view.close_page(&page);
        }
    }
}
