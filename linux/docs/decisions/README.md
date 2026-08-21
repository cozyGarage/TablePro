# Architecture decision records

These documents record the reasons behind major technical choices. Accepted decisions stay stable. A later decision may supersede an earlier one by linking to it explicitly.

## Format

```markdown
# 000N: Title

- **Status**: Accepted | Superseded by 000M | Deprecated
- **Date**: YYYY-MM-DD

## Context

What requires a decision.

## Decision

The selected approach.

## Rationale

Why it fits the project constraints.

## Consequences

Costs and benefits accepted by the project.

## Alternatives considered

Other options and why they were rejected.
```

## Index

| # | Title | Status | Summary |
|---|---|---|---|
| [0001](0001-no-plugin-system.md) | Database drivers are statically linked | Accepted | Driver crates are registered at compile time. |
| [0002](0002-rust-gtk4-libadwaita.md) | Rust, GTK4, and libadwaita | Accepted | Native Linux UI with a virtualized data grid. |
| [0003](0003-relm4-architecture.md) | Relm4 for application architecture | Accepted | Typed component state and async message flow. |
| [0004](0004-libsecret-secret-storage.md) | Secret Service through oo7 | Accepted | Credentials stay in the desktop keyring. |
| [0005](0005-server-side-cancellation.md) | Cancellation must reach the database | Accepted | Stop and timeouts abort the statement server-side, and only the engine's own abort error is terminal. |

## Adding a decision

1. Use the next number.
2. Copy the format above into a lowercase dash-separated filename.
3. Keep the document focused on one decision.
4. Add it to the index in the same change.
5. Submit it through the normal review process.
