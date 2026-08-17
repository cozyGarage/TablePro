## Executive Summary

The goal is to build a lightweight, native Linux database client matching the visual polish of **TablePro** and the backend stability of **Tablio/Beekeeper Studio**, using **Rust** for the engine and **GTK4/Libadwaita** for a 100% native Linux GUI. Instead of writing database drivers from scratch, the strategy relies on extracting open-source connection logic from **Tablio (MIT)** to accelerate development.

---

## The Big Plan: 5-Phase Roadmap

[Phase 1: Architecture] ➔ [Phase 2: Backend Extraction] ➔ [Phase 3: Core Glue] ➔ [Phase 4: GTK4 UI Integration] ➔ [Phase 5: Optimization]

## **Phase 1: Architectural Blueprint & Code Audit**

Establish how the asynchronous Rust backend will safely talk to the synchronous GTK4 main UI loop.

* **Action:** Audit your existing GTK4 repo alongside Tablio’s source code.  
* **Technical Choice:** Enforce the use of **`tokio`** for background database polling/queries and **`glib::MainContext::channel`** to pass data frames safely back to the UI thread without freezing the interface.

## **Phase 2: Backend Extraction (The "Tablio Scavenge")**

Isolate and lift mature database connectivity code so you do not reinvent the wheel for handling PostgreSQL or MySQL connections.

* **Action:** Extract the `src-tauri` database pool handlers from Tablio.  
* **Technical Choice:** Repackage Tablio's query executors, connection management, and connection string parsers into an independent, purely headless Rust library crate (e.g., `libdb_core`).

## **Phase 3: The Rust-to-GTK4 "Glue" Layer**

Bridge raw database results into types that GTK4's data-binding models natively understand.

* **Action:** Build a translation layer that maps database rows into specialized GObjects.  
* **Technical Choice:** Implement custom GObjects or utilize `gio::ListStore` to hold tabular query data, ensuring smooth rendering for large data grids.

## **Phase 4: Native UI Feature Implementation**

Construct a sleek, responsive, TablePro-inspired front-end using the GNOME ecosystem.

* **Action:** Build the three core views of a modern database client using native widgets.  
* **Technical Choice:**  
  * *Sidebar:* Use `Adw.NavigationSplitView` for connection switching.  
  * *SQL Editor:* Use **`GtkSourceView 5`** to handle SQL syntax highlighting and auto-completion natively.  
  * *Data Grid:* Use **`Gtk.ColumnView`** paired with expression bindings for lightning-fast, lazy-loaded data scrolling.

## **Phase 5: Optimization & Compilation**

Maximize performance and prepare the software for easy Linux distribution.

* **Action:** Optimize compilation flags and bundle resources.  
* **Technical Choice:** Implement binary compilation optimizations (`panic = "abort"`, `lto = true`) to achieve a sub-80MB RAM footprint and sub-1-second boot times. Package the final product via **Flatpak** for universal Linux distribution.

---

## For you my AI Research Agent

\#\#\# AGENT ASSIGNMENT: Technical Specification for a Rust \+ GTK4 Database Client

\*\*Context:\*\*   
We are building a highly lightweight, native Linux SQL client using a Rust backend and a GTK4 \+ Libadwaita front-end (inspired by TablePro's UI and Tablio's backend logic).

\*\*Your Tasks:\*\*  
1\. \*\*Analyze Tablio:\*\* Review the open-source repository \`dasunNimantha/tablio\` (specifically its \`src-tauri\` directory). Map out how it structures database connections, authentication, connection pools, and query execution for PostgreSQL and MySQL.  
2\. \*\*Draft the Architecture:\*\* Design a multi-threaded architecture showing how a Tokio async runtime (handling the database connections) will safely communicate with the single-threaded GTK4 main loop using glib channels (\`glib::MainContext::channel\`).  
3\. \*\*Specify UI Components:\*\* Document how to implement a high-performance data grid in GTK4 using \`Gtk.ColumnView\` and \`gio::ListStore\` to handle large query result sets efficiently.  
4\. \*\*Draft a Project Structure:\*\* Provide a recommended Cargo workspace structure that separates the headless database logic (\`core\`) from the GTK4 application logic (\`ui\`).

Provide a highly technical, step-by-step implementation guide addressing these 4 points.

Answer:

I read the whole tree and your wish document side by side. The most important finding first, because it changes the plan: **the wish document describes a project that the pasted repo has already largely built.** Your five phases map onto existing code almost one-to-one, so the job is not "start a Rust \+ GTK4 client and scavenge Tablio", it's "finish and polish an existing 37k-line Rust/GTK4 client that is further along than the plan assumes."

### **Wish plan vs. what the repo already has**

| Wish phase | Status in the repo |
| ----- | ----- |
| 1\. tokio ↔ GTK bridge via `glib::MainContext::channel` | Done, and done better. Relm4 `sender.command(...)` / `relm4::spawn` \+ `async-channel` (glib 0.22, where `MainContext::channel` is gone anyway). See `ARCHITECTURE.md` "Async architecture". |
| 2\. Extract Tablio's `src-tauri` DB pools into a headless crate | Already exists as `crates/core` \+ `crates/drivers/*` on sqlx (PG/MySQL/SQLite), tiberius (MSSQL), official clickhouse/redis/mongodb/duckdb crates, with testcontainers integration suites. |
| 3\. Rows → GObjects / `gio::ListStore` | Done: `ui/row_object.rs` (GObject subclass), `ui/grid/` builds `ColumnView` \+ `MultiSelection` over a `ListStore`, virtualized, keyset pagination helper in `core/pagination.rs`. |
| 4\. `AdwNavigationSplitView`, GtkSourceView 5, `ColumnView` | All present (`ui/app/mod.rs`, `ui/editor/`, `ui/grid/`), plus `AdwTabView` workspace, structure editor, history dialog. |
| 5\. LTO / `panic = "abort"` / Flatpak | **Partially missing.** Flatpak/deb/AUR scaffolding exists, but `linux/Cargo.toml` has no `[profile.release]` at all. This is a real, cheap gap. |

On Tablio specifically: I checked the repo. Tablio is an open-source, cross-platform desktop database client built with Rust \+ Tauri first, with a React and TypeScript frontend on top. Its backend idea of note is that Tablio creates a separate connection pool per database, stored in a RwLock\<HashMap\<String, Pool\>\>. When the user switches to a different database in the sidebar, a new pool is created on demand and cached for reuse. That's a pattern worth borrowing (your `DatabaseService` holds one connection per saved connection, no per-database pool switching yet), but there is nothing in Tablio's driver layer that beats what you already have. Lifting its Tauri-shaped code into a Relm4/policy-guarded architecture would cost more than it saves. Treat Tablio and Tabularis as *feature/UX references*, not as a source of code. [GitHub](https://github.com/dasunNimantha)[Arch Linux](https://aur.archlinux.org/packages/tablio-bin)

### **Where the repo actually is**

The project's own `PLAN.md`, `ROADMAP.md`, and `docs/production-audit.md` are honest and current (audited 2026-08-13). Summary: connect/browse/edit/SQL/structure/history/SSH/TLS all work; five drivers are labelled Stable; policy \+ MCP \+ audit journal are integrated. What blocks a real release: PostgreSQL `VerifyFull` through SSH (sqlx can't separate dial address from TLS server name), no GTK end-to-end safety tests, no release fixture, packaging never verified end-to-end. What blocks *daily-driver polish* (the "TablePro feel" you want): keyword-only completion (`CompletionWords`, not schema-aware), no favorites/quick switcher, no SQL file open/save, no import, no inspector/JSON viewer, no connection groups/tags/colors, no views/routines/users browsing.

There is also a strategic tension you should decide on consciously: the repo carries a heavy governance layer (policy chokepoint, hash-chained audit journal, MCP bridge, agentd). That's excellent engineering, but it's the opposite of "lightweight personal client". I'd keep it, since it's already integrated and tested, but not let its remaining phases gate the UI work you care about.

### **Concrete plan**

**Phase 0 — Own the codebase (week 1).** Get it building on your machine (Ubuntu 25.10 or Arch, per `README`/`docs/toolchains.md`), run `./scripts/preflight.sh` and `./scripts/ci-local.sh`, run the app against the Postgres smoke script. Add `[profile.release]` with `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`, `strip = true`, `opt-level = 3`, and measure with `hyperfine` (startup) and `heaptrack`/`smem` (RSS). That alone gets you most of the wish's Phase 5\. Read `ARCHITECTURE.md`, the four ADRs, and `docs/state-management.md`; they will save you weeks. Decide scope: driver set stays static (ADR 0001), no Tablio extraction, governance kept but not expanded.

**Phase 1 — Editor productivity (weeks 2–5).** This is the highest daily-value gap and matches Priority A in `PLAN.md`: schema-aware autocomplete (tables/columns/aliases via a new provider feeding sourceview's completion, replacing the keyword `CompletionWords`), saved queries/favorites, a quick switcher (`Cmd+Shift+O` equivalent), SQL file open/save with external-change detection, named `:param` support. All of these are UI \+ `storage` work with no driver changes.

**Phase 2 — TablePro-grade UI polish (weeks 5–9).** Row inspector panel, JSON/blob cell viewers, connection groups/tags/colors on the welcome view, keyboard-shortcut coverage, column value filter, copy-as (CSV/JSON/INSERT already partially there). Borrow the per-database pool idea from Tablio for MySQL/PG database switching without reconnect. Use the macOS `docs/features/*.mdx` in the tree as your parity spec; it's a complete feature list you can tick through.

**Phase 3 — Data workflows (weeks 9–13).** CSV/JSON import, SQL dump export, true streaming for large results (drop the 1M-row materialization), views/materialized views/routines in the sidebar, PG/MySQL users & roles, `pg_dump`/`pg_restore`.

**Phase 4 — Release gate (weeks 13–16).** Resolve PG `VerifyFull` over SSH (the plan lists the options: sqlx API, small patch, or supplying the stream), write the three GTK safety tests, run the Docker release fixture, ship AUR first, then Flatpak with offline Cargo sources. Only now does the app become something you'd hand to another person.

### **Corrected brief for your research agent**

Replace the four tasks with: (1) audit `crates/core`, `crates/drivers/*`, and `crates/policy` and produce a capability matrix per driver against `docs/driver-maturity.md`, noting where sqlx/tiberius limit us; (2) skip designing the tokio↔GTK bridge, instead review `ARCHITECTURE.md` \+ `docs/state-management.md` and list any places raw `tokio::spawn` or `glib::clone!`\-captured `&mut` state violate the Relm4 conventions; (3) evaluate the existing `ui/grid/` for large-result behavior (row cap, keyset pagination, column width persistence) and propose true streaming; (4) map the macOS `docs/features/*.mdx` to Linux status (implemented / integrated / release-verified / not ported) to seed the parity matrix `PLAN.md` Phase 5 asks for.

If you tell me which of the four phases you want to start with, I can turn it into a file-by-file task list against this tree.

That is a good review. now can you create a zip file for me of improvement file by file for PLAN.md first. and then we will circle back and research on the concrete plan here. Or base on on the PLAN we have here and the work already done, let create a better PLAN and research and start writing code?

Weighed technical constraints against user options and sought clarification

A quick practical note before the questions: I can't produce a zip here since file/code execution is off in this conversation (enabling it in Settings gives me artifacts and downloadable files). I can still write the improved PLAN.md and any code as inline documents you paste into the tree, and for the "start writing code" part the right tool is Claude Code pointed at your fork, since I can't touch the repo from here.

On direction: I'd lean toward your second option. The existing PLAN.md is honest but it's a *safety-first* plan written for a governed production tool; your stated goal is a polished, lightweight daily driver. A rewritten PLAN that reorders around that goal, backed by research on the specific technical blockers (schema-aware completion in GtkSourceView 5, sqlx TLS server name over SSH, true result streaming in `ColumnView`), is more useful than a diff of the current one. Three things I need pinned down before I launch the research:

