//! Capturing a direct run's output and provenance, and the provenance commands.
use super::*;

pub(super) struct DirectRunObserver {
    pub(super) adapter: HarnessEventAdapter,
    pub(super) events: Vec<CanonicalHarnessEvent>,
    pub(super) lines: u64,
}

impl DirectRunObserver {
    pub(super) fn new(harness: HarnessKind) -> Self {
        Self {
            adapter: HarnessEventAdapter::new(harness),
            events: Vec::new(),
            lines: 0,
        }
    }

    pub(super) fn observe_line(&mut self, line: &[u8]) -> Result<Vec<CanonicalHarnessEvent>> {
        if line.iter().all(u8::is_ascii_whitespace) {
            return Ok(Vec::new());
        }
        self.lines = self.lines.saturating_add(1);
        let observation = self.adapter.observe_line(line)?;
        if observation.malformed {
            bail!("harness emitted malformed JSONL");
        }
        self.events.extend(observation.events.iter().cloned());
        Ok(observation.events)
    }

    pub(super) fn finish(&mut self) -> Result<Vec<CanonicalHarnessEvent>> {
        let events = self.adapter.finish()?;
        self.events.extend(events.iter().cloned());
        Ok(events)
    }

    pub(super) fn take_observed(&mut self) -> Vec<CanonicalHarnessEvent> {
        std::mem::take(&mut self.events)
    }
}

#[derive(Clone)]
pub(super) struct IncrementalProvenanceWriter {
    pub(super) config: RuntimeConfig,
    pub(super) run_id: String,
    pub(super) subject: String,
    pub(super) segment: u32,
    pub(super) harness: HarnessKind,
    pub(super) model: Option<String>,
    pub(super) engine: Option<String>,
    pub(super) parser: Option<String>,
}

impl IncrementalProvenanceWriter {
    pub(super) fn append(
        &self,
        event: &CanonicalHarnessEvent,
    ) -> Result<provenance::ProvenanceAppendReceipt> {
        let context = provenance::ProvenanceContext {
            run_id: &self.run_id,
            subject: &self.subject,
            segment: self.segment,
            harness: self.harness,
            provider: Some("openshell"),
            model: self.model.as_deref(),
            engine: self.engine.as_deref(),
            parser: self.parser.as_deref(),
            session_id: None,
            actor: "host-runner",
        };
        provenance::append_harness_event(&self.config, &context, event)
    }

    pub(super) fn append_all(
        &self,
        events: &[CanonicalHarnessEvent],
    ) -> Result<Vec<provenance::ProvenanceAppendReceipt>> {
        let mut receipts = Vec::with_capacity(events.len());
        for event in events {
            receipts.push(self.append(event)?);
        }
        Ok(receipts)
    }
}

pub(super) fn provenance_command(
    config: &RuntimeConfig,
    output: OutputFormat,
    command: ProvenanceCommand,
) -> Result<()> {
    match command {
        ProvenanceCommand::Prepare => {
            let status = provenance::initialize(config)?;
            audit::append(
                config,
                "host-controller",
                "provenance.prepare",
                "provenance/storage",
                "passed",
                json!({"backend": status.backend, "tenant_id": status.tenant_id}),
            )?;
            print_value(output, &status, "Provenance storage prepared")
        }
        ProvenanceCommand::Configure(args) => {
            let mut settings = config.load_settings()?;
            settings.schema_version = 3;
            settings.provenance.backend = args.backend;
            settings.provenance.tenant_id = args.tenant;
            settings.provenance.postgres_dsn_env = args.postgres_dsn_env;
            settings.provenance.postgres_migration_dsn_env = args.postgres_migration_dsn_env;
            config.save_settings(&settings)?;
            audit::append(
                config,
                "host-controller",
                "provenance.configure",
                "provenance/storage",
                "passed",
                json!({
                    "backend": settings.provenance.backend.as_str(),
                    "tenant_id": settings.provenance.tenant_id,
                    "postgres_dsn_env": settings.provenance.postgres_dsn_env,
                    "postgres_migration_dsn_env": settings.provenance.postgres_migration_dsn_env,
                    "dsn_persisted": false
                }),
            )?;
            print_value(
                output,
                &settings.provenance,
                "Provenance storage configuration updated",
            )
        }
        ProvenanceCommand::Ingest(args) => ingest_provenance(config, output, args),
        ProvenanceCommand::Graph { run, limit } => {
            let graph = provenance::graph(config, &run, limit)?;
            print_value(output, &graph, "Provenance graph loaded")
        }
        ProvenanceCommand::Export(args) => print_value(
            output,
            &provenance::export_run(config, &args.run, &args.output_dir, args.include_restricted)?,
            "Provenance run exported",
        ),
    }
}

pub(super) fn ingest_provenance(
    config: &RuntimeConfig,
    output: OutputFormat,
    args: ProvenanceIngestArgs,
) -> Result<()> {
    const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;
    if args.segment == 0 {
        bail!("provenance segments start at one");
    }
    let reader: Box<dyn BufRead> = if args.input.as_os_str() == "-" {
        Box::new(BufReader::new(io::stdin()))
    } else {
        let file = fs::File::open(&args.input)
            .with_context(|| format!("failed to open {}", args.input.display()))?;
        Box::new(BufReader::new(file))
    };
    let context = provenance::ProvenanceContext {
        run_id: &args.run,
        subject: &args.subject,
        segment: args.segment,
        harness: args.harness,
        provider: args.provider.as_deref(),
        model: args.model.as_deref(),
        engine: args.engine.as_deref(),
        parser: args.parser.as_deref(),
        session_id: args.session.as_deref(),
        actor: "host-ingest",
    };
    let mut adapter = HarnessEventAdapter::new(args.harness);
    let mut lines = 0_u64;
    let mut events = 0_u64;
    for line in reader.split(b'\n') {
        let line = line?;
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        if line.len() > MAX_LINE_BYTES {
            bail!("provenance JSONL line exceeds the ingestion limit");
        }
        lines = lines.saturating_add(1);
        let observation = adapter.observe_line(&line)?;
        if observation.malformed {
            bail!("provenance JSONL is malformed at non-empty line {lines}");
        }
        for event in observation.events {
            provenance::append_harness_event(config, &context, &event)?;
            events = events.saturating_add(1);
        }
    }
    for event in adapter.finish()? {
        provenance::append_harness_event(config, &context, &event)?;
        events = events.saturating_add(1);
    }
    audit::append(
        config,
        "host-controller",
        "provenance.ingest",
        &format!("agent-state/{}/provenance", args.run),
        "passed",
        json!({
            "harness": args.harness,
            "lines": lines,
            "events": events,
            "input_path_sha256": if args.input.as_os_str() == "-" { None } else { Some(hash_path_for_audit(&args.input)) }
        }),
    )?;
    print_value(
        output,
        &json!({
            "run": args.run,
            "subject": args.subject,
            "harness": args.harness,
            "lines": lines,
            "events": events
        }),
        "Provenance stream ingested",
    )
}

pub(super) fn hash_path_for_audit(path: &std::path::Path) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(path.as_os_str().as_encoded_bytes()))
}
