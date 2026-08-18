use std::sync::Arc;

use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::connect::HttpConnector;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};
use rustls_pki_types::{CertificateDer, ServerName, UnixTime, pem::PemObject};
use tablepro_core::{DriverError, TlsConfig};

/// Build the connector ClickHouse talks through. The shared TLS modes are
/// mapped onto rustls, which has no CA-only mode, so `VerifyCa` verifies the
/// hostname as well: stricter than requested and never weaker.
pub(crate) fn https_connector(config: &TlsConfig) -> Result<HttpsConnector<HttpConnector>, DriverError> {
    let mut connector = HttpConnector::new();
    connector.enforce_http(false);
    Ok(hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(client_config(config)?)
        .https_or_http()
        .enable_http1()
        .wrap_connector(connector))
}

fn client_config(config: &TlsConfig) -> Result<ClientConfig, DriverError> {
    // Another driver in this workspace pulls in a second rustls provider, so
    // the process-wide default is ambiguous and has to be named here.
    let builder = ClientConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|error| DriverError::Tls(format!("cannot configure TLS: {error}")))?;
    if !config.mode.verifies_cert() {
        return Ok(builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyServer))
            .with_no_client_auth());
    }
    Ok(builder.with_root_certificates(roots(config)?).with_no_client_auth())
}

fn roots(config: &TlsConfig) -> Result<RootCertStore, DriverError> {
    let Some(path) = &config.root_cert else {
        return Ok(RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        });
    };
    let mut store = RootCertStore::empty();
    let certificates = CertificateDer::pem_file_iter(path).map_err(|error| {
        DriverError::Tls(format!(
            "cannot read the certificate authority at {}: {error}",
            path.display()
        ))
    })?;
    for certificate in certificates {
        let certificate = certificate.map_err(|error| {
            DriverError::Tls(format!(
                "cannot parse the certificate authority at {}: {error}",
                path.display()
            ))
        })?;
        store
            .add(certificate)
            .map_err(|error| DriverError::Tls(format!("cannot trust the certificate authority: {error}")))?;
    }
    if store.is_empty() {
        return Err(DriverError::Tls(format!("no certificate found in {}", path.display())));
    }
    Ok(store)
}

/// Used only when the selected mode encrypts without verifying, which is what
/// `Prefer` and `Require` mean across every driver in this workspace.
#[derive(Debug)]
struct AcceptAnyServer;

impl ServerCertVerifier for AcceptAnyServer {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::aws_lc_rs::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}
