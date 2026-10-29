#![cfg(unix)]

pub use enrichedlab_agent_sandbox as application;
pub mod custody {
    pub use enrichedlab_agent_sandbox::custody::*;
}
pub mod broker {
    pub use enrichedlab_agent_sandbox::broker::{model, sandbox};
    pub mod publication_journal {
        use anyhow::Result;
        use serde_json::Value;
        use std::path::Path;
        pub(crate) struct PublicationJournal;
        impl PublicationJournal {
            pub async fn create(_: &Path, _: &str, _: u32) -> Result<Self> {
                Ok(Self)
            }
            pub async fn append(&mut self, _: Value) -> Result<()> {
                Ok(())
            }
        }
        pub(crate) fn valid_private_root(path: &Path) -> bool {
            path.is_absolute() && path != Path::new("/")
        }
    }
}
pub mod output_inventory {
    pub use enrichedlab_agent_sandbox::output_inventory::*;
}
use application::broker::workload_site;

#[path = "../src/broker/output_workload.rs"]
mod output_workload;

use anyhow::{Result, bail};
use application::broker::{
    model::ClaimedTask,
    sandbox::{
        RuntimeCompletion, SandboxBinding, SandboxBindingCatalog, SandboxHandle, SandboxRuntime,
        SandboxTransport,
    },
};
use async_trait::async_trait;
use output_inventory::{OutputInventoryRequest, REQUEST_SCHEMA, SelectedOutput};
use output_workload::{
    OUTPUT_INVENTORY_COMMAND, OutputInventoryGuard, OutputInventoryJobRunner,
    OutputInventoryObject, OutputInventoryObjectKind, OutputInventoryObjectReceipt,
    OutputInventoryRenderer, OutputInventoryTransport, OutputInventoryWorkloadConfig, OutputPvc,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

fn task() -> ClaimedTask {
    ClaimedTask {
        id: Uuid::nil(),
        principal_id: Uuid::nil(),
        group_key: "research".into(),
        profile_key: "reference".into(),
        objective: String::new(),
        logical_agent: "reference".into(),
        lease_generation: 7,
        attempt: 2,
        max_task_attempts: 3,
        max_provider_attempts: 1,
        harness: String::new(),
        thinking: None,
        route: String::new(),
        controller_profile: String::new(),
        gateway_profile: String::new(),
        image_ref: String::new(),
        cpu_limit: String::new(),
        memory_limit: String::new(),
        state_profile: String::new(),
        role: "research".into(),
        max_run_seconds: 300,
    }
}
fn binding() -> SandboxBinding {
    SandboxBinding {
        principal_id: Uuid::nil(),
        principal_hash: "2f4b66b9706c1ef57d67e2cf9ad98127".into(),
        group_key: "research".into(),
        profile_key: "reference".into(),
        profile_id: "example-kata".into(),
        lab_ref: "lab://example/research".into(),
        namespace: "sandbox-example-001".into(),
        runtime_class_name: "kata-clh".into(),
    }
}
fn handle() -> SandboxHandle {
    SandboxHandle {
        task_id: Uuid::nil(),
        worker_id: "worker-1".into(),
        lease_generation: 7,
        namespace: binding().namespace,
        sandbox_name: "run-example-001".into(),
        sandbox_uid: "sandbox-uid-001".into(),
        sandbox_generation: 1,
        lab_ref: binding().lab_ref,
    }
}
fn request() -> OutputInventoryRequest {
    OutputInventoryRequest {
        schema_version: REQUEST_SCHEMA.into(),
        mounted_root: "/artifacts".into(),
        files: vec![SelectedOutput {
            relative_path: "result/report.txt".into(),
            visibility: "researcher".into(),
            role: "result".into(),
            label: "report".into(),
            media_type: "text/plain".into(),
        }],
        max_files: 4,
        max_bytes: 1024,
    }
}
fn config() -> OutputInventoryWorkloadConfig {
    OutputInventoryWorkloadConfig {
        site: Default::default(),
        runtime_class_name: "kata-clh".into(),
        image: format!(
            "registry.example.invalid/inventory@sha256:{}",
            "a".repeat(64)
        ),
        pool_selector: BTreeMap::from([("sandbox.enrichedlab.org/pool".into(), "custody".into())]),
        cpu_request_millis: 100,
        cpu_limit_millis: 200,
        memory_request_mib: 64,
        memory_limit_mib: 128,
        active_deadline_seconds: 120,
        max_request_bytes: 128 * 1024,
        request_memory_mib: 16,
        journal_root: PathBuf::from("/private/journal"),
        writer_uid: unsafe { libc::geteuid() },
    }
}

#[test]
fn renders_restricted_read_only_inventory_job_and_deny_egress() {
    let mut config = config();
    config
        .site
        .labels
        .insert("research.example.org/owner".into(), "science".into());
    config.site.image_pull_secrets.push("registry-pull".into());
    let workload = OutputInventoryRenderer::new(config)
        .unwrap()
        .render(
            &task(),
            &binding(),
            &handle(),
            &request(),
            &OutputPvc {
                name: "artifacts-run-example-001".into(),
                uid: "pvc-uid-001".into(),
            },
            "inventory-request-001",
        )
        .unwrap();
    let pod = &workload.job["spec"]["template"]["spec"];
    assert_eq!(
        workload.job["metadata"]["labels"]["research.example.org/owner"],
        "science"
    );
    assert_eq!(
        workload.job["spec"]["template"]["metadata"]["labels"]["research.example.org/owner"],
        "science"
    );
    assert_eq!(
        workload.job["spec"]["template"]["spec"]["imagePullSecrets"][0]["name"],
        "registry-pull"
    );
    assert_eq!(workload.job["kind"], "Job");
    assert_eq!(workload.job["spec"]["backoffLimit"], 0);
    assert_eq!(pod["automountServiceAccountToken"], false);
    assert_eq!(
        pod["containers"][0]["command"],
        json!([OUTPUT_INVENTORY_COMMAND])
    );
    assert_eq!(
        pod["containers"][0]["volumeMounts"],
        json!([{"name":"artifacts","mountPath":"/artifacts","readOnly":true},{"name":"request-private","mountPath":"/request","readOnly":true}])
    );
    assert_eq!(
        pod["initContainers"][0]["securityContext"]["runAsUser"],
        65532
    );
    assert_eq!(
        pod["initContainers"][0]["volumeMounts"],
        json!([{"name":"request-projected","mountPath":"/projected","readOnly":true},{"name":"request-private","mountPath":"/request"}])
    );
    assert_eq!(
        pod["initContainers"][0]["resources"],
        pod["containers"][0]["resources"]
    );
    assert_eq!(pod["volumes"][2]["emptyDir"]["medium"], "Memory");
    assert_eq!(
        pod["volumes"][0],
        json!({"name":"artifacts","persistentVolumeClaim":{"claimName":"artifacts-run-example-001","readOnly":true}})
    );
    assert!(workload.job.to_string().contains("request.json"));
    assert!(!workload.job.to_string().contains("result/report.txt"));
    assert_eq!(workload.network_policy["spec"]["ingress"], json!([]));
    assert_eq!(workload.network_policy["spec"]["egress"], json!([]));
}

#[test]
fn materialized_private_request_satisfies_inventory_descriptor_loader() {
    let temp = tempfile::TempDir::new().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let private = root.join("request");
    fs::create_dir(&private).unwrap();
    fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
    let bytes = serde_json::to_vec(&json!({
        "schemaVersion": REQUEST_SCHEMA,
        "mountedRoot": "/artifacts",
        "files": [{
            "relativePath": "result/report.txt",
            "visibility": "researcher",
            "role": "result",
            "label": "report",
            "mediaType": "text/plain"
        }],
        "maxFiles": 4,
        "maxBytes": 1024
    }))
    .unwrap();
    let record = private.join("request.json");
    fs::write(&record, bytes).unwrap();
    fs::set_permissions(&record, fs::Permissions::from_mode(0o600)).unwrap();

    let loaded =
        OutputInventoryRequest::load(&private, "request.json", unsafe { libc::geteuid() }).unwrap();
    assert_eq!(loaded.mounted_root, PathBuf::from("/artifacts"));
    assert_eq!(loaded.files.len(), 1);
    assert_eq!(loaded.files[0].relative_path, "result/report.txt");
}
#[test]
fn rejects_wrong_pvc_and_task_selected_root() {
    let renderer = OutputInventoryRenderer::new(config()).unwrap();
    let mut bad = request();
    bad.mounted_root = "/other".into();
    assert!(
        renderer
            .render(
                &task(),
                &binding(),
                &handle(),
                &bad,
                &OutputPvc {
                    name: "artifacts-run-example-001".into(),
                    uid: "pvc-uid-001".into()
                },
                "inventory-request-001"
            )
            .is_err()
    );
    assert!(
        renderer
            .render(
                &task(),
                &binding(),
                &handle(),
                &request(),
                &OutputPvc {
                    name: "other".into(),
                    uid: "pvc-uid-001".into()
                },
                "inventory-request-001"
            )
            .is_err()
    );
}

struct TerminalBoundary;
#[async_trait]
impl SandboxTransport for TerminalBoundary {
    async fn create_sandbox(&self, _: &str, _: &Value) -> Result<Value> {
        bail!("unexpected_create")
    }
    async fn get_sandbox(&self, _: &str, _: &str) -> Result<Value> {
        let h = handle();
        Ok(json!({
            "metadata":{"name":h.sandbox_name,"namespace":h.namespace,"uid":h.sandbox_uid,"generation":h.sandbox_generation},
            "status":{"conditions":[{"type":"Finished","status":"True","observedGeneration":h.sandbox_generation,"reason":"PodSucceeded"}]}
        }))
    }
    async fn get_pod(&self, _: &str, _: &str) -> Result<Option<Value>> {
        let h = handle();
        Ok(Some(json!({
            "metadata":{"name":h.sandbox_name,"namespace":h.namespace,"uid":"sandbox-pod-001","ownerReferences":[{"apiVersion":"agents.x-k8s.io/v1beta1","kind":"Sandbox","uid":h.sandbox_uid,"controller":true}]},
            "spec":{"runtimeClassName":"kata-clh","restartPolicy":"Never","containers":[{"name":"runner"}]},
            "status":{"phase":"Succeeded","containerStatuses":[{"name":"runner","state":{"terminated":{"exitCode":0,"finishedAt":"2026-09-10T00:00:01Z"}}}]}
        })))
    }
    async fn get_pvc(&self, _: &str, _: &str) -> Result<Value> {
        bail!("unexpected_pvc")
    }
    async fn delete_sandbox(&self, _: &str, _: &str, _: &str) -> Result<()> {
        bail!("unexpected_delete")
    }
}

async fn completion() -> RuntimeCompletion {
    let runtime = SandboxRuntime::new(
        TerminalBoundary,
        SandboxBindingCatalog::new(vec![binding()]).unwrap(),
        60,
    )
    .unwrap();
    let observed = runtime
        .observe_pod(&task(), &handle())
        .await
        .unwrap()
        .unwrap();
    runtime
        .observe_terminal(&task(), &handle(), &observed)
        .await
        .unwrap()
        .unwrap()
}

struct AllowGuard;
#[async_trait]
impl OutputInventoryGuard for AllowGuard {
    async fn check(&self) -> Result<bool> {
        Ok(true)
    }
}

struct BlockingTransport {
    reached_pvc: Arc<Notify>,
    creates: Arc<AtomicUsize>,
}
#[async_trait]
impl OutputInventoryTransport for BlockingTransport {
    fn namespace(&self) -> &str {
        "sandbox-example-001"
    }
    async fn create(
        &self,
        _: &output_workload::OutputInventoryObject,
        _: &CancellationToken,
    ) -> Result<output_workload::OutputInventoryObjectReceipt> {
        self.creates.fetch_add(1, Ordering::SeqCst);
        bail!("unexpected_create")
    }
    async fn get_pvc(&self, _: &str, _: &str) -> Result<Value> {
        self.reached_pvc.notify_one();
        std::future::pending::<Result<Value>>().await
    }
    async fn has_active_writer(&self, _: &str, _: &str) -> Result<bool> {
        bail!("unexpected_writer_check")
    }
    async fn get_job(&self, _: &str, _: &str) -> Result<Value> {
        bail!("unexpected_job")
    }
    async fn get_controlled_pod(&self, _: &str, _: &str) -> Result<Option<Value>> {
        bail!("unexpected_pod")
    }
    async fn helper_stdout(&self, _: &str, _: &str, _: &str) -> Result<Vec<u8>> {
        bail!("unexpected_stdout")
    }
}

#[tokio::test]
async fn cancellation_bounds_a_delayed_pvc_observation_before_any_create() {
    let reached_pvc = Arc::new(Notify::new());
    let creates = Arc::new(AtomicUsize::new(0));
    let runner = OutputInventoryJobRunner::new(
        config(),
        BlockingTransport {
            reached_pvc: reached_pvc.clone(),
            creates: creates.clone(),
        },
        std::time::Duration::from_secs(1),
    )
    .unwrap();
    let cancellation = CancellationToken::new();
    let task = task();
    let binding = binding();
    let handle = handle();
    let completion = completion().await;
    let request = request();
    let future = runner.run(
        &task,
        &binding,
        &handle,
        &completion,
        &request,
        OutputPvc {
            name: "artifacts-run-example-001".into(),
            uid: "pvc-uid-001".into(),
        },
        "inventory-request-001",
        &AllowGuard,
        &cancellation,
    );
    tokio::pin!(future);
    tokio::select! {
        _ = reached_pvc.notified() => cancellation.cancel(),
        _ = &mut future => panic!("delayed PVC observation returned early"),
    }
    let error = match future.await {
        Err(error) => error,
        Ok(_) => panic!("delayed PVC observation completed"),
    };
    assert_eq!(error.to_string(), "output_inventory_cancelled");
    assert_eq!(creates.load(Ordering::SeqCst), 0);
}

struct CompletingTransport {
    config: OutputInventoryWorkloadConfig,
    request: OutputInventoryRequest,
    jobs: AtomicUsize,
    tamper_image: bool,
    tamper_volume: bool,
}

#[async_trait]
impl OutputInventoryTransport for CompletingTransport {
    fn namespace(&self) -> &str {
        "sandbox-example-001"
    }
    async fn create(
        &self,
        object: &OutputInventoryObject,
        _: &CancellationToken,
    ) -> Result<OutputInventoryObjectReceipt> {
        let uid = match object.kind() {
            OutputInventoryObjectKind::NetworkPolicy => "network-uid-001",
            OutputInventoryObjectKind::Secret => "secret-uid-001",
            OutputInventoryObjectKind::Job => "job-uid-001",
        };
        Ok(OutputInventoryObjectReceipt {
            kind: object.kind().clone(),
            namespace: object.namespace().into(),
            name: object.name().into(),
            uid: uid.into(),
        })
    }
    async fn get_pvc(&self, namespace: &str, name: &str) -> Result<Value> {
        Ok(json!({"metadata":{"namespace":namespace,"name":name,"uid":"pvc-uid-001"}}))
    }
    async fn has_active_writer(&self, _: &str, _: &str) -> Result<bool> {
        Ok(false)
    }
    async fn get_job(&self, namespace: &str, name: &str) -> Result<Value> {
        let status = if self.jobs.fetch_add(1, Ordering::SeqCst) == 0 {
            json!({})
        } else {
            json!({"conditions":[{"type":"Complete","status":"True"}],"succeeded":1})
        };
        Ok(
            json!({"metadata":{"namespace":namespace,"name":name,"uid":"job-uid-001"},"status":status}),
        )
    }
    async fn get_controlled_pod(&self, namespace: &str, _job_name: &str) -> Result<Option<Value>> {
        let security = json!({"runAsNonRoot":true,"runAsUser":65532,"runAsGroup":65532,"allowPrivilegeEscalation":false,"capabilities":{"drop":["ALL"]},"seccompProfile":{"type":"RuntimeDefault"}});
        let main_image = if self.tamper_image {
            "registry.example.invalid/tampered@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into()
        } else {
            self.config.image.clone()
        };
        let volumes = if self.tamper_volume {
            json!([
                {"name":"artifacts","emptyDir":{"medium":"Memory"}},
                {"name":"request-projected","projected":{"defaultMode":288,"sources":[{"secret":{"name":"inventory-request-001","items":[{"key":"request.json","path":"request.json"}]}}]}},
                {"name":"request-private","emptyDir":{"medium":"Memory","sizeLimit":format!("{}Mi",self.config.request_memory_mib)}}
            ])
        } else {
            json!([
                {"name":"artifacts","persistentVolumeClaim":{"claimName":"artifacts-run-example-001","readOnly":true}},
                {"name":"request-projected","projected":{"defaultMode":288,"sources":[{"secret":{"name":"inventory-request-001","items":[{"key":"request.json","path":"request.json"}]}}]}},
                {"name":"request-private","emptyDir":{"medium":"Memory","sizeLimit":format!("{}Mi",self.config.request_memory_mib)}}
            ])
        };
        Ok(Some(json!({
            "metadata":{"namespace":namespace,"name":"inventory-pod-001","uid":"inventory-pod-uid-001","ownerReferences":[{"uid":"job-uid-001","controller":true}]},
            "spec":{"runtimeClassName":self.config.runtime_class_name,"automountServiceAccountToken":false,"enableServiceLinks":false,
                "initContainers":[{"name":"materialize-request","image":self.config.image,"imagePullPolicy":"IfNotPresent","command":["/bin/sh","-ec"],"args":["umask 077; install -d -m 0700 /request/private; install -m 0600 /projected/request.json /request/private/request.json"],"securityContext":security,"resources":{"requests":{"cpu":"100m","memory":"64Mi"},"limits":{"cpu":"200m","memory":"128Mi"}},"volumeMounts":[{"name":"request-projected","mountPath":"/projected","readOnly":true},{"name":"request-private","mountPath":"/request"}]}],
                "containers":[{"name":"output-inventory","image":main_image,"command":[OUTPUT_INVENTORY_COMMAND],"args":["--request-root","/request/private","--request","request.json","--writer-uid","65532"],"securityContext":security,"resources":{"requests":{"cpu":"100m","memory":"64Mi"},"limits":{"cpu":"200m","memory":"128Mi"}},"volumeMounts":[{"name":"artifacts","mountPath":"/artifacts","readOnly":true},{"name":"request-private","mountPath":"/request","readOnly":true}]}],
                "volumes":volumes},
            "status":{"phase":"Succeeded","containerStatuses":[{"name":"output-inventory","restartCount":0,"state":{"terminated":{"exitCode":0,"finishedAt":"2026-09-10T00:00:02Z"}}}]}
        })))
    }
    async fn helper_stdout(&self, _: &str, _: &str, _: &str) -> Result<Vec<u8>> {
        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Selection<'a> {
            schema_version: &'a str,
            files: &'a [SelectedOutput],
            max_files: usize,
            max_bytes: u64,
        }
        let digest = application::custody::canonical_document_digest(&Selection {
            schema_version: &self.request.schema_version,
            files: &self.request.files,
            max_files: self.request.max_files,
            max_bytes: self.request.max_bytes,
        })
        .unwrap();
        Ok(format!("{}\n", json!({
            "schemaVersion":"sandbox.enrichedlab.org/output-inventory/v1",
            "selectionDigest":digest,
            "files":[{"relativePath":"result/report.txt","visibility":"researcher","role":"result","label":"report","mediaType":"text/plain","sizeBytes":12,"sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}]
        }))
        .into_bytes())
    }
}

#[tokio::test]
async fn pending_job_requires_a_fixed_succeeded_inventory_helper() {
    let cfg = config();
    let runner = OutputInventoryJobRunner::new(
        cfg.clone(),
        CompletingTransport {
            config: cfg,
            request: request(),
            jobs: AtomicUsize::new(0),
            tamper_image: false,
            tamper_volume: false,
        },
        std::time::Duration::from_millis(1),
    )
    .unwrap();
    let completion = completion().await;
    let receipt = runner
        .run(
            &task(),
            &binding(),
            &handle(),
            &completion,
            &request(),
            OutputPvc {
                name: "artifacts-run-example-001".into(),
                uid: "pvc-uid-001".into(),
            },
            "inventory-request-001",
            &AllowGuard,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(receipt.job_uid(), "job-uid-001");
    assert_eq!(receipt.pod_uid(), "inventory-pod-uid-001");
    assert_eq!(receipt.inventory().files.len(), 1);

    let cfg = config();
    let runner = OutputInventoryJobRunner::new(
        cfg.clone(),
        CompletingTransport {
            config: cfg,
            request: request(),
            jobs: AtomicUsize::new(1),
            tamper_image: true,
            tamper_volume: false,
        },
        std::time::Duration::from_millis(1),
    )
    .unwrap();
    let error = match runner
        .run(
            &task(),
            &binding(),
            &handle(),
            &completion,
            &request(),
            OutputPvc {
                name: "artifacts-run-example-001".into(),
                uid: "pvc-uid-001".into(),
            },
            "inventory-request-001",
            &AllowGuard,
            &CancellationToken::new(),
        )
        .await
    {
        Err(error) => error,
        Ok(_) => panic!("tampered helper was accepted"),
    };
    assert_eq!(error.to_string(), "output_inventory_pod_replaced");

    let cfg = config();
    let runner = OutputInventoryJobRunner::new(
        cfg.clone(),
        CompletingTransport {
            config: cfg,
            request: request(),
            jobs: AtomicUsize::new(1),
            tamper_image: false,
            tamper_volume: true,
        },
        std::time::Duration::from_millis(1),
    )
    .unwrap();
    let error = match runner
        .run(
            &task(),
            &binding(),
            &handle(),
            &completion,
            &request(),
            OutputPvc {
                name: "artifacts-run-example-001".into(),
                uid: "pvc-uid-001".into(),
            },
            "inventory-request-001",
            &AllowGuard,
            &CancellationToken::new(),
        )
        .await
    {
        Err(error) => error,
        Ok(_) => panic!("tampered volume source was accepted"),
    };
    assert_eq!(error.to_string(), "output_inventory_pod_replaced");
}

#[path = "../src/broker/output_kubernetes.rs"]
mod output_kubernetes;

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{HeaderMap, Response, StatusCode, Uri},
    routing::get,
};
use output_kubernetes::OutputInventoryKubernetesClient;
use rcgen::{CertifiedKey, generate_simple_self_signed};
use rustls::{
    ServerConfig,
    pki_types::{CertificateDer, PrivatePkcs8KeyDer},
};
use std::{fs, net::SocketAddr, os::unix::fs::PermissionsExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, server::TlsStream};

struct TlsListener {
    tcp: TcpListener,
    tls: TlsAcceptor,
}
impl axum::serve::Listener for TlsListener {
    type Io = TlsStream<TcpStream>;
    type Addr = SocketAddr;
    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let (s, a) = self.tcp.accept().await.unwrap();
            if let Ok(s) = self.tls.accept(s).await {
                return (s, a);
            }
        }
    }
    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.tcp.local_addr()
    }
}
async fn output_api(State(token): State<String>, uri: Uri, headers: HeaderMap) -> Response<Body> {
    if headers.get("authorization").and_then(|x| x.to_str().ok())
        != Some(format!("Bearer {token}").as_str())
    {
        return Response::builder()
            .status(StatusCode::FORBIDDEN)
            .body(Body::empty())
            .unwrap();
    }
    let body = match uri.path() {
        p if p.ends_with("/jobs/output-inventory-a") => {
            json!({"metadata":{"name":"output-inventory-a","namespace":"sandbox-example-001","uid":"job-uid-001"}})
        }
        p if p.ends_with("/pods") => {
            json!({"metadata":{"continue":""},"items":[{"metadata":{"name":"inventory-pod-a","labels":{"batch.kubernetes.io/job-name":"output-inventory-a"}}}]})
        }
        p if p.ends_with("/log") => {
            return Response::builder()
                .status(StatusCode::OK)
                .body(Body::from("inventory\n"))
                .unwrap();
        }
        _ => {
            return Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::empty())
                .unwrap();
        }
    };
    Response::builder()
        .status(StatusCode::OK)
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

#[tokio::test]
async fn kubernetes_client_uses_pinned_tls_exact_paths_and_bounded_log_query() {
    let temp = tempfile::TempDir::new().unwrap();
    let root = temp.path().canonicalize().unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let CertifiedKey { cert, signing_key } =
        generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
    let ca = root.join("ca.pem");
    fs::write(&ca, cert.pem()).unwrap();
    let token = root.join("token");
    fs::write(&token, b"synthetic-token\n").unwrap();
    fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(cert.der().to_vec())],
            PrivatePkcs8KeyDer::from(signing_key.serialize_der()).into(),
        )
        .unwrap();
    let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin: reqwest::Url = format!("https://{}/", tcp.local_addr().unwrap())
        .parse()
        .unwrap();
    let stop = tokio_util::sync::CancellationToken::new();
    let cancelled = stop.clone();
    let app = Router::new()
        .route("/{*path}", get(output_api))
        .with_state("synthetic-token".to_owned());
    let server = tokio::spawn(async move {
        axum::serve(
            TlsListener {
                tcp,
                tls: TlsAcceptor::from(Arc::new(config)),
            },
            app,
        )
        .with_graceful_shutdown(cancelled.cancelled_owned())
        .await
        .unwrap()
    });
    let client = OutputInventoryKubernetesClient::from_service_account(
        origin,
        "sandbox-example-001".into(),
        token,
        &ca,
    )
    .unwrap();
    assert_eq!(
        client
            .get_job("sandbox-example-001", "output-inventory-a")
            .await
            .unwrap()["metadata"]["uid"],
        "job-uid-001"
    );
    assert_eq!(
        client
            .get_controlled_pod("sandbox-example-001", "output-inventory-a")
            .await
            .unwrap()
            .unwrap()["metadata"]["name"],
        "inventory-pod-a"
    );
    assert_eq!(
        client
            .helper_stdout("sandbox-example-001", "inventory-pod-a", "output-inventory")
            .await
            .unwrap(),
        b"inventory\n"
    );
    stop.cancel();
    server.await.unwrap();
}
