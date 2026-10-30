//! Lab objects (migration 083) against an isolated PostgreSQL: the coordinator is ready only with the migration and
//! reads the run's objects methods under its lease, the turn commit records an objects step and refuses every other
//! shape, the route is request-only, the worker reads the request of its own lease only and records one receipt per
//! attempt, and the lab's tools list names the method's kind.
use enrichedlab_agent_sandbox::broker::{
    coordinator::StartRun,
    model::{PrincipalClass, VerifiedIdentity},
    project::{CheckpointRequest, CreateProject, ResearchState, Visibility},
    store::{ObjectReceipt, Store},
};
use serde_json::{Value, json};
use std::{env, fs};
use time::OffsetDateTime;
use tokio_postgres::{Config, NoTls, config::Host};
use uuid::Uuid;

const MANIFEST: &str = "aabe6e4575c2d36ad41d3dcda679858cddb246f1a3a7c2f4dfa208f295c0e170";
const COLLECTION: &str = "0192d3b0-0000-7000-8000-000000000001";

fn state(steps: Value) -> Value {
    json!({"schema_version":1,"goal":"Which example genes carry the most counts?",
        "constraints":["Read only the lab's files."], "plan": steps,
        "lessons":[],"next_action":"Read the counts.",
        "workspace":{"schema_version":1,"profile":"lab-objects","image_digest":format!("sha256:{}","a".repeat(64)),
        "source_digest":format!("sha256:{}","b".repeat(64)),"input_refs":["input://qualification/example"],
        "tool_refs":["tool://lab-objects"],"preparation_receipt":null}})
}

fn step(key: &str) -> Value {
    json!({"action_key": key, "title": key, "status": "pending", "task_id": null, "evidence_refs": [],
        "rationale": "Answer the question."})
}

fn objects() -> Value {
    json!({"collection": COLLECTION, "bucket": "example-lab-objects", "prefix": "afri/counts/processed/",
        "generation": "g20261030-afri-2", "manifest_sha256": MANIFEST, "files": ["a b (1).csv", "dir/c.tsv"]})
}

fn action(key: &str, profile: &str, objects: Option<Value>) -> Value {
    let mut a = json!({"action_key": key, "profile": profile, "objective": "Read the counts.",
        "request_sha256": "e".repeat(64)});
    if let Some(objects) = objects {
        a["objects"] = objects;
    }
    a
}

#[tokio::test]
#[ignore = "isolated PostgreSQL through qualify_remote_broker.sh lab-objects"]
async fn a_lab_objects_step_is_committed_read_by_its_lease_and_receipted() {
    let dsn_file = env::var_os("BROKER_QUALIFICATION_MIGRATION_DSN_FILE").unwrap();
    let dsn = fs::read_to_string(&dsn_file).unwrap();
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
    coordinator.ready_for_research_results().await.unwrap();
    worker.ready_for_lab_objects().await.unwrap();
    // Without EXECUTE on the lease read the worker is not ready.
    owner
        .batch_execute("REVOKE EXECUTE ON FUNCTION enrichedlab_agent.broker_task_objects(text,uuid,text,integer,bigint) FROM broker_worker_qual")
        .await
        .unwrap();
    assert_eq!(
        worker.ready_for_lab_objects().await.unwrap_err().code,
        "broker_lab_objects_schema_not_ready"
    );
    owner
        .batch_execute("GRANT EXECUTE ON FUNCTION enrichedlab_agent.broker_task_objects(text,uuid,text,integer,bigint) TO broker_worker_qual")
        .await
        .unwrap();
    worker.ready_for_lab_objects().await.unwrap();
    // Without EXECUTE on the run's objects methods the coordinator is not ready.
    owner
        .batch_execute("REVOKE EXECUTE ON FUNCTION enrichedlab_agent.broker_research_run_object_methods(text,uuid,text,bigint) FROM broker_coordinator_qual")
        .await
        .unwrap();
    assert!(coordinator.ready_for_research_results().await.is_err());
    owner
        .batch_execute("GRANT EXECUTE ON FUNCTION enrichedlab_agent.broker_research_run_object_methods(text,uuid,text,bigint) TO broker_coordinator_qual")
        .await
        .unwrap();
    coordinator.ready_for_research_results().await.unwrap();
    // The route is request-only: no person's generic submission creates its tasks.
    let request_only: bool = owner
        .query_one(
            "SELECT enrichedlab_agent.broker_request_only_route('server-objects')",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert!(request_only);

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
        SELECT tenant_id,k,n,description,harness,thinking,r,controller_profile,gateway_profile,image_ref,cpu_limit,
          memory_limit,state_profile,role,tool_bundle,max_run_seconds,1,max_provider_attempts
        FROM enrichedlab_agent.broker_agent_profiles,
          (VALUES ('lab-objects','Read the lab''s files','server-objects'),
                  ('other-method','Another method','sandbox')) v(k,n,r)
        WHERE tenant_id='research-site' AND profile_key='general-research';
        INSERT INTO enrichedlab_agent.broker_group_profile_grants(tenant_id,group_key,profile_key)
          SELECT 'research-site','pilot-lab',k FROM unnest(ARRAY['lab-objects','other-method']) k;").await.unwrap();
    // The lab's tools list names the kind.
    let kind: String = owner
        .query_one(
            "SELECT enrichedlab_agent.broker_lab_tool_json('research-site',$1,'pilot-lab',p)->>'kind'
               FROM enrichedlab_agent.broker_agent_profiles p WHERE tenant_id='research-site' AND profile_key='lab-objects'",
            &[&a],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(kind, "objects_method");

    let project = Uuid::new_v4();
    api.create_project(
        a,
        &CreateProject {
            project_id: project,
            group: "pilot-lab".into(),
            title: "Lab files".into(),
            visibility: Visibility::Private,
        },
    )
    .await
    .unwrap();
    let plan = json!([step("counts"), step("other")]);
    let research: ResearchState = serde_json::from_value(state(plan.clone())).unwrap();
    api.checkpoint_project(
        a,
        project,
        &CheckpointRequest {
            expected_revision: 0,
            request_id: Uuid::new_v4(),
            state: research,
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
                allowed_profiles: vec!["lab-objects".into(), "other-method".into()],
                max_turns: 6,
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
    assert_eq!(claim["run"]["run_id"], json!(run));
    let generation = claim["run"]["generation"].as_i64().unwrap();
    assert_eq!(
        coordinator
            .research_run_object_methods(run, "lead-a", generation)
            .await
            .unwrap(),
        vec!["lab-objects".to_owned()]
    );
    // Another worker's lease reads nothing.
    assert!(
        coordinator
            .research_run_object_methods(run, "lead-b", generation)
            .await
            .is_err()
    );
    let revision = || async {
        owner
            .query_one(
                "SELECT revision FROM enrichedlab_agent.broker_projects WHERE project_id=$1",
                &[&project],
            )
            .await
            .unwrap()
            .get::<_, i64>(0)
    };
    // Every other shape is refused, and nothing is recorded.
    let bad = |field: &str, value: Value| {
        let mut o = objects();
        o[field] = value;
        o
    };
    let mut extra = objects();
    extra["note"] = json!("x");
    let mut missing = objects();
    missing.as_object_mut().unwrap().remove("generation");
    for (a_profile, o) in [
        ("lab-objects", None),
        ("other-method", Some(objects())),
        ("lab-objects", Some(extra)),
        ("lab-objects", Some(missing)),
        (
            "lab-objects",
            Some(bad("collection", json!("knowledge://x"))),
        ),
        ("lab-objects", Some(bad("bucket", json!("Example")))),
        ("lab-objects", Some(bad("prefix", json!("afri/counts/")))),
        (
            "lab-objects",
            Some(bad("prefix", json!("afri/../processed/"))),
        ),
        ("lab-objects", Some(bad("generation", json!("g/1")))),
        (
            "lab-objects",
            Some(bad("manifest_sha256", json!("A".repeat(64)))),
        ),
        ("lab-objects", Some(bad("files", json!([])))),
        (
            "lab-objects",
            Some(bad(
                "files",
                json!((0..21).map(|n| format!("f{n}")).collect::<Vec<_>>()),
            )),
        ),
        ("lab-objects", Some(bad("files", json!(["a.csv", "a.csv"])))),
        ("lab-objects", Some(bad("files", json!(["../a.csv"])))),
        ("lab-objects", Some(bad("files", json!(["d/./a.csv"])))),
        ("lab-objects", Some(bad("files", json!(["/a.csv"])))),
        ("lab-objects", Some(bad("files", json!(["a//b.csv"])))),
        ("lab-objects", Some(bad("files", json!(["a\nb.csv"])))),
        ("lab-objects", Some(bad("files", json!([7])))),
        ("lab-objects", Some(bad("bucket", json!(true)))),
        ("lab-objects", Some(bad("generation", json!(123)))),
        ("lab-objects", Some(bad("collection", json!([COLLECTION])))),
        (
            "lab-objects",
            Some(bad("manifest_sha256", json!({ "sha": MANIFEST }))),
        ),
    ] {
        let refused = coordinator
            .commit_research_turn(
                run,
                "lead-a",
                generation,
                revision().await,
                &state(plan.clone()),
                &json!([action("counts", a_profile, o.clone())]),
                &json!({"schema_version": 1, "test": "bounded metadata"}),
                "delegate",
            )
            .await
            .unwrap_err();
        assert_eq!(refused.code, "objects_invalid", "{a_profile} {o:?}");
    }
    let none: i64 = owner
        .query_one(
            "SELECT count(*) FROM enrichedlab_agent.broker_object_requests",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(none, 0);

    // The step the coordinator admitted is committed with its request.
    let committed = coordinator
        .commit_research_turn(
            run,
            "lead-a",
            generation,
            revision().await,
            &state(plan.clone()),
            &json!([action("counts", "lab-objects", Some(objects()))]),
            &json!({"schema_version": 1, "test": "bounded metadata"}),
            "delegate",
        )
        .await
        .unwrap();
    let task: Uuid = serde_json::from_value(committed["tasks"][0]["task_id"].clone()).unwrap();
    let row = owner
        .query_one(
            "SELECT files, prefix, request->'origin'->>'action_key', request->>'schema'
               FROM enrichedlab_agent.broker_object_requests WHERE task_id=$1",
            &[&task],
        )
        .await
        .unwrap();
    assert_eq!(
        row.get::<_, Vec<String>>(0),
        vec!["a b (1).csv".to_owned(), "dir/c.tsv".to_owned()]
    );
    assert_eq!(row.get::<_, String>(1), "afri/counts/processed/");
    assert_eq!(row.get::<_, String>(2), "counts");
    assert_eq!(
        row.get::<_, String>(3),
        "enrichedlab.lab-objects-request.v1"
    );

    // The worker reads the request under its own lease only, and records one receipt per attempt.
    assert!(
        worker
            .task_objects(task, "worker-a", 1, 1)
            .await
            .unwrap()
            .is_none()
    );
    let claimed = worker.claim_task("worker-a", 60).await.unwrap().unwrap();
    assert_eq!(claimed.id, task);
    assert_eq!(claimed.route, "server-objects");
    let read = worker
        .task_objects(task, "worker-a", claimed.attempt, claimed.lease_generation)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(read.bucket, "example-lab-objects");
    assert_eq!(read.manifest_sha256, MANIFEST);
    assert_eq!(read.files.len(), 2);
    assert!(
        worker
            .task_objects(task, "worker-b", claimed.attempt, claimed.lease_generation)
            .await
            .unwrap()
            .is_none()
    );
    let now = OffsetDateTime::now_utc();
    let receipt = ObjectReceipt {
        status: "completed",
        error_code: None,
        files: Some(2),
        bytes: Some(10),
        tables: Some(
            json!([{"path": "results/lab_data/afri/a_b__1_.csv", "rows": 2, "columns": 2}]),
        ),
        package_sha256: Some("f".repeat(64)),
        started_at: now,
        finished_at: now,
    };
    worker
        .record_object_receipt(
            task,
            "worker-a",
            claimed.attempt,
            claimed.lease_generation,
            &receipt,
        )
        .await
        .unwrap();
    // The same receipt again is accepted; a different one is refused.
    worker
        .record_object_receipt(
            task,
            "worker-a",
            claimed.attempt,
            claimed.lease_generation,
            &receipt,
        )
        .await
        .unwrap();
    // Every recorded field counts: the file count, bytes, table facts and times.
    let changed = [
        ObjectReceipt {
            files: Some(1),
            ..receipt.clone()
        },
        ObjectReceipt {
            bytes: Some(11),
            ..receipt.clone()
        },
        ObjectReceipt {
            tables: Some(
                json!([{"path": "results/lab_data/afri/a_b__1_.csv", "rows": 3, "columns": 2}]),
            ),
            ..receipt.clone()
        },
        ObjectReceipt {
            finished_at: now + time::Duration::seconds(1),
            ..receipt.clone()
        },
    ];
    for other in &changed {
        assert_eq!(
            worker
                .record_object_receipt(
                    task,
                    "worker-a",
                    claimed.attempt,
                    claimed.lease_generation,
                    other
                )
                .await
                .unwrap_err()
                .code,
            "idempotency_conflict"
        );
    }
    drop(owner);
    connection.abort();
}
