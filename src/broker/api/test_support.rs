//! Shared pieces for the API's PostgreSQL qualification tests: a verifier that signs in one identity, an API state on a
//! qualification database, a server on a loopback port, and synthetic packages.
use super::*;
use crate::broker::{
    auth::{AuthError, AuthVerifier},
    model::{PrincipalClass, VerifiedIdentity},
};
use async_trait::async_trait;
use flate2::{Compression, GzBuilder};

pub(super) struct Signed(pub(super) VerifiedIdentity);

#[async_trait]
impl AuthVerifier for Signed {
    async fn verify(&self, _headers: &HeaderMap) -> Result<VerifiedIdentity, AuthError> {
        Ok(self.0.clone())
    }
}

pub(super) fn person(subject: &str, email: &str) -> VerifiedIdentity {
    VerifiedIdentity {
        issuer: "https://identity.example.invalid".into(),
        subject: subject.into(),
        email: email.into(),
        display_name: None,
        class: PrincipalClass::Person,
    }
}

pub(super) fn api(dsn: &str, identity: VerifiedIdentity, root: &Path) -> ApiState {
    ApiState {
        workspace_ui: false,
        site: crate::broker::model::SiteView { organization: None },
        workspace_catalog: Arc::default(),
        store: Store::connect(dsn, "research-site".into(), 2).unwrap(),
        verifier: Arc::new(Signed(identity)),
        artifact_root: root.to_path_buf(),
        public_url: None,
        listings: Arc::default(),
        tables: Arc::default(),
        package_permits: super::permits::Permits::new(2),
        body_permits: super::permits::Permits::new(4),
        output_streams: Arc::default(),
        library: None,
        library_permits: Arc::new(tokio::sync::Semaphore::new(1)),
        knowledge_person: None,
        dataset_guard: None,
        changes: None,
        combine_starting_points: false,
    }
}

pub(super) async fn serve(state: ApiState) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let app = router(state);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    origin
}

pub(super) fn tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(GzBuilder::new().write(Vec::new(), Compression::default()));
    for (path, bytes) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o600);
        header.set_cksum();
        builder.append_data(&mut header, path, *bytes).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
}

pub(super) fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
