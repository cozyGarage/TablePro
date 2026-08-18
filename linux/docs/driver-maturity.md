# Driver maturity

Every registered driver reports a `DriverMaturity` (`Stable` or
`Experimental`). The Connect dialog shows Experimental as a subtitle so
users are not surprised by missing write or transaction paths.

| Driver | Maturity | Connect | Browse | Query | Writes | Params | `begin` / MCP preview | Notes |
|---|---|---|---|---|---|---|---|---|
| PostgreSQL | Stable | yes | yes | SQL | yes | yes | yes | all five TLS modes, release-verified |
| MySQL | Stable | yes | yes | SQL | yes | yes | yes | DDL not transactional; TLS modes mapped but untested |
| SQLite | Stable | yes | yes | SQL | yes | yes | yes | File-based |
| SQL Server | Stable | yes | yes | SQL | yes | yes | no interactive begin | Tiberius; Verify Ca and Verify Full are identical |
| ClickHouse | Stable | yes | yes | SQL | yes | yes | no | Async mutations; no row counts; TLS is on/off only |
| Redis | Experimental | plaintext only | DBs / SCAN | Redis CLI | via query | no | no | built without a TLS feature, so the default Verify Full fails |
| MongoDB | Experimental | plaintext only | collections | find / aggregate | insertOne / deleteMany | no | no | the TLS setting is ignored; see docs/connections.md |
| DuckDB | Experimental | yes | yes | SQL | yes | yes | no | Cargo feature `duckdb` (large build) |
| Oracle | Experimental | with Instant Client | yes | SQL | yes | no binds | no | Cargo feature `odpi`; not registered without it |

## Rules

1. Default maturity is `Stable`. Override only when a path users expect is missing or dialect-limited.
2. Do not register a driver that always fails at connect. Oracle stays behind `--features odpi`.
3. DuckDB stays behind `--features duckdb` for compile size, not maturity.
4. Raising a driver to Stable means browse, the engine's query dialect, common writes, and CI integration coverage are in place.

Transport and TLS behaviour per driver is tracked in [connections.md](connections.md).

See also [adding-drivers.md](adding-drivers.md) and [driver-priority.md](driver-priority.md).
