use std::{
    collections::BTreeMap,
    fs,
    net::IpAddr,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, SanType,
};
use rustls::{
    RootCertStore, ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
    server::WebPkiClientVerifier,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    config::{RuntimeConfig, create_private_dir, set_mode, write_private},
    state::validate_name,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PkiIndex {
    pub schema_version: u32,
    pub ca_fingerprint_sha256: String,
    pub clients: BTreeMap<String, ClientProfile>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClientProfile {
    pub name: String,
    pub fingerprint_sha256: String,
    pub certificate_path: PathBuf,
    pub private_key_path: PathBuf,
    pub pkcs12_path: Option<PathBuf>,
    pub pkcs12_password_path: Option<PathBuf>,
    pub issued_at: String,
    pub revoked: bool,
}

#[derive(Clone, Debug)]
pub struct ServerIdentity {
    pub certificate_path: PathBuf,
    pub private_key_path: PathBuf,
}

pub fn init(config: &RuntimeConfig) -> Result<PkiIndex> {
    let ca_cert = ca_cert_path(config);
    let ca_key = ca_key_path(config);
    if ca_cert.exists() || ca_key.exists() {
        if ca_cert.exists() && ca_key.exists() {
            return load_index(config);
        }
        bail!("incomplete controller PKI; restore or remove it before reinitializing");
    }
    create_private_dir(&config.pki_dir())?;
    let mut params = CertificateParams::new(Vec::new())?;
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params
        .distinguished_name
        .push(DnType::CommonName, "EnrichedLab Agent Sandbox Local CA");
    params.key_usages.extend([
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
    ]);
    params.not_before = OffsetDateTime::now_utc() - Duration::days(1);
    params.not_after = OffsetDateTime::now_utc() + Duration::days(3650);
    let key = KeyPair::generate()?;
    let cert = params.self_signed(&key)?;
    write_private(&ca_key, key.serialize_pem().as_bytes())?;
    write_private(&ca_cert, cert.pem().as_bytes())?;
    let index = PkiIndex {
        schema_version: 1,
        ca_fingerprint_sha256: fingerprint(cert.der().as_ref()),
        clients: BTreeMap::new(),
    };
    save_index(config, &index)?;
    Ok(index)
}

pub fn issue_client(config: &RuntimeConfig, name: &str) -> Result<ClientProfile> {
    validate_name(name)?;
    let mut index = ensure_initialized(config)?;
    if index.clients.contains_key(name) {
        bail!("client profile {name} already exists");
    }
    let issuer = load_issuer(config)?;
    let mut params = CertificateParams::new(vec![name.to_string()])?;
    params.distinguished_name.push(DnType::CommonName, name);
    params.use_authority_key_identifier_extension = true;
    params.key_usages.push(KeyUsagePurpose::DigitalSignature);
    params
        .extended_key_usages
        .push(ExtendedKeyUsagePurpose::ClientAuth);
    params.not_before = OffsetDateTime::now_utc() - Duration::hours(1);
    params.not_after = OffsetDateTime::now_utc() + Duration::days(365);
    let key = KeyPair::generate()?;
    let cert = params.signed_by(&key, &issuer)?;
    let directory = config.pki_dir().join("clients").join(name);
    create_private_dir(&directory)?;
    let certificate_path = directory.join("client.crt");
    let private_key_path = directory.join("client.key");
    write_private(&certificate_path, cert.pem().as_bytes())?;
    write_private(&private_key_path, key.serialize_pem().as_bytes())?;

    let (pkcs12_path, pkcs12_password_path) =
        create_pkcs12(config, &directory, &certificate_path, &private_key_path)?;
    let profile = ClientProfile {
        name: name.into(),
        fingerprint_sha256: fingerprint(cert.der().as_ref()),
        certificate_path,
        private_key_path,
        pkcs12_path,
        pkcs12_password_path,
        issued_at: OffsetDateTime::now_utc().format(&Rfc3339)?,
        revoked: false,
    };
    index.clients.insert(name.into(), profile.clone());
    save_index(config, &index)?;
    Ok(profile)
}

pub fn revoke_client(config: &RuntimeConfig, name: &str) -> Result<ClientProfile> {
    let mut index = load_index(config)?;
    let profile = index
        .clients
        .get_mut(name)
        .with_context(|| format!("unknown client profile {name}"))?;
    profile.revoked = true;
    let result = profile.clone();
    save_index(config, &index)?;
    Ok(result)
}

pub fn load_index(config: &RuntimeConfig) -> Result<PkiIndex> {
    let path = index_path(config);
    let body = fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let index: PkiIndex = serde_json::from_slice(&body)?;
    if index.schema_version != 1 {
        bail!("unsupported PKI index schema");
    }
    Ok(index)
}

pub fn allowed_fingerprints(config: &RuntimeConfig, names: &[String]) -> Result<Vec<String>> {
    let index = load_index(config)?;
    let mut fingerprints = Vec::new();
    for name in names {
        let profile = index
            .clients
            .get(name)
            .with_context(|| format!("unknown client profile {name}"))?;
        if profile.revoked {
            bail!("client profile {name} is revoked");
        }
        fingerprints.push(profile.fingerprint_sha256.clone());
    }
    fingerprints.sort();
    fingerprints.dedup();
    Ok(fingerprints)
}

pub fn fingerprint_is_active(config: &RuntimeConfig, fingerprint: &str) -> Result<bool> {
    let index = load_index(config)?;
    Ok(index
        .clients
        .values()
        .any(|profile| profile.fingerprint_sha256 == fingerprint && !profile.revoked))
}

pub fn ensure_server(config: &RuntimeConfig, bind: IpAddr) -> Result<ServerIdentity> {
    ensure_initialized(config)?;
    let stem = bind.to_string().replace(['.', ':'], "_");
    let directory = config.pki_dir().join("servers").join(stem);
    let certificate_path = directory.join("server.crt");
    let private_key_path = directory.join("server.key");
    if certificate_path.exists() && private_key_path.exists() {
        return Ok(ServerIdentity {
            certificate_path,
            private_key_path,
        });
    }
    if certificate_path.exists() || private_key_path.exists() {
        bail!("incomplete server identity for {bind}");
    }
    create_private_dir(&directory)?;
    let issuer = load_issuer(config)?;
    let mut params = CertificateParams::new(Vec::new())?;
    params.subject_alt_names.push(SanType::IpAddress(bind));
    params
        .distinguished_name
        .push(DnType::CommonName, format!("agent-sandbox-{bind}"));
    params.use_authority_key_identifier_extension = true;
    params.key_usages.push(KeyUsagePurpose::DigitalSignature);
    params
        .extended_key_usages
        .push(ExtendedKeyUsagePurpose::ServerAuth);
    params.not_before = OffsetDateTime::now_utc() - Duration::hours(1);
    params.not_after = OffsetDateTime::now_utc() + Duration::days(365);
    let key = KeyPair::generate()?;
    let cert = params.signed_by(&key, &issuer)?;
    write_private(&certificate_path, cert.pem().as_bytes())?;
    write_private(&private_key_path, key.serialize_pem().as_bytes())?;
    Ok(ServerIdentity {
        certificate_path,
        private_key_path,
    })
}

pub fn server_config(config: &RuntimeConfig, bind: IpAddr) -> Result<ServerConfig> {
    let identity = ensure_server(config, bind)?;
    let ca_certs = read_certs(&ca_cert_path(config))?;
    let mut roots = RootCertStore::empty();
    for cert in ca_certs {
        roots.add(cert)?;
    }
    let verifier = WebPkiClientVerifier::builder(Arc::new(roots)).build()?;
    let certs = read_certs(&identity.certificate_path)?;
    let key = read_key(&identity.private_key_path)?;
    let mut server = ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(certs, key)?;
    server.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(server)
}

pub fn capability_server_config(config: &RuntimeConfig, bind: IpAddr) -> Result<ServerConfig> {
    let identity = ensure_server(config, bind)?;
    let certs = read_certs(&identity.certificate_path)?;
    let key = read_key(&identity.private_key_path)?;
    let mut server = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    server.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(server)
}

pub fn peer_fingerprint(
    stream: &tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
) -> Result<String> {
    let certificates = stream
        .get_ref()
        .1
        .peer_certificates()
        .context("client certificate missing after mTLS")?;
    let leaf = certificates
        .first()
        .context("client certificate chain is empty")?;
    Ok(fingerprint(leaf.as_ref()))
}

pub fn ca_cert_path(config: &RuntimeConfig) -> PathBuf {
    config.pki_dir().join("ca.crt")
}
fn ca_key_path(config: &RuntimeConfig) -> PathBuf {
    config.pki_dir().join("ca.key")
}
fn index_path(config: &RuntimeConfig) -> PathBuf {
    config.pki_dir().join("index.json")
}

fn ensure_initialized(config: &RuntimeConfig) -> Result<PkiIndex> {
    if ca_cert_path(config).exists() && ca_key_path(config).exists() && index_path(config).exists()
    {
        load_index(config)
    } else {
        init(config)
    }
}

fn load_issuer(config: &RuntimeConfig) -> Result<Issuer<'static, KeyPair>> {
    let cert = fs::read_to_string(ca_cert_path(config))?;
    let key = fs::read_to_string(ca_key_path(config))?;
    Ok(Issuer::from_ca_cert_pem(&cert, KeyPair::from_pem(&key)?)?)
}

fn save_index(config: &RuntimeConfig, index: &PkiIndex) -> Result<()> {
    let mut body = serde_json::to_vec_pretty(index)?;
    body.push(b'\n');
    write_private(&index_path(config), &body)
}

fn create_pkcs12(
    config: &RuntimeConfig,
    directory: &Path,
    certificate_path: &Path,
    private_key_path: &Path,
) -> Result<(Option<PathBuf>, Option<PathBuf>)> {
    let mut random = [0_u8; 24];
    getrandom::fill(&mut random)?;
    let password_path = directory.join("client-pkcs12-password.txt");
    let pkcs12_path = directory.join("client.p12");
    write_private(
        &password_path,
        format!("{}\n", hex::encode(random)).as_bytes(),
    )?;
    let status = Command::new("openssl")
        .args(["pkcs12", "-export", "-inkey"])
        .arg(private_key_path)
        .arg("-in")
        .arg(certificate_path)
        .arg("-certfile")
        .arg(ca_cert_path(config))
        .arg("-out")
        .arg(&pkcs12_path)
        .arg("-passout")
        .arg(format!("file:{}", password_path.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(status) if status.success() => {
            set_mode(&pkcs12_path, 0o600)?;
            Ok((Some(pkcs12_path), Some(password_path)))
        }
        _ => {
            let _ = fs::remove_file(&password_path);
            let _ = fs::remove_file(&pkcs12_path);
            Ok((None, None))
        }
    }
}

fn read_certs(path: &Path) -> Result<Vec<CertificateDer<'static>>> {
    CertificateDer::pem_file_iter(path)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn read_key(path: &Path) -> Result<PrivateKeyDer<'static>> {
    PrivateKeyDer::from_pem_file(path).map_err(Into::into)
}

fn fingerprint(der: &[u8]) -> String {
    hex::encode(Sha256::digest(der))
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{allowed_fingerprints, fingerprint, init, issue_client, revoke_client};
    use crate::config::{RuntimeConfig, create_private_dir};

    #[test]
    fn fingerprint_is_stable_sha256() {
        assert_eq!(
            fingerprint(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn revoked_client_is_removed_from_future_lease_eligibility() {
        let temp = tempdir().unwrap();
        let config = RuntimeConfig {
            root: temp.path().join("root"),
            state_dir: temp.path().join("state"),
            config_dir: temp.path().join("config"),
            openshell_bin: "openshell".into(),
            docker_bin: "docker".into(),
        };
        create_private_dir(&config.pki_dir()).unwrap();
        init(&config).unwrap();
        let issued = issue_client(&config, "browser-one").unwrap();
        assert_eq!(
            allowed_fingerprints(&config, &["browser-one".into()]).unwrap(),
            vec![issued.fingerprint_sha256]
        );
        revoke_client(&config, "browser-one").unwrap();
        assert!(allowed_fingerprints(&config, &["browser-one".into()]).is_err());
    }
}
