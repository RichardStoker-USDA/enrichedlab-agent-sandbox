//! Template analyses (migration 058). The worker keeps the render service's analysis catalog and reads the
//! request of its current lease; a person asks for an analysis through the API; readers get an analysis's request with
//! its receipts and the analyses made from one analysis.
use super::*;
use serde::Deserialize;
use serde_json::Value;
use time::OffsetDateTime;

/// A template analysis's request as the worker receives it for its current lease, with the source package's place.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskAnalysis {
    pub source_task: Uuid,
    pub package_path: String,
    pub package_sha256: String,
    pub package_bytes: i64,
    pub table_path: String,
    pub table_sha256: String,
    pub table_rows: i64,
    pub feature: String,
    pub settings: Value,
    pub title: String,
    pub request_sha256: String,
    pub catalog_sha256: String,
}

/// What one attempt of a template analysis produced. A completed receipt names the package, its files and the method the
/// template stated; any other names why, in a fixed code and the broker's own words, with the service's refusal as the
/// broker checked it when there was one.
#[derive(Clone, Debug)]
pub struct AnalysisReceipt {
    pub status: &'static str,
    pub error_code: Option<String>,
    pub message: Option<String>,
    pub refusal: Option<Value>,
    pub service_version: Option<String>,
    pub image_digest: Option<String>,
    pub method: Option<String>,
    pub files: Option<Value>,
    pub notes: Value,
    pub package_sha256: Option<String>,
    pub started_at: OffsetDateTime,
    pub finished_at: OffsetDateTime,
}

/// A person's request for a template analysis.
#[derive(Clone, Debug)]
pub struct AnalysisRequest<'a> {
    pub project: Uuid,
    pub source_task: Uuid,
    pub table_path: &'a str,
    pub feature: &'a str,
    pub settings: &'a Value,
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
    /// Migration 058 with the functions this role calls: the API's when `api`, the worker's otherwise.
    pub async fn ready_for_template_analyses(&self, api: bool) -> StoreResult<()> {
        let functions: &[&str] = if api {
            &[
                "enrichedlab_agent.broker_analysis_catalog(text,uuid)",
                "enrichedlab_agent.broker_request_analysis(text,uuid,uuid,uuid,text,text,jsonb,text,uuid,uuid)",
                "enrichedlab_agent.broker_task_analysis_detail(text,uuid,uuid)",
                "enrichedlab_agent.broker_analysis_versions(text,uuid,uuid)",
            ]
        } else {
            &[
                "enrichedlab_agent.broker_record_analysis_catalog(text,text,text,text,jsonb)",
                "enrichedlab_agent.broker_task_analysis(text,uuid,text,integer,bigint)",
                "enrichedlab_agent.broker_record_analysis_receipt(text,uuid,text,integer,bigint,text,text,text,jsonb,text,text,text,jsonb,jsonb,text,timestamptz,timestamptz)",
            ]
        };
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_schema_ready(58) AND bool_and(COALESCE(\
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

    /// Keep the render service's analysis features as the worker read them; returns the catalog's digest.
    pub async fn record_analysis_catalog(
        &self,
        worker: &str,
        version: &str,
        image_digest: Option<&str>,
        features: &Value,
    ) -> StoreResult<String> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_record_analysis_catalog($1,$2,$3,$4,$5)",
                &[&self.tenant, &worker, &version, &image_digest, features],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// The analysis catalog the worker read most recently, or null before its first read.
    pub async fn analysis_catalog(&self, actor: Uuid) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_analysis_catalog($1,$2)",
                &[&self.tenant, &actor],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get::<_, Option<Value>>(0).unwrap_or(Value::Null))
    }

    /// The run's template-analysis method and the newest analysis catalog, under the run's lease (migration 060):
    /// `{"profile", "catalog"}`, each null when the run has no such method or no worker has read a catalog.
    pub async fn research_run_analysis_catalog(
        &self,
        run: Uuid,
        worker: &str,
        generation: i64,
    ) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_research_run_analysis_catalog($1,$2,$3,$4)",
                &[&self.tenant, &run, &worker, &generation],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// Record a person's request and create its task in one transaction; returns the task.
    pub async fn request_analysis(
        &self,
        actor: Uuid,
        request: &AnalysisRequest<'_>,
    ) -> StoreResult<Uuid> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_request_analysis($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
                &[
                    &self.tenant,
                    &actor,
                    &request.project,
                    &request.source_task,
                    &request.table_path,
                    &request.feature,
                    request.settings,
                    &request.title,
                    &request.previous,
                    &request.request_id,
                ],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// The request of the worker's current lease, or none when the task is not a template analysis.
    pub async fn task_analysis(
        &self,
        task: Uuid,
        worker: &str,
        attempt: i32,
        generation: i64,
    ) -> StoreResult<Option<TaskAnalysis>> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_task_analysis($1,$2,$3,$4,$5)",
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

    pub async fn record_analysis_receipt(
        &self,
        task: Uuid,
        worker: &str,
        attempt: i32,
        generation: i64,
        receipt: &AnalysisReceipt,
    ) -> StoreResult<()> {
        let c = self.pool.get().await.map_err(pool_error)?;
        c.query_one(
            "SELECT enrichedlab_agent.broker_record_analysis_receipt($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17)",
            &[
                &self.tenant,
                &task,
                &worker,
                &attempt,
                &generation,
                &receipt.status,
                &receipt.error_code,
                &receipt.message,
                &receipt.refusal,
                &receipt.service_version,
                &receipt.image_digest,
                &receipt.method,
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

    /// A template analysis's request with its source and receipts, for anyone who may read it; null for any other task.
    pub async fn task_analysis_detail(&self, actor: Uuid, task: Uuid) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_task_analysis_detail($1,$2,$3)",
                &[&self.tenant, &actor, &task],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get::<_, Option<Value>>(0).unwrap_or(Value::Null))
    }

    /// The template analyses made from one analysis that the reader may open, newest first.
    pub async fn analysis_versions(&self, actor: Uuid, source_task: Uuid) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_analysis_versions($1,$2,$3)",
                &[&self.tenant, &actor, &source_task],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }
}
