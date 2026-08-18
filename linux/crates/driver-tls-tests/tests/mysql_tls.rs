use tablepro_core::{DatabaseDriver, TlsMode};
use tablepro_driver_tls_tests::DriverTlsFixture;

use drivers_mysql::MysqlDriver;

#[tokio::test]
#[ignore = "requires the driver tls fixture"]
async fn a_verifying_mode_connects_with_the_fixture_authority() {
    let fixture = DriverTlsFixture::from_env();
    let connection = MysqlDriver
        .connect(fixture.mysql(TlsMode::VerifyFull, Some(fixture.ca_cert.clone())))
        .await
        .expect("verify full must succeed against the fixture certificate");
    connection.ping().await.expect("a verified session must be usable");
}

#[tokio::test]
#[ignore = "requires the driver tls fixture"]
async fn verify_ca_accepts_the_fixture_authority() {
    let fixture = DriverTlsFixture::from_env();
    let connection = MysqlDriver
        .connect(fixture.mysql(TlsMode::VerifyCa, Some(fixture.ca_cert.clone())))
        .await
        .expect("verify ca must succeed against a chain the authority signed");
    connection.ping().await.expect("a verified session must be usable");
}

#[tokio::test]
#[ignore = "requires the driver tls fixture"]
async fn a_plaintext_mode_is_refused_by_a_tls_only_server() {
    let fixture = DriverTlsFixture::from_env();
    let result = MysqlDriver.connect(fixture.mysql(TlsMode::Disabled, None)).await;
    assert!(
        result.is_err(),
        "a server that requires secure transport must refuse an unencrypted client"
    );
}

#[tokio::test]
#[ignore = "requires the driver tls fixture"]
async fn a_verifying_mode_without_an_authority_is_refused() {
    let fixture = DriverTlsFixture::from_env();
    let result = MysqlDriver.connect(fixture.mysql(TlsMode::VerifyFull, None)).await;
    assert!(
        result.is_err(),
        "the system trust store does not know the fixture authority"
    );
}

#[tokio::test]
#[ignore = "requires the driver tls fixture"]
async fn a_verifying_mode_naming_the_wrong_authority_is_refused() {
    let fixture = DriverTlsFixture::from_env();
    let result = MysqlDriver
        .connect(fixture.mysql(TlsMode::VerifyFull, Some(fixture.other_ca_cert.clone())))
        .await;
    assert!(
        result.is_err(),
        "an unrelated authority must not verify the fixture certificate"
    );
}

#[tokio::test]
#[ignore = "requires the driver tls fixture"]
async fn an_encrypt_only_mode_connects_without_an_authority() {
    let fixture = DriverTlsFixture::from_env();
    let connection = MysqlDriver
        .connect(fixture.mysql(TlsMode::Require, None))
        .await
        .expect("encrypt-only must not require an authority it never checks");
    connection.ping().await.expect("an encrypted session must be usable");
}
