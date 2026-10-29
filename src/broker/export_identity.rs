//! Ephemeral mTLS identities for one trusted terminal-export scope.
//!
//! This module neither writes credentials nor exposes private key material.
//! A trusted publisher and broker-side client stager consume the crate-private
//! byte views directly after their independent custody checks succeed.

use crate::broker::export_transport::{ExportScope, RemoteExportConfig};
use anyhow::{Result, ensure};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use sha2::{Digest, Sha256};
use std::fmt;
use time::{Duration, OffsetDateTime};

const MAX_LIFETIME_SECONDS: u64 = 86_400;
const CLOCK_SKEW_SECONDS: i64 = 60;

/// Trusted duration inputs. The leaf lifetime covers the task and the bounded
/// export window, and is never a persistent credential lifetime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExportCredentialLifetime {
    pub task_seconds: u64,
    pub export_seconds: u64,
}

impl ExportCredentialLifetime {
    fn total_seconds(self) -> Result<u64> {
        let total = self
            .task_seconds
            .checked_add(self.export_seconds)
            .ok_or_else(|| anyhow::anyhow!("export_identity_lifetime_invalid"))?;
        ensure!(
            self.task_seconds > 0 && self.export_seconds > 0 && total <= MAX_LIFETIME_SECONDS,
            "export_identity_lifetime_invalid"
        );
        Ok(total)
    }
}

/// Public run binding. This is safe to retain with the export workload
/// metadata; it intentionally contains no certificate or key bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssuedExportMetadata {
    pub scope: ExportScope,
    pub server_dns: String,
    pub client_fingerprint_sha256: String,
    pub expires_at: OffsetDateTime,
}

/// Ephemeral private material. The type deliberately has no Serialize,
/// Display, or derived Debug implementation.
pub struct IssuedExportCredentials {
    metadata: IssuedExportMetadata,
    ca_pem: Vec<u8>,
    server_pem: Vec<u8>,
    server_key_pem: Vec<u8>,
    client_pem: Vec<u8>,
}

impl fmt::Debug for IssuedExportCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IssuedExportCredentials")
            .field("metadata", &self.metadata)
            .finish_non_exhaustive()
    }
}

/// Borrowed server bundle for the trusted Kubernetes publisher.
pub(crate) struct ExportServerFiles<'a> {
    pub(crate) ca_pem: &'a [u8],
    pub(crate) server_pem: &'a [u8],
    pub(crate) server_key_pem: &'a [u8],
}

/// Borrowed client bundle for the trusted broker-side credential stager.
pub(crate) struct ExportClientFiles<'a> {
    pub(crate) ca_pem: &'a [u8],
    pub(crate) client_pem: &'a [u8],
}

impl IssuedExportCredentials {
    pub fn metadata(&self) -> &IssuedExportMetadata {
        &self.metadata
    }

    pub(crate) fn server_files(&self) -> ExportServerFiles<'_> {
        ExportServerFiles {
            ca_pem: &self.ca_pem,
            server_pem: &self.server_pem,
            server_key_pem: &self.server_key_pem,
        }
    }

    pub(crate) fn client_files(&self) -> ExportClientFiles<'_> {
        ExportClientFiles {
            ca_pem: &self.ca_pem,
            client_pem: &self.client_pem,
        }
    }
}

/// Creates a one-run CA and mTLS server/client leaf pair. All identity fields
/// come from trusted operator configuration and the broker's lease scope.
pub fn issue_export_credentials(
    remote: &RemoteExportConfig,
    scope: ExportScope,
    namespace: &str,
    lifetime: ExportCredentialLifetime,
) -> Result<IssuedExportCredentials> {
    remote.validate()?;
    let scope = ExportScope::new(scope.task_id, scope.attempt, scope.lease_generation)?;
    let total_seconds = lifetime.total_seconds()?;
    let service_name = scope.service_name(namespace)?;
    let server_dns = format!("{service_name}.{namespace}.{}", remote.dns_suffix);
    ensure!(dns_name(&server_dns), "export_identity_dns_invalid");

    let now = OffsetDateTime::now_utc();
    let not_before = now - Duration::seconds(CLOCK_SKEW_SECONDS);
    let expires_at = now + Duration::seconds(total_seconds.try_into()?);

    let mut ca_params = CertificateParams::new(Vec::<String>::new())?;
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "EnrichedLab export ephemeral CA");
    ca_params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
    ];
    ca_params.not_before = not_before;
    ca_params.not_after = expires_at + Duration::seconds(CLOCK_SKEW_SECONDS);
    let ca_key = KeyPair::generate()?;
    let ca_certificate = ca_params.self_signed(&ca_key)?;
    let issuer = Issuer::from_params(&ca_params, &ca_key);

    let (server_certificate, server_key) = leaf(
        &issuer,
        &server_dns,
        ExtendedKeyUsagePurpose::ServerAuth,
        not_before,
        expires_at,
    )?;
    let (client_certificate, client_key) = leaf(
        &issuer,
        &format!("export-client-{}", scope.credential_name()),
        ExtendedKeyUsagePurpose::ClientAuth,
        not_before,
        expires_at,
    )?;
    let client_fingerprint_sha256 = hex::encode(Sha256::digest(client_certificate.der()));

    Ok(IssuedExportCredentials {
        metadata: IssuedExportMetadata {
            scope,
            server_dns,
            client_fingerprint_sha256,
            expires_at,
        },
        ca_pem: ca_certificate.pem().into_bytes(),
        server_pem: server_certificate.pem().into_bytes(),
        server_key_pem: server_key.serialize_pem().into_bytes(),
        client_pem: format!("{}{}", client_certificate.pem(), client_key.serialize_pem())
            .into_bytes(),
    })
}

fn leaf(
    issuer: &Issuer<'_, &KeyPair>,
    common_name: &str,
    usage: ExtendedKeyUsagePurpose,
    not_before: OffsetDateTime,
    not_after: OffsetDateTime,
) -> Result<(rcgen::Certificate, KeyPair)> {
    let mut params = if usage == ExtendedKeyUsagePurpose::ServerAuth {
        CertificateParams::new(vec![common_name.to_string()])?
    } else {
        CertificateParams::new(Vec::<String>::new())?
    };
    params
        .distinguished_name
        .push(DnType::CommonName, common_name);
    params.use_authority_key_identifier_extension = true;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![usage];
    params.not_before = not_before;
    params.not_after = not_after;
    let key = KeyPair::generate()?;
    let certificate = params.signed_by(&key, issuer)?;
    Ok((certificate, key))
}

fn dns_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value.starts_with(|character: char| character.is_ascii_alphanumeric())
        && value.ends_with(|character: char| character.is_ascii_alphanumeric())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn dns_name(value: &str) -> bool {
    value.len() <= 253 && value.split('.').all(dns_label)
}
