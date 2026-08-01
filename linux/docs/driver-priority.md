# Adding drivers (order and maturity)

Drivers stay statically linked ([ADR 0001](decisions/0001-no-plugin-system.md)).
Capability status for each engine lives in [driver-maturity.md](driver-maturity.md).

Persona order used for the Stage 5 engines:

1. ClickHouse (Stable)
2. Redis (Experimental)
3. DuckDB (Experimental, `--features duckdb`)
4. MongoDB (Experimental)
5. Oracle (Experimental, `--features odpi`)

For each engine:

1. Create `crates/drivers/<engine>/` implementing `DatabaseDriver` + `Connection`
2. Set `maturity()` when the driver is not yet day-to-day complete
3. Add to workspace members and `build_registry()` in both `tablepro-app` and `tablepro-agentd` (gate with Cargo features when connect needs native libs or a huge build)
4. Map types into `core::Value`
5. Wire `activity_sql` variants when the engine has session introspection
6. Add ignored testcontainers integration tests and CI job

SSH jump-host chains belong in `crates/ssh` (multi-hop via sequential tunnels), not in drivers.
