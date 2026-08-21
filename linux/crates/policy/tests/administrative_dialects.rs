use tablepro_core::Environment;
use tablepro_policy::{Decision, PolicyConfig, Principal, StatementClass, classify, evaluate};

fn agent() -> Principal {
    Principal::Agent {
        token: "token".into(),
        client: None,
        model: None,
    }
}

fn agent_decision(sql: &str, driver_id: &str) -> Decision {
    let facts = classify(sql, driver_id);
    evaluate(
        &agent(),
        Environment::Local,
        &facts,
        false,
        &PolicyConfig::default().for_environment(Environment::Local),
        None,
    )
}

fn assert_admin_denied(sql: &str, driver_id: &str) {
    let facts = classify(sql, driver_id);
    assert_eq!(
        facts.class,
        StatementClass::Administrative,
        "{driver_id} must classify as administrative: {sql}"
    );
    assert!(
        facts.writes,
        "{driver_id} administrative calls must count as writes: {sql}"
    );
    let decision = agent_decision(sql, driver_id);
    assert!(
        matches!(decision, Decision::Deny { ref rule, .. } if rule == "agent_admin_denied"),
        "{driver_id} must deny an agent: {sql}, got {decision:?}"
    );
}

#[test]
fn mysql_server_control_functions_are_administrative() {
    for sql in [
        "SELECT sleep(30)",
        "SELECT benchmark(1000000, md5('x'))",
        "SELECT load_file('/etc/passwd')",
        "SELECT * FROM orders WHERE id = 1 AND sleep(5)",
    ] {
        assert_admin_denied(sql, "mysql");
    }
}

#[test]
fn sql_server_extended_and_system_procedures_are_administrative() {
    for sql in [
        "EXEC xp_cmdshell 'dir'",
        "EXECUTE xp_regread 'HKEY_LOCAL_MACHINE'",
        "EXEC sp_configure 'show advanced options', 1",
        "EXEC sp_who",
    ] {
        assert_admin_denied(sql, "mssql");
    }
}

#[test]
fn kill_is_administrative_on_every_engine_that_parses_it() {
    for driver_id in ["mysql", "mssql"] {
        assert_admin_denied("KILL 42", driver_id);
    }
}

#[test]
fn an_ordinary_read_stays_a_read_on_every_engine() {
    for driver_id in ["postgres", "mysql", "mssql", "sqlite", "clickhouse"] {
        let facts = classify("SELECT id, name FROM customers WHERE id = 1", driver_id);
        assert_eq!(facts.class, StatementClass::Select, "driver {driver_id}");
        assert!(!facts.writes, "driver {driver_id}");
    }
}

#[test]
fn an_engine_procedure_name_inside_a_literal_is_not_administrative() {
    let facts = classify("SELECT 'xp_cmdshell is not called here' AS note", "mssql");
    assert_eq!(facts.class, StatementClass::Select);
    assert!(!facts.writes);
}

#[test]
fn a_column_named_like_a_control_function_is_not_administrative() {
    let facts = classify("SELECT sleep FROM naps WHERE id = 1", "mysql");
    assert_eq!(facts.class, StatementClass::Select);
    assert!(!facts.writes);
}

#[test]
fn postgres_host_and_network_functions_are_administrative() {
    for sql in [
        "SELECT pg_read_file('/etc/passwd')",
        "SELECT pg_read_binary_file('/etc/shadow')",
        "SELECT pg_stat_file('/etc/passwd')",
        "SELECT pg_ls_dir('/')",
        "SELECT pg_ls_waldir()",
        "SELECT dblink('dbname=x', 'DELETE FROM t')",
        "SELECT query_to_xml('DELETE FROM t', true, true, '')",
        "SELECT id FROM t WHERE name = pg_read_file('/etc/passwd')",
    ] {
        assert_admin_denied(sql, "postgres");
    }
}

#[test]
fn a_read_only_connection_denies_a_postgres_host_read() {
    let facts = classify("SELECT pg_read_file('/etc/passwd')", "postgres");
    let decision = evaluate(
        &Principal::human_gui(),
        Environment::Local,
        &facts,
        true,
        &PolicyConfig::default().for_environment(Environment::Local),
        None,
    );
    assert!(
        matches!(decision, Decision::Deny { ref rule, .. } if rule == "connection_read_only"),
        "{decision:?}"
    );
}

#[test]
fn copy_that_reaches_the_host_is_administrative() {
    for sql in [
        "COPY t TO PROGRAM 'curl http://example.com'",
        "COPY t FROM PROGRAM 'sh -c whoami'",
        "COPY t TO '/tmp/exfiltrated.csv'",
        "COPY t FROM '/etc/passwd'",
    ] {
        assert_admin_denied(sql, "postgres");
    }
}

#[test]
fn copy_through_the_client_is_a_write_but_not_administrative() {
    for sql in ["COPY t TO STDOUT", "COPY t FROM STDIN"] {
        let facts = classify(sql, "postgres");
        assert_ne!(facts.class, StatementClass::Administrative, "{sql}");
        assert!(facts.writes, "{sql}");
    }
}

#[test]
fn pg_sleep_stays_an_ordinary_read_because_timeouts_already_bound_it() {
    let facts = classify("SELECT pg_sleep(30)", "postgres");
    assert_eq!(facts.class, StatementClass::Select);
    assert!(!facts.writes);
}

#[test]
fn a_column_named_like_a_host_function_is_not_administrative() {
    let facts = classify("SELECT pg_read_file FROM audit_log", "postgres");
    assert_ne!(facts.class, StatementClass::Administrative);
    assert!(!facts.writes);
}
