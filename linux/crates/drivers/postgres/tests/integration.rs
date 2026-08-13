use std::str::FromStr;

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use rust_decimal::Decimal;
use serde_json::json;
use uuid::Uuid;

use drivers_postgres::PgDriver;
use tablepro_core::{ConnectOptions, Connection, DatabaseDriver, DriverError, OperationControl, Value};
use testcontainers::ContainerAsync;
use testcontainers::ImageExt;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use tokio_util::sync::CancellationToken;

async fn start_pg() -> (ContainerAsync<Postgres>, ConnectOptions) {
    // Pin to Postgres 16: the introspection query in `fetch_columns`
    // reads `pg_attribute.attgenerated`, which was added in PG 12.
    // testcontainers-modules's default tag is older and breaks the
    // generated-column flag query. PG 11 hit upstream EOL in Nov 2023
    // so production deployments shouldn't be older than this anyway.
    let container = Postgres::default()
        .with_tag("16-alpine")
        .start()
        .await
        .expect("start postgres container");
    let host = container.get_host().await.expect("host").to_string();
    let port = container.get_host_port_ipv4(5432).await.expect("port");
    let opts = ConnectOptions {
        host,
        port,
        database: "postgres".into(),
        username: "postgres".into(),
        password: secrecy::SecretString::new("postgres".to_string().into()),
        tls: tablepro_core::TlsConfig::disabled(),
        ..Default::default()
    };
    (container, opts)
}

async fn connect(opts: ConnectOptions) -> Box<dyn Connection> {
    PgDriver.connect(opts).await.expect("connect")
}

async fn tagged_query_is_active(connection: &dyn Connection, tag: &str) -> bool {
    let sql = format!(
        "SELECT count(*) FROM pg_stat_activity WHERE state = 'active' AND query LIKE '%{tag}%' AND pid <> pg_backend_pid()"
    );
    let result = connection.query(&sql).await.expect("inspect pg_stat_activity");
    matches!(result.rows.first().and_then(|row| row.first()), Some(Value::Int(count)) if *count > 0)
}

async fn wait_for_tagged_query(connection: &dyn Connection, tag: &str, active: bool) {
    for _ in 0..100 {
        if tagged_query_is_active(connection, tag).await == active {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("query tag {tag} did not reach active={active}");
}

#[tokio::test]
#[ignore = "requires docker"]
async fn controlled_cancel_stops_server_query_and_keeps_pool_usable() {
    let (_container, opts) = start_pg().await;
    let connection: std::sync::Arc<dyn Connection> = PgDriver.connect(opts.clone()).await.expect("connect").into();
    let observer = connect(opts).await;
    let token = CancellationToken::new();
    let control = OperationControl::new(token.clone(), None);
    let operation_connection = connection.clone();
    let task = tokio::spawn(async move {
        operation_connection
            .query_controlled("SELECT pg_sleep(30) /* bookie_controlled_cancel */", &control)
            .await
    });

    wait_for_tagged_query(observer.as_ref(), "bookie_controlled_cancel", true).await;
    token.cancel();
    let error = task.await.expect("query task").expect_err("query should be cancelled");
    assert!(matches!(error, DriverError::Cancelled));
    wait_for_tagged_query(observer.as_ref(), "bookie_controlled_cancel", false).await;

    let result = connection.query("SELECT 1").await.expect("pool remains usable");
    assert_eq!(result.rows, vec![vec![Value::Int(1)]]);
}

#[tokio::test]
#[ignore = "requires docker"]
async fn controlled_parameterized_cancel_stops_server_query_and_keeps_pool_usable() {
    let (_container, opts) = start_pg().await;
    let connection: std::sync::Arc<dyn Connection> = PgDriver.connect(opts.clone()).await.expect("connect").into();
    let observer = connect(opts).await;
    let token = CancellationToken::new();
    let control = OperationControl::new(token.clone(), None);
    let operation_connection = connection.clone();
    let task = tokio::spawn(async move {
        operation_connection
            .query_params_controlled(
                "SELECT $1::int FROM pg_sleep($2::double precision) /* bookie_controlled_params_cancel */",
                &[Value::Int(7), Value::Float(30.0)],
                &control,
            )
            .await
    });

    wait_for_tagged_query(observer.as_ref(), "bookie_controlled_params_cancel", true).await;
    token.cancel();
    let error = task.await.expect("query task").expect_err("query should be cancelled");
    assert!(matches!(error, DriverError::Cancelled));
    wait_for_tagged_query(observer.as_ref(), "bookie_controlled_params_cancel", false).await;

    let result = connection.query("SELECT 1").await.expect("pool remains usable");
    assert_eq!(result.rows, vec![vec![Value::Int(1)]]);

    let token = CancellationToken::new();
    let control = OperationControl::new(token.clone(), None);
    let operation_connection = connection.clone();
    let task = tokio::spawn(async move {
        operation_connection
            .execute_params_controlled(
                "SELECT pg_sleep($1::double precision) /* bookie_controlled_execute_params_cancel */",
                &[Value::Float(30.0)],
                &control,
            )
            .await
    });

    wait_for_tagged_query(observer.as_ref(), "bookie_controlled_execute_params_cancel", true).await;
    token.cancel();
    let error = task
        .await
        .expect("execute task")
        .expect_err("execute should be cancelled");
    assert!(matches!(error, DriverError::Cancelled));
    wait_for_tagged_query(observer.as_ref(), "bookie_controlled_execute_params_cancel", false).await;

    let result = connection.query("SELECT 1").await.expect("pool remains usable");
    assert_eq!(result.rows, vec![vec![Value::Int(1)]]);
}

#[tokio::test]
#[ignore = "requires docker"]
async fn controlled_transaction_cancel_allows_rollback() {
    let (_container, opts) = start_pg().await;
    let connection = connect(opts.clone()).await;
    let observer = connect(opts).await;
    connection
        .execute("CREATE TABLE controlled_transaction (value int NOT NULL)")
        .await
        .expect("create table");
    let mut transaction = connection.begin().await.expect("begin transaction");
    transaction
        .execute_params_controlled(
            "INSERT INTO controlled_transaction (value) VALUES ($1)",
            &[Value::Int(9)],
            &OperationControl::new(CancellationToken::new(), None),
        )
        .await
        .expect("insert in transaction");
    let token = CancellationToken::new();
    let control = OperationControl::new(token.clone(), None);
    let task = tokio::spawn(async move {
        let result = transaction
            .query_params_controlled(
                "SELECT $1::int FROM pg_sleep($2::double precision) /* bookie_transaction_cancel */",
                &[Value::Int(7), Value::Float(30.0)],
                &control,
            )
            .await;
        (transaction, result)
    });

    wait_for_tagged_query(observer.as_ref(), "bookie_transaction_cancel", true).await;
    token.cancel();
    let (transaction, result) = task.await.expect("transaction task");
    let error = result.expect_err("transaction query should be cancelled");
    assert!(matches!(error, DriverError::Cancelled));
    wait_for_tagged_query(observer.as_ref(), "bookie_transaction_cancel", false).await;
    transaction.rollback().await.expect("rollback cancelled transaction");

    let result = connection
        .query("SELECT count(*) FROM controlled_transaction")
        .await
        .expect("count rows");
    assert_eq!(result.rows, vec![vec![Value::Int(0)]]);
}

#[tokio::test]
#[ignore = "requires docker"]
async fn controlled_timeout_stops_server_query_and_keeps_pool_usable() {
    let (_container, opts) = start_pg().await;
    let connection = connect(opts.clone()).await;
    let observer = connect(opts).await;
    let control = OperationControl::new(
        CancellationToken::new(),
        Some(tokio::time::Instant::now() + std::time::Duration::from_millis(250)),
    );

    let error = connection
        .query_controlled("SELECT pg_sleep(30) /* bookie_controlled_timeout */", &control)
        .await
        .expect_err("query should time out");
    assert!(matches!(error, DriverError::TimedOut));
    wait_for_tagged_query(observer.as_ref(), "bookie_controlled_timeout", false).await;

    let result = connection.query("SELECT 1").await.expect("pool remains usable");
    assert_eq!(result.rows, vec![vec![Value::Int(1)]]);
}

#[tokio::test]
#[ignore = "requires docker"]
async fn dropped_transaction_discards_uncommitted_session() {
    let (_container, opts) = start_pg().await;
    let connection = connect(opts).await;
    connection
        .execute("CREATE TABLE dropped_transaction (id integer PRIMARY KEY)")
        .await
        .expect("create table");
    let mut transaction = connection.begin().await.expect("begin transaction");
    transaction
        .execute("INSERT INTO dropped_transaction (id) VALUES (1)")
        .await
        .expect("insert row");
    drop(transaction);

    let result = connection
        .query("SELECT count(*) FROM dropped_transaction")
        .await
        .expect("query after dropped transaction");
    assert_eq!(result.rows, vec![vec![Value::Int(0)]]);
}

#[tokio::test]
#[ignore = "requires docker"]
async fn connect_list_tables_and_pk_detection() {
    let (_c, opts) = start_pg().await;
    let conn = connect(opts).await;

    conn.execute(
        "CREATE TABLE pk_demo (
            id serial PRIMARY KEY,
            name text NOT NULL,
            note text
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
    let (_c, opts) = start_pg().await;
    let conn = connect(opts).await;

    conn.execute(
        "CREATE TABLE roundtrip (
            id serial PRIMARY KEY,
            b bool,
            i2 smallint,
            i4 integer,
            i8 bigint,
            f4 real,
            f8 double precision,
            num numeric(20,5),
            t text,
            bytes bytea,
            d date,
            tm time,
            dt timestamp,
            tz timestamptz,
            u uuid,
            j jsonb,
            nullable_text text
        )",
    )
    .await
    .unwrap();

    let date = NaiveDate::from_ymd_opt(2024, 6, 15).unwrap();
    let time = NaiveTime::from_hms_opt(13, 45, 30).unwrap();
    let dt = NaiveDateTime::new(date, time);
    let tz: DateTime<Utc> = Utc.with_ymd_and_hms(2024, 6, 15, 13, 45, 30).unwrap();
    let uuid = Uuid::from_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
    let dec = Decimal::from_str("12345.67890").unwrap();
    let json_val = json!({"k": [1, 2, 3], "nested": {"flag": true}});

    let params = vec![
        Value::Bool(true),
        Value::Int(123),
        Value::Int(2_000_000_000),
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
             (b, i2, i4, i8, f4, f8, num, t, bytes, d, tm, dt, tz, u, j, nullable_text)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)",
            &params,
        )
        .await
        .unwrap();
    assert_eq!(res.rows_affected, 1);

    let q = conn
        .query(
            "SELECT b, i2, i4, i8, f4, f8, num, t, bytes, d, tm, dt, tz, u, j, nullable_text
             FROM roundtrip ORDER BY id",
        )
        .await
        .unwrap();
    assert_eq!(q.rows.len(), 1);
    let row = &q.rows[0];

    assert!(matches!(row[0], Value::Bool(true)));
    assert!(matches!(row[1], Value::Int(123)));
    assert!(matches!(row[2], Value::Int(2_000_000_000)));
    assert!(matches!(row[3], Value::Int(9_000_000_000_000_000_000)));
    match &row[4] {
        Value::Float(f) => assert!((*f - 1.5).abs() < 1e-5),
        v => panic!("expected float, got {v:?}"),
    }
    match &row[5] {
        Value::Float(f) => assert!((*f - std::f64::consts::PI).abs() < 1e-9),
        v => panic!("expected float, got {v:?}"),
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
    assert_eq!(row[13], Value::Uuid(uuid));
    match &row[14] {
        Value::Json(v) => assert_eq!(v, &json_val),
        v => panic!("expected json, got {v:?}"),
    }
    assert_eq!(row[15], Value::Null);
}

#[tokio::test]
#[ignore = "requires docker"]
async fn pagination_and_truncated_flag() {
    let (_c, opts) = start_pg().await;
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
async fn bad_sql_returns_query_error() {
    let (_c, opts) = start_pg().await;
    let conn = connect(opts).await;

    let err = conn.query("SELECT * FROM no_such_table").await.unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.to_lowercase().contains("no_such_table") || msg.to_lowercase().contains("relation"),
        "expected error to mention missing relation, got: {msg}"
    );
}
