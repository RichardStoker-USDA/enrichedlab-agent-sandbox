//! Result kinds for readers: the registry the broker was built with, and what the worker recorded about
//! each artifact's bytes under it.
use std::collections::HashMap;

use serde_json::Value;

use super::*;
use crate::broker::{kinds, store::KindRecordView};

fn registry() -> Result<&'static kinds::Registry, ApiError> {
    kinds::registry().map_err(|_| ApiError::internal())
}

/// `GET /api/v1/kinds`. The ETag is the registry digest; a matching `If-None-Match` answers 304.
pub(super) async fn kinds_registry(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    authenticate(&state, &headers).await?;
    let registry = registry()?;
    let etag = format!("\"{}\"", registry.digest);
    let matched = headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(|tag| tag.trim().trim_start_matches("W/"))
        .any(|tag| tag == etag || tag == "*");
    let mut response = if matched {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        Json(registry.public()).into_response()
    };
    let headers = response.headers_mut();
    headers.insert(
        header::ETAG,
        HeaderValue::from_str(&etag).map_err(|_| ApiError::internal())?,
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-cache"),
    );
    Ok(response)
}

/// The fields an artifact item gains: the kind its bytes hold under the current registry, how the check ended, and
/// the method-declared same-values group and views, which no method declares yet.
pub(super) fn kind_fields(record: Option<&KindRecordView>) -> serde_json::Map<String, Value> {
    let fields = match record {
        None => json!({
            "kind": null,
            "validation": {"state": "not_checked", "receipt": null, "checked_as": null, "reason": null},
            "same_values_group": null,
            "views": [],
        }),
        Some(record) => json!({
            "kind": {"id": record.kind.0, "version": record.kind.1},
            "validation": {
                "state": record.state,
                "receipt": format!("sha256:{}", record.receipt_sha256),
                "checked_as": {"id": record.checked.0, "version": record.checked.1},
                "reason": record.reason,
            },
            "same_values_group": null,
            "views": [],
        }),
    };
    match fields {
        Value::Object(fields) => fields,
        _ => unreachable!("a JSON object literal"),
    }
}

/// The record that describes an artifact as listed: the artifact list and the records are separate reads, so a record
/// made for other bytes or another media type, such as before a retry registered the path again, is not attached.
fn describing<'a>(
    records: &HashMap<Uuid, &'a KindRecordView>,
    artifact: &crate::broker::model::ArtifactView,
) -> Option<&'a KindRecordView> {
    records.get(&artifact.id).copied().filter(|record| {
        record.artifact_sha256 == artifact.sha256 && record.media_type == artifact.media_type
    })
}

/// `GET /api/v1/tasks/{task}/artifacts` items with their kind fields, and the registry they refer to.
pub(super) async fn list_with_kinds(
    state: &ApiState,
    principal: Uuid,
    task: Uuid,
    artifacts: Vec<crate::broker::model::ArtifactView>,
) -> Result<Value, ApiError> {
    let registry = registry()?;
    let records = state
        .store
        .task_artifact_kinds(principal, task, &registry.digest)
        .await
        .map_err(ApiError::from)?;
    let by_artifact: HashMap<Uuid, &KindRecordView> = records
        .iter()
        .map(|record| (record.artifact, record))
        .collect();
    let mut items = Vec::with_capacity(artifacts.len());
    for artifact in artifacts {
        let fields = kind_fields(describing(&by_artifact, &artifact));
        let mut item = serde_json::to_value(&artifact).map_err(|_| ApiError::internal())?;
        item.as_object_mut()
            .ok_or_else(ApiError::internal)?
            .extend(fields);
        items.push(item);
    }
    Ok(json!({"artifacts": items, "registry": registry.digest}))
}

/// `GET /api/v1/tasks/{task}/artifacts/{artifact}/kind`: the item's kind fields with the full receipt.
pub(super) async fn artifact_kind(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath((task, artifact)): AxumPath<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    let principal = authenticate(&state, &headers).await?;
    let registry = registry()?;
    let record = state
        .store
        .artifact_kind(principal, task, artifact, &registry.digest)
        .await
        .map_err(ApiError::from)?;
    let text = |name: &str| record[name].as_str().map(str::to_owned);
    let view = match (
        text("kind_id"),
        record["kind_version"].as_i64(),
        text("checked_kind_id"),
        record["checked_kind_version"].as_i64(),
        text("receipt_sha256"),
    ) {
        (Some(kind), Some(version), Some(checked), Some(checked_version), Some(receipt_sha256)) => {
            Some(KindRecordView {
                artifact,
                artifact_sha256: text("artifact_sha256").unwrap_or_default(),
                media_type: text("media_type").unwrap_or_default(),
                kind: (kind, version as i32),
                checked: (checked, checked_version as i32),
                state: text("state").unwrap_or_default(),
                reason: text("reason"),
                receipt_sha256,
            })
        }
        _ => None,
    };
    let receipt = match record["receipt"].as_str() {
        Some(text) => Some(stored_receipt(text)?),
        None => None,
    };
    let mut answer = serde_json::Map::new();
    answer.insert("artifact_id".into(), json!(artifact));
    answer.insert("sha256".into(), record["artifact_sha256"].clone());
    answer.insert("registry".into(), json!(registry.digest));
    answer.extend(kind_fields(view.as_ref()));
    answer.insert("receipt".into(), json!(receipt));
    answer.insert("recorded_at".into(), record["recorded_at"].clone());
    Ok(Json(Value::Object(answer)))
}

/// A stored receipt as the answer carries it. Its digest is over the stored text, and a client recomputes it from the
/// receipt it receives in RFC 8785 canonical JSON, so a stored text that is not already canonical is refused rather than
/// shown with a digest no client could match.
fn stored_receipt(text: &str) -> Result<Value, ApiError> {
    let invalid = || ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        code: "artifact_kind_receipt_invalid",
        message: "The stored kind receipt is not in its canonical form",
    };
    let value: Value = serde_json::from_str(text).map_err(|_| invalid())?;
    if crate::broker::canonical::canonical_json(&value) != text.as_bytes() {
        tracing::warn!(event = "artifact_kind_receipt_not_canonical");
        return Err(invalid());
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{api, person, serve, sha, tar_gz};
    use super::*;
    use crate::broker::{canonical::canonical_json, kind_record, store::KindRecord};

    #[test]
    fn a_record_is_attached_only_to_the_bytes_and_media_type_it_describes() {
        let artifact = crate::broker::model::ArtifactView {
            id: Uuid::new_v4(),
            role: "result".into(),
            label: "table.csv".into(),
            relative_path: "tasks/t/outputs/table.csv".into(),
            media_type: "text/csv".into(),
            size_bytes: 4,
            sha256: "a".repeat(64),
            created_at: time::OffsetDateTime::UNIX_EPOCH,
        };
        let record = |sha256: &str, media_type: &str| KindRecordView {
            artifact: artifact.id,
            artifact_sha256: sha256.into(),
            media_type: media_type.into(),
            kind: ("table.delimited".into(), 1),
            checked: ("table.delimited".into(), 1),
            state: "passed".into(),
            reason: None,
            receipt_sha256: "b".repeat(64),
        };
        for (sha256, media_type, attached) in [
            ("a".repeat(64), "text/csv", true),
            ("c".repeat(64), "text/csv", false),
            ("a".repeat(64), "text/tab-separated-values", false),
        ] {
            let made = record(&sha256, media_type);
            let records = HashMap::from([(artifact.id, &made)]);
            assert_eq!(
                describing(&records, &artifact).is_some(),
                attached,
                "{media_type}"
            );
        }
    }

    #[tokio::test]
    #[ignore = "isolated PostgreSQL through qualify_remote_broker.sh result-kinds"]
    async fn the_worker_records_kinds_and_readers_see_them_with_receipts() {
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
                 VALUES ('research-site','kind-outsider@example.invalid','pilot-lab','researcher');",
            )
            .await
            .unwrap();
        let api_dsn = dsn.replace("user=postgres", "user=broker_api_qual");
        let worker_dsn = dsn.replace("user=postgres", "user=broker_worker_qual");
        let root = tempfile::tempdir().unwrap();
        let me = person("kind-reader", "researcher@example.invalid");
        let state = api(&api_dsn, me.clone(), root.path());
        let actor = state.store.resolve_principal(&me).await.unwrap();
        let outsider = person("kind-outsider", "kind-outsider@example.invalid");
        let stranger = api(&api_dsn, outsider.clone(), root.path());
        let outsider_id = stranger.store.resolve_principal(&outsider).await.unwrap();
        let worker = Store::connect(&worker_dsn, "research-site".into(), 2).unwrap();
        worker.ready_for_kinds(false).await.unwrap();
        state.store.ready_for_kinds(true).await.unwrap();
        assert!(
            state.store.ready_for_kinds(false).await.is_err(),
            "the API role may not record kinds"
        );
        let task = state
            .store
            .submit_task(
                actor,
                "pilot-lab",
                "general-research",
                "Check what each result holds.",
                "result-kinds-1",
                &"f".repeat(64),
            )
            .await
            .unwrap();

        // Synthetic results only.
        let package = tar_gz(&[
            ("results/table.csv", b"dose,len\n0.5,4.2\n1.0,11.5\n"),
            ("results/ragged.csv", b"a,b\n1,2\n3\n"),
            (
                "results/figure.svg",
                b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>",
            ),
            ("results/page.svg", b"<!DOCTYPE html><html></html>"),
            ("results/run.json", b"{\"seed\":7}"),
            ("results/blob.bin", b"\x00\x01\x02"),
        ]);
        let files: [(&str, &str, &[u8]); 5] = [
            ("result.tar.gz", "application/gzip", &package),
            ("bare.csv", "text/csv", b"group,value\nb,10\na,9\n"),
            ("ragged.csv", "text/csv", b"a,b\n1,2\n3\n"),
            (
                "notes.txt",
                "text/plain; charset=utf-8",
                b"Synthetic notes.\n",
            ),
            ("changed.csv", "text/csv", b"x\n1\n"),
        ];
        let directory = root.path().join(format!("tasks/{task}/outputs"));
        std::fs::create_dir_all(&directory).unwrap();
        let mut ids = HashMap::new();
        for (name, media, bytes) in files {
            std::fs::write(directory.join(name), bytes).unwrap();
            // changed.csv is recorded with other bytes of the same size: a custody fault, never a kind.
            let recorded = if name == "changed.csv" {
                sha(b"y\n2\n")
            } else {
                sha(bytes)
            };
            let row = owner
                .query_one(
                    "INSERT INTO enrichedlab_agent.broker_artifacts(tenant_id,task_id,role,label,relative_path,media_type,size_bytes,sha256) \
                     VALUES ('research-site',$1,'result',$2,$3,$4,$5,$6) RETURNING artifact_id",
                    &[&task, &name, &format!("tasks/{task}/outputs/{name}"), &media, &(bytes.len() as i64), &recorded],
                )
                .await
                .unwrap();
            ids.insert(name, row.get::<_, Uuid>(0));
        }
        let registry = kinds::registry().unwrap();
        let origin = serve(state.clone()).await;
        let elsewhere = serve(stranger.clone()).await;
        let client = reqwest::Client::new();
        let list = || async {
            let response = client
                .get(format!("{origin}/api/v1/tasks/{task}/artifacts"))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 200);
            let body: Value = response.json().await.unwrap();
            assert_eq!(body["registry"], registry.digest);
            body["artifacts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| (item["label"].as_str().unwrap().to_owned(), item.clone()))
                .collect::<HashMap<_, _>>()
        };

        // Nothing is checked yet.
        for item in list().await.values() {
            assert_eq!(item["kind"], Value::Null);
            assert_eq!(
                item["validation"],
                json!({"state": "not_checked", "receipt": null, "checked_as": null, "reason": null})
            );
            assert_eq!(
                (&item["same_values_group"], &item["views"]),
                (&Value::Null, &json!([]))
            );
        }

        // The registry, with its digest as the ETag.
        let response = client
            .get(format!("{origin}/api/v1/kinds"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let etag = response.headers()[header::ETAG]
            .to_str()
            .unwrap()
            .to_owned();
        assert_eq!(etag, format!("\"{}\"", registry.digest));
        let body: Value = response.json().await.unwrap();
        assert_eq!(&body, registry.public());
        for (tag, status) in [(etag.as_str(), 304), ("\"sha256:other\"", 200), ("*", 304)] {
            let response = client
                .get(format!("{origin}/api/v1/kinds"))
                .header(header::IF_NONE_MATCH, tag)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), status, "{tag}");
            assert_eq!(response.headers()[header::ETAG], etag.as_str());
        }

        // One recorder pass checks the five artifacts and records four; the changed file waits for a retry.
        let mut skip = HashMap::new();
        assert_eq!(
            kind_record::batch(&worker, root.path(), "kind-worker", registry, &mut skip)
                .await
                .unwrap(),
            (5, 4)
        );
        assert_eq!(skip.keys().collect::<Vec<_>>(), [&ids["changed.csv"]]);
        assert_eq!(
            kind_record::batch(&worker, root.path(), "kind-worker", registry, &mut skip)
                .await
                .unwrap(),
            (0, 0),
            "recorded bytes are not checked twice and a deferred artifact waits"
        );
        let items = list().await;
        let shape = |label: &str| {
            let item = &items[label];
            (
                item["kind"]["id"].as_str().unwrap_or("null").to_owned(),
                item["validation"]["state"].as_str().unwrap().to_owned(),
                item["validation"]["checked_as"]["id"]
                    .as_str()
                    .unwrap_or("null")
                    .to_owned(),
            )
        };
        let owned = |a: &str, b: &str, c: &str| (a.to_owned(), b.to_owned(), c.to_owned());
        assert_eq!(
            shape("result.tar.gz"),
            owned("package.result", "passed", "package.result")
        );
        assert_eq!(
            shape("bare.csv"),
            owned("table.delimited", "passed", "table.delimited")
        );
        assert_eq!(
            shape("ragged.csv"),
            owned("file.opaque", "failed", "table.delimited")
        );
        assert_eq!(
            shape("notes.txt"),
            owned("text.plain", "passed", "text.plain")
        );
        assert_eq!(shape("changed.csv"), owned("null", "not_checked", "null"));
        assert_eq!(
            items["ragged.csv"]["validation"]["reason"],
            "line 3 has 1 field; the header has 2"
        );

        // The full record: its receipt hashes to the digest the item names and binds the artifact's bytes.
        let response = client
            .get(format!(
                "{origin}/api/v1/tasks/{task}/artifacts/{}/kind",
                ids["ragged.csv"]
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let record: Value = response.json().await.unwrap();
        let receipt = &record["receipt"];
        assert_eq!(
            record["validation"]["receipt"],
            format!(
                "sha256:{}",
                hex::encode(Sha256::digest(canonical_json(receipt)))
            )
        );
        assert_eq!(receipt["input_sha256"], record["sha256"]);
        assert_eq!(receipt["registry"], registry.digest);
        assert_eq!(receipt["routine"], "enrichedlab.kind-check");
        assert_eq!(receipt["result"], "failed");
        assert_eq!(record["kind"], json!({"id": "file.opaque", "version": 1}));
        let unchecked: Value = client
            .get(format!(
                "{origin}/api/v1/tasks/{task}/artifacts/{}/kind",
                ids["changed.csv"]
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(
            (&unchecked["validation"]["state"], &unchecked["receipt"]),
            (&json!("not_checked"), &Value::Null)
        );

        // The same record again is accepted; another result for the same bytes, a receipt that disagrees with its
        // columns, and an edit are refused.
        let receipt_text = String::from_utf8(canonical_json(receipt)).unwrap();
        let artifact_sha = record["sha256"].as_str().unwrap().to_owned();
        let same = KindRecord {
            artifact: ids["ragged.csv"],
            artifact_sha256: &artifact_sha,
            media_type: "text/csv",
            registry: &registry.digest,
            kind: ("file.opaque", 1),
            checked: ("table.delimited", 1),
            passed: false,
            reason: Some("line 3 has 1 field; the header has 2"),
            receipt: &receipt_text,
        };
        assert!(
            worker
                .record_artifact_kind("kind-worker", &same)
                .await
                .is_ok()
        );
        let mut other = receipt.clone();
        other["checks"] = json!(["sha256"]);
        let other_text = String::from_utf8(canonical_json(&other)).unwrap();
        let refused = worker
            .record_artifact_kind(
                "kind-worker",
                &KindRecord {
                    receipt: &other_text,
                    ..same
                },
            )
            .await
            .unwrap_err();
        assert_eq!(refused.code, "artifact_kind_conflict");
        let refused = worker
            .record_artifact_kind(
                "kind-worker",
                &KindRecord {
                    passed: true,
                    kind: ("table.delimited", 1),
                    reason: None,
                    ..same
                },
            )
            .await
            .unwrap_err();
        assert_eq!(refused.code, "artifact_kind_receipt_invalid");
        let refused = worker
            .record_artifact_kind(
                "kind-worker",
                &KindRecord {
                    artifact: ids["bare.csv"],
                    ..same
                },
            )
            .await
            .unwrap_err();
        assert_eq!(refused.code, "artifact_kind_target_invalid");
        // A receipt whose result alone disagrees with a coherent row: passed, no reason, receipt says failed.
        let notes: Value = client
            .get(format!(
                "{origin}/api/v1/tasks/{task}/artifacts/{}/kind",
                ids["notes.txt"]
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let mut flipped = notes["receipt"].clone();
        flipped["result"] = json!("failed");
        let flipped_text = String::from_utf8(canonical_json(&flipped)).unwrap();
        let notes_sha = notes["sha256"].as_str().unwrap().to_owned();
        let refused = worker
            .record_artifact_kind(
                "kind-worker",
                &KindRecord {
                    artifact: ids["notes.txt"],
                    artifact_sha256: &notes_sha,
                    media_type: "text/plain; charset=utf-8",
                    registry: &registry.digest,
                    kind: ("text.plain", 1),
                    checked: ("text.plain", 1),
                    passed: true,
                    reason: None,
                    receipt: &flipped_text,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(refused.code, "artifact_kind_receipt_invalid");
        assert!(
            state
                .store
                .record_artifact_kind("kind-api", &same)
                .await
                .is_err(),
            "the API role cannot record"
        );
        assert!(
            owner
                .execute(
                    "UPDATE enrichedlab_agent.broker_artifact_kinds SET reason = 'edited' WHERE artifact_id = $1",
                    &[&ids["ragged.csv"]],
                )
                .await
                .is_err(),
            "records are immutable"
        );

        // A record names the media type the bytes were checked under, as its receipt does.
        assert_eq!(receipt["media_type"], "text/csv");
        let refused = worker
            .record_artifact_kind(
                "kind-worker",
                &KindRecord {
                    media_type: "text/tab-separated-values",
                    ..same
                },
            )
            .await
            .unwrap_err();
        assert_eq!(refused.code, "artifact_kind_target_invalid");
        let mut retyped = receipt.clone();
        retyped["media_type"] = json!("text/tab-separated-values");
        let retyped_text = String::from_utf8(canonical_json(&retyped)).unwrap();
        let refused = worker
            .record_artifact_kind(
                "kind-worker",
                &KindRecord {
                    receipt: &retyped_text,
                    ..same
                },
            )
            .await
            .unwrap_err();
        assert_eq!(refused.code, "artifact_kind_receipt_invalid");
        // The same bytes registered again under another media type are unchecked until they are checked as that type.
        owner
            .execute(
                "UPDATE enrichedlab_agent.broker_artifacts SET media_type = 'text/tab-separated-values' \
                 WHERE artifact_id = $1",
                &[&ids["bare.csv"]],
            )
            .await
            .unwrap();
        assert_eq!(
            list().await["bare.csv"]["validation"]["state"],
            "not_checked"
        );
        // Each read binds the media type itself, apart from the list's own check.
        assert!(
            !state
                .store
                .task_artifact_kinds(actor, task, &registry.digest)
                .await
                .unwrap()
                .iter()
                .any(|record| record.artifact == ids["bare.csv"])
        );
        let unchecked: Value = client
            .get(format!(
                "{origin}/api/v1/tasks/{task}/artifacts/{}/kind",
                ids["bare.csv"]
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(unchecked["validation"]["state"], "not_checked");
        assert_eq!(
            kind_record::batch(&worker, root.path(), "kind-worker", registry, &mut skip)
                .await
                .unwrap(),
            (1, 1)
        );
        let retyped: Value = client
            .get(format!(
                "{origin}/api/v1/tasks/{task}/artifacts/{}/kind",
                ids["bare.csv"]
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(
            retyped["receipt"]["media_type"],
            "text/tab-separated-values"
        );
        assert_eq!(retyped["validation"]["state"], "passed");

        // A receipt stored in any form but RFC 8785 canonical JSON is refused when read: no client could match its
        // digest from the receipt it receives.
        let spaced = b"Spaced notes.\n";
        let spaced_sha = sha(spaced);
        let spaced_path = directory.join("spaced.txt");
        std::fs::write(&spaced_path, spaced).unwrap();
        let spaced_id: Uuid = owner
            .query_one(
                "INSERT INTO enrichedlab_agent.broker_artifacts(tenant_id,task_id,role,label,relative_path,media_type,size_bytes,sha256) \
                 VALUES ('research-site',$1,'result','spaced.txt',$2,'text/plain',$3,$4) RETURNING artifact_id",
                &[&task, &format!("tasks/{task}/outputs/spaced.txt"), &(spaced.len() as i64), &spaced_sha],
            )
            .await
            .unwrap()
            .get(0);
        let checked = kinds::check_stored(
            registry,
            &spaced_path,
            "text/plain",
            spaced.len() as u64,
            &spaced_sha,
        )
        .unwrap();
        let pretty = serde_json::to_string_pretty(&checked.receipt).unwrap();
        worker
            .record_artifact_kind(
                "kind-worker",
                &KindRecord {
                    artifact: spaced_id,
                    artifact_sha256: &spaced_sha,
                    media_type: "text/plain",
                    registry: &registry.digest,
                    kind: ("text.plain", 1),
                    checked: ("text.plain", 1),
                    passed: true,
                    reason: None,
                    receipt: &pretty,
                },
            )
            .await
            .unwrap();
        let response = client
            .get(format!(
                "{origin}/api/v1/tasks/{task}/artifacts/{spaced_id}/kind"
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 500);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"]["code"],
            "artifact_kind_receipt_invalid"
        );
        // So is one in compact sorted form that writes a whole number as `1.0`: the database takes it as equal to 1,
        // but RFC 8785 writes the number `1`, so a client's digest would differ.
        let decimal = b"Decimal notes.\n";
        let decimal_sha = sha(decimal);
        let decimal_path = directory.join("decimal.txt");
        std::fs::write(&decimal_path, decimal).unwrap();
        let decimal_id: Uuid = owner
            .query_one(
                "INSERT INTO enrichedlab_agent.broker_artifacts(tenant_id,task_id,role,label,relative_path,media_type,size_bytes,sha256) \
                 VALUES ('research-site',$1,'result','decimal.txt',$2,'text/plain',$3,$4) RETURNING artifact_id",
                &[&task, &format!("tasks/{task}/outputs/decimal.txt"), &(decimal.len() as i64), &decimal_sha],
            )
            .await
            .unwrap()
            .get(0);
        let checked = kinds::check_stored(
            registry,
            &decimal_path,
            "text/plain",
            decimal.len() as u64,
            &decimal_sha,
        )
        .unwrap();
        let compact = String::from_utf8(canonical_json(&checked.receipt)).unwrap();
        let decimal_text = compact.replace("\"version\":1}", "\"version\":1.0}");
        assert_ne!(decimal_text, compact);
        worker
            .record_artifact_kind(
                "kind-worker",
                &KindRecord {
                    artifact: decimal_id,
                    artifact_sha256: &decimal_sha,
                    media_type: "text/plain",
                    registry: &registry.digest,
                    kind: ("text.plain", 1),
                    checked: ("text.plain", 1),
                    passed: true,
                    reason: None,
                    receipt: &decimal_text,
                },
            )
            .await
            .unwrap();
        let response = client
            .get(format!(
                "{origin}/api/v1/tasks/{task}/artifacts/{decimal_id}/kind"
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 500);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"]["code"],
            "artifact_kind_receipt_invalid"
        );

        // Package members carry their own kinds, checked in the listing pass.
        let response = client
            .get(format!(
                "{origin}/api/v1/tasks/{task}/artifacts/{}/contents",
                ids["result.tar.gz"]
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let contents: Value = response.json().await.unwrap();
        assert_eq!(
            contents["listing"]["kind_check"],
            json!({"routine": "enrichedlab.kind-check", "version": 1, "registry": registry.digest})
        );
        let members: HashMap<String, Value> = contents["listing"]["members"]
            .as_array()
            .unwrap()
            .iter()
            .map(|member| (member["path"].as_str().unwrap().to_owned(), member.clone()))
            .collect();
        for (path, kind, state, checked) in [
            (
                "results/table.csv",
                "table.delimited",
                "passed",
                "table.delimited",
            ),
            (
                "results/ragged.csv",
                "file.opaque",
                "failed",
                "table.delimited",
            ),
            ("results/figure.svg", "image.svg", "passed", "image.svg"),
            ("results/page.svg", "file.opaque", "failed", "image.svg"),
            ("results/run.json", "record.json", "passed", "record.json"),
            ("results/blob.bin", "file.opaque", "passed", "file.opaque"),
        ] {
            let member = &members[path];
            assert_eq!(member["result_kind"]["id"], kind, "{path}");
            assert_eq!(member["validation"]["state"], state, "{path}");
            assert_eq!(member["validation"]["checked_as"]["id"], checked, "{path}");
            assert_eq!(
                (&member["same_values_group"], &member["views"]),
                (&Value::Null, &json!([]))
            );
        }

        // Nothing for a person who may not read the task.
        assert!(
            stranger
                .store
                .task_artifact_kinds(outsider_id, task, &registry.digest)
                .await
                .unwrap()
                .is_empty()
        );
        for path in [
            format!("{elsewhere}/api/v1/tasks/{task}/artifacts"),
            format!(
                "{elsewhere}/api/v1/tasks/{task}/artifacts/{}/kind",
                ids["ragged.csv"]
            ),
        ] {
            assert_eq!(
                client.get(&path).send().await.unwrap().status(),
                404,
                "{path}"
            );
        }
    }

    #[tokio::test]
    #[ignore = "isolated PostgreSQL through qualify_remote_broker.sh figure-kinds"]
    async fn a_figure_version_declares_the_drawings_in_its_package_as_r_figures() {
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
                 VALUES ('research-site','figure-outsider@example.invalid','pilot-lab','researcher');",
            )
            .await
            .unwrap();
        let api_dsn = dsn.replace("user=postgres", "user=broker_api_qual");
        let root = tempfile::tempdir().unwrap();
        let me = person("figure-reader", "researcher@example.invalid");
        let state = api(&api_dsn, me.clone(), root.path());
        let actor = state.store.resolve_principal(&me).await.unwrap();
        let outsider = person("figure-outsider", "figure-outsider@example.invalid");
        let stranger = api(&api_dsn, outsider.clone(), root.path());
        stranger.store.resolve_principal(&outsider).await.unwrap();
        let submit = |key: &'static str| {
            let store = state.store.clone();
            async move {
                store
                    .submit_task(
                        actor,
                        "pilot-lab",
                        "general-research",
                        "Draw a figure from a result table.",
                        key,
                        &"e".repeat(64),
                    )
                    .await
                    .unwrap()
            }
        };
        let (source, version) = (
            submit("figure-kinds-source").await,
            submit("figure-kinds-version").await,
        );

        // Synthetic drawings from the kinds' own fixtures.
        let fixture = |path: &str| {
            std::fs::read(
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("kinds/fixtures")
                    .join(path),
            )
            .unwrap()
        };
        let (svg, png, pdf) = (
            fixture("image.svg/circle.svg"),
            fixture("image.raster/dot.png"),
            fixture("document.pdf/blank.pdf"),
        );
        let drawn = tar_gz(&[
            ("figures/figure.svg", &svg),
            ("figures/figure.png", &png),
            ("figures/figure.pdf", &pdf),
            ("figures/extra.svg", &svg),
            ("spec.json", b"{\"schema\":\"enrichedlab.figure-spec.v1\"}"),
        ]);
        let copied = tar_gz(&[("figures/figure.svg", &svg), ("notes.txt", b"A copy.\n")]);
        let mut ids = HashMap::new();
        for (task, name, media, bytes) in [
            (
                source,
                "result.tar.gz",
                "application/gzip",
                tar_gz(&[("results/table.csv", b"dose,len\n0.5,4.2\n")]),
            ),
            (version, "figure.tar.gz", "application/gzip", drawn.clone()),
            (version, "copy.tar.gz", "application/gzip", copied),
        ] {
            let directory = root.path().join(format!("tasks/{task}/outputs"));
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join(name), &bytes).unwrap();
            let row = owner
                .query_one(
                    "INSERT INTO enrichedlab_agent.broker_artifacts(tenant_id,task_id,role,label,relative_path,media_type,size_bytes,sha256) \
                     VALUES ('research-site',$1,'result',$2,$3,$4,$5,$6) RETURNING artifact_id",
                    &[&task, &name, &format!("tasks/{task}/outputs/{name}"), &media, &(bytes.len() as i64), &sha(&bytes)],
                )
                .await
                .unwrap();
            ids.insert(name, row.get::<_, Uuid>(0));
        }

        // The figure path's record: a refused attempt names nothing; the completed one lists the drawings of its own
        // package, and its PDF entry carries another digest, so that file is not declared.
        let catalog = "c".repeat(64);
        owner
            .execute(
                "INSERT INTO enrichedlab_agent.broker_render_catalogs(tenant_id,catalog_sha256,service_version,features) \
                 VALUES ('research-site',$1,'0.5.22','[{\"id\":\"scatter\"}]')",
                &[&catalog],
            )
            .await
            .unwrap();
        owner
            .execute(
                "INSERT INTO enrichedlab_agent.broker_figure_requests(tenant_id,task_id,project_id,source_task_id,\
                 source_artifact_id,source_package_sha256,table_path,table_sha256,table_rows,table_columns,feature,\
                 controls,title,catalog_sha256,request,request_sha256,principal_id) \
                 VALUES ('research-site',$1,gen_random_uuid(),$2,$3,$4,'results/table.csv',$4,1,'[\"dose\",\"len\"]',\
                 'scatter','{}','Length by dose',$5,'{}',$4,$6)",
                &[&version, &source, &ids["result.tar.gz"], &"a".repeat(64), &catalog, &actor],
            )
            .await
            .unwrap();
        let listed = |path: &str, bytes: &[u8]| json!({"path": path, "sha256": sha(bytes), "size_bytes": bytes.len()});
        let files = json!([
            listed("figures/figure.svg", &svg),
            listed("figures/figure.png", &png),
            {"path": "figures/figure.pdf", "sha256": "0".repeat(64), "size_bytes": pdf.len()},
        ]);
        for (attempt, status, error, files, package) in [
            (1, "refused", Some("figure_feature_unknown"), None, None),
            (2, "completed", None, Some(files), Some(sha(&drawn))),
        ] {
            owner
                .execute(
                    "INSERT INTO enrichedlab_agent.broker_figure_receipts(tenant_id,task_id,attempt,lease_generation,\
                     status,error_code,service_version,files,notes,package_sha256,started_at,finished_at) \
                     VALUES ('research-site',$1,$2,1,$3,$4,'0.5.22',$5,'[]',$6,now(),now())",
                    &[&version, &attempt, &status, &error, &files, &package],
                )
                .await
                .unwrap();
        }

        let origin = serve(state.clone()).await;
        let elsewhere = serve(stranger).await;
        let client = reqwest::Client::new();
        let members = |task: Uuid, artifact: Uuid| {
            let client = client.clone();
            let origin = origin.clone();
            async move {
                let response = client
                    .get(format!(
                        "{origin}/api/v1/tasks/{task}/artifacts/{artifact}/contents"
                    ))
                    .send()
                    .await
                    .unwrap();
                assert_eq!(response.status(), 200);
                let contents: Value = response.json().await.unwrap();
                contents["listing"]["members"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|member| {
                        let text = |key: &str| member[key]["id"].as_str().unwrap().to_owned();
                        (
                            member["path"].as_str().unwrap().to_owned(),
                            (
                                text("result_kind"),
                                member["validation"]["checked_as"]["id"]
                                    .as_str()
                                    .unwrap()
                                    .to_owned(),
                            ),
                        )
                    })
                    .collect::<HashMap<_, _>>()
            }
        };
        let pair = |kind: &str, checked: &str| (kind.to_owned(), checked.to_owned());
        // Twice: a declaration never changes the cached listing other readers get.
        for _ in 0..2 {
            let drawn = members(version, ids["figure.tar.gz"]).await;
            assert_eq!(drawn["figures/figure.svg"], pair("figure.r", "image.svg"));
            assert_eq!(
                drawn["figures/figure.png"],
                pair("figure.r", "image.raster")
            );
            assert_eq!(
                drawn["figures/figure.pdf"],
                pair("document.pdf", "document.pdf"),
                "another digest"
            );
            assert_eq!(
                drawn["figures/extra.svg"],
                pair("image.svg", "image.svg"),
                "not listed"
            );
            assert_eq!(drawn["spec.json"], pair("record.json", "record.json"));
            let copy = members(version, ids["copy.tar.gz"]).await;
            assert_eq!(
                copy["figures/figure.svg"],
                pair("image.svg", "image.svg"),
                "another package"
            );
        }
        let analysis = members(source, ids["result.tar.gz"]).await;
        assert_eq!(
            analysis["results/table.csv"],
            pair("table.delimited", "table.delimited")
        );

        // Nothing for a person who may not read the version.
        let response = client
            .get(format!(
                "{elsewhere}/api/v1/tasks/{version}/artifacts/{}/contents",
                ids["figure.tar.gz"]
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 404);
    }
}
