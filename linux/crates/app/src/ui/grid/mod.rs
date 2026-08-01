mod column;
mod context_menu;
mod display;
mod editing;
mod types;

use gtk4::prelude::*;

use tablepro_core::{ColumnInfo, QueryResult};

use crate::ui::row_object::RowObject;

use column::{build_column, is_cell_editable};
use context_menu::install_grid_context_menus;

pub use display::{editable_null_sentinel, focused_cell_coords, value_to_display_text};

#[derive(Debug)]
pub enum GridMsg {
    SortChanged(usize, bool),
    CellEdited {
        row_position: u32,
        col_index: usize,
        new_value: String,
    },
    CopyToClipboard(String),
    CopyRowAsInsert {
        row_position: u32,
    },
    SetCellNull {
        row_position: u32,
        col_index: usize,
    },
    DeleteRowAt {
        row_position: u32,
    },
    InsertRow,
    DuplicateRow {
        row_position: u32,
    },
}

#[derive(Debug, Clone, Default)]
pub struct TabGridContext {
    pub tab_id: Option<uuid::Uuid>,
    pub pk_col_indices: Vec<usize>,
}

const WIDE_TABLE_THRESHOLD: usize = 8;
const MIN_COLUMN_WIDTH_PX: i32 = 120;

#[allow(clippy::too_many_arguments)]
pub fn build_column_view(
    result: &QueryResult,
    schema_columns: &[ColumnInfo],
    table: &str,
    edit_sender: Option<relm4::Sender<GridMsg>>,
    sort: Option<(usize, bool)>,
    sort_sender: Option<relm4::Sender<GridMsg>>,
    connection_id: Option<uuid::Uuid>,
    tab_ctx: TabGridContext,
) -> (gtk4::ColumnView, gtk4::MultiSelection) {
    let store = gtk4::gio::ListStore::new::<RowObject>();
    for row in &result.rows {
        store.append(&RowObject::new(row.clone()));
    }
    let selection = gtk4::MultiSelection::new(Some(store));
    let column_view = gtk4::ColumnView::builder()
        .model(&selection)
        .show_row_separators(true)
        .show_column_separators(true)
        .build();

    let grid_menus = edit_sender
        .as_ref()
        .map(|s| install_grid_context_menus(&column_view, s.clone()));

    let default_min_width = if result.columns.len() > WIDE_TABLE_THRESHOLD {
        Some(MIN_COLUMN_WIDTH_PX)
    } else {
        None
    };
    let mut columns: Vec<gtk4::ColumnViewColumn> = Vec::with_capacity(result.columns.len());
    for (i, column) in result.columns.iter().enumerate() {
        let editable = is_cell_editable(schema_columns.get(i).unwrap_or(column));
        let col = build_column(
            column,
            i,
            editable,
            table.to_string(),
            edit_sender.clone(),
            sort_sender.clone(),
            connection_id,
            tab_ctx.clone(),
            default_min_width,
            column_view.clone(),
            grid_menus.clone(),
        );
        column_view.append_column(&col);
        columns.push(col);
    }

    if let Some((col_idx, ascending)) = sort
        && let Some(col) = columns.get(col_idx)
    {
        let direction = if ascending {
            gtk4::SortType::Ascending
        } else {
            gtk4::SortType::Descending
        };
        column_view.sort_by_column(Some(col), direction);
    }

    if let Some(app_sender) = sort_sender
        && let Some(view_sorter) = column_view
            .sorter()
            .and_then(|s| s.downcast::<gtk4::ColumnViewSorter>().ok())
    {
        let dispatch = {
            let app_sender = app_sender.clone();
            let columns = columns.clone();
            move |sorter: &gtk4::ColumnViewSorter| {
                let Some(active) = sorter.primary_sort_column() else {
                    return;
                };
                let ascending = matches!(sorter.primary_sort_order(), gtk4::SortType::Ascending);
                for (idx, col) in columns.iter().enumerate() {
                    if col == &active {
                        app_sender.send(GridMsg::SortChanged(idx, ascending)).ok();
                        break;
                    }
                }
            }
        };
        view_sorter.connect_primary_sort_column_notify({
            let dispatch = dispatch.clone();
            move |sorter| dispatch(sorter)
        });
        view_sorter.connect_primary_sort_order_notify(move |sorter| dispatch(sorter));
    }

    (column_view, selection)
}
