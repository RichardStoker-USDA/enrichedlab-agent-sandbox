//! A person's review of an analysis (migration 023): a principal with the project's
//! `review` capability records a decision bound to the analysis's current outputs; anyone who may
//! read the analysis reads the history.
use super::*;
use serde_json::Value;

impl Store {
    /// Records `decision` on `task` for `actor`. The same request again returns the same review.
    pub async fn record_task_review(
        &self,
        actor: Uuid,
        task: Uuid,
        decision: &str,
        note: Option<&str>,
        request: Uuid,
    ) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_record_task_review($1,$2,$3,$4,$5,$6)",
                &[&self.tenant, &actor, &task, &decision, &note, &request],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// Every review of a readable analysis, newest first.
    pub async fn task_reviews(&self, actor: Uuid, task: Uuid) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_task_reviews($1,$2,$3)",
                &[&self.tenant, &actor, &task],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// The newest review of each analysis in a readable project.
    pub async fn project_reviews(&self, actor: Uuid, project: Uuid) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_project_reviews($1,$2,$3)",
                &[&self.tenant, &actor, &project],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }
}
