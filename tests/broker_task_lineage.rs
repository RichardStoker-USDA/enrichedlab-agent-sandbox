use enrichedlab_agent_sandbox::broker::{
    codegen,
    coordinator::StartRun,
    model::{PrincipalClass, SubmitTaskRequest, VerifiedIdentity},
    project::{CheckpointRequest, CreateProject, ResearchState, Visibility},
    store::{Store, StoreErrorKind},
};
use serde_json::{Value, json};
use std::{env, fs};
use tokio_postgres::{Config, NoTls, config::Host, error::SqlState};
use uuid::Uuid;

const TEMPLATE: &str = "template://r-generated-analysis/v1";
const BLOCK: &str = "analyze <- function(data) {\n  fit <- stats::lm(len ~ dose, data = data)\n  list(tables = list(coefficients = as.data.frame(coef(summary(fit)))), figures = list(), notes = character())\n}\n";
const MIGRATION_016: &str = include_str!("../migrations/postgres/016_task_lineage.sql");
const MIGRATION_017: &str =
    include_str!("../migrations/postgres/017_task_lineage_template_class.sql");
const MIGRATION_018: &str = include_str!("../migrations/postgres/018_task_lineage_history.sql");

fn research() -> ResearchState {
    serde_json::from_value(json!({"schema_version":1,"goal":"Model tooth length by dose.",
        "constraints":["Compute every value from the staged input."],
        "plan":[
            {"action_key":"fit-model","title":"Fit a linear model","status":"pending","task_id":null,"evidence_refs":[],"rationale":"Answer the question."},
            {"action_key":"later-step","title":"A later step","status":"pending","task_id":null,"evidence_refs":[],"rationale":"Kept for later."}
        ],
        "lessons":[],"next_action":"Run the template.",
        "workspace":{"schema_version":1,"profile":"generated-r","image_digest":format!("sha256:{}","a".repeat(64)),
        "source_digest":format!("sha256:{}","b".repeat(64)),"input_refs":["input://public/toothgrowth"],
        "tool_refs":["tool://reviewed/template"],"preparation_receipt":null}})).unwrap()
}

fn gaps(lineage: &Value) -> Vec<&str> {
    lineage["gaps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|gap| gap.as_str().unwrap())
        .collect()
}

fn origin(lineage: &Value) -> (String, String) {
    (
        lineage["origin"]["origin"].as_str().unwrap().to_owned(),
        lineage["origin"]["basis"].as_str().unwrap_or("").to_owned(),
    )
}

#[tokio::test]
#[ignore = "isolated PostgreSQL through qualify_remote_broker.sh lineage"]
async fn lineage_records_who_asked_and_names_every_gap() {
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
    api.ready_for_review_api().await.unwrap();
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
    owner.batch_execute("
        INSERT INTO enrichedlab_agent.broker_agent_profiles(tenant_id,profile_key,name,description,harness,thinking,route,
          controller_profile,gateway_profile,image_ref,cpu_limit,memory_limit,state_profile,role,tool_bundle,max_run_seconds,
          max_task_attempts,max_provider_attempts)
        SELECT tenant_id,k,name,description,harness,thinking,route,controller_profile,gateway_profile,image_ref,cpu_limit,
          memory_limit,state_profile,role,tool_bundle,max_run_seconds,max_task_attempts,max_provider_attempts
        FROM enrichedlab_agent.broker_agent_profiles, unnest(ARRAY['generated-r','fixed-r','late-r']) k
        WHERE tenant_id='research-site' AND profile_key='general-research';
        INSERT INTO enrichedlab_agent.broker_group_profile_grants(tenant_id,group_key,profile_key)
          VALUES('research-site','pilot-lab','generated-r'),('research-site','pilot-lab','fixed-r'),
            ('research-site','pilot-lab','late-r');").await.unwrap();
    owner.execute("INSERT INTO enrichedlab_agent.broker_profile_templates(tenant_id,profile_key,template_ref,template_sha256,contract)
        VALUES('research-site','generated-r',$1,$2,$3)",
        &[&TEMPLATE, &format!("sha256:{}", "e".repeat(64)),
          &json!({"dataset":{"name":"toothgrowth","columns":{"len":"numeric","supp":"factor","dose":"numeric"}}})])
        .await.unwrap();

    let project = Uuid::new_v4();
    api.create_project(
        a,
        &CreateProject {
            project_id: project,
            group: "pilot-lab".into(),
            title: "Lineage".into(),
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

    // A person's project task: the origin is written with the task. The project's input
    // selection is recorded; its data class is not, and the lineage says so.
    let request = SubmitTaskRequest {
        group: "pilot-lab".into(),
        profile: "fixed-r".into(),
        objective: "Inspect the staged input.".into(),
        project_id: Some(project),
        action_key: Some("inspect-input".into()),
        parent_task_id: None,
    };
    let asked = api
        .submit_project_task(a, &request, "person-inspect", &"d".repeat(64))
        .await
        .unwrap();
    let lineage = api.task_lineage(a, asked).await.unwrap();
    assert_eq!(lineage["schema"], "enrichedlab.task-lineage.v1");
    assert_eq!(origin(&lineage), ("person".into(), "submission".into()));
    assert_eq!(lineage["origin"]["principal_id"], json!(a));
    assert_eq!(lineage["origin"]["principal_kind"], "person");
    assert!(lineage["origin"]["principal_name"].is_null());
    assert!(lineage["plan_step"].is_null());
    assert_eq!(
        lineage["inputs"]["workspace"]["input_refs"],
        json!(["input://public/toothgrowth"])
    );
    assert_eq!(lineage["inputs"]["checkpoint_revision"], 1);
    assert_eq!(lineage["method"]["tool_class"], "agent_session");
    assert_eq!(lineage["review"]["state"], "unvalidated");
    assert_eq!(
        gaps(&lineage),
        ["plan_step_not_recorded", "input_class_not_recorded"]
    );
    // A replay returns the same task and keeps its first origin.
    assert_eq!(
        api.submit_project_task(a, &request, "person-inspect", &"d".repeat(64))
            .await
            .unwrap(),
        asked
    );

    // A task outside any project names that gap instead of guessing inputs.
    let solo = api
        .submit_task(
            a,
            "pilot-lab",
            "fixed-r",
            "A standalone question.",
            "solo-1",
            &"f".repeat(64),
        )
        .await
        .unwrap();
    let lineage = api.task_lineage(a, solo).await.unwrap();
    assert_eq!(origin(&lineage), ("person".into(), "submission".into()));
    assert!(lineage["inputs"]["workspace"].is_null());
    assert_eq!(gaps(&lineage), ["not_in_project"]);

    // A task on a reviewed-template profile with no admitted block ran no model-written code:
    // its method is still the template's, and the missing block is a gap, never a configured tool.
    let without_block = api
        .submit_task(
            a,
            "pilot-lab",
            "generated-r",
            "A template task without a block.",
            "solo-template-1",
            &"9".repeat(64),
        )
        .await
        .unwrap();
    let lineage = api.task_lineage(a, without_block).await.unwrap();
    assert_eq!(lineage["method"]["tool_class"], "agent_built");
    assert!(lineage["code"].is_null());
    assert_eq!(gaps(&lineage), ["not_in_project", "code_not_recorded"]);

    // A recorded platform tool job outranks the profile: the reviewed tool is what ran.
    let with_tool = api
        .submit_task(
            a,
            "pilot-lab",
            "generated-r",
            "A template task that ran a reviewed tool.",
            "solo-template-2",
            &"8".repeat(64),
        )
        .await
        .unwrap();
    owner.execute("INSERT INTO enrichedlab_agent.broker_task_platform_tool_jobs(tenant_id,task_id,attempt,lease_generation,
          principal_id,worker_id,request_id,conversation_id,platform_job_id,tool_id,catalog_digest,input_sha256,accepted_at)
        VALUES('research-site',$1,1,1,$2,'worker-1',$3,$4,$5,'toothgrowth_anova',$6,$7,clock_timestamp())",
        &[&with_tool, &a, &Uuid::new_v4(), &Uuid::new_v4(), &format!("job_{}", "a".repeat(26)),
          &format!("sha256:{}", "d".repeat(64)), &"c".repeat(64)])
        .await.unwrap();
    let lineage = api.task_lineage(a, with_tool).await.unwrap();
    assert_eq!(lineage["method"]["tool_class"], "vetted_service");
    assert_eq!(
        lineage["inputs"]["platform_tool"]["tool_id"],
        "toothgrowth_anova"
    );
    assert_eq!(
        gaps(&lineage),
        ["not_in_project", "input_class_not_recorded"]
    );

    // A later catalog change does not relabel an earlier analysis: the method is the one the task
    // was submitted under.
    let before = api.task_lineage(a, solo).await.unwrap();
    assert_eq!(before["method"]["recorded"], "submission");
    let name = before["method"]["profile_name"].clone();
    assert!(name.is_string());
    owner.execute("INSERT INTO enrichedlab_agent.broker_profile_templates(tenant_id,profile_key,template_ref,template_sha256,contract)
        VALUES('research-site','fixed-r',$1,$2,$3)",
        &[&TEMPLATE, &format!("sha256:{}", "e".repeat(64)), &json!({"dataset":{"name":"toothgrowth","columns":{"len":"numeric"}}})])
        .await.unwrap();
    owner.batch_execute("UPDATE enrichedlab_agent.broker_agent_profiles SET name='Renamed later' WHERE tenant_id='research-site' AND profile_key='fixed-r'")
        .await.unwrap();
    let after = api.task_lineage(a, solo).await.unwrap();
    assert_eq!(after["method"]["profile_name"], name);
    assert_eq!(after["method"]["tool_class"], "agent_session");
    assert!(after["method"]["template_ref"].is_null());
    assert_eq!(gaps(&after), ["not_in_project"]);

    // A research run's task: the committed turn writes the origin, with the run and turn.
    let run = api
        .start_research_run(
            a,
            project,
            &StartRun {
                run_id: Uuid::new_v4(),
                expected_revision: 1,
                allowed_profiles: vec!["generated-r".into()],
                max_turns: 4,
                max_parallel: 3,
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
    let report = codegen::admit(BLOCK, TEMPLATE, &["len", "supp", "dose"].map(String::from));
    let actions = json!([{"action_key":"fit-model","profile":"generated-r","objective":"Fit the declared model.",
        "request_sha256":"c".repeat(64),"generated_code":{"template_ref":TEMPLATE,"path":"generated/block.R",
        "content":BLOCK,"sha256":codegen::digest(BLOCK),"admission":report}}]);
    let committed = coordinator
        .commit_research_turn(
            run,
            "lead-a",
            generation,
            1,
            &serde_json::to_value(research()).unwrap(),
            &actions,
            &json!({"schema_version":1}),
            "delegate",
        )
        .await
        .unwrap();
    let delegated: Uuid = serde_json::from_value(committed["tasks"][0]["task_id"].clone()).unwrap();
    let lineage = api.task_lineage(a, delegated).await.unwrap();
    assert_eq!(
        origin(&lineage),
        ("research_run".into(), "submission".into())
    );
    assert_eq!(lineage["origin"]["run_id"], json!(run));
    assert_eq!(lineage["origin"]["turn"], 1);
    assert_eq!(lineage["origin"]["principal_id"], json!(a));
    assert_eq!(lineage["plan_step"]["title"], "Fit a linear model");
    assert_eq!(lineage["inputs"]["checkpoint_revision"], 2);
    assert_eq!(lineage["method"]["tool_class"], "agent_built");
    assert_eq!(lineage["method"]["template_ref"], TEMPLATE);
    assert_eq!(lineage["code"]["sha256"], codegen::digest(BLOCK));
    assert_eq!(lineage["outputs"], json!([]));
    assert_eq!(gaps(&lineage), ["input_class_not_recorded"]);
    assert_eq!(lineage["plan_step"]["revision"], 2);
    assert_eq!(lineage["plan_step"]["added_after_submission"], false);
    assert!(lineage["plan_step"]["current"].is_null());

    // A later plan revision is shown beside the step first recorded, not in its place.
    let status_then = lineage["plan_step"]["status"].clone();
    let mut revised = serde_json::to_value(research()).unwrap();
    revised["plan"][0]["title"] = json!("Refit with an interaction term");
    revised["plan"][0]["rationale"] = json!("A later reading.");
    revised["plan"][0]["task_id"] = json!(delegated);
    revised["plan"][0]["status"] = status_then.clone();
    api.checkpoint_project(
        a,
        project,
        &CheckpointRequest {
            expected_revision: 2,
            request_id: Uuid::new_v4(),
            state: serde_json::from_value(revised).unwrap(),
        },
    )
    .await
    .unwrap();
    let lineage = api.task_lineage(a, delegated).await.unwrap();
    assert_eq!(lineage["plan_step"]["title"], "Fit a linear model");
    assert_eq!(lineage["plan_step"]["revision"], 2);
    assert_eq!(lineage["plan_step"]["status"], status_then);
    assert_eq!(
        lineage["plan_step"]["current"]["title"],
        "Refit with an interaction term"
    );
    assert_eq!(lineage["plan_step"]["current"]["revision"], 3);
    assert_eq!(lineage["inputs"]["checkpoint_revision"], 2);

    // Only readers of the task see its lineage.
    let refused = api.task_lineage(outsider, delegated).await.unwrap_err();
    assert_eq!(
        (refused.kind, refused.code),
        (StoreErrorKind::NotFound, "task_not_found")
    );

    // The API role cannot create a task without its origin, and origins cannot change.
    let (api_role, api_connection) = dsn
        .replace("user=postgres", "user=broker_api_qual")
        .parse::<Config>()
        .unwrap()
        .connect(NoTls)
        .await
        .unwrap();
    let api_connection = tokio::spawn(api_connection);
    for statement in [
        "SELECT enrichedlab_agent.broker_submit_task('research-site',$1,'pilot-lab','fixed-r','x','direct-1',repeat('a',64))",
        "SELECT enrichedlab_agent.broker_submit_project_task('research-site',$1,$2,'direct-2',NULL,'pilot-lab','fixed-r','x','direct-2',repeat('a',64))",
    ] {
        let parameters: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
            if statement.contains("$2") {
                vec![&a, &project]
            } else {
                vec![&a]
            };
        let error = api_role
            .query_one(statement, &parameters)
            .await
            .unwrap_err();
        assert_eq!(error.code(), Some(&SqlState::INSUFFICIENT_PRIVILEGE));
    }
    api_connection.abort();
    for statement in [
        "UPDATE enrichedlab_agent.broker_task_origins SET origin='person'",
        "DELETE FROM enrichedlab_agent.broker_task_origins",
    ] {
        assert!(owner.execute(statement, &[]).await.is_err());
    }

    // Earlier tasks, created before origins and methods were recorded: a turn record, a task
    // outside any project, a project task no turn lists, and a task whose profile gained a
    // template only after it was submitted. The backfills run once.
    owner
        .batch_execute("ALTER TABLE enrichedlab_agent.broker_tasks DISABLE TRIGGER task_methods")
        .await
        .unwrap();
    let legacy_late: Uuid = owner
        .query_one("SELECT enrichedlab_agent.broker_submit_task('research-site',$1,'pilot-lab','late-r','Earlier template question.','legacy-4',repeat('4',64))", &[&a])
        .await.unwrap().get(0);
    let legacy_project: Uuid = owner
        .query_one("SELECT enrichedlab_agent.broker_submit_project_task('research-site',$1,$2,'legacy-step',NULL,'pilot-lab','fixed-r','Earlier step.','legacy-1',repeat('1',64))", &[&a, &project])
        .await.unwrap().get(0);
    let legacy_solo: Uuid = owner
        .query_one("SELECT enrichedlab_agent.broker_submit_task('research-site',$1,'pilot-lab','fixed-r','Earlier question.','legacy-2',repeat('2',64))", &[&a])
        .await.unwrap().get(0);
    let legacy_turn: Uuid = owner
        .query_one("SELECT enrichedlab_agent.broker_submit_project_task('research-site',$1,$2,'legacy-run-step',NULL,'pilot-lab','fixed-r','Earlier run step.','legacy-3',repeat('3',64))", &[&a, &project])
        .await.unwrap().get(0);
    owner
        .batch_execute(
            "ALTER TABLE enrichedlab_agent.broker_research_turns DISABLE TRIGGER task_origins",
        )
        .await
        .unwrap();
    owner.execute("INSERT INTO enrichedlab_agent.broker_research_turns VALUES('research-site',$1,2,$2,2,2,$3,clock_timestamp())",
        &[&run, &generation, &json!({"submitted_tasks":[{"task_id":legacy_turn,"action_key":"legacy-run-step"}],"actor_id":a})])
        .await.unwrap();
    owner
        .batch_execute(
            "ALTER TABLE enrichedlab_agent.broker_research_turns ENABLE TRIGGER task_origins",
        )
        .await
        .unwrap();
    owner
        .batch_execute("ALTER TABLE enrichedlab_agent.broker_tasks ENABLE TRIGGER task_methods")
        .await
        .unwrap();
    owner.execute("INSERT INTO enrichedlab_agent.broker_profile_templates(tenant_id,profile_key,template_ref,template_sha256,contract)
        VALUES('research-site','late-r',$1,$2,$3)",
        &[&TEMPLATE, &format!("sha256:{}", "e".repeat(64)), &json!({"dataset":{"name":"toothgrowth","columns":{"len":"numeric"}}})])
        .await.unwrap();
    assert!(
        gaps(&api.task_lineage(a, legacy_late).await.unwrap()).contains(&"method_not_recorded")
    );
    for task in [legacy_project, legacy_solo, legacy_turn] {
        let lineage = api.task_lineage(a, task).await.unwrap();
        assert_eq!(origin(&lineage), ("unrecorded".into(), String::new()));
        assert_eq!(gaps(&lineage)[0], "origin_not_recorded");
    }
    let before: i64 = owner
        .query_one(
            "SELECT count(*) FROM enrichedlab_agent.broker_task_origins",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    owner
        .batch_execute("DELETE FROM enrichedlab_agent.schema_migrations WHERE version=16")
        .await
        .unwrap();
    owner.batch_execute(MIGRATION_016).await.unwrap();
    owner.batch_execute(MIGRATION_016).await.unwrap();
    owner.batch_execute(MIGRATION_017).await.unwrap();
    let methods_before: i64 = owner
        .query_one(
            "SELECT count(*) FROM enrichedlab_agent.broker_task_methods",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    owner
        .batch_execute("DELETE FROM enrichedlab_agent.schema_migrations WHERE version=18")
        .await
        .unwrap();
    owner.batch_execute(MIGRATION_018).await.unwrap();
    owner.batch_execute(MIGRATION_018).await.unwrap();
    let methods_after: i64 = owner
        .query_one(
            "SELECT count(*) FROM enrichedlab_agent.broker_task_methods",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(methods_after, methods_before + 4);
    // A template registered after the task was submitted is not bound to it.
    let late = api.task_lineage(a, legacy_late).await.unwrap();
    assert_eq!(late["method"]["recorded"], "recorded_later");
    assert!(late["method"]["template_ref"].is_null());
    assert_eq!(late["method"]["tool_class"], "agent_session");
    assert!(gaps(&late).contains(&"method_recorded_later"));
    assert!(!gaps(&late).contains(&"code_not_recorded"));
    // One registered before it is.
    let earlier = api.task_lineage(a, legacy_solo).await.unwrap();
    assert_eq!(earlier["method"]["recorded"], "recorded_later");
    assert_eq!(earlier["method"]["template_ref"], TEMPLATE);
    assert_eq!(earlier["method"]["tool_class"], "agent_built");
    assert!(gaps(&earlier).contains(&"code_not_recorded"));
    let after: i64 = owner
        .query_one(
            "SELECT count(*) FROM enrichedlab_agent.broker_task_origins",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(after, before + 4);
    let project_lineage = api.task_lineage(a, legacy_project).await.unwrap();
    assert_eq!(
        origin(&project_lineage),
        ("unrecorded".into(), "before_origin_records".into())
    );
    assert_eq!(project_lineage["origin"]["principal_id"], json!(a));
    assert!(gaps(&project_lineage).contains(&"origin_before_records"));
    assert_eq!(
        origin(&api.task_lineage(a, legacy_solo).await.unwrap()),
        ("person".into(), "standalone_task".into())
    );
    let turn_lineage = api.task_lineage(a, legacy_turn).await.unwrap();
    assert_eq!(
        origin(&turn_lineage),
        ("research_run".into(), "turn_record".into())
    );
    assert_eq!(turn_lineage["origin"]["turn"], 2);
    // The backfill leaves recorded origins as they were.
    assert_eq!(
        origin(&api.task_lineage(a, asked).await.unwrap()),
        ("person".into(), "submission".into())
    );
    assert_eq!(
        origin(&api.task_lineage(a, delegated).await.unwrap()),
        ("research_run".into(), "submission".into())
    );
    // The fill leaves methods recorded at submission as they were.
    let kept = api.task_lineage(a, asked).await.unwrap();
    assert_eq!(kept["method"]["recorded"], "submission");
    assert_eq!(kept["method"]["tool_class"], "agent_session");
    for statement in [
        "UPDATE enrichedlab_agent.broker_task_methods SET profile_name='x'",
        "DELETE FROM enrichedlab_agent.broker_task_methods",
    ] {
        assert!(owner.execute(statement, &[]).await.is_err());
    }
    connection.abort();
}
