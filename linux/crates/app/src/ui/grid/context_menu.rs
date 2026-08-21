use std::cell::RefCell;
use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::{self as gtk, gio, glib};

use super::GridMsg;
use super::display::POSITION_SLOT;
use super::editing::enter_edit_mode;

#[derive(Clone)]
struct CellContext {
    widget: gtk::Widget,
    col_index: usize,
    column_name: String,
}

#[derive(Clone)]
pub(super) struct GridMenus {
    context: Rc<RefCell<Option<CellContext>>>,
    editable_popover: gtk::PopoverMenu,
    readonly_popover: gtk::PopoverMenu,
    edit_action: gio::SimpleAction,
}

pub(super) fn install_grid_context_menus(column_view: &gtk::ColumnView, sender: relm4::Sender<GridMsg>) -> GridMenus {
    let context: Rc<RefCell<Option<CellContext>>> = Rc::new(RefCell::new(None));

    let editable_menu = gio::Menu::new();
    let edit_section = gio::Menu::new();
    let edit_item = gio::MenuItem::new(Some(&crate::tr!("Edit cell")), Some("cell.edit"));
    edit_item.set_attribute_value("hidden-when", Some(&"action-disabled".to_variant()));
    edit_section.append_item(&edit_item);
    editable_menu.append_section(None, &edit_section);
    let copy_section = gio::Menu::new();
    copy_section.append(Some(&crate::tr!("Copy value")), Some("cell.copy-value"));
    copy_section.append(Some(&crate::tr!("Copy column name")), Some("cell.copy-column-name"));
    copy_section.append(Some(&crate::tr!("Copy row as INSERT")), Some("cell.copy-row-insert"));
    editable_menu.append_section(None, &copy_section);
    let mutate_section = gio::Menu::new();
    mutate_section.append(Some(&crate::tr!("Insert row")), Some("cell.insert-row"));
    mutate_section.append(Some(&crate::tr!("Duplicate row")), Some("cell.duplicate-row"));
    mutate_section.append(Some(&crate::tr!("Set to NULL")), Some("cell.set-null"));
    mutate_section.append(Some(&crate::tr!("Delete row")), Some("cell.delete-row"));
    editable_menu.append_section(None, &mutate_section);

    let readonly_menu = gio::Menu::new();
    let copy_section_ro = gio::Menu::new();
    copy_section_ro.append(Some(&crate::tr!("Copy value")), Some("cell.copy-value"));
    copy_section_ro.append(Some(&crate::tr!("Copy column name")), Some("cell.copy-column-name"));
    copy_section_ro.append(Some(&crate::tr!("Copy row as INSERT")), Some("cell.copy-row-insert"));
    readonly_menu.append_section(None, &copy_section_ro);

    let empty_menu = gio::Menu::new();
    empty_menu.append(Some(&crate::tr!("Insert row")), Some("cell.insert-row"));

    let group = gio::SimpleActionGroup::new();
    let edit_action = {
        let ctx = context.clone();
        gio::ActionEntry::builder("edit")
            .activate(move |_, _, _| {
                if let Some(slot) = ctx.borrow().as_ref()
                    && let Ok(label) = slot.widget.clone().downcast::<crate::ui::cell_editor::CellEditor>()
                {
                    enter_edit_mode(&label);
                }
            })
            .build()
    };
    let copy_value_action = {
        let ctx = context.clone();
        let s = sender.clone();
        gio::ActionEntry::builder("copy-value")
            .activate(move |_, _, _| {
                if let Some(slot) = ctx.borrow().as_ref() {
                    s.send(GridMsg::CopyToClipboard(cell_text(&slot.widget))).ok();
                }
            })
            .build()
    };
    let copy_column_name_action = {
        let ctx = context.clone();
        let s = sender.clone();
        gio::ActionEntry::builder("copy-column-name")
            .activate(move |_, _, _| {
                if let Some(slot) = ctx.borrow().as_ref() {
                    s.send(GridMsg::CopyToClipboard(slot.column_name.clone())).ok();
                }
            })
            .build()
    };
    let copy_row_action = {
        let ctx = context.clone();
        let s = sender.clone();
        gio::ActionEntry::builder("copy-row-insert")
            .activate(move |_, _, _| {
                if let Some(slot) = ctx.borrow().as_ref() {
                    let position = POSITION_SLOT.get(&slot.widget).unwrap_or(0);
                    s.send(GridMsg::CopyRowAsInsert { row_position: position }).ok();
                }
            })
            .build()
    };
    let insert_row_action = {
        let s = sender.clone();
        gio::ActionEntry::builder("insert-row")
            .activate(move |_, _, _| {
                s.send(GridMsg::InsertRow).ok();
            })
            .build()
    };
    let set_null_action = {
        let ctx = context.clone();
        let s = sender.clone();
        gio::ActionEntry::builder("set-null")
            .activate(move |_, _, _| {
                if let Some(slot) = ctx.borrow().as_ref() {
                    let position = POSITION_SLOT.get(&slot.widget).unwrap_or(0);
                    s.send(GridMsg::SetCellNull {
                        row_position: position,
                        col_index: slot.col_index,
                    })
                    .ok();
                }
            })
            .build()
    };
    let delete_row_action = {
        let ctx = context.clone();
        let s = sender.clone();
        gio::ActionEntry::builder("delete-row")
            .activate(move |_, _, _| {
                if let Some(slot) = ctx.borrow().as_ref() {
                    let position = POSITION_SLOT.get(&slot.widget).unwrap_or(0);
                    s.send(GridMsg::DeleteRowAt { row_position: position }).ok();
                }
            })
            .build()
    };
    let duplicate_row_action = {
        let ctx = context.clone();
        let s = sender;
        gio::ActionEntry::builder("duplicate-row")
            .activate(move |_, _, _| {
                if let Some(slot) = ctx.borrow().as_ref() {
                    let position = POSITION_SLOT.get(&slot.widget).unwrap_or(0);
                    s.send(GridMsg::DuplicateRow { row_position: position }).ok();
                }
            })
            .build()
    };
    group.add_action_entries([
        edit_action,
        copy_value_action,
        copy_column_name_action,
        copy_row_action,
        insert_row_action,
        set_null_action,
        delete_row_action,
        duplicate_row_action,
    ]);
    column_view.insert_action_group("cell", Some(&group));

    let edit_action_obj = group
        .lookup_action("edit")
        .expect("just registered")
        .downcast::<gio::SimpleAction>()
        .expect("ActionEntry registers SimpleAction");

    let editable_popover = gtk::PopoverMenu::from_model(Some(&editable_menu));
    editable_popover.set_has_arrow(true);
    editable_popover.set_parent(column_view);
    let readonly_popover = gtk::PopoverMenu::from_model(Some(&readonly_menu));
    readonly_popover.set_has_arrow(true);
    readonly_popover.set_parent(column_view);
    let empty_popover = gtk::PopoverMenu::from_model(Some(&empty_menu));
    empty_popover.set_has_arrow(true);
    empty_popover.set_parent(column_view);

    let editable_for_destroy = editable_popover.clone();
    let readonly_for_destroy = readonly_popover.clone();
    let empty_for_destroy = empty_popover.clone();
    column_view.connect_destroy(move |_| {
        editable_for_destroy.unparent();
        readonly_for_destroy.unparent();
        empty_for_destroy.unparent();
    });

    let cv_for_empty = column_view.clone();
    let empty_for_gesture = empty_popover;
    let empty_gesture = gtk::GestureClick::builder().button(3).build();
    empty_gesture.connect_pressed(move |g, _, x, y| {
        let cv_widget: gtk::Widget = cv_for_empty.clone().upcast();
        if let Some(picked) = cv_for_empty.pick(x, y, gtk::PickFlags::DEFAULT)
            && picked != cv_widget
        {
            return;
        }
        g.set_state(gtk::EventSequenceState::Claimed);
        empty_for_gesture.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        empty_for_gesture.popup();
    });
    column_view.add_controller(empty_gesture);

    GridMenus {
        context,
        editable_popover,
        readonly_popover,
        edit_action: edit_action_obj,
    }
}

pub(super) fn attach_cell_gesture(
    widget: &gtk::Widget,
    column_view: &gtk::ColumnView,
    idx: usize,
    column_name: String,
    is_editable: bool,
    is_text_editable: bool,
    menus: &GridMenus,
) {
    let popover = if is_editable {
        menus.editable_popover.clone()
    } else {
        menus.readonly_popover.clone()
    };

    let widget_for_gesture = widget.clone();
    let cv_for_gesture = column_view.clone();
    let context_for_gesture = menus.context.clone();
    let edit_action_for_gesture = menus.edit_action.clone();
    let popover_for_gesture = popover.clone();
    let column_name_for_gesture = column_name.clone();
    let gesture = gtk::GestureClick::new();
    gesture.set_button(3);
    gesture.connect_pressed(move |g, _, x, y| {
        g.set_state(gtk::EventSequenceState::Claimed);
        *context_for_gesture.borrow_mut() = Some(CellContext {
            widget: widget_for_gesture.clone(),
            col_index: idx,
            column_name: column_name_for_gesture.clone(),
        });
        edit_action_for_gesture.set_enabled(is_text_editable);
        let local = gtk::graphene::Point::new(x as f32, y as f32);
        let (cv_x, cv_y) = widget_for_gesture
            .compute_point(&cv_for_gesture, &local)
            .map(|p| (p.x() as i32, p.y() as i32))
            .unwrap_or((x as i32, y as i32));
        popover_for_gesture.set_pointing_to(Some(&gtk::gdk::Rectangle::new(cv_x, cv_y, 1, 1)));
        popover_for_gesture.popup();
    });
    widget.add_controller(gesture);

    let widget_for_key = widget.clone();
    let cv_for_key = column_view.clone();
    let context_for_key = menus.context.clone();
    let edit_action_for_key = menus.edit_action.clone();
    let popover_for_key = popover;
    let column_name_for_key = column_name;
    let menu_shortcut = gtk::Shortcut::builder()
        .trigger(&crate::ui::shortcut::parse("Menu"))
        .action(&gtk::CallbackAction::new(move |_, _| {
            *context_for_key.borrow_mut() = Some(CellContext {
                widget: widget_for_key.clone(),
                col_index: idx,
                column_name: column_name_for_key.clone(),
            });
            edit_action_for_key.set_enabled(is_text_editable);
            if let Some(bounds) = widget_for_key.compute_bounds(&cv_for_key) {
                let rect = gtk::gdk::Rectangle::new(
                    bounds.x() as i32,
                    bounds.y() as i32,
                    bounds.width() as i32,
                    bounds.height() as i32,
                );
                popover_for_key.set_pointing_to(Some(&rect));
            } else {
                popover_for_key.set_pointing_to(None);
            }
            popover_for_key.popup();
            glib::Propagation::Stop
        }))
        .build();
    let shortcut_controller = gtk::ShortcutController::new();
    shortcut_controller.add_shortcut(menu_shortcut);
    widget.add_controller(shortcut_controller);
}

fn cell_text(widget: &gtk::Widget) -> String {
    if let Some(label) = widget.downcast_ref::<crate::ui::cell_editor::CellEditor>() {
        label.text().to_string()
    } else if let Some(label) = widget.downcast_ref::<gtk::Label>() {
        label.text().to_string()
    } else {
        String::new()
    }
}
