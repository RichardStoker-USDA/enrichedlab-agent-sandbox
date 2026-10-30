use super::*;
use crate::broker::coordinator::StartRun;
use serde_json::Value;

impl Store {
    /// A coordinator whose lead may read papers serves only once migration 069 makes the read calls'
    /// kept texts readable to the lab and part of the project's package.
    pub async fn ready_for_reading(&self) -> StoreResult<()> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one("SELECT enrichedlab_agent.broker_schema_ready(69)", &[])
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

    pub async fn reconcile_research_run(
        &self,
        run: Uuid,
        worker: &str,
        generation: i64,
        revision: i64,
        state: &Value,
    ) -> StoreResult<i64> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_reconcile_research_run($1,$2,$3,$4,$5,$6)",
                &[&self.tenant, &run, &worker, &generation, &revision, &state],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }
    /// Move finished steps of the run's project out of its working plan into the next archive page
    /// (migration 048). The database takes the steps from the checkpoint at `revision` and returns the new revision.
    pub async fn archive_research_plan(
        &self,
        run: Uuid,
        worker: &str,
        generation: i64,
        revision: i64,
        keys: &[String],
    ) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_archive_research_plan($1,$2,$3,$4,$5,$6)",
                &[&self.tenant, &run, &worker, &generation, &revision, &keys],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }
    pub async fn start_research_run(
        &self,
        actor: Uuid,
        project: Uuid,
        request: &StartRun,
    ) -> StoreResult<Uuid> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_start_research_run($1,$2,$3,$4,$5,$6,$7,$8)",
                &[
                    &self.tenant,
                    &actor,
                    &project,
                    &request.run_id,
                    &request.expected_revision,
                    &request.allowed_profiles,
                    &request.max_turns,
                    &request.max_parallel,
                ],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }
    pub async fn research_runs(&self, actor: Uuid, project: Uuid) -> StoreResult<Vec<Value>> {
        let c = self.pool.get().await.map_err(pool_error)?;
        Ok(c.query(
            "SELECT enrichedlab_agent.broker_list_research_runs($1,$2,$3)",
            &[&self.tenant, &actor, &project],
        )
        .await
        .map_err(database_error)?
        .into_iter()
        .map(|r| r.get(0))
        .collect())
    }
    pub async fn research_turns(
        &self,
        actor: Uuid,
        run: Uuid,
        after: i32,
        limit: i32,
    ) -> StoreResult<Vec<Value>> {
        let c = self.pool.get().await.map_err(pool_error)?;
        Ok(c.query(
            "SELECT enrichedlab_agent.broker_research_turn_history($1,$2,$3,$4,$5)",
            &[&self.tenant, &actor, &run, &after, &limit],
        )
        .await
        .map_err(database_error)?
        .into_iter()
        .map(|r| r.get(0))
        .collect())
    }
    /// Keeps a decision's exact model texts by SHA-256 under the run's lease;
    /// returns their addresses in the order given.
    pub async fn keep_research_contexts(
        &self,
        run: Uuid,
        worker: &str,
        generation: i64,
        contents: &[&[u8]],
    ) -> StoreResult<Vec<String>> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let contents: Vec<Vec<u8>> = contents.iter().map(|b| b.to_vec()).collect();
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_research_keep_contexts($1,$2,$3,$4,$5)",
                &[&self.tenant, &run, &worker, &generation, &contents],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }
    /// One kept text a decision of this run names, for a project reader who is a lab member.
    pub async fn research_context(
        &self,
        actor: Uuid,
        run: Uuid,
        sha256: &str,
    ) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_research_context($1,$2,$3,$4)",
                &[&self.tenant, &actor, &run, &sha256],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }
    /// Until when each of the project's analyses keeps its working copy, or when the sweep cleared it,
    /// for a project reader.
    pub async fn project_working_copies(&self, actor: Uuid, project: Uuid) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_project_working_copies($1,$2,$3)",
                &[&self.tenant, &actor, &project],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }
    pub async fn pause_research_run(&self, actor: Uuid, run: Uuid) -> StoreResult<()> {
        let c = self.pool.get().await.map_err(pool_error)?;
        c.query_one(
            "SELECT enrichedlab_agent.broker_pause_research_run($1,$2,$3)",
            &[&self.tenant, &actor, &run],
        )
        .await
        .map_err(database_error)?;
        Ok(())
    }
    /// The next accepted run (migration 085: the three-argument claim claims accepted runs only).
    pub async fn claim_research_run(
        &self,
        worker: &str,
        seconds: i32,
    ) -> StoreResult<Option<Value>> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_claim_research_run($1,$2,$3)",
                &[&self.tenant, &worker, &seconds],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }
    /// The next run of `lane` (`accepted` or `candidate`, migration 085): a coordinator claims one lane only.
    pub async fn claim_research_run_in(
        &self,
        worker: &str,
        seconds: i32,
        lane: &str,
    ) -> StoreResult<Option<Value>> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_claim_research_run($1,$2,$3,$4)",
                &[&self.tenant, &worker, &seconds, &lane],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }
    pub async fn renew_research_run(
        &self,
        run: Uuid,
        worker: &str,
        generation: i64,
        seconds: i32,
    ) -> StoreResult<bool> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_renew_research_run($1,$2,$3,$4,$5)",
                &[&self.tenant, &run, &worker, &generation, &seconds],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }
    /// `reason` is one of the coordinator's fixed rejection codes, never model text.
    pub async fn yield_research_run(
        &self,
        run: Uuid,
        worker: &str,
        generation: i64,
        code: &str,
        reason: Option<&str>,
    ) -> StoreResult<()> {
        self.yield_research_run_with_reply(run, worker, generation, code, reason, None)
            .await
    }
    /// A deferral that refused a readable lead reply also names the kept context it answered and the reply
    /// (migration 044), and the reply's reasoning when the model gave some (migration 061), all kept first by
    /// `keep_research_contexts` under the same lease.
    pub async fn yield_research_run_with_reply(
        &self,
        run: Uuid,
        worker: &str,
        generation: i64,
        code: &str,
        reason: Option<&str>,
        reply: Option<(&str, &str, Option<&str>)>,
    ) -> StoreResult<()> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let (context, reasoning) = (reply.map(|r| r.0), reply.and_then(|r| r.2));
        let reply = reply.map(|r| r.1);
        c.query_one(
            "SELECT enrichedlab_agent.broker_yield_research_run($1,$2,$3,$4,$5,$6,$7,$8,$9)",
            &[
                &self.tenant,
                &run,
                &worker,
                &generation,
                &code,
                &reason,
                &context,
                &reply,
                &reasoning,
            ],
        )
        .await
        .map_err(database_error)?;
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn commit_research_turn(
        &self,
        run: Uuid,
        worker: &str,
        generation: i64,
        revision: i64,
        state: &Value,
        actions: &Value,
        record: &Value,
        outcome: &str,
    ) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_commit_research_turn($1,$2,$3,$4,$5,$6,$7,$8,$9)",
                &[
                    &self.tenant,
                    &run,
                    &worker,
                    &generation,
                    &revision,
                    &state,
                    &actions,
                    &record,
                    &outcome,
                ],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }
}
