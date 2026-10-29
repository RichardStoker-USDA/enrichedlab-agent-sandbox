pub mod admission;
pub mod admission_publication;
pub mod analysis_run;
pub mod analysis_view;
pub mod api;
pub mod artifact_retainer;
pub mod auth;
pub mod budgeted_json;
pub mod campaign;
pub mod canonical;
pub mod cell_grid;
pub mod client;
pub mod codegen;
pub mod config;
pub mod configured;
pub mod coordinator;
pub mod csv_records;
pub mod custody_dispatch;
pub mod dataset_prepare;
pub mod dataset_profile;
pub mod dataset_run;
pub mod digest;
pub mod dispatch;
mod document_compose;
pub mod document_run;
pub(crate) mod document_write;
pub mod evidence_append;
pub mod evidence_canary;
pub mod evidence_custody;
pub mod evidence_objects;
pub mod evidence_package;
pub(crate) mod evidence_telemetry;
pub mod execution;
pub mod export_client_staging;
pub mod export_identity;
pub mod export_kubernetes;
pub mod export_publication;
pub mod export_transport;
pub mod export_workload;
pub mod feature_events;
mod feature_metrics;
pub mod figure_run;
pub mod guard_client;
pub mod input_completion;
pub mod input_custodian;
pub mod input_finalization;
pub mod input_kubernetes;
pub mod input_publication;
pub mod input_request;
pub mod input_workload;
pub mod kind_record;
pub mod kinds;
pub mod knowledge;
pub mod knowledge_person;
pub mod lead_analysis;
pub mod library;
pub mod literature_claims;
pub mod literature_review;
pub mod live_output;
pub mod model;
pub mod output_custodian;
pub mod output_kubernetes;
pub mod output_workload;
pub mod package_registry;
mod package_stream;
pub mod paper;
pub mod plan_publisher;
pub mod platform_tool;
pub mod project;
pub(crate) mod publication_journal;
pub mod query_facts;
pub mod query_run;
pub mod research_catalog;
pub mod reviewed_query;
pub mod runtime_events;
pub mod sandbox;
pub mod screen;
pub mod source_card;
pub mod sql_guard;
mod sql_names;
pub mod step_checks;
pub mod store;
pub mod tables;
pub mod terminal_exports;
pub mod terminal_request;
pub mod worker;
pub mod workload_site;
pub mod workspace_catalog;

use anyhow::Result;

use config::BrokerCommand;

/// Logs a panic by where it happened only. A panic message can quote the text it failed on, such as part of a stored
/// file, so the message is never logged.
pub fn log_panics_by_location() {
    std::panic::set_hook(Box::new(|info| {
        let location = info
            .location()
            .map(|at| format!("{}:{}", at.file(), at.line()))
            .unwrap_or_default();
        tracing::error!(event = "panic", location = %location);
    }));
}

/// Logs a spawned task that ended without returning by how it ended (`panicked` or `cancelled`) and, when known, the
/// broker task it ran. A join error is never formatted: its text carries the panic's message, which the panic hook
/// keeps out of the log for the same reason.
pub fn log_join_failure(
    error: &tokio::task::JoinError,
    what: &'static str,
    task_id: Option<uuid::Uuid>,
) {
    let ended = if error.is_panic() {
        "panicked"
    } else if error.is_cancelled() {
        "cancelled"
    } else {
        "failed"
    };
    match task_id {
        Some(task_id) => tracing::error!(event = "join_failed", what, ended, task_id = %task_id),
        None => tracing::error!(event = "join_failed", what, ended),
    }
}

pub async fn run(command: BrokerCommand) -> Result<()> {
    let ready = match &command {
        BrokerCommand::Coordinate(args) => {
            Some(coordinator::ReadyFile::prepare(args.ready_file.as_deref())?)
        }
        _ => None,
    };
    if matches!(
        &command,
        BrokerCommand::Serve(_)
            | BrokerCommand::Worker(_)
            | BrokerCommand::Coordinate(_)
            | BrokerCommand::CampaignDirector(_)
    ) {
        feature_events::install().await?;
    }
    match command {
        BrokerCommand::Serve(args) => api::serve(args).await,
        BrokerCommand::Worker(args) => worker::run(args).await,
        BrokerCommand::Coordinate(args) => {
            coordinator::run_prepared(args, ready.expect("coordinate readiness prepared")).await
        }
        BrokerCommand::CampaignDirector(args) => campaign::run(args).await,
        BrokerCommand::Migrate(args) => store::migrate(args).await,
        BrokerCommand::EvidenceAppend(args) => evidence_append::serve(args).await,
        BrokerCommand::EvidenceProbe(args) => evidence_append::probe(args).await,
        BrokerCommand::EvidenceCustodyCanary(args) => evidence_canary::run(args).await,
        BrokerCommand::Seed(args) => store::seed(args).await,
        BrokerCommand::DigestPackages(args) => {
            let count = digest::digest_packages(
                &args.artifact_root,
                std::io::stdin().lock(),
                std::io::stdout().lock(),
            )?;
            eprintln!("digested {count} packages");
            Ok(())
        }
        BrokerCommand::ScreenTexts => {
            let count = screen::screen_texts(std::io::stdin().lock(), std::io::stdout().lock())?;
            eprintln!("screened {count} texts");
            Ok(())
        }
        BrokerCommand::ReviewedQuery(args) => reviewed_query::registration::run(args).await,
        BrokerCommand::CheckQueries => {
            let count =
                coordinator::check_queries(std::io::stdin().lock(), std::io::stdout().lock())?;
            eprintln!("checked {count} queries");
            Ok(())
        }
        BrokerCommand::CheckLeadReplies => {
            let count =
                coordinator::check_lead_replies(std::io::stdin().lock(), std::io::stdout().lock())?;
            eprintln!("checked {count} lead replies");
            Ok(())
        }
        BrokerCommand::SourceCard(args) => source_card::run(&args).await,
        BrokerCommand::SourceCardView => source_card::run_view(),
        BrokerCommand::CheckKinds => {
            let registry = kinds::registry().map_err(|error| anyhow::anyhow!(error))?;
            println!("{}", kinds::check_line(registry));
            Ok(())
        }
        BrokerCommand::WriteDocument => {
            use std::io::{Read, Write};
            let mut body = Vec::new();
            std::io::stdin().lock().read_to_end(&mut body)?;
            let written = document_write::write(&body)
                .map_err(|e| anyhow::anyhow!("document not written: {}", e.0))?;
            std::io::stdout().lock().write_all(&written.file)?;
            eprintln!(
                "{} bytes, writer {}",
                written.file.len(),
                document_write::writer_version()
            );
            Ok(())
        }
    }
}
