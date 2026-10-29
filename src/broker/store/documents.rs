//! Answer documents (migration 072). A person asks for an Excel workbook, Word document or
//! PowerPoint slides of a research run's answer through the API; the worker reads the request of its current lease,
//! builds the file and records one receipt per attempt; readers get a document's request with its receipts and the
//! documents made from one run.
use super::*;
use serde::Deserialize;
use serde_json::Value;
use time::OffsetDateTime;

/// One result table a document pins: where its package is, its digests, and the table's shape as the digest listed it.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentSource {
    #[serde(rename = "ref")]
    pub reference: String,
    pub task: Uuid,
    pub artifact: Uuid,
    pub package_path: String,
    pub package_bytes: i64,
    pub package_sha256: String,
    pub table_path: String,
    pub table_sha256: String,
    pub rows: i64,
    pub columns: Vec<String>,
}

/// One R-rendered PNG figure a cited package holds, with its package's place.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentFigure {
    #[serde(rename = "ref")]
    pub reference: String,
    pub artifact: Uuid,
    pub package_path: String,
    pub package_bytes: i64,
    pub package_sha256: String,
    pub path: String,
    pub sha256: String,
}

/// A document's request as the worker receives it for its current lease. `proposal` holds the decision's answer,
/// summary, findings, answer tables and parts exactly as the turn stored them.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskDocument {
    pub kind: String,
    pub project: Uuid,
    pub project_title: String,
    pub run: Uuid,
    pub turn: i32,
    #[serde(with = "time::serde::rfc3339")]
    pub decided_at: OffsetDateTime,
    pub question: String,
    pub record_sha256: String,
    pub request_sha256: String,
    #[serde(with = "time::serde::rfc3339")]
    pub requested_at: OffsetDateTime,
    pub proposal: Value,
    pub source_count: usize,
    pub figure_count: usize,
    pub sources: Vec<DocumentSource>,
    pub figures: Vec<DocumentFigure>,
}

/// What one attempt of a document produced. A completed receipt names the file, the writer's counts and the package;
/// any other names why, in a fixed code with the broker's own words for it.
#[derive(Clone, Debug)]
pub struct DocumentReceipt {
    pub status: &'static str,
    pub error_code: Option<String>,
    pub message: Option<String>,
    pub service_version: Option<String>,
    pub image_digest: Option<String>,
    pub file: Option<Value>,
    pub counts: Option<Value>,
    pub notes: Value,
    pub package_sha256: Option<String>,
    pub started_at: OffsetDateTime,
    pub finished_at: OffsetDateTime,
}

/// A document package written to the artifact store and not yet registered: its path, size and SHA-256, its checksum
/// file's, and the digest built from it (`None` when it could not be built).
#[derive(Clone, Debug)]
pub struct RetainedPackage {
    pub package_path: String,
    pub package_bytes: i64,
    pub package_sha256: String,
    pub checksum_path: String,
    pub checksum_bytes: i64,
    pub checksum_sha256: String,
    pub digest_status: Option<String>,
    pub digest: Option<Value>,
}

fn record_invalid() -> StoreError {
    StoreError {
        kind: StoreErrorKind::Internal,
        code: "database_operation_failed",
    }
}

impl Store {
    /// Record a person's request for a document of a run's answer and create its task in one transaction; returns the
    /// task.
    pub async fn request_document(
        &self,
        actor: Uuid,
        project: Uuid,
        run: Uuid,
        kind: &str,
        request_id: Uuid,
    ) -> StoreResult<Uuid> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_request_document($1,$2,$3,$4,$5,$6)",
                &[&self.tenant, &actor, &project, &run, &kind, &request_id],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// The request of the worker's current lease, or none when the task is not a document or its record changed.
    pub async fn task_document(
        &self,
        task: Uuid,
        worker: &str,
        attempt: i32,
        generation: i64,
    ) -> StoreResult<Option<TaskDocument>> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_task_document($1,$2,$3,$4,$5)",
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

    pub async fn record_document_receipt(
        &self,
        task: Uuid,
        worker: &str,
        attempt: i32,
        generation: i64,
        receipt: &DocumentReceipt,
    ) -> StoreResult<()> {
        let c = self.pool.get().await.map_err(pool_error)?;
        c.query_one(
            "SELECT enrichedlab_agent.broker_record_document_receipt($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)",
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
                &receipt.file,
                &receipt.counts,
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

    /// Register a written document's package and checksum, record its digest and completed receipt and finish the task,
    /// in one transaction under the full lease fence (`broker_complete_document`).
    pub async fn complete_document(
        &self,
        task: Uuid,
        worker: &str,
        attempt: i32,
        generation: i64,
        receipt: &DocumentReceipt,
        retained: &RetainedPackage,
    ) -> StoreResult<()> {
        let c = self.pool.get().await.map_err(pool_error)?;
        c.query_one(
            "SELECT enrichedlab_agent.broker_complete_document($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20)",
            &[
                &self.tenant,
                &task,
                &worker,
                &attempt,
                &generation,
                &receipt.service_version,
                &receipt.image_digest,
                &receipt.file,
                &receipt.counts,
                &receipt.notes,
                &retained.package_path,
                &retained.package_bytes,
                &retained.package_sha256,
                &retained.checksum_path,
                &retained.checksum_bytes,
                &retained.checksum_sha256,
                &retained.digest_status,
                &retained.digest,
                &receipt.started_at,
                &receipt.finished_at,
            ],
        )
        .await
        .map_err(database_error)?;
        Ok(())
    }

    /// A document's request with its receipts, for anyone who may read it; null for any other task.
    pub async fn task_document_detail(&self, actor: Uuid, task: Uuid) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_task_document_detail($1,$2,$3)",
                &[&self.tenant, &actor, &task],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get::<_, Option<Value>>(0).unwrap_or(Value::Null))
    }

    /// The documents made from one run that the reader may open, newest first.
    pub async fn run_documents(&self, actor: Uuid, run: Uuid) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_run_documents($1,$2,$3)",
                &[&self.tenant, &actor, &run],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }
}
