# State management with Relm4

The `app` crate uses [Relm4](https://relm4.org) on top of gtk4-rs. The choice is recorded in [decisions/0003-relm4-architecture.md](decisions/0003-relm4-architecture.md). This file describes the patterns we follow inside the codebase. Read the official Relm4 book first; this document only covers the conventions specific to TablePro Linux.

## When to use which component flavour

| Flavour | Use when |
|---|---|
| `Component` | Synchronous init, no async work in `update`. Default choice. |
| `AsyncComponent` | Loading data on init or in update is the core of the component. Connection list, table content viewer. |
| `SimpleComponent` | A leaf widget with no `Output` to its parent. Avoid; very few cases. |
| `Factory` (`FactoryComponent`) | A homogeneous list of children driven by a model. Connection sidebar entries, tab strip items. |
| `Worker` | Background unit that does not own widgets. Use for the driver registry interaction; receives requests, returns results. |

If you find yourself reaching for a static `Mutex<AppState>`, you are not using the framework. Stop and re-read the model.

## Component skeleton

```rust
use relm4::{Component, ComponentParts, ComponentSender};

pub struct ConnectionListModel {
    connections: Vec<SavedConnection>,
    selected: Option<ConnectionId>,
}

#[derive(Debug)]
pub enum ConnectionListInput {
    Select(ConnectionId),
    Connect(ConnectionId),
    Delete(ConnectionId),
    Reload,
}

#[derive(Debug)]
pub enum ConnectionListOutput {
    OpenConnection(SavedConnection),
}

#[derive(Debug)]
pub enum ConnectionListCmd {
    Reloaded(Vec<SavedConnection>),
}

impl Component for ConnectionListModel {
    type Init = ();
    type Input = ConnectionListInput;
    type Output = ConnectionListOutput;
    type CommandOutput = ConnectionListCmd;
    type Root = gtk::Box;
    type Widgets = ConnectionListWidgets;

    fn init(_: Self::Init, root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        // Build widgets, attach handlers, return ComponentParts
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>, _: &Self::Root) {
        match msg {
            ConnectionListInput::Select(id) => self.selected = Some(id),
            ConnectionListInput::Reload => sender.command(|out, shutdown| {
                shutdown.register(async move {
                    let conns = storage::load_connections().await.unwrap_or_default();
                    out.send(ConnectionListCmd::Reloaded(conns)).ok();
                }).drop_on_shutdown()
            }),
            // ...
        }
    }

    fn update_cmd(&mut self, msg: Self::CommandOutput, _: ComponentSender<Self>, _: &Self::Root) {
        match msg {
            ConnectionListCmd::Reloaded(conns) => self.connections = conns,
        }
    }
}
```

Naming:

- Model: `<Thing>Model`. Holds private state.
- Input: `<Thing>Input`, an enum. Every UI interaction is a message.
- Output: `<Thing>Output`, an enum. Only the messages a parent should react to.
- Command output: `<Thing>Cmd`, an enum. Async work finishes by sending one of these back.

## Async work via commands

Components do not call `tokio::spawn` directly. They issue commands:

```rust
sender.command(|out, shutdown| {
    shutdown
        .register(async move {
            let result = some_async_work().await;
            out.send(MyCmd::Done(result)).ok();
        })
        .drop_on_shutdown()
})
```

The command runs on the runtime owned by `app::runtime`. It is automatically cancelled when the component is destroyed. **Always use `drop_on_shutdown`** unless the work is critical to complete (rare; saving user data is the main case).

## Talking to drivers

Driver interaction is centralised in `app::services::DatabaseService`. Components never call `core::DriverRegistry` directly. They send a request to the service worker and receive a typed reply.

```rust
let req = DatabaseRequest::FetchRows {
    connection_id,
    table: "users".into(),
    offset: 0,
    limit: 1000,
};
db_service.send(req);
// In update_cmd:
DatabaseReply::Rows { rows, .. } => { /* update model */ }
```

Why centralise: connection lifecycles, retries, health pings, cancellation, logging are concerns the UI must not see.

## State that does not belong in a component

Some state is genuinely global: open connections, the driver registry, app settings. We model these as `Worker` components or as `Arc<RwLock<T>>` owned by `app::main` and passed to component `init` payloads. Never reach for global statics other than the tokio runtime handle and the application identifier.

## Anti-patterns to flag in review

- `gtk::glib::clone!` capturing `&mut` references to model fields. Use `ComponentSender` and route via `Input`.
- Async work spawned with raw `tokio::spawn` from inside a component. Use `sender.command`.
- Components reading from a `Mutex<AppState>`. Pass state in via `Init` or via parent → child `Input`.
- A `Component` doing async work in its `init`. Promote to `AsyncComponent`.
- One enormous `Input` enum with 30 variants. Split the component.

## Testing components

Relm4 ships test helpers but they require a running GTK main loop, which is awkward in CI. Our policy:

- Test pure logic by extracting it into plain Rust functions or a separate `services` module. Test those.
- Do not write component-level tests until we hit a bug that they would have caught.
- Prefer integration tests at the driver layer and unit tests at the model layer.

## September stabilization

Browse SQL planning is pure and shared between counts and pages. A validation error clears the count through the existing request-generation failure path and surfaces the error; it cannot fall back to an unfiltered total. Delayed sidebar metadata is accepted only for the same connection allocation, so reconnecting the same saved UUID does not make an old response current. See the tests in `services/browse_query.rs` and `services/database_service.rs`.

Editor schema completion uses the same session token: allocating a fresh policy wrapper no longer invalidates its cache. Reconnect invalidates pending requests before they are applied.
