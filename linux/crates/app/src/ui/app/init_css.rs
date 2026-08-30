use relm4::gtk;

pub(super) fn install_pending_change_css() {
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
            }\
            .tp-env-swatch {\
                min-width: 6px;\
                border-radius: 3px;\
                margin-top: 8px;\
                margin-bottom: 8px;\
            }\
            .tp-env-local { background-color: @success_color; }\
            .tp-env-dev { background-color: @accent_color; }\
            .tp-env-staging { background-color: @warning_color; }\
            .tp-env-prod { background-color: @error_color; }",
        );
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }
}
