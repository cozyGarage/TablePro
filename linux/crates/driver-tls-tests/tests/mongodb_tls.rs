use tablepro_core::{DatabaseDriver, TlsMode};
use tablepro_driver_tls_tests::DriverTlsFixture;

use drivers_mongodb::MongodbDriver;

#[tokio::test]
#[ignore = "requires the driver tls fixture"]
async fn a_verifying_mode_connects_with_the_fixture_authority() {
    let fixture = DriverTlsFixture::from_env();
    let connection = MongodbDriver
        .connect(fixture.mongo(TlsMode::VerifyFull, Some(fixture.ca_cert.clone())))
        .await
        .expect("verify full must succeed against the fixture certificate");
    let tables = connection.list_tables().await.expect("list collections over TLS");
    assert!(
        tables.iter().any(|table| table.name == "release_items"),
        "the seeded collection must be visible: {tables:?}"
    );
}

#[tokio::test]
#[ignore = "requires the driver tls fixture"]
async fn a_plaintext_mode_is_refused_by_a_tls_only_server() {
    let fixture = DriverTlsFixture::from_env();
    let result = MongodbDriver.connect(fixture.mongo(TlsMode::Disabled, None)).await;
    assert!(
        result.is_err(),
        "a server that requires TLS must refuse an unencrypted client"
    );
}

#[tokio::test]
#[ignore = "requires the driver tls fixture"]
async fn a_verifying_mode_without_an_authority_is_refused() {
    let fixture = DriverTlsFixture::from_env();
    let result = MongodbDriver.connect(fixture.mongo(TlsMode::VerifyFull, None)).await;
    assert!(
        result.is_err(),
        "the system trust store does not know the fixture authority"
    );
}

#[tokio::test]
#[ignore = "requires the driver tls fixture"]
async fn a_verifying_mode_naming_the_wrong_authority_is_refused() {
    let fixture = DriverTlsFixture::from_env();
    let result = MongodbDriver
        .connect(fixture.mongo(TlsMode::VerifyFull, Some(fixture.other_ca_cert.clone())))
        .await;
    assert!(
        result.is_err(),
        "an unrelated authority must not verify the fixture certificate"
    );
}
