use std::path::PathBuf;

use secrecy::SecretString;
use tablepro_core::{AuthMode, DriverError, Environment, TlsMode};
use tablepro_release_tests::Fixture;
use tablepro_storage::SavedConnection;
use tablepro_transport::{TransportError, connect_options_for, establish};
use uuid::Uuid;

use drivers_postgres::PgDriver;

fn saved(fixture: &Fixture, mode: TlsMode, root_cert: Option<PathBuf>) -> SavedConnection {
    SavedConnection {
        id: Uuid::new_v4(),
        name: "Fixture warehouse".into(),
        driver_id: "postgres".into(),
        host: fixture.proxy_host.clone(),
        port: fixture.proxy_port,
        socket_dir: None,
        database: fixture.database.clone(),
        username: fixture.username.clone(),
        use_tls: mode.encrypts(),
        tls_mode: Some(mode),
        tls_root_cert: root_cert,
        read_only: false,
        auth_mode: AuthMode::Password,
        environment: Environment::Local,
        ssh: None,
        last_opened_at: None,
    }
}

async fn connect(fixture: &Fixture, saved: &SavedConnection) -> Result<(), TransportError> {
    let mut opts = connect_options_for(saved).await?;
    // The fixture has no Secret Service, so the password that would come from
    // the keyring is supplied here. Everything else, including the certificate
    // authority under test, comes from the saved connection.
    opts.password = SecretString::new(fixture.password.clone().into());
    let (connection, _tunnel) = establish(&PgDriver, opts, None).await?;
    connection
        .query("SELECT count(*) FROM release_items")
        .await
        .map_err(TransportError::Driver)?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires the postgres release fixture"]
async fn a_saved_certificate_authority_verifies_a_privately_issued_certificate() {
    let fixture = Fixture::from_env();
    let connection = saved(&fixture, TlsMode::VerifyFull, Some(fixture.ca_cert.clone()));
    connect(&fixture, &connection)
        .await
        .expect("a saved certificate authority must verify the fixture certificate");
}

#[tokio::test]
#[ignore = "requires the postgres release fixture"]
async fn a_saved_connection_without_an_authority_cannot_verify_a_private_certificate() {
    let fixture = Fixture::from_env();
    let connection = saved(&fixture, TlsMode::VerifyFull, None);
    let error = connect(&fixture, &connection)
        .await
        .expect_err("the system trust store does not know the fixture authority");
    assert!(
        matches!(error, TransportError::Driver(DriverError::Tls(_))),
        "expected a TLS error, got {error}"
    );
}

#[tokio::test]
#[ignore = "requires the postgres release fixture"]
async fn a_saved_connection_naming_the_wrong_authority_is_refused() {
    let fixture = Fixture::from_env();
    let connection = saved(&fixture, TlsMode::VerifyFull, Some(fixture.other_ca_cert.clone()));
    let error = connect(&fixture, &connection)
        .await
        .expect_err("an unrelated authority must not verify the fixture certificate");
    assert!(
        matches!(error, TransportError::Driver(DriverError::Tls(_))),
        "expected a TLS error, got {error}"
    );
}

#[tokio::test]
#[ignore = "requires the postgres release fixture"]
async fn an_unverified_mode_ignores_a_saved_authority() {
    let fixture = Fixture::from_env();
    let connection = saved(&fixture, TlsMode::Require, Some(fixture.other_ca_cert.clone()));
    connect(&fixture, &connection)
        .await
        .expect("encrypt-only mode must not fail on an authority it never checks");
}
