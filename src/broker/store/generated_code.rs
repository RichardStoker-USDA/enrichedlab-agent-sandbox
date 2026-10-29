//! Model-written code in a reviewed template (migration 015). Each reader gets the
//! narrowest view: a digest-only reference for the worker, bytes for the custodian's
//! current lease, and the scoped record for a caller who can read the task.
use super::*;
use crate::broker::input_request::AdmittedCodeRef;
use serde::Deserialize;
use serde_json::Value;
use sha2::Digest;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CustodyCode {
    template_ref: String,
    path: String,
    size_bytes: u64,
    sha256: String,
    content: String,
}

fn record_invalid() -> StoreError {
    StoreError {
        kind: StoreErrorKind::Internal,
        code: "database_operation_failed",
    }
}

impl Store {
    pub async fn task_admitted_code(
        &self,
        task: Uuid,
        worker: &str,
        attempt: i32,
        generation: i64,
    ) -> StoreResult<Option<AdmittedCodeRef>> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_task_admitted_code($1,$2,$3,$4,$5)",
                &[&self.tenant, &task, &worker, &attempt, &generation],
            )
            .await
            .map_err(database_error)?;
        let Some(value) = row.get::<_, Option<Value>>(0) else {
            return Ok(None);
        };
        let reference: AdmittedCodeRef =
            serde_json::from_value(value).map_err(|_| record_invalid())?;
        reference.validate().map_err(|_| record_invalid())?;
        Ok(Some(reference))
    }

    /// The bytes are checked against the stored size and digest before return.
    pub async fn custody_admitted_code(
        &self,
        task: Uuid,
        worker: &str,
        attempt: i32,
        generation: i64,
    ) -> StoreResult<Option<(AdmittedCodeRef, String)>> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_custody_admitted_code($1,$2,$3,$4,$5)",
                &[&self.tenant, &task, &worker, &attempt, &generation],
            )
            .await
            .map_err(database_error)?;
        let Some(value) = row.get::<_, Option<Value>>(0) else {
            return Ok(None);
        };
        let code: CustodyCode = serde_json::from_value(value).map_err(|_| record_invalid())?;
        let reference = AdmittedCodeRef {
            template_ref: code.template_ref,
            path: code.path,
            size_bytes: code.size_bytes,
            sha256: code.sha256,
        };
        reference.validate().map_err(|_| record_invalid())?;
        let digest = format!(
            "sha256:{}",
            hex::encode(sha2::Sha256::digest(code.content.as_bytes()))
        );
        if code.content.len() as u64 != reference.size_bytes || digest != reference.sha256 {
            return Err(record_invalid());
        }
        Ok(Some((reference, code.content)))
    }

    pub async fn task_generated_code(&self, actor: Uuid, task: Uuid) -> StoreResult<Option<Value>> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_task_generated_code($1,$2,$3)",
                &[&self.tenant, &actor, &task],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }
}
