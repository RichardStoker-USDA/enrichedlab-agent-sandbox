//! Query facts after a revision (migration 051) in isolated PostgreSQL: a turn whose methods check
//! sends the proposal back keeps the facts of its query steps under `revision.query_facts`, and the run's results read
//! gives them with the revision request for the lead's next call; a request whose turn kept none gives an empty list,
//! and the coordinator does not run on a schema without migration 051.
use enrichedlab_agent_sandbox::broker::{
    coordinator::StartRun,
    model::{PrincipalClass, VerifiedIdentity},
    project::{CheckpointRequest, CreateProject, ResearchState, Visibility},
    store::Store,
};
use serde_json::{Value, json};
use std::{env, fs};
use tokio_postgres::{Config, NoTls, config::Host};
use uuid::Uuid;

fn research() -> ResearchState {
    serde_json::from_value(json!({"schema_version":1,"goal":"Which supplement gave longer teeth?",
        "constraints":["Use the animal as the experimental unit."],"plan":[],"lessons":[],"next_action":"Select an available tool.",
        "workspace":{"schema_version":1,"profile":"general-research","image_digest":format!("sha256:{}","a".repeat(64)),
        "source_digest":format!("sha256:{}","b".repeat(64)),"input_refs":["input://public/example"],"tool_refs":[],
        "preparation_receipt":null}})).unwrap()
}

fn proposal(summary: &str) -> Value {
    json!({"action":"review_required","answer":"answered","summary":summary,"actions":[],"lessons":[],
        "findings":[{"statement":"Orange juice gave a mean of {oj}.",
            "bindings":{"oj":{"ref":"artifact://tasks/x/y","table":"results/means.csv","row":1,"column":"mean"}}}]})
}

fn review(decision: &str) -> Value {
    json!({"decision":decision,"reason":"The finding reads the second run while the summary describes the first.",
        "findings":[{"about":"finding 1","problem":"wrong_rows","detail":"The finding reads the second run's table."}]})
}

#[tokio::test]
#[ignore = "isolated PostgreSQL through qualify_remote_broker.sh query-facts"]
async fn a_revisions_query_facts_reach_the_leads_next_call() {
    let dsn = fs::read_to_string(env::var_os("BROKER_QUALIFICATION_MIGRATION_DSN_FILE").unwrap())
        .unwrap();
    let config: Config = dsn.parse().unwrap();
    assert_eq!(config.get_dbname(), Some("broker_qual"));
    assert!(matches!(config.get_hosts(),[Host::Tcp(h)] if h=="127.0.0.1"));
    let (owner, connection) = config.connect(NoTls).await.unwrap();
    let connection = tokio::spawn(connection);
    let store = |user: &str| {
        Store::connect(
            &dsn.replace("user=postgres", &format!("user={user}")),
            "research-site".into(),
            2,
        )
        .unwrap()
    };
    let api = store("broker_api_qual");
    let coordinator = store("broker_coordinator_qual");
    owner
        .execute(
            "DELETE FROM enrichedlab_agent.schema_migrations WHERE version = 51",
            &[],
        )
        .await
        .unwrap();
    assert!(coordinator.ready_for_research_results().await.is_err());
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (51)",
            &[],
        )
        .await
        .unwrap();
    coordinator.ready_for_research_results().await.unwrap();
    let me = api
        .resolve_principal(&VerifiedIdentity {
            issuer: "https://identity.example.invalid".into(),
            class: PrincipalClass::Person,
            subject: "owner".into(),
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
            title: "A question the check sends back".into(),
            visibility: Visibility::Private,
        },
    )
    .await
    .unwrap();
    let state = research();
    api.checkpoint_project(
        me,
        project,
        &CheckpointRequest {
            expected_revision: 0,
            request_id: Uuid::new_v4(),
            state: state.clone(),
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
                max_turns: 8,
                max_parallel: 2,
            },
        )
        .await
        .unwrap();
    let requeue = || async {
        owner
            .execute(
                "UPDATE enrichedlab_agent.broker_research_runs SET available_at=clock_timestamp()-interval '1 second'
                 WHERE run_id=$1",
                &[&run],
            )
            .await
            .unwrap();
    };
    let claim = |worker: &'static str| {
        let coordinator = coordinator.clone();
        async move {
            let claim = coordinator
                .claim_research_run(worker, 90)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(claim["run"]["run_id"], json!(run));
            claim["run"]["generation"].as_i64().unwrap()
        }
    };

    let facts = json!([{"step": "q-1", "title": "Vitamin C by food", "facts": [
        {"kind": "entry", "column": "nutrient.name", "value": "Vitamin C", "found": false, "complete": true,
         "nearest": [{"key": 1162, "label": "Vitamin C, total ascorbic acid", "unit_name": "MG", "rows": 2}]}]}]);
    let generation = claim("lead-a").await;
    let committed = coordinator
        .commit_research_turn(
            run,
            "lead-a",
            generation,
            1,
            &serde_json::to_value(&state).unwrap(),
            &json!([]),
            &json!({"schema_version":1,"revised_proposal":proposal("Vitamin C."),"method_review":review("revise"),
                "revision":{"used_before":0,"budget":2,"sent_back":true,"targets":["Orange juice gave a mean of {oj}."],
                    "query_facts":facts},
                "query_facts":{"steps":facts,"steps_omitted":0}}),
            "revise",
        )
        .await
        .unwrap();
    requeue().await;
    let generation = claim("lead-b").await;
    let results = coordinator
        .research_run_results(run, "lead-b", generation)
        .await
        .unwrap();
    assert_eq!(results["revision_request"]["query_facts"], facts);
    assert_eq!(results["revision_request"]["turn"], 1);
    // A revision that kept no facts gives an empty list.
    let second = coordinator
        .commit_research_turn(
            run,
            "lead-b",
            generation,
            committed["revision"].as_i64().unwrap(),
            &serde_json::to_value(&state).unwrap(),
            &json!([]),
            &json!({"schema_version":1,"revised_proposal":proposal("Vitamin C again."),"method_review":review("revise")}),
            "revise",
        )
        .await
        .unwrap();
    assert_eq!(second["tasks"], json!([]));
    requeue().await;
    let generation = claim("lead-c").await;
    let results = coordinator
        .research_run_results(run, "lead-c", generation)
        .await
        .unwrap();
    assert_eq!(results["revision_request"]["query_facts"], json!([]));
    drop(owner);
    connection.abort();
}
