# TablePro Linux

TablePro is a Linux-only database client built with Rust, GTK4, libadwaita, and Relm4. The Rust workspace is rooted in this `linux/` directory.

## Status

The GTK application supports PostgreSQL, MySQL, SQLite, SQL Server, and ClickHouse. Redis and MongoDB are experimental. DuckDB and Oracle require the `duckdb` and `odpi` Cargo features.

Current workflows include saved connections, SSH tunnels, browse and SQL tabs, structure editing, inline row changes, query history, policy checks, MCP access, and the headless `tablepro-agentd` process. See [ROADMAP.md](ROADMAP.md), [docs/driver-maturity.md](docs/driver-maturity.md), and [docs/production-audit.md](docs/production-audit.md) for current limits.

The application is suitable for development and personal testing. The PostgreSQL release fixture and the installed GTK safety flows pass locally: certificate hostname and authority checks, verified TLS through an SSH tunnel, read-only denial, rollback, blocking locks, reconnect, approval dismissal, approve-once scope, and unavailable audit storage. Trusted production writes and unattended agents still wait on hosted confirmation of those suites and on packaging verification.

## Named query parameters

Write `:name` anywhere a value belongs:

```sql
SELECT * FROM orders WHERE customer = :customer AND total > :minimum;
```

Running the statement asks for one value per name and sends them as driver-bound parameters. Each value has a type choice: `Auto` (whole numbers and decimals are detected, everything else is text), `Text`, `Integer`, `Decimal`, `Boolean`, or `Null`. Placeholders inside string literals, quoted identifiers, comments, dollar-quoted bodies, PostgreSQL `::` casts, and existing `$1` or `?` placeholders are left alone.

## Editor productivity

- **Completion**: typing after `FROM` or `JOIN` offers tables; elsewhere it offers the columns of the tables named in the statement. `alias.` and `table.` narrow to that table's columns, and columns are fetched on demand for tables you reference.
- **Favorites**: Ctrl+D saves the current editor query under a name in `$XDG_CONFIG_HOME/tablepro/favorites.json`. Saving an existing name replaces its statement.
- **Open Quickly**: Ctrl+P searches favorites, open tabs, and saved connections. Type to filter (name, statement text, or initials), arrow keys to move, Enter to open, Escape to dismiss.

## Stack

| Layer | Technology |
|---|---|
| Language | Rust 1.93+ |
| GUI | GTK4 4.14+, libadwaita 1.6+, GtkSourceView 5.12+ |
| Components | Relm4 |
| Async work | Tokio for database and service work, GLib main context for GTK |
| Drivers | sqlx, tiberius, clickhouse, and engine-specific Rust crates |
| Storage | XDG JSON files, SQLite FTS5, JSONL audit journal, Secret Service through `oo7` |
| Packaging | AUR and Omarchy first, Flatpak later |

Drivers are linked at build time. TablePro does not load database drivers as runtime plugins. The UI uses native GTK widgets and does not embed a browser view.

## Build requirements

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

Check the native libraries and Rust toolchain:

```bash
pkg-config --modversion gtk4 libadwaita-1 gtksourceview-5
rustc --version
```

## Build and run

Run Cargo commands from the workspace root:

```bash
cd linux
cargo run -p tablepro-app
```

### SQL Server Kerberos

Run `kinit` before connecting and confirm the ticket with `klist`. Select **Windows (Kerberos)** in the SQL Server connection form and enter the server's real DNS hostname. SQL Server requests `MSSQLSvc/<host>:<port>`, including when SSH forwards the socket through localhost.

A FILE credential cache works when `KRB5CCNAME` points to a readable location. Custom SPN overrides and cross-realm setup are not exposed in the connection form.

## Checks

```bash
./scripts/preflight.sh
./scripts/ci-local.sh
./scripts/ci-local.sh integration
```

`preflight.sh` skips the GTK application. `ci-local.sh` runs the current workspace library and binary test targets, including the GTK binary tests. Integration mode runs the real-driver Docker suites.

To smoke-test a PostgreSQL server you already run:

```bash
./scripts/smoke-postgres.sh
```

If native development packages are unavailable, `scripts/dev-env.sh` can use Debian-family package payloads extracted under `../.local-deps/root/`.

## Packaging

AUR packaging is the first release target, with Omarchy as the first supported desktop distribution. The current package recipe is in [packaging/aur/PKGBUILD](packaging/aur/PKGBUILD).

```bash
cd packaging/aur
makepkg -si
```

Debian package and Flatpak files exist for development. Flatpak publication is planned after the native Arch and Omarchy path is release-verified. See [packaging/README.md](packaging/README.md).

## Documentation

| Topic | File |
|---|---|
| Architecture and crate boundaries | [ARCHITECTURE.md](ARCHITECTURE.md) |
| Roadmap | [ROADMAP.md](ROADMAP.md) |
| Production audit | [docs/production-audit.md](docs/production-audit.md) |
| Contributing | [CONTRIBUTING.md](CONTRIBUTING.md) |
| Rust toolchains on Arch and Omarchy | [docs/toolchains.md](docs/toolchains.md) |
| Optional upstream reference review | [docs/upstream-sync.md](docs/upstream-sync.md) |
| Adding a database driver | [docs/adding-drivers.md](docs/adding-drivers.md) |
| Driver maturity | [docs/driver-maturity.md](docs/driver-maturity.md) |
| State management | [docs/state-management.md](docs/state-management.md) |
| Storage | [docs/storage.md](docs/storage.md) |
| Error handling | [docs/error-handling.md](docs/error-handling.md) |
| Testing | [docs/testing.md](docs/testing.md) |
| Architecture decisions | [docs/decisions/](docs/decisions/) |

## License

TablePro Linux is licensed under AGPL-3.0-or-later. See [LICENSE.md](LICENSE.md).
