use enrichedlab_agent_sandbox::broker::{
    model::{PrincipalClass, SubmitTaskRequest, VerifiedIdentity},
    project::{Capability, CheckpointRequest, CreateProject, ResearchState, Visibility},
    store::Store,
};
use serde_json::json;
use std::{env, fs};
use tokio_postgres::{Config, NoTls, config::Host};
use uuid::Uuid;

fn state() -> ResearchState {
    serde_json::from_value(json!({
        "schema_version":1,"goal":"Estimate a treatment contrast using the retained public input.",
        "constraints":["Treat the animal as the experimental unit.","Do not invent or retype input values."],
        "plan":[{"action_key":"inspect-input","title":"Inspect source and units","status":"uncertain","task_id":null,"evidence_refs":[],"rationale":"Check the task receipt before retrying after interruption."}],
        "lessons":[{"statement":"The input may require a paired design.","kind":"hypothesis","evidence_refs":[]}],
        "next_action":"Resolve the uncertain inspection against broker task state.",
        "workspace":{"schema_version":1,"profile":"reviewed-r","image_digest":format!("sha256:{}","a".repeat(64)),"source_digest":format!("sha256:{}","b".repeat(64)),"input_refs":["input://public/example"],"tool_refs":["tool://reviewed/contrast"],"preparation_receipt":null}
    })).unwrap()
}

#[test]
fn context_keeps_uncertainty_action_identity_and_source_without_transcripts() {
    let mut research = state();
    for i in 0..30 {
        research.lessons.push(serde_json::from_value(json!({"statement":format!("Candidate interpretation {i}: {}","x".repeat(600)),"kind":"hypothesis","evidence_refs":[]})).unwrap());
    }
    let context = research.context(4, 4096).unwrap();
    assert_eq!(context["goal"], research.goal);
    assert_eq!(context["plan"][0]["status"], "uncertain");
    assert_eq!(context["plan"][0]["action_key"], "inspect-input");
    assert!(context["omitted_lessons"].as_u64().unwrap() > 0);
    assert!(serde_json::to_vec(&context).unwrap().len() <= 4096);
    research.constraints = vec!["x".repeat(1024); 32];
    assert_eq!(
        research.context(4, 4096).unwrap_err(),
        "context_budget_too_small"
    );
}

#[test]
fn state_refuses_duplicate_work_and_unsupported_completed_claims() {
    let mut research = state();
    research.plan.push(research.plan[0].clone());
    assert!(research.validate().is_err());
    research.plan.pop();
    research.plan[0].status = enrichedlab_agent_sandbox::broker::project::StepStatus::Completed;
    assert!(research.validate().is_err());
    research.workspace.image_digest = "latest".into();
    assert!(research.validate().is_err());
}

#[tokio::test]
#[ignore = "uses isolated PostgreSQL in qualify_remote_broker.sh projects"]
async fn project_authority_idempotency_and_restart_state_use_restricted_database_roles() {
    let dsn = fs::read_to_string(
        env::var_os("BROKER_QUALIFICATION_MIGRATION_DSN_FILE").expect("private test DSN required"),
    )
    .expect("private test DSN unreadable");
    let config: Config = dsn.parse().unwrap_or_else(|_| panic!("invalid test DSN"));
    assert_eq!(config.get_dbname(), Some("broker_qual"));
    assert_eq!(config.get_user(), Some("postgres"));
    assert!(matches!(config.get_hosts(),[Host::Tcp(h)] if h=="127.0.0.1"));
    let (owner, connection) = config.connect(NoTls).await.expect("isolated database");
    let connection = tokio::spawn(connection);
    owner.batch_execute("INSERT INTO enrichedlab_agent.broker_principal_invitations(tenant_id,email,group_key,role) VALUES ('research-site','collaborator@example.invalid','pilot-lab','researcher'),('research-site','outsider@example.invalid','pilot-lab','researcher'),('research-site','admin@example.invalid','pilot-lab','researcher');").await.unwrap();
    let api_dsn = dsn.replace("user=postgres", "user=broker_api_qual");
    let api = Store::connect(&api_dsn, "research-site".into(), 4).unwrap();
    let identity = |subject: &str, email: &str| VerifiedIdentity {
        issuer: "https://identity.example.invalid".into(),
        class: PrincipalClass::Person,
        subject: subject.into(),
        email: email.into(),
        display_name: None,
    };
    let a = api
        .resolve_principal(&identity("owner", "researcher@example.invalid"))
        .await
        .unwrap();
    let b = api
        .resolve_principal(&identity("collaborator", "collaborator@example.invalid"))
        .await
        .unwrap();
    let outsider = api
        .resolve_principal(&identity("outsider", "outsider@example.invalid"))
        .await
        .unwrap();
    let admin = api
        .resolve_principal(&identity("administrator", "admin@example.invalid"))
        .await
        .unwrap();
    let project = Uuid::new_v4();
    owner.batch_execute("INSERT INTO enrichedlab_agent.broker_research_groups(tenant_id,group_key,name,max_active_agents,max_active_agents_per_principal,max_queued_tasks,retention_days) VALUES('research-site','other-lab','Other lab',3,2,20,30)").await.unwrap();
    owner.execute("DELETE FROM enrichedlab_agent.broker_group_memberships WHERE tenant_id='research-site' AND principal_id=$1",&[&outsider]).await.unwrap();
    owner.execute("INSERT INTO enrichedlab_agent.broker_group_memberships(tenant_id,group_key,principal_id,role) VALUES('research-site','other-lab',$1,'researcher')",&[&outsider]).await.unwrap();
    let shared = Uuid::new_v4();
    api.create_project(
        a,
        &CreateProject {
            project_id: shared,
            group: "pilot-lab".into(),
            title: "Shared lab work".into(),
            visibility: Visibility::Lab,
        },
    )
    .await
    .unwrap();
    assert!(api.get_project(b, shared).await.is_ok());
    assert!(api.get_project(outsider, shared).await.is_err());
    assert!(
        api.checkpoint_project(
            b,
            shared,
            &CheckpointRequest {
                expected_revision: 0,
                request_id: Uuid::new_v4(),
                state: state()
            }
        )
        .await
        .is_err()
    );
    api.create_project(
        a,
        &CreateProject {
            project_id: project,
            group: "pilot-lab".into(),
            title: "Public reference research".into(),
            visibility: Visibility::Private,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        api.get_project(b, project).await.unwrap_err().code,
        "project_not_found"
    );
    use enrichedlab_agent_sandbox::broker::project::ProjectGrant;
    api.grant_project(
        a,
        project,
        &ProjectGrant {
            principal_id: b,
            capabilities: vec![Capability::Read, Capability::Contribute, Capability::Steer],
        },
    )
    .await
    .unwrap();
    api.get_project(b, project).await.unwrap();
    assert!(
        api.grant_project(
            b,
            project,
            &ProjectGrant {
                principal_id: outsider,
                capabilities: vec![Capability::Read]
            }
        )
        .await
        .is_err()
    );
    let request = SubmitTaskRequest {
        group: "pilot-lab".into(),
        profile: "general-research".into(),
        objective: "Inspect retained public input through deterministic tools.".into(),
        project_id: Some(project),
        action_key: Some("inspect-input".into()),
        parent_task_id: None,
    };
    let task = api
        .submit_project_task(a, &request, "action-request-a", &"a".repeat(64))
        .await
        .unwrap();
    assert_eq!(
        task,
        api.submit_project_task(b, &request, "retry-request-b", &"a".repeat(64))
            .await
            .unwrap()
    );
    assert!(
        api.submit_project_task(b, &request, "changed-request-b", &"b".repeat(64))
            .await
            .is_err()
    );
    assert!(api.get_task(b, task).await.is_ok());
    assert!(api.list_events(b, task, 0, 100).await.is_ok());
    assert!(api.list_artifacts(b, task).await.is_ok());
    assert!(
        api.scientific_history_page(b, task, None, 100)
            .await
            .is_ok()
    );
    assert!(
        api.scientific_history_page(outsider, task, None, 100)
            .await
            .is_err()
    );
    let artifact = Uuid::new_v4();
    owner.execute("INSERT INTO enrichedlab_agent.broker_artifacts(tenant_id,artifact_id,task_id,role,label,relative_path,media_type,size_bytes,sha256,visibility) VALUES('research-site',$1,$2,'result','Public fixture','fixture.txt','text/plain',4,$3,'researcher')",&[&artifact,&task,&"b".repeat(64)]).await.unwrap();
    assert!(api.get_artifact(b, task, artifact).await.is_ok());
    assert!(api.get_artifact(outsider, task, artifact).await.is_err());
    assert_eq!(
        api.get_task(outsider, task).await.unwrap_err().code,
        "task_not_found"
    );
    assert!(api.cancel_task(b, task).await.is_err());
    let checkpoint = CheckpointRequest {
        expected_revision: 0,
        request_id: Uuid::new_v4(),
        state: state(),
    };
    assert_eq!(
        api.checkpoint_project(a, project, &checkpoint)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        api.checkpoint_project(a, project, &checkpoint)
            .await
            .unwrap(),
        1
    );
    let mut competing = checkpoint.clone();
    competing.expected_revision = 1;
    competing.request_id = Uuid::new_v4();
    let mut other = competing.clone();
    other.request_id = Uuid::new_v4();
    let (left, right) = tokio::join!(
        api.checkpoint_project(a, project, &competing),
        api.checkpoint_project(b, project, &other)
    );
    assert_ne!(left.is_ok(), right.is_ok());
    assert_eq!(
        left.err().or_else(|| right.err()).unwrap().code,
        "checkpoint_conflict"
    );
    let restarted = Store::connect(&api_dsn, "research-site".into(), 2).unwrap();
    assert_eq!(
        restarted.get_project(b, project).await.unwrap()["checkpoint"]["revision"],
        2
    );
    assert_eq!(
        restarted
            .project_history(b, project, 0, 10)
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        task,
        restarted
            .submit_project_task(b, &request, "after-restart", &"a".repeat(64))
            .await
            .unwrap()
    );
    // Explicit oversight enrollment does not itself open a project.
    owner.execute("INSERT INTO enrichedlab_agent.broker_project_administrators VALUES('research-site',$1)",&[&admin]).await.unwrap();
    assert!(api.get_project(admin, project).await.is_err());
    api.inspect_project(
        admin,
        project,
        "Verify the retained analysis and attribution.",
    )
    .await
    .unwrap();
    assert!(api.get_project(admin, project).await.is_ok());
    assert!(api.cancel_task(admin, task).await.is_err());
    let audit = api.project_audit(a, project, 100).await.unwrap();
    assert!(audit.iter().any(|v| v["kind"] == "admin_inspection"));
    let worker = Store::connect(
        &dsn.replace("user=postgres", "user=broker_worker_qual"),
        "research-site".into(),
        2,
    )
    .unwrap();
    let claimed = worker
        .claim_task("project-worker", 120)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, task);
    assert!(
        !worker
            .cancel_requested(task, "project-worker", claimed.lease_generation)
            .await
            .unwrap()
    );
    let mut queued_request = request.clone();
    queued_request.action_key = Some("collaborator-follow-up".into());
    api.submit_project_task(
        b,
        &queued_request,
        "collaborator-follow-up",
        &"c".repeat(64),
    )
    .await
    .unwrap();
    // Revoke the collaborator without changing history or the owner.
    api.grant_project(
        a,
        project,
        &ProjectGrant {
            principal_id: b,
            capabilities: vec![],
        },
    )
    .await
    .unwrap();
    assert!(api.get_project(b, project).await.is_err());
    assert!(api.get_task(b, task).await.is_err());
    assert!(api.list_artifacts(b, task).await.is_err());
    assert!(api.get_artifact(b, task, artifact).await.is_err());
    assert!(api.list_events(b, task, 0, 100).await.is_err());
    assert!(
        api.checkpoint_project(b, project, &competing)
            .await
            .is_err()
    );
    assert!(api.get_task(a, task).await.is_ok());
    assert!(
        worker
            .claim_task("project-worker", 120)
            .await
            .unwrap()
            .is_none()
    );
    owner.execute("DELETE FROM enrichedlab_agent.broker_group_memberships WHERE tenant_id='research-site' AND principal_id=$1",&[&a]).await.unwrap();
    assert!(api.get_project(a, project).await.is_err());
    assert!(api.get_task(a, task).await.is_err());
    assert!(
        worker
            .cancel_requested(task, "project-worker", claimed.lease_generation)
            .await
            .unwrap()
    );
    connection.abort();
}
