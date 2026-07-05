//! TLS utilities for the Partitioned Identity Broker prototype.
//!
//! Generates a self-signed CA and per-service server certificates using `rcgen`,
//! and builds `rustls` configurations that all microservices share so that
//! every inter-service call goes over HTTPS.

use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair, SanType};
use rustls::{ClientConfig, RootCertStore, ServerConfig};
use std::io::BufReader;
use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;
use std::{fs, io};

// ---------------------------------------------------------------------------
// Crypto provider initialisation
// ---------------------------------------------------------------------------

/// Installs the `ring` crypto provider as the process-wide default for rustls.
/// Must be called once before any TLS operation (idempotent – safe to call
/// multiple times).
pub fn install_default_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

// ---------------------------------------------------------------------------
// Certificate generation helpers
// ---------------------------------------------------------------------------

/// Generates all TLS certificates needed by the prototype and writes them
/// as PEM files into `certs_dir`.
///
/// Layout produced:
/// ```text
/// certs/
///   ca.pem            – CA certificate (trusted by every client)
///   ab.pem / .key     – server cert + key for AB (App-facing Broker)
///   ib.pem / .key     – server cert + key for IB (IdP-facing Broker)
///   tts.pem  / .key   – server cert + key for tTS
///   mock_idp.pem/.key – server cert + key for Mock IdP
/// ```
///
/// If `ca.pem` already exists the function is a no-op (idempotent).
pub fn ensure_certs(certs_dir: &str) -> Result<(), Box<dyn std::error::Error>> {
    let dir = Path::new(certs_dir);

    if dir.join("ca.pem").exists() {
        tracing::info!("TLS certificates already present in '{}'", certs_dir);
        return Ok(());
    }

    fs::create_dir_all(dir)?;
    tracing::info!(
        "Generating self-signed TLS certificates in '{}' ...",
        certs_dir
    );

    // ── CA certificate ────────────────────────────────────────────────
    let ca_key = KeyPair::generate()?;
    let mut ca_params = CertificateParams::default();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "Prototype CA");
    ca_params
        .distinguished_name
        .push(DnType::OrganizationName, "Thesis PoC");
    let ca_cert = ca_params.self_signed(&ca_key)?;

    fs::write(dir.join("ca.pem"), ca_cert.pem())?;
    tracing::info!("  [CA]       certificate written");

    // ── Per-service certificates (signed by CA) ───────────────────────
    let services = ["ab", "ib", "tts", "mock_idp"];
    for name in &services {
        let srv_key = KeyPair::generate()?;

        let mut srv_params = CertificateParams::new(vec!["localhost".into()])?;
        srv_params
            .distinguished_name
            .push(DnType::CommonName, *name);
        srv_params
            .subject_alt_names
            .push(SanType::IpAddress(IpAddr::from([127, 0, 0, 1])));

        let srv_cert = srv_params.signed_by(&srv_key, &ca_cert, &ca_key)?;

        fs::write(dir.join(format!("{}.pem", name)), srv_cert.pem())?;
        fs::write(dir.join(format!("{}.key", name)), srv_key.serialize_pem())?;
        tracing::info!("  [{}] certificate + key written", name);
    }

    tracing::info!("All TLS certificates generated successfully");
    Ok(())
}

// ---------------------------------------------------------------------------
// rustls configuration builders
// ---------------------------------------------------------------------------

fn crypto_provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Builds a `rustls::ServerConfig` from PEM-encoded certificate chain and
/// private key strings.
pub fn build_server_config(
    cert_pem: &str,
    key_pem: &str,
) -> Result<ServerConfig, Box<dyn std::error::Error>> {
    let certs = rustls_pemfile::certs(&mut BufReader::new(cert_pem.as_bytes()))
        .collect::<Result<Vec<_>, _>>()?;

    let key = rustls_pemfile::private_key(&mut BufReader::new(key_pem.as_bytes()))?
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no private key in PEM"))?;

    let config = ServerConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(certs, key)?;

    Ok(config)
}

/// Convenience: loads a `ServerConfig` for the given microservice directly
/// from the PEM files on disk.
pub fn load_server_config(
    certs_dir: &str,
    service_name: &str,
) -> Result<ServerConfig, Box<dyn std::error::Error>> {
    let dir = Path::new(certs_dir);
    let cert_pem = fs::read_to_string(dir.join(format!("{}.pem", service_name)))?;
    let key_pem = fs::read_to_string(dir.join(format!("{}.key", service_name)))?;
    build_server_config(&cert_pem, &key_pem)
}

/// Builds a `rustls::ClientConfig` that trusts the prototype CA.
pub fn build_client_config(ca_cert_pem: &str) -> Result<ClientConfig, Box<dyn std::error::Error>> {
    let ca_certs = rustls_pemfile::certs(&mut BufReader::new(ca_cert_pem.as_bytes()))
        .collect::<Result<Vec<_>, _>>()?;

    let mut root_store = RootCertStore::empty();
    for cert in ca_certs {
        root_store.add(cert)?;
    }

    let config = ClientConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()?
        .with_root_certificates(root_store)
        .with_no_client_auth();

    Ok(config)
}

/// Convenience: loads a `ClientConfig` that trusts the CA certificate on disk.
pub fn load_client_config(certs_dir: &str) -> Result<ClientConfig, Box<dyn std::error::Error>> {
    let ca_pem = fs::read_to_string(Path::new(certs_dir).join("ca.pem"))?;
    build_client_config(&ca_pem)
}
