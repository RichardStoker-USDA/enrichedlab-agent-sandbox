//! A run's orientation and what the supervisor counts (migration 074).
use super::*;
use serde_json::Value;

impl Store {
    /// The coordinator supervises runs only once migration 074 is in place and its role may read a run's faults and pause
    /// it.
    pub async fn ready_for_supervisor(&self) -> StoreResult<()> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_schema_ready(74) AND coalesce(has_function_privilege(current_user,\
                 to_regprocedure('enrichedlab_agent.broker_research_run_faults(text,uuid,text,bigint)'),'EXECUTE'),false) \
                 AND coalesce(has_function_privilege(current_user,to_regprocedure(\
                 'enrichedlab_agent.broker_supervisor_pause_research_run(text,uuid,text,bigint,text)'),'EXECUTE'),false)",
                &[],
            )
            .await
            .map_err(database_error)?;
        if row.get::<_, bool>(0) {
            Ok(())
        } else {
            Err(StoreError {
                kind: StoreErrorKind::Unavailable,
                code: "broker_schema_not_ready",
            })
        }
    }

    /// The coordinator orients runs only once migration 074 is in place and its role may read and keep a run's
    /// orientation.
    pub async fn ready_for_orientation(&self) -> StoreResult<()> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_schema_ready(74) AND coalesce(has_function_privilege(current_user,\
                 to_regprocedure('enrichedlab_agent.broker_orientation_for_run(text,uuid,text,bigint,jsonb)'),'EXECUTE'),false)",
                &[],
            )
            .await
            .map_err(database_error)?;
        if row.get::<_, bool>(0) {
            Ok(())
        } else {
            Err(StoreError {
                kind: StoreErrorKind::Unavailable,
                code: "broker_schema_not_ready",
            })
        }
    }

    /// Reads the run's orientation, or keeps `record` as it once and returns what is kept.
    pub async fn orientation_for_run(
        &self,
        run: Uuid,
        worker: &str,
        generation: i64,
        record: Option<&Value>,
    ) -> StoreResult<Option<Value>> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_orientation_for_run($1,$2,$3,$4,$5)",
                &[&self.tenant, &run, &worker, &generation, &record],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// A lab member's read of a run's checked orientation.
    pub async fn research_orientation(&self, actor: Uuid, run: Uuid) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_research_orientation($1,$2,$3)",
                &[&self.tenant, &actor, &run],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// The run's deferrals other than waits and each committed turn's outcome and submitted tasks, under its lease.
    pub async fn research_run_faults(
        &self,
        run: Uuid,
        worker: &str,
        generation: i64,
    ) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_research_run_faults($1,$2,$3,$4)",
                &[&self.tenant, &run, &worker, &generation],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// Pauses the run for a person with one of the supervisor's fixed reasons.
    pub async fn supervisor_pause_research_run(
        &self,
        run: Uuid,
        worker: &str,
        generation: i64,
        reason: &str,
    ) -> StoreResult<()> {
        let c = self.pool.get().await.map_err(pool_error)?;
        c.execute(
            "SELECT enrichedlab_agent.broker_supervisor_pause_research_run($1,$2,$3,$4,$5)",
            &[&self.tenant, &run, &worker, &generation, &reason],
        )
        .await
        .map_err(database_error)?;
        Ok(())
    }
}
