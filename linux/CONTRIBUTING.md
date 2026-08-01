# Contributing to TablePro Linux

This file governs the Linux subproject only. The repository-level [CLAUDE.md](../CLAUDE.md) covers cross-cutting rules (no comments in source, security first, root-cause fixes, etc.) — those apply here too.

## Branch model (fork)

On community forks that track Linux work, keep only two long-lived branches:

| Branch | Role |
|---|---|
| `main` | Mirror of `TableProApp/TablePro` `main` (macOS + shared docs). Rarely edited here. |
| `linux` | Day-to-day Linux development. Occasional PRs go upstream from this branch. |

Do not leave `feat/*` branches on the fork remote after their work lands on `linux`. Sync upstream often:

```bash
git fetch origin
git checkout main && git reset --hard origin/main && git push fork main
git checkout linux && git merge origin/main
```

## Dev environment

System packages — see [README.md](README.md) for distro-specific commands. After they are installed, work happens entirely from the `linux/` directory.

Prefer the cheap gates before a full GTK link or `.deb` package:

```bash
cd linux
./scripts/preflight.sh         # fmt + clippy + lib tests, no GTK app
./scripts/ci-local.sh          # full workspace unit tests (includes GTK)
./scripts/ci-local.sh integration  # docker driver suites
cargo run -p tablepro-app      # run from the tree while iterating
./scripts/build-deb.sh         # only when you need an installable package
```

## Code style

| Tool | Config | Notes |
|---|---|---|
| `rustfmt` | `rustfmt.toml` at workspace root | Run before commit. Pre-commit hook enforces it. |
| `clippy` | `clippy.toml` at workspace root | All workspace crates pass with `-D warnings`. New lints are negotiated per PR. |
| Edition | 2024 | Set per workspace. Do not override per crate. |
| MSRV | 1.93 | Pinned in `rust-toolchain.toml`. Bumped only with discussion. |

Conventions, beyond what `rustfmt` decides:

- **No comments unless they explain a hidden constraint or invariant.** Code must be self-documenting through naming. Inherited from CLAUDE.md.
- **No `unwrap()` or `expect()` in production paths.** Tests and `OnceLock::get_or_init` initialisers are the only acceptable callers.
- **No `panic!`, `todo!`, `unimplemented!` in merged code.** Stub a real `Err` variant instead.
- **One public type per module file** when the type's surface is non-trivial. Internal helpers stay private.
- **Errors cross crate boundaries as `thiserror` enums.** Inside a crate, `anyhow::Result` is fine. See [docs/error-handling.md](docs/error-handling.md).

## Adding a database driver

This is the most common substantive change. Follow [docs/adding-drivers.md](docs/adding-drivers.md) end to end. It is short and the steps are mechanical. Skipping a step (most often the registry registration) breaks the app silently.

## Commits

Conventional Commits, single line, no body. Same rule as the macOS app:

```
feat(drivers): add ClickHouse driver via clickhouse-arrow
fix(app): debounce sidebar selection to avoid duplicate fetches
refactor(core): split DatabaseDriver into Driver + Connection traits
docs(adding-drivers): clarify TLS configuration step
```

## Pull requests

1. For Linux work on a fork: commit on `linux` (or a short-lived local branch that you merge into `linux` before opening the upstream PR). Upstream PRs target `TableProApp/TablePro`'s `linux` branch when that is the integration branch.
2. PR title is the conventional commit message you intend to land.
3. PR description has two sections: **Summary** (what and why, 2–4 bullets) and **Test plan** (checkbox list).
4. Run `./scripts/preflight.sh` before every push. Run `./scripts/ci-local.sh` before packaging or opening an upstream PR. CI runs preflight, then GTK checks, then driver integration.
5. UI changes must include before / after screenshots in the PR description, taken at HiDPI on both light and dark themes.

## What does not belong here

- Documentation for end users (installation, FAQ, screenshots for the marketing site) lives in the repository-level `docs/` Mintlify project.
- Cross-platform decisions (release cadence, branding, pricing) are not made in this subproject.
- macOS plugin work — that lives in `apps/macos/Plugins/` (post Phase B restructure) or the current `Plugins/` directory.

## Where to start as a contributor

In rough order of impact:

1. Read [ARCHITECTURE.md](ARCHITECTURE.md) and [docs/decisions/](docs/decisions/). 20 minutes, fixes most "why is it shaped like this" questions.
2. Pick an issue tagged `good-first-issue` or `driver:<engine>`.
3. If adding a driver, copy the most recently merged driver crate as a template. Do not copy the spike code.
4. Open the PR small. We prefer five small PRs over one big one.
