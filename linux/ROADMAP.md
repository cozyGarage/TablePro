# Roadmap

## Where we are (2026-08-10)

The Linux app is a working GTK4 / libadwaita client, not a spike. PostgreSQL, MySQL, SQLite, MSSQL, and ClickHouse are stable, with additional experimental drivers. It includes SSH jump chains, multi-tab browsing, SQL and structure editors, inline editing, query history, policy-gated MCP and agent access, packaging scaffolding, and hundreds of tests.

Phases 0–1 and most of Phase 2 / Phase 3 from the April 2026 roadmap are
done. Remaining work pivots away from DBeaver-parity checklists toward a
**governed data plane**: one policy chokepoint every consumer (GUI, MCP
server, headless daemon, chat) must pass through, then agent access on
top.

The execution order now lives in the repository-level [`PLAN.md`](../PLAN.md). The stages below preserve the earlier roadmap history. [`docs/production-audit.md`](docs/production-audit.md) still needs the truthfulness refresh scheduled for Bookie Phase 3.

## Stage legend

Stages are ordered by dependency. Each has a concrete exit criterion.
Effort estimates assume one focused full-time engineer. Part-time
multiplies calendar time by ~3.

| Stage | Goal | Effort |
|---|---|---|
| 0 — Ground truth | Docs and CI match reality | ~3 days |
| 1 — Safety foundation | Policy engine, TLS fix, audit journal | ~4 weeks |
| 2 — MCP server | Cursor / Claude Code gated against DBs | ~4 weeks |
| 3 — Headless agentd | SRE / CI story without a desktop | ~3 weeks |
| 4 — Built-in chat | Optional GTK chat panel (lowest priority) | ~4 weeks |
| 5 — DBA / SRE / DE depth | Activity, EXPLAIN, scale, drivers | ~6 weeks |
| 6 — Daily-driver polish | Flathub, a11y, i18n finish | ~3 weeks |

---

## Stage 0 — Ground truth

**Status**: complete.

- [x] Inventory: 4 drivers, multi-tab, SSH, structure editor, history, CI
- [x] Rewrite this ROADMAP and `docs/production-audit.md`
- [x] Fix metainfo (MSSQL, release date)
- [x] MSSQL integration tests in CI
- [x] `cargo-deny` and `cargo-audit` in the fast job
- [x] Reconcile upstream Linux Kerberos support without replacing policy, MCP, audit, TLS modes, or SSH jump chains
- [x] Separate SQL Server service identity from the local SSH dial endpoint
- [x] Add Windows Kerberos authentication to GTK, saved connections, reconnect, agentd, CI, and packages
- [x] Upgrade translation startup to the corrected `gettext-rs` 0.8 safety contract

**Exit criterion**: a new contributor reading the docs is not misled about
maturity, drivers, or next priorities. **Met.**

---

## Stage 1 — Safety foundation

**Goal**: fix real safety holes; introduce one policy chokepoint. No AI yet.
This alone makes the app safer for production work.

### Remaining hardening

1. Approval routing, read-only precedence, administrative SQL classification, MCP history isolation, and partial policy inheritance remain in Bookie Phase 1.
2. Production and agent operations still need fail-closed audit behavior from Bookie Phase 2.
3. PostgreSQL `VerifyFull` through SSH still needs separate sqlx dial and service identities. Phase 0 added the core endpoint model without downgrading TLS.

### Deliverables

- [x] New crate `crates/policy`: `classify` (sqlparser AST), `PolicyGuard`,
      `rules`, `ApprovalSink`, `blast_radius`, `mask`, `policy.toml`
- [x] Absorb and delete `core::read_only`; read-only becomes one rule
- [x] `environment: Local | Dev | Staging | Prod` on saved connections
- [x] `DatabaseService.handle(id, principal) -> GuardedConnection`; raw
      connection unreachable outside the service
- [x] `TlsConfig` with `VerifyFull` default and cert-fingerprint TOFU field
- [x] Append-only hash-chained audit journal (distinct from query history)
- [x] `Connection::begin` → `Transaction` trait (default `Unsupported`)
- [x] Test matrix: classifier corpus, guard × principal × environment,
      blast radius, masking, journal integrity

**Exit criterion**: the foundation is implemented. The production-hardening exit criterion remains open until Bookie Phases 1, 2, and 4 close the policy, audit, PostgreSQL TLS, cancellation, and release-test gaps.

---

## Stage 2 — MCP server in the GTK app

**Status**: complete.

**Goal**: Cursor and Claude Code become safe against your databases.

- [x] Crate `crates/mcp` (stdio + loopback HTTP JSON-RPC)
- [x] All tools route through `PolicyGuard` via ConnectionProvider
- [x] Approval via GTK AlertDialog (elicitation-style) with statement,
      targets, estimated rows, triggering rule
- [x] Agent writes: `begin` → execute → preview → rollback (commit via
      `preview=false`)
- [x] Tokens in `$XDG_CONFIG_HOME/tablepro/mcp-tokens.json` + libsecret
      for plaintext; scopes; per-connection allowlist; rate limiting;
      loopback bind by default; Preferences → MCP pairing UI
- [x] Integration test: tool path requires provider + token; read-only
      token cannot write
- [x] Boundary documented: scopes/allowlist = who/which connection;
      `PolicyGuard` = what SQL (`ARCHITECTURE.md`)

**Tools**: `list_connections`, `list_schemas`, `list_tables`,
`describe_table`, `get_table_ddl`, `execute_query`, `execute_write`,
`explain_query`, `search_query_history`, `export_data`

**Exit criterion**: an MCP client can list and SELECT under policy, and
cannot write to Prod without an elicited approval that lands in the
journal. **Met.**

---

## Stage 3 — Headless `tablepro-agentd`

**Status**: complete.

**Goal**: SRE and CI story without a desktop.

- [x] Binary crate `crates/agentd` (core + policy + storage + mcp, no GTK)
- [x] stdio + streamable HTTP; systemd user unit; `--policy` required
- [x] Headless `ApprovalSink`: auto, deny (default), or tty prompt
- [x] Same audit journal as the GUI

**Exit criterion**: `tablepro-agentd --policy policy.toml` serves MCP on
stdio; a denied write is journalled; no GTK linkage in the binary.
**Met.**

---

## Stage 4 — Built-in chat panel (optional)

Lowest priority; Cursor already covers agent UX.

- [ ] GTK panel driving the same tool layer via in-process MCP client
- [ ] Providers: Anthropic, OpenAI-compatible, Ollama
- [ ] Per-connection AI rules as system-prompt guidance (masking remains
      the enforcement control)

---

## Stage 5 — DBA / SRE / DE depth

**Status**: complete for the plan's listed items (MVP drivers).

Reprioritized around the persona, not DBeaver parity.

- [x] Activity tab: `pg_stat_activity` / `SHOW PROCESSLIST`, blocking locks,
      long-running queries, replication lag, kill query
- [x] `EXPLAIN` / `EXPLAIN ANALYZE` plan view
- [x] Keyset pagination past OFFSET threshold
- [x] Streaming export to CSV / Parquet; true result streaming
      (CSV streams; Parquet stub returns Unsupported)
- [x] Server version and capability detection
- [x] Drivers in persona order: ClickHouse, Redis, DuckDB, MongoDB, Oracle
      (Oracle registered only with `--features odpi`; maturity matrix in
      `docs/driver-maturity.md`)
- [x] SSH jump-host chains
- [x] SQL Server Windows integrated authentication through the current Kerberos ticket cache

---

## Stage 6 — Daily-driver polish

**Status**: infrastructure complete; Flathub screenshots still need capture.

- [x] Flathub submission docs + manifest/metainfo ready
      (screenshots captured separately; see `docs/flathub.md`)
- [x] gettext infrastructure (`po/`, `tr!`); English ships via source strings
- [x] Accessibility checklist and chrome a11y labels (`docs/accessibility.md`)
- [x] Multi-window (`win.new-window`)

---

## Explicitly deferred (cut from the old Phase 5 / 6)

ER diagram, schema-editor expansion, vim mode, multi-cursor, snippets,
SQL formatter dialect matrix, drag-reorder columns, AppImage / .deb /
.rpm / AUR packaging (Flatpak is enough for the personal audience), and
an alphabetical 10-driver wishlist ahead of persona order. Parquet
export stays stubbed until arrow/parquet compile cost is justified;
CSV streaming covers the common path.

## Non-goals

- Runtime plugin system (see [ADR 0001](docs/decisions/0001-no-plugin-system.md))
- RBAC / IdP / SSO / org-managed policy distribution / SIEM shipping
  (single-user scope; policy engine and principal-aware journal leave the door open without a rewrite)
- Cross-platform Linux binary for macOS / Windows

## Realistic timeline

| Stage | Focused FT | Part-time (~3x) |
|---|---|---|
| 0 | 3 days | 1–2 weeks |
| 1–2 (agent-safe outcome) | ~8 weeks | ~6 months |
| 0–5 excl. 4 | ~24 weeks | ~18 months |
| +4 +6 | +7 weeks | +5 months |

Stages 1 and 2 deliver the entire "safely work with an agent" outcome.
Protect them from scope creep.
