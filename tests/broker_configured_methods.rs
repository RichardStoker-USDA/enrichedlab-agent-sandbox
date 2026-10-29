use enrichedlab_agent_sandbox::broker::{
    codegen,
    configured::{Configuration, contract_configuration},
    coordinator::StartRun,
    model::{PrincipalClass, VerifiedIdentity},
    project::{CheckpointRequest, CreateProject, ResearchState, Visibility},
    store::{Store, StoreErrorKind},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, env, fs};
use tokio_postgres::{Config, NoTls, config::Host};
use uuid::Uuid;

const TEMPLATE: &str = "template://r-generated-analysis/v2";
const BLOCK: &str = include_str!("../profiles/configured/bootstrap-interval-v2/block.R");
const SETTINGS: &str =
    include_str!("../profiles/configured/bootstrap-interval-v2/configuration.json");
const CODE: &str = "analyze <- function(data) {\n  fit <- stats::lm(len ~ dose, data = data)\n  list(tables = list(coefficients = as.data.frame(coef(summary(fit)))), figures = list(), notes = character())\n}\n";

fn configuration() -> Value {
    contract_configuration(BLOCK, &serde_json::from_str(SETTINGS).unwrap())
}

fn research() -> ResearchState {
    serde_json::from_value(json!({"schema_version":1,"goal":"Mean tooth length per delivery method.",
        "constraints":["Compute every value from the staged input."],
        "plan":[
            {"action_key":"pooled-means","title":"Pooled means","status":"pending","task_id":null,"evidence_refs":[],"rationale":"Answer the question."},
            {"action_key":"fit-model","title":"Fit a model","status":"pending","task_id":null,"evidence_refs":[],"rationale":"A code template."}
        ],
        "lessons":[],"next_action":"Run the recipe.",
        "workspace":{"schema_version":1,"profile":"configured-r","image_digest":format!("sha256:{}","a".repeat(64)),
        "source_digest":format!("sha256:{}","b".repeat(64)),"input_refs":["input://public/toothgrowth"],
        "tool_refs":["tool://reviewed/template"],"preparation_receipt":null}})).unwrap()
}

fn settings(groups: &str) -> BTreeMap<String, String> {
    [
        ("groups", groups),
        ("statistic", "mean"),
        ("resamples", "10000"),
        ("level", "95"),
    ]
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .into()
}

/// What the coordinator submits for a configured action: the rendered block, its admission and
/// the configuration record.
fn configured(chosen: &BTreeMap<String, String>) -> Value {
    let c = Configuration::from_contract(&json!({"configuration": configuration()}))
        .unwrap()
        .unwrap();
    let block = c.render(chosen).unwrap();
    let report = codegen::admit(&block, TEMPLATE, &["len", "supp", "dose"].map(String::from));
    assert!(report.accepted(), "{:?}", report.reasons);
    json!({"template_ref":TEMPLATE,"path":"generated/block.R","content":block,
        "sha256":codegen::digest(&block),"admission":report,"configuration":c.record(chosen)})
}

fn code(content: &str) -> Value {
    let report = codegen::admit(
        content,
        TEMPLATE,
        &["len", "supp", "dose"].map(String::from),
    );
    json!({"template_ref":TEMPLATE,"path":"generated/block.R","content":content,
        "sha256":codegen::digest(content),"admission":report})
}

fn action(profile: &str, key: &str, generated: Value) -> Value {
    json!([{"action_key":key,"profile":profile,"objective":"Pool the doses.","request_sha256":"c".repeat(64),
        "generated_code":generated}])
}

#[tokio::test]
#[ignore = "isolated PostgreSQL through qualify_remote_broker.sh configured-methods"]
async fn a_configured_task_holds_only_its_rendered_recipe() {
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
    // Readiness: both services need migration 022.
    api.ready_for_review_api().await.unwrap();
    coordinator.ready_for_research_results().await.unwrap();
    let a = api
        .resolve_principal(&VerifiedIdentity {
            issuer: "https://identity.example.invalid".into(),
            class: PrincipalClass::Person,
            subject: "owner".into(),
            email: "researcher@example.invalid".into(),
            display_name: None,
        })
        .await
        .unwrap();

    owner.batch_execute("
        INSERT INTO enrichedlab_agent.broker_agent_profiles(tenant_id,profile_key,name,description,harness,thinking,route,
          controller_profile,gateway_profile,image_ref,cpu_limit,memory_limit,state_profile,role,tool_bundle,max_run_seconds,
          max_task_attempts,max_provider_attempts)
        SELECT tenant_id,k,name,description,harness,thinking,route,controller_profile,gateway_profile,image_ref,cpu_limit,
          memory_limit,state_profile,role,tool_bundle,max_run_seconds,max_task_attempts,max_provider_attempts
        FROM enrichedlab_agent.broker_agent_profiles, unnest(ARRAY['configured-r','generated-r']) k
        WHERE tenant_id='research-site' AND profile_key='general-research';
        INSERT INTO enrichedlab_agent.broker_group_profile_grants(tenant_id,group_key,profile_key)
          VALUES('research-site','pilot-lab','configured-r'),('research-site','pilot-lab','generated-r');").await.unwrap();
    let columns = json!({"len":"numeric","supp":"factor","dose":"numeric"});
    for (profile, contract) in [
        (
            "configured-r",
            json!({"dataset":{"columns":columns},"configuration":configuration()}),
        ),
        ("generated-r", json!({"dataset":{"columns":columns}})),
    ] {
        owner.execute("INSERT INTO enrichedlab_agent.broker_profile_templates(tenant_id,profile_key,template_ref,template_sha256,contract)
            VALUES('research-site',$1,$2,$3,$4)", &[&profile, &TEMPLATE, &format!("sha256:{}", "e".repeat(64)), &contract])
            .await.unwrap();
    }

    let project = Uuid::new_v4();
    api.create_project(
        a,
        &CreateProject {
            project_id: project,
            group: "pilot-lab".into(),
            title: "Configured method".into(),
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
                allowed_profiles: vec!["configured-r".into(), "generated-r".into()],
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
    let listed = claim["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["key"] == "configured-r")
        .unwrap()
        .clone();
    assert_eq!(
        listed["template"]["contract"]["configuration"]["block_sha256"],
        codegen::digest(BLOCK)
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
    let good = settings("supplement");
    let refused: Vec<(Value, &str)> = vec![
        // Model code under a configured method.
        (
            action("configured-r", "pooled-means", code(CODE)),
            "code without configuration",
        ),
        // The block rendered for other settings than the record names.
        (
            action("configured-r", "pooled-means", {
                let mut c = configured(&settings("dose"));
                c["configuration"] = configured(&good)["configuration"].clone();
                c
            }),
            "content from other settings",
        ),
        (
            action("configured-r", "pooled-means", {
                let mut c = configured(&good);
                c["configuration"]["settings"]["groups"] = json!("data$len");
                c
            }),
            "unknown choice",
        ),
        (
            action("configured-r", "pooled-means", {
                let mut c = configured(&good);
                c["configuration"]["settings"]
                    .as_object_mut()
                    .unwrap()
                    .remove("level");
                c
            }),
            "missing setting",
        ),
        (
            action("configured-r", "pooled-means", {
                let mut c = configured(&good);
                c["configuration"]["block_sha256"] = json!(codegen::digest("another recipe"));
                c
            }),
            "other template",
        ),
        (
            action("configured-r", "pooled-means", {
                let mut c = configured(&good);
                c["configuration"]["note"] = json!("extra");
                c
            }),
            "extra field",
        ),
        // A code template takes no configuration.
        (
            action("generated-r", "fit-model", {
                let mut c = code(CODE);
                c["configuration"] = configured(&good)["configuration"].clone();
                c
            }),
            "configuration on code",
        ),
    ];
    for (actions, label) in refused {
        let error = commit(actions).await.unwrap_err();
        assert_eq!(
            (error.kind, error.code),
            (StoreErrorKind::Invalid, "generated_code_invalid"),
            "{label}"
        );
    }
    let committed = commit(action("configured-r", "pooled-means", configured(&good)))
        .await
        .unwrap();
    let task: Uuid = serde_json::from_value(committed["tasks"][0]["task_id"].clone()).unwrap();
    let row = owner
        .query_one(
            "SELECT content,configuration FROM enrichedlab_agent.broker_task_generated_code WHERE task_id=$1",
            &[&task],
        )
        .await
        .unwrap();
    let content: String = row.get(0);
    assert!(content.contains("split(data$len, data$supp, drop = TRUE)"));
    let stored: Value = row.get(1);
    assert_eq!(stored["settings"]["groups"], "supplement");
    // The words come from the template row: each setting's label and the chosen choice's label.
    assert_eq!(
        stored["described"][0],
        json!({"setting":"groups","label":"Groups","choice":"supplement",
            "choice_label":"each delivery method (supplement), all doses together"})
    );
    assert_eq!(stored["described"].as_array().unwrap().len(), 4);

    // Readers and lineage name the configuration and class the task as configured.
    let view = api.task_generated_code(a, task).await.unwrap().unwrap();
    assert_eq!(view["configuration"], stored);
    let lineage = api.task_lineage(a, task).await.unwrap();
    assert_eq!(lineage["method"]["tool_class"], "configured");
    assert_eq!(lineage["code"]["configuration"], stored);
    assert_eq!(lineage["review"]["state"], "unvalidated");

    drop(owner);
    connection.abort();
}
