//! Migration 074 on PostgreSQL: the supervisor reads a run's faults across the whole run and pauses it
//! with a fixed reason, both only under the run's lease; the run's orientation is kept once under the lease and read
//! back by a member of the project's lab.
use enrichedlab_agent_sandbox::broker::{
    coordinator::StartRun,
    model::{PrincipalClass, VerifiedIdentity},
    project::{CheckpointRequest, CreateProject, ResearchState, Visibility},
    store::Store,
};
use serde_json::json;
use std::{env, fs};
use uuid::Uuid;

fn research() -> ResearchState {
    serde_json::from_value(json!({"schema_version":1,"goal":"Count the foods.","constraints":[],"plan":[],"lessons":[],
        "next_action":"Select an available tool.",
        "workspace":{"schema_version":1,"profile":"reviewed-r","image_digest":format!("sha256:{}","a".repeat(64)),
            "source_digest":format!("sha256:{}","b".repeat(64)),"input_refs":["input://public/example"],
            "tool_refs":["tool://reviewed/contrast"],"preparation_receipt":null}}))
    .unwrap()
}

#[tokio::test]
#[ignore = "isolated PostgreSQL through qualify_remote_broker.sh orientation"]
async fn the_supervisor_reads_faults_and_pauses_only_under_the_runs_lease() {
    let dsn = fs::read_to_string(env::var_os("BROKER_QUALIFICATION_MIGRATION_DSN_FILE").unwrap())
        .unwrap();
    let store = |user: &str| {
        Store::connect(
            &dsn.replace("user=postgres", &format!("user={user}")),
            "research-site".into(),
            2,
        )
        .unwrap()
    };
    let (api, coordinator) = (store("broker_api_qual"), store("broker_coordinator_qual"));
    coordinator.ready_for_supervisor().await.unwrap();
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
    let project = Uuid::new_v4();
    api.create_project(
        me,
        &CreateProject {
            project_id: project,
            group: "pilot-lab".into(),
            title: "Supervised".into(),
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
    let run = api
        .start_research_run(
            me,
            project,
            &StartRun {
                run_id: Uuid::new_v4(),
                expected_revision: 1,
                allowed_profiles: vec!["general-research".into()],
                max_turns: 6,
                max_parallel: 1,
            },
        )
        .await
        .unwrap();
    let claim = coordinator
        .claim_research_run("supervisor-a", 90)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim["run"]["run_id"], run.to_string());
    let generation = claim["run"]["generation"].as_i64().unwrap();

    // A fresh run has no faults and no turns.
    let faults = coordinator
        .research_run_faults(run, "supervisor-a", generation)
        .await
        .unwrap();
    assert_eq!(faults["events"], json!([]));
    assert_eq!(faults["turns"], json!([]));
    // Only the lease holder reads them.
    assert!(
        coordinator
            .research_run_faults(run, "someone-else", generation)
            .await
            .is_err()
    );
    assert!(
        coordinator
            .research_run_faults(run, "supervisor-a", generation + 1)
            .await
            .is_err()
    );

    // A turn's recorded failed results come back by task and reference only; a malformed entry is left out.
    let committed = coordinator
        .commit_research_turn(
            run,
            "supervisor-a",
            generation,
            claim["project"]["checkpoint"]["revision"].as_i64().unwrap(),
            &claim["project"]["checkpoint"]["state"],
            &json!([]),
            &json!({"schema_version": 1, "revised_proposal": {"action": "answer"},
                "failed_results": [{"task_id": "t1", "ref": "artifact://tasks/t1/r"}, {"task_id": 5, "ref": "bad"}]}),
            "revise",
        )
        .await
        .unwrap();
    assert_eq!(committed["turn"], 1);
    // A revision queues the run again after 5 seconds (migration 072).
    tokio::time::sleep(std::time::Duration::from_secs(6)).await;
    let claim = coordinator
        .claim_research_run("supervisor-a2", 90)
        .await
        .unwrap()
        .expect("the revised run is claimable again");
    let generation = claim["run"]["generation"].as_i64().unwrap();
    let faults = coordinator
        .research_run_faults(run, "supervisor-a2", generation)
        .await
        .unwrap();
    assert_eq!(
        faults["turns"],
        json!([{"turn": 1, "outcome": "revise", "tasks": [],
            "failed": [{"task_id": "t1", "ref": "artifact://tasks/t1/r"}]}])
    );

    // A refused reply is recorded and read back by code and reason, never text.
    coordinator
        .yield_research_run(
            run,
            "supervisor-a2",
            generation,
            "proposal_invalid",
            Some("proposal_shape_invalid"),
        )
        .await
        .unwrap();
    // A refused reply holds the run back 15 seconds before it may be claimed again (migration 011).
    tokio::time::sleep(std::time::Duration::from_secs(16)).await;
    let claim = coordinator
        .claim_research_run("supervisor-b", 90)
        .await
        .unwrap()
        .expect("the run is claimable again");
    let generation = claim["run"]["generation"].as_i64().unwrap();
    let faults = coordinator
        .research_run_faults(run, "supervisor-b", generation)
        .await
        .unwrap();
    let events = faults["events"].as_array().unwrap();
    assert_eq!(events.len(), 1, "{faults}");
    assert_eq!(
        (events[0]["code"].as_str(), events[0]["reason"].as_str()),
        (Some("proposal_invalid"), Some("proposal_shape_invalid"))
    );

    // The run's orientation: none yet; kept once under the lease, a second record leaves the first; a
    // record of the wrong shape and a caller without the lease are refused; a member of the lab reads the checked one.
    coordinator.ready_for_orientation().await.unwrap();
    assert_eq!(
        coordinator
            .orientation_for_run(run, "supervisor-b", generation, None)
            .await
            .unwrap(),
        None
    );
    assert!(
        api.research_orientation(me, run).await.is_err(),
        "nothing kept yet"
    );
    let invalid = json!({"schema_version": 1, "specialist": [], "status": "read", "orientation": {},
        "step_keys": ["a", "b", "c", "d"]});
    assert!(
        coordinator
            .orientation_for_run(run, "supervisor-b", generation, Some(&invalid))
            .await
            .is_err(),
        "at most three profiling steps"
    );
    let record = json!({"schema_version": 1, "specialist": [{"role": "orientation"}], "status": "read",
        "orientation": {"question": "Count the foods."}, "step_keys": ["profile-a"], "attempt": "first"});
    assert!(
        coordinator
            .orientation_for_run(run, "someone-else", generation, Some(&record))
            .await
            .is_err(),
        "only the lease holder keeps it"
    );
    let kept = coordinator
        .orientation_for_run(run, "supervisor-b", generation, Some(&record))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(kept["attempt"], "first");
    let mut second = record.clone();
    second["attempt"] = json!("second");
    let still = coordinator
        .orientation_for_run(run, "supervisor-b", generation, Some(&second))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(still["attempt"], "first", "kept once");
    let read = api.research_orientation(me, run).await.unwrap();
    assert_eq!(read["step_keys"], json!(["profile-a"]));
    // A member of the lab who is not a reader of this private project.
    let owner_dsn: tokio_postgres::Config = dsn.parse().unwrap();
    let (owner, connection) = owner_dsn.connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(connection);
    owner
        .batch_execute(
            "INSERT INTO enrichedlab_agent.broker_principal_invitations(tenant_id,email,group_key,role) VALUES
               ('research-site','outsider@example.invalid','pilot-lab','researcher') ON CONFLICT DO NOTHING;",
        )
        .await
        .unwrap();
    let outsider = api
        .resolve_principal(&VerifiedIdentity {
            issuer: "https://identity.example.invalid".into(),
            class: PrincipalClass::Person,
            subject: "outsider".into(),
            email: "outsider@example.invalid".into(),
            display_name: None,
        })
        .await
        .unwrap();
    assert!(
        api.research_orientation(outsider, run).await.is_err(),
        "not a reader of the project"
    );

    // The pause takes only the supervisor's fixed reasons, only under the lease, and leaves the run paused for a person.
    assert!(
        coordinator
            .supervisor_pause_research_run(run, "supervisor-b", generation, "because")
            .await
            .is_err()
    );
    assert!(
        coordinator
            .supervisor_pause_research_run(run, "supervisor-a", generation, "no_progress")
            .await
            .is_err()
    );
    coordinator
        .supervisor_pause_research_run(run, "supervisor-b", generation, "repeated_refusal")
        .await
        .unwrap();
    assert!(
        coordinator
            .research_run_faults(run, "supervisor-b", generation)
            .await
            .is_err(),
        "the lease is gone"
    );
    assert!(
        coordinator
            .claim_research_run("supervisor-c", 90)
            .await
            .unwrap()
            .is_none(),
        "a paused run waits for a person"
    );
    let runs = api.research_runs(me, project).await.unwrap();
    let paused = runs
        .iter()
        .find(|r| r["run_id"] == run.to_string())
        .expect("the run is listed");
    assert_eq!(paused["status"], "paused");
}

/// How the scripted model answers in one scenario. Each call is answered by its role and its count within the scenario.
#[derive(Clone, Copy, PartialEq)]
enum Scenario {
    /// A sound orientation without profiling steps; the lead pauses.
    Sound,
    /// An orientation that names a method the call was not shown, every time it is asked.
    UnknownKey,
    /// The orientation call gets a server error every time.
    OrientationDown,
    /// A sound orientation without profiling steps; the lead's first reply has the wrong shape, its second pauses.
    LeadRefusedFirst,
    /// An orientation with one small profiling query, which the turn delegates; the methods check proceeds.
    Profiles,
    /// As `Profiles`, but the first two methods calls get a server error, so the first turn fails before its commit.
    ProfilesReviewDown,
    /// An orientation with one profiling query that reads a relation the method does not list.
    ProfileRefused,
    /// An orientation with three admissible profiling queries whose record, as PostgreSQL writes it, is too large.
    ProfilesTooLarge,
}

const QUESTION: &str = "How many foods does the data count?";

fn profile_orientation(sql: &str) -> serde_json::Value {
    json!({"question": QUESTION, "sources": [{"key": "orient-sql", "why": "It holds the foods."}], "tools": [],
        "papers": [], "unknowns": ["How many foods have a name."],
        "profiles": [{"action_key": "profile-foods", "title": "How many foods there are", "profile": "orient-sql",
            "sql": sql, "expect": {"columns": ["foods"], "rows": {"min": 1, "max": 1}},
            "rationale": "Count the foods before planning."}]})
}

/// Three profiling steps, each within every limit a step has (8 KiB of query, 2 KiB of expectation): a query full of
/// separators in its one literal, and twelve ranges whose bounds jsonb writes as hundreds of digits each.
fn large_orientation() -> serde_json::Value {
    let mut o = profile_orientation("SELECT 1");
    let sql = format!(
        "SELECT count(*) AS foods FROM food WHERE description <> '{}'",
        ",:".repeat(4000)
    );
    assert!(sql.len() <= 8192);
    let ranges: Vec<serde_json::Value> = (0..12)
        .map(|_| json!({"column": "foods", "min": -1e308, "max": 1e308, "basis": "b"}))
        .collect();
    o["profiles"] = json!(
        ["profile-a", "profile-b", "profile-c"]
            .iter()
            .map(|key| json!({"action_key": key,
        "title": "How many foods there are", "profile": "orient-sql", "sql": sql,
        "expect": {"columns": ["foods"], "rows": {"min": 1, "max": 1}, "ranges": ranges},
        "rationale": "Count the foods before planning."}))
            .collect::<Vec<_>>()
    );
    o
}

/// A model that follows `Scenario`. Every request body is kept.
async fn fake_model(
    bodies: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    scenario: Scenario,
) -> u16 {
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(move |axum::Json(body): axum::Json<serde_json::Value>| {
            let bodies = bodies.clone();
            async move {
                let system = body["messages"][0]["content"].as_str().unwrap_or_default().to_owned();
                let role = if system.starts_with("You are a research lead orienting") {
                    "orientation"
                } else if system.starts_with("You are a methods specialist") {
                    "methods"
                } else {
                    "lead"
                };
                let seen = {
                    let mut all = bodies.lock().unwrap();
                    all.push(body.clone());
                    all.iter()
                        .filter(|b| {
                            let s = b["messages"][0]["content"].as_str().unwrap_or_default();
                            match role {
                                "orientation" => s.starts_with("You are a research lead orienting"),
                                "methods" => s.starts_with("You are a methods specialist"),
                                _ => !s.starts_with("You are a research lead orienting")
                                    && !s.starts_with("You are a methods specialist"),
                            }
                        })
                        .count()
                };
                let server_error = (role == "orientation" && scenario == Scenario::OrientationDown)
                    || (role == "methods" && scenario == Scenario::ProfilesReviewDown && seen <= 2);
                let content = match role {
                    "orientation" => match scenario {
                        Scenario::UnknownKey => json!({"question": QUESTION, "sources": [],
                            "tools": [{"key": "not-shown", "why": "A method it was not shown."}],
                            "papers": [], "unknowns": [], "profiles": []}),
                        Scenario::Profiles | Scenario::ProfilesReviewDown => {
                            profile_orientation("SELECT count(*) AS foods FROM food")
                        }
                        Scenario::ProfileRefused => profile_orientation("SELECT count(*) AS foods FROM pg_authid"),
                        Scenario::ProfilesTooLarge => large_orientation(),
                        _ => json!({"question": QUESTION, "sources": [{"key": "orient-sql", "why": "It holds the foods."}],
                            "tools": [], "papers": [], "unknowns": ["How the foods are grouped."], "profiles": []}),
                    },
                    "methods" => json!({"decision": "proceed", "reason": "The step is sound."}),
                    _ if scenario == Scenario::LeadRefusedFirst && seen == 1 => json!({"x": 1}),
                    _ => json!({"action": "paused", "answer": "not_yet", "summary": "The scientist chooses the grouping next.",
                        "parts": [{"part": "Which grouping to use", "state": "awaiting",
                            "reason": "The scientist chooses the grouping."}]}),
                };
                let status = if server_error {
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR
                } else {
                    axum::http::StatusCode::OK
                };
                (status, axum::Json(json!({"model": "qualification-model", "choices": [{"message":
                    {"content": content.to_string()}, "finish_reason": "stop"}],
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

fn roles(bodies: &std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>) -> Vec<&'static str> {
    bodies
        .lock()
        .unwrap()
        .iter()
        .map(|b| {
            let system = b["messages"][0]["content"].as_str().unwrap_or_default();
            if system.starts_with("You are a research lead orienting") {
                "orientation"
            } else if system.starts_with("You are a methods specialist") {
                "methods"
            } else {
                "lead"
            }
        })
        .collect()
}

fn sent(
    bodies: &std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    n: usize,
) -> serde_json::Value {
    serde_json::from_str(
        bodies.lock().unwrap()[n]["messages"][1]["content"]
            .as_str()
            .unwrap(),
    )
    .unwrap()
}

#[tokio::test]
#[ignore = "isolated PostgreSQL through qualify_remote_broker.sh orientation"]
async fn a_runs_first_turn_orients_before_the_lead_plans_and_every_later_call_reads_it() {
    use enrichedlab_agent_sandbox::broker::coordinator::CoordinatorArgs;
    use std::sync::{Arc, Mutex};
    // The service installs the same provider at start (bin/broker.rs).
    let _ = rustls::crypto::ring::default_provider().install_default();
    let dsn = fs::read_to_string(env::var_os("BROKER_QUALIFICATION_MIGRATION_DSN_FILE").unwrap())
        .unwrap();
    let login = |user: &str| dsn.replace("user=postgres", &format!("user={user}"));
    let api = Store::connect(&login("broker_api_qual"), "research-site".into(), 2).unwrap();
    let config: tokio_postgres::Config = dsn.parse().unwrap();
    let (owner, connection) = config.connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(connection);
    // A query method over a small invented table, granted to the lab, so a profiling query can be admitted.
    owner
        .batch_execute(
            "CREATE SCHEMA orient_qual;
             CREATE TABLE orient_qual.food (fdc_id integer PRIMARY KEY, description text NOT NULL);
             INSERT INTO orient_qual.food VALUES (1, 'Example food A'), (2, 'Example food B');
             INSERT INTO enrichedlab_agent.broker_agent_profiles(tenant_id,profile_key,name,description,harness,thinking,route,
               controller_profile,gateway_profile,image_ref,cpu_limit,memory_limit,state_profile,role,tool_bundle,max_run_seconds,
               max_task_attempts,max_provider_attempts)
             SELECT tenant_id,'orient-sql','Query the example foods',description,harness,thinking,'server-query',controller_profile,
               gateway_profile,image_ref,cpu_limit,memory_limit,state_profile,role,tool_bundle,max_run_seconds,1,max_provider_attempts
             FROM enrichedlab_agent.broker_agent_profiles WHERE tenant_id='research-site' AND profile_key='general-research';
             INSERT INTO enrichedlab_agent.broker_group_profile_grants(tenant_id,group_key,profile_key)
               VALUES('research-site','pilot-lab','orient-sql');",
        )
        .await
        .unwrap();
    owner
        .query_one(
            "SELECT enrichedlab_agent.broker_register_sql_source('research-site',$1)",
            &[&json!({"source": "orient-foods", "profile": "orient-sql", "title": "Example foods",
                "description": "Invented foods.", "release": "Qualification release", "schema": "orient_qual",
                "relations": [{"name": "food", "row_count": 2, "columns": [{"name": "fdc_id", "type": "integer"},
                    {"name": "description", "type": "text"}]}],
                "functions": ["count"], "row_cap": 100, "statement_timeout_ms": 5000,
                "connection_ref": "orient-qual", "snapshot": {"manifest_sha256": "e".repeat(64)}})],
        )
        .await
        .unwrap();
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
            let run = api
                .start_research_run(
                    me,
                    project,
                    &StartRun {
                        run_id: Uuid::new_v4(),
                        expected_revision: 1,
                        allowed_profiles: vec!["orient-sql".into()],
                        max_turns: 4,
                        max_parallel: 1,
                    },
                )
                .await
                .unwrap();
            (project, run)
        }
    };
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("coordinator.dsn");
    private(&database, &login("broker_coordinator_qual"));
    let coordinate = |port: u16, orientation: Vec<String>| {
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
            worker_id: Some("orientation-lead".into()),
            once: true,
            run_lane: "accepted".into(),
            lead_system_file: None,
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
            orientation_labs: orientation,
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
                .map(|row| row.get::<_, serde_json::Value>(0))
        }
    };
    let failures = |_project: Uuid, run: Uuid| {
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
    let kept = |digest: String| {
        let owner = &owner;
        async move {
            owner
                .query_one(
                    "SELECT count(*) FROM enrichedlab_agent.broker_research_contexts WHERE tenant_id='research-site' AND sha256=$1",
                    &[&digest],
                )
                .await
                .unwrap()
                .get::<_, i64>(0)
        }
    };
    let scenario = |s: Scenario| async move {
        let bodies: Arc<Mutex<Vec<serde_json::Value>>> = Arc::default();
        let port = fake_model(bodies.clone(), s).await;
        (bodies, port)
    };

    // A label that is not an action key is refused before the coordinator serves.
    let (_, port) = scenario(Scenario::Sound).await;
    assert_eq!(
        coordinate(port, vec!["pilot lab".into()])
            .await
            .unwrap_err()
            .to_string(),
        "coordinator_config_invalid"
    );

    // 1. A sound orientation is kept once, placed in the lead's call with its own note, and shown to a lab member.
    let (bodies, port) = scenario(Scenario::Sound).await;
    let (_, run) = start("Oriented").await;
    coordinate(port, vec!["pilot-lab".into()]).await.unwrap();
    assert_eq!(roles(&bodies), ["orientation", "lead", "methods"]);
    let lead = sent(&bodies, 1);
    assert_eq!(lead["orientation"]["question"], QUESTION);
    assert_eq!(lead["orientation"]["sources"][0]["key"], "orient-sql");
    assert!(
        lead["orientation"]["instructions"]
            .as_str()
            .unwrap()
            .starts_with("This is your own orientation")
    );
    let record = turn_record(run).await.expect("a committed turn");
    assert_eq!(record["proposal"]["action"], "paused", "{record}");
    assert_eq!(
        record["orientation"],
        json!({"status": "read", "step_keys": [], "delegated": false, "placed": true})
    );
    let shown = api.research_orientation(me, run).await.unwrap();
    assert_eq!(shown["status"], "read");
    assert_eq!(
        shown["orientation"]["unknowns"],
        json!(["How the foods are grouped."])
    );

    // 2. An orientation that names what the call did not show is asked again once with its fault named, then kept as
    // refused; the lead's call goes on without the section and the run's failures stay at none.
    let (bodies, port) = scenario(Scenario::UnknownKey).await;
    let (project, run) = start("Refused orientation").await;
    coordinate(port, vec!["pilot-lab".into()]).await.unwrap();
    assert_eq!(
        roles(&bodies),
        ["orientation", "orientation", "lead", "methods"]
    );
    assert_eq!(
        sent(&bodies, 1)["prior_attempt"],
        json!({"reason": "orientation_invalid", "names": ["tools 1: key_unknown"]})
    );
    assert!(sent(&bodies, 2).get("orientation").is_none());
    let record = turn_record(run).await.expect("a committed turn");
    assert_eq!(record["orientation"]["status"], "refused");
    assert_eq!(record["orientation"]["placed"], false);
    assert!(
        api.research_orientation(me, run).await.is_err(),
        "only a read orientation is shown"
    );
    assert_eq!(failures(project, run).await, 0);

    // 3. A model service fault on the orientation call (a server error, asked again once) leaves the run to plan
    // without one; the fault is the orientation's, not the run's.
    let (bodies, port) = scenario(Scenario::OrientationDown).await;
    let (project, run) = start("Orientation down").await;
    coordinate(port, vec!["pilot-lab".into()]).await.unwrap();
    assert_eq!(
        roles(&bodies),
        ["orientation", "orientation", "lead", "methods"]
    );
    assert_eq!(
        turn_record(run).await.unwrap()["orientation"]["status"],
        "refused"
    );
    assert_eq!(failures(project, run).await, 0);

    // 4. A later call of the same run reads the kept orientation and does not ask for another.
    let (bodies, port) = scenario(Scenario::LeadRefusedFirst).await;
    let (_, run) = start("Two calls").await;
    coordinate(port, vec!["pilot-lab".into()]).await.unwrap();
    assert_eq!(roles(&bodies), ["orientation", "lead"]);
    // A refused reply holds the run back 15 seconds before it may be claimed again (migration 011).
    tokio::time::sleep(std::time::Duration::from_secs(16)).await;
    coordinate(port, vec!["pilot-lab".into()]).await.unwrap();
    assert_eq!(roles(&bodies), ["orientation", "lead", "lead", "methods"]);
    assert_eq!(sent(&bodies, 2)["orientation"]["question"], QUESTION);
    assert_eq!(
        turn_record(run).await.unwrap()["proposal"]["action"],
        "paused"
    );

    // 5. A small profiling query is this turn's step: no lead call, the methods check reviews it, the record names
    // the orientation's call as the lead, and every text the turn names is kept.
    let (bodies, port) = scenario(Scenario::Profiles).await;
    let (_, run) = start("Profiled").await;
    coordinate(port, vec!["pilot-lab".into()]).await.unwrap();
    assert_eq!(roles(&bodies), ["orientation", "methods"]);
    let record = turn_record(run).await.expect("a committed turn");
    assert_eq!(
        record["orientation"],
        json!({"status": "read", "step_keys": ["profile-foods"], "delegated": true, "placed": true}),
        "{record}"
    );
    assert_eq!(record["lead"]["role"], "orientation");
    assert_eq!(
        record["proposal"]["actions"][0]["action_key"],
        "profile-foods"
    );
    assert_eq!(record["submitted_tasks"].as_array().unwrap().len(), 1);
    for receipt in ["lead", "methods_specialist"] {
        for text in ["system_sha256", "context_sha256", "content_sha256"] {
            let digest = record[receipt][text].as_str().unwrap().to_owned();
            assert_eq!(kept(digest).await, 1, "{receipt} {text}");
        }
    }
    let shown = enrichedlab_agent_sandbox::broker::coordinator::orientation_display(
        &api.research_orientation(me, run).await.unwrap(),
    );
    assert_eq!(shown["orientation"]["profiles"][0]["profile"], "orient-sql");
    assert!(
        !shown.to_string().contains("SELECT"),
        "the queries stay in the record"
    );
    // The run waits on its profiling task, which no worker here runs; paused, it is not offered to the next claim.
    api.pause_research_run(me, run).await.unwrap();

    // 6. A profiling turn that fails before its commit (the methods check's model is down) leaves its steps to the
    // next attempt, which delegates them without asking for another orientation.
    let (bodies, port) = scenario(Scenario::ProfilesReviewDown).await;
    let (_, run) = start("Profiled after a fault").await;
    coordinate(port, vec!["pilot-lab".into()]).await.unwrap();
    assert_eq!(roles(&bodies), ["orientation", "methods", "methods"]);
    assert!(turn_record(run).await.is_none(), "nothing committed");
    tokio::time::sleep(std::time::Duration::from_secs(16)).await;
    coordinate(port, vec!["pilot-lab".into()]).await.unwrap();
    assert_eq!(
        roles(&bodies),
        ["orientation", "methods", "methods", "methods"]
    );
    let record = turn_record(run).await.expect("the retried turn committed");
    assert_eq!(record["orientation"]["delegated"], true);
    assert_eq!(
        record["proposal"]["actions"][0]["action_key"],
        "profile-foods"
    );
    api.pause_research_run(me, run).await.unwrap();

    // 7. A profiling query the lead check refuses is left out of the kept record, which names the refusal, and the
    // lead plans as without profiles.
    let (bodies, port) = scenario(Scenario::ProfileRefused).await;
    let (_, run) = start("Refused profile").await;
    coordinate(port, vec!["pilot-lab".into()]).await.unwrap();
    assert_eq!(roles(&bodies), ["orientation", "lead", "methods"]);
    let record = turn_record(run).await.unwrap();
    assert_eq!(record["orientation"]["step_keys"], json!([]));
    let kept_record = owner
        .query_one(
            "SELECT record FROM enrichedlab_agent.broker_research_orientations WHERE tenant_id='research-site' AND run_id=$1",
            &[&run],
        )
        .await
        .unwrap()
        .get::<_, serde_json::Value>(0);
    assert_eq!(kept_record["orientation"]["profiles"], json!([]));
    assert!(
        !kept_record.to_string().contains("pg_authid"),
        "a refused query is not stored"
    );
    assert!(
        kept_record["profile_faults"][0]
            .as_str()
            .unwrap()
            .starts_with("profiles: ")
    );

    // 8. The size bound holds against PostgreSQL's own measure of jsonb text, numbers written without an exponent and
    // separators inside strings included; and a record over it is kept as refused, with no failure of the run.
    let sample = json!({"a": [-1e308, 1e308, 1e-300, 1.5e-7, 123, -0.25], "b": "x,y:z \u{e9} \"quoted\" \\ \n",
        "c": {"d": [{"e": 1e300}, [], {}], "f": null, "g": true}, "orientation": large_orientation()});
    let written = owner
        .query_one("SELECT octet_length($1::jsonb::text)", &[&sample])
        .await
        .unwrap()
        .get::<_, i32>(0) as usize;
    let bound = enrichedlab_agent_sandbox::broker::coordinator::orientation_stored_bytes(&sample);
    assert!(
        bound >= written,
        "bound {bound} under PostgreSQL's {written}"
    );
    assert!(
        written > serde_json::to_vec(&sample).unwrap().len() + 20_000,
        "the numbers grow as jsonb text"
    );
    let (bodies, port) = scenario(Scenario::ProfilesTooLarge).await;
    let project = Uuid::new_v4();
    api.create_project(
        me,
        &CreateProject {
            project_id: project,
            group: "pilot-lab".into(),
            title: "Too large".into(),
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
    let run = api
        .start_research_run(
            me,
            project,
            &StartRun {
                run_id: Uuid::new_v4(),
                expected_revision: 1,
                allowed_profiles: vec!["orient-sql".into()],
                max_turns: 4,
                max_parallel: 3,
            },
        )
        .await
        .unwrap();
    coordinate(port, vec!["pilot-lab".into()]).await.unwrap();
    assert_eq!(roles(&bodies), ["orientation", "lead", "methods"]);
    let record = turn_record(run).await.expect("a committed turn");
    assert_eq!(record["orientation"]["status"], "refused");
    let kept_record = owner
        .query_one(
            "SELECT record FROM enrichedlab_agent.broker_research_orientations WHERE tenant_id='research-site' AND run_id=$1",
            &[&run],
        )
        .await
        .unwrap()
        .get::<_, serde_json::Value>(0);
    assert_eq!(kept_record["faults"], json!(["too_large"]));
    assert_eq!(failures(project, run).await, 0);

    // 9. Without the lab in `--orientation-labs` no orientation call is made.
    let (bodies, port) = scenario(Scenario::Sound).await;
    let (_, run) = start("Not oriented").await;
    coordinate(port, vec![]).await.unwrap();
    assert_eq!(roles(&bodies), ["lead", "methods"]);
    assert!(turn_record(run).await.unwrap().get("orientation").is_none());

    // No run of this test is left for another to claim: the two waiting on their profiling steps are paused.
    let open = owner
        .query(
            "SELECT run_id FROM enrichedlab_agent.broker_research_runs
             WHERE tenant_id='research-site' AND status IN ('queued','running','waiting')",
            &[],
        )
        .await
        .unwrap();
    for row in open {
        api.pause_research_run(me, row.get::<_, Uuid>(0))
            .await
            .unwrap();
    }
}
