# TablePro Linux

Native Linux database client. Sister product to the macOS TablePro app,
sharing no code but matching the feature set over time.

## Status

Working GTK4 / libadwaita client with PostgreSQL, MySQL, SQLite, MSSQL,
and ClickHouse stable, plus Experimental Redis / MongoDB (DuckDB and
Oracle behind `--features duckdb` / `--features odpi`). SSH tunnels,
multi-tab browse / SQL editor / structure editor, inline cell edit,
query history, Flatpak scaffolding, and a governed data plane (policy
chokepoint, MCP server, headless agentd). See [ROADMAP.md](ROADMAP.md)
and [docs/driver-maturity.md](docs/driver-maturity.md).

It is past demo-grade, but still not beta-shippable for Flathub.

## Stack

| Layer | Pick |
|---|---|
| Language | Rust 1.93+ |
| GUI toolkit | GTK4 4.14+ + libadwaita 1.6+ + GtkSourceView 5.12+ |
| App architecture | [Relm4](https://relm4.org) — Elm-style components on gtk4-rs |
| Async | tokio (DB drivers) bridged to glib main loop (UI) |
| DB drivers | sqlx (PG / MySQL / SQLite), tiberius (MSSQL), official `clickhouse` crate; Redis / DuckDB / MongoDB / Oracle crates |
| Persistence | libsecret (passwords), JSON files (connections / prefs / workspace) |
| Distribution | Flathub primary; `.deb` / AUR secondary |

## What this is not

| Not | Why |
|---|---|
| A port of the macOS app | Swift does not run on Linux. Zero shared source. |
| A plugin host | Drivers are statically linked. See [decisions/0001-no-plugin-system.md](docs/decisions/0001-no-plugin-system.md). |
| Cross-platform | Linux only. macOS and iOS have separate apps in this monorepo. |
| Electron / WebView | Native GTK4 widgets throughout. |

## Quickstart

System dependencies:

```bash
# Ubuntu / Debian
sudo apt install -y build-essential pkg-config libgtk-4-dev libadwaita-1-dev \
  libgtksourceview-5-dev libssl-dev libsecret-1-dev libkrb5-dev clang

# Fedora
sudo dnf install -y gcc pkg-config gtk4-devel libadwaita-devel \
  gtksourceview5-devel openssl-devel libsecret-devel krb5-devel clang

# Arch
sudo pacman -S --needed base-devel pkg-config gtk4 libadwaita \
  gtksourceview5 openssl libsecret krb5 clang
```

```bash
pkg-config --modversion gtk4 libadwaita-1 gtksourceview-5   # need 4.14+ / 1.6+ / 5.12+
rustc --version                                             # need 1.93+
```

Build and run:

```bash
cd linux
cargo run -p tablepro-app
```

### SQL Server Kerberos

Run `kinit` before connecting, then confirm the ticket with `klist`. Select **Windows (Kerberos)** in the SQL Server connection form and enter the server's real DNS hostname. SQL Server requests the service principal `MSSQLSvc/<host>:<port>`, including when SSH forwards the socket through localhost.

The Flatpak can read the host Kerberos configuration and KCM socket. A FILE credential cache also works when `KRB5CCNAME` points inside the shared home directory. Custom SPN overrides and cross-realm setup are not exposed in the connection form.

Local CI mirrors (prefer the cheap gate while iterating):

```bash
./scripts/preflight.sh              # no GTK app; policy/mcp/drivers/unit tests
./scripts/ci-local.sh               # full workspace unit tests (includes GTK)
./scripts/ci-local.sh integration   # docker driver suites
```

Driver smoke against a Postgres you already run, no Docker needed:

```bash
./scripts/smoke-postgres.sh
```

Optional: if the system `-dev` packages above are missing, extract the package payloads under `../.local-deps/root/` (so headers land in `../.local-deps/root/usr/include`) and `source scripts/dev-env.sh` before cargo. Debian-family layouts only.

## Packaging

See [packaging/README.md](packaging/README.md) once present on this branch.
Quick local `.deb`:

```bash
./scripts/build-deb.sh
# → packaging/out/tablepro_0.1.0-1_amd64.deb
```

## Documentation index

| Topic | File |
|---|---|
| Layered architecture, crate boundaries | [ARCHITECTURE.md](ARCHITECTURE.md) |
| Roadmap and current stage | [ROADMAP.md](ROADMAP.md) |
| Production gap analysis | [docs/production-audit.md](docs/production-audit.md) |
| Contributing | [CONTRIBUTING.md](CONTRIBUTING.md) |
| Rust toolchains on Arch/Omarchy | [docs/toolchains.md](docs/toolchains.md) |
| Upstream Linux sync history | [docs/upstream-sync.md](docs/upstream-sync.md) |
| Adding a database driver | [docs/adding-drivers.md](docs/adding-drivers.md) |
| Driver maturity matrix | [docs/driver-maturity.md](docs/driver-maturity.md) |
| State management with Relm4 | [docs/state-management.md](docs/state-management.md) |
| Persistence | [docs/storage.md](docs/storage.md) |
| Error handling | [docs/error-handling.md](docs/error-handling.md) |
| Testing | [docs/testing.md](docs/testing.md) |
| Architecture decision records | [docs/decisions/](docs/decisions/) |

## License

Same as the parent TablePro project.
