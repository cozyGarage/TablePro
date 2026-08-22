# TablePro Linux — Audit and Roadmap

**Fork:** `cozyGarage/TablePro`, branch `linux`
**Upstream:** `TableProApp/TablePro`, branch `main` (macOS/iOS, Swift)
**Audit date:** 2026-08-22
**Audited commits:** fork `linux` @ `e4989b2bd`, fork `main` @ `159be66f5`, upstream `main` @ `159be66f5` (v0.67.1), upstream `linux` @ `807d28094`

---

## Context

The Linux branch of this fork is a **ground-up Rust/GTK4 rewrite**, not a port of the upstream tree. Upstream is 3,813 Swift files targeting macOS and iOS; the Linux branch is ~52,700 lines of Rust across 19 workspace crates under `linux/`. **There is no code path between them.** Every upstream feature that reaches Linux must be re-implemented behaviourally, which is exactly what `linux/docs/upstream-sync.md` already mandates.

Two things prompted this audit:

1. The branch is carrying real, verified engineering (policy chokepoint, fail-closed audit journal, server-confirmed cancellation, TLS-through-SSH) but has **never shipped a release**, and the release gate as currently written **cannot be satisfied by the CI that exists**.
2. Upstream has shipped **six releases** since this branch diverged, and the branch has no record of what is in them.

The intended outcome: a Linux branch whose core is trustworthy enough to release internally, plus a prioritised, executable backlog of upstream behaviour worth re-implementing natively.

**Decisions taken for this plan** (confirmed with the repository owner):

- **Sequencing:** stabilise the core first, but run the first port wave *in parallel* rather than blocking on the release gate.
- **Port scope, wave 1:** all four clusters — grid/pagination correctness, editor and statement navigation, sidebar/navigation/organisation, query plan + insights + autocomplete depth.
- **UI fidelity:** port the *semantics and guarantees* exactly; re-express interaction in native GTK4/libadwaita idiom. No macOS layout or keybinding mimicry.

---

## Part A — Audit findings

### A1. Repository topology and divergence

| Comparison | Result |
|---|---|
| fork `main` vs upstream `main` | **0 ahead, 0 behind** — an exact mirror of upstream v0.67.1 |
| fork `linux` vs upstream `linux` | 118 ahead, 1 behind |
| fork `linux` vs upstream `main` | 324 ahead, **317 behind** |
| merge-base (`b2f4125ae`) | 2026-08-01 — between v0.61.0 (07-30) and v0.62.0 (08-02) |

**Finding A1.1 — the gap is six releases, not one.** The question posed was "what is new in 0.67". The branch actually diverged before **v0.62.0**, so the unreviewed surface is **v0.62.0, v0.63.0, v0.64.0, v0.65.0, v0.66.0, v0.67.0 and v0.67.1**. Planning against 0.67 alone would miss Query History as a filterable drawer (0.65), query-plan result tabs (0.65), tree sidebar folders (0.66), Query Insights (0.66), the workspace rail (0.64) and Count Exactly (0.62).

**Finding A1.2 — fork `main` is a free, offline reference tree.** Because it is an exact mirror of upstream v0.67.1, an implementing agent can read the Swift behaviour locally (`git show origin/main:<path>`) without network access and without ever merging it. This is the correct way to satisfy the "inspect the behaviour and the reason for the change" rule in `linux/docs/upstream-sync.md`.

**Finding A1.3 — the branch tip is a work-in-progress commit.** `e4989b2bd "update step 7, ongoing"` touches `core/tests/query_pipeline.rs`, `policy/src/guard/connection.rs` and `storage/src/secrets.rs`. The branch tip is not a clean, described state. It is also 1 commit behind upstream `linux`.

### A2. What the core actually is

The branch's own `PLAN.md` and `linux/ROADMAP.md` are unusually honest — they distinguish *implemented* / *integrated* / *release-verified* and they do not overclaim. This audit **confirms** their inventory rather than contradicting it. Genuinely solid:

- Policy chokepoint, hash-chained audit journal, and MCP/agentd governance are **integrated**, not bolted on.
- Server-side cancellation verified against real engines on PostgreSQL, MySQL, ClickHouse, SQLite.
- PostgreSQL `VerifyFull` through an SSH tunnel, solved without patching sqlx (last hop binds a private Unix socket; host field carries the TLS service identity). This is a real piece of engineering.
- Every database call the interface starts carries a deadline, enforced by `scripts/check-bounded-operations.sh`.
- Deterministic release fixture for PostgreSQL (44 tests) plus `crates/release-tests` covering TLS identity, reconnect, transactions, MCP tools, browse filters, structure DDL.

### A3. Defects and risks found by this audit

These are **new findings**, not restatements of the branch's own roadmap.

---

**F1 — CRITICAL: the release gate cannot be satisfied by current CI.**

`ROADMAP.md` requires "30 consecutive retry-free attempts across at least six runs" at an **exact commit**. But:

- `.github/workflows/gtk-soak.yml:26` hardcodes `ref: linux`
- `.github/workflows/build-linux.yml` uses `ref: ${{ github.event_name == 'schedule' && 'linux' || github.ref }}` at all seven checkout sites

Every scheduled soak therefore checks out **the moving branch tip**, not a frozen candidate. Feature work landing on `linux` silently invalidates the ledger, and the "3 of 30 attempts across 3 of 6 runs" currently recorded in `PLAN.md` is measuring different trees. **The gate as written is unreachable.** This must be fixed before any further soak evidence is credited.

Fix: introduce a `release/*` branch, add a `ref` / commit-SHA `workflow_dispatch` input to `gtk-soak.yml`, and record the resolved SHA in the ledger for each attempt.

---

**F2 — No `[profile.release]` in `linux/Cargo.toml`.**

Confirmed still absent. Release builds get default `opt-level = 3` with `codegen-units = 16`, no LTO, no symbol stripping. `external-audit.md` flagged this and it was never closed. It is the cheapest single win available and it directly serves the "lightweight native client" goal.

---

**F3 — The panic surface is far smaller than a raw grep suggests, and should be locked in.**

A naive scan reports 362 `unwrap()` + 95 `expect(` in `src`. Excluding test modules, the real production count is **~23 sites**:

| Location | Count |
|---|---|
| `core/src/export.rs` | 13 |
| `app/src/ui/cell_editor.rs` | 6 |
| `app/src/ui/grid/context_menu.rs` | 2 |
| `app/src/ui/structure_tab/columns.rs` | 1 |
| `app/src/ui/app/shortcuts.rs` | 1 |

The remainder live in `core/src/sql_ddl/tests.rs` (70) and `policy/src/guard_tests.rs` (30) — test files that simply are not wrapped in `mod tests { }`. This is a **good** result given the branch's stated no-panic work; the action is to triage the 23, then add a guard script in the style of `check-bounded-operations.sh` so the number cannot silently grow.

---

**F4 — File-size guard will block the port work unless files are split first.**

`scripts/check-file-size.sh` warns at 1,200 lines and fails at 1,800. Current headroom is thin exactly where wave 1 lands:

| File | Lines | Headroom |
|---|---|---|
| `app/src/ui/app/mod.rs` | 1,186 | 14 |
| `app/src/ui/connect_dialog/mod.rs` | 1,118 | 82 |
| `app/src/ui/app/workspace_tabs.rs` | 1,114 | 86 |
| `app/src/ui/structure_tab/mod.rs` | 1,091 | 109 |
| `app/src/ui/browse_tab/mod.rs` | 1,032 | 168 |

Sidebar work targets `app/mod.rs`; editor-tab work targets `workspace_tabs.rs`; grid work targets `browse_tab/mod.rs`. **Extraction must precede feature code**, or every port lands as a guard failure.

---

**F5 — Oracle is a broken workspace member.**

`crates/drivers/oracle` is in `[workspace] members` but does not compile under its `odpi` feature against `oracle 0.6.3`, is never built in CI, and `ROADMAP.md` marks it "Broken". A permanently-red optional target is noise. Decide: remove it, or move it behind a documented, CI-exercised optional job.

---

**F6 — README overstates SQL Server.**

`README.md` lists SQL Server as **Stable**, but `ROADMAP.md` records that it cannot do server-side cancellation (tiberius sends no TDS attention packet), its TLS is "mapped but untested", and it cannot name a certificate authority. The two documents disagree. README should match the verified inventory.

---

**F7 — 113 `#[ignore]` tests with no ledger.**

There is no single place recording why each is ignored or what would un-ignore it. Some are legitimately environment-gated (Secret Service, Docker); others may be silently dead. Needs an inventory.

---

**F8 — Upstream removed MCP remote access in 0.67.0** ("The server binds to this Mac only"). This validates the fork's hardening direction. Worth an explicit verification that `agentd` and the MCP bridge bind local-only and cannot be configured otherwise.

### A4. Upstream feature gap, v0.62.0 → v0.67.1

Grouped by the wave-1 clusters. Issue numbers are upstream's.

**Grid, pagination and result correctness** — mostly 0.67.0
Count Exactly (0.62) · row count reflects what the grid shows and survives an empty result · estimates explicitly marked as estimates, with Last/All deferring to an exact count · locale-formatted rows-per-page that states its range · page-size change off page 1 no longer strands the user · overflowing page size no longer crashes · rows past the driver's estimate reachable · All-rows loads all of them · per-tab page number, page size, sort, hidden columns and caret all restore independently · hidden column keeps its ordinal · sort survives hide/show · column virtualization for hundreds of columns (#1219) · `Cmd+F` find bar over results plus server-side Search All Rows · edits route to the owning result's own table · results whose table cannot be identified are not editable · JSON result mode shows the grid's rows in the grid's order (#2251, #2244)

**Editor and statement navigation** — 0.67.0
Current-statement highlight and a per-statement gutter run button (#2278) · statement navigation shortcuts (#2279) · Execute All Statements (#2230) · result tabs named after the statement that produced them, with click-to-cursor (#2280) · code folding · drag-to-reorder editor tabs with Move Left/Right · `BEGIN ... END` runs whole; a semicolon inside a MySQL `#` comment is not a boundary (#2278) · a commit-time failure reports itself rather than blaming the last successful statement (#2280) · reopening an already-open `.sql` file focuses its tab · editor tab context menu

> Partly already done: the branch has a dialect-aware lexer, so a PostgreSQL function body already runs whole. **Verify with tests before reimplementing.**

**Sidebar, navigation and organisation** — 0.64.0–0.67.0
Tree sidebar folders grouping tables/views/materialized views/foreign tables/procedures/functions (#1590) · multi-select databases and schemas with bulk drop/refresh/copy/export · type-to-jump in the tree · browse Back/Forward history (#2316) · Quick Switcher across *every* open connection · workspace rail listing open connections and databases (#1282) · Disconnect/Reconnect · double-click or Return keeps the tab instead of reusing a preview tab (#2235) · Open in New Tab · tabs carry the database name when objects collide across databases, and picking a database returns you to its last-used tab (#2217) · right-click highlights the target row and works on empty space · connection groups/tags/favorites

**Query plan, insights and autocomplete depth** — 0.65.0–0.66.0
Query plan opens as a *pinnable result tab* beside the data rather than a modal · plan tree with sortable Operation/Cost/Rows/Actual-Time columns, keyboard navigation, copy-step and a resizable detail panel · cost badge escalating in **both shape and colour** (never colour alone) · Query Insights: local aggregation over query history — most-run, slowest, regressed, failing, with queries differing only in literals normalised together (#2107) · PostgreSQL completion depth: operators including `::`, JSON/array/range containment, regex and full-text search, ~400 builtins, multi-word syntax such as `ON CONFLICT DO UPDATE SET` (#2095) · enum labels suggested when comparing against an enum column (#2095) · structural diagnostics underlining unclosed brackets and comments without ever flagging a half-written statement (#2095)

**Deliberately out of wave 1** (recorded so nothing is lost): MCP 2026-07-28 protocol revision + 27 new tools + prompt templates + progress notifications; native charts over results; background-completion notifications; import/export and backup/restore; Redis Sentinel and Cluster; MongoDB nested/array-element filters; DuckDB opening Parquet/CSV/JSON read-only; Query History as a filterable paged drawer.

---

## Part B — Roadmap

Two tracks run concurrently. **Track 1 blocks the release; Track 2 does not.**

### Track 1 — Make the core releasable

*Goal: the release gate becomes reachable, and the evidence behind it becomes real.*

**T1.1 — Fix the soak gate (blocks everything else in Track 1)**
- Cut `release/0.1.0-rc1` from a described, non-WIP commit on `linux`.
- Add a `workflow_dispatch` commit/ref input to `.github/workflows/gtk-soak.yml`; replace the hardcoded `ref: linux` at line 26.
- Replace the `github.event_name == 'schedule' && 'linux'` fallback in `build-linux.yml` with the same explicit input.
- Have every job print and record its resolved SHA.
- **Reset the soak ledger to 0/30.** The existing 3/30 measured a moving tree and cannot be carried over.
- *Done when:* two consecutive scheduled soaks report the same SHA, and the ledger records it.

**T1.2 — Clean the branch tip**
- Finish or revert `e4989b2bd "update step 7, ongoing"`; the tip must be a described state.
- Reconcile the 1 commit behind upstream `linux`.

**T1.3 — Add `[profile.release]`** to `linux/Cargo.toml`: `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`, `strip = "symbols"`, `opt-level = 3`.
- Verify `panic = "abort"` against the poisoned-lock recovery in `app` and the Relm4 worker boundaries — if any recovery path depends on unwinding, keep `panic = "unwind"` and say so in an ADR.
- *Done when:* startup time and RSS are measured before/after (`hyperfine`, `smem`) and recorded in `linux/docs/`.

**T1.4 — Triage the 23 production panic sites** (F3), starting with the 13 in `core/src/export.rs`. Convert to typed errors on `tablepro_core::error`. Then add `scripts/check-panic-sites.sh` modelled on `check-bounded-operations.sh`, with a baseline file, and wire it into preflight.

**T1.5 — Split the five files at the size ceiling** (F4) *before* any Track 2 code lands:
- `app/src/ui/app/mod.rs` → extract sidebar/tree handling
- `app/src/ui/app/workspace_tabs.rs` → extract tab lifecycle
- `app/src/ui/browse_tab/mod.rs` → extract pagination/status-bar state
- Pure refactor. No behaviour change. Follow the pattern already used for `connect_dialog` endpoint form state in `4fb5cdde7`.

**T1.6 — Reconcile documentation with evidence** (F5, F6, F7)
- README driver table must match `ROADMAP.md`'s verified inventory; SQL Server is not Stable while cancellation and TLS are unproven.
- Decide Oracle: remove from `[workspace] members`, or add a CI job that actually builds it.
- Produce `linux/docs/ignored-tests.md` listing all 113 `#[ignore]` tests with the reason and the un-ignore condition.

**T1.7 — Verify MCP/agentd is local-only** (F8) and add a regression test that a non-local bind is refused.

**T1.8 — Installed-package verification** (already on the roadmap; unchanged)
- Install, upgrade, rollback and Wayland verification for the internal Arch RC via `scripts/validate-arch-package.sh`.

### Track 2 — Port wave 1

*Runs in parallel on `linux`. The RC stays frozen on `release/*`.*

**Rules for every item in this track:**

1. Read the upstream behaviour from `git show origin/main:<path>` — fork `main` is an exact v0.67.1 mirror (A1.2). **Never merge, rebase or cherry-pick Swift into `linux`.**
2. Implement the *semantics*; express the UI in GTK4/libadwaita idiom. `AdwToastOverlay` not macOS alerts, `AdwTabView` not NSTabView, GNOME shortcut conventions not `Cmd+` mappings.
3. Every port lands with Rust tests. UI-observable behaviour lands with a scenario in the installed GTK suite (`scripts/test-gtk-safety.sh`).
4. Record each port in `linux/docs/upstream-sync.md` using its existing template — behaviour, not file movement.
5. Nothing bypasses the policy chokepoint. Ported features acquire connections through the same governed handles.

**T2.1 — Grid and pagination correctness** *(highest value: these are correctness bugs, not new surface)*
- Files: `core/src/pagination.rs`, `app/src/ui/browse_tab/{mod,chrome,grid_render}.rs`, `app/src/ui/grid/*`
- Separate *estimated* from *exact* row totals as distinct types in `core`, so an estimate can never be rendered as a count. Add Count Exactly.
- Make page number, page size, sort, hidden columns and caret **per-tab** state, keyed by tab, and restore them independently.
- Ensure a hidden column keeps its ordinal and that sort survives hide/show.
- Reject overflowing page sizes at the type boundary.
- Bind edits to the owning result's table identity; a result whose table cannot be resolved is read-only.
- Virtualize column construction for wide results (#1219).
- Find bar over results with server-side Search All Rows.
- *Done when:* the PostgreSQL release fixture covers estimate-vs-exact, per-tab restore, and edit routing; the GTK suite covers find-bar and wide-result scroll.

**T2.2 — Editor and statement navigation**
- Files: `app/src/ui/editor/*`, `core/src/sql_lex.rs`, `core/src/sql_dialect.rs`, `app/src/ui/app/workspace_tabs.rs`
- **First**, add tests asserting current splitter behaviour for `BEGIN ... END` and MySQL `#` comments — the dialect-aware lexer may already be correct.
- Current-statement highlight driven by the existing lexer; per-statement run action in the GtkSourceView gutter.
- Statement navigation actions (next/previous/run-current) bound to GNOME-conventional accelerators.
- Execute All Statements.
- Name result tabs after their statement; selecting one moves the editor cursor to it.
- Code folding via GtkSourceView.
- Drag-to-reorder editor tabs (`AdwTabView` supports this natively) plus Move Left/Right menu items.
- Commit-time failure must report the commit, not the last successful statement.

**T2.3 — Sidebar, navigation and organisation** *(depends on T1.5 split)*
- Files: `app/src/ui/sidebar_row.rs`, extracted sidebar module, `app/src/ui/quick_switcher_dialog.rs`, `crates/storage`
- Tree folders grouping tables/views/materialized views/foreign tables/procedures/functions.
- Multi-select databases and schemas with bulk refresh/copy/export. **Bulk drop stays policy-gated and is not in this slice.**
- Type-to-jump in the tree.
- Browse Back/Forward history per tab.
- Extend the existing Quick Switcher to search across every open connection — this is now possible because `4c9a97fe7` made several connections concurrent.
- Preview-tab semantics: double-click/Return keeps the tab; Open in New Tab.
- Disambiguate same-named objects across databases in tab titles; return to a database's last-used tab.
- Connection groups, tags, favorites, search and URL import — this **is** the open Phase 10.2 item; do it here rather than tracking it twice.

**T2.4 — Query plan, insights and completion depth**
- Files: `app/src/ui/explain_dialog.rs` → new plan result-tab module, `app/src/ui/editor/{completion,schema}.rs`, `crates/storage` history
- Convert EXPLAIN from a modal dialog into a **pinnable result tab** beside the data. This is the structural change; do it first.
- Plan tree: sortable Operation/Cost/Rows/Actual-Time columns over `Gtk.ColumnView`, keyboard navigation, copy-step, resizable detail panel.
- Cost badge must escalate in **shape and colour together** — colour alone fails the accessibility checklist in `linux/docs/accessibility.md`.
- **Defer the plan *diagram*.** The tree carries most of the value; the zoomable diagram is disproportionate GTK effort.
- Query Insights: aggregate the existing local history — normalise queries differing only in literals (the fork's dialect-aware lexer can do this), then report most-run, slowest, regressed and failing. Nothing leaves the machine; it reads the audit/history store through existing storage APIs.
- PostgreSQL completion depth: operators (`::`, JSON, array/range containment, regex, FTS), builtin functions, multi-word syntax, and enum labels on enum comparison. Builds directly on the schema-aware completion shipped in `76e17bbee`.
- Structural diagnostics: underline unclosed brackets and comments; **never** flag a half-written statement.

### Track 3 — Standing guardrails

- Update `PLAN.md` and `linux/ROADMAP.md` in the same commit as any status change. Phase 5's "keep documentation current" is a standing rule.
- Every upstream port gets an `upstream-sync.md` entry.
- Re-run this divergence check monthly: `git fetch upstream && git rev-list --count origin/linux..upstream/main`.
- A checkbox is checked only when its criterion is *release-verified*, per the existing status vocabulary.

---

## Execution order

```
T1.1 soak gate ──────────────► T1.8 installed-package verification ──► RC
   │                                    ▲
   ├─ T1.2 clean tip                    │
   ├─ T1.3 release profile ─────────────┤
   ├─ T1.4 panic triage ────────────────┤
   ├─ T1.6 docs reconcile ──────────────┤
   └─ T1.7 MCP local-only ──────────────┘

T1.5 file splits ──┬──► T2.1 grid & pagination      (start here in Track 2)
                   ├──► T2.2 editor & statements
                   ├──► T2.3 sidebar & organisation
                   └──► T2.4 plan, insights, completion
```

`T1.1` and `T1.5` are the two true prerequisites. `T1.1` unblocks all release evidence; `T1.5` unblocks all UI work. Everything else in Track 1 is independent and can be parallelised. In Track 2, start with **T2.1** — it fixes real correctness bugs rather than adding surface.

---

## Verification

**Per change (local, before push):**
```bash
cd linux
./scripts/preflight.sh                  # fmt, clippy -D warnings, file-size, bounded-operations
./scripts/ci-local.sh
cargo deny check                        # must run from linux/, does not accept --manifest-path
```

**Tiers:**
```bash
./scripts/test-sandbox.sh               # sandbox tier, ~431 tests
./scripts/test-gtk-safety.sh            # installed GTK scenarios — required for UI-visible ports
./scripts/test-postgres-release.sh      # PostgreSQL release fixture, 44 tests
./scripts/test-driver-tls.sh            # driver TLS fixture
./scripts/smoke-postgres.sh
```

**Gate-specific:**
- T1.1: dispatch `gtk-soak.yml` twice at a pinned SHA; both runs must report that same SHA.
- T1.3: `hyperfine` startup and `smem` RSS, before and after, recorded in `linux/docs/`.
- T1.8: `scripts/validate-arch-package.sh`, then a manual install → upgrade → rollback cycle on a Wayland session.

**Toolchain note:** the workspace pins Rust 1.93 (`rust-toolchain.toml`). A distro `/usr/bin/cargo` does not honour it without rustup. CI tests the 1.93 MSRV; a scheduled job tests current stable via an explicit `+stable` selector.

---

## Non-goals

- **No source merge from upstream.** Swift never enters `linux`. `git merge upstream/main` on this branch is always wrong.
- **No macOS UI mimicry.** GNOME idiom, macOS semantics.
- **No account, licence key, subscription, paid tier or entitlement gate.** This is a hard product invariant in `PLAN.md` and applies to every ported feature — including Query Insights and charts, which are Starter features upstream and must be unconditionally free here.
- **No expansion of the governance layer** in wave 1. Policy, audit and MCP stay as they are; ported features route through them but do not extend them.
- **No new drivers** until Track 1 closes and wave 1 lands (existing Phase 7 sequencing: Redshift/CockroachDB profiles, then Trino, Snowflake, BigQuery).
- **No plan diagram, no charts, no MCP protocol bump** in wave 1.
