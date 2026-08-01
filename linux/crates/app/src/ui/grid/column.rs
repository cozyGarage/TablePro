use gtk4::prelude::*;

use tablepro_core::{ColumnInfo, Value};

use super::context_menu::GridMenus;
use super::display::{
    POPOVER_SLOT, POSITION_SLOT, SNAPSHOT_SLOT, SUPPRESS_SLOT, auto_filled_sentinel, editable_null_sentinel,
    value_to_display_text, value_to_edit_text,
};
use super::editing::{setup_bool_cell, setup_editable_cell, setup_readonly_cell};
use super::types::{classify_editor_kind, is_bool_type, is_bytes_type};
use super::{GridMsg, TabGridContext};

const PENDING_CSS_CLASSES: &[&str] = &[
    "tp-cell-modified",
    "tp-row-pending-delete",
    "tp-row-pending-insert",
    "tp-row-leftmost-error-flash",
];

const TOOLTIP_MIN_CHARS: usize = 40;

pub(super) fn is_cell_editable(col: &ColumnInfo) -> bool {
    !col.primary_key && !col.is_generated && !col.is_auto_increment && !is_bytes_type(&col.data_type)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn build_column(
    info: &ColumnInfo,
    idx: usize,
    editable: bool,
    table: String,
    sender: Option<relm4::Sender<GridMsg>>,
    sort_sender: Option<relm4::Sender<GridMsg>>,
    connection_id: Option<uuid::Uuid>,
    tab_ctx: TabGridContext,
    default_min_width: Option<i32>,
    column_view: gtk4::ColumnView,
    grid_menus: Option<GridMenus>,
) -> gtk4::ColumnViewColumn {
    let factory = gtk4::SignalListItemFactory::new();
    let edit_sender = if editable { sender.clone() } else { None };
    let readonly_sender = sender.clone();
    let table_for_persist = table;

    let column_data_type = info.data_type.clone();
    let column_name = info.name.clone();
    let column_view_for_setup = column_view.clone();
    let grid_menus_for_setup = grid_menus.clone();
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk4::ListItem>() else {
            return;
        };
        if let Some(edit_sender) = edit_sender.clone() {
            if is_bool_type(&column_data_type) {
                setup_bool_cell(
                    item,
                    idx,
                    column_name.clone(),
                    edit_sender,
                    &column_view_for_setup,
                    grid_menus_for_setup.as_ref(),
                );
            } else {
                let editor_kind = classify_editor_kind(&column_data_type);
                setup_editable_cell(
                    item,
                    idx,
                    column_name.clone(),
                    edit_sender,
                    editor_kind,
                    &column_view_for_setup,
                    grid_menus_for_setup.as_ref(),
                );
            }
        } else {
            setup_readonly_cell(
                item,
                idx,
                column_name.clone(),
                readonly_sender.clone(),
                &column_view_for_setup,
                grid_menus_for_setup.as_ref(),
            );
        }
    });

    let editable_for_bind = editable && sender.is_some();
    let column_auto_filled = info.is_auto_increment || info.is_generated;
    let tab_ctx_for_bind = tab_ctx.clone();
    factory.connect_bind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk4::ListItem>() else {
            return;
        };
        let Some(row) = item.item().and_downcast::<crate::ui::row_object::RowObject>() else {
            return;
        };
        let raw_value = row.cell_value(idx);
        let value = if let Some(tab_id) = tab_ctx_for_bind.tab_id
            && row.draft_id().is_none()
        {
            let pk_values: Vec<Value> = tab_ctx_for_bind
                .pk_col_indices
                .iter()
                .map(|&i| row.cell_value(i))
                .collect();
            crate::services::change_tracker::with_tab_ref(tab_id, |t| {
                crate::services::change_tracker::RowKey::from_pk_values(&pk_values)
                    .map(|key| t.current_cell_value(&key, idx, &raw_value).clone())
                    .unwrap_or_else(|| raw_value.clone())
            })
            .unwrap_or(raw_value)
        } else {
            raw_value
        };
        let is_null = matches!(value, Value::Null);
        let text = if is_null && column_auto_filled {
            auto_filled_sentinel()
        } else if editable_for_bind {
            if is_null {
                editable_null_sentinel()
            } else {
                value_to_edit_text(&value)
            }
        } else {
            value_to_display_text(&value)
        };

        let pending_classes: Vec<&'static str> = if let Some(_tab_id) = tab_ctx_for_bind.tab_id {
            if row.draft_id().is_some() {
                vec!["tp-row-pending-insert"]
            } else {
                let pk_values: Vec<Value> = tab_ctx_for_bind
                    .pk_col_indices
                    .iter()
                    .map(|&i| row.cell_value(i))
                    .collect();
                crate::services::change_tracker::with_tab_ref(_tab_id, |t| {
                    let mut v: Vec<&'static str> = Vec::new();
                    let Some(key) = crate::services::change_tracker::RowKey::from_pk_values(&pk_values) else {
                        return v;
                    };
                    let row_state = t.row_state(&key);
                    let cell_state = t.cell_state(&key, idx);
                    use crate::services::change_tracker::{CellState, RowState};
                    match (row_state, cell_state) {
                        (RowState::PendingDelete, _) => v.push("tp-row-pending-delete"),
                        (RowState::InsertDraft, _) => v.push("tp-row-pending-insert"),
                        (_, CellState::Modified) => v.push("tp-cell-modified"),
                        _ => {}
                    }
                    if idx == 0 && t.is_error_row(&key) {
                        v.push("tp-row-leftmost-error-flash");
                    }
                    v
                })
                .unwrap_or_default()
            }
        } else {
            Vec::new()
        };

        let is_pending_delete = pending_classes.contains(&"tp-row-pending-delete");
        let Some(child) = item.child() else { return };
        if let Ok(label) = child.clone().downcast::<crate::ui::cell_editor::CellEditor>() {
            label.set_text(&text);
            apply_cell_tooltip(label.upcast_ref(), &text, is_null);
            if is_null && !editable_for_bind {
                label.add_css_class("dim-label");
            } else {
                label.remove_css_class("dim-label");
            }
            if is_null && (editable_for_bind || column_auto_filled) {
                label.add_css_class("tp-null-sentinel");
            } else {
                label.remove_css_class("tp-null-sentinel");
            }
            clear_pending_classes(label.upcast_ref());
            for cls in &pending_classes {
                label.add_css_class(cls);
            }
            label.set_strikethrough(is_pending_delete);
            POSITION_SLOT.set(&label, item.position());
        } else if let Ok(checkbox) = child.clone().downcast::<gtk4::CheckButton>() {
            SUPPRESS_SLOT.set(&checkbox, true);
            match value {
                Value::Bool(true) => {
                    checkbox.set_inconsistent(false);
                    checkbox.set_active(true);
                }
                Value::Bool(false) => {
                    checkbox.set_inconsistent(false);
                    checkbox.set_active(false);
                }
                Value::Null => {
                    checkbox.set_inconsistent(true);
                    checkbox.set_active(false);
                }
                _ => {
                    checkbox.set_inconsistent(true);
                    checkbox.set_active(false);
                }
            }
            SUPPRESS_SLOT.set(&checkbox, false);
            clear_pending_classes(checkbox.upcast_ref());
            for cls in &pending_classes {
                checkbox.add_css_class(cls);
            }
            checkbox.set_opacity(if is_pending_delete { 0.5 } else { 1.0 });
            POSITION_SLOT.set(&checkbox, item.position());
        } else if let Ok(label) = child.downcast::<gtk4::Label>() {
            label.set_text(&text);
            apply_cell_tooltip(label.upcast_ref(), &text, is_null);
            if is_null {
                label.add_css_class("dim-label");
            } else {
                label.remove_css_class("dim-label");
            }
            clear_pending_classes(label.upcast_ref());
            for cls in &pending_classes {
                label.add_css_class(cls);
            }
            set_label_strikethrough(&label, is_pending_delete);
            POSITION_SLOT.set(&label, item.position());
        }
    });

    factory.connect_unbind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk4::ListItem>() else {
            return;
        };
        let Some(child) = item.child() else { return };
        if let Ok(label) = child.clone().downcast::<crate::ui::cell_editor::CellEditor>() {
            if label.is_editing() {
                label.stop_editing(false);
            }
            if let Some(popover) = POPOVER_SLOT.take(&label) {
                popover.popdown();
            }
            POSITION_SLOT.take(&label);
            SNAPSHOT_SLOT.take(&label);
        } else if let Ok(checkbox) = child.clone().downcast::<gtk4::CheckButton>() {
            POSITION_SLOT.take(&checkbox);
        } else if let Ok(label) = child.downcast::<gtk4::Label>() {
            POSITION_SLOT.take(&label);
        }
    });

    let column = gtk4::ColumnViewColumn::builder()
        .title(&info.name)
        .factory(&factory)
        .resizable(true)
        .expand(true)
        .build();
    if sort_sender.is_some() {
        let dummy = gtk4::CustomSorter::new(|_, _| gtk4::Ordering::Equal);
        column.set_sorter(Some(&dummy));
    }
    if let Some(id) = connection_id {
        if let Some(saved) = crate::services::column_widths::load(id, &table_for_persist, &info.name) {
            column.set_fixed_width(saved);
        } else if let Some(min) = default_min_width {
            column.set_fixed_width(min);
        }
        let column_for_save = column.clone();
        let column_name = info.name.clone();
        column.connect_fixed_width_notify(move |_| {
            let width = column_for_save.fixed_width();
            if width > 0 {
                crate::services::column_widths::save(id, &table_for_persist, &column_name, width);
            }
        });
    } else if let Some(min) = default_min_width {
        column.set_fixed_width(min);
    }
    column
}

fn clear_pending_classes(widget: &gtk4::Widget) {
    for cls in PENDING_CSS_CLASSES {
        widget.remove_css_class(cls);
    }
}

fn apply_cell_tooltip(widget: &gtk4::Widget, text: &str, is_null: bool) {
    if is_null || text.chars().take(TOOLTIP_MIN_CHARS + 1).count() <= TOOLTIP_MIN_CHARS {
        widget.set_tooltip_text(None);
    } else {
        widget.set_tooltip_text(Some(text));
    }
}

fn set_label_strikethrough(label: &gtk4::Label, on: bool) {
    if on {
        let attrs = gtk4::pango::AttrList::new();
        attrs.insert(gtk4::pango::AttrInt::new_strikethrough(true));
        label.set_attributes(Some(&attrs));
    } else {
        label.set_attributes(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tablepro_core::ColumnInfo;

    fn col(data_type: &str, primary_key: bool) -> ColumnInfo {
        ColumnInfo {
            name: "x".into(),
            data_type: data_type.into(),
            nullable: true,
            primary_key,
            is_auto_increment: false,
            default_value: None,
            is_generated: false,
        }
    }

    #[test]
    fn editable_for_normal_column() {
        assert!(is_cell_editable(&col("text", false)));
        assert!(is_cell_editable(&col("integer", false)));
    }

    #[test]
    fn not_editable_for_primary_key() {
        assert!(!is_cell_editable(&col("integer", true)));
    }

    #[test]
    fn not_editable_for_generated_column() {
        let mut c = col("integer", false);
        c.is_generated = true;
        assert!(!is_cell_editable(&c));
    }

    #[test]
    fn not_editable_for_auto_increment_non_pk() {
        let mut c = col("integer", false);
        c.is_auto_increment = true;
        assert!(!is_cell_editable(&c));
    }

    #[test]
    fn not_editable_for_bytes() {
        assert!(!is_cell_editable(&col("bytea", false)));
        assert!(!is_cell_editable(&col("blob", false)));
        assert!(!is_cell_editable(&col("longblob", false)));
        assert!(!is_cell_editable(&col("BINARY", false)));
        assert!(!is_cell_editable(&col("varbinary", false)));
    }
}
