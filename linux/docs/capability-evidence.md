# Capability evidence

Last audited: 2026-08-19

`PLAN.md` lists ten capabilities as "Already useful on Linux". This file records
what actually proves each one, so the list is a claim backed by evidence rather
than an assertion. Status terms match [ROADMAP.md](../ROADMAP.md).

Evidence tiers are the regression tiers in [CLAUDE.md](../../CLAUDE.md):
`unit`, `sandbox`, `driver`, `release`, `gtk`.

| Capability | Strongest evidence | Verdict |
|---|---|---|
| Saved connections and secret storage | 15 unit tests on the connections file, 8 on secret mapping | **Partial** — the Secret Service round-trip test is `#[ignore]`, so no test proves a password survives a real keyring |
| Multi-tab workspace and restoration | 6 unit tests on workspace state | **Weak** — serialization is tested, restoration is not. No test starts the app, opens tabs, restarts, and checks they return |
| SQL editor and multi-result execution | 19 unit tests on the SQL text scanner, 15 on completion, 1 gtk scenario for parameters | **Partial** — editing and completion are well covered; nothing proves one batch produces several result tabs |
| Browse/edit/filter/sort/pagination | 34 unit tests on filters, 6 release tests covering all 17 operators, keyset vs offset paging, and an injection payload | **Release-verified** for filter, sort, and pagination. The inline-edit write path has unit tests only |
| Table/column/index/foreign-key editing | 76 unit tests on DDL generation, 5 release tests against PostgreSQL | **Release-verified**. This suite found two real bugs when it was written |
| Query history | 2 tests against ~600 lines of code | **Weak** — the least-tested subsystem in the workspace relative to its size |
| CSV and JSON export | 2 unit tests in the core exporter, 1 release test for agent CSV escaping | **Weak** — streaming paging is untested, and JSON export lives in the UI with no test at all |
| Activity and EXPLAIN | 2 unit tests plus 2 release tests for activity, 7 unit tests plus release coverage for EXPLAIN classification | **Integrated** |
| SSH, TLS, reconnect and Kerberos foundations | 8 release tests for TLS identity, 2 for reconnect, 3 for the shared transport | **Partial** — proven on PostgreSQL only. Kerberos has no test of any kind; there is no KDC in any fixture |
| MCP and headless agent foundations | 9 + 2 sandbox tests, 6 release tests, 1 transport regression test | **Integrated** |

## What this changes

Three claims need work before the list is accurate:

1. **Query history** carries the most code per test of anything in the
   workspace. It stores user SQL, so it is also privacy-relevant.
2. **Workspace restoration** is the half of the tab feature users notice when it
   fails, and it is the half with no test.
3. **JSON export and streaming CSV** are user-visible data paths with no
   coverage of the streaming or paging behaviour.

Two claims are overstated rather than untested:

- **Kerberos** appears in a list of "foundations" with zero tests. Either build a
  KDC fixture or say plainly that Kerberos is configuration-only.
- **Secret storage** is unproven against a real Secret Service, so the claim
  rests on an ignored test.

Connection-layer evidence is tracked separately and in more detail in
[connections.md](connections.md).
