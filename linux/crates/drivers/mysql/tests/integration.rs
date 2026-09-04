use std::str::FromStr;

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use rust_decimal::Decimal;
use serde_json::json;

use drivers_mysql::MysqlDriver;
use tablepro_core::{ConnectOptions, Connection, DatabaseDriver, DriverError, OperationControl, Value};
use testcontainers::ContainerAsync;
use testcontainers::ImageExt;
use testcontainers_modules::mysql::Mysql;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

async fn start_mysql() -> (ContainerAsync<Mysql>, ConnectOptions) {
    let container = Mysql::default()
        .with_env_var("MYSQL_ROOT_PASSWORD", "tablepro_test")
        .with_cmd(["--default-authentication-plugin=mysql_native_password"])
        .start()
        .await
        .expect("start mysql container");
    let host = container.get_host().await.expect("host").to_string();
    let port = container.get_host_port_ipv4(3306).await.expect("port");
    let opts = ConnectOptions {
        host,
        port,
        database: "test".into(),
        username: "root".into(),
        password: secrecy::SecretString::new("tablepro_test".to_string().into()),
        tls: tablepro_core::TlsConfig::disabled(),
        ..Default::default()
    };
    (container, opts)
}

async fn connect(opts: ConnectOptions) -> Box<dyn Connection> {
    MysqlDriver.connect(opts).await.expect("connect")
}

async fn tagged_query_is_active(connection: &dyn Connection, tag: &str) -> bool {
    let sql = format!(
        "SELECT count(*) FROM information_schema.processlist \
         WHERE info LIKE '%{tag}%' AND info NOT LIKE '%processlist%'"
    );
    let result = connection.query(&sql).await.expect("inspect the processlist");
    matches!(result.rows.first().and_then(|row| row.first()), Some(Value::Int(count)) if *count > 0)
}

/// MySQL's `SLEEP()` returns 1 when interrupted instead of raising an
/// error, so it cannot prove a cancellation reached the server. A
/// cross join with a non-indexable predicate is interruptible and
/// reports `ER_QUERY_INTERRUPTED`, which is the outcome under test.
async fn create_long_query_source(connection: &dyn Connection) {
    connection
        .execute("CREATE TABLE cancel_probe (x int NOT NULL)")
        .await
        .expect("create the probe table");
    connection
        .execute(
            "INSERT INTO cancel_probe (x) \
             WITH RECURSIVE s AS (SELECT 1 AS x UNION ALL SELECT x + 1 FROM s WHERE x < 999) SELECT x FROM s",
        )
        .await
        .expect("fill the probe table");
}

fn long_query(tag: &str) -> String {
    format!(
        "SELECT count(*) FROM cancel_probe a, cancel_probe b, cancel_probe c \
         WHERE a.x + b.x + c.x > 0 /* {tag} */"
    )
}

async fn wait_for_tagged_query(connection: &dyn Connection, tag: &str, active: bool) {
    for _ in 0..200 {
        if tagged_query_is_active(connection, tag).await == active {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("query tag {tag} did not reach active={active}");
}

#[tokio::test]
#[ignore = "requires docker"]
async fn connect_list_tables_and_pk_detection() {
    let (_c, opts) = start_mysql().await;
    let conn = connect(opts).await;

    conn.execute(
        "CREATE TABLE pk_demo (
            id int AUTO_INCREMENT PRIMARY KEY,
            name varchar(255) NOT NULL,
            note text NULL
        )",
    )
    .await
    .unwrap();
    conn.execute("INSERT INTO pk_demo (name, note) VALUES ('a', NULL), ('b', 'second')")
        .await
        .unwrap();

    let tables = conn.list_tables().await.unwrap();
    assert!(tables.iter().any(|t| t.name == "pk_demo"));

    let cols = conn.fetch_columns(None, "pk_demo").await.unwrap();
    assert_eq!(cols.len(), 3);
    let id_col = cols.iter().find(|c| c.name == "id").unwrap();
    assert!(id_col.primary_key, "id must be detected as primary key");
    assert!(!id_col.nullable);
    let note_col = cols.iter().find(|c| c.name == "note").unwrap();
    assert!(!note_col.primary_key);
    assert!(note_col.nullable);

    let result = conn.fetch_rows(None, "pk_demo", 0, 100).await.unwrap();
    assert_eq!(result.rows.len(), 2);
    assert!(!result.truncated);
}

#[tokio::test]
#[ignore = "requires docker"]
async fn value_roundtrip_all_types() {
    let (_c, opts) = start_mysql().await;
    let conn = connect(opts).await;

    conn.execute(
        "CREATE TABLE roundtrip (
            id int AUTO_INCREMENT PRIMARY KEY,
            b tinyint(1),
            i_small smallint,
            i_medium mediumint,
            i_big bigint,
            f_single float,
            f_double double,
            num decimal(20,5),
            t text,
            bytes varbinary(64),
            d date,
            tm time,
            dt datetime,
            ts timestamp NULL,
            u varchar(36),
            j json,
            nullable_text text NULL
        )",
    )
    .await
    .unwrap();

    let date = NaiveDate::from_ymd_opt(2024, 6, 15).unwrap();
    let time = NaiveTime::from_hms_opt(13, 45, 30).unwrap();
    let dt = NaiveDateTime::new(date, time);
    let tz: DateTime<Utc> = Utc.with_ymd_and_hms(2024, 6, 15, 13, 45, 30).unwrap();
    let uuid = uuid::Uuid::from_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
    let dec = Decimal::from_str("12345.67890").unwrap();
    let json_val = json!({"k": [1, 2, 3], "nested": {"flag": true}});

    let params = vec![
        Value::Bool(true),
        Value::Int(123),
        Value::Int(456_789),
        Value::Int(9_000_000_000_000_000_000),
        Value::Float(1.5_f64),
        Value::Float(std::f64::consts::PI),
        Value::Decimal(dec),
        Value::Text("hello\nworld".into()),
        Value::Bytes(vec![0xde, 0xad, 0xbe, 0xef]),
        Value::Date(date),
        Value::Time(time),
        Value::DateTime(dt),
        Value::TimestampTz(tz),
        Value::Uuid(uuid),
        Value::Json(json_val.clone()),
        Value::Null,
    ];

    let res = conn
        .execute_params(
            "INSERT INTO roundtrip
             (b, i_small, i_medium, i_big, f_single, f_double, num, t, bytes, d, tm, dt, ts, u, j, nullable_text)
             VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            &params,
        )
        .await
        .unwrap();
    assert_eq!(res.rows_affected, 1);

    let q = conn
        .query(
            "SELECT b, i_small, i_medium, i_big, f_single, f_double, num, t, bytes, d, tm, dt, ts, u, j, nullable_text
             FROM roundtrip ORDER BY id",
        )
        .await
        .unwrap();
    assert_eq!(q.rows.len(), 1);
    let row = &q.rows[0];

    match &row[0] {
        Value::Bool(true) => {}
        Value::Int(1) => {}
        v => panic!("expected tinyint(1) -> Bool(true) or Int(1), got {v:?}"),
    }
    assert!(matches!(row[1], Value::Int(123)));
    assert!(matches!(row[2], Value::Int(456_789)));
    assert!(matches!(row[3], Value::Int(9_000_000_000_000_000_000)));
    match &row[4] {
        Value::Float(f) => assert!((*f - 1.5).abs() < 1e-5),
        v => panic!("expected float, got {v:?}"),
    }
    match &row[5] {
        Value::Float(f) => assert!((*f - std::f64::consts::PI).abs() < 1e-9),
        v => panic!("expected double, got {v:?}"),
    }
    match &row[6] {
        Value::Decimal(d) => assert_eq!(d.to_string(), "12345.67890"),
        v => panic!("expected decimal, got {v:?}"),
    }
    assert_eq!(row[7], Value::Text("hello\nworld".into()));
    assert_eq!(row[8], Value::Bytes(vec![0xde, 0xad, 0xbe, 0xef]));
    assert_eq!(row[9], Value::Date(date));
    assert_eq!(row[10], Value::Time(time));
    assert_eq!(row[11], Value::DateTime(dt));
    assert_eq!(row[12], Value::TimestampTz(tz));
    match &row[13] {
        Value::Text(s) => assert_eq!(s, "550e8400-e29b-41d4-a716-446655440000"),
        v => panic!("expected uuid as text, got {v:?}"),
    }
    match &row[14] {
        Value::Json(v) => assert_eq!(v, &json_val),
        v => panic!("expected json, got {v:?}"),
    }
    assert_eq!(row[15], Value::Null);
}

#[tokio::test]
#[ignore = "requires docker"]
async fn pagination_and_truncated_flag() {
    let (_c, opts) = start_mysql().await;
    let conn = connect(opts).await;

    conn.execute("CREATE TABLE big (i int PRIMARY KEY)").await.unwrap();
    let mut sql = String::from("INSERT INTO big (i) VALUES ");
    for i in 0..50 {
        if i > 0 {
            sql.push(',');
        }
        sql.push_str(&format!("({i})"));
    }
    conn.execute(&sql).await.unwrap();

    let page = conn.fetch_rows(None, "big", 10, 5).await.unwrap();
    assert_eq!(page.rows.len(), 5);
    let firsts: Vec<i64> = page
        .rows
        .iter()
        .map(|r| match r[0] {
            Value::Int(i) => i,
            _ => panic!(),
        })
        .collect();
    assert_eq!(firsts, vec![10, 11, 12, 13, 14]);

    let q = conn.query("SELECT i FROM big ORDER BY i").await.unwrap();
    assert_eq!(q.rows.len(), 50);
    assert!(!q.truncated);
}

#[tokio::test]
#[ignore = "requires docker"]
async fn a_bigint_unsigned_value_past_i64_max_keeps_its_digits() {
    let (_c, opts) = start_mysql().await;
    let conn = connect(opts).await;
    conn.execute("CREATE TABLE big_unsigned (v BIGINT UNSIGNED)")
        .await
        .unwrap();
    conn.execute("INSERT INTO big_unsigned (v) VALUES (18446744073709551615)")
        .await
        .unwrap();

    let result = conn.query("SELECT v FROM big_unsigned").await.unwrap();
    assert_eq!(result.rows, vec![vec![Value::Text("18446744073709551615".into())]]);
}

#[tokio::test]
#[ignore = "requires docker"]
async fn bad_sql_returns_query_error() {
    let (_c, opts) = start_mysql().await;
    let conn = connect(opts).await;

    let err = conn.query("SELECT * FROM no_such_table").await.unwrap_err();
    let msg = format!("{err}").to_lowercase();
    assert!(
        msg.contains("no_such_table") || msg.contains("doesn't exist") || msg.contains("table"),
        "expected error to mention missing table, got: {msg}"
    );
}

#[tokio::test]
#[ignore = "requires docker"]
async fn the_driver_declares_server_side_cancellation() {
    let (_container, opts) = start_mysql().await;
    let connection = connect(opts).await;
    assert!(connection.supports_server_cancellation());
}

#[tokio::test]
#[ignore = "requires docker"]
async fn a_cancelled_query_is_killed_on_the_server_and_the_pool_stays_usable() {
    let (_container, opts) = start_mysql().await;
    let connection: std::sync::Arc<dyn Connection> = MysqlDriver.connect(opts.clone()).await.expect("connect").into();
    let observer = connect(opts).await;
    let token = tokio_util::sync::CancellationToken::new();
    let control = OperationControl::new(token.clone(), None);
    create_long_query_source(observer.as_ref()).await;
    let sql = long_query("tablepro_mysql_cancel");
    let operation_connection = connection.clone();
    let task = tokio::spawn(async move { operation_connection.query_controlled(&sql, &control).await });

    wait_for_tagged_query(observer.as_ref(), "tablepro_mysql_cancel", true).await;
    token.cancel();
    let error = task
        .await
        .expect("query task")
        .expect_err("the query must be cancelled");
    assert!(matches!(error, DriverError::Cancelled), "unexpected error: {error:?}");
    wait_for_tagged_query(observer.as_ref(), "tablepro_mysql_cancel", false).await;

    let result = connection.query("SELECT 1").await.expect("the pool remains usable");
    assert_eq!(result.rows, vec![vec![Value::Int(1)]]);
}

#[tokio::test]
#[ignore = "requires docker"]
async fn a_timed_out_write_is_killed_on_the_server_and_reports_a_timeout() {
    let (_container, opts) = start_mysql().await;
    let connection: std::sync::Arc<dyn Connection> = MysqlDriver.connect(opts.clone()).await.expect("connect").into();
    let observer = connect(opts).await;
    create_long_query_source(observer.as_ref()).await;
    connection
        .execute("CREATE TABLE cancel_sink (total bigint)")
        .await
        .expect("create the sink table");

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    let control = OperationControl::new(tokio_util::sync::CancellationToken::new(), Some(deadline));
    let sql = format!(
        "INSERT INTO cancel_sink (total) {}",
        long_query("tablepro_mysql_timeout")
    );
    let operation_connection = connection.clone();
    let task = tokio::spawn(async move { operation_connection.execute_controlled(&sql, &control).await });

    wait_for_tagged_query(observer.as_ref(), "tablepro_mysql_timeout", true).await;
    let error = task.await.expect("execute task").expect_err("the write must time out");
    assert!(matches!(error, DriverError::TimedOut), "unexpected error: {error:?}");
    wait_for_tagged_query(observer.as_ref(), "tablepro_mysql_timeout", false).await;

    let rows = connection
        .query("SELECT count(*) FROM cancel_sink")
        .await
        .expect("the pool remains usable");
    assert_eq!(
        rows.rows,
        vec![vec![Value::Int(0)]],
        "the aborted insert must not commit"
    );
}

/// A value ending in a backslash used to break out of the literal that
/// "Copy row as INSERT" produced, because MySQL treats a backslash as an
/// escape inside a string. MySQL 8.1 evaluated the unescaped form as the
/// expression `'x\'' OR 1=1` and returned 1 instead of storing the text.
#[tokio::test]
#[ignore = "requires docker"]
async fn a_copied_insert_survives_a_value_that_could_escape_its_literal() {
    let (_c, opts) = start_mysql().await;
    let connection = connect(opts).await;
    connection
        .execute("CREATE TABLE copy_probe (id int PRIMARY KEY, note varchar(200))")
        .await
        .expect("create the probe table");

    let payload = "x\\' OR 1=1 -- ";
    connection
        .execute_params(
            "INSERT INTO copy_probe (id, note) VALUES (?, ?)",
            &[Value::Int(1), Value::Text(payload.into())],
        )
        .await
        .expect("store the payload as data");

    let columns = connection.fetch_columns(None, "copy_probe").await.expect("columns");
    let loaded = connection
        .query("SELECT id, note FROM copy_probe WHERE id = 1")
        .await
        .expect("read the row back");
    let row: Vec<Value> = loaded.rows[0]
        .iter()
        .cloned()
        .map(|value| match value {
            Value::Int(id) => Value::Int(id + 1),
            other => other,
        })
        .collect();

    let sql = tablepro_core::sql_literal::build_insert_literal("mysql", None, "copy_probe", &columns, &row)
        .expect("render the insert");
    connection.execute(&sql).await.expect("the copied insert must execute");

    let after = connection
        .query("SELECT note FROM copy_probe WHERE id = 2")
        .await
        .expect("read the copied row");
    assert_eq!(
        after.rows,
        vec![vec![Value::Text(payload.into())]],
        "the copied row must hold the same text, not the result of evaluating it"
    );
    let count = connection
        .query("SELECT count(*) FROM copy_probe")
        .await
        .expect("count the rows");
    assert_eq!(
        count.rows,
        vec![vec![Value::Int(2)]],
        "the insert must add exactly one row"
    );
}
