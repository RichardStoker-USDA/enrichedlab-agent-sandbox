//! Migration 088 and the lead's working memory on PostgreSQL, through the coordinator with a scripted model: for a lab
//! in `--working-memory-labs` a checked call after each committed decision keeps the memory on the turn record, the
//! run's next lead call reads the newest accepted memory as one section, a refused or failed call leaves only its codes
//! and costs the run no failure, the read is the run holder's alone, and a lab not listed makes no memory call.
use enrichedlab_agent_sandbox::broker::{
    coordinator::{CoordinatorArgs, StartRun},
    model::{PrincipalClass, VerifiedIdentity},
    project::{CheckpointRequest, CreateProject, ResearchState, Visibility},
    store::Store,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};
use std::{env, fs};
use uuid::Uuid;

const MEMORY_SYSTEM: &str = "You keep a research lead's working memory";

fn research() -> ResearchState {
    serde_json::from_value(json!({"schema_version":1,"goal":"Count the foods.","constraints":[],
        "plan":[{"action_key":"count-foods","title":"Count the foods","status":"pending","task_id":null,
            "evidence_refs":[],"rationale":"The count answers the question."}],
        "lessons":[],"next_action":"Select an available tool.",
        "workspace":{"schema_version":1,"profile":"reviewed-r","image_digest":format!("sha256:{}","a".repeat(64)),
            "source_digest":format!("sha256:{}","b".repeat(64)),"input_refs":["input://public/example"],
            "tool_refs":["tool://reviewed/contrast"],"preparation_receipt":null}}))
    .unwrap()
}

#[derive(Clone, Copy, PartialEq)]
enum Scenario {
    /// A sound memory: an open thread, a dependency of the planned step and an unknown.
    Sound,
    /// A memory that rules something out by a ref the call was not shown, every time it is asked.
    UnknownRef,
    /// The memory call gets a server error every time.
    MemoryDown,
}

fn role(body: &Value) -> &'static str {
    let system = body["messages"][0]["content"].as_str().unwrap_or_default();
    if system.starts_with(MEMORY_SYSTEM) {
        "working_memory"
    } else if system.starts_with("You are a methods specialist") {
        "methods"
    } else {
        "lead"
    }
}

async fn fake_model(bodies: Arc<Mutex<Vec<Value>>>, scenario: Scenario) -> u16 {
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
            let bodies = bodies.clone();
            async move {
                bodies.lock().unwrap().push(body.clone());
                let role = role(&body);
                let content = match role {
                    "working_memory" => match scenario {
                        Scenario::UnknownRef => json!({"open_threads": [], "depends_on": [], "unknowns": [],
                            "ruled_out": [{"what": "Counting by name.", "why_ref": "library://not/shown", "under": []}]}),
                        _ => json!({"open_threads": ["Whether duplicates are counted once."], "ruled_out": [],
                            "depends_on": [{"next_step_key": "count-foods", "needs": []}],
                            "unknowns": ["How the foods are grouped."]}),
                    },
                    "methods" => json!({"decision": "proceed", "reason": "The step is sound."}),
                    _ => json!({"action": "paused", "answer": "not_yet", "summary": "The scientist chooses the grouping next.",
                        "parts": [{"part": "Which grouping to use", "state": "awaiting",
                            "reason": "The scientist chooses the grouping."}]}),
                };
                let status = if role == "working_memory" && scenario == Scenario::MemoryDown {
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR
                } else {
                    axum::http::StatusCode::OK
                };
                let mut message = json!({"content": content.to_string()});
                if role == "working_memory" {
                    message["reasoning_content"] = json!("The memory call's own reasoning.");
                }
                (status, axum::Json(json!({"model": "qualification-model", "choices": [{"message": message,
                    "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 10, "completion_tokens": 5}})))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    port
}

fn private(path: &std::path::Path, contents: &str) {
    use std::os::unix::fs::PermissionsExt;
    fs::write(path, contents).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn roles(bodies: &Arc<Mutex<Vec<Value>>>) -> Vec<&'static str> {
    bodies.lock().unwrap().iter().map(role).collect()
}

fn sent(bodies: &Arc<Mutex<Vec<Value>>>, n: usize) -> Value {
    serde_json::from_str(
        bodies.lock().unwrap()[n]["messages"][1]["content"]
            .as_str()
            .unwrap(),
    )
    .unwrap()
}

#[tokio::test]
#[ignore = "isolated PostgreSQL through qualify_remote_broker.sh working-memory"]
async fn a_checked_memory_is_kept_on_the_turn_and_the_runs_next_lead_call_reads_it() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let dsn = fs::read_to_string(env::var_os("BROKER_QUALIFICATION_MIGRATION_DSN_FILE").unwrap())
        .unwrap();
    let login = |user: &str| dsn.replace("user=postgres", &format!("user={user}"));
    let api = Store::connect(&login("broker_api_qual"), "research-site".into(), 2).unwrap();
    let config: tokio_postgres::Config = dsn.parse().unwrap();
    let (owner, connection) = config.connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(connection);
    let me = api
        .resolve_principal(&VerifiedIdentity {
            issuer: "https://identity.example.invalid".into(),
            class: PrincipalClass::Person,
            subject: "researcher".into(),
            email: "researcher@example.invalid".into(),
            display_name: None,
        })
        .await
        .unwrap();
    let start = |title: &'static str| {
        let api = &api;
        async move {
            let project = Uuid::new_v4();
            api.create_project(
                me,
                &CreateProject {
                    project_id: project,
                    group: "pilot-lab".into(),
                    title: title.into(),
                    visibility: Visibility::Private,
                },
            )
            .await
            .unwrap();
            api.checkpoint_project(
                me,
                project,
                &CheckpointRequest {
                    expected_revision: 0,
                    request_id: Uuid::new_v4(),
                    state: research(),
                },
            )
            .await
            .unwrap();
            api.start_research_run(
                me,
                project,
                &StartRun {
                    run_id: Uuid::new_v4(),
                    expected_revision: 1,
                    allowed_profiles: vec!["general-research".into()],
                    max_turns: 4,
                    max_parallel: 1,
                },
            )
            .await
            .unwrap()
        }
    };
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("coordinator.dsn");
    private(&database, &login("broker_coordinator_qual"));
    let coordinate = |port: u16, labs: Vec<String>| {
        let model = dir.path().join(format!("model-{port}.json"));
        private(
            &model,
            &json!({"schema_version": 1, "endpoint": format!("http://127.0.0.1:{port}/v1/chat/completions"),
                "model": "qualification-model", "allow_internal_http": true, "max_input_bytes": 65536,
                "max_output_tokens": 1024, "context_window_tokens": 262144, "timeout_seconds": 30})
            .to_string(),
        );
        enrichedlab_agent_sandbox::broker::coordinator::run(CoordinatorArgs {
            database_dsn_file: database.clone(),
            tenant: "research-site".into(),
            model_config_file: model,
            max_parallel: 1,
            worker_id: Some("memory-lead".into()),
            once: true,
            run_lane: "accepted".into(),
            lead_system_file: None,
            model_profile_file: None,
            library_config_file: None,
            lead_text_labs: vec![],
            lead_text_unreviewed_labs: vec![],
            literature_claims_labs: vec![],
            knowledge_config_file: None,
            research_catalog_labs: vec![],
            lead_reading_labs: vec![],
            lead_reading_bytes: 0,
            lead_study_fields: vec![],
            literature_review_labs: vec![],
            literature_review_bytes: 32768,
            supervisor_labs: vec![],
            orientation_labs: vec![],
            working_memory_labs: labs,
            ready_file: None,
        })
    };
    let turn_record = |run: Uuid| {
        let owner = &owner;
        async move {
            owner
                .query_opt(
                    "SELECT record FROM enrichedlab_agent.broker_research_turns
                     WHERE tenant_id='research-site' AND run_id=$1 ORDER BY turn DESC LIMIT 1",
                    &[&run],
                )
                .await
                .unwrap()
                .map(|row| row.get::<_, Value>(0))
        }
    };
    let failures = |run: Uuid| {
        let owner = &owner;
        async move {
            owner
                .query_one(
                    "SELECT failures FROM enrichedlab_agent.broker_research_runs WHERE tenant_id='research-site' AND run_id=$1",
                    &[&run],
                )
                .await
                .unwrap()
                .get::<_, i32>(0)
        }
    };
    let scenario = |s: Scenario| async move {
        let bodies: Arc<Mutex<Vec<Value>>> = Arc::default();
        let port = fake_model(bodies.clone(), s).await;
        (bodies, port)
    };
    let listed = || vec!["pilot-lab".to_owned()];

    // A label that is not an action key is refused before the coordinator serves.
    let (_, port) = scenario(Scenario::Sound).await;
    assert_eq!(
        coordinate(port, vec!["pilot lab".into()])
            .await
            .unwrap_err()
            .to_string(),
        "coordinator_config_invalid"
    );

    // 1. Without the lab listed no memory call is made and the record holds no memory.
    let (bodies, port) = scenario(Scenario::Sound).await;
    let run = start("Not listed").await;
    coordinate(port, vec![]).await.unwrap();
    assert_eq!(roles(&bodies), ["lead", "methods"]);
    let record = turn_record(run).await.expect("a committed turn");
    assert!(record.get("working_memory").is_none() && record.get("working_memory_call").is_none());
    // Every turn record names the harness manifest the coordinator stored at start, and the stored bytes hash to it.
    let manifest = record["harness_manifest_sha256"]
        .as_str()
        .expect("a manifest digest")
        .to_owned();
    let (content, schema): (Vec<u8>, Option<String>) = {
        let row = owner
            .query_one(
                "SELECT content, convert_from(content,'UTF8')::jsonb->>'schema_version'
                 FROM enrichedlab_agent.broker_harness_manifests WHERE tenant_id='research-site' AND sha256=$1",
                &[&manifest],
            )
            .await
            .unwrap();
        (row.get(0), row.get(1))
    };
    assert_eq!(
        manifest,
        format!("sha256:{}", hex::encode(Sha256::digest(&content)))
    );
    assert_eq!(schema.as_deref(), Some("1"));
    // The table refuses a change, and the record function refuses bytes whose digest is not the one given.
    let changed = owner
        .execute(
            "UPDATE enrichedlab_agent.broker_harness_manifests SET created_at=created_at WHERE sha256=$1",
            &[&manifest],
        )
        .await;
    assert!(changed.is_err());
    let refused = owner
        .query_one(
            "SELECT enrichedlab_agent.broker_record_harness_manifest('research-site',$1,'{\"schema_version\":1,\"surfaces\":{}}'::bytea)",
            &[&manifest],
        )
        .await
        .unwrap_err();
    assert_eq!(
        refused.as_db_error().map(|e| e.message()),
        Some("broker:harness_manifest_invalid")
    );
    // A schema version written as a string is refused, even with its own digest.
    let text = br#"{"schema_version":"1","surfaces":{}}"#.to_vec();
    let digest = format!("sha256:{}", hex::encode(Sha256::digest(&text)));
    let refused = owner
        .query_one(
            "SELECT enrichedlab_agent.broker_record_harness_manifest('research-site',$1,$2)",
            &[&digest, &text],
        )
        .await
        .unwrap_err();
    assert_eq!(
        refused.as_db_error().map(|e| e.message()),
        Some("broker:harness_manifest_invalid")
    );

    // 2. A sound memory follows the decision and is kept on its record with its digest; the run's first call had
    // none to read.
    let (bodies, port) = scenario(Scenario::Sound).await;
    let run = start("Remembered").await;
    coordinate(port, listed()).await.unwrap();
    assert_eq!(roles(&bodies), ["lead", "methods", "working_memory"]);
    assert!(sent(&bodies, 0).get("working_memory").is_none());
    let ask = sent(&bodies, 2);
    assert_eq!(
        ask["newest_turn"]["summary"],
        "The scientist chooses the grouping next."
    );
    assert!(ask["context"].get("methods_review").is_none());
    let record = turn_record(run).await.expect("a committed turn");
    assert_eq!(
        record["working_memory"]["depends_on"][0]["next_step_key"], "count-foods",
        "{record}"
    );
    assert!(
        record["working_memory_sha256"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert_eq!(record["working_memory_call"]["status"], "accepted");
    assert_eq!(record["working_memory_call"]["placed"], false);
    let receipt = &record["working_memory_call"]["specialist"][0];
    let kept: i64 = owner
        .query_one(
            "SELECT count(*) FROM enrichedlab_agent.broker_research_contexts WHERE tenant_id='research-site' AND sha256=$1",
            &[&receipt["context_sha256"].as_str().unwrap()],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(kept, 1, "the memory call's context is kept");
    // A lab member reads the memory call's kept context by digest, as every model call's.
    let shown = api
        .research_context(me, run, receipt["context_sha256"].as_str().unwrap())
        .await
        .unwrap();
    let read: Value = serde_json::from_str(shown["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        read["newest_turn"]["summary"],
        "The scientist chooses the grouping next."
    );
    assert!(
        read["context"].get("methods").is_none(),
        "the call reads only its view of the context"
    );
    // A package with reasoning lists the memory call's reasoning, and the run's orientation calls' too.
    let orientation_reasoning = "The orientation call's own reasoning.";
    let digest: String = owner
        .query_one(
            "INSERT INTO enrichedlab_agent.broker_research_contexts(tenant_id,sha256,content)
             VALUES ('research-site','sha256:'||encode(sha256($1::bytea),'hex'),$1::bytea) RETURNING sha256",
            &[&orientation_reasoning.as_bytes()],
        )
        .await
        .unwrap()
        .get(0);
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_research_orientations(tenant_id,run_id,record) VALUES ('research-site',$1,$2)",
            &[&run, &json!({"schema_version": 1, "status": "refused", "faults": ["shape"],
                "specialist": [{"reasoning_sha256": digest}]})],
        )
        .await
        .unwrap();
    let project: Uuid = owner
        .query_one(
            "SELECT project_id FROM enrichedlab_agent.broker_research_runs WHERE run_id=$1",
            &[&run],
        )
        .await
        .unwrap()
        .get(0);
    let components: Value = owner
        .query_one(
            "SELECT enrichedlab_agent.broker_project_package_components('research-site',$1,$2)",
            &[&me, &project],
        )
        .await
        .unwrap()
        .get(0);
    let reasoning: Vec<(String, String)> = components["reasoning"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["role"].as_str().unwrap().to_owned(),
                r["text"].as_str().unwrap().to_owned(),
            )
        })
        .filter(|(role, _)| role == "working_memory" || role == "orientation")
        .collect();
    assert_eq!(
        reasoning,
        vec![
            ("orientation".to_owned(), orientation_reasoning.to_owned()),
            (
                "working_memory".to_owned(),
                "The memory call's own reasoning.".to_owned()
            ),
        ]
    );

    // 3. A memory kept by an earlier turn of the run is read back under the run's lease and placed in the next lead
    // call with its own note; the new decision keeps a new memory.
    let run = start("Read back").await;
    let earlier = json!({"open_threads": ["An earlier open thread."], "ruled_out": [], "depends_on": [],
        "unknowns": []});
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_research_runs SET turn=1 WHERE tenant_id='research-site' AND run_id=$1",
            &[&run],
        )
        .await
        .unwrap();
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_research_turns
               (tenant_id,run_id,turn,generation,context_revision,checkpoint_revision,record)
             VALUES ('research-site',$1,0,0,1,1,$2)",
            &[&run, &json!({"schema_version": 1, "working_memory": earlier, "working_memory_sha256": "sha256:x"})],
        )
        .await
        .unwrap();
    let (bodies, port) = scenario(Scenario::Sound).await;
    coordinate(port, listed()).await.unwrap();
    assert_eq!(roles(&bodies), ["lead", "methods", "working_memory"]);
    let lead = sent(&bodies, 0);
    assert_eq!(
        lead["working_memory"]["open_threads"],
        json!(["An earlier open thread."])
    );
    assert!(
        lead["working_memory"]["instructions"]
            .as_str()
            .unwrap()
            .starts_with("This is what you were")
    );
    let record = turn_record(run).await.expect("the second turn");
    assert_eq!(record["working_memory_call"]["placed"], true);
    assert_eq!(
        record["working_memory"]["open_threads"],
        json!(["Whether duplicates are counted once."])
    );

    // 4. A memory that names a ref the call did not show is asked again once with its fault named, then left out: the
    // record keeps only the codes and the run counts no failure.
    let (bodies, port) = scenario(Scenario::UnknownRef).await;
    let run = start("Refused memory").await;
    coordinate(port, listed()).await.unwrap();
    assert_eq!(
        roles(&bodies),
        ["lead", "methods", "working_memory", "working_memory"]
    );
    assert_eq!(
        sent(&bodies, 3)["prior_attempt"],
        json!({"reason": "working_memory_invalid", "names": ["ruled_out 1: why_ref ref_unknown"]})
    );
    let record = turn_record(run).await.expect("a committed turn");
    assert!(record.get("working_memory").is_none());
    assert_eq!(record["working_memory_call"]["status"], "refused");
    assert_eq!(
        record["working_memory_call"]["faults"],
        json!(["ruled_out 1: why_ref ref_unknown"])
    );
    assert_eq!(failures(run).await, 0);

    // 5. A model service fault on the memory call (a server error, asked again once) commits the turn without a memory.
    let (bodies, port) = scenario(Scenario::MemoryDown).await;
    let run = start("Memory down").await;
    coordinate(port, listed()).await.unwrap();
    assert_eq!(
        roles(&bodies),
        ["lead", "methods", "working_memory", "working_memory"]
    );
    let record = turn_record(run).await.expect("a committed turn");
    assert_eq!(record["working_memory_call"]["status"], "failed");
    assert_eq!(failures(run).await, 0);

    // 6. Only the run's holder reads its memory: without the lease the read refuses.
    let (client, raw) = login("broker_coordinator_qual")
        .parse::<tokio_postgres::Config>()
        .unwrap()
        .connect(tokio_postgres::NoTls)
        .await
        .unwrap();
    let raw = tokio::spawn(raw);
    let refused = client
        .query_one(
            "SELECT enrichedlab_agent.broker_research_run_working_memory('research-site',$1,'memory-lead',0::bigint)",
            &[&run],
        )
        .await
        .unwrap_err();
    assert_eq!(
        refused.as_db_error().map(|e| e.message()),
        Some("broker:lease_fence_lost"),
        "{refused:?}"
    );
    let denied = api_denied(&login("broker_api_qual"), run).await;
    assert!(denied, "the API role cannot read a run's memory");
    raw.abort();
}

/// Whether the API role is refused the coordinator's read.
async fn api_denied(dsn: &str, run: Uuid) -> bool {
    let (client, raw) = dsn
        .parse::<tokio_postgres::Config>()
        .unwrap()
        .connect(tokio_postgres::NoTls)
        .await
        .unwrap();
    let raw = tokio::spawn(raw);
    let error = client
        .query_one(
            "SELECT enrichedlab_agent.broker_research_run_working_memory('research-site',$1,'memory-lead',1::bigint)",
            &[&run],
        )
        .await
        .unwrap_err();
    raw.abort();
    error.code() == Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE)
}
