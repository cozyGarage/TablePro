use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use relm4::adw::prelude::*;
use relm4::gtk::glib;
use relm4::{ComponentSender, adw};
use uuid::Uuid;

use super::{App, AppMsg, WorkspaceTab};

/// Owning handles the model needs from the window-close plumbing: the
/// close-after-save counters, the "close the window once the map
/// drains" flag, and the in-flight transaction counter the close
/// guard polls.
pub(super) struct WindowLifecycleHandles {
    pub(super) close_after_save: Rc<RefCell<HashMap<Uuid, u32>>>,
    pub(super) close_window_after_save: Rc<Cell<bool>>,
    pub(super) in_flight_saves: Rc<Cell<usize>>,
}

pub(super) fn install_window_lifecycle(
    window: &adw::ApplicationWindow,
    split_view: &adw::OverlaySplitView,
    sender: &ComponentSender<App>,
    workspace_tabs: Rc<RefCell<HashMap<Uuid, WorkspaceTab>>>,
) -> WindowLifecycleHandles {
    let restored = crate::services::window_state::load();
    window.set_default_size(restored.width, restored.height);
    if restored.maximized {
        window.maximize();
    }
    // Window-close handler. Three responsibilities: persist window
    // size + maximize state, intercept close when any tab has
    // unsaved edits with a Cancel | Discard | Save dialog, and
    // route Save through the same SaveCompletedForTab plumbing as
    // a per-tab close so failures abort cleanly.
    let force_close: Rc<Cell<bool>> = Rc::new(Cell::new(false));
    let force_close_for_close = force_close.clone();
    let close_after_save_for_close: Rc<RefCell<HashMap<Uuid, u32>>> = Rc::new(RefCell::new(HashMap::new()));
    let close_window_after_save_for_close: Rc<Cell<bool>> = Rc::new(Cell::new(false));
    let in_flight_saves: Rc<Cell<usize>> = Rc::new(Cell::new(0));
    let handles = WindowLifecycleHandles {
        close_after_save: close_after_save_for_close.clone(),
        close_window_after_save: close_window_after_save_for_close.clone(),
        in_flight_saves: in_flight_saves.clone(),
    };
    let in_flight_saves_for_close = in_flight_saves.clone();
    let close_request_input_sender = sender.input_sender().clone();
    let workspace_tabs_for_close = workspace_tabs;
    window.connect_close_request(move |w| {
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
        let window_tab_ids: Vec<Uuid> = workspace_tabs_for_close.borrow().keys().copied().collect();
        let has_pending = crate::services::change_tracker::any_pending_in(window_tab_ids.iter().copied())
            || crate::services::structure_tracker::any_pending_in(window_tab_ids.iter().copied());
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
            let window_tab_ids_for_resp = window_tab_ids.clone();
            dialog.connect_response(None, move |dlg, response| {
                dlg.close();
                match response {
                    "discard" => {
                        for tab_id in
                            crate::services::change_tracker::pending_among(window_tab_ids_for_resp.iter().copied())
                        {
                            crate::services::change_tracker::with_tab(tab_id, |t| t.clear());
                        }
                        for tab_id in
                            crate::services::structure_tracker::pending_among(window_tab_ids_for_resp.iter().copied())
                        {
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
                        let browse_tabs: Vec<Uuid> =
                            crate::services::change_tracker::pending_among(window_tab_ids_for_resp.iter().copied());
                        let structure_tabs: Vec<Uuid> =
                            crate::services::structure_tracker::pending_among(window_tab_ids_for_resp.iter().copied());
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
        crate::services::window_state::save_geometry(width, height, w.is_maximized());
        glib::Propagation::Proceed
    });

    let breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        600.0,
        adw::LengthUnit::Sp,
    ));
    breakpoint.add_setter(split_view, "collapsed", Some(&true.into()));
    window.add_breakpoint(breakpoint);
    handles
}
