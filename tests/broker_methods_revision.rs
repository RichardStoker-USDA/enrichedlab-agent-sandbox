//! Methods review revisions (migration 049) in isolated PostgreSQL: a turn whose methods check sends
//! the proposal back commits with outcome `revise`, submits no analysis, keeps the proposal as `revised_proposal` so the
//! readers of answers skip it, and returns the run to the queue; the run's results read gives the newest turn's request
//! and how many revisions the run used; a run is sent back at most twice, and the inbox names the methods check's reason
//! for a run stopped after its revisions.
use enrichedlab_agent_sandbox::broker::{
    coordinator::StartRun,
    model::{PrincipalClass, VerifiedIdentity},
    project::{CheckpointRequest, CreateProject, ResearchState, Visibility},
    store::{Store, StoreErrorKind},
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
#[ignore = "isolated PostgreSQL through qualify_remote_broker.sh methods-revision"]
async fn a_revision_returns_the_run_to_the_lead_without_an_answer_and_within_its_budget() {
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
    let mut state = research();
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

    // 1. Before any revision the results read says none was used, from a budget of two.
    let generation = claim("lead-a").await;
    let results = coordinator
        .research_run_results(run, "lead-a", generation)
        .await
        .unwrap();
    assert_eq!(
        (
            &results["revisions"],
            &results["revision_budget"],
            &results["revision_request"]
        ),
        (&json!(0), &json!(2), &Value::Null)
    );

    // 2. A revision that also keeps the proposal as `proposal` is refused: no reader may take it for an answer.
    state.next_action =
        "The finding reads the second run while the summary describes the first.".into();
    let as_answer = coordinator
        .commit_research_turn(
            run,
            "lead-a",
            generation,
            1,
            &serde_json::to_value(&state).unwrap(),
            &json!([]),
            &json!({"schema_version":1,"proposal":proposal("Orange juice."),"method_review":review("revise")}),
            "revise",
        )
        .await
        .unwrap_err();
    assert_eq!(
        (as_answer.kind, as_answer.code),
        (StoreErrorKind::Invalid, "research_run_invalid")
    );
    // A revision with an analysis is refused too.
    let with_step = coordinator
        .commit_research_turn(
            run,
            "lead-a",
            generation,
            1,
            &serde_json::to_value(&state).unwrap(),
            &json!([{"action_key":"s-1","profile":"general-research","objective":"Run it.","request_sha256":"c".repeat(64)}]),
            &json!({"schema_version":1,"revised_proposal":proposal("Orange juice."),"method_review":review("revise")}),
            "revise",
        )
        .await
        .unwrap_err();
    assert_eq!(with_step.kind, StoreErrorKind::Invalid);

    // 3. The first revision commits no analysis and returns the run to the queue with the code `revise`.
    let committed = coordinator
        .commit_research_turn(
            run,
            "lead-a",
            generation,
            1,
            &serde_json::to_value(&state).unwrap(),
            &json!([]),
            &json!({"schema_version":1,"revised_proposal":proposal("Orange juice."),"method_review":review("revise"),
                "revision":{"used_before":0,"budget":2,"sent_back":true,"targets":["Orange juice gave a mean of {oj}."]}}),
            "revise",
        )
        .await
        .unwrap();
    assert_eq!(committed["tasks"], json!([]));
    let row = owner
        .query_one(
            "SELECT status, code, turn, failures FROM enrichedlab_agent.broker_research_runs WHERE run_id=$1",
            &[&run],
        )
        .await
        .unwrap();
    assert_eq!(
        (
            row.get::<_, String>(0),
            row.get::<_, String>(1),
            row.get::<_, i32>(2),
            row.get::<_, i32>(3)
        ),
        ("queued".to_owned(), "revise".to_owned(), 1, 0)
    );

    // 4. The lead's next claim reads the request and the count; the sent-back proposal is not an answer.
    requeue().await;
    let generation = claim("lead-b").await;
    let results = coordinator
        .research_run_results(run, "lead-b", generation)
        .await
        .unwrap();
    assert_eq!(results["revisions"], 1);
    assert_eq!(results["revision_request"]["turn"], 1);
    assert_eq!(
        results["revision_request"]["findings"],
        review("revise")["findings"]
    );
    assert_eq!(
        results["revision_request"]["targets"],
        json!(["Orange juice gave a mean of {oj}."])
    );
    assert_eq!(
        (&results["earlier_findings"], &results["latest_answer"]),
        (&Value::Null, &Value::Null)
    );

    // 5. A second revision is allowed; a third in the same run is refused.
    let revision = committed["revision"].as_i64().unwrap();
    let second = coordinator
        .commit_research_turn(
            run,
            "lead-b",
            generation,
            revision,
            &serde_json::to_value(&state).unwrap(),
            &json!([]),
            &json!({"schema_version":1,"revised_proposal":proposal("Orange juice again."),"method_review":review("revise")}),
            "revise",
        )
        .await
        .unwrap();
    requeue().await;
    let generation = claim("lead-c").await;
    let spent = coordinator
        .commit_research_turn(
            run,
            "lead-c",
            generation,
            second["revision"].as_i64().unwrap(),
            &serde_json::to_value(&state).unwrap(),
            &json!([]),
            &json!({"schema_version":1,"revised_proposal":proposal("Orange juice a third time."),"method_review":review("revise")}),
            "revise",
        )
        .await
        .unwrap_err();
    assert_eq!(
        (spent.kind, spent.code),
        (StoreErrorKind::Invalid, "revision_budget_spent")
    );

    // 6. With the budget spent the coordinator records a person's review, an answer the readers take; the request is
    // gone once the newest turn is not a revision.
    let answered = coordinator
        .commit_research_turn(
            run,
            "lead-c",
            generation,
            second["revision"].as_i64().unwrap(),
            &serde_json::to_value(&state).unwrap(),
            &json!([]),
            &json!({"schema_version":1,"proposal":proposal("Orange juice, for a person to check."),
                "method_review":review("revise"),"revision":{"used_before":2,"budget":2,"sent_back":false}}),
            "review_required",
        )
        .await
        .unwrap();
    assert_eq!(answered["turn"], 3);
    let latest: Value = owner
        .query_one(
            "SELECT record->'proposal'->'summary' FROM enrichedlab_agent.broker_research_turns WHERE run_id=$1 AND turn=3",
            &[&run],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(latest, json!("Orange juice, for a person to check."));
    let outcomes: Vec<String> = owner
        .query(
            "SELECT record->>'outcome' FROM enrichedlab_agent.broker_research_turns WHERE run_id=$1 ORDER BY turn",
            &[&run],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    assert_eq!(outcomes, ["revise", "revise", "review_required"]);
    let status: String = owner
        .query_one(
            "SELECT status FROM enrichedlab_agent.broker_research_runs WHERE run_id=$1",
            &[&run],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(status, "review_required");
    // The inbox names the methods check's reason for a run stopped after its revisions, as for `needs_review`.
    let inbox = api.inbox(me).await.unwrap();
    let request = inbox["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == "review_request" && i["run_id"] == json!(run))
        .unwrap()
        .clone();
    assert_eq!(
        (&request["reason_source"], &request["reason"]),
        (&json!("methods_check"), &review("revise")["reason"])
    );
    connection.abort();
}
