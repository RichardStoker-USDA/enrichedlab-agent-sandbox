//! Lab datasets end to end over HTTP: a text table and a workbook rebuilt as cell grids, the files that are refused
//! or held, and the fetcher that reads a caller's grids. A stand-in ClamAV daemon answers; the worker is this
//! package's binary. Datasets need no PDF helper, so these tests run without qpdf and poppler.
#[path = "../src/fixtures.rs"]
mod fixtures;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use enrichedlab_guard::api::{self, App, Metrics};
use enrichedlab_guard::config::{CALLERS_SCHEMA, Callers};
use enrichedlab_guard::dataset::{GRID_MEDIA_TYPE, read_grid};
use enrichedlab_guard::pdf;
use enrichedlab_guard::pipeline::Engines;
use enrichedlab_guard::policy::Policy;
use enrichedlab_guard::rules::Rules;
use enrichedlab_guard::store::Store;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tower::ServiceExt;

const CALLER: &str = "workspace-token-for-tests";
const OTHER: &str = "other-caller-token";
const FETCHER: &str = "worker-fetch-token";
const OPERATOR: &str = "operator-token-for-tests";
const CSV: &str = "text/csv";
const XLSX: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";

fn digest(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

struct Harness {
    app: Arc<App>,
    _dir: tempfile::TempDir,
}

async fn harness() -> Harness {
    harness_with_worker_rules(fixtures::rules_dir()).await
}

/// The service's rules are always the shipped ones; the worker may be given others.
async fn harness_with_worker_rules(worker_rules: PathBuf) -> Harness {
    let daemon = fixtures::fake_daemon(&fixtures::fresh_signatures()).await;
    let dir = tempfile::tempdir().unwrap();
    let callers_file = dir.path().join("callers.json");
    std::fs::write(
        &callers_file,
        json!({"schema": CALLERS_SCHEMA,
            "callers": [
                {"id": "workspace", "token_sha256": digest(CALLER), "use_cases": ["library-paper", "lab-dataset"]},
                {"id": "other", "token_sha256": digest(OTHER), "use_cases": ["lab-dataset"]}],
            "fetchers": [{"id": "worker", "token_sha256": digest(FETCHER), "fetches_for": ["workspace"],
                "use_cases": ["lab-dataset"]}],
            "operators": [{"id": "operator", "token_sha256": digest(OPERATOR)}]})
        .to_string(),
    )
    .unwrap();
    let callers = Callers::load(&callers_file).unwrap();
    let tools = pdf::Tools {
        qpdf: "qpdf".into(),
        pdftotext: "pdftotext".into(),
        pdftoppm: "pdftoppm".into(),
        worker: PathBuf::from(env!("CARGO_BIN_EXE_enrichedlab-guard")),
        rules_dir: worker_rules,
        limits: enrichedlab_guard::helper_limits(60, 1536),
    };
    let policy: Policy = serde_json::from_str(fixtures::POLICY).unwrap();
    policy.validate().unwrap();
    let engines = Engines::new(
        Rules::load(&fixtures::rules_dir()).unwrap(),
        tools,
        Some(daemon),
        Duration::from_secs(5),
        Duration::from_secs(72 * 3600),
        "not used by datasets".into(),
    );
    let app = Arc::new(App {
        engines,
        policy,
        callers_file,
        callers: RwLock::new((Instant::now(), callers)),
        callers_reload: Duration::from_secs(3600),
        store: Store::open(&dir.path().join("store"), 1 << 30).unwrap(),
        permits: tokio::sync::Semaphore::new(4),
        queue_wait: Duration::from_millis(500),
        body_wait: Duration::from_secs(5),
        metrics: Metrics::default(),
    });
    Harness { app, _dir: dir }
}

async fn answer(h: &Harness, request: Request<Body>) -> (StatusCode, Vec<u8>, Option<String>) {
    let response = api::router(Arc::clone(&h.app))
        .oneshot(request)
        .await
        .unwrap();
    let status = response.status();
    let media = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, bytes.to_vec(), media)
}

async fn send_as(
    h: &Harness,
    token: &str,
    use_case: &str,
    media: &str,
    body: &[u8],
) -> (StatusCode, Value) {
    let request = Request::post("/v1/intake")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", media)
        .header("x-guard-use-case", use_case)
        .header("x-guard-tenant", "qualification")
        .header("x-guard-lab", "pilot-lab")
        .header("x-guard-person", "person-1")
        .header("x-guard-file-name", "weights.csv")
        .body(Body::from(body.to_vec()))
        .unwrap();
    let (status, bytes, _) = answer(h, request).await;
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn send(h: &Harness, media: &str, body: &[u8]) -> (StatusCode, Value) {
    send_as(h, CALLER, "lab-dataset", media, body).await
}

async fn get(h: &Harness, token: &str, path: &str) -> (StatusCode, Vec<u8>, Option<String>) {
    let request = Request::get(path)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    answer(h, request).await
}

/// Each finding's code and action.
fn findings(v: &Value) -> Vec<(String, String)> {
    v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["code"].as_str().unwrap().to_string(),
                f["action"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn has(v: &Value, code: &str, action: &str) -> bool {
    findings(v).contains(&(code.to_string(), action.to_string()))
}

#[tokio::test]
async fn a_text_table_passes_as_its_grid_and_only_its_readers_take_it() {
    let h = harness().await;
    let table = b"mouse,weight (g)\nM1,25.4\nM2,<0.5\n";
    let (status, v) = send(&h, CSV, table).await;
    assert_eq!(
        (status, v["outcome"].as_str()),
        (StatusCode::OK, Some("allowed")),
        "{v}"
    );
    assert_eq!(v["detected_type"], "delimited");
    assert_eq!(v["disarmed"]["media_type"], GRID_MEDIA_TYPE);
    let id = v["intake_id"].as_str().unwrap();
    let path = format!("/v1/intake/{id}/disarmed");
    let (status, copy, media) = get(&h, FETCHER, &path).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(media.as_deref(), Some(GRID_MEDIA_TYPE));
    assert_eq!(
        hex::encode(Sha256::digest(&copy)),
        v["disarmed"]["sha256"].as_str().unwrap()
    );
    let (first, sheets) = read_grid(&copy).unwrap();
    assert_eq!(
        first["source"]["sha256"],
        hex::encode(Sha256::digest(table))
    );
    assert_eq!(
        sheets[0],
        [
            json!(["mouse", "weight (g)"]),
            json!(["M1", "25.4"]),
            json!(["M2", "<0.5"])
        ]
    );
    assert_eq!(get(&h, CALLER, &path).await.0, StatusCode::OK);
    assert_eq!(
        get(&h, FETCHER, &format!("/v1/intake/{id}")).await.0,
        StatusCode::OK
    );
    // Another caller and an operator do not take the copy; an operator reads the record.
    assert_eq!(get(&h, OTHER, &path).await.0, StatusCode::NOT_FOUND);
    assert_eq!(get(&h, OPERATOR, &path).await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        get(&h, OPERATOR, &format!("/v1/intake/{id}")).await.0,
        StatusCode::OK
    );
    // A fetcher sends nothing.
    let (status, v) = send_as(&h, FETCHER, "lab-dataset", CSV, table).await;
    assert_eq!(
        (status, v["error"].as_str()),
        (StatusCode::FORBIDDEN, Some("caller_required"))
    );
}

#[tokio::test]
async fn a_fetcher_reads_only_its_callers_and_use_cases() {
    let h = harness().await;
    // The other caller's table, and the workspace's file under the library's use case.
    let (_, theirs) = send_as(&h, OTHER, "lab-dataset", CSV, b"a,b\n1,2\n").await;
    assert_eq!(theirs["outcome"], "allowed", "{theirs}");
    let (_, library) = send_as(&h, CALLER, "library-paper", CSV, b"a,b\n1,2\n").await;
    assert_eq!(library["outcome"], "blocked", "{library}");
    for v in [theirs, library] {
        let id = v["intake_id"].as_str().unwrap();
        assert_eq!(
            get(&h, FETCHER, &format!("/v1/intake/{id}")).await.0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            get(&h, FETCHER, &format!("/v1/intake/{id}/disarmed"))
                .await
                .0,
            StatusCode::NOT_FOUND
        );
    }
}

#[tokio::test]
async fn a_workbook_passes_without_what_runs_or_embeds_under_any_table_name() {
    let h = harness().await;
    let book = fixtures::sample_book()
        .with("xl/vbaProject.bin", b"macro project")
        .with("xl/embeddings/oleObject1.bin", b"object")
        .bytes();
    // Sent as a text table: a spreadsheet program's names are not a disguise between table forms.
    let (status, v) = send(&h, CSV, &book).await;
    assert_eq!(
        (status, v["outcome"].as_str()),
        (StatusCode::OK, Some("allowed")),
        "{v}"
    );
    assert_eq!(v["detected_type"], "xlsx");
    for (code, action) in [
        ("active_macros", "disarm"),
        ("embedded_objects", "disarm"),
        ("hidden_sheets", "annotate"),
        ("formulas_without_values", "annotate"),
    ] {
        assert!(has(&v, code, action), "{code} {v}");
    }
    assert!(
        !findings(&v).iter().any(|(code, _)| code == "type_mismatch"),
        "{v}"
    );
    let id = v["intake_id"].as_str().unwrap();
    let (_, copy, _) = get(&h, FETCHER, &format!("/v1/intake/{id}/disarmed")).await;
    let (first, sheets) = read_grid(&copy).unwrap();
    assert_eq!(first["source"]["type"], "xlsx");
    assert_eq!(sheets[0][2][1], json!(25.4));
    assert_eq!(sheets.len(), 3);
    let hidden = &v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["code"] == "hidden_sheets")
        .unwrap()["count"];
    assert_eq!(hidden, 2);
}

#[tokio::test]
async fn what_is_not_a_table_is_refused_by_its_type() {
    let h = harness().await;
    let archive = fixtures::Book::empty()
        .with("data.csv", b"a,b\n1,2\n")
        .bytes();
    let mut ole = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    ole.resize(512, 0);
    let cases: [(&str, &[u8], &str); 4] = [
        (XLSX, &archive, "zip"),
        ("application/vnd.ms-excel", &ole, "ole"),
        (CSV, b"a,b\n\x00\x01\x02\n", "unknown"),
        (CSV, &fixtures::paper(), "pdf"),
    ];
    for (media, body, detected) in cases {
        let (status, v) = send(&h, media, body).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        assert_eq!(
            (v["outcome"].as_str(), v["detected_type"].as_str()),
            (Some("blocked"), Some(detected)),
            "{v}"
        );
        assert!(has(&v, "type_not_allowed", "block"), "{v}");
        assert!(v["disarmed"].is_null());
    }
    let (_, v) = send(&h, XLSX, &archive).await;
    assert_eq!(
        v["reasons"][0],
        "The file is a ZIP archive, which this upload does not accept. The file was refused."
    );
}

#[tokio::test]
async fn tables_past_a_bound_or_with_hidden_parts_are_refused_or_held() {
    let h = harness().await;
    let long_cell = format!("note\n{}\n", "x".repeat(32_768));
    let (_, v) = send(&h, CSV, long_cell.as_bytes()).await;
    assert_eq!(v["outcome"], "blocked", "{v}");
    assert!(has(&v, "table_too_large", "block"), "{v}");
    assert_eq!(
        v["reasons"][0],
        "A cell holds more text than a spreadsheet cell can. The file was refused."
    );
    let tags: String = "hidden"
        .chars()
        .map(|c| char::from_u32(0xE0000 + c as u32).unwrap())
        .collect();
    let (_, v) = send(&h, CSV, format!("name\nA{tags}\n").as_bytes()).await;
    assert_eq!(v["outcome"], "quarantined", "{v}");
    assert!(has(&v, "unicode_tags", "quarantine"), "{v}");
    let declared = fixtures::sample_book()
        .replace("xl/styles.xml", "<styleSheet", "<!DOCTYPE s><styleSheet")
        .bytes();
    let (_, v) = send(&h, XLSX, &declared).await;
    assert_eq!(v["outcome"], "quarantined", "{v}");
    assert!(has(&v, "xml_entities", "quarantine"), "{v}");
    let (_, v) = send(&h, CSV, b"a,\"b\n1,2\n").await;
    assert_eq!(v["outcome"], "quarantined", "{v}");
    assert!(has(&v, "structure_invalid", "quarantine"), "{v}");
    let eicar = [b"name\n".as_slice(), fixtures::EICAR, b"\n"].concat();
    let (_, v) = send(&h, CSV, &eicar).await;
    assert_eq!(v["outcome"], "blocked", "{v}");
    assert!(has(&v, "malware_signature", "block"), "{v}");
}

#[tokio::test]
async fn a_worker_checking_with_other_rules_holds_the_table() {
    let rules = tempfile::tempdir().unwrap();
    std::fs::copy(
        fixtures::rules_dir().join("guard.yar"),
        rules.path().join("guard.yar"),
    )
    .unwrap();
    std::fs::write(
        rules.path().join("extra.yar"),
        "rule extra_for_tests { meta: severity = \"low\" condition: false }\n",
    )
    .unwrap();
    let h = harness_with_worker_rules(rules.path().to_path_buf()).await;
    let (_, v) = send(&h, CSV, b"a,b\n1,2\n").await;
    assert_eq!(v["outcome"], "quarantined", "{v}");
    assert!(has(&v, "analysis_failed", "quarantine"), "{v}");
    assert!(v["disarmed"].is_null());
}
