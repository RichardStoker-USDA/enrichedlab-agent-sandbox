//! Watching a worker's events and workspace while it runs.
use super::*;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WorkspaceHead {
    pub(super) schema_version: u32,
    pub(super) sequence: u64,
    pub(super) segment: u32,
    pub(super) manifest_sha256: String,
    pub(super) manifest: SnapshotManifest,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct ObservedToolEvent {
    pub(super) run_id: String,
    pub(super) event_id: String,
    pub(super) sequence: u64,
    pub(super) hash: String,
}

impl From<provenance::ProvenanceAppendReceipt> for ObservedToolEvent {
    fn from(receipt: provenance::ProvenanceAppendReceipt) -> Self {
        Self {
            run_id: receipt.run_id,
            event_id: receipt.event_id,
            sequence: receipt.sequence,
            hash: receipt.hash,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct WorkspaceDelta {
    pub(super) schema_version: u32,
    pub(super) study: String,
    pub(super) worker: String,
    pub(super) segment: u32,
    pub(super) sequence: u64,
    pub(super) trigger: String,
    pub(super) captured_at: String,
    pub(super) previous_manifest_sha256: Option<String>,
    pub(super) manifest_sha256: String,
    pub(super) added: Vec<SnapshotEntry>,
    pub(super) changed: Vec<SnapshotEntry>,
    pub(super) removed: Vec<SnapshotEntry>,
    pub(super) artifact_versions: Vec<Value>,
    // Observation order, not proof that any one tool caused these changes.
    pub(super) observed_after: Vec<ObservedToolEvent>,
}

pub struct ResearchEventObserver {
    pub(super) config: RuntimeConfig,
    pub(super) study: ResearchStudy,
    pub(super) worker: ResearchWorker,
    pub(super) run_id: String,
    pub(super) segment: u32,
    pub(super) adapter: HarnessEventAdapter,
    pub(super) model: Option<String>,
    pub(super) engine: Option<String>,
    pub(super) parser: Option<String>,
    pub(super) model_revision: Option<String>,
    pub(super) head: Option<WorkspaceHead>,
    pub(super) summary: ResearchObservationSummary,
}

impl ResearchEventObserver {
    pub fn new(
        config: &RuntimeConfig,
        study_name: &str,
        worker: &ResearchWorker,
        segment: u32,
    ) -> Result<Self> {
        if segment == 0 {
            bail!("research segment numbers start at one");
        }
        let study = load(config, study_name)?;
        let current = study
            .workers
            .get(&worker.name)
            .context("unknown research worker")?;
        if current.sandbox != worker.sandbox || current.workspace != worker.workspace {
            bail!("research observer worker identity changed");
        }
        let run_id = worker
            .state_run_id
            .clone()
            .context("research worker has no state run")?;
        let head_path = workspace_head_path(&study, &worker.name);
        let head = if head_path.exists() {
            Some(read_json(&head_path)?)
        } else {
            None
        };
        let routes = RouteCatalog::load(config)?;
        let route = routes.get(&worker.route)?;
        let model = route.model.clone();
        let mut observer = Self {
            config: config.clone(),
            study,
            worker: worker.clone(),
            run_id,
            segment,
            adapter: HarnessEventAdapter::new(worker.harness),
            model,
            engine: route.inference_engine.clone(),
            parser: route.reasoning_parser.clone(),
            model_revision: route.model_revision.clone(),
            head,
            summary: ResearchObservationSummary::default(),
        };
        let event = agent_state::append_event(
            &observer.config,
            &observer.run_id,
            &observer.worker.name,
            "agent.segment.started",
            "host-observer",
            agent_state::StateAuthority::Machine,
            json!({
                "segment": segment,
                "fresh_provider_session": true,
                "prior_transcript_replayed": false,
                "prior_kv_state_reused": false
            }),
        )?;
        observer.update_execution_state(
            &format!("segment-{segment}-running"),
            Some("complete one bounded unit and flush state"),
            &event.event_id,
            false,
        )?;
        observer.capture_workspace_delta("segment-start", observer.head.is_none(), &[])?;
        agent_state::materialize_view(
            &observer.config,
            &observer.run_id,
            &observer.worker.name,
            &observer.worker.workspace,
        )?;
        Ok(observer)
    }

    pub fn observe_line(&mut self, line: &[u8]) -> Result<()> {
        self.summary.json_lines += 1;
        let observation = self.adapter.observe_line(line)?;
        if observation.malformed {
            self.summary.malformed_lines = self.summary.malformed_lines.saturating_add(1);
            return Ok(());
        }
        self.summary.tool_calls = self
            .summary
            .tool_calls
            .saturating_add(observation.tool_calls);
        self.summary.step_finishes = self
            .summary
            .step_finishes
            .saturating_add(observation.step_finishes);
        self.summary.error_events = self.summary.error_events.saturating_add(observation.errors);
        self.summary.input_tokens = self
            .summary
            .input_tokens
            .saturating_add(observation.usage.input_tokens);
        self.summary.output_tokens = self
            .summary
            .output_tokens
            .saturating_add(observation.usage.output_tokens);
        self.summary.reasoning_tokens = self
            .summary
            .reasoning_tokens
            .saturating_add(observation.usage.reasoning_tokens);
        self.summary.total_tokens = self
            .summary
            .total_tokens
            .saturating_add(observation.usage.total_tokens);
        self.summary.peak_context_tokens = self
            .summary
            .peak_context_tokens
            .max(observation.usage.total_tokens);
        self.summary.cache_read_tokens = self
            .summary
            .cache_read_tokens
            .saturating_add(observation.usage.cache_read_tokens);
        self.summary.cache_write_tokens = self
            .summary
            .cache_write_tokens
            .saturating_add(observation.usage.cache_write_tokens);
        let mut observed_after = Vec::new();
        for event in observation.events {
            let receipt = self.record_canonical_event(&event)?;
            if event.kind == "agent.tool.finished"
                || (event.kind == "agent.tool.observed"
                    && matches!(
                        event.metadata.get("status").and_then(Value::as_str),
                        Some("completed" | "error" | "failed")
                    ))
            {
                observed_after.push(ObservedToolEvent::from(receipt));
            }
        }
        if !observed_after.is_empty() {
            self.capture_workspace_delta("tool-completed", false, &observed_after)?;
        }
        Ok(())
    }

    pub fn finish(&mut self) -> Result<ResearchObservationSummary> {
        for event in self.adapter.finish()? {
            self.record_canonical_event(&event)?;
        }
        self.capture_workspace_delta("segment-finish", true, &[])?;
        Ok(self.summary.clone())
    }

    pub fn summary(&self) -> ResearchObservationSummary {
        self.summary.clone()
    }

    pub(super) fn record_canonical_event(
        &self,
        event: &crate::harness::CanonicalHarnessEvent,
    ) -> Result<provenance::ProvenanceAppendReceipt> {
        let receipt = provenance::append_harness_event(
            &self.config,
            &ProvenanceContext {
                run_id: &self.run_id,
                subject: &self.worker.name,
                segment: self.segment,
                harness: self.worker.harness,
                provider: Some("openshell"),
                model: self.model.as_deref(),
                engine: self.engine.as_deref(),
                parser: self.parser.as_deref(),
                session_id: None,
                actor: "host-observer",
            },
            event,
        )?;
        let mut payload = event.metadata.clone();
        let object = payload
            .as_object_mut()
            .context("canonical harness event metadata must be an object")?;
        object.insert("segment".into(), json!(self.segment));
        object.insert("harness".into(), json!(self.worker.harness));
        object.insert("evidence_role".into(), json!(event.evidence_role));
        object.insert("evidence_visibility".into(), json!(event.visibility));
        object.insert("provenance_event_id".into(), json!(receipt.event_id));
        object.insert("provenance_hash".into(), json!(receipt.hash));
        object.insert(
            "raw_content_custody".into(),
            json!("restricted-provenance-store"),
        );
        if let Some(revision) = &self.model_revision {
            object.insert("model_revision".into(), json!(revision));
        }
        agent_state::append_event(
            &self.config,
            &self.run_id,
            &self.worker.name,
            &event.kind,
            "host-observer",
            agent_state::StateAuthority::Machine,
            payload,
        )?;
        Ok(receipt)
    }

    pub(super) fn capture_workspace_delta(
        &mut self,
        trigger: &str,
        force: bool,
        observed_after: &[ObservedToolEvent],
    ) -> Result<()> {
        let manifest = inventory_tree(&self.worker.output)?;
        if manifest.files.len() as u64 > self.study.max_output_files
            || manifest.total_bytes > self.study.max_output_bytes
        {
            bail!("research output exceeds the study snapshot limit");
        }
        let manifest_sha256 = hash_bytes(&serde_json::to_vec(&manifest)?);
        let previous = self
            .head
            .as_ref()
            .map(|head| {
                head.manifest
                    .files
                    .iter()
                    .map(|entry| (entry.path.clone(), entry.clone()))
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        let current = manifest
            .files
            .iter()
            .map(|entry| (entry.path.clone(), entry.clone()))
            .collect::<BTreeMap<_, _>>();
        let added = current
            .iter()
            .filter(|(path, _)| !previous.contains_key(*path))
            .map(|(_, entry)| entry.clone())
            .collect::<Vec<_>>();
        let changed = current
            .iter()
            .filter_map(|(path, entry)| {
                previous
                    .get(path)
                    .filter(|prior| prior.sha256 != entry.sha256 || prior.size != entry.size)
                    .map(|_| entry.clone())
            })
            .collect::<Vec<_>>();
        let removed = previous
            .iter()
            .filter(|(path, _)| !current.contains_key(*path))
            .map(|(_, entry)| entry.clone())
            .collect::<Vec<_>>();
        if !force && added.is_empty() && changed.is_empty() && removed.is_empty() {
            return Ok(());
        }
        let sequence = self
            .head
            .as_ref()
            .map_or(1, |head| head.sequence.saturating_add(1));
        let mut artifact_versions = Vec::new();
        let mut references = Vec::new();
        for entry in added.iter().chain(changed.iter()) {
            let relative = Path::new(&entry.path);
            let source = self.worker.output.join(relative);
            let (collection_key, role, sensitivity) = classify_research_artifact(relative);
            let label = versioned_artifact_label(self.segment, sequence, &entry.path);
            let artifact = agent_state::add_artifact(
                &self.config,
                &self.run_id,
                &self.worker.name,
                &source,
                research_media_type(relative),
                role,
                &label,
                sensitivity,
                "host-observer",
            )?;
            if artifact.sha256 != entry.sha256 || artifact.size != entry.size {
                bail!("research output changed while its immutable version was recorded");
            }
            artifact_versions.push(json!({
                "path": entry.path,
                "sha256": artifact.sha256,
                "size": artifact.size,
                "role": role,
                "sensitivity": sensitivity
            }));
            references.push((
                collection_key.to_string(),
                label,
                artifact.sha256,
                role.to_string(),
            ));
        }
        index_scientific_references(&self.config, &self.worker, &self.run_id, &references)?;
        let snapshot_dir =
            segment_log_dir(&self.study, &self.worker.name, self.segment).join("snapshots");
        create_private_dir(&snapshot_dir)?;
        let receipt_path = snapshot_dir.join(format!("{sequence:06}.json"));
        let delta = WorkspaceDelta {
            schema_version: 2,
            study: self.study.name.clone(),
            worker: self.worker.name.clone(),
            segment: self.segment,
            sequence,
            trigger: trigger.into(),
            captured_at: now()?,
            previous_manifest_sha256: self.head.as_ref().map(|head| head.manifest_sha256.clone()),
            manifest_sha256: manifest_sha256.clone(),
            added: added.clone(),
            changed: changed.clone(),
            removed: removed.clone(),
            artifact_versions,
            observed_after: observed_after.to_vec(),
        };
        write_json(&receipt_path, &delta)?;
        let receipt = agent_state::add_artifact(
            &self.config,
            &self.run_id,
            &self.worker.name,
            &receipt_path,
            "application/json",
            "workspace-delta",
            &format!("segment-{}/snapshot-{sequence}", self.segment),
            agent_state::ArtifactSensitivity::Standard,
            "host-observer",
        )?;
        let event = agent_state::append_event(
            &self.config,
            &self.run_id,
            &self.worker.name,
            "workspace.snapshot.captured",
            "host-observer",
            agent_state::StateAuthority::Machine,
            json!({
                "segment": self.segment,
                "sequence": sequence,
                "trigger": trigger,
                "manifest_sha256": manifest_sha256,
                "receipt_sha256": receipt.sha256,
                "added_files": added.len(),
                "changed_files": changed.len(),
                "removed_files": removed.len(),
                "artifact_versions": references.len(),
                "observed_after": observed_after
            }),
        )?;
        self.summary.workspace_snapshots += 1;
        self.summary.added_files += added.len() as u64;
        self.summary.changed_files += changed.len() as u64;
        self.summary.removed_files += removed.len() as u64;
        self.summary.artifact_versions += references.len() as u64;
        let head = WorkspaceHead {
            schema_version: 1,
            sequence,
            segment: self.segment,
            manifest_sha256,
            manifest,
        };
        write_json(&workspace_head_path(&self.study, &self.worker.name), &head)?;
        self.head = Some(head);
        self.update_execution_state(
            &format!("segment-{}-running", self.segment),
            Some("continue the bounded unit using the refreshed state view"),
            &event.event_id,
            false,
        )?;
        agent_state::materialize_view(
            &self.config,
            &self.run_id,
            &self.worker.name,
            &self.worker.workspace,
        )?;
        Ok(())
    }

    pub(super) fn update_execution_state(
        &self,
        phase: &str,
        next_action: Option<&str>,
        observation_ref: &str,
        completed: bool,
    ) -> Result<()> {
        let mut set = BTreeMap::from([
            ("/phase".into(), json!(phase)),
            ("/next_action".into(), json!(next_action)),
            ("/last_observation_ref".into(), json!(observation_ref)),
        ]);
        if completed {
            set.insert(
                format!("/completed/segment-{}", self.segment),
                json!({
                    "summary": "The provider session ended and controller evidence was captured.",
                    "evidence": [observation_ref]
                }),
            );
        }
        for attempt in 0..8 {
            let execution = agent_state::load_collection(
                &self.config,
                &self.run_id,
                &self.worker.name,
                "execution",
            )?;
            let result = agent_state::apply_mutation(
                &self.config,
                &agent_state::StateMutation {
                    schema_version: 1,
                    run_id: self.run_id.clone(),
                    subject: self.worker.name.clone(),
                    collection: "execution".into(),
                    base_version: execution.version,
                    set: set.clone(),
                    remove: Vec::new(),
                    rationale: Some(agent_state::DecisionRationale {
                        summary: "The host updated machine-observed execution state.".into(),
                        alternatives: Vec::new(),
                        evidence: vec![observation_ref.into()],
                        confidence: Some(1.0),
                    }),
                },
                "host-observer",
                agent_state::StateAuthority::Machine,
            );
            match result {
                Ok(_) => return Ok(()),
                Err(error)
                    if attempt < 7 && error.to_string().contains("state version conflict") =>
                {
                    continue;
                }
                Err(error) => return Err(error),
            }
        }
        unreachable!("bounded state retry loop returns on every branch")
    }
}

pub(super) fn versioned_artifact_label(segment: u32, sequence: u64, path: &str) -> String {
    let prefix = format!("versions/segment-{segment}/snapshot-{sequence}/");
    if prefix.len().saturating_add(path.len()) <= 1024 {
        format!("{prefix}{path}")
    } else {
        format!("{prefix}sha256-{}", hash_bytes(path.as_bytes()))
    }
}

pub(super) fn segment_log_dir(study: &ResearchStudy, worker: &str, segment: u32) -> PathBuf {
    study
        .root
        .join("controller-logs")
        .join(worker)
        .join("segments")
        .join(format!("{segment:06}"))
}

pub(super) fn workspace_head_path(study: &ResearchStudy, worker: &str) -> PathBuf {
    study
        .root
        .join("controller-logs")
        .join(worker)
        .join("workspace-head.json")
}
