//! Per-attempt lineage (migration 019) in isolated PostgreSQL: each attempt keeps the
//! digests it registered, the execution its worker bound and the inputs its custodian sealed; a
//! retry that registers the same path does not rewrite the first attempt's record; every part not
//! recorded is named on that attempt; the records are immutable and each writer is fenced by its lease.
use enrichedlab_agent_sandbox::broker::{
    model::{PrincipalClass, SubmitTaskRequest, VerifiedIdentity},
    project::{CheckpointRequest, CreateProject, ResearchState, Visibility},
    store::{AttemptInput, Store, StoreErrorKind},
};
use serde_json::{Value, json};
use std::{env, fs};
use tokio_postgres::{Config, NoTls, config::Host};
use uuid::Uuid;

const INPUT: &str = "input://public/toothgrowth";

fn research() -> ResearchState {
    serde_json::from_value(json!({"schema_version":1,"goal":"Model tooth length by dose.",
        "constraints":["Compute every value from the staged input."],
        "plan":[{"action_key":"fit-model","title":"Fit a linear model","status":"pending","task_id":null,
            "evidence_refs":[],"rationale":"Answer the question."}],
        "lessons":[],"next_action":"Run the analysis.",
        "workspace":{"schema_version":1,"profile":"attempt-r","image_digest":format!("sha256:{}","a".repeat(64)),
        "source_digest":format!("sha256:{}","b".repeat(64)),"input_refs":[INPUT],
        "tool_refs":[],"preparation_receipt":null}})).unwrap()
}

fn digest(c: char) -> String {
    format!("sha256:{}", c.to_string().repeat(64))
}

fn gaps(value: &Value) -> Vec<&str> {
    value["gaps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|gap| gap.as_str().unwrap())
        .collect()
}

fn outputs(attempt: &Value) -> Vec<(String, bool)> {
    attempt["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| {
            (
                o["sha256"].as_str().unwrap().to_owned(),
                o["current"].as_bool().unwrap(),
            )
        })
        .collect()
}

#[tokio::test]
#[ignore = "isolated PostgreSQL through qualify_remote_broker.sh attempt-lineage"]
async fn each_attempt_keeps_what_it_sealed_ran_and_registered() {
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
    let api = store("broker_api_qual", 2);
    let worker = store("broker_worker_qual", 2);
    let custodian = store("broker_custodian_qual", 1);
    // The API needs schema 19 and the version 2 read grant before it serves.
    api.ready_for_review_api().await.unwrap();

    owner.batch_execute("INSERT INTO enrichedlab_agent.broker_principal_invitations(tenant_id,email,group_key,role) VALUES
        ('research-site','outsider@example.invalid','pilot-lab','researcher');
        INSERT INTO enrichedlab_agent.broker_agent_profiles(tenant_id,profile_key,name,description,harness,thinking,route,
          controller_profile,gateway_profile,image_ref,cpu_limit,memory_limit,state_profile,role,tool_bundle,max_run_seconds,
          max_task_attempts,max_provider_attempts)
        SELECT tenant_id,'attempt-r',name,description,harness,thinking,route,controller_profile,gateway_profile,image_ref,cpu_limit,
          memory_limit,state_profile,role,tool_bundle,max_run_seconds,3,max_provider_attempts
        FROM enrichedlab_agent.broker_agent_profiles WHERE tenant_id='research-site' AND profile_key='general-research';
        INSERT INTO enrichedlab_agent.broker_group_profile_grants(tenant_id,group_key,profile_key)
          VALUES('research-site','pilot-lab','attempt-r');").await.unwrap();
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

    let project = Uuid::new_v4();
    api.create_project(
        a,
        &CreateProject {
            project_id: project,
            group: "pilot-lab".into(),
            title: "Attempts".into(),
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
    let request = SubmitTaskRequest {
        group: "pilot-lab".into(),
        profile: "attempt-r".into(),
        objective: "Fit the model on the staged input.".into(),
        project_id: Some(project),
        action_key: Some("fit-model".into()),
        parent_task_id: None,
    };
    let task = api
        .submit_project_task(a, &request, "attempts-1", &"d".repeat(64))
        .await
        .unwrap();

    // Before any attempt there is nothing to report per attempt.
    let queued = api.task_lineage_v2(a, task).await.unwrap();
    assert_eq!(queued["schema"], "enrichedlab.task-lineage.v2");
    assert_eq!(queued["attempts"], json!([]));

    // Attempt 1: the custodian seals the input, the worker records what it bound and registers a result.
    let first = worker.claim_task("worker-1", 300).await.unwrap().unwrap();
    assert_eq!((first.id, first.attempt), (task, 1));
    let g1 = first.lease_generation;
    let params = digest('c');
    worker
        .record_attempt_execution(task, "worker-1", g1, &digest('1'), None, None, &params)
        .await
        .unwrap();
    // The same values again are accepted; different values for the same lease are refused.
    worker
        .record_attempt_execution(task, "worker-1", g1, &digest('1'), None, None, &params)
        .await
        .unwrap();
    assert!(
        worker
            .record_attempt_execution(task, "worker-1", g1, &digest('9'), None, None, &params)
            .await
            .is_err()
    );
    let reference = json!({"source_version":"datasets-4.4.1"});
    let sealed = "e".repeat(64);
    let input = AttemptInput {
        input_ref: INPUT,
        class: "public_reference",
        sealed_sha256: &sealed,
        size_bytes: 1_200,
        custody_transaction: "custody-1",
        source: &reference,
    };
    custodian
        .record_attempt_input(task, "worker-1", 1, g1, &input)
        .await
        .unwrap();
    // A lab extract without the snapshot it read, or an unknown class, is refused.
    let empty = json!({});
    let unsnapshotted = AttemptInput {
        input_ref: "input://lab/example/plasma",
        class: "lab_database",
        source: &empty,
        ..input
    };
    assert!(
        custodian
            .record_attempt_input(task, "worker-1", 1, g1, &unsnapshotted)
            .await
            .is_err()
    );
    let unclassed = AttemptInput {
        input_ref: "input://other/thing",
        class: "dataset",
        ..input
    };
    assert!(
        custodian
            .record_attempt_input(task, "worker-1", 1, g1, &unclassed)
            .await
            .is_err()
    );
    // Version 1 names the class the latest attempt's custodian recorded, and the gap goes (migration 039).
    let classed = api.task_lineage(a, task).await.unwrap();
    assert_eq!(classed["inputs"]["classes"], json!(["public_reference"]));
    assert_eq!(classed["inputs"]["classes_recorded_by"], "input_custodian");
    assert!(
        !gaps(&classed).contains(&"input_class_not_recorded"),
        "{classed}"
    );
    let first_result = "1".repeat(64);
    worker
        .register_artifact(
            task,
            "worker-1",
            g1,
            "researcher",
            "result",
            "Result",
            "results/table.csv",
            "text/csv",
            10,
            &first_result,
        )
        .await
        .unwrap();

    // Attempt 1's lease expires and the task is claimed again.
    owner.execute("UPDATE enrichedlab_agent.broker_tasks SET lease_expires_at=clock_timestamp()-interval '1 second'
        WHERE tenant_id='research-site' AND task_id=$1", &[&task]).await.unwrap();
    let second = worker.claim_task("worker-2", 300).await.unwrap().unwrap();
    assert_eq!((second.id, second.attempt), (task, 2));
    let g2 = second.lease_generation;
    assert!(g2 > g1);
    // The first lease can no longer write anything for the task.
    assert!(
        worker
            .register_artifact(
                task,
                "worker-1",
                g1,
                "researcher",
                "result",
                "Result",
                "results/late.csv",
                "text/csv",
                1,
                &"3".repeat(64),
            )
            .await
            .is_err()
    );
    assert!(
        worker
            .record_attempt_execution(task, "worker-1", g1, &digest('1'), None, None, &params)
            .await
            .is_err()
    );
    assert!(
        custodian
            .record_attempt_input(task, "worker-1", 1, g1, &input)
            .await
            .is_err()
    );

    // Attempt 2 binds another image and registers the same path with a different digest; its
    // custodian records no inputs.
    worker
        .record_attempt_execution(task, "worker-2", g2, &digest('2'), None, None, &params)
        .await
        .unwrap();
    let second_result = "2".repeat(64);
    worker
        .register_artifact(
            task,
            "worker-2",
            g2,
            "researcher",
            "result",
            "Result",
            "results/table.csv",
            "text/csv",
            12,
            &second_result,
        )
        .await
        .unwrap();
    assert!(
        worker
            .finish_task(task, "worker-2", g2, true, "completed", "")
            .await
            .unwrap()
    );

    let lineage = api.task_lineage_v2(a, task).await.unwrap();
    let attempts = lineage["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 2);
    let (one, two) = (&attempts[0], &attempts[1]);
    assert_eq!(one["attempt"], 1);
    assert_eq!(one["inputs"][0]["class"], "public_reference");
    assert_eq!(one["inputs"][0]["sha256"], json!(sealed));
    assert_eq!(one["execution"]["image_digest"], json!(digest('1')));
    // The first attempt keeps its own digest, marked as no longer the task's current file.
    assert_eq!(outputs(one), [(first_result.clone(), false)]);
    assert!(gaps(one).is_empty(), "{one}");
    assert_eq!(two["execution"]["image_digest"], json!(digest('2')));
    assert_eq!(outputs(two), [(second_result.clone(), true)]);
    assert_eq!(gaps(two), ["attempt_inputs_not_recorded"]);
    // The latest attempt recorded no input classes, so the task still says so.
    assert!(gaps(&lineage).contains(&"input_class_not_recorded"));
    assert_eq!(two["validation"]["state"], "unvalidated");

    // Version 1 is unchanged: one output, the current digest.
    let v1 = api.task_lineage(a, task).await.unwrap();
    assert_eq!(v1["schema"], "enrichedlab.task-lineage.v1");
    assert!(v1.get("attempts").is_none());
    // The latest attempt recorded no class, so version 1 names none and keeps the gap.
    assert_eq!(v1["inputs"]["classes"], json!([]));
    assert!(v1["inputs"]["classes_recorded_by"].is_null());
    assert!(gaps(&v1).contains(&"input_class_not_recorded"));
    assert_eq!(v1["outputs"].as_array().unwrap().len(), 1);
    assert_eq!(v1["outputs"][0]["sha256"], json!(second_result));

    // A reader without access to the task gets not found, as for version 1.
    let err = api.task_lineage_v2(outsider, task).await.unwrap_err();
    assert_eq!(err.kind, StoreErrorKind::NotFound);

    // The records are immutable, even for the database owner through the table.
    for statement in [
        "UPDATE enrichedlab_agent.broker_attempt_outputs SET size_bytes=0 WHERE task_id=$1",
        "DELETE FROM enrichedlab_agent.broker_attempt_outputs WHERE task_id=$1",
        "UPDATE enrichedlab_agent.broker_attempt_inputs SET size_bytes=0 WHERE task_id=$1",
        "DELETE FROM enrichedlab_agent.broker_attempt_executions WHERE task_id=$1",
    ] {
        assert!(
            owner.execute(statement, &[&task]).await.is_err(),
            "{statement}"
        );
    }

    // Each writer is granted only to its role, and no runtime role reads the tables directly.
    for (role, function) in [
        (
            "broker_api_qual",
            "broker_record_attempt_execution(text,uuid,text,bigint,text,text,text,text,text)",
        ),
        (
            "broker_api_qual",
            "broker_record_attempt_input(text,uuid,text,integer,bigint,text,text,text,bigint,text,jsonb)",
        ),
        (
            "broker_custodian_qual",
            "broker_record_attempt_execution(text,uuid,text,bigint,text,text,text,text,text)",
        ),
        (
            "broker_worker_qual",
            "broker_record_attempt_input(text,uuid,text,integer,bigint,text,text,text,bigint,text,jsonb)",
        ),
        (
            "broker_worker_qual",
            "broker_task_lineage_v2(text,uuid,uuid)",
        ),
        ("public", "broker_task_lineage_v2(text,uuid,uuid)"),
    ] {
        let granted: bool = owner
            .query_one(
                &format!(
                    "SELECT has_function_privilege($1,'enrichedlab_agent.{function}','EXECUTE')"
                ),
                &[&role],
            )
            .await
            .unwrap()
            .get(0);
        assert!(!granted, "{role} {function}");
    }
    for (role, table) in [
        ("broker_api_qual", "broker_attempt_outputs"),
        ("broker_worker_qual", "broker_attempt_inputs"),
        ("broker_custodian_qual", "broker_attempt_executions"),
    ] {
        let readable: bool = owner
            .query_one(
                &format!("SELECT has_table_privilege($1,'enrichedlab_agent.{table}','SELECT')"),
                &[&role],
            )
            .await
            .unwrap()
            .get(0);
        assert!(!readable, "{role} {table}");
    }

    drop(owner);
    connection.await.unwrap().unwrap();
}
