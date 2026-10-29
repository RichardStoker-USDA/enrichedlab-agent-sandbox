//! Lab memory (migration 027): a lab lead or reviewer shares an observation from a
//! lab-visible project whose analyses a person accepted; lab members read the lab's shared notes;
//! the coordinator reads the active notes relevant to the run it holds.
use super::*;
use serde_json::Value;

impl Store {
    /// Shares the observation `statement` with `evidence_refs` from `project`'s saved version
    /// `revision`. The same request again returns the same note.
    #[allow(clippy::too_many_arguments)]
    pub async fn share_lab_note(
        &self,
        actor: Uuid,
        project: Uuid,
        revision: i64,
        statement: &str,
        evidence_refs: &Value,
        reason: Option<&str>,
        review_days: Option<i32>,
        request: Uuid,
    ) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_share_lab_note($1,$2,$3,$4,$5,$6,$7,$8,$9)",
                &[
                    &self.tenant,
                    &actor,
                    &project,
                    &revision,
                    &statement,
                    evidence_refs,
                    &reason,
                    &review_days,
                    &request,
                ],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// Retires a shared note with a reason: its sharer or a lab lead or reviewer.
    pub async fn retire_lab_note(
        &self,
        actor: Uuid,
        note: Uuid,
        reason: &str,
        request: Uuid,
    ) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_retire_lab_note($1,$2,$3,$4,$5)",
                &[&self.tenant, &actor, &note, &reason, &request],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// Moves a shared note's review-by date `review_days` from now: a lab lead or reviewer.
    pub async fn renew_lab_note(
        &self,
        actor: Uuid,
        note: Uuid,
        review_days: i32,
        request: Uuid,
    ) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_renew_lab_note($1,$2,$3,$4,$5)",
                &[&self.tenant, &actor, &note, &review_days, &request],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// Every shared note of a lab `actor` belongs to, newest first, and whether `actor` may
    /// share and renew.
    pub async fn lab_notes(&self, actor: Uuid, lab: &str) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_lab_notes($1,$2,$3)",
                &[&self.tenant, &actor, &lab],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// For the coordinator holding `run`: the lab's active shared notes relevant to it, in the
    /// fixed order the database applies.
    pub async fn research_run_lab_notes(
        &self,
        run: Uuid,
        worker: &str,
        generation: i64,
    ) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_research_run_lab_notes($1,$2,$3,$4)",
                &[&self.tenant, &run, &worker, &generation],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }
}
