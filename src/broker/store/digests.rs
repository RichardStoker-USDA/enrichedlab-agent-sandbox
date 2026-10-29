//! Result digests (migration 021): the worker records a bounded digest of each result
//! package it retained; the coordinator reads the digests of its run's analyses; the API serves a
//! digest to the artifact's readers.
use super::*;
use serde_json::Value;

impl Store {
    /// The worker's digest of a result package it registered under its current lease. Returns the
    /// digest's SHA-256 over the stored form. The same digest again is accepted.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_artifact_digest(
        &self,
        task: Uuid,
        worker: &str,
        generation: i64,
        relative_path: &str,
        artifact_sha256: &str,
        status: &str,
        digest: &Value,
    ) -> StoreResult<String> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_record_artifact_digest($1,$2,$3,$4,$5,$6,$7,$8)",
                &[
                    &self.tenant,
                    &task,
                    &worker,
                    &generation,
                    &relative_path,
                    &artifact_sha256,
                    &status,
                    digest,
                ],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// For the coordinator holding `run`: each planned analysis's result digests and the
    /// project's newest recorded findings.
    pub async fn research_run_results(
        &self,
        run: Uuid,
        worker: &str,
        generation: i64,
    ) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_research_run_results($1,$2,$3,$4)",
                &[&self.tenant, &run, &worker, &generation],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// A reader's view of an artifact's digest for its current bytes. An unreadable artifact is
    /// not found; a readable one without a digest says `not_recorded`.
    pub async fn artifact_digest(
        &self,
        principal: Uuid,
        task: Uuid,
        artifact: Uuid,
    ) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        c.query_one(
            "SELECT enrichedlab_agent.broker_get_artifact_digest($1,$2,$3,$4)",
            &[&self.tenant, &principal, &task, &artifact],
        )
        .await
        .map_err(database_error)?
        .get::<_, Option<Value>>(0)
        .ok_or(StoreError {
            kind: StoreErrorKind::NotFound,
            code: "artifact_not_found",
        })
    }
}
