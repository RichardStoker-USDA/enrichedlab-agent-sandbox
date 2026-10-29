//! Answer documents (migration 072) against an isolated PostgreSQL: a person asks for a document
//! of a run's answer only once the run ended for review, only with the lab's document method and only for results of
//! the project; the method is request-only; a request id names one task for good; a cancellation while the worker
//! writes leaves nothing registered; the worker's document route reads the pinned table, fills the finding from its
//! cell, writes the file with the broker's own writer and keeps its package; readers and lineage see it.
use enrichedlab_agent_sandbox::broker::{
    config::WorkerConfig,
    coordinator::StartRun,
    document_run,
    model::{PrincipalClass, VerifiedIdentity},
    project::{CheckpointRequest, CreateProject, ResearchState, Visibility},
    query_run,
    sql_guard::{self, Allowed},
    store::{DocumentReceipt, RetainedPackage, Store, StoreErrorKind},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::Read,
    path::PathBuf,
    time::Duration,
};
use time::OffsetDateTime;
use tokio_postgres::{Config, NoTls, config::Host};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const MANIFEST: &str = "5f1c0d9a3a4e2b7c8d6e5f4a3b2c1d0e9f8a7b6c5d4e3f2a1b0c9d8e7f6a5b4c";
const TABLE: &str = "results/query_result.csv";

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn research() -> ResearchState {
    serde_json::from_value(json!({"schema_version":1,"goal":"Which example foods carry the most of the example nutrient?",
        "constraints":["Read only the reference data."],"plan":[{"action_key":"rank-foods","title":"Rank foods by the nutrient",
        "status":"pending","task_id":null,"evidence_refs":[],"rationale":"Answer the question."}],"lessons":[],
        "next_action":"Query the reference data.",
        "workspace":{"schema_version":1,"profile":"reference-sql","image_digest":format!("sha256:{}","a".repeat(64)),
        "source_digest":format!("sha256:{}","b".repeat(64)),"input_refs":["reference://qualification/example"],
        "tool_refs":["tool://reference-sql"],"preparation_receipt":null}})).unwrap()
}

fn query_action() -> Value {
    let sql = "SELECT f.description, n.amount FROM food f JOIN food_nutrient n USING (fdc_id) ORDER BY n.amount DESC NULLS LAST, f.description";
    let relations: BTreeSet<String> = ["food", "food_nutrient"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let functions: BTreeSet<String> = ["count"].iter().map(|s| s.to_string()).collect();
    let a = sql_guard::admit(
        sql,
        &Allowed {
            schema: "reference_qual",
            relations: &relations,
            functions: &functions,
            row_cap: 100,
        },
    )
    .unwrap();
    json!({"action_key": "rank-foods", "profile": "reference-sql", "objective": "Rank the foods.", "request_sha256": "c".repeat(64),
        "query": {"source": "example-reference", "proposed_sql": sql, "admitted_sql": a.sql,
            "sha256": format!("sha256:{}", sha(a.sql.as_bytes())), "guard": sql_guard::GUARD,
            "relations": a.relations, "row_limit": a.limit}})
}

fn members(package: &[u8]) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(package));
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let path = entry.path().unwrap().to_string_lossy().into_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        out.insert(path, bytes);
    }
    out
}

#[tokio::test]
#[ignore = "isolated PostgreSQL through qualify_remote_broker.sh answer-documents"]
async fn a_person_gets_a_document_of_a_runs_answer() {
    let dsn = fs::read_to_string(env::var_os("BROKER_QUALIFICATION_MIGRATION_DSN_FILE").unwrap())
        .unwrap();
    let config: Config = dsn.parse().unwrap();
    assert_eq!(config.get_dbname(), Some("broker_qual"));
    assert!(matches!(config.get_hosts(), [Host::Tcp(h)] if h == "127.0.0.1"));
    let (owner, connection) = config.connect(NoTls).await.unwrap();
    let connection = tokio::spawn(connection);
    let store = |user: &str, size| {
        Store::connect(
            &dsn.replace("user=postgres", &format!("user={user}")),
            "research-site".into(),
            size,
        )
        .unwrap()
    };
    let api = store("broker_api_qual", 3);
    let coordinator = store("broker_coordinator_qual", 2);
    let worker = store("broker_worker_qual", 2);
    // Each role is ready only with migration 072 and its own grants.
    api.ready_for_review_api().await.unwrap();
    worker.ready_for_documents().await.unwrap();
    assert_eq!(
        api.ready_for_documents().await.unwrap_err().code,
        "broker_document_schema_not_ready"
    );

    owner.batch_execute("INSERT INTO enrichedlab_agent.broker_principal_invitations(tenant_id,email,group_key,role) VALUES
        ('research-site','outsider@example.invalid','pilot-lab','researcher');").await.unwrap();
    let person = |subject: &str, email: &str| VerifiedIdentity {
        issuer: "https://identity.example.invalid".into(),
        class: PrincipalClass::Person,
        subject: subject.into(),
        email: email.into(),
        display_name: None,
    };
    let a = api
        .resolve_principal(&person("owner", "researcher@example.invalid"))
        .await
        .unwrap();
    let outsider = api
        .resolve_principal(&person("outsider", "outsider@example.invalid"))
        .await
        .unwrap();

    owner.batch_execute(&format!("
        CREATE SCHEMA reference_qual;
        CREATE TABLE reference_qual.food (fdc_id integer PRIMARY KEY, description text NOT NULL);
        CREATE TABLE reference_qual.food_nutrient (fdc_id integer NOT NULL, amount double precision);
        CREATE TABLE reference_qual.snapshot_manifest (manifest_sha256 text NOT NULL);
        INSERT INTO reference_qual.food VALUES (1,'Example food A'),(2,'Example food B, raw'),(3,'Example food C');
        INSERT INTO reference_qual.food_nutrient VALUES (1,48.2),(2,31.7),(3,12.9);
        INSERT INTO reference_qual.snapshot_manifest VALUES ('{MANIFEST}');
        CREATE ROLE reference_reader_qual LOGIN;
        GRANT USAGE ON SCHEMA reference_qual TO reference_reader_qual;
        GRANT SELECT ON ALL TABLES IN SCHEMA reference_qual TO reference_reader_qual;
        INSERT INTO enrichedlab_agent.broker_agent_profiles(tenant_id,profile_key,name,description,harness,thinking,route,
          controller_profile,gateway_profile,image_ref,cpu_limit,memory_limit,state_profile,role,tool_bundle,max_run_seconds,
          max_task_attempts,max_provider_attempts)
        SELECT tenant_id,k,n,description,harness,thinking,r,controller_profile,gateway_profile,image_ref,cpu_limit,
          memory_limit,state_profile,role,tool_bundle,max_run_seconds,1,max_provider_attempts
        FROM enrichedlab_agent.broker_agent_profiles,
          (VALUES ('reference-sql','Query the example reference data','server-query'),
                  ('document-output','Write a document of an answer','server-document')) v(k,n,r)
        WHERE tenant_id='research-site' AND profile_key='general-research';
        INSERT INTO enrichedlab_agent.broker_group_profile_grants(tenant_id,group_key,profile_key)
          VALUES('research-site','pilot-lab','reference-sql');")).await.unwrap();
    owner
        .query_one("SELECT enrichedlab_agent.broker_register_sql_source('research-site',$1)",
            &[&json!({"source": "example-reference", "profile": "reference-sql", "title": "Example reference data",
                "description": "Invented foods and one invented nutrient's amount per 100 g.", "release": "Qualification release",
                "schema": "reference_qual",
                "relations": [{"name": "food", "row_count": 3, "columns": [{"name": "fdc_id", "type": "integer"}, {"name": "description", "type": "text"}]},
                              {"name": "food_nutrient", "row_count": 3, "columns": [{"name": "fdc_id", "type": "integer"}, {"name": "amount", "type": "double precision"}]}],
                "functions": ["count"], "row_cap": 100, "statement_timeout_ms": 5000,
                "connection_ref": "reference-qual", "snapshot": {"manifest_sha256": MANIFEST}})])
        .await
        .unwrap();
    let project = Uuid::new_v4();
    api.create_project(
        a,
        &CreateProject {
            project_id: project,
            group: "pilot-lab".into(),
            title: "Example answers".into(),
            visibility: Visibility::Private,
        },
    )
    .await
    .unwrap();
    api.checkpoint_project(
        a,
        project,
        &CheckpointRequest {
            expected_revision: 0,
            request_id: Uuid::new_v4(),
            state: research(),
        },
    )
    .await
    .unwrap();
    let run = api
        .start_research_run(
            a,
            project,
            &StartRun {
                run_id: Uuid::new_v4(),
                expected_revision: 1,
                allowed_profiles: vec!["reference-sql".into()],
                max_turns: 4,
                max_parallel: 2,
            },
        )
        .await
        .unwrap();
    let claim = coordinator
        .claim_research_run("lead-a", 90)
        .await
        .unwrap()
        .unwrap();
    let committed = coordinator
        .commit_research_turn(
            run,
            "lead-a",
            claim["run"]["generation"].as_i64().unwrap(),
            1,
            &serde_json::to_value(research()).unwrap(),
            &json!([query_action()]),
            &json!({"schema_version": 1}),
            "delegate",
        )
        .await
        .unwrap();
    let source: Uuid = serde_json::from_value(committed["tasks"][0]["task_id"].clone()).unwrap();

    let root = env::temp_dir().join(format!("answer_documents_codetestfile_{}", Uuid::new_v4()));
    let connections = root.join("connections");
    fs::create_dir_all(root.join("artifacts")).unwrap();
    fs::create_dir_all(&connections).unwrap();
    fs::write(
        connections.join("reference-qual.dsn"),
        dsn.replace("user=postgres", "user=reference_reader_qual"),
    )
    .unwrap();
    let config = WorkerConfig {
        database_dsn: dsn.replace("user=postgres", "user=broker_worker_qual"),
        tenant: "research-site".into(),
        artifact_root: root.join("artifacts"),
        controller_root: root.clone(),
        controller_state: root.clone(),
        controller_config: root.clone(),
        controller_bin: PathBuf::from("/bin/false"),
        openshell_bin: PathBuf::from("/bin/false"),
        docker_bin: PathBuf::from("/bin/false"),
        worker_id: "document-worker".into(),
        direct_dispatch: None,
        query_connection_dir: Some(connections),
        render_url: None,
        dataset_fetcher_config: None,
        live_output: false,
        max_parallel: 1,
        lease_seconds: 120,
        poll_interval: Duration::from_millis(200),
        database_pool: 2,
        max_workspace_files: 10,
        max_workspace_bytes: 1024,
    };
    let claimed = worker
        .claim_task("document-worker", 120)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (claimed.id, claimed.route.as_str()),
        (source, query_run::ROUTE)
    );
    query_run::execute(worker.clone(), &config, claimed, CancellationToken::new())
        .await
        .unwrap();
    let artifact: Uuid = owner
        .query_one("SELECT artifact_id FROM enrichedlab_agent.broker_artifacts WHERE task_id=$1 AND role='result'
            AND visibility='researcher'", &[&source])
        .await
        .unwrap()
        .get(0);
    let reference = format!("artifact://tasks/{source}/{artifact}");

    // A run that has not ended has no answer to write down.
    let refusal = api
        .request_document(a, project, run, "docx", Uuid::new_v4())
        .await
        .unwrap_err();
    assert_eq!(
        (refusal.kind, refusal.code),
        (StoreErrorKind::Conflict, "document_run_not_ended")
    );

    // The tools finished; the lead looks again now rather than at its next poll.
    owner
        .execute("UPDATE enrichedlab_agent.broker_research_runs SET available_at=clock_timestamp()-interval '1 second'
            WHERE run_id=$1", &[&run])
        .await
        .unwrap();
    let claim = coordinator
        .claim_research_run("lead-b", 90)
        .await
        .unwrap()
        .unwrap();
    let binding = |row: u64, column: &str| json!({"ref": reference, "table": TABLE, "row": row, "column": column});
    let decision = json!({"schema_version": 1, "proposal": {"action": "review_required", "answer": "answered",
        "summary": "One example food carries most of the example nutrient.",
        "findings": [{"statement": "{food} carries {amount} per 100 g.",
            "bindings": {"food": binding(1, "description"), "amount": binding(1, "amount")}}],
        "tables": [{"ref": reference, "table": TABLE, "columns": ["description", "amount"],
            "lead_in": "Every example food with its amount.", "rows": 3}],
        "actions": [], "lessons": [], "parts": [{"part": "Which foods carry the most", "state": "supported", "findings": [1]}]}});
    coordinator
        .commit_research_turn(
            run,
            "lead-b",
            claim["run"]["generation"].as_i64().unwrap(),
            claim["project"]["checkpoint"]["revision"].as_i64().unwrap(),
            &claim["project"]["checkpoint"]["state"],
            &json!([]),
            &decision,
            "review_required",
        )
        .await
        .unwrap();

    // Without the lab's document method nothing is asked for; the method is request-only once granted.
    let refusal = api
        .request_document(a, project, run, "docx", Uuid::new_v4())
        .await
        .unwrap_err();
    assert_eq!(
        (refusal.kind, refusal.code),
        (StoreErrorKind::Forbidden, "document_method_unavailable")
    );
    owner.batch_execute("INSERT INTO enrichedlab_agent.broker_group_profile_grants(tenant_id,group_key,profile_key)
        VALUES('research-site','pilot-lab','document-output');").await.unwrap();
    let me = api.me(a).await.unwrap();
    let offered = me
        .profiles
        .iter()
        .find(|p| p.key == "document-output")
        .map(|p| (p.offered, p.in_runs));
    assert_eq!(offered, Some((false, false)));
    let direct = api
        .submit_task(
            a,
            "pilot-lab",
            "document-output",
            "Write it",
            "direct-document",
            &"0".repeat(64),
        )
        .await
        .unwrap_err();
    assert_eq!(
        (direct.kind, direct.code),
        (StoreErrorKind::Forbidden, "profile_forbidden")
    );
    let refusal = api
        .request_document(a, project, run, "pdf", Uuid::new_v4())
        .await
        .unwrap_err();
    assert_eq!(
        (refusal.kind, refusal.code),
        (StoreErrorKind::Invalid, "document_request_invalid")
    );
    let refusal = api
        .request_document(outsider, project, run, "docx", Uuid::new_v4())
        .await
        .unwrap_err();
    assert_eq!(
        (refusal.kind, refusal.code),
        (StoreErrorKind::NotFound, "project_not_found")
    );

    // A document is not an analysis: the lab's project list leaves it out of the counts and review standing.
    let counts = |list: Vec<Value>| {
        list.into_iter()
            .find(|p| p["project_id"] == json!(project))
            .unwrap()["task_counts"]
            .clone()
    };
    let counts_before = counts(api.list_projects(a, 50).await.unwrap());

    // A cancellation that lands while the worker writes leaves nothing registered: the completing transaction checks
    // the whole fence, so the package, its receipt and the task's end arrive together or not at all.
    let cancelled = api
        .request_document(a, project, run, "docx", Uuid::new_v4())
        .await
        .unwrap();
    let claimed = worker
        .claim_task("document-worker", 120)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, cancelled);
    assert!(
        worker
            .mark_running(claimed.id, "document-worker", claimed.lease_generation)
            .await
            .unwrap()
    );
    api.cancel_task(a, cancelled).await.unwrap();
    let now = OffsetDateTime::now_utc();
    let late = DocumentReceipt {
        status: "completed",
        error_code: None,
        message: None,
        service_version: Some("writer.1".into()),
        image_digest: None,
        file: Some(json!({"kind": "docx"})),
        counts: Some(json!({})),
        notes: json!([]),
        package_sha256: Some("a".repeat(64)),
        started_at: now,
        finished_at: now,
    };
    let folder = format!(
        "tasks/{cancelled}/attempts/{}-{}/exports",
        claimed.attempt, claimed.lease_generation
    );
    let retained = RetainedPackage {
        package_path: format!("{folder}/payload"),
        package_bytes: 1,
        package_sha256: "a".repeat(64),
        checksum_path: format!("{folder}/payload.sha256"),
        checksum_bytes: 1,
        checksum_sha256: "b".repeat(64),
        digest_status: None,
        digest: None,
    };
    let lost = worker
        .complete_document(
            cancelled,
            "document-worker",
            claimed.attempt,
            claimed.lease_generation,
            &late,
            &retained,
        )
        .await
        .unwrap_err();
    assert_eq!(lost.kind, StoreErrorKind::LeaseLost);
    assert!(
        api.list_artifacts(a, cancelled).await.unwrap().is_empty(),
        "nothing registered"
    );
    let alone = worker
        .record_document_receipt(
            cancelled,
            "document-worker",
            claimed.attempt,
            claimed.lease_generation,
            &late,
        )
        .await
        .unwrap_err();
    assert_eq!(
        alone.code, "document_receipt_invalid",
        "a completed receipt comes only with its package"
    );
    assert!(
        api.task_document_detail(a, cancelled).await.unwrap()["receipts"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    worker
        .finish_task(
            cancelled,
            "document-worker",
            claimed.lease_generation,
            false,
            "document_not_written",
            "cancelled",
        )
        .await
        .unwrap();

    let request_id = Uuid::new_v4();
    let task = api
        .request_document(a, project, run, "docx", request_id)
        .await
        .unwrap();
    assert_ne!(
        task, cancelled,
        "a cancelled document is not the one a new request gets"
    );
    assert_eq!(
        api.request_document(a, project, run, "docx", request_id)
            .await
            .unwrap(),
        task,
        "the same request, the same task"
    );
    // Another request for the same document gets the same task and binds its own id to it, so a retry of it returns
    // that task whatever the run holds by then; an id is never reused for another kind.
    let second = Uuid::new_v4();
    assert_eq!(
        api.request_document(a, project, run, "docx", second)
            .await
            .unwrap(),
        task
    );
    let bound: Uuid = owner
        .query_one(
            "SELECT task_id FROM enrichedlab_agent.broker_document_request_keys WHERE request_id=$1",
            &[&second],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(bound, task);
    for (id, kind) in [(second, "xlsx"), (request_id, "pptx")] {
        let conflict = api
            .request_document(a, project, run, kind, id)
            .await
            .unwrap_err();
        assert_eq!(conflict.code, "idempotency_conflict", "{kind}");
    }
    assert_eq!(
        counts(api.list_projects(a, 50).await.unwrap()),
        counts_before,
        "a queued document is not an analysis"
    );
    let pinned = api.task_document_detail(a, task).await.unwrap();
    assert_eq!(pinned["turn"], 2);
    assert_eq!(pinned["sources"][0]["ref"], reference.as_str());
    assert_eq!(pinned["sources"][0]["rows"], 3);

    let claimed = worker
        .claim_task("document-worker", 120)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (claimed.id, claimed.route.as_str()),
        (task, document_run::ROUTE)
    );
    document_run::execute(worker.clone(), &config, claimed, CancellationToken::new())
        .await
        .unwrap();
    let detail = api.task_document_detail(a, task).await.unwrap();
    assert_eq!(detail["status"], "completed", "{detail}");
    let receipt = &detail["receipts"][0];
    assert_eq!(receipt["status"], "completed");
    assert_eq!(receipt["file"]["kind"], "docx");
    let version = receipt["service_version"].as_str().unwrap();
    assert!(version.starts_with("writer.1"), "{version}");
    assert!(receipt["image_digest"].is_null());
    assert_eq!(
        counts(api.list_projects(a, 50).await.unwrap()),
        counts_before,
        "a finished document is not an analysis"
    );
    let inbox = api.inbox(a).await.unwrap().to_string();
    assert!(
        !inbox.contains(&task.to_string()),
        "a finished document does not wait for review: {inbox}"
    );

    let path: String = owner
        .query_one("SELECT relative_path FROM enrichedlab_agent.broker_artifacts WHERE task_id=$1 AND role='result'", &[&task])
        .await
        .unwrap()
        .get(0);
    let package = fs::read(root.join("artifacts").join(&path)).unwrap();
    assert_eq!(receipt["package_sha256"], sha(&package));
    let files = members(&package);
    for name in [
        "document.docx",
        "request.json",
        "receipt.json",
        "spec.json",
        "README.md",
        "SHA256SUMS",
        "tables/answer-table-1.csv",
    ] {
        assert!(files.contains_key(name), "{name}");
    }
    assert!(files["document.docx"].starts_with(b"PK\x03\x04"));
    let written: Value = serde_json::from_slice(&files["receipt.json"]).unwrap();
    assert_eq!(written["request_sha256"], sha(&files["request.json"]));
    assert_eq!(written["file_sha256"], sha(&files["document.docx"]));
    assert_eq!(written["writer"]["version"], version);
    let sent: Value = serde_json::from_slice(&files["request.json"]).unwrap();
    let paragraphs = |heading: &str| {
        sent["sections"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["heading"] == heading)
            .unwrap()["paragraphs"]
            .clone()
    };
    assert_eq!(
        paragraphs("Question")[0],
        "Which example foods carry the most of the example nutrient?"
    );
    assert_eq!(
        paragraphs("Findings")[0],
        "1. Example food A carries 48.2 per 100 g."
    );
    let table = sent["sections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["heading"] == "Table 1: query_result.csv")
        .unwrap();
    assert_eq!(table["table"]["rows"].as_array().unwrap().len(), 3);
    let spec: Value = serde_json::from_slice(&files["spec.json"]).unwrap();
    assert_eq!(
        spec["values"][1],
        json!({"finding": 1, "slot": "amount", "value": "48.2", "ref": reference, "table": TABLE,
        "row": 1, "column": "amount"})
    );
    assert!(
        String::from_utf8_lossy(&files["SHA256SUMS"])
            .contains(&format!("{}  document.docx", sha(&files["document.docx"])))
    );

    let read = api.read_project(a, project).await.unwrap();
    let item = read["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["task_id"] == json!(task))
        .unwrap()
        .clone();
    assert_eq!(
        item["document"],
        json!({"kind": "docx", "run": run, "turn": 2}),
        "the project read marks a document"
    );
    assert!(
        item["figure"].is_null()
            && item["template_analysis"].is_null()
            && item["dataset"].is_null()
    );
    let documents = api.run_documents(a, run).await.unwrap();
    assert_eq!(
        documents.as_array().unwrap().len(),
        2,
        "the cancelled and the written document"
    );
    assert_eq!(
        api.run_documents(outsider, run).await.unwrap_err().code,
        "project_not_found"
    );
    let lineage = api.task_lineage(a, task).await.unwrap();
    assert_eq!(lineage["method"]["tool_class"], "vetted_service");
    assert_eq!(lineage["outputs"][0]["provenance_class"], "document_output");
    assert_eq!(lineage["review"]["state"], "unvalidated");
    assert_eq!(lineage["inputs"]["classes"], json!(["prior_output"]));
    assert_eq!(lineage["document"]["turn"], 2);
    // A release build compiles its source revision into the writer, which pins the document; a local build does not.
    let unpinned = lineage["gaps"]
        .as_array()
        .unwrap()
        .iter()
        .any(|g| g == "image_not_digest_pinned");
    assert_eq!(unpinned, !version.contains('+'), "{lineage}");

    // The other kinds are written the same way.
    for kind in ["xlsx", "pptx"] {
        let other = api
            .request_document(a, project, run, kind, Uuid::new_v4())
            .await
            .unwrap();
        let claimed = worker
            .claim_task("document-worker", 120)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.id, other);
        document_run::execute(worker.clone(), &config, claimed, CancellationToken::new())
            .await
            .unwrap();
        let detail = api.task_document_detail(a, other).await.unwrap();
        assert_eq!(
            detail["receipts"][0]["status"], "completed",
            "{kind}: {detail}"
        );
        let path: String = owner
            .query_one("SELECT relative_path FROM enrichedlab_agent.broker_artifacts WHERE task_id=$1 AND role='result'", &[&other])
            .await
            .unwrap()
            .get(0);
        let files = members(&fs::read(root.join("artifacts").join(&path)).unwrap());
        assert!(
            files[&format!("document.{kind}")].starts_with(b"PK\x03\x04"),
            "{kind}"
        );
    }

    fs::remove_dir_all(&root).ok();
    drop(owner);
    connection.abort();
}
