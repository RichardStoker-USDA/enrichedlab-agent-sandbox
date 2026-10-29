//! Review, collaboration, administration and evidence-package reads (migration 014).
use super::*;
use serde_json::Value;
use std::collections::HashMap;
use tokio_postgres::IsolationLevel;

fn merge_package_components(snapshot: &mut Value, components: &Value) {
    let inspecting = !snapshot["requester"]["inspection_expires_at"].is_null();
    let reasoning_access = !inspecting && components["reasoning_access"].as_bool() == Some(true);
    snapshot["reasoning_access"] = Value::Bool(reasoning_access);
    snapshot["reasoning"] = if reasoning_access {
        components["reasoning"].clone()
    } else {
        Value::Array(Vec::new())
    };
    snapshot["literature_reviews"] = if reasoning_access {
        components
            .get("literature_reviews")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([]))
    } else {
        serde_json::json!([])
    };
    snapshot["disclosure_scope"] = if inspecting {
        Value::String("inspection".into())
    } else {
        components["disclosure_scope"].clone()
    };
    for key in ["digests", "task_origins", "papers"] {
        snapshot[key] = components[key].clone();
    }
}

fn merge_package_task_details(snapshot: &mut Value, details: &Value) {
    let Some(tasks) = snapshot["tasks"].as_array_mut() else {
        return;
    };
    let by_task: HashMap<&str, &Value> = details
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|detail| detail["task_id"].as_str().map(|task_id| (task_id, detail)))
        .collect();
    for task in tasks {
        let Some(detail) = task["task_id"]
            .as_str()
            .and_then(|task_id| by_task.get(task_id))
        else {
            continue;
        };
        if detail["query_detail"].is_object() {
            task["query_detail"] = detail["query_detail"].clone();
        }
        if detail["analysis_detail"].is_object() {
            task["analysis_detail"] = detail["analysis_detail"].clone();
        }
    }
}

impl Store {
    pub async fn research_run_events(
        &self,
        actor: Uuid,
        run: Uuid,
        after: i64,
        limit: i32,
    ) -> StoreResult<Vec<Value>> {
        let c = self.pool.get().await.map_err(pool_error)?;
        Ok(c.query(
            "SELECT enrichedlab_agent.broker_research_run_event_history($1,$2,$3,$4,$5)",
            &[&self.tenant, &actor, &run, &after, &limit],
        )
        .await
        .map_err(database_error)?
        .into_iter()
        .map(|r| r.get(0))
        .collect())
    }

    pub async fn is_project_administrator(&self, actor: Uuid) -> StoreResult<bool> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_is_project_administrator($1,$2)",
                &[&self.tenant, &actor],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    pub async fn admin_project_directory(
        &self,
        actor: Uuid,
        limit: i32,
    ) -> StoreResult<Vec<Value>> {
        let c = self.pool.get().await.map_err(pool_error)?;
        Ok(c.query(
            "SELECT enrichedlab_agent.broker_admin_project_directory($1,$2,$3)",
            &[&self.tenant, &actor, &limit],
        )
        .await
        .map_err(database_error)?
        .into_iter()
        .map(|r| r.get(0))
        .collect())
    }

    pub async fn end_project_inspection(&self, actor: Uuid, project: Uuid) -> StoreResult<bool> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_end_project_inspection($1,$2,$3)",
                &[&self.tenant, &actor, &project],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    pub async fn project_members(&self, actor: Uuid, project: Uuid) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_project_members($1,$2,$3)",
                &[&self.tenant, &actor, &project],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// One read-only repeatable-read transaction: every record class in the package
    /// comes from the same committed state.
    pub async fn project_package_snapshot(&self, actor: Uuid, project: Uuid) -> StoreResult<Value> {
        let mut c = self.pool.get().await.map_err(pool_error)?;
        let transaction = c
            .build_transaction()
            .isolation_level(IsolationLevel::RepeatableRead)
            .read_only(true)
            .start()
            .await
            .map_err(database_error)?;
        let row = transaction
            .query_one(
                "SELECT enrichedlab_agent.broker_project_package_snapshot($1,$2,$3)",
                &[&self.tenant, &actor, &project],
            )
            .await
            .map_err(database_error)?;
        let mut snapshot: Value = row.get(0);
        // Query and template-analysis requests and receipts are read through their existing per-task reader rules.
        // One batched call keeps them in this transaction's snapshot without trusting an analysis archive's own text.
        let details = transaction
            .query_one(
                "WITH task_ids AS (\
                   SELECT value->>'task_id' AS written, (value->>'task_id')::uuid AS task_id \
                   FROM jsonb_array_elements($3::jsonb) AS tasks(value)\
                 ), details AS (\
                   SELECT written AS task_id,\
                     enrichedlab_agent.broker_task_query_detail($1,$2,task_id) AS query_detail,\
                     enrichedlab_agent.broker_task_analysis_detail($1,$2,task_id) AS analysis_detail \
                   FROM task_ids\
                 ) \
                 SELECT coalesce(jsonb_agg(jsonb_build_object('task_id',task_id,\
                   'query_detail',query_detail,'analysis_detail',analysis_detail) ORDER BY task_id),'[]'::jsonb) \
                 FROM details",
                &[&self.tenant, &actor, &snapshot["tasks"]],
            )
            .await
            .map_err(database_error)?;
        let details: Value = details.get(0);
        merge_package_task_details(&mut snapshot, &details);
        // Same repeatable-read snapshot: the admitted blocks for the project's tasks.
        let code = transaction
            .query_one(
                "SELECT enrichedlab_agent.broker_project_generated_code($1,$2,$3)",
                &[&self.tenant, &actor, &project],
            )
            .await
            .map_err(database_error)?;
        snapshot["generated_code"] = code.get(0);
        // And every person's review of those tasks (migration 023).
        let reviews = transaction
            .query_one(
                "SELECT enrichedlab_agent.broker_project_review_records($1,$2,$3)",
                &[&self.tenant, &actor, &project],
            )
            .await
            .map_err(database_error)?;
        snapshot["reviews"] = reviews.get(0);
        let components = transaction
            .query_one(
                "SELECT enrichedlab_agent.broker_project_package_components($1,$2,$3)",
                &[&self.tenant, &actor, &project],
            )
            .await
            .map_err(database_error)?;
        let components: Value = components.get(0);
        merge_package_components(&mut snapshot, &components);
        transaction.commit().await.map_err(database_error)?;
        Ok(snapshot)
    }

    /// Return the existing revision for unchanged content or atomically allocate
    /// the next per-project revision for a new digest inventory.
    pub async fn register_project_package(
        &self,
        actor: Uuid,
        project: Uuid,
        disclosure_scope: crate::broker::package_registry::DisclosureScope,
        content_sha256: &str,
        inventory: &Value,
    ) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_register_project_package($1,$2,$3,$4,$5,$6)",
                &[
                    &self.tenant,
                    &actor,
                    &project,
                    &disclosure_scope.as_str(),
                    &content_sha256,
                    inventory,
                ],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    pub async fn record_project_export(
        &self,
        actor: Uuid,
        project: Uuid,
        detail: &Value,
    ) -> StoreResult<Uuid> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_record_project_export($1,$2,$3,$4)",
                &[&self.tenant, &actor, &project, detail],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn inspection_snapshot_cannot_receive_reasoning_but_keeps_r3_readable_records() {
        let mut snapshot = json!({
            "requester": {"inspection_expires_at": "2099-01-01T00:00:00Z"},
            "research_runs": [{"turns": [{"record": {"context_inputs": {"literature": {"passages": []}}}}]}]
        });
        let runs = snapshot["research_runs"].clone();
        let components = json!({
            "disclosure_scope": "inspection",
            "reasoning_access": true,
            "reasoning": [{"text": "reasoning-must-not-leak"}],
            "digests": [{"digest_sha256": "a".repeat(64)}],
            "task_origins": [{"task_id": "00000000-0000-4000-8000-000000000001"}],
            "papers": [{"paper_id": "00000000-0000-4000-8000-000000000002"}]
        });

        merge_package_components(&mut snapshot, &components);

        assert_eq!(snapshot["reasoning_access"], false);
        assert_eq!(snapshot["disclosure_scope"], "inspection");
        assert_eq!(snapshot["reasoning"], json!([]));
        assert_eq!(snapshot["research_runs"], runs);
        assert_eq!(snapshot["digests"], components["digests"]);
        assert_eq!(snapshot["task_origins"], components["task_origins"]);
        assert_eq!(snapshot["papers"], components["papers"]);
    }

    #[test]
    fn package_task_details_are_bound_to_their_task_and_null_records_are_omitted() {
        let mut snapshot = json!({"tasks": [
            {"task_id": "t2"},
            {"task_id": "t1"}
        ]});
        let details = json!([
            {"task_id": "t1", "query_detail": {"task_id": "t1", "title": "Source"},
             "analysis_detail": null},
            {"task_id": "t2", "query_detail": null,
             "analysis_detail": {"task_id": "t2", "feature": "summary"}}
        ]);

        merge_package_task_details(&mut snapshot, &details);

        assert_eq!(snapshot["tasks"][0]["analysis_detail"]["task_id"], "t2");
        assert!(snapshot["tasks"][0].get("query_detail").is_none());
        assert_eq!(snapshot["tasks"][1]["query_detail"]["task_id"], "t1");
        assert!(snapshot["tasks"][1].get("analysis_detail").is_none());
    }
}
