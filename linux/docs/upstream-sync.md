# Optional upstream reference review

TablePro Linux is an independent Rust and GTK codebase. Other TablePro implementations may be reviewed as references for security fixes, product behavior, SQL semantics, and user expectations. This review is optional and is not a source synchronization process.

## Rules

- Never merge, rebase, or cherry-pick Apple source trees into this repository.
- Do not copy platform framework code or assume another implementation's architecture applies to Linux.
- Inspect the behavior and the reason for the change.
- Check whether the same risk or product need exists in the Rust and GTK application.
- Manually implement only the behavior that applies to Linux.
- Keep existing Linux policy, audit, storage, GTK ownership, and driver boundaries intact.
- Add Rust tests that prove the ported behavior here.
- Omit changes that depend on platform services with no Linux equivalent unless there is a clear Linux product requirement.

## Good review targets

Reference review is most useful for:

- SQL safety classification and approval rules
- Credential handling and redaction
- Connection cancellation and reconnect behavior
- TLS and SSH identity checks
- Data-grid correctness and destructive-action guards
- User-facing behavior that should be consistent across TablePro products

Packaging, desktop integration, process management, keyrings, and UI framework details should follow native Linux behavior instead.

## Recording a manual port

Add an entry only when a reference review causes a Linux code or product change. Routine Linux development does not need an entry.

```text
## YYYY-MM-DD: short behavior name

- Reference reviewed: repository, ref, and commit or release.
- Linux relevance: risk or product behavior that also applies here.
- Manual port: Rust or GTK behavior implemented in this repository.
- Not ported: platform-specific parts that do not apply.
- Verification: focused tests, real-driver fixture, or GTK flow run.
```

The entry should describe behavior, not file-by-file source movement. There should be no source-tree merge to record.

## 2026-08-18: exact wide integers, export target, and unterminated SQL literals

- Reference reviewed: `TableProApp/TablePro` `origin/main` at `f696b5f3`, commits `060e5ea4` (preserve wide integer values), `e055dcd0` (export from the database you picked), and `00885a7e` (Format Query on an unterminated SQL literal).
- Linux relevance: the GTK grid edited integer cells through a spin button backed by `f64`, so opening the editor on an integer wider than 2^53 rewrote the value; streaming CSV export read from the active connection instead of the browse tab that owns the selection; unterminated literals are a known parser hazard.
- Manual port: integer cells wider than the exact `f64` range, and values that do not parse, now open an exact text editor instead of a spin button. CSV export resolves the browse tab's own connection and reports a closed connection instead of exporting from another one.
- Not ported: macOS view models, the Liquid Glass and Open Quickly work, license gating, and the display-format state machine. Linux has no SQL formatter, so the Format Query crash has no equivalent; the statement splitter was reviewed and covered with unterminated-literal tests instead.
- Verification: `cargo test -p tablepro-app --bins` covers the numeric editor choice, export connection resolution, and unterminated literals in both splitter paths.

## 2026-08-22: data-grid and result-correctness review, v0.62.0 through v0.67.1

- Reference reviewed: `TableProApp/TablePro` `origin/main` at `159be66f5` (v0.67.1), covering the `Fixed` and `Security` entries of v0.62.0 through v0.67.1. The heaviest churn was the data grid and the SQL editor.
- Linux relevance: eleven upstream fix clusters were checked against Rust code paths. Five described a defect that also existed here; six do not apply, because the Linux architecture differs in a way that rules the defect out. Two defects found while checking were not on the upstream list at all.
- Manual port:
  - A statement boundary now follows the connected engine's quoting, so a PostgreSQL dollar-quoted body runs whole and a semicolon inside a MySQL `#` comment is not a boundary. One dialect-aware lexer in `tablepro_core::sql_lex` replaced the editor's own splitter.
  - Statements that read a host file, run a program, or send SQL to another server classify as administrative, so a read-only agent token is refused.
  - Stop and the query timeout abort the running statement on the server for MySQL, ClickHouse and SQLite, matching the PostgreSQL behaviour that was already verified. SQL Server retires the connection instead, because tiberius cannot send the TDS attention packet.
  - Copy row as INSERT omits generated columns, which every engine rejects, and escapes each value the way the connected engine reads it.
  - A cell edit is discarded if the row it opened on is no longer at that position, instead of being written to whichever row took its place.
- Not applicable, with the reason:
  - Results wrongly editable for `UNION`, joins, subqueries and CTEs: the Linux editor renders results read-only, so no table identity is ever inferred from a result set.
  - Rows-per-page overflow crashing, a page number typed for one tab applying to another, and rows past a row-count estimate being unreachable: page size comes from a fixed dropdown, pagination state is per browse tab, row counts are exact rather than estimated, and Next is enabled from the page being full rather than from a total.
  - A failing query showing a blank result area: the failing statement's own message is already rendered in its result tab.
  - ClickHouse Verify Ca falling back to public roots: a named certificate authority replaces the public roots outright, and an unreadable or empty authority file is an error.
- Found while reviewing, not on the upstream list:
  - The rendered INSERT escaped only quotes. MySQL and ClickHouse read a backslash as an escape, so a stored value ending in one closed the literal early and the rest parsed as SQL. Confirmed against MySQL 8.1, which evaluated the payload as an expression and returned 1.
  - Every SQLite column with no declared type decoded as NULL, so `count(*)` and any expression showed an empty cell.
- Verification: `tablepro_core::sql_lex` and `sql_literal` unit tests, `crates/core/tests/query_pipeline.rs`, the MySQL, ClickHouse, SQLite and SQL Server container suites, the PostgreSQL release fixture, and the installed GTK suite.
