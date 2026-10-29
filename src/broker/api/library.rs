//! Lab library routes. Every route first requires membership of the lab, and the
//! document service is reached only with the lab's own corpus token from server configuration: a
//! request never names a corpus or a token.
use super::*;
use crate::broker::{
    guard_client::Upload,
    library::{
        GuardOutcome, GuardVerdict, LibraryError, LibraryService, MAX_PAPER_BYTES, check_pdf,
    },
    store::LibraryPaperRecord,
};
use axum::body::Body;
use futures_util::StreamExt;
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;

/// How long an upload waits for its share of the library's upload memory before it is turned away as busy.
const PERMIT_WAIT: Duration = Duration::from_secs(30);
/// The memory the library's uploads may hold at once (384 MiB of the API's 512 MiB limit), in KiB.
pub(super) const UPLOAD_BUDGET_KIB: usize = 384 * 1024;

/// The memory an upload of `length` bytes may hold, in KiB: the original, then the rebuilt copy (at most twice the
/// original, never past the library's limit) and its base64 request body, and at least 16 MiB, which also bounds how
/// many uploads run at once.
fn upload_weight_kib(length: usize) -> u32 {
    let rebuilt = length.saturating_mul(2).min(MAX_PAPER_BYTES);
    let peak = length.max(rebuilt * 7 / 3).max(16 * 1024 * 1024);
    u32::try_from(peak.div_ceil(1024)).unwrap_or(u32::MAX)
}
/// How long a client may take to send a paper once it holds a permit.
const BODY_WAIT: Duration = Duration::from_secs(300);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AddQuery {
    request_id: Uuid,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SearchQuery {
    q: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RemoveRequest {
    reason: String,
    request_id: Uuid,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AcceptRequest {
    accept: bool,
    #[serde(default)]
    reason: Option<String>,
    request_id: Uuid,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LabelRequest {
    kind: String,
    namespace: String,
    value: String,
    request_id: Uuid,
}

fn lab_key(value: &str) -> bool {
    let mut bytes = value.bytes();
    (2..=64).contains(&value.len())
        && bytes.next().is_some_and(|b| b.is_ascii_lowercase())
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
}

fn library_error(error: LibraryError) -> ApiError {
    let (status, message) = match error {
        LibraryError::NotConfigured => (
            StatusCode::CONFLICT,
            "The library is not set up for this lab",
        ),
        LibraryError::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "The document service is unavailable",
        ),
        LibraryError::ParseFailed => (
            StatusCode::UNPROCESSABLE_ENTITY,
            "The file could not be read as a paper",
        ),
        LibraryError::TooLarge => (
            StatusCode::PAYLOAD_TOO_LARGE,
            "The paper is larger than the library accepts",
        ),
        LibraryError::GuardNotConfigured => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Uploads are closed until the upload check is set up",
        ),
        LibraryError::GuardUnavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "The upload check is unavailable",
        ),
        LibraryError::GuardBusy => (StatusCode::TOO_MANY_REQUESTS, "The upload check is busy"),
        LibraryError::Busy => (
            StatusCode::TOO_MANY_REQUESTS,
            "The library is adding other papers",
        ),
        LibraryError::Incomplete => (
            StatusCode::BAD_REQUEST,
            "The file did not arrive whole or in time",
        ),
        LibraryError::LengthRequired => (
            StatusCode::LENGTH_REQUIRED,
            "The upload must state its size",
        ),
        LibraryError::GuardQuota => (
            StatusCode::TOO_MANY_REQUESTS,
            "Today's upload limit for this person is used",
        ),
        LibraryError::Unchecked => (
            StatusCode::CONFLICT,
            "This file matches a paper added before uploads were checked, which is not added again or restored",
        ),
    };
    ApiError {
        status,
        code: error.code(),
        message,
    }
}

/// The guard held or refused the file: its finding codes and intake let the workspace explain it and an operator
/// find it. Nothing was added.
fn guard_refusal(verdict: &GuardVerdict) -> Response {
    let (code, message) = match verdict.outcome {
        GuardOutcome::Blocked => (
            "library_upload_blocked",
            "The upload check refused this file",
        ),
        _ => (
            "library_upload_quarantined",
            "The upload check is holding this file for review",
        ),
    };
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(
            json!({"error": {"code": code, "message": message, "findings": verdict.findings,
            "intake": verdict.intake}}),
        ),
    )
        .into_response()
}

/// Whether a paper's parse record carries the upload guard's evidence: its intake and the digest of the copy kept.
fn guard_checked(paper: &Value) -> bool {
    let guard = &paper["parse"]["guard"];
    guard["intake"]
        .as_str()
        .is_some_and(|intake| Uuid::parse_str(intake).is_ok())
        && guard["rebuilt_sha256"]
            .as_str()
            .is_some_and(|sha| sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// What the same bytes sent again may do with the lab's paper: return it while it is in the library, or restore it
/// after a removal. Neither applies to a paper the upload guard never checked (one added before the guard existed).
#[derive(Debug, PartialEq, Eq)]
enum Known {
    Present,
    Removed,
}

fn known(paper: &Value) -> Result<Known, LibraryError> {
    if !guard_checked(paper) {
        return Err(LibraryError::Unchecked);
    }
    Ok(if paper["removed"].is_null() {
        Known::Present
    } else {
        Known::Removed
    })
}

fn service<'a>(state: &'a ApiState, lab: &str) -> Result<&'a LibraryService, ApiError> {
    state
        .library
        .as_deref()
        .filter(|service| service.configured(lab))
        .ok_or_else(|| library_error(LibraryError::NotConfigured))
}

fn checked_lab(lab: &str) -> Result<(), ApiError> {
    if lab_key(lab) {
        Ok(())
    } else {
        Err(ApiError::not_found("lab_not_found"))
    }
}

/// The lab's papers, the caller's role, and whether the lab has a corpus.
pub(super) async fn library(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(lab): AxumPath<String>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    checked_lab(&lab)?;
    let mut value = state.store.library(actor, &lab).await?;
    value["configured"] = json!(service(&state, &lab).is_ok());
    Ok(Json(value))
}

/// Reads an upload up to `limit` bytes, stopping as soon as it passes the limit.
async fn read_paper(body: Body, limit: usize) -> Result<Vec<u8>, LibraryError> {
    let mut stream = body.into_data_stream();
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| LibraryError::Incomplete)?;
        if out.len() + chunk.len() > limit {
            return Err(LibraryError::TooLarge);
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// Adds a PDF to the lab's library. The upload guard checks it first and the lab's
/// corpus receives the guard's rebuilt copy, never the original; a file the guard holds or refuses is
/// not added. Every add needs the guard, the same bytes again included: they return the lab's paper
/// (restoring it when it had been removed) only when the guard checked that paper. The same request
/// again returns the same answer. No body is read before the caller is known to be a member, the lab's
/// library and upload check are set up, and the upload holds its weight of the library's upload memory
/// (`upload_weight_kib`), so uploads together stay inside the API's memory limit.
pub(super) async fn add_paper(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(lab): AxumPath<String>,
    Query(query): Query<AddQuery>,
    body: Body,
) -> Result<Response, ApiError> {
    require_mutation_intent(&headers)?;
    let actor = authenticate(&state, &headers).await?;
    checked_lab(&lab)?;
    let pdf = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.split(';').next() == Some("application/pdf"));
    if !pdf {
        return Err(ApiError::invalid(
            "library_not_pdf",
            "The file must be a PDF",
        ));
    }
    // Membership first, so a non-member learns nothing about the lab's setup.
    state.store.library_resolve(actor, &lab, &[]).await?;
    let service = service(&state, &lab)?;
    let guard = service
        .guard()
        .ok_or_else(|| library_error(LibraryError::GuardNotConfigured))?;
    let Some(length) = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
    else {
        return Err(library_error(LibraryError::LengthRequired));
    };
    if length > MAX_PAPER_BYTES {
        return Err(library_error(LibraryError::TooLarge));
    }
    let _permit = tokio::time::timeout(
        PERMIT_WAIT,
        state
            .library_permits
            .acquire_many(upload_weight_kib(length)),
    )
    .await
    .map_err(|_| library_error(LibraryError::Busy))?
    .map_err(|_| ApiError::internal())?;
    let body = tokio::time::timeout(BODY_WAIT, read_paper(body, length))
        .await
        .map_err(|_| library_error(LibraryError::Incomplete))?
        .map_err(library_error)?;
    if let Err(error) = check_pdf(&body) {
        return Err(if error.to_string() == "library_paper_too_large" {
            library_error(LibraryError::TooLarge)
        } else {
            ApiError::invalid("library_not_pdf", "The file must be a PDF")
        });
    }
    let sha256 = hex::encode(Sha256::digest(&body));
    let byte_size = body.len() as i64;
    let found = state.store.library_find(actor, &lab, &sha256).await?;
    let record = match found {
        Some((paper, document_id)) => {
            if known(&paper).map_err(library_error)? == Known::Present {
                return Ok(
                    Json(json!({"paper": paper, "created": false, "restored": false}))
                        .into_response(),
                );
            }
            LibraryPaperRecord {
                document_id,
                source_sha256: sha256.clone(),
                byte_size,
                title: None,
                authors: json!([]),
                year: None,
                doi: None,
                page_count: None,
                parse: json!({}),
            }
        }
        None => {
            // The original goes to the guard and is not kept: only its digest and size stay.
            let person = actor.to_string();
            let upload = Upload {
                media_type: "application/pdf",
                lab: &lab,
                person: &person,
                project: None,
                file_name: None,
            };
            let verdict = guard
                .check(&upload, body)
                .await
                .map_err(|e| library_error(e.into()))?;
            if verdict.outcome != GuardOutcome::Allowed {
                return Ok(guard_refusal(&verdict));
            }
            let rebuilt = guard
                .rebuilt(&verdict, MAX_PAPER_BYTES as u64)
                .await
                .map_err(|e| library_error(e.into()))?;
            let rebuilt_sha256 = hex::encode(Sha256::digest(&rebuilt));
            let got = service
                .ingest(&lab, rebuilt, &rebuilt_sha256)
                .await
                .map_err(library_error)?;
            let mut parse = got.parse;
            parse["guard"] = json!({"intake": verdict.intake, "policy": verdict.policy,
                "rebuilt_sha256": rebuilt_sha256, "findings": verdict.findings});
            LibraryPaperRecord {
                document_id: got.document_id,
                source_sha256: sha256.clone(),
                byte_size,
                title: got.title,
                authors: json!(got.authors),
                year: got.year,
                doi: got.doi,
                page_count: got.page_count,
                parse,
            }
        }
    };
    let value = state
        .store
        .library_record(actor, &lab, &record, query.request_id)
        .await?;
    let status = if value["created"] == true {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(value)).into_response())
}

/// One paper with its figure captions and counts from the parse.
pub(super) async fn paper(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath((lab, paper)): AxumPath<(String, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    checked_lab(&lab)?;
    let (paper, document_id) = state.store.library_paper(actor, &lab, paper).await?;
    let (outline, outline_error) = if paper["removed"].is_null() {
        match service(&state, &lab) {
            Ok(service) => match service.outline(&lab, document_id).await {
                Ok(outline) => (outline, Value::Null),
                Err(error) => (Value::Null, json!(error.code())),
            },
            Err(_) => (Value::Null, json!(LibraryError::NotConfigured.code())),
        }
    } else {
        (Value::Null, Value::Null)
    };
    Ok(Json(
        json!({"paper": paper, "outline": outline, "outline_error": outline_error}),
    ))
}

/// Word search over the lab's papers still in its library: each passage with its paper, section and pages.
pub(super) async fn search(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(lab): AxumPath<String>,
    Query(query): Query<SearchQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    checked_lab(&lab)?;
    let q = query.q.trim();
    if !(2..=200).contains(&q.chars().count()) || q.chars().any(char::is_control) {
        return Err(ApiError::invalid(
            "library_query_invalid",
            "A search needs 2 to 200 characters",
        ));
    }
    // Membership first: an empty resolve answers lab_not_found to a non-member.
    state.store.library_resolve(actor, &lab, &[]).await?;
    let service = service(&state, &lab)?;
    let passages = service.search(&lab, q).await.map_err(library_error)?;
    let mut documents: Vec<Uuid> = passages.iter().map(|p| p.document_id).collect();
    documents.sort();
    documents.dedup();
    let resolved = state.store.library_resolve(actor, &lab, &documents).await?;
    let papers: BTreeMap<String, &Value> = resolved
        .as_array()
        .map(|all| {
            all.iter()
                .filter_map(|p| Some((p["document_id"].as_str()?.to_owned(), p)))
                .collect()
        })
        .unwrap_or_default();
    let mut left_out = 0;
    let results: Vec<Value> = passages
        .into_iter()
        .filter_map(|passage| {
            let Some(paper) = papers.get(&passage.document_id.to_string()) else {
                left_out += 1;
                return None;
            };
            Some(json!({
                "paper_id": paper["paper_id"], "title": paper["title"], "year": paper["year"],
                "authors": paper["authors"].as_array().map(|a| a.iter().take(3).cloned().collect::<Vec<_>>()),
                "section": passage.section, "heading_path": passage.heading_path,
                "pages": passage.pages, "kind": passage.kind, "text": passage.text,
            }))
        })
        .collect();
    Ok(Json(json!({
        "query": q,
        "ranking": "words",
        "results": results,
        "left_out": left_out,
    })))
}

pub(super) async fn remove(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath((lab, paper)): AxumPath<(String, Uuid)>,
    Json(request): Json<RemoveRequest>,
) -> Result<impl IntoResponse, ApiError> {
    require_mutation_intent(&headers)?;
    let actor = authenticate(&state, &headers).await?;
    checked_lab(&lab)?;
    let paper = state
        .store
        .library_remove(actor, &lab, paper, &request.reason, request.request_id)
        .await?;
    Ok(Json(json!({ "paper": paper })))
}

/// A lab curator accepts a paper for the research lead, or withdraws that acceptance with a reason.
pub(super) async fn accept(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath((lab, paper)): AxumPath<(String, Uuid)>,
    Json(request): Json<AcceptRequest>,
) -> Result<impl IntoResponse, ApiError> {
    require_mutation_intent(&headers)?;
    let actor = authenticate(&state, &headers).await?;
    checked_lab(&lab)?;
    let paper = state
        .store
        .library_accept(
            actor,
            &lab,
            paper,
            request.accept,
            request.reason.as_deref(),
            request.request_id,
        )
        .await?;
    Ok(Json(json!({ "paper": paper })))
}

pub(super) async fn label(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath((lab, paper)): AxumPath<(String, Uuid)>,
    Json(request): Json<LabelRequest>,
) -> Result<impl IntoResponse, ApiError> {
    require_mutation_intent(&headers)?;
    let actor = authenticate(&state, &headers).await?;
    checked_lab(&lab)?;
    let paper = state
        .store
        .library_label(
            actor,
            &lab,
            paper,
            &request.kind,
            &request.namespace,
            &request.value,
            request.request_id,
        )
        .await?;
    Ok(Json(json!({ "paper": paper })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uploads_are_weighed_by_the_memory_they_can_hold() {
        let mib = |kib: u32| f64::from(kib) / 1024.0;
        assert_eq!(
            mib(upload_weight_kib(1024)),
            16.0,
            "a small upload takes the minimum"
        );
        let largest = upload_weight_kib(MAX_PAPER_BYTES);
        assert!((233.0..234.0).contains(&mib(largest)), "{}", mib(largest));
        assert!(
            largest as usize <= UPLOAD_BUDGET_KIB && 2 * largest as usize > UPLOAD_BUDGET_KIB,
            "one largest upload at a time"
        );
        assert!(
            (UPLOAD_BUDGET_KIB / upload_weight_kib(1) as usize) <= 24,
            "the minimum bounds how many run at once"
        );
    }

    #[tokio::test]
    async fn an_upload_is_read_only_to_its_limit_and_only_whole() {
        let chunks = |n: usize| {
            futures_util::stream::iter(
                (0..n).map(|_| Ok::<_, std::io::Error>(axum::body::Bytes::from(vec![b'%'; 1000]))),
            )
        };
        assert_eq!(
            read_paper(Body::from_stream(chunks(3)), 3000)
                .await
                .unwrap()
                .len(),
            3000
        );
        assert_eq!(
            read_paper(Body::from_stream(chunks(4)), 3000)
                .await
                .unwrap_err(),
            LibraryError::TooLarge
        );
        let broken = futures_util::stream::iter([
            Ok(axum::body::Bytes::from_static(b"%PDF-")),
            Err(std::io::Error::other("connection reset")),
        ]);
        assert_eq!(
            read_paper(Body::from_stream(broken), 3000)
                .await
                .unwrap_err(),
            LibraryError::Incomplete
        );
    }

    #[test]
    fn only_a_paper_the_guard_checked_is_returned_or_restored() {
        let evidence = json!({"intake": "0199b0f5-0000-7000-8000-000000000001",
            "rebuilt_sha256": "a".repeat(64), "policy": "library-paper-2026-10-05.2", "findings": []});
        assert_eq!(
            known(&json!({"parse": {"guard": evidence}, "removed": null})),
            Ok(Known::Present)
        );
        assert_eq!(
            known(&json!({"parse": {"guard": evidence}, "removed": {"reason": "duplicate"}})),
            Ok(Known::Removed)
        );
        for parse in [
            json!({}),
            json!({"guard": {"intake": "not-an-id", "rebuilt_sha256": "a".repeat(64)}}),
            json!({"guard": {"intake": "0199b0f5-0000-7000-8000-000000000001"}}),
        ] {
            assert_eq!(
                known(&json!({"parse": parse, "removed": {"reason": "duplicate"}})),
                Err(LibraryError::Unchecked),
                "a paper added before the guard is not restored"
            );
        }
    }
}
