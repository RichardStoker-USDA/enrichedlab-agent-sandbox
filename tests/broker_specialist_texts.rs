//! The literature specialist's kept texts (migration 050) in isolated PostgreSQL: a
//! turn keeps the specialist's instructions, context and reply and names their digests in its record under
//! `literature_claims.specialist`; a reader of the project who is a member of its lab reads each of them, while a kept text
//! no turn names stays unreadable.
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
    serde_json::from_value(json!({"schema_version":1,"goal":"Does boiling lower vitamin C?",
        "constraints":[],"plan":[],"lessons":[],"next_action":"Select an available tool.",
        "workspace":{"schema_version":1,"profile":"general-research","image_digest":format!("sha256:{}","a".repeat(64)),
        "source_digest":format!("sha256:{}","b".repeat(64)),"input_refs":["input://public/example"],"tool_refs":[],
        "preparation_receipt":null}})).unwrap()
}

#[tokio::test]
#[ignore = "isolated PostgreSQL through qualify_remote_broker.sh specialist-texts"]
async fn a_lab_member_reads_the_specialist_texts_a_turn_names_and_no_other() {
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
    // Neither the coordinator nor the API runs on a schema without migration 050.
    owner
        .execute(
            "DELETE FROM enrichedlab_agent.schema_migrations WHERE version = 50",
            &[],
        )
        .await
        .unwrap();
    assert!(coordinator.ready_for_research_results().await.is_err());
    assert!(api.ready_for_review_api().await.is_err());
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (50)",
            &[],
        )
        .await
        .unwrap();
    coordinator.ready_for_research_results().await.unwrap();
    api.ready_for_review_api().await.unwrap();
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
            title: "A question the literature specialist read for".into(),
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
    let claim = coordinator
        .claim_research_run("lead-a", 90)
        .await
        .unwrap()
        .unwrap();
    let generation = claim["run"]["generation"].as_i64().unwrap();

    // The specialist's three texts and one text no turn names, kept under the run's lease.
    let texts: [&[u8]; 4] = [
        b"You are a literature specialist.",
        br#"{"question":"Does boiling lower vitamin C?","passages":[]}"#,
        br#"{"claims":[],"off_topic":[]}"#,
        b"a kept text no turn names",
    ];
    let kept = coordinator
        .keep_research_contexts(run, "lead-a", generation, &texts)
        .await
        .unwrap();
    assert_eq!(kept.len(), 4);
    let record = json!({"schema_version":1,
        "proposal":{"action":"review_required","answer":"not_yet","summary":"Nothing to run yet.","actions":[],"lessons":[]},
        "method_review":{"decision":"proceed","reason":"Nothing to check."},
        "literature_claims":{"status":"read","passages_sha256":"c".repeat(64),
            "specialist":[{"system_sha256":kept[0],"context_sha256":kept[1],"content_sha256":kept[2]}]}});
    coordinator
        .commit_research_turn(
            run,
            "lead-a",
            generation,
            1,
            &serde_json::to_value(&state).unwrap(),
            &json!([]),
            &record,
            "review_required",
        )
        .await
        .unwrap();

    for (sha, text) in kept.iter().zip(texts).take(3) {
        let read: Value = api.research_context(me, run, sha).await.unwrap();
        assert_eq!(read["text"].as_str().unwrap().as_bytes(), text);
    }
    let unnamed = api.research_context(me, run, &kept[3]).await.unwrap_err();
    assert_eq!(
        (unnamed.kind, unnamed.code),
        (StoreErrorKind::NotFound, "context_not_found")
    );
    drop(owner);
    connection.abort();
}
