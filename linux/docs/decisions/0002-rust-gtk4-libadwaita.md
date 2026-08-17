# 0002: Rust, GTK4, and libadwaita

- **Status**: Accepted
- **Date**: 2026-04-26

## Context

The application needs a native Linux GUI with these properties:

- A virtualized grid for large database results.
- A SQL editor with highlighting and completion support.
- Keyboard, screen-reader, and input-method support.
- Native desktop integration on GNOME and KDE Plasma.
- Async database work without blocking the UI thread.
- A maintenance cost suitable for a small team.

Changing the GUI stack after product development starts would require a large rewrite.

## Decision

TablePro uses Rust, GTK4 4.14 or later, libadwaita 1.6 or later, and GtkSourceView 5.12 or later. Rust bindings come from the gtk-rs project. The application supports Linux only.

## Rationale

`GtkColumnView` provides the virtualized list and column model needed by the result grid. GtkSourceView provides the editor foundation. GTK supplies accessibility, input methods, clipboard integration, drag and drop, and desktop services without embedding a browser runtime.

libadwaita provides navigation, tab, toolbar, dialog, and preference widgets that match the selected GNOME platform baseline. GTK remains usable on KDE Plasma without a second UI implementation.

Rust provides the async and database libraries used by the static driver crates. Keeping the host and drivers in one language avoids a foreign-function boundary between the UI service layer and database operations.

## Consequences

Accepted:

- The application targets Linux only.
- GNOME behavior is the primary desktop reference.
- KDE Plasma is supported through GTK and standard desktop services.
- Wayland is the primary display path. X11 remains supported by GTK.
- GTK, libadwaita, Relm4, and system GLib requirements must be upgraded together.
- Native development packages are required for local builds.

Gained:

- Native widgets for the application shell and data grid.
- GtkSourceView for SQL editing.
- Existing accessibility and input-method integration.
- One Rust type system across services and database drivers.

## Alternatives considered

**Qt with C++.** It has a mature table widget, but it would split the application from the Rust driver ecosystem or require a large foreign-function interface.

**Slint, Iced, Floem, and egui.** Rejected because none provided the required native, accessible, virtualized database grid when the decision was made.

**Browser-based desktop shells.** Rejected because the product contract requires a native Linux interface without an embedded browser.
