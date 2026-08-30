# Upstream adoption

Last updated: 2026-08-30

This is the remember-list for taking useful behavior from the macOS product
(0.68–0.69 and the commits already on their `main` after the tag) without
taking their architecture.

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
