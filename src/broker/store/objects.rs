//! Lab objects (migration 083). The worker reads the objects request of its current lease and records one receipt per
//! attempt.
use super::*;
use serde::Deserialize;
use serde_json::Value;
use time::OffsetDateTime;

/// One step's objects request as the worker receives it for its current lease: the collection the lead named, the
/// bucket, the collection's key prefix, the copy generation and its manifest's SHA-256, and the files.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskObjects {
    pub profile: String,
    pub collection: Uuid,
    pub bucket: String,
    pub prefix: String,
    pub generation: String,
    pub manifest_sha256: String,
    pub files: Vec<String>,
    pub request_sha256: String,
}

/// What one attempt read and kept, or why it stopped.
pub struct ObjectReceipt {
    pub status: &'static str,
    pub error_code: Option<String>,
    pub files: Option<i32>,
    pub bytes: Option<i64>,
    pub tables: Option<Value>,
    pub package_sha256: Option<String>,
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
    /// A worker that serves lab objects reads its lease's request and records a receipt per attempt (migration 083).
    pub async fn ready_for_lab_objects(&self) -> StoreResult<()> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let row = client
            .query_one(
                "SELECT enrichedlab_agent.broker_schema_ready(83) AND COALESCE(has_function_privilege(current_user, \
                 to_regprocedure('enrichedlab_agent.broker_task_objects(text,uuid,text,integer,bigint)'), 'EXECUTE'), \
                 false) AND COALESCE(has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_record_object_receipt(text,uuid,text,integer,bigint,text,text,integer,bigint,\
                 jsonb,text,timestamptz,timestamptz)'), 'EXECUTE'), false)",
                &[],
            )
            .await
            .map_err(database_error)?;
        if row.get::<_, bool>(0) {
            Ok(())
        } else {
            Err(StoreError {
                kind: StoreErrorKind::Unavailable,
                code: "broker_lab_objects_schema_not_ready",
            })
        }
    }

    /// The run's lab objects methods (enabled, allowed, route `server-objects`), under the run's lease.
    pub async fn research_run_object_methods(
        &self,
        run: Uuid,
        worker: &str,
        generation: i64,
    ) -> StoreResult<Vec<String>> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_research_run_object_methods($1,$2,$3,$4)",
                &[&self.tenant, &run, &worker, &generation],
            )
            .await
            .map_err(database_error)?;
        serde_json::from_value(row.get::<_, Value>(0)).map_err(|_| record_invalid())
    }

    /// The objects request for the worker's current lease, or none when the task is not one.
    pub async fn task_objects(
        &self,
        task: Uuid,
        worker: &str,
        attempt: i32,
        generation: i64,
    ) -> StoreResult<Option<TaskObjects>> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_task_objects($1,$2,$3,$4,$5)",
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

    pub async fn record_object_receipt(
        &self,
        task: Uuid,
        worker: &str,
        attempt: i32,
        generation: i64,
        receipt: &ObjectReceipt,
    ) -> StoreResult<()> {
        let c = self.pool.get().await.map_err(pool_error)?;
        c.query_one(
            "SELECT enrichedlab_agent.broker_record_object_receipt($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
            &[
                &self.tenant,
                &task,
                &worker,
                &attempt,
                &generation,
                &receipt.status,
                &receipt.error_code,
                &receipt.files,
                &receipt.bytes,
                &receipt.tables,
                &receipt.package_sha256,
                &receipt.started_at,
                &receipt.finished_at,
            ],
        )
        .await
        .map_err(database_error)?;
        Ok(())
    }
}
