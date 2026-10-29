use std::{collections::HashSet, fmt, fs, str::FromStr};

use anyhow::{Context, Result, bail};
use deadpool_postgres::{Manager, Pool, Runtime};
use native_tls::TlsConnector;
use postgres_native_tls::MakeTlsConnector;
use tokio_postgres::{Config as PostgresConfig, NoTls, Row, Transaction, error::SqlState};
use uuid::Uuid;

mod analyses;
mod campaigns;
mod changes;
mod datasets;
mod digests;
mod documents;
mod figure_previews;
mod figures;
mod kinds;
mod live_output;
pub use analyses::{AnalysisReceipt, AnalysisRequest, TaskAnalysis};
pub use campaigns::{CampaignClaim, CampaignPage};
pub use changes::{ChangeCursor, ChangeListener, ChangeRead};
pub use datasets::{DatasetReceipt, DatasetRecord, PreparationRequest, TaskDataset};
pub use documents::{
    DocumentFigure, DocumentReceipt, DocumentSource, RetainedPackage, TaskDocument,
};
pub use figure_previews::PreviewOutcome;
pub use figures::{FigureReceipt, FigureRequest, TaskFigure};
pub use kinds::{AwaitingKind, KindRecord, KindRecordView};
pub use live_output::{OutputPiece, OutputTotals};
mod generated_code;
mod lab_notes;
mod lab_tools;
mod library;
pub use library::LibraryPaperRecord;
mod lineage;
mod queries;
pub use queries::{QueryReceipt, TaskQuery};
mod literature_reviews;
mod orientations;
mod projects;
mod research_runs;
mod review;
mod task_reviews;

pub use lineage::AttemptInput;

use super::{
    config::{MigrationArgs, SeedArgs, migration_dsn},
    model::{
        AdministrationView, ArtifactView, BrokerCatalog, ClaimedTask, GroupView, MeView,
        PrincipalView, ProfileView, ScientificHistoryEventView, ScientificHistoryEvidencePage,
        ScientificHistoryEvidenceView, ScientificHistoryPage, TaskEventView, TaskView,
        VerifiedIdentity,
    },
    platform_tool::AcceptedPlatformJob,
    runtime_events::{RuntimeEvent, RuntimeEventRecorder},
};

const MIGRATION_001: &str = include_str!("../../migrations/postgres/001_provenance.sql");
const MIGRATION_002: &str = include_str!("../../migrations/postgres/002_broker.sql");
const MIGRATION_003: &str = include_str!("../../migrations/postgres/003_runtime_events.sql");
const MIGRATION_004: &str = include_str!("../../migrations/postgres/004_broker_readiness.sql");
const MIGRATION_005: &str = include_str!("../../migrations/postgres/005_broker_custody_task.sql");
const MIGRATION_006: &str =
    include_str!("../../migrations/postgres/006_broker_terminal_custody_task.sql");
const MIGRATION_007: &str =
    include_str!("../../migrations/postgres/007_broker_scientific_history.sql");
const MIGRATION_008: &str =
    include_str!("../../migrations/postgres/008_broker_platform_tool_job.sql");
const MIGRATION_009: &str =
    include_str!("../../migrations/postgres/009_broker_scientific_history_evidence.sql");
const MIGRATION_010: &str = include_str!("../../migrations/postgres/010_research_projects.sql");
const MIGRATION_011: &str = include_str!("../../migrations/postgres/011_research_runs.sql");
const MIGRATION_012: &str =
    include_str!("../../migrations/postgres/012_broker_identity_enrollment.sql");
const MIGRATION_013: &str =
    include_str!("../../migrations/postgres/013_broker_artifact_recovery.sql");
const MIGRATION_014: &str = include_str!("../../migrations/postgres/014_research_review.sql");
const MIGRATION_015: &str = include_str!("../../migrations/postgres/015_generated_code.sql");
const MIGRATION_016: &str = include_str!("../../migrations/postgres/016_task_lineage.sql");
const MIGRATION_017: &str =
    include_str!("../../migrations/postgres/017_task_lineage_template_class.sql");
const MIGRATION_018: &str = include_str!("../../migrations/postgres/018_task_lineage_history.sql");
const MIGRATION_019: &str = include_str!("../../migrations/postgres/019_attempt_lineage.sql");
const MIGRATION_020: &str = include_str!("../../migrations/postgres/020_project_list_summary.sql");
const MIGRATION_021: &str = include_str!("../../migrations/postgres/021_result_digests.sql");
const MIGRATION_022: &str = include_str!("../../migrations/postgres/022_configured_methods.sql");
const MIGRATION_023: &str = include_str!("../../migrations/postgres/023_person_review.sql");
const MIGRATION_024: &str =
    include_str!("../../migrations/postgres/024_person_review_people_only.sql");
const MIGRATION_025: &str = include_str!("../../migrations/postgres/025_lead_reads_reviews.sql");
const MIGRATION_026: &str = include_str!("../../migrations/postgres/026_project_list_reviews.sql");
const MIGRATION_027: &str = include_str!("../../migrations/postgres/027_lab_notes.sql");
const MIGRATION_028: &str = include_str!("../../migrations/postgres/028_lab_library.sql");
const MIGRATION_029: &str = include_str!("../../migrations/postgres/029_lead_reads_library.sql");
const MIGRATION_030: &str = include_str!("../../migrations/postgres/030_principal_class.sql");
const MIGRATION_031: &str = include_str!("../../migrations/postgres/031_current_answer.sql");
const MIGRATION_032: &str = include_str!("../../migrations/postgres/032_decision_contexts.sql");
const MIGRATION_033: &str = include_str!("../../migrations/postgres/033_working_copies.sql");
const MIGRATION_034: &str = include_str!("../../migrations/postgres/034_paper_acceptance.sql");
const MIGRATION_035: &str = include_str!("../../migrations/postgres/035_digest_backfill.sql");
const MIGRATION_036: &str = include_str!("../../migrations/postgres/036_plan_ledger.sql");
const MIGRATION_037: &str = include_str!("../../migrations/postgres/037_inbox.sql");
const MIGRATION_038: &str = include_str!("../../migrations/postgres/038_reference_sql.sql");
const MIGRATION_039: &str = include_str!("../../migrations/postgres/039_input_classes.sql");
const MIGRATION_040: &str = include_str!("../../migrations/postgres/040_figure_versions.sql");
const MIGRATION_041: &str = include_str!("../../migrations/postgres/041_figure_lineage.sql");
const MIGRATION_042: &str = include_str!("../../migrations/postgres/042_prior_queries.sql");
const MIGRATION_043: &str = include_str!("../../migrations/postgres/043_own_queries.sql");
const MIGRATION_044: &str = include_str!("../../migrations/postgres/044_lead_replies.sql");
const MIGRATION_045: &str = include_str!("../../migrations/postgres/045_answerability_context.sql");
const MIGRATION_046: &str = include_str!("../../migrations/postgres/046_fair_claim.sql");
const MIGRATION_047: &str = include_str!("../../migrations/postgres/047_source_cards.sql");
const MIGRATION_048: &str = include_str!("../../migrations/postgres/048_plan_pages.sql");
const MIGRATION_049: &str = include_str!("../../migrations/postgres/049_methods_revision.sql");
const MIGRATION_050: &str = include_str!("../../migrations/postgres/050_specialist_texts.sql");
const MIGRATION_051: &str = include_str!("../../migrations/postgres/051_query_facts.sql");
const MIGRATION_052: &str = include_str!("../../migrations/postgres/052_lab_methods.sql");
const MIGRATION_053: &str = include_str!("../../migrations/postgres/053_lab_sources.sql");
const MIGRATION_054: &str = include_str!("../../migrations/postgres/054_workspace_reads.sql");
const MIGRATION_055: &str = include_str!("../../migrations/postgres/055_result_kinds.sql");
const MIGRATION_056: &str = include_str!("../../migrations/postgres/056_live_output.sql");
const MIGRATION_057: &str =
    include_str!("../../migrations/postgres/057_figure_previews_and_marks.sql");
const MIGRATION_058: &str = include_str!("../../migrations/postgres/058_template_analyses.sql");
const MIGRATION_059: &str = include_str!("../../migrations/postgres/059_lab_datasets.sql");
const MIGRATION_060: &str =
    include_str!("../../migrations/postgres/060_lead_template_analyses.sql");
const MIGRATION_061: &str = include_str!("../../migrations/postgres/061_change_feed.sql");
const MIGRATION_062: &str = include_str!("../../migrations/postgres/062_lab_source_relations.sql");
const MIGRATION_063: &str = include_str!("../../migrations/postgres/063_methods_refused_texts.sql");
const MIGRATION_064: &str = include_str!("../../migrations/postgres/064_answer_tables.sql");
const MIGRATION_065: &str = include_str!("../../migrations/postgres/065_task_lineage_upstream.sql");
const MIGRATION_066: &str = include_str!("../../migrations/postgres/066_campaigns.sql");
const MIGRATION_067: &str = include_str!("../../migrations/postgres/067_campaign_planner.sql");
const MIGRATION_068: &str =
    include_str!("../../migrations/postgres/068_project_package_revisions.sql");
const MIGRATION_069: &str = include_str!("../../migrations/postgres/069_reading_contexts.sql");
const MIGRATION_070: &str = include_str!("../../migrations/postgres/070_literature_reviews.sql");

const MIGRATION_071: &str = include_str!("../../migrations/postgres/071_reviewed_queries.sql");
const MIGRATION_072: &str = include_str!("../../migrations/postgres/072_answer_documents.sql");
const MIGRATION_073: &str = include_str!("../../migrations/postgres/073_step_checks.sql");
const MIGRATION_074: &str = include_str!("../../migrations/postgres/074_orientation.sql");
const MIGRATION_075: &str = include_str!("../../migrations/postgres/075_evidence_record.sql");
const MIGRATION_076: &str = include_str!("../../migrations/postgres/076_evidence_lineage.sql");
const MIGRATION_077: &str = include_str!("../../migrations/postgres/077_evidence_adapter.sql");
const MIGRATION_078: &str =
    include_str!("../../migrations/postgres/078_evidence_history_import.sql");
const MIGRATION_079: &str = include_str!("../../migrations/postgres/079_evidence_custody.sql");
const MIGRATION_080: &str = include_str!("../../migrations/postgres/080_tool_groups.sql");
const MIGRATION_081: &str = include_str!("../../migrations/postgres/081_evidence_live_capture.sql");
const MIGRATION_082: &str = include_str!("../../migrations/postgres/082_card_freshness.sql");

#[derive(Clone)]
pub struct Store {
    pool: Pool,
    tenant: String,
    /// How to open the one connection outside the pool that listens for changes (migration 061).
    listen: std::sync::Arc<ListenConfig>,
}

struct ListenConfig {
    config: PostgresConfig,
    tls: Option<MakeTlsConnector>,
}

pub struct WorkerLeaderGuard {
    _connection: deadpool_postgres::Client,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreErrorKind {
    Invalid,
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict,
    Quota,
    LeaseLost,
    Unavailable,
    Internal,
}

#[derive(Debug)]
pub struct StoreError {
    pub kind: StoreErrorKind,
    pub code: &'static str,
}

#[derive(Clone, Debug)]
pub struct ScientificHistoryEvidenceInput<'a> {
    pub event_id: Uuid,
    pub event_sequence: i64,
    pub event_hash: &'a str,
    pub snapshot_event_id: Uuid,
    pub artifact_id: Uuid,
    pub artifact_sha256: &'a str,
    pub artifact_size_bytes: i64,
    pub artifact_role: &'a str,
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code)
    }
}

impl std::error::Error for StoreError {}

type StoreResult<T> = std::result::Result<T, StoreError>;

impl Store {
    pub fn connect(dsn: &str, tenant: String, max_size: usize) -> Result<Self> {
        let pool = build_pool(dsn, max_size)?;
        let (config, tls) = postgres_config(dsn)?;
        Ok(Self {
            pool,
            tenant,
            listen: std::sync::Arc::new(ListenConfig { config, tls }),
        })
    }

    pub fn tenant(&self) -> &str {
        &self.tenant
    }

    pub async fn ready(&self) -> StoreResult<()> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let row = client
            // Every worker path may write a result digest with column summaries, whose smallest and largest values only
            // migration 073's reader rule keeps from readers that are not persons.
            .query_one(
                "SELECT enrichedlab_agent.broker_schema_ready(11) AND enrichedlab_agent.broker_schema_ready(73)",
                &[],
            )
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

    /// The API's review routes need migration 014 and the API role's grant; its task
    /// submissions and lineage reads need migration 016, the as-submitted lineage 018, per-attempt lineage 019,
    /// result digests 021, configured methods 022, a person's review 023 and 024, the index's review counts 026,
    /// lab memory 027, the lab library 028, the principal class 030, the kept decision texts 032, working copies 033,
    /// paper acceptance 034, the plan ledger 036, what waits for each person 037, guarded queries 038, figure versions 040, the
    /// literature specialist's kept texts 050, upstream lineage 065, digests whose column summaries lose a lab's
    /// values for a reader that is not a person (073) and the lab's tools (080). A missing grant keeps a new API
    /// replica out of service instead of failing scientist requests at runtime. Workers
    /// keep the generic check above.
    pub async fn ready_for_review_api(&self) -> StoreResult<()> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let row = client
            .query_one(
                "SELECT enrichedlab_agent.broker_schema_ready(19) \
                 AND enrichedlab_agent.broker_schema_ready(21) \
                 AND enrichedlab_agent.broker_schema_ready(22) \
                 AND enrichedlab_agent.broker_schema_ready(23) \
                 AND enrichedlab_agent.broker_schema_ready(24) \
                 AND enrichedlab_agent.broker_schema_ready(26) \
                 AND enrichedlab_agent.broker_schema_ready(27) \
                 AND enrichedlab_agent.broker_schema_ready(28) \
                 AND enrichedlab_agent.broker_schema_ready(30) \
                 AND enrichedlab_agent.broker_schema_ready(32) \
                 AND enrichedlab_agent.broker_schema_ready(33) \
                 AND enrichedlab_agent.broker_schema_ready(34) \
                 AND enrichedlab_agent.broker_schema_ready(36) \
                 AND enrichedlab_agent.broker_schema_ready(37) \
                 AND enrichedlab_agent.broker_schema_ready(71) AND enrichedlab_agent.broker_schema_ready(38) \
                 AND enrichedlab_agent.broker_schema_ready(40) \
                 AND enrichedlab_agent.broker_schema_ready(50) \
                 AND enrichedlab_agent.broker_schema_ready(52) \
                 AND enrichedlab_agent.broker_schema_ready(54) \
                 AND enrichedlab_agent.broker_schema_ready(65) \
                 AND enrichedlab_agent.broker_schema_ready(68) \
                 AND enrichedlab_agent.broker_schema_ready(72) \
                 AND enrichedlab_agent.broker_schema_ready(73) \
                 AND enrichedlab_agent.broker_schema_ready(80) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_lab_tools(text,uuid,text)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_read_project(text,uuid,uuid)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_request_document(text,uuid,uuid,uuid,text,uuid)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_task_document_detail(text,uuid,uuid)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_run_documents(text,uuid,uuid)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_project_look(text,uuid,uuid,bigint)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_project_analyses(text,uuid,uuid,text,integer)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_project_history_after(text,uuid,uuid,bigint,integer)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_request_figure(text,uuid,uuid,uuid,text,text,jsonb,text,uuid,uuid)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_task_figure_detail(text,uuid,uuid)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_figure_versions(text,uuid,uuid)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_render_catalog(text,uuid)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_list_profile_choices(text,uuid)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_get_group_methods(text,uuid)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_task_query_detail(text,uuid,uuid)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_inbox(text,uuid)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_inbox_acknowledge(text,uuid,text,text,text,uuid)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_project_acknowledgements(text,uuid,uuid)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_project_plan_ledger(text,uuid,uuid)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_library_accept(text,uuid,text,uuid,boolean,text,uuid)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_resolve_principal(text,text,text,text,text,text)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_project_working_copies(text,uuid,uuid)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_research_context(text,uuid,uuid,text)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_library(text,uuid,text)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_library_record(text,uuid,text,uuid,text,bigint,text,jsonb,integer,text,integer,jsonb,uuid)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_library_resolve(text,uuid,text,uuid[])'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_record_task_review(text,uuid,uuid,text,text,uuid)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_share_lab_note(text,uuid,uuid,bigint,text,jsonb,text,integer,uuid)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_retire_lab_note(text,uuid,uuid,text,uuid)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_renew_lab_note(text,uuid,uuid,integer,uuid)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_lab_notes(text,uuid,text)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_project_reviews(text,uuid,uuid)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_project_review_records(text,uuid,uuid)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_get_artifact_digest(text,uuid,uuid,uuid)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_project_package_snapshot(text,uuid,uuid)'),\
                 'EXECUTE'), false) AND COALESCE(has_function_privilege(current_user, \
                 to_regprocedure('enrichedlab_agent.broker_project_package_components(text,uuid,uuid)'),\
                 'EXECUTE'), false) AND COALESCE(has_function_privilege(current_user, \
                  to_regprocedure('enrichedlab_agent.broker_register_project_package(text,uuid,uuid,text,text,jsonb)'),\
                 'EXECUTE'), false) AND COALESCE(has_function_privilege(current_user, \
                 to_regprocedure('enrichedlab_agent.broker_project_generated_code(text,uuid,uuid)'),\
                 'EXECUTE'), false) AND COALESCE(has_function_privilege(current_user, \
                 to_regprocedure('enrichedlab_agent.broker_task_lineage(text,uuid,uuid)'),\
                 'EXECUTE'), false) AND COALESCE(has_function_privilege(current_user, \
                 to_regprocedure('enrichedlab_agent.broker_task_lineage_v2(text,uuid,uuid)'),\
                 'EXECUTE'), false) AND COALESCE(has_function_privilege(current_user, \
                 to_regprocedure('enrichedlab_agent.broker_task_lineage_v3(text,uuid,uuid)'),\
                 'EXECUTE'), false) AND COALESCE(has_function_privilege(current_user, \
                 to_regprocedure('enrichedlab_agent.broker_submit_person_task(text,uuid,text,text,text,text,text)'),\
                 'EXECUTE'), false) AND COALESCE(has_function_privilege(current_user, \
                 to_regprocedure('enrichedlab_agent.broker_submit_person_project_task(text,uuid,uuid,text,uuid,text,text,text,text,text)'),\
                 'EXECUTE'), false)",
                &[],
            )
            .await
            .map_err(database_error)?;
        if row.get::<_, bool>(0) {
            Ok(())
        } else {
            Err(StoreError {
                kind: StoreErrorKind::Unavailable,
                code: "broker_review_schema_not_ready",
            })
        }
    }

    /// The coordinator's context version 8 reads result digests (migration 021), people's reviews
    /// (025), the lab's shared notes (027) and which searched documents are papers in the run's lab
    /// library (029) through its own grants, names the newest answer (031) and keeps each decision's
    /// exact texts (032), whether each paper is accepted (034), the sources the run may query (038) and the admitted
    /// queries of earlier analyses its knowledge search found (042) and of its own project's analyses (043), and each
    /// lead reply, a refused one named by its deferral (044), and the run's newest answerability ledger and refused
    /// replies (045); a configured method's block (022) and an admitted query (038) are checked again at commit. It
    /// claims runs fairly by research group, then person (046), moves finished steps out of a full working plan into
    /// archive pages while its claim names each analysis's method (048), commits a methods review's request for a
    /// revision within the run's budget (049), keeps the literature specialist's texts where a lab member can read
    /// them (050), reads a revision's query facts back for the lead (051), and reads the run's template-analysis method
    /// with the newest analysis catalog, whose analyses the commit checks again (060), and names a refused reply's
    /// reasoning in its deferral (061), and reads each digest's column summaries without a lab's values for its step
    /// checks (073), and keeps a run's orientation and counts its faults (074), and reads each source's card and data
    /// versions to say when a card is behind its data (082). A coordinator without them must not claim runs.
    pub async fn ready_for_research_results(&self) -> StoreResult<()> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let row = client
            .query_one(
                "SELECT enrichedlab_agent.broker_schema_ready(82) \
                 AND enrichedlab_agent.broker_schema_ready(74) \
                 AND enrichedlab_agent.broker_schema_ready(73) \
                 AND enrichedlab_agent.broker_schema_ready(21) \
                 AND enrichedlab_agent.broker_schema_ready(22) \
                 AND enrichedlab_agent.broker_schema_ready(25) \
                 AND enrichedlab_agent.broker_schema_ready(27) \
                 AND enrichedlab_agent.broker_schema_ready(29) \
                 AND enrichedlab_agent.broker_schema_ready(31) \
                 AND enrichedlab_agent.broker_schema_ready(32) \
                 AND enrichedlab_agent.broker_schema_ready(34) \
                 AND enrichedlab_agent.broker_schema_ready(71) AND enrichedlab_agent.broker_schema_ready(38) \
                 AND enrichedlab_agent.broker_schema_ready(42) \
                 AND enrichedlab_agent.broker_schema_ready(43) \
                 AND enrichedlab_agent.broker_schema_ready(44) \
                 AND enrichedlab_agent.broker_schema_ready(45) \
                 AND enrichedlab_agent.broker_schema_ready(46) \
                 AND enrichedlab_agent.broker_schema_ready(48) \
                 AND enrichedlab_agent.broker_schema_ready(49) \
                 AND enrichedlab_agent.broker_schema_ready(50) \
                 AND enrichedlab_agent.broker_schema_ready(51) \
                 AND enrichedlab_agent.broker_schema_ready(60) \
                 AND enrichedlab_agent.broker_schema_ready(61) \
                 AND enrichedlab_agent.broker_schema_ready(63) \
                 AND enrichedlab_agent.broker_schema_ready(64) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_research_run_analysis_catalog(text,uuid,text,bigint)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_yield_research_run(text,uuid,text,bigint,text,text,text,text,text)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_yield_research_run(text,uuid,text,bigint,text,text,text,text)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_archive_research_plan(text,uuid,text,bigint,bigint,text[])'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_research_run_sql_sources(text,uuid,text,bigint)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_research_run_prior_queries(text,uuid,text,bigint,uuid[])'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_research_run_own_queries(text,uuid,text,bigint,uuid[])'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_research_keep_contexts(text,uuid,text,bigint,bytea[])'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_research_run_results(text,uuid,text,bigint)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_research_run_lab_notes(text,uuid,text,bigint)'),\
                 'EXECUTE'), false) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_research_run_library(text,uuid,text,bigint,uuid[])'),\
                 'EXECUTE'), false)",
                &[],
            )
            .await
            .map_err(database_error)?;
        if row.get::<_, bool>(0) {
            Ok(())
        } else {
            Err(StoreError {
                kind: StoreErrorKind::Unavailable,
                code: "broker_results_schema_not_ready",
            })
        }
    }

    /// Direct work must be able to retain its first runtime event before claim, and to
    /// record each attempt's execution (migration 019), which it needs before a Sandbox starts.
    pub async fn ready_for_direct_execution(&self) -> StoreResult<()> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let row = client
            .query_one(
                "SELECT enrichedlab_agent.broker_schema_ready(3) \
                 AND enrichedlab_agent.broker_schema_ready(73) \
                 AND enrichedlab_agent.broker_schema_ready(19) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_record_runtime_event(text,uuid,text,bigint,text,jsonb)'),\
                 'EXECUTE'), false) AND COALESCE(has_function_privilege(current_user, \
                 to_regprocedure('enrichedlab_agent.broker_record_attempt_execution(text,uuid,text,bigint,text,text,text,text,text)'),\
                 'EXECUTE'), false)",
                &[],
            )
            .await
            .map_err(database_error)?;
        if row.get::<_, bool>(0) {
            Ok(())
        } else {
            Err(StoreError {
                kind: StoreErrorKind::Unavailable,
                code: "broker_direct_runtime_not_ready",
            })
        }
    }

    /// A worker that serves guarded queries reads its lease's admitted query and records a receipt per attempt with
    /// the version a lab source's database stated (migrations 038 and 053), and refreshes a live source's card
    /// (migration 082). Without them it must not claim work.
    pub async fn ready_for_queries(&self) -> StoreResult<()> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let row = client
            .query_one(
                "SELECT enrichedlab_agent.broker_schema_ready(73) AND enrichedlab_agent.broker_schema_ready(71) AND enrichedlab_agent.broker_schema_ready(38) AND enrichedlab_agent.broker_schema_ready(53) \
                 AND COALESCE(has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_task_query(text,uuid,text,integer,bigint)'),\
                 'EXECUTE'), false) AND COALESCE(has_function_privilege(current_user, \
                 to_regprocedure('enrichedlab_agent.broker_record_query_receipt(text,uuid,text,integer,bigint,text,text,integer,jsonb,text,text,timestamptz,timestamptz,jsonb)'),\
                 'EXECUTE'), false) AND enrichedlab_agent.broker_schema_ready(82) AND COALESCE(has_function_privilege(current_user, \
                 to_regprocedure('enrichedlab_agent.broker_begin_sql_source_card_refresh(text,text,jsonb,text)'), \
                 'EXECUTE'), false) AND COALESCE(has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_finish_sql_source_card_refresh(text,text,text,text,jsonb,text)'), \
                 'EXECUTE'), false)",
                &[],
            )
            .await
            .map_err(database_error)?;
        if row.get::<_, bool>(0) {
            Ok(())
        } else {
            Err(StoreError {
                kind: StoreErrorKind::Unavailable,
                code: "broker_query_schema_not_ready",
            })
        }
    }

    /// A worker with a render service configured serves figure versions only with migration 040 and the worker's
    /// grants for the catalog, the request of its lease and its receipts.
    pub async fn ready_for_figures(&self) -> StoreResult<()> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let row = client
            .query_one(
                "SELECT enrichedlab_agent.broker_schema_ready(40) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_task_figure(text,uuid,text,integer,bigint)'),\
                 'EXECUTE'), false) AND COALESCE(has_function_privilege(current_user, \
                 to_regprocedure('enrichedlab_agent.broker_record_figure_receipt(text,uuid,text,integer,bigint,text,text,text,text,text,jsonb,jsonb,text,timestamptz,timestamptz)'),\
                 'EXECUTE'), false) AND COALESCE(has_function_privilege(current_user, \
                 to_regprocedure('enrichedlab_agent.broker_record_render_catalog(text,text,text,text,jsonb)'),\
                 'EXECUTE'), false)",
                &[],
            )
            .await
            .map_err(database_error)?;
        if row.get::<_, bool>(0) {
            Ok(())
        } else {
            Err(StoreError {
                kind: StoreErrorKind::Unavailable,
                code: "broker_figure_schema_not_ready",
            })
        }
    }

    /// Answer documents (migration 072): the worker's lease read and receipt, so a worker with the render service never
    /// claims a document task it cannot record.
    pub async fn ready_for_documents(&self) -> StoreResult<()> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let row = client
            .query_one(
                "SELECT enrichedlab_agent.broker_schema_ready(72) AND COALESCE(\
                 has_function_privilege(current_user, to_regprocedure(\
                 'enrichedlab_agent.broker_task_document(text,uuid,text,integer,bigint)'),\
                 'EXECUTE'), false) AND COALESCE(has_function_privilege(current_user, \
                 to_regprocedure('enrichedlab_agent.broker_record_document_receipt(text,uuid,text,integer,bigint,text,text,text,text,text,jsonb,jsonb,jsonb,text,timestamptz,timestamptz)'),\
                 'EXECUTE'), false) AND COALESCE(has_function_privilege(current_user, \
                 to_regprocedure('enrichedlab_agent.broker_complete_document(text,uuid,text,integer,bigint,text,text,jsonb,jsonb,jsonb,text,bigint,text,text,bigint,text,text,jsonb,timestamptz,timestamptz)'),\
                 'EXECUTE'), false)",
                &[],
            )
            .await
            .map_err(database_error)?;
        if row.get::<_, bool>(0) {
            Ok(())
        } else {
            Err(StoreError {
                kind: StoreErrorKind::Unavailable,
                code: "broker_document_schema_not_ready",
            })
        }
    }

    /// The platform adapter remains unavailable until migration 008 and its
    /// narrow worker function grant exist. This does not prove platform release
    /// readiness or identity issuance.
    pub async fn ready_for_platform_tool_adapter(&self) -> StoreResult<()> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let row = client
            .query_one(
                r#"SELECT enrichedlab_agent.broker_schema_ready(8)
                     AND COALESCE(
                       has_function_privilege(
                         current_user,
                         to_regprocedure('enrichedlab_agent.broker_bind_platform_tool_job(text,uuid,text,integer,bigint,uuid,uuid,text,text,text,text,timestamptz)'),
                         'EXECUTE'
                       ),
                       false
                     )"#,
                &[],
            )
            .await
            .map_err(database_error)?;
        if row.get::<_, bool>(0) {
            Ok(())
        } else {
            Err(StoreError {
                kind: StoreErrorKind::Unavailable,
                code: "broker_platform_tool_adapter_not_ready",
            })
        }
    }

    pub async fn acquire_worker_leader(&self) -> StoreResult<WorkerLeaderGuard> {
        let connection = self.pool.get().await.map_err(pool_error)?;
        let lock_name = format!("enrichedlab-agent-broker:{}", self.tenant);
        let row = connection
            .query_one(
                "SELECT pg_try_advisory_lock(hashtextextended($1, 0))",
                &[&lock_name],
            )
            .await
            .map_err(database_error)?;
        if !row.get::<_, bool>(0) {
            return Err(StoreError {
                kind: StoreErrorKind::Conflict,
                code: "worker_leader_exists",
            });
        }
        Ok(WorkerLeaderGuard {
            _connection: connection,
        })
    }

    pub async fn resolve_principal(&self, identity: &VerifiedIdentity) -> StoreResult<Uuid> {
        let mut client = self.pool.get().await.map_err(pool_error)?;
        let transaction = client.transaction().await.map_err(database_error)?;
        set_context(&transaction, &self.tenant, None).await?;
        let row = transaction
            .query_one(
                "SELECT enrichedlab_agent.broker_resolve_principal($1,$2,$3,$4,$5,$6)",
                &[
                    &self.tenant,
                    &identity.issuer,
                    &identity.subject,
                    &identity.email,
                    &identity.display_name,
                    &identity.class.as_str(),
                ],
            )
            .await
            .map_err(database_error)?;
        let principal = row.get(0);
        set_context(&transaction, &self.tenant, Some(principal)).await?;
        transaction.commit().await.map_err(database_error)?;
        Ok(principal)
    }

    pub async fn me(&self, principal: Uuid) -> StoreResult<MeView> {
        let mut client = self.pool.get().await.map_err(pool_error)?;
        let transaction = client.transaction().await.map_err(database_error)?;
        set_context(&transaction, &self.tenant, Some(principal)).await?;
        let principal_row = transaction
            .query_opt(
                "SELECT * FROM enrichedlab_agent.broker_get_principal($1,$2)",
                &[&self.tenant, &principal],
            )
            .await
            .map_err(database_error)?
            .ok_or(StoreError {
                kind: StoreErrorKind::Forbidden,
                code: "principal_disabled",
            })?;
        let methods = transaction
            .query(
                "SELECT * FROM enrichedlab_agent.broker_get_group_methods($1,$2)",
                &[&self.tenant, &principal],
            )
            .await
            .map_err(database_error)?;
        let lab_data: Vec<String> = transaction
            .query(
                "SELECT group_key FROM enrichedlab_agent.broker_get_group_lab_data($1,$2) WHERE lab_data",
                &[&self.tenant, &principal],
            )
            .await
            .map_err(database_error)?
            .iter()
            .map(|row| row.get(0))
            .collect();
        let groups = transaction
            .query(
                "SELECT * FROM enrichedlab_agent.broker_get_groups($1,$2)",
                &[&self.tenant, &principal],
            )
            .await
            .map_err(database_error)?
            .into_iter()
            .map(|row| {
                let mut group = map_group(row);
                group.profiles = methods
                    .iter()
                    .filter(|m| m.get::<_, String>("group_key") == group.key)
                    .map(|m| m.get("profile_key"))
                    .collect();
                group.lab_data = lab_data.contains(&group.key);
                group
            })
            .collect();
        let profiles = transaction
            .query(
                "SELECT * FROM enrichedlab_agent.broker_list_profile_choices($1,$2)",
                &[&self.tenant, &principal],
            )
            .await
            .map_err(database_error)?
            .into_iter()
            .map(map_profile)
            .collect();
        let project_inspection: bool = transaction
            .query_one(
                "SELECT enrichedlab_agent.broker_is_project_administrator($1,$2)",
                &[&self.tenant, &principal],
            )
            .await
            .map_err(database_error)?
            .get(0);
        transaction.commit().await.map_err(database_error)?;
        Ok(MeView {
            principal: PrincipalView {
                id: principal_row.get("principal_id"),
                email: principal_row.get("email"),
                display_name: principal_row.get("display_name"),
            },
            groups,
            profiles,
            administration: AdministrationView { project_inspection },
        })
    }

    pub async fn submit_task(
        &self,
        principal: Uuid,
        group: &str,
        profile: &str,
        objective: &str,
        idempotency_key: &str,
        request_sha256: &str,
    ) -> StoreResult<Uuid> {
        let mut client = self.pool.get().await.map_err(pool_error)?;
        let transaction = client.transaction().await.map_err(database_error)?;
        set_context(&transaction, &self.tenant, Some(principal)).await?;
        let row = transaction
            .query_one(
                "SELECT enrichedlab_agent.broker_submit_person_task($1,$2,$3,$4,$5,$6,$7)",
                &[
                    &self.tenant,
                    &principal,
                    &group,
                    &profile,
                    &objective,
                    &idempotency_key,
                    &request_sha256,
                ],
            )
            .await
            .map_err(database_error)?;
        let task = row.get(0);
        transaction.commit().await.map_err(database_error)?;
        Ok(task)
    }

    pub async fn list_tasks(&self, principal: Uuid, limit: i32) -> StoreResult<Vec<TaskView>> {
        let client = self.pool.get().await.map_err(pool_error)?;
        client
            .query(
                "SELECT * FROM enrichedlab_agent.broker_list_tasks($1,$2,$3)",
                &[&self.tenant, &principal, &limit],
            )
            .await
            .map_err(database_error)
            .map(|rows| rows.into_iter().map(map_task).collect())
    }

    pub async fn get_task(&self, principal: Uuid, task: Uuid) -> StoreResult<TaskView> {
        let client = self.pool.get().await.map_err(pool_error)?;
        client
            .query_opt(
                "SELECT * FROM enrichedlab_agent.broker_get_task($1,$2,$3)",
                &[&self.tenant, &principal, &task],
            )
            .await
            .map_err(database_error)?
            .map(map_task)
            .ok_or(StoreError {
                kind: StoreErrorKind::NotFound,
                code: "task_not_found",
            })
    }

    pub async fn cancel_task(&self, principal: Uuid, task: Uuid) -> StoreResult<String> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let row = client
            .query_one(
                "SELECT enrichedlab_agent.broker_request_cancel($1,$2,$3)",
                &[&self.tenant, &principal, &task],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    pub async fn list_events(
        &self,
        principal: Uuid,
        task: Uuid,
        after: i64,
        limit: i32,
    ) -> StoreResult<Vec<TaskEventView>> {
        let client = self.pool.get().await.map_err(pool_error)?;
        client
            .query(
                "SELECT * FROM enrichedlab_agent.broker_list_task_events($1,$2,$3,$4,$5)",
                &[&self.tenant, &principal, &task, &after, &limit],
            )
            .await
            .map_err(database_error)
            .map(|rows| rows.into_iter().map(map_event).collect())
    }

    /// Bind the exact server-issued logical-agent run to the worker's current task lease.
    /// This does not claim that the controller's ledger is readable; readers require a
    /// matching canonical run in this broker's PostgreSQL provenance tables.
    pub async fn bind_scientific_history(
        &self,
        task: &ClaimedTask,
        worker: &str,
        state_run_id: &str,
        subject: &str,
        first_segment: i64,
    ) -> StoreResult<bool> {
        let client = self.pool.get().await.map_err(pool_error)?;
        client
            .query_one(
                "SELECT enrichedlab_agent.broker_bind_scientific_history($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
                &[
                    &self.tenant,
                    &task.id,
                    &worker,
                    &task.attempt,
                    &task.lease_generation,
                    &task.principal_id,
                    &task.logical_agent,
                    &state_run_id,
                    &subject,
                    &first_segment,
                ],
            )
            .await
            .map_err(database_error)
            .map(|row| row.get(0))
    }

    pub async fn finalize_scientific_history(
        &self,
        task: &ClaimedTask,
        worker: &str,
        last_segment: i64,
    ) -> StoreResult<bool> {
        let client = self.pool.get().await.map_err(pool_error)?;
        client
            .query_one(
                "SELECT enrichedlab_agent.broker_finalize_scientific_history($1,$2,$3,$4,$5,$6)",
                &[
                    &self.tenant,
                    &task.id,
                    &worker,
                    &task.attempt,
                    &task.lease_generation,
                    &last_segment,
                ],
            )
            .await
            .map_err(database_error)
            .map(|row| row.get(0))
    }

    pub async fn scientific_history_page(
        &self,
        principal: Uuid,
        task: Uuid,
        cursor: Option<(i32, i64, i64)>,
        limit: i32,
    ) -> StoreResult<ScientificHistoryPage> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let binding = client
            .query_one(
                "SELECT * FROM enrichedlab_agent.broker_scientific_history_status($1,$2,$3,$4,$5)",
                &[
                    &self.tenant,
                    &principal,
                    &task,
                    &cursor.map(|(attempt, _, _)| attempt),
                    &cursor.map(|(_, generation, _)| generation),
                ],
            )
            .await
            .map_err(database_error)?;
        let coverage: String = binding.get("coverage");
        let attempt: Option<i32> = binding.get("attempt");
        let generation: Option<i64> = binding.get("lease_generation");
        let state_run_id: Option<String> = binding.get("state_run_id");
        if !matches!(coverage.as_str(), "available" | "pending" | "unavailable") {
            return Err(StoreError {
                kind: StoreErrorKind::Internal,
                code: "scientific_history_status_invalid",
            });
        }
        if let Some((cursor_attempt, cursor_generation, _)) = cursor
            && (attempt != Some(cursor_attempt) || generation != Some(cursor_generation))
        {
            return Err(StoreError {
                kind: StoreErrorKind::Invalid,
                code: "scientific_history_cursor_stale",
            });
        }
        if coverage != "available" {
            return Ok(ScientificHistoryPage {
                coverage,
                attempt,
                lease_generation: generation,
                events: Vec::new(),
            });
        }
        let (attempt, generation, _state_run_id) = match (attempt, generation, state_run_id) {
            (Some(attempt), Some(generation), Some(state_run_id)) => {
                (attempt, generation, state_run_id)
            }
            _ => {
                return Err(StoreError {
                    kind: StoreErrorKind::Internal,
                    code: "scientific_history_binding_invalid",
                });
            }
        };
        let after = cursor.map(|(_, _, sequence)| sequence).unwrap_or_default();
        let rows = client
            .query(
                "SELECT * FROM enrichedlab_agent.broker_list_scientific_history_events_with_evidence($1,$2,$3,$4,$5,$6,$7)",
                &[&self.tenant, &principal, &task, &attempt, &generation, &after, &limit],
            )
            .await
            .map_err(database_error)?;
        let events = rows.into_iter().map(map_scientific_history_event).collect();
        Ok(ScientificHistoryPage {
            coverage,
            attempt: Some(attempt),
            lease_generation: Some(generation),
            events,
        })
    }

    /// Record an explicit observer-produced file version for a canonical tool event.
    /// The database rechecks the current lease, canonical event identity, and the
    /// retained researcher-visible artifact before accepting the idempotent link.
    pub async fn record_scientific_history_evidence(
        &self,
        task: &ClaimedTask,
        worker: &str,
        evidence: &ScientificHistoryEvidenceInput<'_>,
    ) -> StoreResult<bool> {
        self.boolean_call(
            "SELECT enrichedlab_agent.broker_record_scientific_history_evidence($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
            &[
                &self.tenant,
                &task.id,
                &worker,
                &task.attempt,
                &task.lease_generation,
                &evidence.event_id,
                &evidence.event_sequence,
                &evidence.event_hash,
                &evidence.snapshot_event_id,
                &evidence.artifact_id,
                &evidence.artifact_sha256,
                &evidence.artifact_size_bytes,
                &evidence.artifact_role,
            ],
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn scientific_history_evidence_page(
        &self,
        principal: Uuid,
        task: Uuid,
        attempt: i32,
        generation: i64,
        event: Uuid,
        after: Option<Uuid>,
        limit: i32,
    ) -> StoreResult<ScientificHistoryEvidencePage> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let rows = client
            .query(
                "SELECT * FROM enrichedlab_agent.broker_list_scientific_history_evidence($1,$2,$3,$4,$5,$6,$7,$8)",
                &[
                    &self.tenant,
                    &principal,
                    &task,
                    &attempt,
                    &generation,
                    &event,
                    &after,
                    &limit,
                ],
            )
            .await
            .map_err(database_error)?;
        Ok(ScientificHistoryEvidencePage {
            attempt,
            lease_generation: generation,
            evidence: rows
                .into_iter()
                .map(map_scientific_history_evidence)
                .collect(),
        })
    }

    pub async fn list_artifacts(
        &self,
        principal: Uuid,
        task: Uuid,
    ) -> StoreResult<Vec<ArtifactView>> {
        let client = self.pool.get().await.map_err(pool_error)?;
        client
            .query(
                "SELECT * FROM enrichedlab_agent.broker_list_artifacts($1,$2,$3)",
                &[&self.tenant, &principal, &task],
            )
            .await
            .map_err(database_error)
            .map(|rows| rows.into_iter().map(map_artifact).collect())
    }

    pub async fn get_artifact(
        &self,
        principal: Uuid,
        task: Uuid,
        artifact: Uuid,
    ) -> StoreResult<ArtifactView> {
        let client = self.pool.get().await.map_err(pool_error)?;
        client
            .query_opt(
                "SELECT * FROM enrichedlab_agent.broker_get_artifact($1,$2,$3,$4)",
                &[&self.tenant, &principal, &task, &artifact],
            )
            .await
            .map_err(database_error)?
            .map(map_artifact)
            .ok_or(StoreError {
                kind: StoreErrorKind::NotFound,
                code: "artifact_not_found",
            })
    }

    pub async fn claim_task(
        &self,
        worker: &str,
        lease_seconds: i32,
    ) -> StoreResult<Option<ClaimedTask>> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let row = client
            .query_opt(
                "SELECT * FROM enrichedlab_agent.broker_claim_task($1,$2,$3)",
                &[&self.tenant, &worker, &lease_seconds],
            )
            .await
            .map_err(database_error)?;
        Ok(row.map(map_claimed_task))
    }

    /// Recover only the caller's current lease for trusted custodian setup.
    /// The database function returns an empty objective and performs no mutation.
    pub async fn custody_task(
        &self,
        task: Uuid,
        worker: &str,
        attempt: i32,
        generation: i64,
    ) -> StoreResult<Option<ClaimedTask>> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let row = client
            .query_opt(
                "SELECT * FROM enrichedlab_agent.broker_custody_task($1,$2,$3,$4,$5)",
                &[&self.tenant, &task, &worker, &attempt, &generation],
            )
            .await
            .map_err(database_error)?;
        Ok(row.map(map_claimed_task))
    }

    /// Recover only the caller's exact running lease for trusted terminal custody.
    /// The database function returns an empty objective and performs no mutation.
    pub async fn terminal_custody_task(
        &self,
        task: Uuid,
        worker: &str,
        attempt: i32,
        generation: i64,
    ) -> StoreResult<Option<ClaimedTask>> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let row = client
            .query_opt(
                "SELECT * FROM enrichedlab_agent.broker_terminal_custody_task($1,$2,$3,$4,$5)",
                &[&self.tenant, &task, &worker, &attempt, &generation],
            )
            .await
            .map_err(database_error)?;
        Ok(row.map(map_claimed_task))
    }

    /// Binds an accepted platform job to the exact current core lease. A false
    /// result means no local receipt was written and recovery needs a newly
    /// server-issued platform operation context; it never adopts another job.
    pub async fn bind_platform_tool_job(
        &self,
        task: &ClaimedTask,
        worker: &str,
        accepted: &AcceptedPlatformJob,
    ) -> StoreResult<bool> {
        let accepted_at = time::OffsetDateTime::parse(
            &accepted.accepted_at,
            &time::format_description::well_known::Rfc3339,
        )
        .map_err(|_| StoreError {
            kind: StoreErrorKind::Invalid,
            code: "platform_tool_acceptance_time_invalid",
        })?;
        self.boolean_call(
            "SELECT enrichedlab_agent.broker_bind_platform_tool_job($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
            &[
                &self.tenant,
                &task.id,
                &worker,
                &task.attempt,
                &task.lease_generation,
                &accepted.request_id,
                &accepted.conversation_id,
                &accepted.job_id,
                &accepted.tool_id,
                &accepted.catalog_digest,
                &accepted.input_sha256,
                &accepted_at,
            ],
        )
        .await
    }

    pub async fn renew_task(
        &self,
        task: Uuid,
        worker: &str,
        generation: i64,
        lease_seconds: i32,
    ) -> StoreResult<bool> {
        self.boolean_call(
            "SELECT enrichedlab_agent.broker_renew_task($1,$2,$3,$4,$5)",
            &[&self.tenant, &task, &worker, &generation, &lease_seconds],
        )
        .await
    }

    pub async fn mark_running(
        &self,
        task: Uuid,
        worker: &str,
        generation: i64,
    ) -> StoreResult<bool> {
        self.boolean_call(
            "SELECT enrichedlab_agent.broker_mark_task_running($1,$2,$3,$4)",
            &[&self.tenant, &task, &worker, &generation],
        )
        .await
    }

    pub async fn cancel_requested(
        &self,
        task: Uuid,
        worker: &str,
        generation: i64,
    ) -> StoreResult<bool> {
        self.boolean_call(
            "SELECT enrichedlab_agent.broker_task_cancel_requested($1,$2,$3,$4)",
            &[&self.tenant, &task, &worker, &generation],
        )
        .await
    }

    pub async fn finish_task(
        &self,
        task: Uuid,
        worker: &str,
        generation: i64,
        passed: bool,
        result_class: &str,
        error_code: &str,
    ) -> StoreResult<bool> {
        self.boolean_call(
            "SELECT enrichedlab_agent.broker_finish_task($1,$2,$3,$4,$5,$6,$7)",
            &[
                &self.tenant,
                &task,
                &worker,
                &generation,
                &passed,
                &result_class,
                &error_code,
            ],
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn register_artifact(
        &self,
        task: Uuid,
        worker: &str,
        generation: i64,
        visibility: &str,
        role: &str,
        label: &str,
        relative_path: &str,
        media_type: &str,
        size_bytes: i64,
        sha256: &str,
    ) -> StoreResult<Uuid> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let row = client
            .query_one(
                "SELECT enrichedlab_agent.broker_register_artifact($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
                &[
                    &self.tenant,
                    &task,
                    &worker,
                    &generation,
                    &visibility,
                    &role,
                    &label,
                    &relative_path,
                    &media_type,
                    &size_bytes,
                    &sha256,
                ],
            )
            .await
            .map_err(database_error)?;
        Ok(row.get(0))
    }

    async fn boolean_call(
        &self,
        statement: &str,
        parameters: &[&(dyn tokio_postgres::types::ToSql + Sync)],
    ) -> StoreResult<bool> {
        let client = self.pool.get().await.map_err(pool_error)?;
        let row = client
            .query_one(statement, parameters)
            .await
            .map_err(database_error)?;
        Ok(row
            .try_get::<_, Option<bool>>(0)
            .ok()
            .flatten()
            .unwrap_or(false))
    }
}

#[async_trait::async_trait]
impl RuntimeEventRecorder for Store {
    async fn record_runtime_event(
        &self,
        task: &ClaimedTask,
        worker_id: &str,
        event: &RuntimeEvent,
    ) -> Result<bool> {
        let metadata = event.metadata(task, worker_id)?;
        Ok(self
            .boolean_call(
                "SELECT enrichedlab_agent.broker_record_runtime_event($1,$2,$3,$4,$5,$6)",
                &[
                    &self.tenant,
                    &task.id,
                    &worker_id,
                    &task.lease_generation,
                    &event.code(),
                    &metadata,
                ],
            )
            .await?)
    }
}

pub async fn migrate(args: MigrationArgs) -> Result<()> {
    let dsn = migration_dsn(&args.migration_dsn_file, &args.migration_dsn_env)?;
    let pool = build_pool(&dsn, 1)?;
    let client = pool
        .get()
        .await
        .context("failed to connect as migration owner")?;
    client
        .batch_execute(MIGRATION_001)
        .await
        .context("failed to apply PostgreSQL migration 001")?;
    client
        .batch_execute(MIGRATION_002)
        .await
        .context("failed to apply PostgreSQL migration 002")?;
    client
        .batch_execute(MIGRATION_003)
        .await
        .context("failed to apply PostgreSQL migration 003")?;
    client
        .batch_execute(MIGRATION_004)
        .await
        .context("failed to apply PostgreSQL migration 004")?;
    client
        .batch_execute(MIGRATION_005)
        .await
        .context("failed to apply PostgreSQL migration 005")?;
    client
        .batch_execute(MIGRATION_006)
        .await
        .context("failed to apply PostgreSQL migration 006")?;
    client
        .batch_execute(MIGRATION_007)
        .await
        .context("failed to apply PostgreSQL migration 007")?;
    client
        .batch_execute(MIGRATION_008)
        .await
        .context("failed to apply PostgreSQL migration 008")?;
    client
        .batch_execute(MIGRATION_009)
        .await
        .context("failed to apply PostgreSQL migration 009")?;
    client
        .batch_execute(MIGRATION_010)
        .await
        .context("failed to apply PostgreSQL migration 010")?;
    client
        .batch_execute(MIGRATION_011)
        .await
        .context("failed to apply PostgreSQL migration 011")?;
    client
        .batch_execute(MIGRATION_012)
        .await
        .context("failed to apply PostgreSQL migration 012")?;
    client
        .batch_execute(MIGRATION_013)
        .await
        .context("failed to apply PostgreSQL migration 013")?;
    client
        .batch_execute(MIGRATION_014)
        .await
        .context("failed to apply PostgreSQL migration 014")?;
    client
        .batch_execute(MIGRATION_015)
        .await
        .context("failed to apply PostgreSQL migration 015")?;
    client
        .batch_execute(MIGRATION_016)
        .await
        .context("failed to apply PostgreSQL migration 016")?;
    client
        .batch_execute(MIGRATION_017)
        .await
        .context("failed to apply PostgreSQL migration 017")?;
    client
        .batch_execute(MIGRATION_018)
        .await
        .context("failed to apply PostgreSQL migration 018")?;
    client
        .batch_execute(MIGRATION_019)
        .await
        .context("failed to apply PostgreSQL migration 019")?;
    client
        .batch_execute(MIGRATION_020)
        .await
        .context("failed to apply PostgreSQL migration 020")?;
    client
        .batch_execute(MIGRATION_021)
        .await
        .context("failed to apply PostgreSQL migration 021")?;
    client
        .batch_execute(MIGRATION_022)
        .await
        .context("failed to apply PostgreSQL migration 022")?;
    client
        .batch_execute(MIGRATION_023)
        .await
        .context("failed to apply PostgreSQL migration 023")?;
    client
        .batch_execute(MIGRATION_024)
        .await
        .context("failed to apply PostgreSQL migration 024")?;
    client
        .batch_execute(MIGRATION_025)
        .await
        .context("failed to apply PostgreSQL migration 025")?;
    client
        .batch_execute(MIGRATION_026)
        .await
        .context("failed to apply PostgreSQL migration 026")?;
    client
        .batch_execute(MIGRATION_027)
        .await
        .context("failed to apply PostgreSQL migration 027")?;
    client
        .batch_execute(MIGRATION_028)
        .await
        .context("failed to apply PostgreSQL migration 028")?;
    client
        .batch_execute(MIGRATION_029)
        .await
        .context("failed to apply PostgreSQL migration 029")?;
    client
        .batch_execute(MIGRATION_030)
        .await
        .context("failed to apply PostgreSQL migration 030")?;
    client
        .batch_execute(MIGRATION_031)
        .await
        .context("failed to apply PostgreSQL migration 031")?;
    client
        .batch_execute(MIGRATION_032)
        .await
        .context("failed to apply PostgreSQL migration 032")?;
    client
        .batch_execute(MIGRATION_033)
        .await
        .context("failed to apply PostgreSQL migration 033")?;
    client
        .batch_execute(MIGRATION_034)
        .await
        .context("failed to apply PostgreSQL migration 034")?;
    client
        .batch_execute(MIGRATION_035)
        .await
        .context("failed to apply PostgreSQL migration 035")?;
    client
        .batch_execute(MIGRATION_036)
        .await
        .context("failed to apply PostgreSQL migration 036")?;
    client
        .batch_execute(MIGRATION_037)
        .await
        .context("failed to apply PostgreSQL migration 037")?;
    client
        .batch_execute(MIGRATION_038)
        .await
        .context("failed to apply PostgreSQL migration 038")?;
    client
        .batch_execute(MIGRATION_039)
        .await
        .context("failed to apply PostgreSQL migration 039")?;
    client
        .batch_execute(MIGRATION_040)
        .await
        .context("failed to apply PostgreSQL migration 040")?;
    client
        .batch_execute(MIGRATION_041)
        .await
        .context("failed to apply PostgreSQL migration 041")?;
    client
        .batch_execute(MIGRATION_042)
        .await
        .context("failed to apply PostgreSQL migration 042")?;
    client
        .batch_execute(MIGRATION_043)
        .await
        .context("failed to apply PostgreSQL migration 043")?;
    client
        .batch_execute(MIGRATION_044)
        .await
        .context("failed to apply PostgreSQL migration 044")?;
    client
        .batch_execute(MIGRATION_045)
        .await
        .context("failed to apply PostgreSQL migration 045")?;
    client
        .batch_execute(MIGRATION_046)
        .await
        .context("failed to apply PostgreSQL migration 046")?;
    client
        .batch_execute(MIGRATION_047)
        .await
        .context("failed to apply PostgreSQL migration 047")?;
    client
        .batch_execute(MIGRATION_048)
        .await
        .context("failed to apply PostgreSQL migration 048")?;
    client
        .batch_execute(MIGRATION_049)
        .await
        .context("failed to apply PostgreSQL migration 049")?;
    client
        .batch_execute(MIGRATION_050)
        .await
        .context("failed to apply PostgreSQL migration 050")?;
    client
        .batch_execute(MIGRATION_051)
        .await
        .context("failed to apply PostgreSQL migration 051")?;
    client
        .batch_execute(MIGRATION_052)
        .await
        .context("failed to apply PostgreSQL migration 052")?;
    client
        .batch_execute(MIGRATION_053)
        .await
        .context("failed to apply PostgreSQL migration 053")?;
    client
        .batch_execute(MIGRATION_054)
        .await
        .context("failed to apply PostgreSQL migration 054")?;
    client
        .batch_execute(MIGRATION_055)
        .await
        .context("failed to apply PostgreSQL migration 055")?;
    client
        .batch_execute(MIGRATION_056)
        .await
        .context("failed to apply PostgreSQL migration 056")?;
    client
        .batch_execute(MIGRATION_057)
        .await
        .context("failed to apply PostgreSQL migration 057")?;
    client
        .batch_execute(MIGRATION_058)
        .await
        .context("failed to apply PostgreSQL migration 058")?;
    client
        .batch_execute(MIGRATION_059)
        .await
        .context("failed to apply PostgreSQL migration 059")?;
    client
        .batch_execute(MIGRATION_060)
        .await
        .context("failed to apply PostgreSQL migration 060")?;
    client
        .batch_execute(MIGRATION_061)
        .await
        .context("failed to apply PostgreSQL migration 061")?;
    client
        .batch_execute(MIGRATION_062)
        .await
        .context("failed to apply PostgreSQL migration 062")?;
    client
        .batch_execute(MIGRATION_063)
        .await
        .context("failed to apply PostgreSQL migration 063")?;
    client
        .batch_execute(MIGRATION_064)
        .await
        .context("failed to apply PostgreSQL migration 064")?;
    client
        .batch_execute(MIGRATION_065)
        .await
        .context("failed to apply PostgreSQL migration 065")?;
    client
        .batch_execute(MIGRATION_066)
        .await
        .context("failed to apply PostgreSQL migration 066")?;
    client
        .batch_execute(MIGRATION_067)
        .await
        .context("failed to apply PostgreSQL migration 067")?;
    client
        .batch_execute(MIGRATION_068)
        .await
        .context("failed to apply PostgreSQL migration 068")?;
    client
        .batch_execute(MIGRATION_069)
        .await
        .context("failed to apply PostgreSQL migration 069")?;
    client
        .batch_execute(MIGRATION_070)
        .await
        .context("failed to apply PostgreSQL migration 070")?;
    client
        .batch_execute(MIGRATION_071)
        .await
        .context("failed to apply PostgreSQL migration 071")?;
    client
        .batch_execute(MIGRATION_072)
        .await
        .context("failed to apply PostgreSQL migration 072")?;
    client
        .batch_execute(MIGRATION_073)
        .await
        .context("failed to apply PostgreSQL migration 073")?;
    client
        .batch_execute(MIGRATION_074)
        .await
        .context("failed to apply PostgreSQL migration 074")?;
    client
        .batch_execute(MIGRATION_075)
        .await
        .context("failed to apply PostgreSQL migration 075")?;
    client
        .batch_execute(MIGRATION_076)
        .await
        .context("failed to apply PostgreSQL migration 076")?;
    client
        .batch_execute(MIGRATION_077)
        .await
        .context("failed to apply PostgreSQL migration 077")?;
    client
        .batch_execute(MIGRATION_078)
        .await
        .context("failed to apply PostgreSQL migration 078")?;
    client
        .batch_execute(MIGRATION_079)
        .await
        .context("failed to apply PostgreSQL migration 079")?;
    client
        .batch_execute(MIGRATION_080)
        .await
        .context("failed to apply PostgreSQL migration 080")?;
    client
        .batch_execute(MIGRATION_081)
        .await
        .context("failed to apply PostgreSQL migration 081")?;
    client
        .batch_execute(MIGRATION_082)
        .await
        .context("failed to apply PostgreSQL migration 082")?;
    Ok(())
}

pub async fn seed(args: SeedArgs) -> Result<()> {
    let bytes = fs::read(&args.file)
        .with_context(|| format!("failed to read broker catalog {}", args.file.display()))?;
    if bytes.len() > 1024 * 1024 {
        bail!("broker catalog cannot exceed 1 MiB");
    }
    let catalog: BrokerCatalog =
        serde_yaml_ng::from_slice(&bytes).context("invalid broker catalog YAML")?;
    validate_catalog(&catalog)?;
    let dsn = migration_dsn(&args.migration_dsn_file, &args.migration_dsn_env)?;
    let pool = build_pool(&dsn, 1)?;
    let mut client = pool
        .get()
        .await
        .context("failed to connect as migration owner")?;
    let transaction = client
        .transaction()
        .await
        .context("failed to begin catalog transaction")?;
    transaction
        .execute(
            "INSERT INTO enrichedlab_agent.broker_tenants AS t (tenant_id,name,enabled) VALUES ($1,$2,true) \
             ON CONFLICT (tenant_id) DO UPDATE SET name=EXCLUDED.name,enabled=true,updated_at=clock_timestamp() \
             WHERE (t.name, t.enabled) IS DISTINCT FROM (EXCLUDED.name, true)",
            &[&catalog.tenant.key, &catalog.tenant.name],
        )
        .await?;
    set_context(&transaction, &catalog.tenant.key, None).await?;
    // Only what differs is written, so a rerun of the same catalog is no change (each lab or method row
    // that changes tells the lab's readers to read everything again). Every upsert below skips a row whose fields already
    // match, and each reconciliation disables or revokes only what the catalog no longer lists.
    let group_keys: Vec<&str> = catalog.groups.iter().map(|g| g.key.as_str()).collect();
    let profile_keys: Vec<&str> = catalog.profiles.iter().map(|p| p.key.as_str()).collect();
    transaction
        .execute(
            "UPDATE enrichedlab_agent.broker_research_groups SET enabled=false,updated_at=clock_timestamp() \
             WHERE tenant_id=$1 AND enabled AND NOT (group_key = ANY($2))",
            &[&catalog.tenant.key, &group_keys],
        )
        .await?;
    transaction
        .execute(
            "UPDATE enrichedlab_agent.broker_agent_profiles SET enabled=false,updated_at=clock_timestamp() \
             WHERE tenant_id=$1 AND enabled AND NOT (profile_key = ANY($2))",
            &[&catalog.tenant.key, &profile_keys],
        )
        .await?;
    for group in &catalog.groups {
        transaction.execute(
            "INSERT INTO enrichedlab_agent.broker_research_groups AS g \
             (tenant_id,group_key,name,max_active_agents,max_active_agents_per_principal,max_queued_tasks,retention_days,enabled) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,true) ON CONFLICT (tenant_id,group_key) DO UPDATE SET \
             name=EXCLUDED.name,max_active_agents=EXCLUDED.max_active_agents,\
             max_active_agents_per_principal=EXCLUDED.max_active_agents_per_principal,\
             max_queued_tasks=EXCLUDED.max_queued_tasks,retention_days=EXCLUDED.retention_days,\
             enabled=true,updated_at=clock_timestamp() \
             WHERE (g.name,g.max_active_agents,g.max_active_agents_per_principal,g.max_queued_tasks,g.retention_days,g.enabled) \
             IS DISTINCT FROM (EXCLUDED.name,EXCLUDED.max_active_agents,EXCLUDED.max_active_agents_per_principal,\
             EXCLUDED.max_queued_tasks,EXCLUDED.retention_days,true)",
            &[&catalog.tenant.key,&group.key,&group.name,&group.max_active_agents,
              &group.max_active_agents_per_principal,&group.max_queued_tasks,&group.retention_days],
        ).await?;
    }
    for profile in &catalog.profiles {
        transaction.execute(
            "INSERT INTO enrichedlab_agent.broker_agent_profiles AS p \
             (tenant_id,profile_key,name,description,harness,thinking,route,controller_profile,\
              gateway_profile,image_ref,cpu_limit,memory_limit,state_profile,role,tool_bundle,\
              max_run_seconds,max_task_attempts,max_provider_attempts,enabled) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,true) \
             ON CONFLICT (tenant_id,profile_key) DO UPDATE SET name=EXCLUDED.name,description=EXCLUDED.description,\
             harness=EXCLUDED.harness,thinking=EXCLUDED.thinking,route=EXCLUDED.route,\
             controller_profile=EXCLUDED.controller_profile,gateway_profile=EXCLUDED.gateway_profile,\
             image_ref=EXCLUDED.image_ref,cpu_limit=EXCLUDED.cpu_limit,memory_limit=EXCLUDED.memory_limit,\
             state_profile=EXCLUDED.state_profile,role=EXCLUDED.role,tool_bundle=EXCLUDED.tool_bundle,\
             max_run_seconds=EXCLUDED.max_run_seconds,max_task_attempts=EXCLUDED.max_task_attempts,\
             max_provider_attempts=EXCLUDED.max_provider_attempts,\
             enabled=true,updated_at=clock_timestamp() \
             WHERE (p.name,p.description,p.harness,p.thinking,p.route,p.controller_profile,p.gateway_profile,p.image_ref,\
             p.cpu_limit,p.memory_limit,p.state_profile,p.role,p.tool_bundle,p.max_run_seconds,p.max_task_attempts,\
             p.max_provider_attempts,p.enabled) IS DISTINCT FROM (EXCLUDED.name,EXCLUDED.description,EXCLUDED.harness,\
             EXCLUDED.thinking,EXCLUDED.route,EXCLUDED.controller_profile,EXCLUDED.gateway_profile,EXCLUDED.image_ref,\
             EXCLUDED.cpu_limit,EXCLUDED.memory_limit,EXCLUDED.state_profile,EXCLUDED.role,EXCLUDED.tool_bundle,\
             EXCLUDED.max_run_seconds,EXCLUDED.max_task_attempts,EXCLUDED.max_provider_attempts,true)",
            &[&catalog.tenant.key,&profile.key,&profile.name,&profile.description,&profile.harness,
              &profile.thinking,&profile.route,&profile.controller_profile,&profile.gateway_profile,
              &profile.image_ref,&profile.cpu_limit,&profile.memory_limit,&profile.state_profile,
              &profile.role,&profile.tool_bundle,&profile.max_run_seconds,&profile.max_task_attempts,
              &profile.max_provider_attempts],
        ).await?;
    }
    seed_tool_groups(&transaction, &catalog).await?;
    let invited_emails: Vec<String> = catalog
        .invitations
        .iter()
        .map(|i| i.email.to_ascii_lowercase())
        .collect();
    let invited_groups: Vec<&str> = catalog
        .invitations
        .iter()
        .map(|i| i.group.as_str())
        .collect();
    let capability_emails: Vec<String> = catalog
        .capabilities
        .iter()
        .map(|c| c.email.to_ascii_lowercase())
        .collect();
    let capability_groups: Vec<&str> = catalog
        .capabilities
        .iter()
        .map(|c| c.group.as_str())
        .collect();
    let capability_names: Vec<&str> = catalog
        .capabilities
        .iter()
        .map(|c| c.capability.as_str())
        .collect();
    // A lab's profile grants are its direct grants and the methods of the tool group versions it holds, so a profile
    // grant goes only when neither holds it. Proposal and the worker read only these rows.
    let grant_groups: Vec<&str> = catalog.grants.iter().map(|g| g.group.as_str()).collect();
    let grant_profiles: Vec<&str> = catalog.grants.iter().map(|g| g.profile.as_str()).collect();
    const EFFECTIVE_GRANTS: &str = "SELECT c.group_key, c.profile_key \
         FROM unnest($2::text[], $3::text[]) AS c(group_key, profile_key) \
         UNION SELECT gr.group_key, m.profile_key FROM enrichedlab_agent.broker_tool_group_grants gr \
         JOIN enrichedlab_agent.broker_tool_group_members m ON m.tenant_id = gr.tenant_id \
          AND m.tool_group_key = gr.tool_group_key AND m.version = gr.version \
         WHERE gr.tenant_id = $1 AND gr.revoked_at IS NULL";
    transaction
        .execute(
            &format!(
                "DELETE FROM enrichedlab_agent.broker_group_profile_grants g WHERE g.tenant_id=$1 \
                 AND NOT EXISTS (SELECT 1 FROM ({EFFECTIVE_GRANTS}) AS e \
                                 WHERE e.group_key = g.group_key AND e.profile_key = g.profile_key)"
            ),
            &[&catalog.tenant.key, &grant_groups, &grant_profiles],
        )
        .await?;
    transaction
        .execute(
            &format!(
                "INSERT INTO enrichedlab_agent.broker_group_profile_grants (tenant_id,group_key,profile_key) \
                 SELECT $1, e.group_key, e.profile_key FROM ({EFFECTIVE_GRANTS}) AS e ON CONFLICT DO NOTHING"
            ),
            &[&catalog.tenant.key, &grant_groups, &grant_profiles],
        )
        .await?;
    transaction
        .execute(
            "UPDATE enrichedlab_agent.broker_principal_invitations AS i \
             SET enabled=false,updated_at=clock_timestamp() WHERE i.tenant_id=$1 AND i.enabled \
             AND NOT EXISTS (SELECT 1 FROM unnest($2::text[], $3::text[]) AS c(email, group_key) \
                             WHERE c.email = i.email AND c.group_key = i.group_key)",
            &[&catalog.tenant.key, &invited_emails, &invited_groups],
        )
        .await?;
    for invitation in &catalog.invitations {
        let email = invitation.email.to_ascii_lowercase();
        transaction
            .execute(
                "INSERT INTO enrichedlab_agent.broker_principal_invitations AS i \
             (tenant_id,email,group_key,role,enabled) VALUES ($1,$2,$3,$4,true) \
             ON CONFLICT (tenant_id,email,group_key) DO UPDATE SET role=EXCLUDED.role,\
             enabled=true,updated_at=clock_timestamp() \
             WHERE (i.role, i.enabled) IS DISTINCT FROM (EXCLUDED.role, true)",
                &[
                    &catalog.tenant.key,
                    &email,
                    &invitation.group,
                    &invitation.role,
                ],
            )
            .await?;
    }
    transaction
        .execute(
            "UPDATE enrichedlab_agent.broker_capability_grants AS g SET enabled=false,\
             revoked_at=COALESCE(revoked_at,clock_timestamp()),updated_at=clock_timestamp() \
             WHERE g.tenant_id=$1 AND g.enabled \
             AND NOT EXISTS (SELECT 1 FROM unnest($2::text[], $3::text[], $4::text[]) AS c(email, group_key, capability) \
                             WHERE c.email = g.email AND c.group_key = g.group_key AND c.capability = g.capability)",
            &[&catalog.tenant.key, &capability_emails, &capability_groups, &capability_names],
        )
        .await?;
    for capability in &catalog.capabilities {
        let email = capability.email.to_ascii_lowercase();
        transaction
            .execute(
                "INSERT INTO enrichedlab_agent.broker_capability_grants \
                 (tenant_id,email,group_key,capability,enabled,revoked_at) \
                 VALUES ($1,$2,$3,$4,true,NULL) \
                 ON CONFLICT (tenant_id,email,group_key,capability) DO UPDATE SET \
                 enabled=true,revoked_at=NULL,updated_at=clock_timestamp() \
                 WHERE NOT broker_capability_grants.enabled OR broker_capability_grants.revoked_at IS NOT NULL",
                &[
                    &catalog.tenant.key,
                    &email,
                    &capability.group,
                    &capability.capability,
                ],
            )
            .await?;
    }
    transaction
        .execute(
            "UPDATE enrichedlab_agent.broker_principals AS principals SET \
             enabled=EXISTS (SELECT 1 FROM enrichedlab_agent.broker_principal_invitations AS invitations \
               WHERE invitations.tenant_id=principals.tenant_id AND invitations.email=principals.email \
                 AND invitations.enabled), updated_at=clock_timestamp() \
             WHERE principals.tenant_id=$1 AND principals.principal_class <> 'campaign' \
               AND principals.enabled IS DISTINCT FROM EXISTS (SELECT 1 FROM enrichedlab_agent.broker_principal_invitations AS i \
                 WHERE i.tenant_id=principals.tenant_id AND i.email=principals.email AND i.enabled)",
            &[&catalog.tenant.key],
        )
        .await?;
    transaction
        .execute(
            "DELETE FROM enrichedlab_agent.broker_group_memberships AS memberships \
             WHERE memberships.tenant_id=$1 \
               AND EXISTS (SELECT 1 FROM enrichedlab_agent.broker_principals AS owned \
                           WHERE owned.tenant_id=memberships.tenant_id \
                             AND owned.principal_id=memberships.principal_id \
                             AND owned.principal_class <> 'campaign') \
               AND NOT EXISTS ( \
               SELECT 1 FROM enrichedlab_agent.broker_principals AS principals \
               JOIN enrichedlab_agent.broker_principal_invitations AS invitations \
                 ON invitations.tenant_id=principals.tenant_id AND invitations.email=principals.email \
                AND invitations.group_key=memberships.group_key AND invitations.enabled \
               WHERE principals.tenant_id=memberships.tenant_id \
                 AND principals.principal_id=memberships.principal_id AND principals.enabled)",
            &[&catalog.tenant.key],
        )
        .await?;
    transaction
        .execute(
            "INSERT INTO enrichedlab_agent.broker_group_memberships \
             (tenant_id,group_key,principal_id,role) \
             SELECT invitations.tenant_id,invitations.group_key,principals.principal_id,invitations.role \
             FROM enrichedlab_agent.broker_principal_invitations AS invitations \
             JOIN enrichedlab_agent.broker_principals AS principals \
               ON principals.tenant_id=invitations.tenant_id AND principals.email=invitations.email \
             WHERE invitations.tenant_id=$1 AND invitations.enabled AND principals.enabled \
             ON CONFLICT (tenant_id,group_key,principal_id) DO UPDATE \
             SET role=EXCLUDED.role,updated_at=clock_timestamp() \
             WHERE broker_group_memberships.role IS DISTINCT FROM EXCLUDED.role",
            &[&catalog.tenant.key],
        )
        .await?;
    transaction
        .commit()
        .await
        .context("failed to commit broker catalog")?;
    Ok(())
}

/// How long a call waits for a pooled connection, to open one and to check one before reuse. A call that waits longer
/// fails as `database_unavailable` instead of holding its caller; statement and lock timeouts are the
/// database roles' own.
const POOL_TIMEOUTS: deadpool_postgres::Timeouts = deadpool_postgres::Timeouts {
    wait: Some(std::time::Duration::from_secs(15)),
    create: Some(std::time::Duration::from_secs(10)),
    recycle: Some(std::time::Duration::from_secs(5)),
};

/// The DSN's settings, with the TLS connector unless the DSN disables TLS.
fn postgres_config(dsn: &str) -> Result<(PostgresConfig, Option<MakeTlsConnector>)> {
    let config = PostgresConfig::from_str(dsn).context("invalid PostgreSQL DSN")?;
    let ssl_disabled = format!("{:?}", config.get_ssl_mode()).eq_ignore_ascii_case("disable");
    if ssl_disabled {
        return Ok((config, None));
    }
    let connector = TlsConnector::builder()
        .build()
        .context("failed to build PostgreSQL TLS connector")?;
    Ok((config, Some(MakeTlsConnector::new(connector))))
}

pub(super) fn build_pool(dsn: &str, max_size: usize) -> Result<Pool> {
    let manager = match postgres_config(dsn)? {
        (config, None) => Manager::new(config, NoTls),
        (config, Some(tls)) => Manager::new(config, tls),
    };
    Pool::builder(manager)
        .runtime(Runtime::Tokio1)
        .max_size(max_size)
        .timeouts(POOL_TIMEOUTS)
        .build()
        .context("failed to build PostgreSQL connection pool")
}

async fn set_context(
    transaction: &Transaction<'_>,
    tenant: &str,
    principal: Option<Uuid>,
) -> StoreResult<()> {
    let principal = principal.map(|value| value.to_string()).unwrap_or_default();
    transaction
        .query_one(
            "SELECT set_config('enrichedlab.tenant_id',$1,true), set_config('enrichedlab.principal_id',$2,true)",
            &[&tenant, &principal],
        )
        .await
        .map_err(database_error)?;
    Ok(())
}

fn map_group(row: Row) -> GroupView {
    GroupView {
        key: row.get("group_key"),
        name: row.get("name"),
        role: row.get("role"),
        max_active_agents: row.get("max_active_agents"),
        max_active_agents_per_principal: row.get("max_active_agents_per_principal"),
        max_queued_tasks: row.get("max_queued_tasks"),
        retention_days: row.get("retention_days"),
        profiles: Vec::new(),
        lab_data: false,
    }
}

fn map_profile(row: Row) -> ProfileView {
    ProfileView {
        key: row.get("profile_key"),
        name: row.get("name"),
        description: row.get("description"),
        tool_bundle: row.get("tool_bundle"),
        max_run_seconds: row.get("max_run_seconds"),
        offered: row.get("offered"),
        in_runs: row.get("in_runs"),
    }
}

fn map_task(row: Row) -> TaskView {
    TaskView {
        id: row.get("task_id"),
        group: row.get("group_key"),
        profile: row.get("profile_key"),
        status: row.get("status"),
        objective: row.get("objective"),
        attempt: row.get("attempt"),
        max_task_attempts: row.get("max_task_attempts"),
        max_provider_attempts: row.get("max_provider_attempts"),
        logical_agent: row.get("logical_agent"),
        result_class: row.get("result_class"),
        error_code: row.get("error_code"),
        submitted_at: row.get("submitted_at"),
        updated_at: row.get("updated_at"),
        started_at: row.get("started_at"),
        finished_at: row.get("finished_at"),
    }
}

fn map_event(row: Row) -> TaskEventView {
    TaskEventView {
        sequence: row.get("sequence"),
        kind: row.get("kind"),
        state: row.get("state"),
        code: row.get("code"),
        occurred_at: row.get("occurred_at"),
    }
}

fn map_scientific_history_event(row: Row) -> ScientificHistoryEventView {
    ScientificHistoryEventView {
        sequence: row.get("sequence"),
        event_id: row.get("event_id"),
        segment: row.get("segment"),
        harness: row.get("harness"),
        provider: row.get("provider"),
        model: row.get("model"),
        engine: row.get("engine"),
        parser: row.get("parser"),
        kind: row.get("kind"),
        evidence_role: row.get("evidence_role"),
        visibility: row.get("visibility"),
        occurred_at: row.get("occurred_at"),
        hash: row.get("hash"),
        evidence_count: row.get("evidence_count"),
    }
}

fn map_scientific_history_evidence(row: Row) -> ScientificHistoryEvidenceView {
    ScientificHistoryEvidenceView {
        artifact_id: row.get("artifact_id"),
        sha256: row.get("sha256"),
        size_bytes: row.get("size_bytes"),
        media_type: row.get("media_type"),
        role: row.get("role"),
        relation: row.get("relation"),
    }
}

fn map_artifact(row: Row) -> ArtifactView {
    ArtifactView {
        id: row.get("artifact_id"),
        role: row.get("role"),
        label: row.get("label"),
        relative_path: row.get("relative_path"),
        media_type: row.get("media_type"),
        size_bytes: row.get("size_bytes"),
        sha256: row.get("sha256"),
        created_at: row.get("created_at"),
    }
}

fn map_claimed_task(row: Row) -> ClaimedTask {
    ClaimedTask {
        id: row.get("task_id"),
        principal_id: row.get("principal_id"),
        group_key: row.get("group_key"),
        profile_key: row.get("profile_key"),
        objective: row.get("objective"),
        logical_agent: row.get("logical_agent"),
        lease_generation: row.get("lease_generation"),
        attempt: row.get("attempt"),
        max_task_attempts: row.get("max_task_attempts"),
        max_provider_attempts: row.get("max_provider_attempts"),
        harness: row.get("harness"),
        thinking: row.get("thinking"),
        route: row.get("route"),
        controller_profile: row.get("controller_profile"),
        gateway_profile: row.get("gateway_profile"),
        image_ref: row.get("image_ref"),
        cpu_limit: row.get("cpu_limit"),
        memory_limit: row.get("memory_limit"),
        state_profile: row.get("state_profile"),
        role: row.get("role"),
        max_run_seconds: row.get("max_run_seconds"),
    }
}

fn pool_error(_error: deadpool_postgres::PoolError) -> StoreError {
    StoreError {
        kind: StoreErrorKind::Unavailable,
        code: "database_unavailable",
    }
}

fn database_error(error: tokio_postgres::Error) -> StoreError {
    let Some(database) = error.as_db_error() else {
        return StoreError {
            kind: StoreErrorKind::Unavailable,
            code: "database_unavailable",
        };
    };
    let message = database.message();
    let mapped = match message {
        "broker:not_invited" | "broker:principal_disabled" => {
            (StoreErrorKind::Unauthorized, &message[7..])
        }
        // Migration 030: a principal's class comes from its authenticator.
        "broker:principal_class_required"
        | "broker:principal_class_unknown"
        | "broker:principal_class_mismatch"
        | "broker:campaign_authentication_denied" => (StoreErrorKind::Unauthorized, &message[7..]),
        // Migration 032: a decision's kept texts.
        "broker:context_not_found" | "broker:context_not_kept" => {
            (StoreErrorKind::NotFound, &message[7..])
        }
        "broker:context_lab_members_only" => {
            (StoreErrorKind::Forbidden, "context_lab_members_only")
        }
        "broker:context_invalid" => (StoreErrorKind::Invalid, "context_invalid"),
        "broker:orientation_invalid" => (StoreErrorKind::Invalid, "orientation_invalid"),
        "broker:orientation_not_found" => (StoreErrorKind::NotFound, "orientation_not_found"),
        "broker:group_forbidden" | "broker:profile_forbidden" => {
            (StoreErrorKind::Forbidden, &message[7..])
        }
        "broker:campaign_capability_required"
        | "broker:campaign_scope_forbidden"
        | "broker:campaign_checkpoint_forbidden" => (StoreErrorKind::Forbidden, &message[7..]),
        "broker:campaign_not_found" => (StoreErrorKind::NotFound, "campaign_not_found"),
        "broker:campaign_lease_fence_lost" => {
            (StoreErrorKind::LeaseLost, "campaign_lease_fence_lost")
        }
        "broker:campaign_not_running"
        | "broker:campaign_state_conflict"
        | "broker:campaign_question_duplicate"
        | "broker:campaign_approach_repeated" => (StoreErrorKind::Conflict, &message[7..]),
        "broker:campaign_invalid"
        | "broker:campaign_change_invalid"
        | "broker:campaign_lease_invalid"
        | "broker:campaign_outcome_invalid"
        | "broker:campaign_ref_invalid"
        | "broker:campaign_step_invalid"
        | "broker:campaign_checkpoint_scope_changed"
        | "broker:campaign_planner_text_invalid"
        | "broker:campaign_stop_reason_invalid" => (StoreErrorKind::Invalid, &message[7..]),
        "broker:project_not_found" => (StoreErrorKind::NotFound, "project_not_found"),
        "broker:administrator_required" => (StoreErrorKind::Forbidden, "administrator_required"),
        "broker:project_package_too_large" => {
            (StoreErrorKind::Invalid, "project_package_too_large")
        }
        "broker:project_package_invalid" => (StoreErrorKind::Invalid, "project_package_invalid"),
        "broker:project_package_digest_collision" | "broker:project_package_scope_changed" => {
            (StoreErrorKind::Conflict, &message[7..])
        }
        "broker:project_export_invalid" => (StoreErrorKind::Invalid, "project_export_invalid"),
        "broker:checkpoint_conflict" => (StoreErrorKind::Conflict, "checkpoint_conflict"),
        "broker:checkpoint_invalid" => (StoreErrorKind::Invalid, "checkpoint_invalid"),
        "broker:plan_archive_invalid" => (StoreErrorKind::Invalid, "plan_archive_invalid"),
        "broker:plan_archive_unfinished" => (StoreErrorKind::Invalid, "plan_archive_unfinished"),
        "broker:revision_budget_spent" => (StoreErrorKind::Invalid, "revision_budget_spent"),
        "broker:research_run_invalid" => (StoreErrorKind::Invalid, "research_run_invalid"),
        "broker:generated_code_invalid" => (StoreErrorKind::Invalid, "generated_code_invalid"),
        "broker:inspection_reason_invalid" => {
            (StoreErrorKind::Invalid, "inspection_reason_invalid")
        }
        "broker:task_not_found" => (StoreErrorKind::NotFound, "task_not_found"),
        "broker:idempotency_conflict" => (StoreErrorKind::Conflict, "idempotency_conflict"),
        "broker:group_queue_full" => (StoreErrorKind::Quota, "group_queue_full"),
        "broker:lease_fence_lost" => (StoreErrorKind::LeaseLost, "lease_fence_lost"),
        "broker:artifact_digest_target_invalid" | "broker:artifact_digest_invalid" => {
            (StoreErrorKind::Invalid, &message[7..])
        }
        // Migration 055: result kinds.
        "broker:artifact_kind_target_invalid" | "broker:artifact_kind_receipt_invalid" => {
            (StoreErrorKind::Invalid, &message[7..])
        }
        "broker:artifact_kind_conflict" => (StoreErrorKind::Conflict, "artifact_kind_conflict"),
        // Migration 056: live output.
        "broker:task_output_invalid"
        | "broker:task_output_too_large"
        | "broker:task_output_digest_mismatch"
        | "broker:task_output_sequence" => (StoreErrorKind::Invalid, &message[7..]),
        "broker:task_output_closed" => (StoreErrorKind::Conflict, "task_output_closed"),
        // Migration 057: figure previews and the manuscript mark.
        "broker:capacity_reached" => (StoreErrorKind::Quota, "capacity_reached"),
        "broker:figure_preview_not_found" | "broker:figure_not_found" => {
            (StoreErrorKind::NotFound, &message[7..])
        }
        "broker:figure_preview_invalid" | "broker:figure_preview_too_large" => {
            (StoreErrorKind::Invalid, &message[7..])
        }
        "broker:version_not_in_figure" | "broker:version_not_drawn" => {
            (StoreErrorKind::Conflict, &message[7..])
        }
        "broker:review_forbidden" => (StoreErrorKind::Forbidden, "review_forbidden"),
        "broker:review_person_only" => (StoreErrorKind::Forbidden, "review_person_only"),
        "broker:inbox_person_only" => (StoreErrorKind::Forbidden, "inbox_person_only"),
        "broker:inbox_item_not_found" => (StoreErrorKind::NotFound, "inbox_item_not_found"),
        // Migration 054: workspace reads.
        "broker:look_person_only" => (StoreErrorKind::Forbidden, "look_person_only"),
        "broker:look_revision_invalid" | "broker:analyses_cursor_invalid" => {
            (StoreErrorKind::Invalid, &message[7..])
        }
        "broker:query_invalid" => (StoreErrorKind::Invalid, "query_invalid"),
        "broker:reviewed_query_invalid" => (StoreErrorKind::Invalid, "reviewed_query_invalid"),
        // Migration 061: the change feed.
        "broker:change_cursor_invalid" => (StoreErrorKind::Invalid, "change_cursor_invalid"),
        "broker:template_analysis_invalid" => {
            (StoreErrorKind::Invalid, "template_analysis_invalid")
        }
        // Migration 040: figure versions.
        "broker:figure_table_unknown"
        | "broker:figure_table_unsuitable"
        | "broker:figure_feature_unknown"
        | "broker:figure_settings_invalid"
        | "broker:figure_previous_invalid"
        | "broker:figure_receipt_invalid"
        | "broker:render_catalog_invalid" => (StoreErrorKind::Invalid, &message[7..]),
        "broker:figure_source_not_ready" => (StoreErrorKind::Conflict, "figure_source_not_ready"),
        "broker:figure_method_unavailable" => {
            (StoreErrorKind::Forbidden, "figure_method_unavailable")
        }
        "broker:figure_catalog_unavailable" => {
            (StoreErrorKind::Unavailable, "figure_catalog_unavailable")
        }
        // Migration 072: answer documents.
        "broker:document_request_invalid"
        | "broker:document_source_unknown"
        | "broker:document_sources_too_many"
        | "broker:document_receipt_invalid" => (StoreErrorKind::Invalid, &message[7..]),
        "broker:document_run_unknown" => (StoreErrorKind::NotFound, "document_run_unknown"),
        "broker:document_run_not_ended" => (StoreErrorKind::Conflict, "document_run_not_ended"),
        "broker:document_method_unavailable" => {
            (StoreErrorKind::Forbidden, "document_method_unavailable")
        }
        // Migration 058: template analyses.
        "broker:analysis_table_unknown"
        | "broker:analysis_table_unsuitable"
        | "broker:analysis_feature_unknown"
        | "broker:analysis_settings_invalid"
        | "broker:analysis_previous_invalid"
        | "broker:analysis_receipt_invalid"
        | "broker:analysis_catalog_invalid" => (StoreErrorKind::Invalid, &message[7..]),
        "broker:analysis_source_not_ready" => {
            (StoreErrorKind::Conflict, "analysis_source_not_ready")
        }
        "broker:analysis_catalog_changed" => (StoreErrorKind::Conflict, "analysis_catalog_changed"),
        "broker:analysis_method_unavailable" => {
            (StoreErrorKind::Forbidden, "analysis_method_unavailable")
        }
        "broker:analysis_catalog_unavailable" => {
            (StoreErrorKind::Unavailable, "analysis_catalog_unavailable")
        }
        // Migration 059: lab datasets.
        "broker:dataset_invalid"
        | "broker:dataset_preparation_invalid"
        | "broker:dataset_previous_invalid"
        | "broker:dataset_receipt_invalid" => (StoreErrorKind::Invalid, &message[7..]),
        "broker:dataset_not_found" => (StoreErrorKind::NotFound, "dataset_not_found"),
        "broker:dataset_not_profiled" => (StoreErrorKind::Conflict, "dataset_not_profiled"),
        "broker:dataset_not_allowed" => (StoreErrorKind::Conflict, "dataset_not_allowed"),
        "broker:dataset_method_unavailable" | "broker:person_required" => {
            (StoreErrorKind::Forbidden, &message[7..])
        }
        "broker:query_receipt_invalid" => (StoreErrorKind::Invalid, "query_receipt_invalid"),
        "broker:sql_source_invalid" => (StoreErrorKind::Invalid, "sql_source_invalid"),
        "broker:review_invalid" => (StoreErrorKind::Invalid, "review_invalid"),
        "broker:review_not_finished" | "broker:review_nothing_to_accept" => {
            (StoreErrorKind::Conflict, &message[7..])
        }
        "broker:note_not_found" | "broker:lab_not_found" | "broker:paper_not_found" => {
            (StoreErrorKind::NotFound, &message[7..])
        }
        "broker:library_forbidden" | "broker:label_person_only" => {
            (StoreErrorKind::Forbidden, &message[7..])
        }
        "broker:library_invalid" | "broker:label_invalid" => {
            (StoreErrorKind::Invalid, &message[7..])
        }
        "broker:library_conflict" | "broker:paper_removed" => {
            (StoreErrorKind::Conflict, &message[7..])
        }
        "broker:note_person_only" | "broker:note_forbidden" => {
            (StoreErrorKind::Forbidden, &message[7..])
        }
        "broker:note_invalid" => (StoreErrorKind::Invalid, "note_invalid"),
        "broker:note_project_private"
        | "broker:note_stale"
        | "broker:note_evidence_unaccepted"
        | "broker:note_already_shared"
        | "broker:note_retired" => (StoreErrorKind::Conflict, &message[7..]),
        "broker:objective_invalid"
        | "broker:list_limit_invalid"
        | "broker:event_cursor_invalid"
        | "broker:scientific_history_cursor_invalid"
        | "broker:scientific_history_binding_invalid"
        | "broker:lease_invalid" => (StoreErrorKind::Invalid, &message[7..]),
        _ if database.code() == &SqlState::UNIQUE_VIOLATION => {
            (StoreErrorKind::Conflict, "conflict")
        }
        _ => (StoreErrorKind::Internal, "database_operation_failed"),
    };
    StoreError {
        kind: mapped.0,
        code: static_code(mapped.1),
    }
}

/// A mapped code as the `'static` text an error carries; a code missing here leaves as `database_operation_failed`,
/// so every code `database_error` maps must be listed (`every_mapped_database_code_keeps_its_name`).
fn static_code(code: &str) -> &'static str {
    match code {
        "not_invited" => "not_invited",
        "orientation_invalid" => "orientation_invalid",
        "orientation_not_found" => "orientation_not_found",
        "principal_disabled" => "principal_disabled",
        "group_forbidden" => "group_forbidden",
        "profile_forbidden" => "profile_forbidden",
        "task_not_found" => "task_not_found",
        "project_not_found" => "project_not_found",
        "administrator_required" => "administrator_required",
        "project_package_too_large" => "project_package_too_large",
        "project_package_invalid" => "project_package_invalid",
        "project_package_digest_collision" => "project_package_digest_collision",
        "project_package_scope_changed" => "project_package_scope_changed",
        "project_export_invalid" => "project_export_invalid",
        "checkpoint_conflict" => "checkpoint_conflict",
        "checkpoint_invalid" => "checkpoint_invalid",
        "plan_archive_invalid" => "plan_archive_invalid",
        "plan_archive_unfinished" => "plan_archive_unfinished",
        "revision_budget_spent" => "revision_budget_spent",
        "inspection_reason_invalid" => "inspection_reason_invalid",
        "generated_code_invalid" => "generated_code_invalid",
        "research_run_invalid" => "research_run_invalid",
        "idempotency_conflict" => "idempotency_conflict",
        "group_queue_full" => "group_queue_full",
        "lease_fence_lost" => "lease_fence_lost",
        "objective_invalid" => "objective_invalid",
        "list_limit_invalid" => "list_limit_invalid",
        "event_cursor_invalid" => "event_cursor_invalid",
        "scientific_history_cursor_invalid" => "scientific_history_cursor_invalid",
        "scientific_history_binding_invalid" => "scientific_history_binding_invalid",
        "lease_invalid" => "lease_invalid",
        "artifact_digest_target_invalid" => "artifact_digest_target_invalid",
        "artifact_digest_invalid" => "artifact_digest_invalid",
        "artifact_kind_target_invalid" => "artifact_kind_target_invalid",
        "artifact_kind_receipt_invalid" => "artifact_kind_receipt_invalid",
        "artifact_kind_conflict" => "artifact_kind_conflict",
        "task_output_invalid" => "task_output_invalid",
        "task_output_too_large" => "task_output_too_large",
        "task_output_digest_mismatch" => "task_output_digest_mismatch",
        "task_output_closed" => "task_output_closed",
        "task_output_sequence" => "task_output_sequence",
        "capacity_reached" => "capacity_reached",
        "figure_preview_not_found" => "figure_preview_not_found",
        "figure_not_found" => "figure_not_found",
        "figure_preview_invalid" => "figure_preview_invalid",
        "figure_preview_too_large" => "figure_preview_too_large",
        "version_not_in_figure" => "version_not_in_figure",
        "version_not_drawn" => "version_not_drawn",
        "review_forbidden" => "review_forbidden",
        "review_person_only" => "review_person_only",
        "inbox_person_only" => "inbox_person_only",
        "inbox_item_not_found" => "inbox_item_not_found",
        "look_person_only" => "look_person_only",
        "look_revision_invalid" => "look_revision_invalid",
        "analyses_cursor_invalid" => "analyses_cursor_invalid",
        "query_invalid" => "query_invalid",
        "reviewed_query_invalid" => "reviewed_query_invalid",
        "change_cursor_invalid" => "change_cursor_invalid",
        "template_analysis_invalid" => "template_analysis_invalid",
        "figure_table_unknown" => "figure_table_unknown",
        "figure_table_unsuitable" => "figure_table_unsuitable",
        "figure_feature_unknown" => "figure_feature_unknown",
        "figure_settings_invalid" => "figure_settings_invalid",
        "figure_previous_invalid" => "figure_previous_invalid",
        "figure_receipt_invalid" => "figure_receipt_invalid",
        "render_catalog_invalid" => "render_catalog_invalid",
        "figure_source_not_ready" => "figure_source_not_ready",
        "figure_method_unavailable" => "figure_method_unavailable",
        "document_request_invalid" => "document_request_invalid",
        "document_source_unknown" => "document_source_unknown",
        "document_sources_too_many" => "document_sources_too_many",
        "document_receipt_invalid" => "document_receipt_invalid",
        "document_run_unknown" => "document_run_unknown",
        "document_run_not_ended" => "document_run_not_ended",
        "document_method_unavailable" => "document_method_unavailable",
        "figure_catalog_unavailable" => "figure_catalog_unavailable",
        "analysis_table_unknown" => "analysis_table_unknown",
        "analysis_table_unsuitable" => "analysis_table_unsuitable",
        "analysis_feature_unknown" => "analysis_feature_unknown",
        "analysis_settings_invalid" => "analysis_settings_invalid",
        "analysis_previous_invalid" => "analysis_previous_invalid",
        "analysis_receipt_invalid" => "analysis_receipt_invalid",
        "analysis_catalog_invalid" => "analysis_catalog_invalid",
        "analysis_source_not_ready" => "analysis_source_not_ready",
        "analysis_catalog_changed" => "analysis_catalog_changed",
        "analysis_method_unavailable" => "analysis_method_unavailable",
        "analysis_catalog_unavailable" => "analysis_catalog_unavailable",
        "query_receipt_invalid" => "query_receipt_invalid",
        "sql_source_invalid" => "sql_source_invalid",
        "review_invalid" => "review_invalid",
        "review_not_finished" => "review_not_finished",
        "review_nothing_to_accept" => "review_nothing_to_accept",
        "note_not_found" => "note_not_found",
        "lab_not_found" => "lab_not_found",
        "note_person_only" => "note_person_only",
        "note_forbidden" => "note_forbidden",
        "note_invalid" => "note_invalid",
        "note_project_private" => "note_project_private",
        "note_stale" => "note_stale",
        "note_evidence_unaccepted" => "note_evidence_unaccepted",
        "note_already_shared" => "note_already_shared",
        "note_retired" => "note_retired",
        "paper_not_found" => "paper_not_found",
        "library_forbidden" => "library_forbidden",
        "label_person_only" => "label_person_only",
        "library_invalid" => "library_invalid",
        "label_invalid" => "label_invalid",
        "library_conflict" => "library_conflict",
        "paper_removed" => "paper_removed",
        "principal_class_required" => "principal_class_required",
        "principal_class_unknown" => "principal_class_unknown",
        "principal_class_mismatch" => "principal_class_mismatch",
        "context_not_found" => "context_not_found",
        "context_not_kept" => "context_not_kept",
        "context_lab_members_only" => "context_lab_members_only",
        "context_invalid" => "context_invalid",
        "conflict" => "conflict",
        "dataset_invalid" => "dataset_invalid",
        "dataset_preparation_invalid" => "dataset_preparation_invalid",
        "dataset_previous_invalid" => "dataset_previous_invalid",
        "dataset_receipt_invalid" => "dataset_receipt_invalid",
        "dataset_not_found" => "dataset_not_found",
        "dataset_not_profiled" => "dataset_not_profiled",
        "dataset_not_allowed" => "dataset_not_allowed",
        "dataset_method_unavailable" => "dataset_method_unavailable",
        "person_required" => "person_required",
        "campaign_authentication_denied" => "campaign_authentication_denied",
        "campaign_capability_required" => "campaign_capability_required",
        "campaign_scope_forbidden" => "campaign_scope_forbidden",
        "campaign_checkpoint_forbidden" => "campaign_checkpoint_forbidden",
        "campaign_checkpoint_scope_changed" => "campaign_checkpoint_scope_changed",
        "campaign_planner_text_invalid" => "campaign_planner_text_invalid",
        "campaign_not_found" => "campaign_not_found",
        "campaign_lease_fence_lost" => "campaign_lease_fence_lost",
        "campaign_not_running" => "campaign_not_running",
        "campaign_state_conflict" => "campaign_state_conflict",
        "campaign_question_duplicate" => "campaign_question_duplicate",
        "campaign_approach_repeated" => "campaign_approach_repeated",
        "campaign_invalid" => "campaign_invalid",
        "campaign_change_invalid" => "campaign_change_invalid",
        "campaign_lease_invalid" => "campaign_lease_invalid",
        "campaign_outcome_invalid" => "campaign_outcome_invalid",
        "campaign_ref_invalid" => "campaign_ref_invalid",
        "campaign_step_invalid" => "campaign_step_invalid",
        "campaign_stop_reason_invalid" => "campaign_stop_reason_invalid",
        _ => "database_operation_failed",
    }
}

fn validate_catalog(catalog: &BrokerCatalog) -> Result<()> {
    if catalog.schema_version != 1 {
        bail!("broker catalog schema_version must be 1");
    }
    validate_key(&catalog.tenant.key, "tenant key")?;
    validate_text(&catalog.tenant.name, "tenant name", 160)?;
    if catalog.groups.is_empty() || catalog.profiles.is_empty() {
        bail!("broker catalog needs at least one group and one profile");
    }
    let mut groups = HashSet::new();
    for group in &catalog.groups {
        validate_key(&group.key, "group key")?;
        validate_text(&group.name, "group name", 160)?;
        if !groups.insert(group.key.as_str()) {
            bail!("duplicate group key {}", group.key);
        }
        if !(1..=1024).contains(&group.max_active_agents)
            || !(1..=group.max_active_agents).contains(&group.max_active_agents_per_principal)
            || !(1..=10_000).contains(&group.max_queued_tasks)
            || !(1..=3650).contains(&group.retention_days)
        {
            bail!("invalid quota or retention for group {}", group.key);
        }
    }
    let mut profiles = HashSet::new();
    for profile in &catalog.profiles {
        validate_key(&profile.key, "profile key")?;
        validate_text(&profile.name, "profile name", 160)?;
        validate_text(&profile.description, "profile description", 2000)?;
        if !profiles.insert(profile.key.as_str()) {
            bail!("duplicate profile key {}", profile.key);
        }
        if !matches!(profile.harness.as_str(), "open-code" | "pi")
            || profile
                .thinking
                .as_deref()
                .is_some_and(|value| !matches!(value, "off" | "low" | "medium" | "on" | "xhigh"))
            || !profile.image_ref.contains("@sha256:")
            || !(30..=604_800).contains(&profile.max_run_seconds)
            || !(1..=8).contains(&profile.max_task_attempts)
            || !(1..=8).contains(&profile.max_provider_attempts)
        {
            bail!("invalid runtime profile {}", profile.key);
        }
        for (value, label, max) in [
            (&profile.route, "route", 128),
            (&profile.controller_profile, "controller profile", 128),
            (&profile.gateway_profile, "gateway profile", 128),
            (&profile.image_ref, "image reference", 512),
            (&profile.cpu_limit, "CPU limit", 32),
            (&profile.memory_limit, "memory limit", 32),
            (&profile.state_profile, "state profile", 128),
            (&profile.role, "role", 512),
            (&profile.tool_bundle, "tool bundle", 128),
        ] {
            validate_text(value, label, max)?;
        }
    }
    let mut grants = HashSet::new();
    for grant in &catalog.grants {
        if !groups.contains(grant.group.as_str()) || !profiles.contains(grant.profile.as_str()) {
            bail!("profile grant references an unknown group or profile");
        }
        if !grants.insert((grant.group.as_str(), grant.profile.as_str())) {
            bail!("duplicate profile grant");
        }
    }
    let mut invitations = HashSet::new();
    for invitation in &catalog.invitations {
        let email = invitation.email.to_ascii_lowercase();
        if invitation.email != email
            || email.len() > 320
            || !email.contains('@')
            || email
                .chars()
                .any(|character| character.is_control() || character.is_whitespace())
            || !groups.contains(invitation.group.as_str())
            || !matches!(invitation.role.as_str(), "researcher" | "lead" | "reviewer")
            || email.ends_with("@campaign.invalid")
        {
            bail!("invalid broker invitation");
        }
        if !invitations.insert((email, invitation.group.as_str())) {
            bail!("duplicate broker invitation");
        }
    }
    let mut capabilities = HashSet::new();
    for grant in &catalog.capabilities {
        let email = grant.email.to_ascii_lowercase();
        if grant.email != email
            || email.len() < 3
            || email.len() > 320
            || !email.contains('@')
            || email
                .chars()
                .any(|character| character.is_control() || character.is_whitespace())
            || !groups.contains(grant.group.as_str())
            || grant.capability != "research.discovery.autonomous"
            || email.ends_with("@campaign.invalid")
        {
            bail!("invalid broker capability grant");
        }
        if !capabilities.insert((email, grant.group.as_str(), grant.capability.as_str())) {
            bail!("duplicate broker capability grant");
        }
    }
    let mut tool_groups = HashSet::new();
    for tool_group in &catalog.tool_groups {
        validate_key(&tool_group.key, "tool group key")?;
        validate_text(&tool_group.title, "tool group title", 160)?;
        validate_text(&tool_group.description, "tool group description", 2000)?;
        if !(1..=10_000).contains(&tool_group.version) {
            bail!("tool group {} version must be 1 to 10000", tool_group.key);
        }
        if !tool_groups.insert((tool_group.key.as_str(), tool_group.version)) {
            bail!(
                "duplicate tool group {} version {}",
                tool_group.key,
                tool_group.version
            );
        }
        if tool_group.tools.is_empty() || tool_group.tools.len() > 256 {
            bail!("tool group {} needs 1 to 256 tools", tool_group.key);
        }
        let mut tools = HashSet::new();
        for tool in &tool_group.tools {
            if !profiles.contains(tool.as_str()) {
                bail!("tool group {} names an unknown profile", tool_group.key);
            }
            if !tools.insert(tool.as_str()) {
                bail!("tool group {} lists a profile twice", tool_group.key);
            }
        }
    }
    let mut tool_group_grants = HashSet::new();
    for grant in &catalog.tool_group_grants {
        if !groups.contains(grant.group.as_str())
            || !tool_groups.contains(&(grant.tool_group.as_str(), grant.version))
        {
            bail!("tool group grant references an unknown lab or tool group version");
        }
        if !tool_group_grants.insert((grant.group.as_str(), grant.tool_group.as_str())) {
            bail!("a lab holds one version of each tool group");
        }
    }
    Ok(())
}

/// Stores each tool group version the catalog names, refusing one whose stored title, description or tools differ, then
/// revokes the lab grants the catalog no longer holds and adds the new ones. A rerun of the same catalog changes nothing.
async fn seed_tool_groups(
    transaction: &deadpool_postgres::Transaction<'_>,
    catalog: &BrokerCatalog,
) -> Result<()> {
    let tenant = &catalog.tenant.key;
    for tool_group in &catalog.tool_groups {
        let mut tools: Vec<&str> = tool_group.tools.iter().map(String::as_str).collect();
        tools.sort_unstable();
        let created = transaction
            .query_opt(
                "INSERT INTO enrichedlab_agent.broker_tool_groups (tenant_id,tool_group_key,version,title,description) \
                 VALUES ($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING RETURNING version",
                &[tenant, &tool_group.key, &tool_group.version, &tool_group.title, &tool_group.description],
            )
            .await?
            .is_some();
        if created {
            transaction
                .execute(
                    "INSERT INTO enrichedlab_agent.broker_tool_group_members (tenant_id,tool_group_key,version,profile_key) \
                     SELECT $1,$2,$3,t FROM unnest($4::text[]) AS t",
                    &[tenant, &tool_group.key, &tool_group.version, &tools],
                )
                .await?;
            continue;
        }
        let stored = transaction
            .query_one(
                "SELECT g.title, g.description, ARRAY(SELECT m.profile_key FROM enrichedlab_agent.broker_tool_group_members m \
                   WHERE m.tenant_id = g.tenant_id AND m.tool_group_key = g.tool_group_key AND m.version = g.version \
                   ORDER BY m.profile_key COLLATE \"C\") \
                 FROM enrichedlab_agent.broker_tool_groups g \
                 WHERE g.tenant_id = $1 AND g.tool_group_key = $2 AND g.version = $3",
                &[tenant, &tool_group.key, &tool_group.version],
            )
            .await?;
        let (title, description, members): (String, String, Vec<String>) =
            (stored.get(0), stored.get(1), stored.get(2));
        if title != tool_group.title || description != tool_group.description || members != tools {
            bail!(
                "tool group {} version {} is stored with other contents; give the change a new version",
                tool_group.key,
                tool_group.version
            );
        }
    }
    let labs: Vec<&str> = catalog
        .tool_group_grants
        .iter()
        .map(|g| g.group.as_str())
        .collect();
    let keys: Vec<&str> = catalog
        .tool_group_grants
        .iter()
        .map(|g| g.tool_group.as_str())
        .collect();
    let versions: Vec<i32> = catalog
        .tool_group_grants
        .iter()
        .map(|g| g.version)
        .collect();
    transaction
        .execute(
            "UPDATE enrichedlab_agent.broker_tool_group_grants gr SET revoked_at = clock_timestamp() \
             WHERE gr.tenant_id = $1 AND gr.revoked_at IS NULL AND NOT EXISTS ( \
               SELECT 1 FROM unnest($2::text[], $3::text[], $4::integer[]) AS c(group_key, tool_group_key, version) \
               WHERE c.group_key = gr.group_key AND c.tool_group_key = gr.tool_group_key AND c.version = gr.version)",
            &[tenant, &labs, &keys, &versions],
        )
        .await?;
    transaction
        .execute(
            "INSERT INTO enrichedlab_agent.broker_tool_group_grants (tenant_id,group_key,tool_group_key,version) \
             SELECT $1, c.group_key, c.tool_group_key, c.version \
             FROM unnest($2::text[], $3::text[], $4::integer[]) AS c(group_key, tool_group_key, version) \
             WHERE NOT EXISTS (SELECT 1 FROM enrichedlab_agent.broker_tool_group_grants gr \
               WHERE gr.tenant_id = $1 AND gr.group_key = c.group_key AND gr.tool_group_key = c.tool_group_key \
                 AND gr.version = c.version AND gr.revoked_at IS NULL)",
            &[tenant, &labs, &keys, &versions],
        )
        .await?;
    Ok(())
}

fn validate_key(value: &str, label: &str) -> Result<()> {
    if value.len() < 2
        || value.len() > 64
        || !value.as_bytes()[0].is_ascii_lowercase()
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
    {
        bail!("{label} must be a lowercase identifier");
    }
    Ok(())
}

fn validate_text(value: &str, label: &str, max: usize) -> Result<()> {
    if value.trim().is_empty() || value.len() > max || value.chars().any(char::is_control) {
        bail!("{label} is invalid");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{database_error, validate_catalog};
    use crate::broker::model::{
        BrokerCatalog, CapabilityGrantSeed, ToolGroupGrantSeed, ToolGroupSeed,
    };

    fn catalog() -> BrokerCatalog {
        serde_yaml_ng::from_str(include_str!("../../examples/broker-catalog.yaml")).unwrap()
    }

    fn tool_group(key: &str, version: i32, tools: &[&str]) -> ToolGroupSeed {
        ToolGroupSeed {
            key: key.into(),
            version,
            title: "Core tools".into(),
            description: "The lab's counting tools.".into(),
            tools: tools.iter().map(|t| (*t).into()).collect(),
        }
    }

    fn tool_group_grant(lab: &str, key: &str, version: i32) -> ToolGroupGrantSeed {
        ToolGroupGrantSeed {
            group: lab.into(),
            tool_group: key.into(),
            version,
        }
    }

    #[test]
    fn tool_group_catalog_entries_name_known_methods_and_one_version_per_lab() {
        let mut accepted = catalog();
        accepted.tool_groups = vec![
            tool_group("core", 1, &["general-research"]),
            tool_group("core", 2, &["general-research"]),
        ];
        accepted.tool_group_grants = vec![tool_group_grant("pilot-lab", "core", 2)];
        validate_catalog(&accepted).unwrap();

        let refused = |groups: Vec<ToolGroupSeed>, grants: Vec<ToolGroupGrantSeed>| {
            let mut value = catalog();
            value.tool_groups = groups;
            value.tool_group_grants = grants;
            validate_catalog(&value).unwrap_err().to_string()
        };
        let core = || tool_group("core", 1, &["general-research"]);
        assert!(
            refused(vec![tool_group("core", 1, &["missing"])], vec![]).contains("unknown profile")
        );
        assert!(refused(vec![tool_group("core", 1, &[])], vec![]).contains("1 to 256 tools"));
        assert!(
            refused(
                vec![tool_group(
                    "core",
                    1,
                    &["general-research", "general-research"]
                )],
                vec![]
            )
            .contains("lists a profile twice")
        );
        assert!(
            refused(vec![core(), core()], vec![]).contains("duplicate tool group core version 1")
        );
        assert!(
            refused(vec![tool_group("core", 0, &["general-research"])], vec![])
                .contains("1 to 10000")
        );
        assert!(
            refused(vec![tool_group("Core", 1, &["general-research"])], vec![])
                .contains("lowercase identifier")
        );
        assert!(
            refused(vec![core()], vec![tool_group_grant("pilot-lab", "core", 2)])
                .contains("unknown lab or tool group version")
        );
        assert!(
            refused(vec![core()], vec![tool_group_grant("other-lab", "core", 1)])
                .contains("unknown lab or tool group version")
        );
        assert!(
            refused(
                vec![core(), tool_group("core", 2, &["general-research"])],
                vec![
                    tool_group_grant("pilot-lab", "core", 1),
                    tool_group_grant("pilot-lab", "core", 2)
                ]
            )
            .contains("one version of each tool group")
        );
        let mut untitled = core();
        untitled.title = " ".into();
        assert!(refused(vec![untitled], vec![]).contains("tool group title is invalid"));
    }

    #[test]
    fn a_catalog_without_tool_groups_still_reads() {
        let value: BrokerCatalog =
            serde_yaml_ng::from_str(include_str!("../../examples/broker-catalog.yaml")).unwrap();
        assert!(value.tool_groups.is_empty() && value.tool_group_grants.is_empty());
        let unknown =
            include_str!("../../examples/broker-catalog.yaml").to_owned() + "tool_bundles: []\n";
        assert!(serde_yaml_ng::from_str::<BrokerCatalog>(&unknown).is_err());
    }

    #[test]
    fn campaign_capability_catalog_rejects_unknown_and_duplicate_entries() {
        let grant = CapabilityGrantSeed {
            email: "researcher@example.invalid".into(),
            group: "pilot-lab".into(),
            capability: "research.discovery.autonomous".into(),
        };
        let mut accepted = catalog();
        accepted.capabilities.push(grant);
        validate_catalog(&accepted).unwrap();

        let mut duplicate = catalog();
        duplicate.capabilities.extend([
            CapabilityGrantSeed {
                email: "researcher@example.invalid".into(),
                group: "pilot-lab".into(),
                capability: "research.discovery.autonomous".into(),
            },
            CapabilityGrantSeed {
                email: "researcher@example.invalid".into(),
                group: "pilot-lab".into(),
                capability: "research.discovery.autonomous".into(),
            },
        ]);
        assert!(validate_catalog(&duplicate).is_err());

        let mut unknown_group = catalog();
        unknown_group.capabilities.push(CapabilityGrantSeed {
            email: "researcher@example.invalid".into(),
            group: "unknown-lab".into(),
            capability: "research.discovery.autonomous".into(),
        });
        assert!(validate_catalog(&unknown_group).is_err());

        let mut unknown_capability = catalog();
        unknown_capability.capabilities.push(CapabilityGrantSeed {
            email: "researcher@example.invalid".into(),
            group: "pilot-lab".into(),
            capability: "research.discovery.unbounded".into(),
        });
        assert!(validate_catalog(&unknown_capability).is_err());
    }

    /// Every message `database_error` maps, and every code it maps one to, keeps its name; a code added to the mapping
    /// but not to `static_code` would reach callers as `database_operation_failed`.
    #[test]
    fn every_mapped_database_code_keeps_its_name() {
        use super::static_code;
        let source = include_str!("store.rs");
        let first = &source[source.find("let mapped = match message {").unwrap()..];
        let first = &first[..first.find("code: static_code(mapped.1)").unwrap()];
        let mut checked = 0;
        for literal in first.split('"').skip(1).step_by(2) {
            let code = literal.strip_prefix("broker:").unwrap_or(literal);
            assert_eq!(static_code(code), code, "{literal}");
            checked += 1;
        }
        assert!(checked > 100, "{checked}");
    }

    #[test]
    fn pooled_calls_wait_a_bounded_time() {
        let pool = super::build_pool(
            "host=127.0.0.1 user=broker dbname=broker sslmode=disable",
            2,
        )
        .unwrap();
        let timeouts = pool.timeouts();
        assert_eq!(
            (timeouts.wait, timeouts.create, timeouts.recycle),
            (
                Some(std::time::Duration::from_secs(15)),
                Some(std::time::Duration::from_secs(10)),
                Some(std::time::Duration::from_secs(5))
            )
        );
    }

    #[test]
    fn database_errors_never_include_server_text_without_a_mapping() {
        let error = "not a database error"
            .parse::<tokio_postgres::Config>()
            .unwrap_err();
        assert_eq!(database_error(error).code, "database_unavailable");
    }
}
