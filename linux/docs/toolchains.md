# Rust toolchains on Arch and Omarchy

TablePro Linux declares Rust 1.93 as its minimum supported Rust version (MSRV) in `rust-toolchain.toml`, `Cargo.toml`, and `clippy.toml`. CI compiles and lints with 1.93. A separate scheduled job runs Clippy with the current stable release so new compiler lints are found before an Arch or Omarchy upgrade reaches contributors. Omarchy uses Arch packages, so the same toolchain behavior applies.

## Why Arch's Cargo may ignore `rust-toolchain.toml`

The `rust-toolchain.toml` override is interpreted by the Cargo and Rust compiler proxies installed by rustup. Arch's `rust` package installs `/usr/bin/cargo` and `/usr/bin/rustc` directly. Those binaries use the distro version and do not switch to 1.93 when you enter this directory.

Check which setup is active:

```bash
type -a cargo rustc
cargo --version
rustc --version
rustup show active-toolchain  # available only with rustup
cargo +1.93.0 --version
cargo +stable --version
```

Using a newer Arch compiler is useful for the current-stable check, but it does not prove MSRV compatibility.

## Recommended contributor setup: rustup

Install Arch's `rustup` package instead of the direct `rust` package, then install both test toolchains. The Arch `rust` and `rustup` packages conflict, so pacman replaces the direct compiler package with rustup-managed proxies.

```bash
sudo pacman -S rustup
rustup toolchain install 1.93.0 --profile minimal --component rustfmt,clippy
rustup toolchain install stable --profile minimal --component clippy
rustup default stable
```

Inside `linux/`, the repository override selects 1.93 automatically. Use an explicit `+stable` selector for the forward-compatibility check:

```bash
cd linux
./scripts/preflight.sh
cargo +stable clippy --workspace --exclude tablepro-driver-duckdb --all-targets -- -D warnings
```

Run `cargo +1.93.0 ...` when you want to make the MSRV choice explicit. Do not change the repository override merely because Arch has published a newer stable compiler.

## Keeping Arch's distro Rust

If you intentionally keep the `rust` package, `./scripts/preflight.sh` uses the compiler found first on `PATH`; it therefore checks current Arch stable, not the MSRV. Rely on the required GitHub Actions preflight for the 1.93 result, or use a rustup/container environment before declaring an MSRV-sensitive change complete.

The weekly `Current stable Clippy (scheduled)` job is a required signal: fix a new lint in code where practical, and add a narrowly documented lint allowance only when supporting both 1.93 and current stable genuinely requires it.
