//! The service end to end over HTTP, with a stand-in ClamAV daemon, the real qpdf and pdftotext, and this package's
//! binary as the parsing worker. These tests need qpdf and pdftotext; set GUARD_REQUIRE_TOOLS=1 (the container test
//! run does) to fail instead of skipping without them.
#[path = "../src/fixtures.rs"]
mod fixtures;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use enrichedlab_guard::api::{self, App, Metrics};
use enrichedlab_guard::config::{CALLERS_SCHEMA, Caller, Callers, Operator};
use enrichedlab_guard::pdf;
use enrichedlab_guard::pipeline::Engines;
use enrichedlab_guard::policy::{Action, Policy};
use enrichedlab_guard::rules::Rules;
use enrichedlab_guard::store::Store;
use http_body_util::BodyExt;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tower::ServiceExt;

const CALLER: &str = "caller-token-for-tests";
const OTHER: &str = "other-caller-token";
const OPERATOR: &str = "operator-token-for-tests";

/// Runs a test on a runtime with the service's 8 MiB thread stacks, on a worker thread rather than the test thread.
fn on_large_stacks<F: std::future::Future<Output = ()> + Send + 'static>(test: F) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(8 << 20)
        .build()
        .unwrap();
    runtime.block_on(async move { tokio::spawn(test).await.unwrap() });
}

fn tools() -> Option<pdf::Tools> {
    let present = |p: &str| std::process::Command::new(p).arg("-v").output().is_ok();
    if present("qpdf") && present("pdftotext") && present("pdftoppm") {
        return Some(pdf::Tools {
            qpdf: "qpdf".into(),
            pdftotext: "pdftotext".into(),
            pdftoppm: "pdftoppm".into(),
            worker: PathBuf::from(env!("CARGO_BIN_EXE_enrichedlab-guard")),
            rules_dir: fixtures::rules_dir(),
            limits: enrichedlab_guard::helper_limits(60, 1536),
        });
    }
    assert!(
        std::env::var("GUARD_REQUIRE_TOOLS").is_err(),
        "qpdf, pdftotext and pdftoppm are required for this run"
    );
    eprintln!("skipped: qpdf, pdftotext or pdftoppm is not installed");
    None
}

fn library_paper() -> Policy {
    let policy: Policy = serde_json::from_str(fixtures::POLICY).unwrap();
    policy.validate().unwrap();
    policy
}

fn digest(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

struct Harness {
    app: Arc<App>,
    _dir: tempfile::TempDir,
    callers_file: PathBuf,
}

fn callers_json(include_caller: bool) -> String {
    let mut callers = vec![];
    if include_caller {
        callers.push(serde_json::json!({"id": "workspace", "token_sha256": digest(CALLER), "use_cases": ["library-paper"]}));
    }
    callers
        .push(serde_json::json!({"id": "other", "token_sha256": digest(OTHER), "use_cases": []}));
    serde_json::json!({"schema": CALLERS_SCHEMA, "callers": callers,
        "operators": [{"id": "operator", "token_sha256": digest(OPERATOR)}]})
    .to_string()
}

async fn harness(scanner: Option<String>, permits: usize, policy: Policy) -> Option<Harness> {
    let tools = tools()?;
    let qpdf_version = pdf::qpdf_version(&tools).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let callers_file = dir.path().join("callers.json");
    std::fs::write(&callers_file, callers_json(true)).unwrap();
    let engines = Engines::new(
        Rules::load(&fixtures::rules_dir()).unwrap(),
        tools,
        scanner,
        Duration::from_secs(5),
        Duration::from_secs(72 * 3600),
        qpdf_version,
    );
    let app = Arc::new(App {
        engines,
        policy,
        callers_file: callers_file.clone(),
        callers: RwLock::new((
            Instant::now(),
            Callers {
                schema: CALLERS_SCHEMA.into(),
                callers: vec![Caller {
                    id: "workspace".into(),
                    token_sha256: digest(CALLER),
                    use_cases: vec!["library-paper".into()],
                }],
                fetchers: vec![],
                operators: vec![Operator {
                    id: "operator".into(),
                    token_sha256: digest(OPERATOR),
                }],
            },
        )),
        callers_reload: Duration::ZERO,
        store: Store::open(&dir.path().join("store"), 1 << 30).unwrap(),
        permits: tokio::sync::Semaphore::new(permits),
        queue_wait: Duration::from_millis(200),
        body_wait: Duration::from_secs(5),
        metrics: Metrics::default(),
    });
    Some(Harness {
        app,
        _dir: dir,
        callers_file,
    })
}

async fn running() -> Option<Harness> {
    let daemon = fixtures::fake_daemon(&fixtures::fresh_signatures()).await;
    harness(Some(daemon), 4, library_paper()).await
}

async fn send_request(h: &Harness, request: Request<Body>) -> (StatusCode, Value) {
    let response = api::router(Arc::clone(&h.app))
        .oneshot(request)
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn upload(token: &str, media: &str, person: &str, body: Vec<u8>) -> Request<Body> {
    Request::post("/v1/intake")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", media)
        .header("x-guard-use-case", "library-paper")
        .header("x-guard-tenant", "qualification")
        .header("x-guard-lab", "pilot-lab")
        .header("x-guard-person", person)
        .header("x-guard-file-name", "paper%20one.pdf")
        .body(Body::from(body))
        .unwrap()
}

async fn send(
    h: &Harness,
    token: &str,
    media: &str,
    person: &str,
    body: Vec<u8>,
) -> (StatusCode, Value) {
    send_request(h, upload(token, media, person, body)).await
}

async fn get(h: &Harness, token: &str, path: &str) -> (StatusCode, Vec<u8>) {
    let request = Request::get(path)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let response = api::router(Arc::clone(&h.app))
        .oneshot(request)
        .await
        .unwrap();
    let status = response.status();
    (
        status,
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
}

async fn except(h: &Harness, sha: &str) {
    let expires =
        enrichedlab_guard::store::stamp(enrichedlab_guard::store::now() + time::Duration::days(1));
    let body = serde_json::json!({"sha256": sha, "use_case": "library-paper", "approved_by": "lab curator",
        "reason": "reviewed by the lab", "expires_at": expires})
    .to_string();
    let request = |token: &str| {
        Request::post("/v1/exceptions")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(body.clone()))
            .unwrap()
    };
    assert_eq!(
        send_request(h, request(CALLER)).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send_request(h, request(OPERATOR)).await.0,
        StatusCode::CREATED
    );
}

fn codes(v: &Value) -> Vec<String> {
    v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["code"].as_str().unwrap().to_string())
        .collect()
}

fn details(v: &Value) -> Vec<String> {
    v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|f| f["detail"].as_str().map(str::to_string))
        .collect()
}

/// The worker's report on a rebuilt copy: what remains in it.
fn rebuilt_report(copy: &[u8]) -> pdf::Report {
    let doc = lopdf::Document::load_mem(copy).unwrap();
    pdf::report(&doc, &Rules::load(&fixtures::rules_dir()).unwrap())
}

#[test]
fn a_clean_paper_passes_rebuilt_and_its_copy_is_fetched_once_by_its_caller() {
    on_large_stacks(async move {
        let Some(h) = running().await else {
            return;
        };
        let (status, v) = send(&h, CALLER, "application/pdf", "person-1", fixtures::paper()).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        assert_eq!(v["outcome"], "allowed", "{v}");
        assert_eq!(v["retry"], false);
        let id = v["intake_id"].as_str().unwrap();
        let (status, copy) = get(&h, CALLER, &format!("/v1/intake/{id}/disarmed")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            hex::encode(Sha256::digest(&copy)),
            v["disarmed"]["sha256"].as_str().unwrap()
        );
        assert!(copy.starts_with(b"%PDF-"));
        let report = rebuilt_report(&copy);
        assert_eq!(report.active, pdf::Active::default());
        assert_eq!((report.streams.unknown, report.matches.len()), (0, 0));
        assert_eq!(
            get(&h, OTHER, &format!("/v1/intake/{id}/disarmed")).await.0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            get(&h, OPERATOR, &format!("/v1/intake/{id}")).await.0,
            StatusCode::OK
        );
        let record = h.app.store.read_record(id).unwrap().unwrap();
        assert_eq!(
            (
                record.caller.as_str(),
                record.tenant.as_str(),
                record.lab.as_str()
            ),
            ("workspace", "qualification", "pilot-lab")
        );
        assert_eq!(record.file_name.as_deref(), Some("paper one.pdf"));
        assert_ne!(
            record.person, "person-1",
            "the person is stored as a digest"
        );
        let (status, again) =
            send(&h, CALLER, "application/pdf", "person-1", fixtures::paper()).await;
        assert_eq!(
            (
                status,
                again["intake_id"].as_str(),
                again["retry"].as_bool()
            ),
            (StatusCode::OK, Some(id), Some(true))
        );
        let record = h.app.store.read_record(id).unwrap().unwrap();
        assert_eq!((record.retries.len(), record.retry_count), (1, 1));
    })
}

#[test]
fn active_content_is_removed_and_the_text_stays() {
    on_large_stacks(async move {
        let Some(h) = running().await else {
            return;
        };
        let mut doc = fixtures::active_document();
        let (status, v) = send(
            &h,
            CALLER,
            "application/pdf",
            "person-2",
            fixtures::to_bytes(&mut doc),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["outcome"], "allowed", "{v}");
        let found = codes(&v);
        for code in [
            "active_javascript",
            "active_launch",
            "active_embedded_file",
            "active_open_action",
            "active_additional_actions",
            "active_xfa",
        ] {
            assert!(
                found.contains(&code.to_string()),
                "{code} missing from {found:?}"
            );
        }
        assert!(!found.iter().any(|c| c.starts_with("disarm_")), "{found:?}");
        let id = v["intake_id"].as_str().unwrap();
        let copy = get(&h, CALLER, &format!("/v1/intake/{id}/disarmed"))
            .await
            .1;
        let report = rebuilt_report(&copy);
        assert_eq!(report.active, pdf::Active::default());
        assert_eq!(report.streams.unknown, 0);
        assert!(
            v["reasons"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r.as_str().unwrap().contains("JavaScript"))
        );
        // The same content with every action, file and annotation type given by reference: the same findings, and
        // the rebuilt copy is as clean.
        let mut aliased = fixtures::active_document();
        fixtures::indirect_names(&mut aliased, &[b"S", b"Type", b"Subtype"]);
        let (_, a) = send(
            &h,
            CALLER,
            "application/pdf",
            "person-2",
            fixtures::to_bytes(&mut aliased),
        )
        .await;
        assert_eq!(a["outcome"], "allowed", "{a}");
        assert_eq!(codes(&a), found);
        let id = a["intake_id"].as_str().unwrap();
        let copy = get(&h, CALLER, &format!("/v1/intake/{id}/disarmed"))
            .await
            .1;
        let report = rebuilt_report(&copy);
        assert_eq!(report.active, pdf::Active::default());
        assert_eq!(report.streams.unknown, 0);
        // The same again through chains of 300 references, past lopdf's own limit of 128, written both ways: as plain
        // objects and inside object streams. qpdf's normalization reads an object whose value is a reference as a
        // number, so a chained type does not reach the guard's reader, and no finding the plain document lacks appears;
        // the guard's own resolution covers chains that survive normalization (unit tests). Either way the rebuilt
        // copy, which the L2 inspection reads, carries no active content.
        for modern in [false, true] {
            let mut chained = fixtures::active_document();
            fixtures::indirect_chains(&mut chained, &[b"S", b"Type", b"Subtype"], 300);
            let bytes = if modern {
                fixtures::to_modern_bytes(&mut chained)
            } else {
                fixtures::to_bytes(&mut chained)
            };
            let (_, c) = send(&h, CALLER, "application/pdf", "person-2", bytes).await;
            assert_eq!(c["outcome"], "allowed", "{c}");
            assert!(codes(&c).iter().all(|code| found.contains(code)), "{c}");
            let id = c["intake_id"].as_str().unwrap();
            let copy = get(&h, CALLER, &format!("/v1/intake/{id}/disarmed"))
                .await
                .1;
            let report = rebuilt_report(&copy);
            assert_eq!(report.active, pdf::Active::default(), "modern={modern}");
            assert_eq!(report.streams.unknown, 0);
        }
        // A launch action that names a shell is held, not only disarmed.
        let mut shell = fixtures::document("BT /F1 12 Tf 72 700 Td (Results) Tj ET");
        let launch = shell.add_object(
            lopdf::dictionary! {"S" => "Launch", "F" => lopdf::Object::string_literal("cmd.exe")},
        );
        fixtures::catalog_mut(&mut shell).set("OpenAction", launch);
        let (_, v) = send(
            &h,
            CALLER,
            "application/pdf",
            "person-2",
            fixtures::to_bytes(&mut shell),
        )
        .await;
        assert_eq!(v["outcome"], "quarantined", "{v}");
        assert!(
            details(&v).contains(&"pdf_launch_command in a launch action".to_string()),
            "{v}"
        );
    })
}

#[test]
fn malware_blocks_and_a_polyglot_waits_for_an_exception() {
    on_large_stacks(async move {
        let Some(h) = running().await else {
            return;
        };
        let mut infected = fixtures::paper();
        infected.extend_from_slice(b"\n%");
        infected.extend_from_slice(fixtures::EICAR);
        let (_, v) = send(&h, CALLER, "application/pdf", "person-3", infected).await;
        assert_eq!(v["outcome"], "blocked", "{v}");
        assert!(codes(&v).contains(&"malware_signature".to_string()));
        assert!(v["disarmed"].is_null());

        let mut leading = b"<html><script>alert(1)</script>".to_vec();
        leading.extend_from_slice(&fixtures::paper());
        let (_, v) = send(&h, CALLER, "application/pdf", "person-3", leading.clone()).await;
        assert_eq!(v["outcome"], "quarantined", "{v}");
        assert!(codes(&v).contains(&"polyglot".to_string()));
        except(&h, v["sha256"].as_str().unwrap()).await;
        let (_, v) = send(&h, CALLER, "application/pdf", "person-3", leading).await;
        assert_eq!(v["outcome"], "allowed", "{v}");
        assert_eq!(
            v["retry"], false,
            "an exception gives the retry a new evaluation"
        );
        assert!(codes(&v).contains(&"exception_applied".to_string()));
        let id = v["intake_id"].as_str().unwrap();
        let copy = get(&h, CALLER, &format!("/v1/intake/{id}/disarmed"))
            .await
            .1;
        assert!(
            copy.starts_with(b"%PDF-"),
            "the rebuilt copy, not the original"
        );
    })
}

#[test]
fn a_hidden_program_is_held_and_an_exception_releases_only_a_clean_rebuild() {
    on_large_stacks(async move {
        let Some(h) = running().await else {
            return;
        };
        // A compressed program in a stream no page uses: held, and the rebuild removes it.
        let private = fixtures::to_bytes(&mut fixtures::private_program_document());
        let (_, v) = send(&h, CALLER, "application/pdf", "person-5", private.clone()).await;
        assert_eq!(v["outcome"], "quarantined", "{v}");
        assert!(codes(&v).contains(&"private_streams".to_string()), "{v}");
        assert!(
            details(&v).contains(&"embedded_pe_program in data no page uses".to_string()),
            "{v}"
        );
        except(&h, v["sha256"].as_str().unwrap()).await;
        let (_, v) = send(&h, CALLER, "application/pdf", "person-5", private).await;
        assert_eq!(v["outcome"], "allowed", "{v}");
        assert!(codes(&v).contains(&"exception_applied".to_string()));
        let id = v["intake_id"].as_str().unwrap();
        let copy = get(&h, CALLER, &format!("/v1/intake/{id}/disarmed"))
            .await
            .1;
        let report = rebuilt_report(&copy);
        assert_eq!((report.streams.unknown, report.matches.len()), (0, 0));

        // A program as an image's pixel data survives the rebuild, so the exception cannot release it.
        let image = fixtures::to_bytes(&mut fixtures::image_program_document());
        let (_, v) = send(&h, CALLER, "application/pdf", "person-5", image.clone()).await;
        assert_eq!(v["outcome"], "quarantined", "{v}");
        except(&h, v["sha256"].as_str().unwrap()).await;
        let (_, v) = send(&h, CALLER, "application/pdf", "person-5", image).await;
        assert_eq!(v["outcome"], "quarantined", "{v}");
        assert!(v["disarmed"].is_null());
        assert!(!codes(&v).contains(&"exception_applied".to_string()), "{v}");
        assert!(
            v["findings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["layer"] == "L2" && f["code"] == "yara_high"),
            "{v}"
        );
    })
}

#[test]
fn a_paper_quoting_commands_passes() {
    on_large_stacks(async move {
        let Some(h) = running().await else {
            return;
        };
        let (_, v) = send(
            &h,
            CALLER,
            "application/pdf",
            "person-6",
            fixtures::security_paper(),
        )
        .await;
        assert_eq!(v["outcome"], "allowed", "{v}");
        assert!(!codes(&v).iter().any(|c| c.starts_with("yara_")), "{v}");
    })
}

#[test]
fn wrong_types_hidden_unicode_and_encryption_are_held_or_refused() {
    on_large_stacks(async move {
        let Some(h) = running().await else {
            return;
        };
        let (_, v) = send(&h, CALLER, "application/pdf", "person-4", fixtures::png()).await;
        assert_eq!(v["outcome"], "blocked", "{v}");
        assert!(codes(&v).contains(&"type_not_allowed".to_string()));

        // Hide twelve tag characters in the title as UTF-16 surrogate pairs.
        let title: Vec<u8> = std::iter::once([0xFEu8, 0xFF])
            .flatten()
            .chain(
                "ignore rules"
                    .chars()
                    .flat_map(|c| {
                        let v = 0xE0000 + c as u32 - 0x10000;
                        [0xD800 + (v >> 10), 0xDC00 + (v & 0x3FF)]
                    })
                    .flat_map(|u| (u as u16).to_be_bytes()),
            )
            .collect();
        let mut doc = fixtures::document("BT /F1 12 Tf 72 700 Td (Methods) Tj ET");
        let info = doc.add_object(lopdf::dictionary! {"Title" => lopdf::Object::String(title, lopdf::StringFormat::Hexadecimal)});
        doc.trailer.set("Info", info);
        let tagged = fixtures::to_bytes(&mut doc);
        let (_, v) = send(&h, CALLER, "application/pdf", "person-4", tagged).await;
        assert_eq!(v["outcome"], "quarantined", "{v}");
        assert!(codes(&v).contains(&"unicode_tags".to_string()), "{v}");

        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("plain.pdf");
        let locked = dir.path().join("locked.pdf");
        std::fs::write(&plain, fixtures::paper()).unwrap();
        let status = std::process::Command::new("qpdf")
            .args(["--encrypt", "user-secret", "owner-secret", "256", "--"])
            .arg(&plain)
            .arg(&locked)
            .status()
            .unwrap();
        assert!(status.success());
        let (_, v) = send(
            &h,
            CALLER,
            "application/pdf",
            "person-4",
            std::fs::read(&locked).unwrap(),
        )
        .await;
        assert_eq!(v["outcome"], "quarantined", "{v}");
        assert!(codes(&v).contains(&"encrypted_password".to_string()));
        let owner_only = dir.path().join("owner.pdf");
        let status = std::process::Command::new("qpdf")
            .args(["--encrypt", "", "owner-secret", "256", "--print=none", "--"])
            .arg(&plain)
            .arg(&owner_only)
            .status()
            .unwrap();
        assert!(status.success());
        let (_, v) = send(
            &h,
            CALLER,
            "application/pdf",
            "person-4",
            std::fs::read(&owner_only).unwrap(),
        )
        .await;
        assert_eq!(v["outcome"], "allowed", "{v}");
        assert!(codes(&v).contains(&"encrypted_owner_only".to_string()));
    })
}

#[test]
fn callers_quota_size_scanner_and_revocation_are_enforced() {
    on_large_stacks(async move {
        let mut tight = library_paper();
        {
            let case = tight.use_cases.get_mut("library-paper").unwrap();
            case.quota.files_per_day = 1;
            case.max_bytes = 4096;
            case.actions.insert("trailing_data".into(), Action::Disarm);
        }
        let daemon = fixtures::fake_daemon(&fixtures::fresh_signatures()).await;
        let Some(h) = harness(Some(daemon), 4, tight).await else {
            return;
        };
        assert_eq!(
            send(&h, "wrong-token", "application/pdf", "p", fixtures::paper())
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            send(&h, OTHER, "application/pdf", "p", fixtures::paper())
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            send(&h, OPERATOR, "application/pdf", "p", fixtures::paper())
                .await
                .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            send(&h, CALLER, "text/plain", "p", fixtures::paper())
                .await
                .0,
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        );
        // Past the size limit: refused while reading, or before reading when the length is declared.
        let big = vec![b'%'; 5000];
        let (status, v) = send(&h, CALLER, "application/pdf", "p", big.clone()).await;
        assert_eq!(
            (status, v["error"].as_str()),
            (StatusCode::PAYLOAD_TOO_LARGE, Some("too_large"))
        );
        let mut declared = upload(CALLER, "application/pdf", "p", big);
        declared
            .headers_mut()
            .insert("content-length", "5000".parse().unwrap());
        assert_eq!(
            send_request(&h, declared).await.0,
            StatusCode::PAYLOAD_TOO_LARGE
        );
        assert_eq!(
            h.app.store.used(),
            0,
            "a refusal before evaluation writes nothing"
        );
        assert_eq!(
            send(&h, CALLER, "application/pdf", "p", fixtures::paper())
                .await
                .0,
            StatusCode::OK
        );
        let used = h.app.store.used();
        let mut second = fixtures::paper();
        second.extend_from_slice(b"\n% another version\n");
        let (status, v) = send(&h, CALLER, "application/pdf", "p", second).await;
        assert_eq!(
            (status, v["error"].as_str()),
            (StatusCode::TOO_MANY_REQUESTS, Some("quota_exceeded"))
        );
        assert_eq!(h.app.store.used(), used, "a quota refusal writes nothing");
        std::fs::write(&h.callers_file, callers_json(false)).unwrap();
        assert_eq!(
            send(&h, CALLER, "application/pdf", "q", fixtures::paper())
                .await
                .0,
            StatusCode::UNAUTHORIZED,
            "a removed digest stops working"
        );

        let Some(down) = harness(Some("127.0.0.1:9".into()), 4, library_paper()).await else {
            return;
        };
        let (status, v) = send(&down, CALLER, "application/pdf", "p", fixtures::paper()).await;
        assert_eq!(
            (status, v["error"].as_str()),
            (StatusCode::SERVICE_UNAVAILABLE, Some("scanner_unavailable"))
        );
        let stale = fixtures::fake_daemon("Mon Sep  1 00:00:00 2025").await;
        let Some(old) = harness(Some(stale), 4, library_paper()).await else {
            return;
        };
        assert_eq!(
            send(&old, CALLER, "application/pdf", "p", fixtures::paper())
                .await
                .0,
            StatusCode::SERVICE_UNAVAILABLE
        );
    })
}

#[test]
fn images_are_rebuilt_and_a_payload_after_the_image_is_held() {
    on_large_stacks(async move {
        let mut images = library_paper();
        images.use_cases.get_mut("library-paper").unwrap().types = vec![
            enrichedlab_guard::detect::FileType::Png,
            enrichedlab_guard::detect::FileType::Jpeg,
        ];
        let daemon = fixtures::fake_daemon(&fixtures::fresh_signatures()).await;
        let Some(h) = harness(Some(daemon), 4, images).await else {
            return;
        };
        let (_, v) = send(&h, CALLER, "image/png", "p", fixtures::png()).await;
        assert_eq!(v["outcome"], "allowed", "{v}");
        let mut php = fixtures::jpeg();
        php.extend_from_slice(b"<?php system($_GET['c']); ?>");
        let (_, v) = send(&h, CALLER, "image/jpeg", "p", php).await;
        assert_eq!(v["outcome"], "quarantined", "{v}");
        assert!(codes(&v).contains(&"yara_high".to_string()), "{v}");
    })
}
