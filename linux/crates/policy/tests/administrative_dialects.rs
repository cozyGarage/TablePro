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
