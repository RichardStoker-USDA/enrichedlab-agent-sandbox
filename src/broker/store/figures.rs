//! Figure versions (migration 040). The worker keeps the render service's figure catalog and reads the
//! request of its current lease; a person asks for a version through the API; readers get a version's request with its
//! receipts and the versions made from one analysis.
use super::*;
use serde::Deserialize;
use serde_json::Value;
use time::OffsetDateTime;

/// A figure version's request as the worker receives it for its current lease, with the source package's place.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskFigure {
    pub source_task: Uuid,
    pub package_path: String,
    pub package_sha256: String,
    pub package_bytes: i64,
    pub table_path: String,
    pub table_sha256: String,
    pub table_rows: i64,
    pub feature: String,
    pub controls: Value,
    pub title: String,
    pub request_sha256: String,
    pub catalog_sha256: String,
}

/// What one attempt of a figure version produced. A completed receipt names the files and the package; any other names
/// why, in a fixed code and, when the render service refused the request, its own words.
#[derive(Clone, Debug)]
pub struct FigureReceipt {
    pub status: &'static str,
    pub error_code: Option<String>,
    pub message: Option<String>,
    pub service_version: Option<String>,
    pub image_digest: Option<String>,
    pub files: Option<Value>,
    pub notes: Value,
    pub package_sha256: Option<String>,
    pub started_at: OffsetDateTime,
    pub finished_at: OffsetDateTime,
}

/// A person's request for a figure version.
#[derive(Clone, Debug)]
pub struct FigureRequest<'a> {
    pub project: Uuid,
    pub source_task: Uuid,
    pub table_path: &'a str,
    pub feature: &'a str,
    pub controls: &'a Value,
    pub title: &'a str,
    pub previous: Option<Uuid>,
    pub request_id: Uuid,
}

fn record_invalid() -> StoreError {
    StoreError {
        kind: StoreErrorKind::Internal,
        code: "database_operation_failed",
    }
}

impl Store {
    /// Keep the render service's figure features as the worker read them; returns the catalog's digest.
    pub async fn record_render_catalog(
        &self,
        worker: &str,
        version: &str,
        image_digest: Option<&str>,
        features: &Value,
    ) -> StoreResult<String> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_record_render_catalog($1,$2,$3,$4,$5)",
                &[&self.tenant, &worker, &version, &image_digest, features],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// The catalog the worker read most recently, or null before its first read.
    pub async fn render_catalog(&self, actor: Uuid) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_render_catalog($1,$2)",
                &[&self.tenant, &actor],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get::<_, Option<Value>>(0).unwrap_or(Value::Null))
    }

    /// Record a person's request and create its task in one transaction; returns the task.
    pub async fn request_figure(
        &self,
        actor: Uuid,
        request: &FigureRequest<'_>,
    ) -> StoreResult<Uuid> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_request_figure($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
                &[
                    &self.tenant,
                    &actor,
                    &request.project,
                    &request.source_task,
                    &request.table_path,
                    &request.feature,
                    request.controls,
                    &request.title,
                    &request.previous,
                    &request.request_id,
                ],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// The request of the worker's current lease, or none when the task is not a figure version.
    pub async fn task_figure(
        &self,
        task: Uuid,
        worker: &str,
        attempt: i32,
        generation: i64,
    ) -> StoreResult<Option<TaskFigure>> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_task_figure($1,$2,$3,$4,$5)",
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

    pub async fn record_figure_receipt(
        &self,
        task: Uuid,
        worker: &str,
        attempt: i32,
        generation: i64,
        receipt: &FigureReceipt,
    ) -> StoreResult<()> {
        let c = self.pool.get().await.map_err(pool_error)?;
        c.query_one(
            "SELECT enrichedlab_agent.broker_record_figure_receipt($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)",
            &[
                &self.tenant,
                &task,
                &worker,
                &attempt,
                &generation,
                &receipt.status,
                &receipt.error_code,
                &receipt.message,
                &receipt.service_version,
                &receipt.image_digest,
                &receipt.files,
                &receipt.notes,
                &receipt.package_sha256,
                &receipt.started_at,
                &receipt.finished_at,
            ],
        )
        .await
        .map_err(database_error)?;
        Ok(())
    }

    /// A figure version's request with its source and receipts, for anyone who may read it; null for any other task.
    pub async fn task_figure_detail(&self, actor: Uuid, task: Uuid) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_task_figure_detail($1,$2,$3)",
                &[&self.tenant, &actor, &task],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get::<_, Option<Value>>(0).unwrap_or(Value::Null))
    }

    /// The figure versions made from one analysis that the reader may open, newest first.
    pub async fn figure_versions(&self, actor: Uuid, source_task: Uuid) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_figure_versions($1,$2,$3)",
                &[&self.tenant, &actor, &source_task],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }
}
