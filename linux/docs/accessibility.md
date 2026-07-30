# Accessibility notes (Stage 6)

## Done in-tree

- [x] Accessible names on primary chrome (New connection, Saved connections, Main menu)
- [x] Keyboard shortcuts window (`win.shortcuts`) and standard Ctrl+T / Ctrl+W / Ctrl+S bindings
- [x] Dialogs use Adwaita `AlertDialog` / `PreferencesDialog` with default/close responses
- [x] Destructive actions use destructive appearance only on the confirm response
- [x] Status is not color-only (read-only badge uses text; reconnect banner has a button)
- [x] Multi-window via **New Window** (`win.new-window`); each window is an `ApplicationWindow` on the same `gtk::Application`

## Remaining manual pass (before Flathub)

- [ ] Orca screen-reader pass on GNOME: connect, browse, edit cell, run SQL, disconnect
- [ ] Keyboard-only Tab order through connect dialog, sidebar, grid, editor
- [ ] High contrast / `gsettings text-scaling-factor` smoke
- [ ] Accessible names on custom grid cells and popovers

gettext: `po/tablepro.pot` exists; wrap new UI strings with `tr!`. Ship English first; add LINGUAS entries as translations arrive. See [`../po/README.md`](../po/README.md).
