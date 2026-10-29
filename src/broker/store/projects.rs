use super::*;
use crate::broker::{
    model::SubmitTaskRequest,
    project::{CheckpointRequest, CreateProject, ProjectGrant},
};
use serde_json::Value;

impl Store {
    pub async fn create_project(&self, actor: Uuid, request: &CreateProject) -> StoreResult<Uuid> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let visibility = serde_json::to_value(&request.visibility).expect("visibility");
        let row = client
            .query_one(
                "SELECT enrichedlab_agent.broker_create_project($1,$2,$3,$4,$5,$6)",
                &[
                    &self.tenant,
                    &actor,
                    &request.project_id,
                    &request.group,
                    &request.title,
                    &visibility.as_str().unwrap(),
                ],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }
    pub async fn get_project(&self, actor: Uuid, project: Uuid) -> StoreResult<Value> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let row = client
            .query_one(
                "SELECT enrichedlab_agent.broker_get_project($1,$2,$3)",
                &[&self.tenant, &actor, &project],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }
    /// The API's project read (migration 054): the project with the caller's last look, the number of analyses and
    /// the cursor for those past the newest 200.
    pub async fn read_project(&self, actor: Uuid, project: Uuid) -> StoreResult<Value> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let row = client
            .query_one(
                "SELECT enrichedlab_agent.broker_read_project($1,$2,$3)",
                &[&self.tenant, &actor, &project],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }
    /// A person records the revision they last looked at; it moves forward only.
    pub async fn project_look(
        &self,
        actor: Uuid,
        project: Uuid,
        revision: i64,
    ) -> StoreResult<Value> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let row = client
            .query_one(
                "SELECT enrichedlab_agent.broker_project_look($1,$2,$3,$4)",
                &[&self.tenant, &actor, &project, &revision],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }
    /// One page of a project's analyses, newest first, after an opaque cursor.
    pub async fn project_analyses(
        &self,
        actor: Uuid,
        project: Uuid,
        cursor: Option<&str>,
        limit: i32,
    ) -> StoreResult<Value> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let row = client
            .query_one(
                "SELECT enrichedlab_agent.broker_project_analyses($1,$2,$3,$4,$5)",
                &[&self.tenant, &actor, &project, &cursor, &limit],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }
    /// A project's checkpoints after a revision, oldest first, with the revision to read from next.
    pub async fn project_history_after(
        &self,
        actor: Uuid,
        project: Uuid,
        after: i64,
        limit: i32,
    ) -> StoreResult<Value> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let row = client
            .query_one(
                "SELECT enrichedlab_agent.broker_project_history_after($1,$2,$3,$4,$5)",
                &[&self.tenant, &actor, &project, &after, &limit],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }
    pub async fn list_projects(&self, actor: Uuid, limit: i32) -> StoreResult<Vec<Value>> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let rows = client
            .query(
                "SELECT enrichedlab_agent.broker_list_projects($1,$2,$3)",
                &[&self.tenant, &actor, &limit],
            )
            .await
            .map_err(database_error)?;
        Ok(rows.into_iter().map(|r| r.get(0)).collect())
    }
    pub async fn grant_project(
        &self,
        actor: Uuid,
        project: Uuid,
        request: &ProjectGrant,
    ) -> StoreResult<()> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let capabilities: Vec<String> = request
            .capabilities
            .iter()
            .map(|c| {
                serde_json::to_value(c)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        client
            .query_one(
                "SELECT enrichedlab_agent.broker_grant_project($1,$2,$3,$4,$5)",
                &[
                    &self.tenant,
                    &actor,
                    &project,
                    &request.principal_id,
                    &capabilities,
                ],
            )
            .await
            .map_err(database_error)?;
        Ok(())
    }
    pub async fn inspect_project(
        &self,
        actor: Uuid,
        project: Uuid,
        reason: &str,
    ) -> StoreResult<time::OffsetDateTime> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let row = client
            .query_one(
                "SELECT enrichedlab_agent.broker_inspect_project($1,$2,$3,$4)",
                &[&self.tenant, &actor, &project, &reason],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }
    pub async fn checkpoint_project(
        &self,
        actor: Uuid,
        project: Uuid,
        request: &CheckpointRequest,
    ) -> StoreResult<i64> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let state = serde_json::to_value(&request.state).expect("research state");
        let row = client
            .query_one(
                "SELECT enrichedlab_agent.broker_checkpoint_project($1,$2,$3,$4,$5,$6)",
                &[
                    &self.tenant,
                    &actor,
                    &project,
                    &request.expected_revision,
                    &request.request_id,
                    &state,
                ],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }
    pub async fn project_history(
        &self,
        actor: Uuid,
        project: Uuid,
        before: i64,
        limit: i32,
    ) -> StoreResult<Vec<Value>> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let rows = client
            .query(
                "SELECT enrichedlab_agent.broker_project_history($1,$2,$3,$4,$5)",
                &[&self.tenant, &actor, &project, &before, &limit],
            )
            .await
            .map_err(database_error)?;
        Ok(rows.into_iter().map(|r| r.get(0)).collect())
    }
    pub async fn project_audit(
        &self,
        actor: Uuid,
        project: Uuid,
        limit: i32,
    ) -> StoreResult<Vec<Value>> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let rows = client
            .query(
                "SELECT enrichedlab_agent.broker_project_audit_page($1,$2,$3,$4)",
                &[&self.tenant, &actor, &project, &limit],
            )
            .await
            .map_err(database_error)?;
        Ok(rows.into_iter().map(|r| r.get(0)).collect())
    }
    /// The project's plan ledger: each recorded checkpoint revision with the plan steps it changed.
    pub async fn project_plan_ledger(&self, actor: Uuid, project: Uuid) -> StoreResult<Value> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let row = client
            .query_one(
                "SELECT enrichedlab_agent.broker_project_plan_ledger($1,$2,$3)",
                &[&self.tenant, &actor, &project],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }
    /// What waits for the caller across their labs, each item with when they saw it.
    pub async fn inbox(&self, actor: Uuid) -> StoreResult<Value> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let row = client
            .query_one(
                "SELECT enrichedlab_agent.broker_inbox($1,$2)",
                &[&self.tenant, &actor],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }
    /// A person marks one of their current items as seen; the same request again returns the same row.
    pub async fn inbox_acknowledge(
        &self,
        actor: Uuid,
        kind: &str,
        reference: &str,
        version: &str,
        request: Uuid,
    ) -> StoreResult<Value> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let row = client
            .query_one(
                "SELECT enrichedlab_agent.broker_inbox_acknowledge($1,$2,$3,$4,$5,$6)",
                &[&self.tenant, &actor, &kind, &reference, &version, &request],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }
    /// Who saw each of a project's review requests, for anyone who may read the project.
    pub async fn project_acknowledgements(&self, actor: Uuid, project: Uuid) -> StoreResult<Value> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let row = client
            .query_one(
                "SELECT enrichedlab_agent.broker_project_acknowledgements($1,$2,$3)",
                &[&self.tenant, &actor, &project],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }
    pub async fn submit_project_task(
        &self,
        actor: Uuid,
        request: &SubmitTaskRequest,
        key: &str,
        digest: &str,
    ) -> StoreResult<Uuid> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let row=client.query_one("SELECT enrichedlab_agent.broker_submit_person_project_task($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
            &[&self.tenant,&actor,&request.project_id,&request.action_key,&request.parent_task_id,&request.group,&request.profile,&request.objective,&key,&digest]).await.map_err(database_error)?;
        Ok(row.get(0))
    }
}
