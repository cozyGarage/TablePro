# Upstream adoption

Last updated: 2026-09-04

This is the remember-list for taking useful behavior from the macOS product
(0.62–0.71 and the commits already on their `main` after the v0.71.0 tag)
without taking their architecture. The 2026-08-30 review covered upstream
through roughly 0.69; the 2026-09-04 pass reviewed the 48 commits added
since (`v0.69.0` through `v0.71.0` and unreleased `main`) and added the new
"Since 0.69" table below. Re-running `git log --oneline main..origin/main`
after fetching `origin` and diffing against `release: v0.71.0` in that log
finds the next slice to review.

PLAN.md stays the source of truth for sequencing. This file records *what*
we take from upstream, *why*, and *in what order*, so a later session does
not have to reconstruct the review.

## Rules

- Review behavior. Reimplement it. Never merge their source tree.
- Every connection stays a `PolicyGuard`. GUI, MCP, and `agentd` use
  `tablepro-transport`.
- Drivers stay static workspace crates. No runtime plugins.
- Nothing ships behind an account, license, subscription, or entitlement.
- Identifiers are dialect-quoted. Database metadata is untrusted input.
- A failed catalog read must not look like an empty catalog.
- Phase 10 finishes before Phase 6 object administration (DDL). Phase 6
  object admin finishes before new drivers (Phase 7).

## Order

1. Finish the open Phase 10 holes: restore every referenced connection
   (last-connection restore already landed), then the typed activity
   console (10.3).
2. Phase 10.6 read-only schema review, PostgreSQL first: views, then
   materialized views, routines, triggers, sequences, extensions, roles,
   and grants. Capability-declared. No DDL.
3. Phase 6 structure depth from 0.69: check constraints, generated-column
   expression and stored/virtual, rename table/database/schema from the
   tree. MCP `describe_table` gains `check_constraints` and
   `generation_expression` in the same change, not as a separate agent
   feature.
4. Server-side row caps whenever a small safety slice fits. The client
   already stops materialising at `MAX_QUERY_ROWS`. The engine should
   stop sending the rest (PostgreSQL `LIMIT`, MySQL `SQL_SELECT_LIMIT`,
   same idea on other drivers).
5. SQLite over SSH (read-only remote file) after reusable SSH profiles.
6. Restore Previous Values later: local snapshot, every rewind is a
   governed write with audit. Ships with no license.
7. Copy or duplicate objects across connections only after object admin.
   Two connections means two policy contexts.

## Take

| Upstream | Linux home | Status |
|---|---|---|
| Triggers, routines, functions in the sidebar, read-only source | 10.6 then Phase 6 object admin | Not started |
| Views in the object tree | 10.6 first slice | In progress on this branch |
| Check constraints and generated columns in Structure | Phase 6 structure | Not started |
| `check_constraints` and `generation_expression` on MCP `describe_table` | Same slice as Structure | Not started |
| Rename table, database, schema from the tree | Phase 6 object admin | DDL helpers exist; no sidebar action |
| Server-side row cap (0.68.1) | Bounded operations | Not started |
| Failed structure read must not look empty | Standing invariant | Indexes and foreign keys done |
| Edit or delete without a primary key must not update every matching row | Browse writes | Set Null and delete carry `row_key`; remaining write paths still need a pass |
| Never join metadata into SQL | Standing invariant | Keep when Snowflake or others land |
| SQLite over SSH, read-only copy | Transport, after SSH profiles | Parked |
| Restore Previous Values | Browse quality, free, audited | Parked |
| Copy or duplicate across connections | After object admin | Parked |
| Pick a foreign key value from the rows it references; show a row as JSON with foreign keys expanded | Browse quality | Not started |
| Read a binary column holding valid UTF-8 as text instead of raw bytes | Browse quality | Not started |
| Exclude AUTO_INCREMENT/DEFINER from a SQL export, order exports by foreign-key dependency | SQL dump export (Priority A) | Not started |
| MSSQL sign-in with Microsoft Entra ID | Connection auth, alongside Kerberos | Not started |
| Redis Sentinel and Cluster connection modes | Redis driver depth (experimental) | Not started |
| Open a Parquet/CSV/TSV/JSON file directly as a DuckDB-backed connection | DuckDB driver depth (experimental) | Not started |
| MongoDB: nested field/array filtering, legacy binary UUID decoding, MQL-aware editor authoring | MongoDB driver depth (experimental) | Not started |
| Editor: code folding, run-statement-from-gutter, move cursor between statements, Execute All Statements | Editor productivity (Phase 6) | Not started |
| Find in results, case-sensitive data grid filters, back/forward browse history | Browse quality | Not started |
| Move the cell editor to the row above/below with arrow keys; close a tab by middle-clicking | Browse/tab quality, small GTK-native wins | Not started |

## Since 0.69 (through 0.71 and unreleased `main`)

Correctness fixes worth checking against our own drivers now, independent of
feature-phase ordering — these are bugs, not features, so they do not need
to wait for Phase 10/6 sequencing:

| Upstream fix | Check | Status |
|---|---|---|
| `fix(mongodb): implement dropObjectStatement to support dropping collections` | Does `tablepro-driver-mongodb` support dropping a collection through the structure tab's drop action, or only tables/databases it treats as SQL-shaped? | Confirmed and fixed: `execute()` only recognized `insertOne`/`deleteMany` shell forms, so Drop Table's engine-neutral `DROP TABLE IF EXISTS "name"` failed with `Unsupported`. Added a `DROP TABLE` recognizer that translates to a native collection drop |
| `fix(connections): wait for a stale tunnel process to exit before reusing its port` | Does `tablepro-ssh`'s local-port allocation risk rebinding a port a just-closed tunnel hasn't released yet? | Not applicable: `bind_local` always binds TCP to port 0 (OS-assigned ephemeral port) for every new tunnel; it never reuses a specific remembered port, so this race has no analog here |
| `fix(datagrid): refuse edits to server-owned columns at the model boundary` | Generated/identity columns are already read-only in Structure; confirm Browse's inline cell editor also refuses to open an editor on a generated column, not just on save | Already correct: `is_cell_editable` (`ui/grid/column.rs`) excludes `is_generated`/`is_auto_increment`/primary-key columns before a cell editor is ever constructed, with an existing unit test |
| `fix(datagrid): carry identity columns through so a new row pre-fills DEFAULT` | Does inserting a new Browse row already leave an identity/auto-increment column unset (DEFAULT) rather than sending an explicit value? | Already correct: `build_insert_from_draft` (`core/src/sql_dialect.rs`) unconditionally omits `is_auto_increment`/`is_generated` columns from every generated INSERT, letting the server apply its own default |

Feature-shaped items from the same slice are folded into the Take table
above, in their natural category. None of these outrank the existing
Order; they slot into Browse quality, import/export, or driver depth
whenever those come up.

Client certificate import (SSL settings for MySQL/PostgreSQL/Redis) is
already tracked, not new: see Priority A in PLAN.md and "Known seam" in
the Agent surface section.

## Do not take

- Runtime driver plugins (Turso, Kafka as downloadable engines). A static
  crate only if a real Linux workflow exists.
- Compare & Sync, seat or license UI, PRO badges, Settings > License.
- Embedded mongosh, Beancount, Liquid Glass tabs, iOS insert-row, macOS
  shortcut rebinding.
- Built-in AI chat.

## First slice (now)

PostgreSQL views, read-only:

- `Connection::list_views` defaults to empty, same pattern as indexes.
- `DatabaseDriver::supports_view_metadata` is true only when the driver
  reads the catalog.
- `PolicyGuard` wraps the list. The GUI uses `list_views_controlled`.
- The sidebar shows views under a Views heading, with Open only. No
  Drop, no Edit Structure.
- A failed view list does not pretend the database has no views.

Next after this slice: typed activity console (10.3), then the rest of
10.6 (materialized views, routines, triggers).
