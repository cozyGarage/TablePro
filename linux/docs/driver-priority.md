# Adding drivers (Stage 5 order)

Drivers stay statically linked ([ADR 0001](decisions/0001-no-plugin-system.md)).
Persona order for the next engines:

1. ClickHouse
2. Redis
3. DuckDB
4. MongoDB
5. Oracle

For each engine:

1. Create `crates/drivers/<engine>/` implementing `DatabaseDriver` + `Connection`
2. Add to workspace members and `build_registry()` in both `tablepro-app` and `tablepro-agentd`
3. Map types into `core::Value`
4. Wire `activity_sql` variants when the engine has session introspection
5. Add ignored testcontainers integration tests and CI job

SSH jump-host chains belong in `crates/ssh` (multi-hop via sequential tunnels), not in drivers.
