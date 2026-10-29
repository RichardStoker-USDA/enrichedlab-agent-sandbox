//! Plan pages (migration 048) in isolated PostgreSQL: the archive writer moves only finished
//! steps, under the run's lease and the project's expected revision, into the next immutable page and records that
//! revision in the plan ledger as `plan_archive`; the claim carries every archived key, the counts by status and by
//! method, the newest archived steps and each planned analysis's method; a project then grows past the 128 steps one
//! working plan holds, an archived analysis's key cannot be planned again, the pages cannot change and only the
//! coordinator archives.
use enrichedlab_agent_sandbox::broker::{
    coordinator::{StartRun, newest_tasks, reconcile},
    model::{PrincipalClass, SubmitTaskRequest, VerifiedIdentity},
    project::{CheckpointRequest, CreateProject, ResearchState, StepStatus, Visibility},
    store::{Store, StoreErrorKind},
};
use serde_json::{Value, json};
use std::{env, fs};
use tokio_postgres::{Config, NoTls, config::Host, error::SqlState};
use uuid::Uuid;

fn research() -> ResearchState {
    serde_json::from_value(json!({"schema_version":1,"goal":"Inspect the retained input and estimate a treatment contrast.",
        "constraints":["Use the animal as the experimental unit."],"plan":[],"lessons":[],"next_action":"Select an available tool.",
        "workspace":{"schema_version":1,"profile":"general-research","image_digest":format!("sha256:{}","a".repeat(64)),
        "source_digest":format!("sha256:{}","b".repeat(64)),"input_refs":["input://public/example"],"tool_refs":[],
        "preparation_receipt":null}})).unwrap()
}

fn step(key: &str, status: &str) -> Value {
    json!({"action_key":key,"title":format!("Step {key}"),"status":status,"task_id":null,"evidence_refs":[],
        "rationale":"Part of a long project."})
}

fn keys(state: &Value) -> Vec<String> {
    state["plan"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["action_key"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
#[ignore = "isolated PostgreSQL through qualify_remote_broker.sh plan-pages"]
async fn finished_steps_move_into_immutable_pages_and_a_project_grows_past_one_working_plan() {
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
            title: "A long project".into(),
            visibility: Visibility::Private,
        },
    )
    .await
    .unwrap();

    // 1. A person's checkpoint holds 126 steps: 100 skipped without an analysis and 26 pending.
    let mut state = research();
    for i in 0..126 {
        let status = if i < 100 { "skipped" } else { "pending" };
        state
            .plan
            .push(serde_json::from_value(step(&format!("s-{i}"), status)).unwrap());
    }
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

    // 2. The lead's turn plans one more step and submits its analysis; the claim carries an empty archive.
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
    assert_eq!(
        claim["plan_pages"],
        json!({"pages":0,"steps":0,"keys":[],"tasks":[],"by_status":{},"by_method":{},"newest":[]})
    );
    let generation = claim["run"]["generation"].as_i64().unwrap();
    state
        .plan
        .push(serde_json::from_value(step("run-a", "pending")).unwrap());
    let actions = json!([{"action_key":"run-a","profile":"general-research",
        "objective":"Inspect the retained public source.","request_sha256":"c".repeat(64)}]);
    let committed = coordinator
        .commit_research_turn(
            run,
            "lead-a",
            generation,
            1,
            &serde_json::to_value(&state).unwrap(),
            &actions,
            &json!({"schema_version":1}),
            "delegate",
        )
        .await
        .unwrap();
    assert_eq!(committed["revision"], 2);
    let task = committed["tasks"][0]["task_id"]
        .as_str()
        .unwrap()
        .to_owned();

    // 3. The analysis fails; the next claim names its method in its record, and reconciliation marks the step failed.
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_tasks SET status='failed',error_code='query_failed',
             finished_at=clock_timestamp() WHERE task_id=$1::text::uuid",
            &[&task],
        )
        .await
        .unwrap();
    owner.execute("UPDATE enrichedlab_agent.broker_research_runs SET available_at=clock_timestamp()-interval '1 second' WHERE run_id=$1",&[&run]).await.unwrap();
    let claim = coordinator
        .claim_research_run("lead-b", 90)
        .await
        .unwrap()
        .unwrap();
    let generation = claim["run"]["generation"].as_i64().unwrap();
    let fact = claim["task_facts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["task_id"] == json!(task))
        .unwrap()
        .clone();
    assert_eq!(
        (&fact["profile"], &fact["status"], &fact["error_code"]),
        (
            &json!("general-research"),
            &json!("failed"),
            &json!("query_failed")
        )
    );
    let mut restored: ResearchState =
        serde_json::from_value(claim["project"]["checkpoint"]["state"].clone()).unwrap();
    assert!(!reconcile(&mut restored, &claim["task_facts"]).unwrap());
    assert_eq!(restored.plan[126].status, StepStatus::Failed);
    let revision = coordinator
        .reconcile_research_run(
            run,
            "lead-b",
            generation,
            2,
            &serde_json::to_value(&restored).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(revision, 3);

    // 4. The writer refuses what it must not move, and a stale lease or revision.
    let archive = |worker: &'static str, generation: i64, revision: i64, keys: Vec<&str>| {
        let coordinator = coordinator.clone();
        let keys: Vec<String> = keys.into_iter().map(str::to_owned).collect();
        async move {
            coordinator
                .archive_research_plan(run, worker, generation, revision, &keys)
                .await
        }
    };
    for (keys, code) in [
        (vec!["s-100"], "plan_archive_unfinished"),
        (vec!["s-0", "missing"], "plan_archive_invalid"),
        (vec!["s-0", "s-0"], "plan_archive_invalid"),
        (vec![], "plan_archive_invalid"),
    ] {
        let error = archive("lead-b", generation, 3, keys).await.unwrap_err();
        assert_eq!((error.kind, error.code), (StoreErrorKind::Invalid, code));
    }
    assert_eq!(
        archive("lead-a", generation, 3, vec!["s-0"])
            .await
            .unwrap_err()
            .kind,
        StoreErrorKind::LeaseLost
    );
    assert_eq!(
        archive("lead-b", generation, 2, vec!["s-0"])
            .await
            .unwrap_err()
            .kind,
        StoreErrorKind::Conflict
    );
    // A missing generation, worker or revision refuses instead of passing the fence.
    {
        let (client, raw) = dsn
            .replace("user=postgres", "user=broker_coordinator_qual")
            .parse::<Config>()
            .unwrap()
            .connect(NoTls)
            .await
            .unwrap();
        let raw = tokio::spawn(raw);
        for statement in [
            "SELECT enrichedlab_agent.broker_archive_research_plan('research-site',$1::uuid,'lead-b',NULLIF($2::bigint,$2::bigint),3::bigint,ARRAY['s-0'])",
            "SELECT enrichedlab_agent.broker_archive_research_plan('research-site',$1::uuid,NULL::text,$2::bigint,3::bigint,ARRAY['s-0'])",
        ] {
            let error = client
                .query(statement, &[&run, &generation])
                .await
                .unwrap_err();
            assert!(
                error
                    .as_db_error()
                    .is_some_and(|e| e.message() == "broker:lease_fence_lost"),
                "{statement}: {:?}",
                error
                    .as_db_error()
                    .map(|e| (e.code().code().to_owned(), e.message().to_owned()))
            );
        }
        let error = client
            .query(
                "SELECT enrichedlab_agent.broker_archive_research_plan('research-site',$1::uuid,'lead-b',$2::bigint,NULL::bigint,ARRAY['s-0'])",
                &[&run, &generation],
            )
            .await
            .unwrap_err();
        assert!(
            error
                .as_db_error()
                .is_some_and(|e| e.message() == "broker:checkpoint_conflict"),
            "{error}"
        );
        raw.abort();
    }

    // 5. The 100 skipped steps and the failed analysis move into page 1 in plan order; the working plan keeps the rest.
    let moved: Vec<String> = (0..100)
        .map(|i| format!("s-{i}"))
        .chain(["run-a".to_owned()])
        .collect();
    let archived = coordinator
        .archive_research_plan(run, "lead-b", generation, 3, &moved)
        .await
        .unwrap();
    assert_eq!(archived, json!({"revision": 4, "page": 1, "steps": 101}));
    let row = owner
        .query_one(
            "SELECT c.state, p.page, p.revision, jsonb_array_length(p.steps), p.steps_sha256 =
               'sha256:'||encode(sha256(convert_to(p.steps::text,'UTF8')),'hex'), p.run_id
             FROM enrichedlab_agent.broker_plan_pages p
             JOIN enrichedlab_agent.broker_project_checkpoints c
               ON c.tenant_id=p.tenant_id AND c.project_id=p.project_id AND c.revision=p.revision
             WHERE p.project_id=$1",
            &[&project],
        )
        .await
        .unwrap();
    let kept: Value = row.get(0);
    assert_eq!(
        keys(&kept),
        (100..126).map(|i| format!("s-{i}")).collect::<Vec<_>>()
    );
    assert_eq!(
        (
            row.get::<_, i32>(1),
            row.get::<_, i64>(2),
            row.get::<_, i32>(3),
            row.get::<_, bool>(4),
            row.get::<_, Uuid>(5)
        ),
        (1, 4, 101, true, run)
    );
    // The plan ledger records the revision under the archive writer's label, with each moved step removed.
    let ledger = owner
        .query_one(
            "SELECT r.source, r.run_id, r.turn IS NULL AND r.principal_id IS NULL, r.step_changes,
               (SELECT count(*) FROM enrichedlab_agent.broker_plan_events e WHERE e.tenant_id=r.tenant_id
                  AND e.project_id=r.project_id AND e.revision=r.revision AND e.changes=ARRAY['removed'])
             FROM enrichedlab_agent.broker_plan_revisions r WHERE r.project_id=$1 AND r.revision=4",
            &[&project],
        )
        .await
        .unwrap();
    assert_eq!(
        (
            ledger.get::<_, String>(0),
            ledger.get::<_, Uuid>(1),
            ledger.get::<_, bool>(2),
            ledger.get::<_, i32>(3),
            ledger.get::<_, i64>(4)
        ),
        ("plan_archive".to_owned(), run, true, 101, 101)
    );
    // The writer works from the checkpoint it holds: the same keys again find no such steps.
    assert_eq!(
        archive("lead-b", generation, 4, vec!["s-0"])
            .await
            .unwrap_err()
            .code,
        "plan_archive_invalid"
    );

    // 6. The next claim carries the archive: every key in order, counts by status and by method, the newest sixteen.
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_research_runs SET lease_until=clock_timestamp(),
             available_at=clock_timestamp()-interval '1 second' WHERE run_id=$1",
            &[&run],
        )
        .await
        .unwrap();
    let claim = coordinator
        .claim_research_run("lead-c", 90)
        .await
        .unwrap()
        .unwrap();
    let pages = &claim["plan_pages"];
    assert_eq!((&pages["pages"], &pages["steps"]), (&json!(1), &json!(101)));
    assert_eq!(pages["keys"], json!(moved));
    assert_eq!(pages["tasks"], json!([task]));
    assert_eq!(pages["by_status"], json!({"failed": 1, "skipped": 100}));
    assert_eq!(
        pages["by_method"],
        json!({"general-research": {"failed": 1}})
    );
    let newest = pages["newest"].as_array().unwrap();
    assert_eq!(newest.len(), 16);
    assert_eq!(
        newest[0],
        json!({"action_key":"run-a","title":"Step run-a","status":"failed"})
    );
    assert_eq!(newest[1]["action_key"], "s-99");

    // 7. The project grows past one working plan: two new steps make 128 steps planned in all, and the archived
    // analysis's key cannot be planned again.
    let generation = claim["run"]["generation"].as_i64().unwrap();
    let mut grown: ResearchState =
        serde_json::from_value(claim["project"]["checkpoint"]["state"].clone()).unwrap();
    let mut reused = grown.clone();
    reused
        .plan
        .push(serde_json::from_value(step("run-a", "pending")).unwrap());
    let reuse = coordinator
        .commit_research_turn(
            run,
            "lead-c",
            generation,
            4,
            &serde_json::to_value(&reused).unwrap(),
            &json!([{"action_key":"run-a","profile":"general-research",
                "objective":"Inspect it again.","request_sha256":"d".repeat(64)}]),
            &json!({"schema_version":1}),
            "delegate",
        )
        .await
        .unwrap_err();
    assert_eq!(reuse.code, "research_run_invalid");
    // A skipped step's archived key has no analysis; the database still refuses one under it, so a coordinator that does
    // not know the archive (the rollback image) cannot plan it again.
    let mut skipped_again = grown.clone();
    skipped_again
        .plan
        .push(serde_json::from_value(step("s-1", "pending")).unwrap());
    let reuse = coordinator
        .commit_research_turn(
            run,
            "lead-c",
            generation,
            4,
            &serde_json::to_value(&skipped_again).unwrap(),
            &json!([{"action_key":"s-1","profile":"general-research",
                "objective":"Inspect it again.","request_sha256":"9".repeat(64)}]),
            &json!({"schema_version":1}),
            "delegate",
        )
        .await
        .unwrap_err();
    assert_eq!(reuse.code, "research_run_invalid");
    // The reserved keys cannot change either.
    for statement in [
        "UPDATE enrichedlab_agent.broker_plan_archived_keys SET status='completed' WHERE project_id=$1",
        "DELETE FROM enrichedlab_agent.broker_plan_archived_keys WHERE project_id=$1",
    ] {
        assert!(
            owner.execute(statement, &[&project]).await.is_err(),
            "{statement}"
        );
    }
    for key in ["run-b", "run-c"] {
        grown
            .plan
            .push(serde_json::from_value(step(key, "pending")).unwrap());
    }
    let committed = coordinator
        .commit_research_turn(
            run,
            "lead-c",
            generation,
            4,
            &serde_json::to_value(&grown).unwrap(),
            &json!([{"action_key":"run-b","profile":"general-research","objective":"Estimate the contrast.",
                    "request_sha256":"e".repeat(64)},
                {"action_key":"run-c","profile":"general-research","objective":"Check the units.",
                    "request_sha256":"f".repeat(64)}]),
            &json!({"schema_version":1}),
            "delegate",
        )
        .await
        .unwrap();
    assert_eq!(committed["revision"], 5);
    let planned: i64 = owner
        .query_one(
            "SELECT (SELECT jsonb_array_length(c.state->'plan') FROM enrichedlab_agent.broker_project_checkpoints c
                       WHERE c.project_id=$1 AND c.revision=5)
                  + (SELECT sum(jsonb_array_length(steps)) FROM enrichedlab_agent.broker_plan_pages WHERE project_id=$1)",
            &[&project],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(planned, 129);

    // 8. The pages cannot change, even for the owner, and only the coordinator archives; neither runtime role reads
    // the pages directly.
    for statement in [
        "UPDATE enrichedlab_agent.broker_plan_pages SET steps='[]' WHERE project_id=$1",
        "DELETE FROM enrichedlab_agent.broker_plan_pages WHERE project_id=$1",
    ] {
        assert!(
            owner.execute(statement, &[&project]).await.is_err(),
            "{statement}"
        );
    }
    for (role, statement) in [
        (
            "broker_api_qual",
            "SELECT enrichedlab_agent.broker_archive_research_plan('research-site',$1,'x',1,1,ARRAY['s-100'])",
        ),
        (
            "broker_api_qual",
            "SELECT count(*) FROM enrichedlab_agent.broker_plan_pages WHERE project_id=$1",
        ),
        (
            "broker_coordinator_qual",
            "SELECT count(*) FROM enrichedlab_agent.broker_plan_pages WHERE project_id=$1",
        ),
        (
            "broker_worker_qual",
            "SELECT enrichedlab_agent.broker_archive_research_plan('research-site',$1,'x',1,1,ARRAY['s-100'])",
        ),
    ] {
        let (client, raw) = dsn
            .replace("user=postgres", &format!("user={role}"))
            .parse::<Config>()
            .unwrap()
            .connect(NoTls)
            .await
            .unwrap();
        let raw = tokio::spawn(raw);
        let error = client.query(statement, &[&project]).await.unwrap_err();
        assert_eq!(
            error.code(),
            Some(&SqlState::INSUFFICIENT_PRIVILEGE),
            "{role}: {statement}"
        );
        raw.abort();
    }

    // 9. The claim names the project's 64 newest analyses by submission across the plan and its pages.
    // A plan keeps 64 older analyses (finding-bound steps stay), and a newer one is archived; the newer stays among the
    // newest and the oldest drops, so its query still counts as run.
    api.pause_research_run(me, run).await.unwrap();
    let aged = Uuid::new_v4();
    api.create_project(
        me,
        &CreateProject {
            project_id: aged,
            group: "pilot-lab".into(),
            title: "Older analyses kept".into(),
            visibility: Visibility::Private,
        },
    )
    .await
    .unwrap();
    let mut tasks = Vec::new();
    for i in 0..65 {
        let request = SubmitTaskRequest {
            group: "pilot-lab".into(),
            profile: "general-research".into(),
            objective: "Read the public source.".into(),
            project_id: Some(aged),
            action_key: Some(format!("k-{i}")),
            parent_task_id: None,
        };
        let task = api
            .submit_project_task(
                me,
                &request,
                &format!("aged-{i}"),
                &format!("{:064x}", i + 1),
            )
            .await
            .unwrap();
        owner
            .execute(
                "UPDATE enrichedlab_agent.broker_tasks SET status='failed',error_code='query_failed',
                 finished_at=clock_timestamp(),submitted_at=timestamptz '2026-01-01 00:00:00+00'+make_interval(secs=>$2)
                 WHERE task_id=$1",
                &[&task, &f64::from(i)],
            )
            .await
            .unwrap();
        tasks.push(task);
    }
    let mut aged_state = research();
    for (i, task) in tasks.iter().enumerate() {
        let mut s = step(&format!("k-{i}"), "failed");
        s["task_id"] = json!(task);
        aged_state.plan.push(serde_json::from_value(s).unwrap());
    }
    api.checkpoint_project(
        me,
        aged,
        &CheckpointRequest {
            expected_revision: 0,
            request_id: Uuid::new_v4(),
            state: aged_state,
        },
    )
    .await
    .unwrap();
    let aged_run = api
        .start_research_run(
            me,
            aged,
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
        .claim_research_run("lead-d", 90)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim["run"]["run_id"], json!(aged_run));
    assert_eq!(newest_tasks(&claim), tasks[1..]);
    let generation = claim["run"]["generation"].as_i64().unwrap();
    coordinator
        .archive_research_plan(aged_run, "lead-d", generation, 1, &["k-64".to_owned()])
        .await
        .unwrap();
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_research_runs SET lease_until=clock_timestamp()-interval '1 second'
             WHERE run_id=$1",
            &[&aged_run],
        )
        .await
        .unwrap();
    let claim = coordinator
        .claim_research_run("lead-e", 90)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim["run"]["run_id"], json!(aged_run));
    assert_eq!(claim["plan_pages"]["tasks"], json!([tasks[64]]));
    assert_eq!(
        claim["project"]["checkpoint"]["state"]["plan"]
            .as_array()
            .unwrap()
            .len(),
        64
    );
    let newest = newest_tasks(&claim);
    assert_eq!(newest, tasks[1..]);
    assert_eq!(newest.last(), Some(&tasks[64]));
    // The archived key keeps its analysis's submission time, and an index orders a project's archived
    // analyses by it, so a claim reads only the newest 64 of them.
    let kept: bool = owner
        .query_one(
            "SELECT k.submitted_at = t.submitted_at FROM enrichedlab_agent.broker_plan_archived_keys k
             JOIN enrichedlab_agent.broker_tasks t USING (tenant_id, task_id) WHERE k.project_id=$1 AND k.action_key='k-64'",
            &[&aged],
        )
        .await
        .unwrap()
        .get(0);
    assert!(kept);
    let indexed: String = owner
        .query_one(
            "SELECT indexdef FROM pg_indexes WHERE schemaname='enrichedlab_agent'
             AND indexname='broker_plan_archived_keys_newest'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert!(
        indexed.contains(
            "(tenant_id, project_id, submitted_at DESC, task_id DESC) WHERE (task_id IS NOT NULL)"
        ),
        "{indexed}"
    );
    connection.abort();
}
