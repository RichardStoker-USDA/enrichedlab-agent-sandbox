//! Figure previews and the manuscript mark (migration 057). A person asks for a preview
//! through the API; the worker that reaches the render service claims it, draws it and records what it came to; the API
//! hands the drawing over once. A person who may steer a project marks the version of a figure that is in the
//! manuscript.
use super::*;
use serde_json::Value;

/// What a claimed preview came to: `drawn` with the SVG, or `refused` or `failed` with a result only.
#[derive(Clone, Debug)]
pub struct PreviewOutcome {
    pub state: &'static str,
    pub svg: Option<Vec<u8>>,
    pub result: Value,
}

impl Store {
    /// The API serves previews and marks only with migration 057 and its grants; a worker draws previews only with
    /// its claim, finish and sweep functions.
    pub async fn ready_for_figure_previews(&self, api: bool) -> StoreResult<()> {
        let functions: &[&str] = if api {
            &[
                "enrichedlab_agent.broker_request_figure_preview(text,uuid,uuid,uuid,text,text,jsonb,text)",
                "enrichedlab_agent.broker_take_figure_preview(text,uuid,uuid)",
                "enrichedlab_agent.broker_abandon_figure_preview(text,uuid,uuid)",
                "enrichedlab_agent.broker_mark_figure_manuscript(text,uuid,uuid,uuid,uuid)",
                "enrichedlab_agent.broker_clear_figure_manuscript(text,uuid,uuid,uuid)",
            ]
        } else {
            &[
                "enrichedlab_agent.broker_claim_figure_preview(text,text)",
                "enrichedlab_agent.broker_finish_figure_preview(text,text,uuid,text,bytea,jsonb)",
                "enrichedlab_agent.broker_sweep_figure_previews(text)",
            ]
        };
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_schema_ready(57) AND bool_and(COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(f), 'EXECUTE'), false)) \
                 FROM unnest($1::text[]) AS f",
                &[&functions],
            )
            .await
            .map_err(database_error)?;
        if row.get::<_, Option<bool>>(0) == Some(true) {
            Ok(())
        } else {
            Err(StoreError {
                kind: StoreErrorKind::Unavailable,
                code: "broker_schema_not_ready",
            })
        }
    }

    /// Records a preview request after the checks a version request passes; returns the preview's id.
    pub async fn request_figure_preview(
        &self,
        actor: Uuid,
        request: &FigureRequest<'_>,
    ) -> StoreResult<Uuid> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_request_figure_preview($1,$2,$3,$4,$5,$6,$7,$8)",
                &[
                    &self.tenant,
                    &actor,
                    &request.project,
                    &request.source_task,
                    &request.table_path,
                    &request.feature,
                    request.controls,
                    &request.title,
                ],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// The requester's preview: `{"state": "waiting" | "expired"}`, or what it came to, handed over once.
    pub async fn take_figure_preview(&self, actor: Uuid, preview: Uuid) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_take_figure_preview($1,$2,$3)",
                &[&self.tenant, &actor, &preview],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// Ends the requester's preview when its request ends before the drawing is handed over; true when it was still
    /// waiting, being drawn or drawn.
    pub async fn abandon_figure_preview(&self, actor: Uuid, preview: Uuid) -> StoreResult<bool> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_abandon_figure_preview($1,$2,$3)",
                &[&self.tenant, &actor, &preview],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// The oldest waiting preview, claimed by this worker, as a figure request.
    pub async fn claim_figure_preview(
        &self,
        worker: &str,
    ) -> StoreResult<Option<(Uuid, TaskFigure)>> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_claim_figure_preview($1,$2)",
                &[&self.tenant, &worker],
            )
            .await
            .map_err(database_error)?;
        let Some(value) = row.get::<_, Option<Value>>(0) else {
            return Ok(None);
        };
        let invalid = || StoreError {
            kind: StoreErrorKind::Internal,
            code: "database_operation_failed",
        };
        let preview = value["preview_id"]
            .as_str()
            .and_then(|id| id.parse().ok())
            .ok_or_else(invalid)?;
        let figure = serde_json::from_value(value["figure"].clone()).map_err(|_| invalid())?;
        Ok(Some((preview, figure)))
    }

    /// Records what a claimed preview came to. False when this worker no longer holds it.
    pub async fn finish_figure_preview(
        &self,
        worker: &str,
        preview: Uuid,
        outcome: &PreviewOutcome,
    ) -> StoreResult<bool> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_finish_figure_preview($1,$2,$3,$4,$5,$6)",
                &[
                    &self.tenant,
                    &worker,
                    &preview,
                    &outcome.state,
                    &outcome.svg,
                    &outcome.result,
                ],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// Ends previews past their expiry and deletes those made more than two minutes ago; returns how many it deleted.
    pub async fn sweep_figure_previews(&self) -> StoreResult<i32> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_sweep_figure_previews($1)",
                &[&self.tenant],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// Marks one completed version of a figure as the one in the manuscript; returns the mark.
    pub async fn mark_figure_manuscript(
        &self,
        actor: Uuid,
        project: Uuid,
        figure: Uuid,
        version: Uuid,
    ) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_mark_figure_manuscript($1,$2,$3,$4,$5)",
                &[&self.tenant, &actor, &project, &figure, &version],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// Clears a figure's mark; false when nothing was marked.
    pub async fn clear_figure_manuscript(
        &self,
        actor: Uuid,
        project: Uuid,
        figure: Uuid,
    ) -> StoreResult<bool> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_clear_figure_manuscript($1,$2,$3,$4)",
                &[&self.tenant, &actor, &project, &figure],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }
}
