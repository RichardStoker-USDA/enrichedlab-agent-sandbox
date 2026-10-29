//! Result kinds (migration 055): the worker records the kind each researcher-visible artifact's bytes
//! hold under the broker's registry; the API serves those records to the artifact's readers.
use super::*;
use serde_json::Value;

/// An artifact the registry has not checked yet.
#[derive(Clone, Debug)]
pub struct AwaitingKind {
    pub task: Uuid,
    pub artifact: Uuid,
    pub relative_path: String,
    pub media_type: String,
    pub size_bytes: i64,
    pub sha256: String,
}

/// One check, as the worker records it.
pub struct KindRecord<'a> {
    pub artifact: Uuid,
    pub artifact_sha256: &'a str,
    /// The media type the bytes were checked under, as the artifact records it.
    pub media_type: &'a str,
    pub registry: &'a str,
    pub kind: (&'a str, u32),
    pub checked: (&'a str, u32),
    pub passed: bool,
    pub reason: Option<&'a str>,
    /// RFC 8785 canonical JSON.
    pub receipt: &'a str,
}

/// A recorded check as a task reader sees it in the artifact list.
#[derive(Clone, Debug)]
pub struct KindRecordView {
    pub artifact: Uuid,
    /// The bytes and media type the record was made for.
    pub artifact_sha256: String,
    pub media_type: String,
    pub kind: (String, i32),
    pub checked: (String, i32),
    pub state: String,
    pub reason: Option<String>,
    pub receipt_sha256: String,
}

impl Store {
    /// The worker needs migration 055 and its two kind functions before it checks artifacts; the API needs the two
    /// read functions before it serves kinds.
    pub async fn ready_for_kinds(&self, api: bool) -> StoreResult<()> {
        let functions: &[&str] = if api {
            &[
                "enrichedlab_agent.broker_task_artifact_kinds(text,uuid,uuid,text)",
                "enrichedlab_agent.broker_get_artifact_kind(text,uuid,uuid,uuid,text)",
            ]
        } else {
            &[
                "enrichedlab_agent.broker_artifacts_awaiting_kind(text,text,uuid[],integer)",
                "enrichedlab_agent.broker_record_artifact_kind(text,text,uuid,text,text,text,text,integer,text,integer,text,text,text)",
            ]
        };
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_schema_ready(55) AND bool_and(COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(f), 'EXECUTE'), false)) \
                 FROM unnest($1::text[]) AS f",
                &[&functions],
            )
            .await
            .map_err(database_error)?;
        if row.get::<_, Option<bool>>(0) == Some(true) {
            Ok(())
        } else {
            Err(StoreError {
                kind: StoreErrorKind::Unavailable,
                code: "broker_schema_not_ready",
            })
        }
    }

    pub async fn artifacts_awaiting_kind(
        &self,
        registry: &str,
        skip: &[Uuid],
        limit: i32,
    ) -> StoreResult<Vec<AwaitingKind>> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let rows = c
            .query(
                "SELECT * FROM enrichedlab_agent.broker_artifacts_awaiting_kind($1,$2,$3,$4)",
                &[&self.tenant, &registry, &skip, &limit],
            )
            .await
            .map_err(database_error)?;
        Ok(rows
            .into_iter()
            .map(|row| AwaitingKind {
                task: row.get("task_id"),
                artifact: row.get("artifact_id"),
                relative_path: row.get("relative_path"),
                media_type: row.get("media_type"),
                size_bytes: row.get("size_bytes"),
                sha256: row.get("sha256"),
            })
            .collect())
    }

    /// Records one check and returns its receipt's SHA-256. The same record again is accepted.
    pub async fn record_artifact_kind(
        &self,
        worker: &str,
        record: &KindRecord<'_>,
    ) -> StoreResult<String> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let state = if record.passed { "passed" } else { "failed" };
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_record_artifact_kind($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
                &[
                    &self.tenant,
                    &worker,
                    &record.artifact,
                    &record.artifact_sha256,
                    &record.media_type,
                    &record.registry,
                    &record.kind.0,
                    &(record.kind.1 as i32),
                    &record.checked.0,
                    &(record.checked.1 as i32),
                    &state,
                    &record.reason,
                    &record.receipt,
                ],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    pub async fn task_artifact_kinds(
        &self,
        principal: Uuid,
        task: Uuid,
        registry: &str,
    ) -> StoreResult<Vec<KindRecordView>> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let rows = c
            .query(
                "SELECT * FROM enrichedlab_agent.broker_task_artifact_kinds($1,$2,$3,$4)",
                &[&self.tenant, &principal, &task, &registry],
            )
            .await
            .map_err(database_error)?;
        Ok(rows
            .into_iter()
            .map(|row| KindRecordView {
                artifact: row.get("artifact_id"),
                artifact_sha256: row.get("artifact_sha256"),
                media_type: row.get("media_type"),
                kind: (row.get("kind_id"), row.get("kind_version")),
                checked: (row.get("checked_kind_id"), row.get("checked_kind_version")),
                state: row.get("state"),
                reason: row.get("reason"),
                receipt_sha256: row.get("receipt_sha256"),
            })
            .collect())
    }

    /// One artifact's record with its receipt. An artifact the principal cannot read is not found.
    pub async fn artifact_kind(
        &self,
        principal: Uuid,
        task: Uuid,
        artifact: Uuid,
        registry: &str,
    ) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        c.query_opt(
            "SELECT enrichedlab_agent.broker_get_artifact_kind($1,$2,$3,$4,$5)",
            &[&self.tenant, &principal, &task, &artifact, &registry],
        )
        .await
        .map_err(database_error)?
        .and_then(|row| row.get::<_, Option<Value>>(0))
        .ok_or(StoreError {
            kind: StoreErrorKind::NotFound,
            code: "artifact_not_found",
        })
    }
}
