# Upstream Linux sync log

Record every reconciliation from `TableProApp/TablePro` into this Linux fork from 2026-08-10 onward. Earlier merges remain in Git history. An entry names the upstream ref reviewed, the local result, conflicts or deliberate deviations, and the verification performed. Normal local feature commits do not need entries.

## 2026-08-10 — SQL Server Kerberos reconciliation

- Upstream reviewed: `origin/linux` at `807d2809` (`feat(drivers): Windows integrated (Kerberos) auth for SQL Server`).
- Local result: `c0b368a2` (`feat(linux): reconcile Kerberos support and tunnel identity`).
- Method: manually reconciled instead of merging because the fork already had policy guards, TLS modes, SSH jump chains, expanded drivers, and refactored core/UI modules.
- Preserved fork behavior: all connections still enter through `DatabaseService` and `PolicyGuard`; saved password connections remain compatible; direct TLS behavior is unchanged.
- Intentional extension: `ConnectOptions` separates the physical dial endpoint from the database service endpoint so an SSH-forwarded SQL Server connection dials localhost while TLS and Kerberos use the original hostname and port.
- Verification: core service-address tests, saved-connection legacy/round-trip tests, SQL Server password/Kerberos target tests, strict Clippy, workspace unit tests, and local supply-chain checks.

## 2026-08-13 - Linux security catch-up

- Upstream reviewed: `origin/linux` at `807d2809`, `origin/main` at `c849d75f`, and security hardening commit `05389523`.
- Local result: the Phase 0 security catch-up commit containing this entry.
- Method: reviewed every new upstream commit touching Linux directly, then mapped shared security behavior onto the Rust implementation.
- Preserved fork behavior: MCP remains loopback-only and token-authenticated; SSH remains TOFU-based with jump chains; Linux CI keeps Rust 1.93 as the MSRV and current stable as a scheduled signal.
- Intentional deviations: SQL Server Kerberos remains the stronger manual port from `c0b368a2`; Swift plugin, AppKit, iOS, FreeTDS, libssh2, and Sparkle changes do not apply to the native Rust client.
- Security ports: reject untrusted browser origins before MCP JSON-RPC dispatch, treat SSH host-key algorithm changes as mismatches, and pin Linux GitHub Actions to immutable commits.
- Verification: focused MCP and SSH tests, full Rust formatting, workspace tests, Clippy, and supply-chain checks.

## Entry template

```text
## YYYY-MM-DD — short description

- Upstream reviewed: remote/ref and commit.
- Local result: commit or pull request.
- Method: merge, rebase, cherry-pick, or manual reconciliation.
- Preserved fork behavior: safety and product invariants retained.
- Intentional deviations: differences from upstream and why.
- Verification: commands and release fixtures run.
```
