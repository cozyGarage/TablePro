use std::sync::Arc;
use std::time::Duration;

use drivers_sqlite::SqliteDriver;
use tablepro_core::{ConnectOptions, Connection, DatabaseDriver, DriverError, OperationControl, Value};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

fn options_for(path: &str) -> ConnectOptions {
    ConnectOptions {
        database: path.to_string(),
        ..Default::default()
    }
}

async fn connect_file(directory: &TempDir) -> Arc<dyn Connection> {
    let path = directory.path().join("cancel.db");
    let path = path.to_string_lossy().to_string();
    SqliteDriver
        .connect(options_for(&path))
        .await
        .expect("connect to the sqlite file")
        .into()
}

/// A recursive CTE counting to a large bound is pure computation, so it
/// runs long enough to be interrupted and reports `SQLITE_INTERRUPT`
/// when it is.
const LONG_QUERY: &str = "WITH RECURSIVE counter(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM counter WHERE x < 400000000) \
     SELECT count(*) FROM counter";

#[tokio::test(flavor = "multi_thread")]
async fn the_driver_declares_server_side_cancellation() {
    let directory = TempDir::new().expect("temp dir");
    let connection = connect_file(&directory).await;
    assert!(connection.supports_server_cancellation());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_query_is_interrupted_and_the_pool_stays_usable() {
    let directory = TempDir::new().expect("temp dir");
    let connection = connect_file(&directory).await;

    let token = CancellationToken::new();
    let control = OperationControl::new(token.clone(), None);
    let operation_connection = connection.clone();
    let task = tokio::spawn(async move { operation_connection.query_controlled(LONG_QUERY, &control).await });

    tokio::time::sleep(Duration::from_millis(300)).await;
    token.cancel();
    let error = task
        .await
        .expect("query task")
        .expect_err("the query must be cancelled");
    assert!(matches!(error, DriverError::Cancelled), "unexpected error: {error:?}");

    let result = connection.query("SELECT 1").await.expect("the pool remains usable");
    assert_eq!(result.rows, vec![vec![Value::Int(1)]]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_timed_out_write_is_interrupted_and_reports_a_timeout() {
    let directory = TempDir::new().expect("temp dir");
    let connection = connect_file(&directory).await;
    connection
        .execute("CREATE TABLE sink (total integer)")
        .await
        .expect("create the sink table");

    let deadline = tokio::time::Instant::now() + Duration::from_millis(400);
    let control = OperationControl::new(CancellationToken::new(), Some(deadline));
    let sql = format!("INSERT INTO sink (total) {LONG_QUERY}");
    let operation_connection = connection.clone();
    let task = tokio::spawn(async move { operation_connection.execute_controlled(&sql, &control).await });

    let error = task.await.expect("execute task").expect_err("the write must time out");
    assert!(matches!(error, DriverError::TimedOut), "unexpected error: {error:?}");

    let rows = connection
        .query("SELECT count(*) FROM sink")
        .await
        .expect("the pool remains usable");
    assert_eq!(
        rows.rows,
        vec![vec![Value::Int(0)]],
        "the aborted insert must not commit"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_completed_query_is_unaffected_by_the_control() {
    let directory = TempDir::new().expect("temp dir");
    let connection = connect_file(&directory).await;
    let control = OperationControl::new(CancellationToken::new(), None);

    let result = connection
        .query_controlled("SELECT 7", &control)
        .await
        .expect("an uninterrupted query returns its rows");
    assert_eq!(result.rows, vec![vec![Value::Int(7)]]);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_already_cancelled_control_never_reaches_the_database() {
    let directory = TempDir::new().expect("temp dir");
    let connection = connect_file(&directory).await;
    let token = CancellationToken::new();
    token.cancel();
    let control = OperationControl::new(token, None);

    let error = connection
        .query_controlled("SELECT 1", &control)
        .await
        .expect_err("a cancelled control must refuse before dispatch");
    assert!(matches!(error, DriverError::Cancelled), "unexpected error: {error:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_declared_structure_capability_returns_real_catalog_rows() {
    let directory = TempDir::new().expect("temp dir");
    let connection = connect_file(&directory).await;
    connection
        .execute("CREATE TABLE parent (id INTEGER PRIMARY KEY)")
        .await
        .expect("create the referenced table");
    connection
        .execute("CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES parent(id))")
        .await
        .expect("create the referencing table");
    connection
        .execute("CREATE UNIQUE INDEX child_parent_idx ON child(parent_id)")
        .await
        .expect("create the index");

    assert!(SqliteDriver.supports_index_metadata());
    let indexes = connection.fetch_indexes(None, "child").await.expect("fetch indexes");
    assert!(
        indexes.iter().any(|index| index.name == "child_parent_idx"),
        "a driver that declares index support must return the index it created: {indexes:?}"
    );

    assert!(SqliteDriver.supports_foreign_key_metadata());
    let foreign_keys = connection
        .fetch_foreign_keys(None, "child")
        .await
        .expect("fetch foreign keys");
    assert!(
        foreign_keys
            .iter()
            .any(|key| key.ref_table == "parent" && key.columns == vec!["parent_id".to_string()]),
        "a driver that declares foreign-key support must return the constraint it created: {foreign_keys:?}"
    );
}
