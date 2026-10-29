//! Live output (migration 056): the worker holding a task's lease appends sanitized output pieces
//! and closes each attempt with a digest the database recomputes; the API reads pieces after a cursor.
use super::*;
use serde_json::Value;

/// One piece as the worker records it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct OutputPiece {
    pub stream: &'static str,
    pub text: String,
    pub bytes: u32,
    pub dropped: u32,
    /// RFC 3339.
    pub at: String,
}

/// What an attempt stored, for its closure.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutputTotals {
    pub pieces: i32,
    pub size: i64,
    pub sha256: String,
    pub dropped: i64,
}

impl Store {
    /// The API needs migration 056 and its read function to serve live output; a worker that records it needs the
    /// two writing functions.
    pub async fn ready_for_live_output(&self, api: bool) -> StoreResult<()> {
        let functions: &[&str] = if api {
            &["enrichedlab_agent.broker_task_output_after(text,uuid,uuid,bigint,integer)"]
        } else {
            &[
                "enrichedlab_agent.broker_record_task_output(text,uuid,text,bigint,integer,jsonb)",
                "enrichedlab_agent.broker_close_task_output(text,uuid,text,bigint,integer,bigint,text,bigint)",
            ]
        };
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_schema_ready(56) AND bool_and(COALESCE(\
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

    /// Appends pieces to the leased attempt after the `first` it already holds; the same batch again is accepted.
    /// Returns the last piece's number.
    pub async fn record_task_output(
        &self,
        task: Uuid,
        worker: &str,
        generation: i64,
        first: i32,
        pieces: &[OutputPiece],
    ) -> StoreResult<i64> {
        let pieces = serde_json::to_value(pieces).map_err(|_| StoreError {
            kind: StoreErrorKind::Invalid,
            code: "task_output_invalid",
        })?;
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_record_task_output($1,$2,$3,$4,$5,$6)",
                &[&self.tenant, &task, &worker, &generation, &first, &pieces],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// Closes the leased attempt with what it stored. Returns the stored text's SHA-256.
    pub async fn close_task_output(
        &self,
        task: Uuid,
        worker: &str,
        generation: i64,
        totals: &OutputTotals,
    ) -> StoreResult<String> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_close_task_output($1,$2,$3,$4,$5,$6,$7,$8)",
                &[
                    &self.tenant,
                    &task,
                    &worker,
                    &generation,
                    &totals.pieces,
                    &totals.size,
                    &totals.sha256,
                    &totals.dropped,
                ],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// A reader's pieces after `after`, with the task's status and attempt, the attempt's closure and the saved
    /// output files. A task the principal cannot read is not found.
    pub async fn task_output_after(
        &self,
        principal: Uuid,
        task: Uuid,
        after: i64,
        limit: i32,
    ) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        c.query_opt(
            "SELECT enrichedlab_agent.broker_task_output_after($1,$2,$3,$4,$5)",
            &[&self.tenant, &principal, &task, &after, &limit],
        )
        .await
        .map_err(database_error)?
        .and_then(|row| row.get::<_, Option<Value>>(0))
        .ok_or(StoreError {
            kind: StoreErrorKind::NotFound,
            code: "task_not_found",
        })
    }
}
