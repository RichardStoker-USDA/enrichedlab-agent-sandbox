//! Task lineage (migration 016): who asked for a task, as recorded when it was
//! submitted, and the broker's records of its inputs, method, code and outputs; version 2
//! (migration 019) adds what each attempt sealed, ran and registered, and version 3
//! (migration 065) walks those recorded inputs to their readable upstream tasks and sources.
use super::*;
use serde_json::Value;

impl Store {
    /// One task's lineage for a reader of that task. An unreadable task is not found.
    pub async fn task_lineage(&self, actor: Uuid, task: Uuid) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_task_lineage($1,$2,$3)",
                &[&self.tenant, &actor, &task],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// Version 2 (migration 019): version 1 for the same reader plus each attempt's sealed
    /// inputs, bound execution and registered outputs, with every part not recorded named.
    pub async fn task_lineage_v2(&self, actor: Uuid, task: Uuid) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_task_lineage_v2($1,$2,$3)",
                &[&self.tenant, &actor, &task],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// Version 3 (migration 065): version 2 plus the readable upstream tasks, the exact
    /// records that connect them, terminal sources and the count of withheld inputs.
    pub async fn task_lineage_v3(&self, actor: Uuid, task: Uuid) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_task_lineage_v3($1,$2,$3)",
                &[&self.tenant, &actor, &task],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// The worker's record of what its own lease bound: image digest, template or tool and
    /// a digest of the parameters. Written once; the same values again are accepted.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_attempt_execution(
        &self,
        task: Uuid,
        worker: &str,
        generation: i64,
        image_digest: &str,
        template: Option<(&str, &str)>,
        tool_ref: Option<&str>,
        parameters_sha256: &str,
    ) -> StoreResult<()> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let (template_ref, template_sha256) = template.unzip();
        c.execute(
            "SELECT enrichedlab_agent.broker_record_attempt_execution($1,$2,$3,$4,$5,$6,$7,$8,$9)",
            &[
                &self.tenant,
                &task,
                &worker,
                &generation,
                &image_digest,
                &template_ref,
                &template_sha256,
                &tool_ref,
                &parameters_sha256,
            ],
        )
        .await
        .map_err(database_error)?;
        Ok(())
    }

    /// The input custodian's record of one copy it sealed for an attempt it is preparing,
    /// with the class of the source it resolved and that source's snapshot.
    pub async fn record_attempt_input(
        &self,
        task: Uuid,
        worker: &str,
        attempt: i32,
        generation: i64,
        input: &AttemptInput<'_>,
    ) -> StoreResult<()> {
        let c = self.pool.get().await.map_err(pool_error)?;
        c.execute(
            "SELECT enrichedlab_agent.broker_record_attempt_input($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
            &[
                &self.tenant,
                &task,
                &worker,
                &attempt,
                &generation,
                &input.input_ref,
                &input.class,
                &input.sealed_sha256,
                &input.size_bytes,
                &input.custody_transaction,
                &input.source,
            ],
        )
        .await
        .map_err(database_error)?;
        Ok(())
    }
}

/// One sealed input copy as the custodian records it (migration 019).
pub struct AttemptInput<'a> {
    pub input_ref: &'a str,
    /// `lab_database`, `object_data`, `paper`, `public_reference` or `prior_output`.
    pub class: &'a str,
    pub sealed_sha256: &'a str,
    pub size_bytes: i64,
    pub custody_transaction: &'a str,
    /// For `lab_database`: `query_sha256`, `snapshot` and `wal_position`; for `object_data`:
    /// `source_version` and `source_sha256`.
    pub source: &'a Value,
}
