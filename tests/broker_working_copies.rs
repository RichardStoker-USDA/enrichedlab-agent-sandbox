//! Working copies (migration 033) in isolated PostgreSQL: the operator's sweep reads the state of the
//! analyses its run volumes belong to and records each volume it observed and cleared; a project reader sees until
//! when each analysis keeps its working copy, and when it was cleared once every volume is gone; a cleared volume
//! stays cleared and never moves to another analysis; a reader of another lab's private project learns nothing; no
//! runtime role can record or read the sweep's view.
use enrichedlab_agent_sandbox::broker::{
    model::{PrincipalClass, SubmitTaskRequest, VerifiedIdentity},
    project::{CheckpointRequest, CreateProject, ResearchState, Visibility},
    store::{Store, StoreErrorKind},
};
use serde_json::{Value, json};
use std::{env, fs};
use tokio_postgres::{Config, NoTls, config::Host};
use uuid::Uuid;

fn research() -> ResearchState {
    serde_json::from_value(json!({"schema_version":1,"goal":"Does dose change tooth length?",
        "constraints":["Compute every value from the staged input."],
        "plan":[{"action_key":"fit-model","title":"Fit a linear model","status":"pending","task_id":null,
            "evidence_refs":[],"rationale":"Answer the question."}],
        "lessons":[],"next_action":"Run the analysis.",
        "workspace":{"schema_version":1,"profile":"general-research","image_digest":format!("sha256:{}","a".repeat(64)),
        "source_digest":format!("sha256:{}","b".repeat(64)),"input_refs":["input://public/toothgrowth"],
        "tool_refs":[],"preparation_receipt":null}})).unwrap()
}

#[tokio::test]
#[ignore = "isolated PostgreSQL through qualify_remote_broker.sh working-copies"]
async fn the_sweep_records_working_copies_and_readers_see_when_they_go() {
    let dsn = fs::read_to_string(env::var_os("BROKER_QUALIFICATION_MIGRATION_DSN_FILE").unwrap())
        .unwrap();
    let config: Config = dsn.parse().unwrap();
    assert_eq!(config.get_dbname(), Some("broker_qual"));
    assert!(matches!(config.get_hosts(),[Host::Tcp(h)] if h=="127.0.0.1"));
    let (owner, connection) = config.connect(NoTls).await.unwrap();
    let connection = tokio::spawn(connection);
    let api = Store::connect(
        &dsn.replace("user=postgres", "user=broker_api_qual"),
        "research-site".into(),
        2,
    )
    .unwrap();
    // The API stays out of service until migration 033 and its grant exist.
    api.ready_for_review_api().await.unwrap();
    owner
        .batch_execute(
            "INSERT INTO enrichedlab_agent.broker_research_groups(tenant_id,group_key,name,max_active_agents,max_active_agents_per_principal,max_queued_tasks,retention_days)
        VALUES ('research-site','other-lab','Other lab',1,1,1,30) ON CONFLICT DO NOTHING;
        INSERT INTO enrichedlab_agent.broker_principal_invitations(tenant_id,email,group_key,role) VALUES
        ('research-site','copies-owner@example.invalid','pilot-lab','researcher'),
        ('research-site','copies-outsider@example.invalid','other-lab','researcher')
        ON CONFLICT DO NOTHING;",
        )
        .await
        .unwrap();
    let person = |subject: &str, email: &str| VerifiedIdentity {
        issuer: "https://identity.example.invalid".into(),
        class: PrincipalClass::Person,
        subject: subject.into(),
        email: email.into(),
        display_name: None,
    };
    let me = api
        .resolve_principal(&person("copies-owner", "copies-owner@example.invalid"))
        .await
        .unwrap();
    let project = Uuid::new_v4();
    api.create_project(
        me,
        &CreateProject {
            project_id: project,
            group: "pilot-lab".into(),
            title: "Working copies".into(),
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
    let task = api
        .submit_project_task(
            me,
            &SubmitTaskRequest {
                group: "pilot-lab".into(),
                profile: "general-research".into(),
                objective: "Fit the model.".into(),
                project_id: Some(project),
                action_key: Some("fit-model".into()),
                parent_task_id: None,
            },
            "working-copies-1",
            &"d".repeat(64),
        )
        .await
        .unwrap();
    let hex = task.simple().to_string();
    let (input, artifacts) = (format!("input-run-{hex}"), format!("artifacts-run-{hex}"));
    let record = |items: Value| {
        let owner = &owner;
        async move {
            owner
                .query_one(
                    "SELECT enrichedlab_agent.broker_record_working_copies('research-site',$1::jsonb)",
                    &[&items],
                )
                .await
        }
    };
    let copies = || api.project_working_copies(me, project);

    // Before the sweep has observed anything, no analysis has a line.
    assert_eq!(copies().await.unwrap(), json!([]));
    // The sweep's view of the analysis: its state and how many result files custody kept.
    let view: Value = owner
        .query_one(
            "SELECT enrichedlab_agent.broker_working_copy_tasks('research-site',$1)",
            &[&vec![task, Uuid::new_v4()]],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        view.as_array().unwrap().len(),
        1,
        "an unknown task is not answered"
    );
    assert_eq!(view[0]["task_id"], json!(task));
    assert_eq!(view[0]["status"], "queued");
    assert_eq!(view[0]["retained_files"], 0);

    // Observed: both volumes, kept until their dates; the reader sees the later date.
    let n: i32 = record(json!([
        {"volume": input, "task_id": task, "role": "immutable-input", "retention_until": "2026-10-25T17:24:01Z"},
        {"volume": artifacts, "task_id": task, "role": "durable-artifacts", "retention_until": "2026-10-25T17:26:12Z"}]))
    .await
    .unwrap()
    .get(0);
    assert_eq!(n, 2);
    let seen = copies().await.unwrap();
    assert_eq!(seen[0]["task_id"], json!(task));
    assert_eq!(seen[0]["volumes"], 2);
    assert!(
        seen[0]["kept_until"]
            .as_str()
            .unwrap()
            .starts_with("2026-10-25T17:26:12")
    );
    assert!(seen[0]["cleared_at"].is_null());

    // One volume cleared: the analysis still keeps a working copy.
    let receipt = format!("sha256:{}", "e".repeat(64));
    record(
        json!([{"volume": input, "task_id": task, "role": "immutable-input",
        "retention_until": "2026-10-25T17:24:01Z", "cleared_at": "2026-10-26T03:00:00Z",
        "clear_receipt_sha256": receipt}]),
    )
    .await
    .unwrap();
    assert!(copies().await.unwrap()[0]["cleared_at"].is_null());
    // Both cleared: the reader sees when.
    record(
        json!([{"volume": artifacts, "task_id": task, "role": "durable-artifacts",
        "retention_until": "2026-10-25T17:26:12Z", "cleared_at": "2026-10-26T03:00:05Z",
        "clear_receipt_sha256": receipt}]),
    )
    .await
    .unwrap();
    let cleared = copies().await.unwrap();
    assert!(
        cleared[0]["cleared_at"]
            .as_str()
            .unwrap()
            .starts_with("2026-10-26T03:00:05")
    );

    // A cleared volume stays cleared: a later observation changes nothing.
    let n: i32 = record(
        json!([{"volume": input, "task_id": task, "role": "immutable-input",
        "retention_until": "2026-12-01T00:00:00Z"}]),
    )
    .await
    .unwrap()
    .get(0);
    assert_eq!(n, 0);
    assert_eq!(copies().await.unwrap(), cleared);
    // A volume never moves to another analysis or role, and malformed records are refused.
    for bad in [
        json!([{"volume": input, "task_id": Uuid::new_v4(), "role": "immutable-input", "retention_until": "2026-10-25T00:00:00Z"}]),
        json!([{"volume": input, "task_id": task, "role": "durable-artifacts", "retention_until": "2026-10-25T00:00:00Z"}]),
        json!([{"volume": "input-other-volume", "task_id": task, "role": "immutable-input", "retention_until": "2026-10-25T00:00:00Z"}]),
        json!([{"volume": format!("input-retained-{}", &hex[..8]), "task_id": task, "role": "immutable-input",
            "retention_until": "2026-10-25T00:00:00Z", "cleared_at": "2026-10-26T00:00:00Z"}]),
        json!({"volume": input}),
    ] {
        assert!(record(bad.clone()).await.is_err(), "{bad}");
    }
    // An unknown analysis cannot be recorded.
    assert!(
        record(
            json!([{"volume": "input-run-0123456789abcdef", "task_id": Uuid::new_v4(),
            "role": "immutable-input", "retention_until": "2026-10-25T00:00:00Z"}])
        )
        .await
        .is_err()
    );

    // A person outside the project's lab cannot read the private project's working copies.
    let outsider = api
        .resolve_principal(&person(
            "copies-outsider",
            "copies-outsider@example.invalid",
        ))
        .await
        .unwrap();
    let refused = api
        .project_working_copies(outsider, project)
        .await
        .unwrap_err();
    assert_eq!(
        (refused.kind, refused.code),
        (StoreErrorKind::NotFound, "project_not_found")
    );
    // Only the operator records and reads the sweep's view; no runtime role can.
    for role in [
        "broker_api_qual",
        "broker_worker_qual",
        "broker_custodian_qual",
    ] {
        let (direct, direct_connection) = dsn
            .replace("user=postgres", &format!("user={role}"))
            .parse::<Config>()
            .unwrap()
            .connect(NoTls)
            .await
            .unwrap();
        let direct_connection = tokio::spawn(direct_connection);
        for sql in [
            "SELECT enrichedlab_agent.broker_record_working_copies('research-site','[]'::jsonb)",
            "SELECT enrichedlab_agent.broker_working_copy_tasks('research-site','{}'::uuid[])",
            "SELECT count(*) FROM enrichedlab_agent.broker_task_working_copies",
        ] {
            assert!(direct.query(sql, &[]).await.is_err(), "{role}: {sql}");
        }
        drop(direct);
        direct_connection.await.unwrap().unwrap();
    }
    drop(owner);
    connection.await.unwrap().unwrap();
}
