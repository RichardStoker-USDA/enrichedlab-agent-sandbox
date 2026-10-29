use super::*;
use serde_json::Value;

#[derive(Clone, Debug)]
pub struct CampaignClaim {
    pub campaign_id: Uuid,
    pub generation: i64,
    pub view: Value,
}

#[derive(Clone, Debug)]
pub struct CampaignPage {
    pub items: Vec<Value>,
    pub next_after: Option<i64>,
}

impl Store {
    pub async fn ready_for_campaign_api(&self) -> StoreResult<()> {
        self.campaign_readiness(&[
            "enrichedlab_agent.broker_create_campaign(text,uuid,uuid,text,text,text,jsonb,jsonb,jsonb)",
            "enrichedlab_agent.broker_start_campaign(text,uuid,uuid)",
            "enrichedlab_agent.broker_pause_campaign(text,uuid,uuid)",
            "enrichedlab_agent.broker_resume_campaign(text,uuid,uuid)",
            "enrichedlab_agent.broker_stop_campaign(text,uuid,uuid)",
            "enrichedlab_agent.broker_get_campaign(text,uuid,uuid)",
            "enrichedlab_agent.broker_list_campaigns(text,uuid,text,integer)",
            "enrichedlab_agent.broker_campaign_events_page(text,uuid,uuid,bigint,integer)",
            "enrichedlab_agent.broker_campaign_records_page(text,uuid,uuid,bigint,integer)",
            "enrichedlab_agent.broker_campaign_planner_calls_page(text,uuid,uuid,bigint,integer)",
            "enrichedlab_agent.broker_campaign_planner_text(text,uuid,uuid,text)",
        ])
        .await
    }

    pub async fn ready_for_campaign_director(&self) -> StoreResult<()> {
        let functions = [
            "enrichedlab_agent.broker_claim_campaign(text,text,integer)",
            "enrichedlab_agent.broker_renew_campaign(text,uuid,text,bigint,integer)",
            "enrichedlab_agent.broker_yield_campaign(text,uuid,text,bigint)",
            "enrichedlab_agent.broker_campaign_director_view(text,uuid,text,bigint)",
            "enrichedlab_agent.broker_campaign_finished_steps(text,uuid,text,bigint)",
            "enrichedlab_agent.broker_campaign_apply(text,uuid,text,bigint,jsonb)",
            "enrichedlab_agent.broker_start_campaign_step(text,uuid,text,bigint,uuid,uuid,text,text,text[],uuid,uuid)",
            "enrichedlab_agent.broker_start_campaign_planner_call(text,uuid,text,bigint,uuid,bytea,text)",
            "enrichedlab_agent.broker_finish_campaign_planner_call(text,uuid,text,bigint,uuid,bytea,text,text)",
        ];
        let client = self.pool.get().await.map_err(pool_error)?;
        let row = client
            .query_one(
                "SELECT enrichedlab_agent.broker_schema_ready(67), \
                        enrichedlab_agent.broker_campaign_director_ready(), \
                        NOT EXISTS ( \
                          SELECT 1 FROM unnest($1::text[]) signature \
                          WHERE NOT COALESCE(has_function_privilege( \
                            current_user,to_regprocedure(signature),'EXECUTE'),false))",
                &[&functions.as_slice()],
            )
            .await
            .map_err(database_error)?;
        if !row.get::<_, bool>(1) {
            return Err(StoreError {
                kind: StoreErrorKind::Unavailable,
                code: "campaign_director_role_unsafe",
            });
        }
        if row.get::<_, bool>(0) && row.get::<_, bool>(2) {
            Ok(())
        } else {
            Err(StoreError {
                kind: StoreErrorKind::Unavailable,
                code: "campaign_schema_not_ready",
            })
        }
    }

    async fn campaign_readiness(&self, functions: &[&str]) -> StoreResult<()> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let row = client
            .query_one(
                "SELECT enrichedlab_agent.broker_schema_ready(67) AND NOT EXISTS (\
                   SELECT 1 FROM unnest($1::text[]) signature \
                   WHERE NOT COALESCE(has_function_privilege(current_user,to_regprocedure(signature),'EXECUTE'),false))",
                &[&functions],
            )
            .await
            .map_err(database_error)?;
        if row.get(0) {
            Ok(())
        } else {
            Err(StoreError {
                kind: StoreErrorKind::Unavailable,
                code: "campaign_schema_not_ready",
            })
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_campaign(
        &self,
        actor: Uuid,
        campaign: Uuid,
        group: &str,
        goal: &str,
        constraints: &str,
        scope: &Value,
        budgets: &Value,
        stop_rules: &Value,
    ) -> StoreResult<Uuid> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let row = client
            .query_one(
                "SELECT enrichedlab_agent.broker_create_campaign($1,$2,$3,$4,$5,$6,$7,$8,$9)",
                &[
                    &self.tenant,
                    &actor,
                    &campaign,
                    &group,
                    &goal,
                    &constraints,
                    scope,
                    budgets,
                    stop_rules,
                ],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    pub async fn campaign(&self, actor: Uuid, campaign: Uuid) -> StoreResult<Value> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let row = client
            .query_one(
                "SELECT enrichedlab_agent.broker_get_campaign($1,$2,$3)",
                &[&self.tenant, &actor, &campaign],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    pub async fn campaigns(&self, actor: Uuid, group: &str, limit: i32) -> StoreResult<Vec<Value>> {
        let client = self.pool.get().await.map_err(pool_error)?;
        Ok(client
            .query(
                "SELECT enrichedlab_agent.broker_list_campaigns($1,$2,$3,$4)",
                &[&self.tenant, &actor, &group, &limit],
            )
            .await
            .map_err(database_error)?
            .into_iter()
            .map(|row| row.get(0))
            .collect())
    }

    pub async fn campaign_transition(
        &self,
        action: &str,
        actor: Uuid,
        campaign: Uuid,
        _reason: Option<&str>,
    ) -> StoreResult<Value> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let statement = match action {
            "start" => "SELECT enrichedlab_agent.broker_start_campaign($1,$2,$3)",
            "resume" => "SELECT enrichedlab_agent.broker_resume_campaign($1,$2,$3)",
            "pause" => "SELECT enrichedlab_agent.broker_pause_campaign($1,$2,$3)",
            "stop" => "SELECT enrichedlab_agent.broker_stop_campaign($1,$2,$3)",
            _ => {
                return Err(StoreError {
                    kind: StoreErrorKind::Invalid,
                    code: "campaign_transition_invalid",
                });
            }
        };
        let row = client
            .query_one(statement, &[&self.tenant, &actor, &campaign])
            .await
            .map_err(database_error)?;
        Ok(serde_json::json!({"state": row.get::<_, String>(0)}))
    }

    pub async fn campaign_events(
        &self,
        actor: Uuid,
        campaign: Uuid,
        after: i64,
        limit: i32,
    ) -> StoreResult<CampaignPage> {
        self.campaign_page("events", actor, campaign, after, limit)
            .await
    }

    pub async fn campaign_records(
        &self,
        actor: Uuid,
        campaign: Uuid,
        after: i64,
        limit: i32,
    ) -> StoreResult<CampaignPage> {
        self.campaign_page("records", actor, campaign, after, limit)
            .await
    }

    pub async fn campaign_planner_calls(
        &self,
        actor: Uuid,
        campaign: Uuid,
        after: i64,
        limit: i32,
    ) -> StoreResult<CampaignPage> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let rows = client
            .query(
                "SELECT enrichedlab_agent.broker_campaign_planner_calls_page($1,$2,$3,$4,$5)",
                &[&self.tenant, &actor, &campaign, &after, &limit],
            )
            .await
            .map_err(database_error)?;
        let items: Vec<Value> = rows.into_iter().map(|row| row.get(0)).collect();
        let next_after = if items.len() == limit as usize {
            items
                .last()
                .and_then(|value| value.get("sequence"))
                .and_then(Value::as_i64)
        } else {
            None
        };
        Ok(CampaignPage { items, next_after })
    }

    pub async fn campaign_planner_text(
        &self,
        actor: Uuid,
        campaign: Uuid,
        digest: &str,
    ) -> StoreResult<Value> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let row = client
            .query_one(
                "SELECT enrichedlab_agent.broker_campaign_planner_text($1,$2,$3,$4)",
                &[&self.tenant, &actor, &campaign, &digest],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    async fn campaign_page(
        &self,
        kind: &str,
        actor: Uuid,
        campaign: Uuid,
        after: i64,
        limit: i32,
    ) -> StoreResult<CampaignPage> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let function = if kind == "events" {
            "enrichedlab_agent.broker_campaign_events_page"
        } else {
            "enrichedlab_agent.broker_campaign_records_page"
        };
        let rows = client
            .query(
                &format!("SELECT {function}($1,$2,$3,$4,$5)"),
                &[&self.tenant, &actor, &campaign, &after, &limit],
            )
            .await
            .map_err(database_error)?;
        let items: Vec<Value> = rows.into_iter().map(|row| row.get(0)).collect();
        let next_after = if items.len() == limit as usize {
            items
                .last()
                .and_then(|value| value.get("sequence"))
                .and_then(Value::as_i64)
        } else {
            None
        };
        Ok(CampaignPage { items, next_after })
    }

    pub async fn claim_campaign(
        &self,
        worker: &str,
        seconds: i32,
    ) -> StoreResult<Option<CampaignClaim>> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let row = client
            .query_one(
                "SELECT enrichedlab_agent.broker_claim_campaign($1,$2,$3)",
                &[&self.tenant, &worker, &seconds],
            )
            .await
            .map_err(database_error)?;
        let Some(view) = row.get::<_, Option<Value>>(0) else {
            return Ok(None);
        };
        let campaign_id = value_uuid(&view, "campaign_id")?;
        let generation = view
            .get("generation")
            .and_then(Value::as_i64)
            .ok_or(StoreError {
                kind: StoreErrorKind::Internal,
                code: "campaign_view_invalid",
            })?;
        Ok(Some(CampaignClaim {
            campaign_id,
            generation,
            view,
        }))
    }

    pub async fn campaign_director_view(
        &self,
        campaign: Uuid,
        worker: &str,
        generation: i64,
    ) -> StoreResult<Value> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let row = client
            .query_one(
                "SELECT enrichedlab_agent.broker_campaign_director_view($1,$2,$3,$4)",
                &[&self.tenant, &campaign, &worker, &generation],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    pub async fn renew_campaign(
        &self,
        campaign: Uuid,
        worker: &str,
        generation: i64,
        seconds: i32,
    ) -> StoreResult<bool> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let row = client
            .query_one(
                "SELECT enrichedlab_agent.broker_renew_campaign($1,$2,$3,$4,$5)",
                &[&self.tenant, &campaign, &worker, &generation, &seconds],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    pub async fn campaign_finished_steps(
        &self,
        campaign: Uuid,
        worker: &str,
        generation: i64,
    ) -> StoreResult<Vec<Value>> {
        let client = self.pool.get().await.map_err(pool_error)?;
        Ok(client
            .query(
                "SELECT enrichedlab_agent.broker_campaign_finished_steps($1,$2,$3,$4)",
                &[&self.tenant, &campaign, &worker, &generation],
            )
            .await
            .map_err(database_error)?
            .into_iter()
            .map(|row| row.get(0))
            .collect())
    }

    pub async fn apply_campaign_change(
        &self,
        campaign: Uuid,
        worker: &str,
        generation: i64,
        change: &Value,
    ) -> StoreResult<Value> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let row = client
            .query_one(
                "SELECT enrichedlab_agent.broker_campaign_apply($1,$2,$3,$4,$5)",
                &[&self.tenant, &campaign, &worker, &generation, change],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    pub async fn start_campaign_planner_call(
        &self,
        campaign: Uuid,
        worker: &str,
        generation: i64,
        call: Uuid,
        context: &[u8],
        model: &str,
    ) -> StoreResult<Value> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let row = client
            .query_one(
                "SELECT enrichedlab_agent.broker_start_campaign_planner_call($1,$2,$3,$4,$5,$6,$7)",
                &[
                    &self.tenant,
                    &campaign,
                    &worker,
                    &generation,
                    &call,
                    &context,
                    &model,
                ],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn finish_campaign_planner_call(
        &self,
        campaign: Uuid,
        worker: &str,
        generation: i64,
        call: Uuid,
        reply: Option<&[u8]>,
        model: Option<&str>,
        outcome: &str,
    ) -> StoreResult<Value> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let row = client
            .query_one(
                "SELECT enrichedlab_agent.broker_finish_campaign_planner_call($1,$2,$3,$4,$5,$6,$7,$8)",
                &[&self.tenant, &campaign, &worker, &generation, &call, &reply, &model, &outcome],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// Returns `started=false` for a capacity refusal. Lease, authority, scope, and
    /// malformed-request failures remain `StoreError`s.
    #[allow(clippy::too_many_arguments)]
    pub async fn start_campaign_step(
        &self,
        campaign: Uuid,
        worker: &str,
        generation: i64,
        step: Uuid,
        branch: Uuid,
        purpose: &str,
        question: &str,
        profiles: &[String],
        project: Uuid,
        run: Uuid,
    ) -> StoreResult<Value> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let row = client.query_one(
            "SELECT enrichedlab_agent.broker_start_campaign_step($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
            &[&self.tenant, &campaign, &worker, &generation, &step, &branch, &purpose, &question, &profiles, &project, &run],
        ).await.map_err(database_error)?;
        Ok(row.get(0))
    }

    pub async fn yield_campaign(
        &self,
        campaign: Uuid,
        worker: &str,
        generation: i64,
    ) -> StoreResult<()> {
        let client = self.pool.get().await.map_err(pool_error)?;
        client
            .execute(
                "SELECT enrichedlab_agent.broker_yield_campaign($1,$2,$3,$4)",
                &[&self.tenant, &campaign, &worker, &generation],
            )
            .await
            .map_err(database_error)?;
        Ok(())
    }
}

fn value_uuid(value: &Value, field: &str) -> StoreResult<Uuid> {
    value
        .get(field)
        .and_then(Value::as_str)
        .and_then(|raw| raw.parse().ok())
        .ok_or(StoreError {
            kind: StoreErrorKind::Internal,
            code: "campaign_view_invalid",
        })
}
