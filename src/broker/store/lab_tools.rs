//! A lab's tools (migration 080): for a member of the lab, each enabled method the lab may
//! run, how it is granted (directly or through tool groups), its kind and catalog fields, and the tool groups the lab
//! holds. Catalog fields only; what may run is still decided at proposal and in the worker.
use super::*;
use serde_json::Value;

impl Store {
    /// The tools of `lab` for `actor`, who must belong to it (`lab_not_found` otherwise).
    pub async fn lab_tools(&self, actor: Uuid, lab: &str) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_lab_tools($1,$2,$3)",
                &[&self.tenant, &actor, &lab],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }
}
