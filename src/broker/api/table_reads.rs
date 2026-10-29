//! Pages and column summaries of a result table and byte ranges of stored files
//!. A table is a CSV or TSV member of a retained result package, or an artifact that is itself one. It is read
//! from verified bytes, parsed once per digest by `broker::tables`, and its values leave as the stored strings.
use super::*;
use crate::broker::evidence_package::{PackageError, normalize_member_path, read_member};
use crate::broker::tables::{self, MAX_PAGE_ROWS, MAX_TABLE_BYTES, Table, TableError};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PagesQuery {
    path: Option<String>,
    #[serde(default)]
    offset: i64,
    #[serde(default = "default_page_rows")]
    limit: i64,
    sort: Option<String>,
    order: Option<String>,
}

fn default_page_rows() -> i64 {
    100
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SummaryQuery {
    path: Option<String>,
}

/// A refusal from a table route. An unreadable table names its reason, such as the line whose field count differs.
pub(super) enum TableRefusal {
    Api(ApiError),
    Unreadable(String),
}

impl From<ApiError> for TableRefusal {
    fn from(error: ApiError) -> Self {
        Self::Api(error)
    }
}

impl From<StoreError> for TableRefusal {
    fn from(error: StoreError) -> Self {
        Self::Api(error.into())
    }
}

impl IntoResponse for TableRefusal {
    fn into_response(self) -> Response {
        match self {
            Self::Api(error) => error.into_response(),
            Self::Unreadable(message) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({"error": {"code": "table_unreadable", "message": message}})),
            )
                .into_response(),
        }
    }
}

fn table_error(error: TableError) -> TableRefusal {
    match error {
        TableError::Unreadable(message) => TableRefusal::Unreadable(message),
        TableError::NotATable => TableRefusal::Api(ApiError {
            status: StatusCode::UNSUPPORTED_MEDIA_TYPE,
            code: "not_a_table",
            message: "Only CSV and TSV files are read as tables",
        }),
        TableError::TooLarge => TableRefusal::Api(ApiError {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            code: "table_too_large",
            message: "This table is too large to page in the browser; download the original",
        }),
    }
}

/// The table an artifact names: a package member by `path`, or the artifact itself.
async fn load_table(
    state: &ApiState,
    task: Uuid,
    record: &ArtifactView,
    path: Option<&str>,
) -> Result<(Option<String>, Arc<Table>), TableRefusal> {
    let file = resolve_artifact_path(&state.artifact_root, task, record)?;
    let size = record.size_bytes as u64;
    if review::is_package(record) {
        let raw = path.ok_or(ApiError::invalid(
            "table_member_required",
            "Name the table inside the result package with path",
        ))?;
        let member_path = normalize_member_path(raw.as_bytes())
            .ok_or_else(|| ApiError::not_found("package_member_not_found"))?;
        let listing = review::verified_listing(state, task, record).await?;
        let member = listing
            .member(&member_path)
            .cloned()
            .ok_or_else(|| ApiError::not_found("package_member_not_found"))?;
        let delimiter = tables::delimiter_for(member.media_type)
            .ok_or_else(|| table_error(TableError::NotATable))?;
        if member.size_bytes > MAX_TABLE_BYTES {
            return Err(table_error(TableError::TooLarge));
        }
        if let Some(hit) = cached(state, &member.sha256, delimiter)? {
            return Ok((Some(member_path), hit));
        }
        let archive_sha = record.sha256.clone();
        let parsed = state
            .package_permits
            .run(move || -> Result<Result<Table, TableError>, PackageError> {
                match read_member(&file, size, &archive_sha, &member, MAX_TABLE_BYTES) {
                    Ok(bytes) => Ok(tables::parse(&bytes, delimiter, member.sha256.clone())),
                    Err(PackageError::TooLarge) => Ok(Err(TableError::TooLarge)),
                    Err(other) => Err(other),
                }
            })
            .await?
            .map_err(review::package_error)?
            .map_err(table_error)?;
        return Ok((Some(member_path), remember(state, parsed)?));
    }
    if path.is_some() {
        return Err(review::package_error(PackageError::NotAPackage).into());
    }
    let delimiter = tables::delimiter_for(&record.media_type)
        .ok_or_else(|| table_error(TableError::NotATable))?;
    if size > MAX_TABLE_BYTES {
        return Err(table_error(TableError::TooLarge));
    }
    if let Some(hit) = cached(state, &record.sha256, delimiter)? {
        return Ok((None, hit));
    }
    let sha = record.sha256.clone();
    let parsed = state
        .package_permits
        .run(move || {
            let bytes = read_verified(&file, size, &sha).map_err(|_| integrity_error())?;
            Ok::<_, ApiError>(tables::parse(&bytes, delimiter, sha))
        })
        .await??
        .map_err(table_error)?;
    Ok((None, remember(state, parsed)?))
}

fn integrity_error() -> ApiError {
    review::package_error(PackageError::Integrity)
}

/// A stored file's bytes, read once and checked against its recorded size and digest before any is parsed.
fn read_verified(path: &Path, size: u64, sha256: &str) -> std::io::Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    let mut bytes = Vec::with_capacity(size as usize);
    file.take(size + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != size || hex::encode(Sha256::digest(&bytes)) != sha256 {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    Ok(bytes)
}

fn cached(state: &ApiState, sha256: &str, delimiter: u8) -> Result<Option<Arc<Table>>, ApiError> {
    Ok(state
        .tables
        .lock()
        .map_err(|_| ApiError::internal())?
        .get(sha256, delimiter))
}

fn remember(state: &ApiState, table: Table) -> Result<Arc<Table>, ApiError> {
    let table = Arc::new(table);
    state
        .tables
        .lock()
        .map_err(|_| ApiError::internal())?
        .insert(table.clone());
    Ok(table)
}

/// The largest page or summary body the broker builds. A larger one is refused before it is built whole.
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

#[derive(Serialize)]
struct PageBody<'a> {
    path: Option<&'a str>,
    sha256: &'a str,
    columns: &'a [String],
    types: &'a [tables::ColumnType],
    rows: tables::Page<'a>,
    total: usize,
    offset: usize,
    limit: usize,
    sort: Option<SortBody<'a>>,
}

#[derive(Serialize)]
struct SortBody<'a> {
    column: &'a str,
    order: &'static str,
}

#[derive(Serialize)]
struct SummaryBody<'a> {
    path: Option<&'a str>,
    sha256: &'a str,
    columns: &'a [tables::ColumnSummary],
    receipt: &'a tables::Receipt,
}

/// A JSON body built off the request threads, a few at a time, within `MAX_BODY_BYTES`.
async fn bounded_body(
    state: &ApiState,
    build: impl FnOnce() -> Option<Vec<u8>> + Send + 'static,
    refusal: ApiError,
) -> Result<Response, TableRefusal> {
    let body = state.body_permits.run(build).await?.ok_or(refusal)?;
    Ok((
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )],
        body,
    )
        .into_response())
}

/// One page of a table's rows, in file order or sorted by one column.
pub(super) async fn artifact_pages(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath((task, artifact)): AxumPath<(Uuid, Uuid)>,
    Query(query): Query<PagesQuery>,
) -> Result<Response, TableRefusal> {
    if !(0..=i64::from(u32::MAX)).contains(&query.offset) {
        return Err(ApiError::invalid(
            "page_offset_invalid",
            "offset must be between 0 and 4294967295",
        )
        .into());
    }
    if !(1..=MAX_PAGE_ROWS as i64).contains(&query.limit) {
        return Err(
            ApiError::invalid("page_limit_invalid", "limit must be between 1 and 1000").into(),
        );
    }
    let descending = match (query.order.as_deref(), query.sort.is_some()) {
        (None, _) | (Some("asc"), true) => false,
        (Some("desc"), true) => true,
        _ => {
            return Err(ApiError::invalid(
                "page_order_invalid",
                "order is asc or desc, and only with sort",
            )
            .into());
        }
    };
    let actor = authenticate(&state, &headers).await?;
    let record = state.store.get_artifact(actor, task, artifact).await?;
    let (path, table) = load_table(&state, task, &record, query.path.as_deref()).await?;
    let sort = match &query.sort {
        None => None,
        Some(name) => Some((
            table
                .columns
                .iter()
                .position(|column| column == name)
                .ok_or(ApiError::invalid(
                    "sort_column_unknown",
                    "sort names no column of this table",
                ))?,
            descending,
        )),
    };
    // A first sort of a large table takes a moment and memory for its keys: it waits for a permit, runs off the request
    // threads, and is kept for later pages.
    if let Some((column, descending)) = sort
        && !table.has_order(column, descending)
    {
        let sorted = table.clone();
        state
            .package_permits
            .run(move || sorted.order(column, descending))
            .await?;
    }
    let (offset, limit) = (query.offset as usize, query.limit as usize);
    let too_large = ApiError {
        status: StatusCode::PAYLOAD_TOO_LARGE,
        code: "page_too_large",
        message: "This page is too large to send at once; ask for fewer rows",
    };
    bounded_body(
        &state,
        move || {
            let body = PageBody {
                path: path.as_deref(),
                sha256: &table.sha256,
                columns: &table.columns,
                types: &table.types,
                rows: table.page(offset, limit, sort),
                total: table.rows(),
                offset,
                limit,
                sort: sort.map(|(column, descending)| SortBody {
                    column: &table.columns[column],
                    order: if descending { "desc" } else { "asc" },
                }),
            };
            tables::bounded_json(&body, MAX_BODY_BYTES)
        },
        too_large,
    )
    .await
}

/// Every column's type, counts, stored extremes and distinct values, with the receipt that binds them to the bytes.
pub(super) async fn artifact_summary(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath((task, artifact)): AxumPath<(Uuid, Uuid)>,
    Query(query): Query<SummaryQuery>,
) -> Result<Response, TableRefusal> {
    let actor = authenticate(&state, &headers).await?;
    let record = state.store.get_artifact(actor, task, artifact).await?;
    let (path, table) = load_table(&state, task, &record, query.path.as_deref()).await?;
    if !table.summary_ready() {
        let summarized = table.clone();
        state
            .package_permits
            .run(move || {
                summarized.summary();
            })
            .await?;
    }
    let too_large = ApiError {
        status: StatusCode::PAYLOAD_TOO_LARGE,
        code: "summary_too_large",
        message: "This table's summary is too large to send; download the original",
    };
    bounded_body(
        &state,
        move || {
            let (columns, receipt) = table.summary();
            let body = SummaryBody {
                path: path.as_deref(),
                sha256: &table.sha256,
                columns,
                receipt,
            };
            tables::bounded_json(&body, MAX_BODY_BYTES)
        },
        too_large,
    )
    .await
}

/// One `Range: bytes=` request against a file of `size` bytes (RFC 9110). Anything the broker does not serve as a
/// single range (another unit, several ranges, bad syntax) gets the whole file, as the standard allows.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum RangeRequest {
    Whole,
    Part(u64, u64),
    Unsatisfiable,
}

pub(super) fn range_request(headers: &HeaderMap, size: u64) -> RangeRequest {
    let mut fields = headers.get_all(header::RANGE).iter();
    // Several Range fields are not combined: the request is served whole.
    let (Some(field), None) = (fields.next(), fields.next()) else {
        return RangeRequest::Whole;
    };
    let Some(spec) = field
        .to_str()
        .ok()
        .and_then(|value| value.trim().strip_prefix("bytes="))
    else {
        return RangeRequest::Whole;
    };
    let Some((first, last)) = spec.split_once('-').filter(|_| !spec.contains(',')) else {
        return RangeRequest::Whole;
    };
    let (first, last) = (first.trim(), last.trim());
    if first.is_empty() {
        return match position(last) {
            Some(0) => RangeRequest::Unsatisfiable,
            Some(_) if size == 0 => RangeRequest::Unsatisfiable,
            Some(suffix) => RangeRequest::Part(size.saturating_sub(suffix), size - 1),
            None => RangeRequest::Whole,
        };
    }
    let Some(start) = position(first) else {
        return RangeRequest::Whole;
    };
    // The whole range must be well formed before its start is compared with the size: a bad range is ignored.
    let end = match (last.is_empty(), position(last)) {
        (true, _) => None,
        (false, Some(end)) if end >= start => Some(end),
        (false, _) => return RangeRequest::Whole,
    };
    if start >= size {
        return RangeRequest::Unsatisfiable;
    }
    RangeRequest::Part(start, end.map_or(size - 1, |end| end.min(size - 1)))
}

/// A byte position: digits only, which Rust's parser alone would also take with a leading `+`.
fn position(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// 416 with the size the client should have asked within.
pub(super) fn range_not_satisfiable(size: u64) -> Result<Response, ApiError> {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
    response.headers_mut().insert(
        header::CONTENT_RANGE,
        HeaderValue::from_str(&format!("bytes */{size}")).map_err(|_| ApiError::internal())?,
    );
    response
        .headers_mut()
        .insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    Ok(response)
}

/// Marks a response as partial content for `first..=last` of `size` bytes.
pub(super) fn mark_partial(
    response: &mut Response,
    first: u64,
    last: u64,
    size: u64,
) -> Result<(), ApiError> {
    *response.status_mut() = StatusCode::PARTIAL_CONTENT;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_RANGE,
        HeaderValue::from_str(&format!("bytes {first}-{last}/{size}"))
            .map_err(|_| ApiError::internal())?,
    );
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&(last - first + 1).to_string()).map_err(|_| ApiError::internal())?,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{api, person, serve, sha, tar_gz};
    use super::*;

    fn with(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::RANGE, HeaderValue::from_str(value).unwrap());
        headers
    }

    #[test]
    fn ranges_follow_rfc_9110_for_one_byte_range() {
        use RangeRequest::*;
        assert_eq!(range_request(&HeaderMap::new(), 10), Whole);
        for (value, want) in [
            ("bytes=0-3", Part(0, 3)),
            ("bytes=4-", Part(4, 9)),
            ("bytes=-3", Part(7, 9)),
            ("bytes=-30", Part(0, 9)),
            ("bytes=2-99", Part(2, 9)),
            ("bytes= 1 - 2", Part(1, 2)),
            ("bytes=10-", Unsatisfiable),
            ("bytes=-0", Unsatisfiable),
            ("bytes=3-1", Whole),
            ("bytes=0-1,4-5", Whole),
            ("items=0-1", Whole),
            ("bytes=a-b", Whole),
            ("bytes=", Whole),
            ("bytes=100-x", Whole),
            ("bytes=100-5", Whole),
            ("bytes=+1-2", Whole),
            ("bytes=1-+2", Whole),
            ("bytes=-+2", Whole),
            ("bytes=100-", Unsatisfiable),
            ("bytes=100-200", Unsatisfiable),
        ] {
            assert_eq!(range_request(&with(value), 10), want, "{value}");
        }
        let mut two = with("bytes=0-1");
        two.append(header::RANGE, HeaderValue::from_static("bytes=2-3"));
        assert_eq!(range_request(&two, 10), Whole, "two Range fields");
        assert_eq!(range_request(&with("bytes=-5"), 0), Unsatisfiable);
        assert_eq!(range_request(&with("bytes=0-0"), 0), Unsatisfiable);
    }

    #[tokio::test]
    #[ignore = "isolated PostgreSQL through qualify_remote_broker.sh table-reads"]
    async fn tables_page_and_summarize_from_verified_bytes_and_ranges_stay_inside_the_file() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dsn = std::fs::read_to_string(
            std::env::var_os("BROKER_QUALIFICATION_MIGRATION_DSN_FILE").unwrap(),
        )
        .unwrap();
        let config: tokio_postgres::Config = dsn.parse().unwrap();
        let (owner, connection) = config.connect(tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(connection);
        owner
            .batch_execute(
                "INSERT INTO enrichedlab_agent.broker_principal_invitations(tenant_id,email,group_key,role) \
                 VALUES ('research-site','table-outsider@example.invalid','pilot-lab','researcher');",
            )
            .await
            .unwrap();
        let dsn = dsn.replace("user=postgres", "user=broker_api_qual");
        let root = tempfile::tempdir().unwrap();
        let me = person("table-reader", "researcher@example.invalid");
        let state = api(&dsn, me.clone(), root.path());
        let actor = state.store.resolve_principal(&me).await.unwrap();
        let outsider = person("table-outsider", "table-outsider@example.invalid");
        let stranger = api(&dsn, outsider.clone(), root.path());
        stranger.store.resolve_principal(&outsider).await.unwrap();
        let task = state
            .store
            .submit_task(
                actor,
                "pilot-lab",
                "general-research",
                "Tabulate the dose groups.",
                "table-reads-1",
                &"e".repeat(64),
            )
            .await
            .unwrap();

        // Synthetic results: a table with a quoted line break, a ragged table, a figure and a second table.
        let table = b"supp,dose,len,note\nVC,0.5,4.2,\"low\ndose\"\nVC,2.0,23.6,high\nOJ,0.5,NA,missing\nOJ,1.0,22.7,\"\"\n";
        let ragged = b"a,b\n1,2\n3\n";
        let second = b"x\n3\n1\n2\n";
        // Two values of 5 MiB: one fits a page, both do not, and a summary names both.
        let long = [
            &b"v\n"[..],
            &[b'b'; 5 << 20],
            b"\n",
            &[b'a'; 5 << 20],
            b"\n",
        ]
        .concat();
        let package = tar_gz(&[
            ("results/table.csv", table),
            ("results/ragged.csv", ragged),
            ("results/figure.pdf", b"%PDF-1.4 synthetic"),
            ("results/second.csv", second),
            ("results/long.csv", &long),
        ]);
        let bare = b"group,value\nb,10\na,9\n";
        let directory = root.path().join(format!("tasks/{task}/outputs"));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("result.tar.gz"), &package).unwrap();
        std::fs::write(directory.join("bare.csv"), bare).unwrap();
        let mut artifacts = Vec::new();
        for (name, media, bytes) in [
            ("result.tar.gz", "application/gzip", &package[..]),
            ("bare.csv", "text/csv", &bare[..]),
        ] {
            let row = owner
                .query_one(
                    "INSERT INTO enrichedlab_agent.broker_artifacts(tenant_id,task_id,role,label,relative_path,media_type,size_bytes,sha256) \
                     VALUES ('research-site',$1,'result',$2,$3,$4,$5,$6) RETURNING artifact_id",
                    &[&task, &name, &format!("tasks/{task}/outputs/{name}"), &media, &(bytes.len() as i64), &sha(bytes)],
                )
                .await
                .unwrap();
            artifacts.push(row.get::<_, Uuid>(0));
        }
        let (packaged, bare_artifact) = (artifacts[0], artifacts[1]);
        let origin = serve(state.clone()).await;
        let elsewhere = serve(stranger).await;
        let client = reqwest::Client::new();
        let get = |base: &str, path: String| {
            client.get(format!("{base}/api/v1/tasks/{task}/artifacts/{path}"))
        };
        let code = |body: &serde_json::Value| {
            body["error"]["code"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        };

        // Pages in file order: stored strings, each row's index and starting line.
        let response = get(
            &origin,
            format!("{packaged}/pages?path=results/table.csv&limit=2"),
        )
        .send()
        .await
        .unwrap();
        assert_eq!(response.status(), 200);
        let page: serde_json::Value = response.json().await.unwrap();
        assert_eq!(page["columns"], json!(["supp", "dose", "len", "note"]));
        assert_eq!(
            page["types"],
            json!(["string", "number", "number", "string"])
        );
        assert_eq!(
            (page["total"].as_u64(), page["sha256"].as_str()),
            (Some(4), Some(sha(table).as_str()))
        );
        assert_eq!(
            page["rows"][0],
            json!({"index": 0, "line": 2, "values": ["VC", "0.5", "4.2", "low\ndose"]})
        );
        assert_eq!(page["rows"][1]["line"], 4);
        assert_eq!(page["path"], "results/table.csv");
        assert_eq!(page["sort"], serde_json::Value::Null);

        // Sorted by value, missing last, later pages from the same order.
        let sorted: serde_json::Value = get(
            &origin,
            format!("{packaged}/pages?path=results/table.csv&sort=len&order=desc&offset=1&limit=3"),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
        let lens: Vec<&str> = sorted["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["values"][2].as_str().unwrap())
            .collect();
        assert_eq!(lens, ["22.7", "4.2", "NA"]);
        assert_eq!(sorted["sort"], json!({"column": "len", "order": "desc"}));

        // The summary and its receipt.
        let summary: serde_json::Value = get(
            &origin,
            format!("{packaged}/summary?path=results/table.csv"),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
        assert_eq!(
            summary["columns"][2],
            json!({"name": "len", "type": "number", "type_source": "inferred",
            "count": 3, "missing": 1, "min": "4.2", "max": "23.6", "distinct": 3})
        );
        assert_eq!(summary["receipt"]["input_sha256"], sha(table));
        assert_eq!(summary["receipt"]["routine"], "enrichedlab.table-summary");
        // A client recomputes the receipt from the columns it received: keys sorted, no whitespace (RFC 8785).
        let sorted: Vec<std::collections::BTreeMap<String, serde_json::Value>> = summary["columns"]
            .as_array()
            .unwrap()
            .iter()
            .map(|column| column.as_object().unwrap().clone().into_iter().collect())
            .collect();
        assert_eq!(
            summary["receipt"]["output_sha256"],
            sha(&serde_json::to_vec(&sorted).unwrap())
        );

        // Refusals.
        for (path, status, want) in [
            (format!("{packaged}/pages"), 400, "table_member_required"),
            (
                format!("{packaged}/pages?path=results/figure.pdf"),
                415,
                "not_a_table",
            ),
            (
                format!("{packaged}/pages?path=results/absent.csv"),
                404,
                "package_member_not_found",
            ),
            (
                format!("{packaged}/pages?path=../x.csv"),
                404,
                "package_member_not_found",
            ),
            (
                format!("{packaged}/pages?path=results/table.csv&sort=height"),
                400,
                "sort_column_unknown",
            ),
            (
                format!("{packaged}/pages?path=results/table.csv&order=desc"),
                400,
                "page_order_invalid",
            ),
            (
                format!("{packaged}/pages?path=results/table.csv&order=asc"),
                400,
                "page_order_invalid",
            ),
            (
                format!("{packaged}/pages?path=results/long.csv&limit=2"),
                413,
                "page_too_large",
            ),
            (
                format!("{packaged}/summary?path=results/long.csv"),
                413,
                "summary_too_large",
            ),
            (
                format!("{packaged}/pages?path=results/table.csv&limit=0"),
                400,
                "page_limit_invalid",
            ),
            (
                format!("{packaged}/pages?path=results/table.csv&limit=1001"),
                400,
                "page_limit_invalid",
            ),
            (
                format!("{packaged}/pages?path=results/table.csv&offset=-1"),
                400,
                "page_offset_invalid",
            ),
            (
                format!("{bare_artifact}/pages?path=bare.csv"),
                415,
                "artifact_not_a_package",
            ),
        ] {
            let response = get(&origin, path.clone()).send().await.unwrap();
            assert_eq!(response.status(), status, "{path}");
            assert_eq!(code(&response.json().await.unwrap()), want, "{path}");
        }
        let unknown = get(
            &origin,
            format!("{packaged}/pages?path=results/table.csv&columns=1"),
        )
        .send()
        .await
        .unwrap();
        assert_eq!(unknown.status(), 400, "an unknown query field is refused");
        let response = get(
            &origin,
            format!("{packaged}/summary?path=results/ragged.csv"),
        )
        .send()
        .await
        .unwrap();
        assert_eq!(response.status(), 422);
        assert_eq!(
            response.json::<serde_json::Value>().await.unwrap()["error"],
            json!({"code": "table_unreadable", "message": "line 3 has 1 field; the header has 2"})
        );

        // A bare CSV artifact pages without a path.
        let bare_page: serde_json::Value =
            get(&origin, format!("{bare_artifact}/pages?sort=value"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
        assert_eq!(bare_page["path"], serde_json::Value::Null);
        assert_eq!(bare_page["rows"][0]["values"], json!(["a", "9"]));
        // One long value fits a page of its own.
        let response = get(
            &origin,
            format!("{packaged}/pages?path=results/long.csv&limit=1"),
        )
        .send()
        .await
        .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
        let one: serde_json::Value = response.json().await.unwrap();
        assert_eq!(one["rows"][0]["values"][0].as_str().unwrap().len(), 5 << 20);

        // Someone who may not read the task sees nothing.
        let response = get(
            &elsewhere,
            format!("{packaged}/pages?path=results/table.csv"),
        )
        .send()
        .await
        .unwrap();
        assert_eq!(response.status(), 404);
        let response = get(&elsewhere, format!("{packaged}/download"))
            .header(header::RANGE, "bytes=0-1")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 404);

        // Ranges of a member and of the stored file.
        let member = |range: &'static str| {
            get(
                &origin,
                format!("{packaged}/contents/file?path=results/table.csv"),
            )
            .header(header::RANGE, range)
        };
        let response = member("bytes=0-4").send().await.unwrap();
        assert_eq!(response.status(), 206);
        assert_eq!(
            response.headers()[header::CONTENT_RANGE],
            format!("bytes 0-4/{}", table.len()).as_str()
        );
        assert_eq!(response.headers()["x-artifact-sha256"], sha(table).as_str());
        assert_eq!(response.headers()[header::ACCEPT_RANGES], "bytes");
        assert_eq!(&response.bytes().await.unwrap()[..], &table[..5]);
        let tail = member("bytes=-6").send().await.unwrap();
        assert_eq!(tail.status(), 206);
        assert_eq!(&tail.bytes().await.unwrap()[..], &table[table.len() - 6..]);
        let beyond = member("bytes=9999-").send().await.unwrap();
        assert_eq!(beyond.status(), 416);
        assert_eq!(
            beyond.headers()[header::CONTENT_RANGE],
            format!("bytes */{}", table.len()).as_str()
        );
        let several = member("bytes=0-1,3-4").send().await.unwrap();
        assert_eq!(several.status(), 200, "several ranges get the whole member");
        assert_eq!(&several.bytes().await.unwrap()[..], &table[..]);
        let malformed = member("bytes=9999-x").send().await.unwrap();
        assert_eq!(
            malformed.status(),
            200,
            "a malformed range is ignored, even past the end"
        );
        assert_eq!(&malformed.bytes().await.unwrap()[..], &table[..]);
        let file = get(&origin, format!("{packaged}/download"))
            .header(header::RANGE, "bytes=2-9")
            .send()
            .await
            .unwrap();
        assert_eq!(file.status(), 206);
        assert_eq!(
            file.headers()[header::CONTENT_RANGE],
            format!("bytes 2-9/{}", package.len()).as_str()
        );
        assert_eq!(&file.bytes().await.unwrap()[..], &package[2..10]);
        let whole = get(&origin, format!("{packaged}/download"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            (
                whole.status().as_u16(),
                whole.headers()[header::ACCEPT_RANGES].to_str().unwrap()
            ),
            (200, "bytes")
        );
        assert_eq!(&whole.bytes().await.unwrap()[..], &package[..]);

        // Changed stored bytes: a table not yet read and every range read are refused; nothing comes from the change.
        let mut changed = package.clone();
        let last = changed.len() - 9;
        changed[last] ^= 0xff;
        std::fs::write(directory.join("result.tar.gz"), &changed).unwrap();
        for path in [
            format!("{packaged}/pages?path=results/second.csv"),
            format!("{packaged}/summary?path=results/second.csv"),
        ] {
            let response = get(&origin, path.clone()).send().await.unwrap();
            assert_eq!(response.status(), 409, "{path}");
            assert_eq!(
                code(&response.json().await.unwrap()),
                "artifact_integrity_failed"
            );
        }
        assert_eq!(member("bytes=0-4").send().await.unwrap().status(), 409);
        let response = get(&origin, format!("{packaged}/download"))
            .header(header::RANGE, "bytes=0-4")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 409);
    }
}
