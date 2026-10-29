//! Checked review custody and lab-member reads (migration 070).
use super::*;
use serde_json::Value;

impl Store {
    pub async fn ready_for_literature_review(&self) -> StoreResult<()> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c.query_one("SELECT enrichedlab_agent.broker_schema_ready(70) AND coalesce(has_function_privilege(\
            current_user,to_regprocedure('enrichedlab_agent.broker_literature_review_for_run(text,uuid,text,bigint,text,jsonb)'),\
            'EXECUTE'),false)", &[]).await.map_err(database_error)?;
        if row.get::<_, bool>(0) {
            Ok(())
        } else {
            Err(StoreError {
                kind: StoreErrorKind::Unavailable,
                code: "broker_schema_not_ready",
            })
        }
    }

    pub async fn literature_review_for_run(
        &self,
        run: Uuid,
        worker: &str,
        generation: i64,
        inputs: &str,
        record: Option<&Value>,
    ) -> StoreResult<Option<Value>> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_literature_review_for_run($1,$2,$3,$4,$5,$6)",
                &[&self.tenant, &run, &worker, &generation, &inputs, &record],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    pub async fn literature_review(
        &self,
        actor: Uuid,
        run: Uuid,
        inputs: &str,
    ) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_literature_review($1,$2,$3,$4)",
                &[&self.tenant, &actor, &run, &inputs],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }
}
