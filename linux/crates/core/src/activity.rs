//! Server activity / monitoring SQL templates per driver.
//! The UI fills these in and runs them through the policy-gated connection.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityQuery {
    Sessions,
    BlockingLocks,
    LongRunning,
    ReplicationLag,
    KillSession,
}

pub fn parse_session_id(input: &str) -> Option<u64> {
    input.trim().parse::<u64>().ok().filter(|id| *id > 0)
}

pub fn activity_sql(driver_id: &str, kind: ActivityQuery, session_id: Option<u64>) -> Option<String> {
    match (driver_id, kind) {
        ("postgres", ActivityQuery::Sessions) => Some(
            "SELECT pid, usename, datname, state, wait_event_type, wait_event, \
             now() - query_start AS duration, left(query, 200) AS query \
             FROM pg_stat_activity \
             WHERE backend_type = 'client backend' \
             ORDER BY query_start NULLS LAST"
                .into(),
        ),
        ("postgres", ActivityQuery::BlockingLocks) => Some(
            "SELECT blocked.pid AS blocked_pid, blocked.usename AS blocked_user, \
             blocking.pid AS blocking_pid, blocking.usename AS blocking_user, \
             left(blocked.query, 120) AS blocked_query, \
             left(blocking.query, 120) AS blocking_query \
             FROM pg_stat_activity blocked \
             JOIN pg_locks bl ON bl.pid = blocked.pid AND NOT bl.granted \
             JOIN pg_locks kl ON kl.locktype = bl.locktype \
               AND kl.database IS NOT DISTINCT FROM bl.database \
               AND kl.relation IS NOT DISTINCT FROM bl.relation \
               AND kl.page IS NOT DISTINCT FROM bl.page \
               AND kl.tuple IS NOT DISTINCT FROM bl.tuple \
               AND kl.virtualxid IS NOT DISTINCT FROM bl.virtualxid \
               AND kl.transactionid IS NOT DISTINCT FROM bl.transactionid \
               AND kl.classid IS NOT DISTINCT FROM bl.classid \
               AND kl.objid IS NOT DISTINCT FROM bl.objid \
               AND kl.objsubid IS NOT DISTINCT FROM bl.objsubid \
               AND kl.pid != bl.pid \
               AND kl.granted \
             JOIN pg_stat_activity blocking ON blocking.pid = kl.pid"
                .into(),
        ),
        ("postgres", ActivityQuery::LongRunning) => Some(
            "SELECT pid, usename, datname, now() - query_start AS duration, \
             left(query, 200) AS query \
             FROM pg_stat_activity \
             WHERE state = 'active' AND query_start < now() - interval '30 seconds' \
             ORDER BY query_start"
                .into(),
        ),
        ("postgres", ActivityQuery::ReplicationLag) => Some(
            "SELECT client_addr, state, sent_lsn, write_lsn, flush_lsn, replay_lsn, \
             write_lag, flush_lag, replay_lag \
             FROM pg_stat_replication"
                .into(),
        ),
        ("postgres", ActivityQuery::KillSession) => {
            let pid = session_id.filter(|id| *id > 0)?;
            Some(format!("SELECT pg_terminate_backend({pid})"))
        }
        ("mysql", ActivityQuery::Sessions) => Some("SHOW FULL PROCESSLIST".into()),
        ("mysql", ActivityQuery::LongRunning) => Some(
            "SELECT id, user, host, db, command, time, state, left(info, 200) AS query \
             FROM information_schema.processlist \
             WHERE command != 'Sleep' AND time > 30 \
             ORDER BY time DESC"
                .into(),
        ),
        ("mysql", ActivityQuery::KillSession) => {
            let id = session_id.filter(|id| *id > 0)?;
            Some(format!("KILL {id}"))
        }
        ("mssql", ActivityQuery::Sessions) => Some(
            "SELECT session_id, login_name, status, host_name, program_name, \
             cpu_time, memory_usage, total_elapsed_time \
             FROM sys.dm_exec_sessions WHERE is_user_process = 1"
                .into(),
        ),
        ("mssql", ActivityQuery::KillSession) => {
            let id = session_id.filter(|id| *id > 0)?;
            Some(format!("KILL {id}"))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postgres_sessions_sql() {
        let sql = activity_sql("postgres", ActivityQuery::Sessions, None).unwrap();
        assert!(sql.contains("pg_stat_activity"));
    }

    #[test]
    fn kill_requires_positive_id() {
        for driver in ["postgres", "mysql", "mssql"] {
            assert!(activity_sql(driver, ActivityQuery::KillSession, None).is_none());
            assert!(activity_sql(driver, ActivityQuery::KillSession, Some(0)).is_none());
        }
        assert!(
            activity_sql("postgres", ActivityQuery::KillSession, Some(42))
                .unwrap()
                .contains("42")
        );
    }

    #[test]
    fn session_id_accepts_only_positive_integers() {
        assert_eq!(parse_session_id(" 42 "), Some(42));
        assert_eq!(parse_session_id("0"), None);
        assert_eq!(parse_session_id("1; DROP TABLE users"), None);
        assert_eq!(parse_session_id("-1"), None);
    }
}
