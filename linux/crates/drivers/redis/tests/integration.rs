use drivers_redis::RedisDriver;
use tablepro_core::{ConnectOptions, DatabaseDriver, TlsConfig, Value};
use testcontainers::ContainerAsync;
use testcontainers_modules::redis::Redis;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

async fn start_redis() -> (ContainerAsync<Redis>, String, u16) {
    let container = Redis::default().start().await.expect("start redis container");
    let host = container.get_host().await.expect("host").to_string();
    let port = container.get_host_port_ipv4(6379).await.expect("port");
    (container, host, port)
}

fn opts(host: &str, port: u16, database: &str) -> ConnectOptions {
    ConnectOptions {
        host: host.to_string(),
        port,
        database: database.to_string(),
        username: String::new(),
        password: secrecy::SecretString::new(String::new().into()),
        tls: TlsConfig::disabled(),
        ..Default::default()
    }
}

/// fetch_rows() browses a table named after a database other than the
/// connection's own -- it must restore the connection's own database
/// before returning, or the next call on this shared connection lands
/// wherever the browse last left it.
#[tokio::test]
#[ignore = "requires docker"]
async fn browsing_another_database_does_not_leak_into_later_queries() {
    let (_container, host, port) = start_redis().await;
    let conn = RedisDriver.connect(opts(&host, port, "0")).await.expect("connect");

    conn.query("SET home_key 1").await.expect("seed db0");

    conn.fetch_rows(None, "db2", 0, 10).await.expect("browse db2");

    let result = conn
        .query("GET home_key")
        .await
        .expect("read back on the connection's own db");
    assert_eq!(result.rows, vec![vec![Value::Text("1".into())]]);
}
