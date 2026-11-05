//! A figure version's browser explorer (decision 103, steps 3 and 4). The worker keeps the Shinylive app files the
//! render service wrote under `explorer/` in the version's package; these routes serve them on the workspace's origin
//! under the same read access as the package itself. Shinylive's runtime, its service worker
//! (`/explorer/shinylive-sw.js`) and the webR packages come from the cluster's webR mirror on the same origin, so the
//! page loads scripts, workers and data from that origin only. The browser draws previews; the R figure stays the
//! master.
use super::*;
use crate::broker::evidence_package::{PackageError, read_member};
use crate::broker::figure_run::EXPLORER_FILES;
use axum::http::HeaderName;
use serde_json::Value;

/// The response policy every explorer route sends: scripts, workers, connections and frames from this origin only
/// (render-service `PAGE_CONTENT_SECURITY_POLICY`); webR needs `wasm-unsafe-eval` and `unsafe-eval`.
const POLICY: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval' 'unsafe-eval'; worker-src 'self' blob:; \
connect-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; frame-src 'self'; \
object-src 'none'; base-uri 'none'; frame-ancestors 'self'";
/// The most bytes one explorer file may hold (the render service's budget for all of them).
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// The page: Shinylive's styles and service-worker loader from the mirror, the worker's folder named so it controls
/// `/explorer/**`, and `page.js`, which starts the app from `app.json` beside the page.
const PAGE: &str = r#"<!doctype html>
<html lang="en">
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="shinylive:serviceworker_dir" content="/explorer">
<title>Figure explorer</title>
<link rel="stylesheet" href="/webr-mirror/shinylive/shinylive/style-resets.css">
<link rel="stylesheet" href="/webr-mirror/shinylive/shinylive/shinylive.css">
<script src="/webr-mirror/shinylive/shinylive/load-shinylive-sw.js" type="module"></script>
<script src="/explorer/page.js" type="module"></script>
<div style="height: 100vh; width: 100vw" id="root"></div>
</html>
"#;

/// Starts the explorer once the service worker controls the page; `load-shinylive-sw.js` registers the worker and
/// reloads the page the first time.
const PAGE_SCRIPT: &str = r#"import { runExportedApp } from "/webr-mirror/shinylive/shinylive/shinylive.js";

if (navigator.serviceWorker.controller) {
  runExportedApp({ id: "root", appEngine: "r", relPath: "" });
}
"#;

fn explorer_headers(media: &'static str, cache: &'static str) -> [(HeaderName, HeaderValue); 6] {
    [
        (header::CONTENT_TYPE, HeaderValue::from_static(media)),
        (
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(POLICY),
        ),
        (
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ),
        (header::CACHE_CONTROL, HeaderValue::from_static(cache)),
        (
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ),
        (
            HeaderName::from_static("cross-origin-resource-policy"),
            HeaderValue::from_static("same-origin"),
        ),
    ]
}

fn not_found() -> ApiError {
    ApiError::not_found("explorer_not_found")
}

/// The version's package record and its verified listing, when the reader may open it and it holds an explorer.
async fn explorer_package(
    state: &ApiState,
    headers: &HeaderMap,
    task: Uuid,
    artifact: Uuid,
) -> Result<
    (
        ArtifactView,
        Arc<crate::broker::evidence_package::PackageListing>,
    ),
    ApiError,
> {
    let actor = authenticate(state, headers).await?;
    let record = state.store.get_artifact(actor, task, artifact).await?;
    if !review::is_package(&record) {
        return Err(not_found());
    }
    let listing = review::verified_listing(state, task, &record).await?;
    if EXPLORER_FILES
        .iter()
        .any(|name| listing.member(&format!("explorer/{name}")).is_none())
    {
        return Err(not_found());
    }
    Ok((record, listing))
}

/// `GET /explorer/{task}/{artifact}/`: the explorer page for a version's package the reader may open.
pub(super) async fn page(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath((task, artifact)): AxumPath<(Uuid, Uuid)>,
) -> Result<Response, ApiError> {
    explorer_package(&state, &headers, task, artifact).await?;
    Ok((
        explorer_headers("text/html; charset=utf-8", "no-store"),
        PAGE,
    )
        .into_response())
}

/// `GET /explorer/{task}/{artifact}`: the same page needs its trailing slash so `app.json` resolves beside it.
pub(super) async fn page_without_slash(
    AxumPath((task, artifact)): AxumPath<(Uuid, Uuid)>,
) -> Response {
    axum::response::Redirect::permanent(&format!("/explorer/{task}/{artifact}/")).into_response()
}

/// `GET /explorer/page.js`: the page's start script. It holds no data.
pub(super) async fn page_script() -> Response {
    (
        explorer_headers("text/javascript; charset=utf-8", "no-cache"),
        PAGE_SCRIPT,
    )
        .into_response()
}

/// `GET /explorer/{task}/{artifact}/app.json`: Shinylive's file list, read from the package's `explorer/` members,
/// each checked against the listing's SHA-256 as it is read.
pub(super) async fn app(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath((task, artifact)): AxumPath<(Uuid, Uuid)>,
) -> Result<Response, ApiError> {
    let (record, listing) = explorer_package(&state, &headers, task, artifact).await?;
    let file = resolve_artifact_path(&state.artifact_root, task, &record)?;
    let size = record.size_bytes as u64;
    let archive_sha = record.sha256.clone();
    let members: Vec<_> = EXPLORER_FILES
        .iter()
        .map(|name| {
            listing
                .member(&format!("explorer/{name}"))
                .cloned()
                .map(|m| ((*name).to_owned(), m))
        })
        .collect::<Option<_>>()
        .ok_or_else(not_found)?;
    let files = state
        .package_permits
        .run(move || -> Result<Vec<Value>, PackageError> {
            members
                .into_iter()
                .map(|(name, member)| {
                    let bytes = read_member(&file, size, &archive_sha, &member, MAX_FILE_BYTES)?;
                    let content = String::from_utf8(bytes).map_err(|_| PackageError::Integrity)?;
                    Ok(json!({"name": name, "content": content, "type": "text"}))
                })
                .collect()
        })
        .await?
        .map_err(review::package_error)?;
    let body = serde_json::to_vec(&files).map_err(|_| ApiError::internal())?;
    Ok((explorer_headers("application/json", "no-store"), body).into_response())
}
