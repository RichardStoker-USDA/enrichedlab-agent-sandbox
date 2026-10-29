//! Lab library (migration 028): the papers a lab keeps in its document-service
//! corpus, who added them, removals and typed labels. Every function requires membership of the lab.
use super::*;
use serde_json::Value;

/// A paper the API has just placed in the lab's corpus, with the catalog fields read from its parse.
pub struct LibraryPaperRecord {
    pub document_id: Uuid,
    pub source_sha256: String,
    pub byte_size: i64,
    pub title: Option<String>,
    pub authors: Value,
    pub year: Option<i32>,
    pub doi: Option<String>,
    pub page_count: Option<i32>,
    pub parse: Value,
}

impl Store {
    /// The lab's papers still in the library, newest first, the caller's role and how many were removed.
    pub async fn library(&self, actor: Uuid, lab: &str) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_library($1,$2,$3)",
                &[&self.tenant, &actor, &lab],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// One paper of the lab, removed or not, and its document identifier for the API's own use.
    pub async fn library_paper(
        &self,
        actor: Uuid,
        lab: &str,
        paper: Uuid,
    ) -> StoreResult<(Value, Uuid)> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT paper, document_id FROM enrichedlab_agent.broker_library_paper($1,$2,$3,$4)",
                &[&self.tenant, &actor, &lab, &paper],
            )
            .await
            .map_err(database_error)?;
        Ok((row.get(0), row.get(1)))
    }

    /// The lab's paper with these source bytes, removed or not.
    pub async fn library_find(
        &self,
        actor: Uuid,
        lab: &str,
        sha256: &str,
    ) -> StoreResult<Option<(Value, Uuid)>> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_opt(
                "SELECT paper, document_id FROM enrichedlab_agent.broker_library_find($1,$2,$3,$4)",
                &[&self.tenant, &actor, &lab, &sha256],
            )
            .await
            .map_err(database_error)?;
        Ok(row.map(|row| (row.get(0), row.get(1))))
    }

    /// Records a paper in the lab's library, or returns the lab's paper with the same bytes,
    /// restoring it when it had been removed. The same request again returns the same paper.
    pub async fn library_record(
        &self,
        actor: Uuid,
        lab: &str,
        paper: &LibraryPaperRecord,
        request: Uuid,
    ) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_library_record($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
                &[
                    &self.tenant,
                    &actor,
                    &lab,
                    &paper.document_id,
                    &paper.source_sha256,
                    &paper.byte_size,
                    &paper.title,
                    &paper.authors,
                    &paper.year,
                    &paper.doi,
                    &paper.page_count,
                    &paper.parse,
                    &request,
                ],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// Removes a paper from the lab's library with a reason: a lab lead or whoever added it.
    pub async fn library_remove(
        &self,
        actor: Uuid,
        lab: &str,
        paper: Uuid,
        reason: &str,
        request: Uuid,
    ) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_library_remove($1,$2,$3,$4,$5,$6)",
                &[&self.tenant, &actor, &lab, &paper, &reason, &request],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// Accepts a paper for the research lead (`accept`), or withdraws that acceptance with a reason:
    /// a lab curator, a person who leads or reviews in the lab (migration 034).
    pub async fn library_accept(
        &self,
        actor: Uuid,
        lab: &str,
        paper: Uuid,
        accept: bool,
        reason: Option<&str>,
        request: Uuid,
    ) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_library_accept($1,$2,$3,$4,$5,$6,$7)",
                &[
                    &self.tenant,
                    &actor,
                    &lab,
                    &paper,
                    &accept,
                    &reason,
                    &request,
                ],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// Adds (`added`) or removes (`removed`) a typed label on a paper in the library.
    #[allow(clippy::too_many_arguments)]
    pub async fn library_label(
        &self,
        actor: Uuid,
        lab: &str,
        paper: Uuid,
        kind: &str,
        namespace: &str,
        value: &str,
        request: Uuid,
    ) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_library_label($1,$2,$3,$4,$5,$6,$7,$8)",
                &[
                    &self.tenant,
                    &actor,
                    &lab,
                    &paper,
                    &kind,
                    &namespace,
                    &value,
                    &request,
                ],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// The lab's papers still in the library among `documents`, for search results.
    pub async fn library_resolve(
        &self,
        actor: Uuid,
        lab: &str,
        documents: &[Uuid],
    ) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_library_resolve($1,$2,$3,$4)",
                &[&self.tenant, &actor, &lab, &documents],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    /// For the coordinator holding `run` (migration 029): whether the run's starter
    /// is a member of the project's lab, and which of `documents` are papers the lab's library holds
    /// and has not removed, each with its paper id, title, year and whether it is accepted (034).
    pub async fn research_run_library(
        &self,
        run: Uuid,
        worker: &str,
        generation: i64,
        documents: &[Uuid],
    ) -> StoreResult<Value> {
        let c = self.pool.get().await.map_err(pool_error)?;
        let row = c
            .query_one(
                "SELECT enrichedlab_agent.broker_research_run_library($1,$2,$3,$4,$5)",
                &[&self.tenant, &run, &worker, &generation, &documents],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }
}
