//! Lab datasets (migration 059). The API checks an upload before the guard sees it and records the guard's
//! decision with the dataset and its intake task; a person asks for a preparation; the worker reads its lease's request
//! and records one receipt per attempt; readers get a project's datasets and one dataset in full.
use super::*;
use serde::Deserialize;
use serde_json::Value;
use time::OffsetDateTime;

/// A dataset's intake or preparation as the worker receives it for its current lease.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskDataset {
    pub operation: String,
    pub dataset: Uuid,
    pub file_name: String,
    pub media_type: String,
    pub guard_intake: Uuid,
    pub grid_sha256: String,
    pub grid_bytes: i64,
    /// The specification as PostgreSQL stores it (`spec::text`); `spec_sha256` is the digest of these bytes.
    pub spec_text: Option<String>,
    pub spec_sha256: Option<String>,
    pub title: String,
    pub request_sha256: String,
    pub intake_task: Uuid,
    /// The intake's package, for a preparation.
    pub intake_package: Option<IntakePackage>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntakePackage {
    pub path: String,
    pub bytes: i64,
    pub sha256: String,
}

/// The guard's decision on a person's file, with the file's facts, for the dataset record.
#[derive(Clone, Debug)]
pub struct DatasetRecord<'a> {
    pub project: Uuid,
    pub request_id: Uuid,
    pub file_name: &'a str,
    pub media_type: &'a str,
    pub size_bytes: i64,
    pub sha256: &'a str,
    pub guard_intake: Uuid,
    pub outcome: &'a str,
    pub findings: &'a Value,
    pub policy: &'a str,
    pub grid: Option<(&'a str, i64)>,
}

/// A person's preparation of a profiled dataset.
#[derive(Clone, Debug)]
pub struct PreparationRequest<'a> {
    pub project: Uuid,
    pub dataset: Uuid,
    pub spec: &'a Value,
    pub title: &'a str,
    pub previous: Option<Uuid>,
    pub request_id: Uuid,
}

/// What one attempt produced: a profile for an intake, a prepared table for a preparation, or why neither.
#[derive(Clone, Debug)]
pub struct DatasetReceipt {
    pub status: &'static str,
    pub error_code: Option<String>,
    pub profile: Option<Value>,
    pub summary: Option<Value>,
    pub grid_sha256: Option<String>,
    pub table: Option<(String, String, i64, Value)>,
    pub package_sha256: Option<String>,
    pub executor: &'static str,
    pub executor_version: i32,
    pub started_at: OffsetDateTime,
    pub finished_at: OffsetDateTime,
}

fn record_invalid() -> StoreError {
    StoreError {
        kind: StoreErrorKind::Internal,
        code: "database_operation_failed",
    }
}

impl Store {
    /// Migration 059 with the functions this role calls: the API's when `api`, the worker's otherwise.
    pub async fn ready_for_lab_datasets(&self, api: bool) -> StoreResult<()> {
        let functions: &[&str] = if api {
            &[
                "enrichedlab_agent.broker_dataset_upload_check(text,uuid,uuid,uuid)",
                "enrichedlab_agent.broker_record_dataset(text,uuid,uuid,uuid,text,text,bigint,text,uuid,text,jsonb,text,text,bigint)",
                "enrichedlab_agent.broker_request_dataset_preparation(text,uuid,uuid,uuid,jsonb,text,uuid,uuid)",
                "enrichedlab_agent.broker_project_datasets(text,uuid,uuid)",
                "enrichedlab_agent.broker_dataset_detail(text,uuid,uuid,uuid)",
                "enrichedlab_agent.broker_start_dataset_intake(text,uuid,uuid,uuid)",
            ]
        } else {
            &[
                "enrichedlab_agent.broker_task_dataset(text,uuid,text,integer,bigint)",
                "enrichedlab_agent.broker_record_dataset_receipt(text,uuid,text,integer,bigint,text,text,jsonb,jsonb,text,text,text,bigint,jsonb,text,text,integer,timestamptz,timestamptz)",
            ]
        };
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_schema_ready(59) AND bool_and(COALESCE(\
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

    /// Before the guard sees a file: the person may contribute and the lab has its dataset method. A request id already
    /// used in the project answers `{"existing": dataset}`.
    pub async fn dataset_upload_check(
        &self,
        actor: Uuid,
        project: Uuid,
        request_id: Uuid,
    ) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_dataset_upload_check($1,$2,$3,$4)",
                &[&self.tenant, &actor, &project, &request_id],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// Record the guard's decision with the dataset and, for an allowed file, its intake task; returns the dataset.
    pub async fn record_dataset(
        &self,
        actor: Uuid,
        record: &DatasetRecord<'_>,
    ) -> StoreResult<Value> {
        let (grid_sha, grid_bytes) = match record.grid {
            Some((sha, bytes)) => (Some(sha), Some(bytes)),
            None => (None, None),
        };
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_record_dataset($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)",
                &[
                    &self.tenant,
                    &actor,
                    &record.project,
                    &record.request_id,
                    &record.file_name,
                    &record.media_type,
                    &record.size_bytes,
                    &record.sha256,
                    &record.guard_intake,
                    &record.outcome,
                    record.findings,
                    &record.policy,
                    &grid_sha,
                    &grid_bytes,
                ],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// Record a person's preparation and create its task in one transaction; returns the task.
    pub async fn request_dataset_preparation(
        &self,
        actor: Uuid,
        request: &PreparationRequest<'_>,
    ) -> StoreResult<Uuid> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_request_dataset_preparation($1,$2,$3,$4,$5,$6,$7,$8)",
                &[
                    &self.tenant,
                    &actor,
                    &request.project,
                    &request.dataset,
                    request.spec,
                    &request.title,
                    &request.previous,
                    &request.request_id,
                ],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// The project's datasets with their intake and preparations, newest first.
    pub async fn project_datasets(&self, actor: Uuid, project: Uuid) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_project_datasets($1,$2,$3)",
                &[&self.tenant, &actor, &project],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// One dataset with the intake's receipts and each preparation's specification and receipts.
    pub async fn dataset_detail(
        &self,
        actor: Uuid,
        project: Uuid,
        dataset: Uuid,
    ) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_dataset_detail($1,$2,$3,$4)",
                &[&self.tenant, &actor, &project, &dataset],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// A person starts the intake of an allowed dataset that waited for room in the lab's queue; a dataset whose
    /// intake has started is returned as it is.
    pub async fn start_dataset_intake(
        &self,
        actor: Uuid,
        project: Uuid,
        dataset: Uuid,
    ) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_start_dataset_intake($1,$2,$3,$4)",
                &[&self.tenant, &actor, &project, &dataset],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// The request of the worker's current lease, or none when the task is not a dataset's.
    pub async fn task_dataset(
        &self,
        task: Uuid,
        worker: &str,
        attempt: i32,
        generation: i64,
    ) -> StoreResult<Option<TaskDataset>> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_task_dataset($1,$2,$3,$4,$5)",
                &[&self.tenant, &task, &worker, &attempt, &generation],
            )
            .await
            .map_err(database_error)?;
        let Some(value) = row.get::<_, Option<Value>>(0) else {
            return Ok(None);
        };
        serde_json::from_value(value)
            .map(Some)
            .map_err(|_| record_invalid())
    }

    pub async fn record_dataset_receipt(
        &self,
        task: Uuid,
        worker: &str,
        attempt: i32,
        generation: i64,
        receipt: &DatasetReceipt,
    ) -> StoreResult<()> {
        let (table_path, table_sha, table_rows, table_columns) = match &receipt.table {
            Some((path, sha, rows, columns)) => (
                Some(path.as_str()),
                Some(sha.as_str()),
                Some(*rows),
                Some(columns),
            ),
            None => (None, None, None, None),
        };
        let c = self.pool.get().await.map_err(pool_error)?;
        c.query_one(
            "SELECT enrichedlab_agent.broker_record_dataset_receipt($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19)",
            &[
                &self.tenant,
                &task,
                &worker,
                &attempt,
                &generation,
                &receipt.status,
                &receipt.error_code,
                &receipt.profile,
                &receipt.summary,
                &receipt.grid_sha256,
                &table_path,
                &table_sha,
                &table_rows,
                &table_columns,
                &receipt.package_sha256,
                &receipt.executor,
                &receipt.executor_version,
                &receipt.started_at,
                &receipt.finished_at,
            ],
        )
        .await
        .map_err(database_error)?;
        Ok(())
    }
}
