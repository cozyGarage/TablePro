# TablePro Linux

Native Linux database client. Sister product to the macOS TablePro app,
sharing no code but matching the feature set over time.

## Status

Working GTK4 / libadwaita client: PostgreSQL, MySQL, SQLite, and MSSQL;
SSH tunnels; multi-tab browse / SQL editor / structure editor; inline
cell edit; query history; Flatpak scaffolding. See
[ROADMAP.md](ROADMAP.md) for the governed-data-plane stages (policy,
MCP, headless agentd).

## Stack

| Layer | Pick |
|---|---|
| Language | Rust 1.93+ |
| GUI toolkit | GTK4 4.14+ + libadwaita 1.5+ |
| App architecture | [Relm4](https://relm4.org) — Elm-style components on gtk4-rs |
| Async | tokio (DB drivers) bridged to glib main loop (UI) |
| DB drivers | sqlx (PG / MySQL / SQLite), tiberius (MSSQL); more via static crates |
| Persistence | libsecret (passwords), JSON files (connections / prefs / workspace) |
| Distribution | Flathub primary |

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
  libgtksourceview-5-dev libssl-dev libsecret-1-dev

# Fedora
sudo dnf install -y gcc pkg-config gtk4-devel libadwaita-devel \
  gtksourceview5-devel openssl-devel libsecret-devel

# Arch
sudo pacman -S --needed base-devel pkg-config gtk4 libadwaita \
  gtksourceview5 openssl libsecret
```

```bash
pkg-config --modversion gtk4 libadwaita-1   # need 4.14+ / 1.5+
rustc --version                              # need 1.93+

cd linux
cargo run -p tablepro-app
```

## Documentation index

| Topic | File |
|---|---|
| Layered architecture, crate boundaries | [ARCHITECTURE.md](ARCHITECTURE.md) |
| Roadmap and current stage | [ROADMAP.md](ROADMAP.md) |
| Production gap analysis | [docs/production-audit.md](docs/production-audit.md) |
| Contributing | [CONTRIBUTING.md](CONTRIBUTING.md) |
| Adding a database driver | [docs/adding-drivers.md](docs/adding-drivers.md) |
| State management with Relm4 | [docs/state-management.md](docs/state-management.md) |
| Persistence | [docs/storage.md](docs/storage.md) |
| Error handling | [docs/error-handling.md](docs/error-handling.md) |
| Testing | [docs/testing.md](docs/testing.md) |
| Architecture decision records | [docs/decisions/](docs/decisions/) |

## License

Same as the parent TablePro project.
