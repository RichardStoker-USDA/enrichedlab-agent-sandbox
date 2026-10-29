//! Guarded read-only queries (migration 038). The coordinator reads the sources its run may query and the
//! admitted queries of earlier analyses it may learn from (migration 042) and of its own project's analyses (043), the
//! worker reads the admitted query of its current lease and records one receipt per attempt, and a task's reader gets
//! the query with its source and receipts.
use super::*;
use serde::Deserialize;
use serde_json::Value;
use sha2::Digest;
use time::OffsetDateTime;

/// The admitted query and its source, as the worker receives it for its current lease.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskQuery {
    pub source: String,
    pub title: String,
    pub release: String,
    pub schema: String,
    pub relations: Vec<String>,
    pub functions: Vec<String>,
    pub row_cap: u64,
    pub statement_timeout_ms: u64,
    pub connection_ref: String,
    pub snapshot: Value,
    pub proposed_sql: String,
    pub admitted_sql: String,
    pub sql_sha256: String,
    pub guard: String,
    pub row_limit: u64,
    #[serde(default)]
    pub reviewed_query: Option<super::super::reviewed_query::Record>,
}

/// What one attempt of a query produced. A completed receipt names the rows, columns and package; any other names why.
#[derive(Clone, Debug)]
pub struct QueryReceipt {
    pub status: &'static str,
    pub error_code: Option<String>,
    pub row_count: Option<i32>,
    pub columns: Option<Value>,
    pub package_sha256: Option<String>,
    pub server_version: Option<String>,
    pub started_at: OffsetDateTime,
    pub finished_at: OffsetDateTime,
    /// The row a live lab source's database states about its version, read in the result's own transaction (migration
    /// 053); none for a public reference source and for an attempt that did not complete.
    pub source_version: Option<Value>,
}

fn record_invalid() -> StoreError {
    StoreError {
        kind: StoreErrorKind::Internal,
        code: "database_operation_failed",
    }
}

impl Store {
    /// The sources the held run may query, as the lead reads them.
    pub async fn research_run_sql_sources(
        &self,
        run: Uuid,
        worker: &str,
        generation: i64,
    ) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_research_run_sql_sources($1,$2,$3,$4)",
                &[&self.tenant, &run, &worker, &generation],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// The admitted queries of the earlier analyses in `tasks` that this run may learn from (migration 042),
    /// in the order given: the database decides which, under the run's lease, and returns no rows of theirs.
    pub async fn research_run_prior_queries(
        &self,
        run: Uuid,
        worker: &str,
        generation: i64,
        tasks: &[Uuid],
    ) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let tasks = tasks.to_vec();
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_research_run_prior_queries($1,$2,$3,$4,$5)",
                &[&self.tenant, &run, &worker, &generation, &tasks],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// The admitted queries of this run's own project among `tasks`, with each one's newest receipt (migration 043),
    /// in the order given; never a row.
    pub async fn research_run_own_queries(
        &self,
        run: Uuid,
        worker: &str,
        generation: i64,
        tasks: &[Uuid],
    ) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let tasks = tasks.to_vec();
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_research_run_own_queries($1,$2,$3,$4,$5)",
                &[&self.tenant, &run, &worker, &generation, &tasks],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// The admitted query for the worker's current lease, or none when the task is not a query. The stored digest must
    /// match the text, or the record is refused.
    pub async fn task_query(
        &self,
        task: Uuid,
        worker: &str,
        attempt: i32,
        generation: i64,
    ) -> StoreResult<Option<TaskQuery>> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_task_query($1,$2,$3,$4,$5)",
                &[&self.tenant, &task, &worker, &attempt, &generation],
            )
            .await
            .map_err(database_error)?;
        let Some(value) = row.get::<_, Option<Value>>(0) else {
            return Ok(None);
        };
        let query: TaskQuery = serde_json::from_value(value).map_err(|_| record_invalid())?;
        let digest = format!(
            "sha256:{}",
            hex::encode(sha2::Sha256::digest(query.admitted_sql.as_bytes()))
        );
        if digest != query.sql_sha256 {
            return Err(record_invalid());
        }
        Ok(Some(query))
    }

    pub async fn record_query_receipt(
        &self,
        task: Uuid,
        worker: &str,
        attempt: i32,
        generation: i64,
        receipt: &QueryReceipt,
    ) -> StoreResult<()> {
        let c = self.pool.get().await.map_err(pool_error)?;
        c.query_one(
            "SELECT enrichedlab_agent.broker_record_query_receipt($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)",
            &[
                &self.tenant,
                &task,
                &worker,
                &attempt,
                &generation,
                &receipt.status,
                &receipt.error_code,
                &receipt.row_count,
                &receipt.columns,
                &receipt.package_sha256,
                &receipt.server_version,
                &receipt.started_at,
                &receipt.finished_at,
                &receipt.source_version,
            ],
        )
        .await
        .map_err(database_error)?;
        Ok(())
    }

    /// Start re-measuring a live source's card for a version a completed query of that source read (migration 082):
    /// the source as the card command takes it, the newest card's spec, the version's digest and the source's reader.
    /// `None` when nothing is to be done or another refresh of the source runs.
    pub async fn begin_card_refresh(
        &self,
        source: &str,
        version: &Value,
        worker: &str,
    ) -> StoreResult<Option<Value>> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_begin_sql_source_card_refresh($1,$2,$3,$4)",
                &[&self.tenant, &source, version, &worker],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get::<_, Option<Value>>(0))
    }

    /// Finish a running refresh with the measured card (which states the version row it read), or with the code that
    /// says why there is none. Returns the refresh's state (`registered`, `refused` or `failed`) and its code or card digest.
    pub async fn finish_card_refresh(
        &self,
        source: &str,
        version_sha256: &str,
        worker: &str,
        card: Option<&Value>,
        code: Option<&str>,
    ) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_finish_sql_source_card_refresh($1,$2,$3,$4,$5,$6)",
                &[
                    &self.tenant,
                    &source,
                    &version_sha256,
                    &worker,
                    &card,
                    &code,
                ],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// A task's query with its source and receipts, for anyone who may read the task; null when it ran no query.
    pub async fn task_query_detail(&self, actor: Uuid, task: Uuid) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_task_query_detail($1,$2,$3)",
                &[&self.tenant, &actor, &task],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get::<_, Option<Value>>(0).unwrap_or(Value::Null))
    }
}
