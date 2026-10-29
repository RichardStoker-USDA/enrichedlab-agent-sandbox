use enrichedlab_agent_sandbox::broker::{
    codegen,
    coordinator::StartRun,
    model::{PrincipalClass, VerifiedIdentity},
    project::{CheckpointRequest, CreateProject, ResearchState, Visibility},
    store::{Store, StoreErrorKind},
};
use serde_json::{Value, json};
use std::{env, fs};
use tokio_postgres::{Config, NoTls, config::Host};
use uuid::Uuid;

const TEMPLATE: &str = "template://r-generated-analysis/v1";
const BLOCK: &str = "analyze <- function(data) {\n  fit <- stats::lm(len ~ dose, data = data)\n  list(tables = list(coefficients = as.data.frame(coef(summary(fit)))), figures = list(), notes = character())\n}\n";

fn research() -> ResearchState {
    serde_json::from_value(json!({"schema_version":1,"goal":"Model tooth length by dose.",
        "constraints":["Compute every value from the staged input."],
        "plan":[
            {"action_key":"fit-model","title":"Fit a linear model","status":"pending","task_id":null,"evidence_refs":[],"rationale":"Answer the question."},
            {"action_key":"fixed-method","title":"Fixed method","status":"pending","task_id":null,"evidence_refs":[],"rationale":"A fixed method."}
        ],
        "lessons":[],"next_action":"Run the template.",
        "workspace":{"schema_version":1,"profile":"generated-r","image_digest":format!("sha256:{}","a".repeat(64)),
        "source_digest":format!("sha256:{}","b".repeat(64)),"input_refs":["input://public/toothgrowth"],
        "tool_refs":["tool://reviewed/template"],"preparation_receipt":null}})).unwrap()
}

fn code_action(profile: &str, code: Option<Value>) -> Value {
    let mut action = json!({"action_key":if profile == "generated-r" {"fit-model"} else {"fixed-method"},
        "profile":profile,"objective":"Fit the declared model.","request_sha256":"c".repeat(64)});
    if let Some(code) = code {
        action["generated_code"] = code;
    }
    json!([action])
}

fn admitted(content: &str) -> Value {
    let report = codegen::admit(
        content,
        TEMPLATE,
        &["len", "supp", "dose"].map(String::from),
    );
    json!({"template_ref":TEMPLATE,"path":"generated/block.R","content":content,
        "sha256":codegen::digest(content),"admission":report})
}

#[tokio::test]
#[ignore = "isolated PostgreSQL through qualify_remote_broker.sh generated-code"]
async fn admitted_code_reaches_only_its_lease_and_readers() {
    let dsn = fs::read_to_string(env::var_os("BROKER_QUALIFICATION_MIGRATION_DSN_FILE").unwrap())
        .unwrap();
    let config: Config = dsn.parse().unwrap();
    assert_eq!(config.get_dbname(), Some("broker_qual"));
    assert!(matches!(config.get_hosts(),[Host::Tcp(h)] if h=="127.0.0.1"));
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
    let custodian = store("broker_custodian_qual", 2);
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

    // Two profiles for the group: one reviewed template, one fixed method.
    owner.batch_execute("
        INSERT INTO enrichedlab_agent.broker_agent_profiles(tenant_id,profile_key,name,description,harness,thinking,route,
          controller_profile,gateway_profile,image_ref,cpu_limit,memory_limit,state_profile,role,tool_bundle,max_run_seconds,
          max_task_attempts,max_provider_attempts)
        SELECT tenant_id,k,name,description,harness,thinking,route,controller_profile,gateway_profile,image_ref,cpu_limit,
          memory_limit,state_profile,role,tool_bundle,max_run_seconds,max_task_attempts,max_provider_attempts
        FROM enrichedlab_agent.broker_agent_profiles, unnest(ARRAY['generated-r','fixed-r']) k
        WHERE tenant_id='research-site' AND profile_key='general-research';
        INSERT INTO enrichedlab_agent.broker_group_profile_grants(tenant_id,group_key,profile_key)
          VALUES('research-site','pilot-lab','generated-r'),('research-site','pilot-lab','fixed-r');").await.unwrap();
    owner.execute("INSERT INTO enrichedlab_agent.broker_profile_templates(tenant_id,profile_key,template_ref,template_sha256,contract)
        VALUES('research-site','generated-r',$1,$2,$3)",
        &[&TEMPLATE, &format!("sha256:{}", "e".repeat(64)),
          &json!({"dataset":{"name":"toothgrowth","columns":{"len":"numeric","supp":"factor","dose":"numeric"}}})])
        .await.unwrap();
    // Operator configuration is immutable once written.
    assert!(owner.execute("UPDATE enrichedlab_agent.broker_profile_templates SET template_ref='template://other/v1'", &[]).await.is_err());

    let project = Uuid::new_v4();
    api.create_project(
        a,
        &CreateProject {
            project_id: project,
            group: "pilot-lab".into(),
            title: "Generated code".into(),
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
                allowed_profiles: vec!["generated-r".into(), "fixed-r".into()],
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
    let generation = claim["run"]["generation"].as_i64().unwrap();
    let profiles = claim["profiles"].as_array().unwrap();
    let template = profiles.iter().find(|p| p["key"] == "generated-r").unwrap();
    assert_eq!(template["template"]["template_ref"], TEMPLATE);
    assert_eq!(
        template["template"]["contract"]["dataset"]["name"],
        "toothgrowth"
    );
    assert!(
        profiles
            .iter()
            .find(|p| p["key"] == "fixed-r")
            .unwrap()
            .get("template")
            .is_none()
    );

    let state = serde_json::to_value(research()).unwrap();
    let record = json!({"schema_version":1,"test":"bounded metadata"});
    let commit = |actions: Value| {
        let coordinator = coordinator.clone();
        let state = state.clone();
        let record = record.clone();
        async move {
            coordinator
                .commit_research_turn(
                    run, "lead-a", generation, 1, &state, &actions, &record, "delegate",
                )
                .await
        }
    };
    // A template requires an admitted block; a fixed method refuses one; any mismatch refuses.
    for (actions, label) in [
        (code_action("generated-r", None), "missing"),
        (code_action("fixed-r", Some(admitted(BLOCK))), "unexpected"),
        (
            code_action(
                "generated-r",
                Some({
                    let mut c = admitted(BLOCK);
                    c["sha256"] = json!(format!("sha256:{}", "0".repeat(64)));
                    c
                }),
            ),
            "digest",
        ),
        (
            code_action(
                "generated-r",
                Some({
                    let mut c = admitted(BLOCK);
                    c["template_ref"] = json!("template://other/v1");
                    c
                }),
            ),
            "template",
        ),
        (
            code_action(
                "generated-r",
                Some({
                    let mut c = admitted(BLOCK);
                    c["admission"]["result"] = json!("rejected");
                    c
                }),
            ),
            "admission",
        ),
        (
            code_action(
                "generated-r",
                Some({
                    let mut c = admitted(BLOCK);
                    c["note"] = json!("extra");
                    c
                }),
            ),
            "field",
        ),
        (
            code_action("generated-r", Some(admitted("system('id')"))),
            "rejected block",
        ),
    ] {
        let error = commit(actions).await.unwrap_err();
        assert_eq!(
            (error.kind, error.code),
            (StoreErrorKind::Invalid, "generated_code_invalid"),
            "{label}"
        );
    }
    let committed = commit(code_action("generated-r", Some(admitted(BLOCK))))
        .await
        .unwrap();
    let task: Uuid = serde_json::from_value(committed["tasks"][0]["task_id"].clone()).unwrap();
    let stored: (String, i32, String) = {
        let row = owner.query_one("SELECT content,size_bytes,sha256 FROM enrichedlab_agent.broker_task_generated_code WHERE task_id=$1", &[&task]).await.unwrap();
        (row.get(0), row.get(1), row.get(2))
    };
    assert_eq!(
        stored,
        (BLOCK.to_owned(), BLOCK.len() as i32, codegen::digest(BLOCK))
    );
    assert!(
        owner
            .execute(
                "UPDATE enrichedlab_agent.broker_task_generated_code SET content='x'",
                &[]
            )
            .await
            .is_err()
    );
    assert!(
        owner
            .execute(
                "DELETE FROM enrichedlab_agent.broker_task_generated_code",
                &[]
            )
            .await
            .is_err()
    );

    // Only the current lease reads the reference or the bytes; the API role reads neither.
    let claimed = worker
        .claim_task("code-worker", 120)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, task);
    let reference = worker
        .task_admitted_code(
            task,
            "code-worker",
            claimed.attempt,
            claimed.lease_generation,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            reference.template_ref.as_str(),
            reference.path.as_str(),
            reference.size_bytes,
            reference.sha256.clone()
        ),
        (
            TEMPLATE,
            "generated/block.R",
            BLOCK.len() as u64,
            codegen::digest(BLOCK)
        )
    );
    assert!(
        worker
            .task_admitted_code(
                task,
                "code-worker",
                claimed.attempt,
                claimed.lease_generation + 1
            )
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        worker
            .task_admitted_code(
                task,
                "other-worker",
                claimed.attempt,
                claimed.lease_generation
            )
            .await
            .unwrap()
            .is_none()
    );
    let (bytes_reference, content) = custodian
        .custody_admitted_code(
            task,
            "code-worker",
            claimed.attempt,
            claimed.lease_generation,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!((bytes_reference, content.as_str()), (reference, BLOCK));
    assert!(
        custodian
            .custody_admitted_code(
                task,
                "code-worker",
                claimed.attempt + 1,
                claimed.lease_generation
            )
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        worker
            .custody_admitted_code(
                task,
                "code-worker",
                claimed.attempt,
                claimed.lease_generation
            )
            .await
            .is_err()
    );
    assert!(
        api.custody_admitted_code(
            task,
            "code-worker",
            claimed.attempt,
            claimed.lease_generation
        )
        .await
        .is_err()
    );
    assert!(custodian.task_generated_code(a, task).await.is_err());

    // A reader of the task sees the block and its admission; nobody else does.
    let view = api.task_generated_code(a, task).await.unwrap().unwrap();
    assert_eq!(
        (view["content"].as_str(), view["validation"].as_str()),
        (Some(BLOCK), Some("unvalidated"))
    );
    assert_eq!(view["admission"]["result"], "accepted");
    assert_eq!(
        api.task_generated_code(outsider, task)
            .await
            .unwrap_err()
            .code,
        "task_not_found"
    );
    let snapshot = api.project_package_snapshot(a, project).await.unwrap();
    assert_eq!(snapshot["generated_code"].as_array().unwrap().len(), 1);
    assert_eq!(
        snapshot["generated_code"][0]["sha256"],
        codegen::digest(BLOCK)
    );
    api.ready_for_review_api().await.unwrap();

    drop(owner);
    connection.abort();
}
