//! Project list summary (migration 020) in isolated PostgreSQL: each listed project names who
//! started it, its question, the reader's own access, its latest work session and its analysis counts; a colleague's
//! private project stays unlisted, and a colleague's lab project is listed with the colleague's name and read access.
use enrichedlab_agent_sandbox::broker::{
    coordinator::StartRun,
    model::{PrincipalClass, SubmitTaskRequest, VerifiedIdentity},
    project::{CheckpointRequest, CreateProject, ResearchState, Visibility},
    store::Store,
};
use serde_json::{Value, json};
use std::{env, fs};
use tokio_postgres::{Config, NoTls, config::Host};
use uuid::Uuid;

fn research(goal: &str) -> ResearchState {
    serde_json::from_value(json!({"schema_version":1,"goal":goal,
        "constraints":["Compute every value from the staged input."],
        "plan":[{"action_key":"fit-model","title":"Fit a linear model","status":"pending","task_id":null,
            "evidence_refs":[],"rationale":"Answer the question."}],
        "lessons":[],"next_action":"Run the analysis.",
        "workspace":{"schema_version":1,"profile":"general-research","image_digest":format!("sha256:{}","a".repeat(64)),
        "source_digest":format!("sha256:{}","b".repeat(64)),"input_refs":["input://public/toothgrowth"],
        "tool_refs":[],"preparation_receipt":null}})).unwrap()
}

fn find(list: &[Value], project: Uuid) -> Option<&Value> {
    list.iter()
        .find(|p| p["project_id"].as_str() == Some(&project.to_string()))
}

#[tokio::test]
#[ignore = "isolated PostgreSQL through qualify_remote_broker.sh project-list"]
async fn the_list_says_who_started_each_project_and_where_it_stands() {
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
    api.ready_for_review_api().await.unwrap();
    owner
        .batch_execute(
            "INSERT INTO enrichedlab_agent.broker_principal_invitations(tenant_id,email,group_key,role) VALUES
        ('research-site','colleague@example.invalid','pilot-lab','researcher');",
        )
        .await
        .unwrap();
    let person = |subject: &str, email: &str, name: Option<&str>| VerifiedIdentity {
        issuer: "https://identity.example.invalid".into(),
        class: PrincipalClass::Person,
        subject: subject.into(),
        email: email.into(),
        display_name: name.map(Into::into),
    };
    let me = api
        .resolve_principal(&person("me", "researcher@example.invalid", None))
        .await
        .unwrap();
    let colleague = api
        .resolve_principal(&person(
            "colleague",
            "colleague@example.invalid",
            Some("Example Colleague"),
        ))
        .await
        .unwrap();
    let create = |title: &str, visibility| CreateProject {
        project_id: Uuid::new_v4(),
        group: "pilot-lab".into(),
        title: title.into(),
        visibility,
    };

    // Mine, with a question, one analysis and a work session.
    let mine = create("My study", Visibility::Private);
    api.create_project(me, &mine).await.unwrap();
    api.checkpoint_project(
        me,
        mine.project_id,
        &CheckpointRequest {
            expected_revision: 0,
            request_id: Uuid::new_v4(),
            state: research("Does dose change tooth length?"),
        },
    )
    .await
    .unwrap();
    api.submit_project_task(
        me,
        &SubmitTaskRequest {
            group: "pilot-lab".into(),
            profile: "general-research".into(),
            objective: "Fit the model.".into(),
            project_id: Some(mine.project_id),
            action_key: Some("fit-model".into()),
            parent_task_id: None,
        },
        "list-summary-1",
        &"d".repeat(64),
    )
    .await
    .unwrap();
    let run = api
        .start_research_run(
            me,
            mine.project_id,
            &StartRun {
                run_id: Uuid::new_v4(),
                expected_revision: 1,
                allowed_profiles: vec!["general-research".into()],
                max_turns: 4,
                max_parallel: 1,
            },
        )
        .await
        .unwrap();
    // A colleague's lab project, and a colleague's private one.
    let shared = create("Colleague's lab study", Visibility::Lab);
    api.create_project(colleague, &shared).await.unwrap();
    let hidden = create("Colleague's private study", Visibility::Private);
    api.create_project(colleague, &hidden).await.unwrap();

    let list = api.list_projects(me, 200).await.unwrap();
    assert!(
        find(&list, hidden.project_id).is_none(),
        "a colleague's private project is not listed"
    );

    let m = find(&list, mine.project_id).unwrap();
    assert_eq!(m["owner"]["principal_id"], json!(me));
    assert_eq!(m["owner"]["kind"], "person");
    assert_eq!(m["question"], "Does dose change tooth length?");
    assert_eq!(
        m["capabilities"],
        json!(["read", "review", "contribute", "steer", "cancel", "manage"])
    );
    assert_eq!(m["latest_run"]["run_id"], json!(run));
    assert_eq!(m["latest_run"]["status"], "queued");
    assert_eq!(m["latest_run"]["max_turns"], 4);
    assert_eq!(
        m["task_counts"],
        json!({"total":1,"active":1,"finished":0,"failed":0,"reviewed":0,"rejected":0,"changes":0})
    );
    assert!(m.get("tenant_id").is_none());

    let s = find(&list, shared.project_id).unwrap();
    assert_eq!(s["owner"]["principal_id"], json!(colleague));
    assert_eq!(s["owner"]["display_name"], "Example Colleague");
    assert_eq!(s["capabilities"], json!(["read"]));
    assert!(s["question"].is_null(), "no question saved yet");
    assert!(s["latest_run"].is_null());
    assert_eq!(
        s["task_counts"],
        json!({"total":0,"active":0,"finished":0,"failed":0,"reviewed":0,"rejected":0,"changes":0})
    );

    // The colleague sees their own two projects and not mine.
    let theirs = api.list_projects(colleague, 200).await.unwrap();
    assert!(find(&theirs, mine.project_id).is_none());
    assert!(find(&theirs, hidden.project_id).is_some());
    let own = find(&theirs, shared.project_id).unwrap();
    assert_eq!(
        own["capabilities"],
        json!(["read", "review", "contribute", "steer", "cancel", "manage"])
    );

    drop(owner);
    connection.abort();
}
