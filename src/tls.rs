// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! TLS support, built on `tokio-rustls`. Only compiled with the `tls`
//! cargo feature enabled.

use std::fs::File;
use std::io::BufReader;
use std::sync::Arc;

use tokio::net::TcpStream;
use tokio_rustls::rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use tokio_rustls::rustls::crypto::CryptoProvider;
use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use tokio_rustls::rustls::{ClientConfig, DigitallySignedStruct, Error as RustlsError, RootCertStore, SignatureScheme};
use tokio_rustls::{client::TlsStream, TlsConnector};

use crate::error::{Error, Result};
use crate::options::TlsOptions;

/// Performs a TLS client handshake over an already connected TCP stream.
pub(crate) async fn wrap(stream: TcpStream, host: &str, opts: &TlsOptions) -> Result<TlsStream<TcpStream>> {
    let config = build_config(opts)?;
    let connector = TlsConnector::from(Arc::new(config));
    let name = opts.server_name.clone().unwrap_or_else(|| host.to_string());
    let server_name = ServerName::try_from(name.clone()).map_err(|_| Error::Tls(format!("invalid server name '{}'", name)))?;
    connector
        .connect(server_name, stream)
        .await
        .map_err(|e| Error::Tls(e.to_string()))
}

fn build_config(opts: &TlsOptions) -> Result<ClientConfig> {
    let builder = ClientConfig::builder();

    let builder = if opts.insecure_skip_verify {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyServerCert::new()?))
    } else {
        let mut roots = RootCertStore::empty();
        if let Some(ca_file) = &opts.ca_file {
            load_ca_file(&mut roots, ca_file)?;
        } else {
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        }
        builder.with_root_certificates(roots)
    };

    let config = match (&opts.cert_file, &opts.key_file) {
        (Some(cert_path), Some(key_path)) => {
            let certs = load_certs(cert_path)?;
            let key = load_private_key(key_path)?;
            builder
                .with_client_auth_cert(certs, key)
                .map_err(|e| Error::Tls(format!("invalid client certificate/key: {}", e)))?
        }
        (None, None) => builder.with_no_client_auth(),
        _ => {
            return Err(Error::Tls(
                "TLS client authentication needs both cert_file and key_file".into(),
            ))
        }
    };

    Ok(config)
}

fn load_ca_file(roots: &mut RootCertStore, path: &std::path::Path) -> Result<()> {
    let file = File::open(path).map_err(|e| Error::Tls(format!("cannot open CA file {}: {}", path.display(), e)))?;
    let mut reader = BufReader::new(file);
    for cert in rustls_pemfile::certs(&mut reader) {
        let cert = cert.map_err(|e| Error::Tls(format!("cannot parse CA file {}: {}", path.display(), e)))?;
        roots
            .add(cert)
            .map_err(|e| Error::Tls(format!("invalid CA certificate in {}: {}", path.display(), e)))?;
    }
    Ok(())
}

fn load_certs(path: &std::path::Path) -> Result<Vec<CertificateDer<'static>>> {
    let file = File::open(path).map_err(|e| Error::Tls(format!("cannot open cert file {}: {}", path.display(), e)))?;
    let mut reader = BufReader::new(file);
    rustls_pemfile::certs(&mut reader)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| Error::Tls(format!("cannot parse cert file {}: {}", path.display(), e)))
}

fn load_private_key(path: &std::path::Path) -> Result<tokio_rustls::rustls::pki_types::PrivateKeyDer<'static>> {
    let file = File::open(path).map_err(|e| Error::Tls(format!("cannot open key file {}: {}", path.display(), e)))?;
    let mut reader = BufReader::new(file);
    rustls_pemfile::private_key(&mut reader)
        .map_err(|e| Error::Tls(format!("cannot parse key file {}: {}", path.display(), e)))?
        .ok_or_else(|| Error::Tls(format!("no private key found in {}", path.display())))
}

/// A certificate verifier that accepts any server certificate. Backs
/// [`TlsOptions::insecure_skip_verify`]; for tests only.
#[derive(Debug)]
struct AcceptAnyServerCert {
    provider: Arc<CryptoProvider>,
}

impl AcceptAnyServerCert {
    fn new() -> Result<Self> {
        Ok(AcceptAnyServerCert {
            provider: Arc::new(tokio_rustls::rustls::crypto::ring::default_provider()),
        })
    }
}

impl ServerCertVerifier for AcceptAnyServerCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, RustlsError> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, RustlsError> {
        tokio_rustls::rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, RustlsError> {
        tokio_rustls::rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}
