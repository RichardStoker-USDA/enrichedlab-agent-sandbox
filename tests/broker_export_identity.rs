#![cfg(unix)]

extern crate enrichedlab_agent_sandbox as application;

mod broker {
    pub use ::application::broker::*;
}

#[path = "../src/broker/export_identity.rs"]
mod export_identity;

use broker::export_transport::{ExportScope, RemoteExportConfig};
use export_identity::{ExportCredentialLifetime, issue_export_credentials};
use rustls::{
    ClientConfig, RootCertStore, ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, ServerName, pem::PemObject},
    server::WebPkiClientVerifier,
};
use std::sync::Arc;
use time::OffsetDateTime;
use tokio_rustls::{TlsAcceptor, TlsConnector};
use uuid::Uuid;

fn remote() -> RemoteExportConfig {
    RemoteExportConfig {
        dns_suffix: "svc.cluster.test".into(),
        port: 8443,
        credential_root: "/run/export-clients".into(),
        writer_uid: 65_532,
    }
}

fn scope(generation: i64) -> ExportScope {
    ExportScope::new(Uuid::nil(), 2, generation).unwrap()
}

fn lifetime() -> ExportCredentialLifetime {
    ExportCredentialLifetime {
        task_seconds: 300,
        export_seconds: 60,
    }
}

#[tokio::test]
async fn issued_scope_identities_complete_real_mtls_and_do_not_share_client_leaves() {
    let issued =
        issue_export_credentials(&remote(), scope(7), "sandbox-example-001", lifetime()).unwrap();
    let other =
        issue_export_credentials(&remote(), scope(8), "sandbox-example-001", lifetime()).unwrap();
    let metadata = issued.metadata();
    assert_eq!(metadata.scope, scope(7));
    assert_eq!(
        metadata.server_dns,
        format!(
            "{}.sandbox-example-001.svc.cluster.test",
            scope(7).service_name("sandbox-example-001").unwrap()
        )
    );
    assert_eq!(metadata.client_fingerprint_sha256.len(), 64);
    assert_ne!(
        metadata.client_fingerprint_sha256,
        other.metadata().client_fingerprint_sha256
    );
    assert!(metadata.expires_at > OffsetDateTime::now_utc());
    assert!(!format!("{issued:?}").contains("PRIVATE KEY"));

    let server = issued.server_files();
    let client = issued.client_files();
    assert_eq!(client.ca_pem, server.ca_pem);
    let mut roots = RootCertStore::empty();
    for certificate in CertificateDer::pem_slice_iter(server.ca_pem) {
        roots.add(certificate.unwrap()).unwrap();
    }
    let verifier = WebPkiClientVerifier::builder(Arc::new(roots.clone()))
        .build()
        .unwrap();
    let server_certificates = CertificateDer::pem_slice_iter(server.server_pem)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let server_key = PrivateKeyDer::from_pem_slice(server.server_key_pem).unwrap();
    let server_config = ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(server_certificates, server_key)
        .unwrap();
    let client_certificates = CertificateDer::pem_slice_iter(client.client_pem)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let client_key = PrivateKeyDer::from_pem_slice(client.client_pem).unwrap();
    let client_config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_client_auth_cert(client_certificates, client_key)
        .unwrap();
    let (server_stream, client_stream) = tokio::io::duplex(16 * 1024);
    let server_name = ServerName::try_from(metadata.server_dns.clone()).unwrap();
    let (server_result, client_result) = tokio::join!(
        TlsAcceptor::from(Arc::new(server_config)).accept(server_stream),
        TlsConnector::from(Arc::new(client_config)).connect(server_name, client_stream),
    );
    assert!(server_result.is_ok());
    assert!(client_result.is_ok());
}

#[test]
fn operator_inputs_reject_invalid_scope_lifetime_and_dns() {
    let invalid_lifetimes = [
        ExportCredentialLifetime {
            task_seconds: 0,
            export_seconds: 60,
        },
        ExportCredentialLifetime {
            task_seconds: 60,
            export_seconds: 0,
        },
        ExportCredentialLifetime {
            task_seconds: 86_400,
            export_seconds: 1,
        },
    ];
    for invalid in invalid_lifetimes {
        assert!(
            issue_export_credentials(&remote(), scope(7), "sandbox-example-001", invalid).is_err()
        );
    }
    let mut invalid_remote = remote();
    invalid_remote.dns_suffix = "not/a-dns-name".into();
    assert!(
        issue_export_credentials(&invalid_remote, scope(7), "sandbox-example-001", lifetime())
            .is_err()
    );
    assert!(issue_export_credentials(&remote(), scope(7), "bad_namespace", lifetime()).is_err());
    assert!(
        issue_export_credentials(
            &remote(),
            ExportScope {
                task_id: Uuid::nil(),
                attempt: 0,
                lease_generation: 7,
            },
            "sandbox-example-001",
            lifetime()
        )
        .is_err()
    );
}
