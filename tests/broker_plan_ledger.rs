//! The plan ledger (migration 036) in isolated PostgreSQL: each checkpoint writer records the plan steps
//! its revision changed under its own label (the lead's turn, reconciliation from analysis records, a person, a service
//! account); the stored rows equal a fresh comparison of the checkpoints; a revision an older writer left unrecorded is
//! labeled when the project is next written; only the writers record, the rows cannot change, and only a project reader
//! can read them.
use enrichedlab_agent_sandbox::broker::{
    coordinator::{StartRun, reconcile},
    model::{PrincipalClass, VerifiedIdentity},
    project::{
        Capability, CheckpointRequest, CreateProject, ProjectGrant, ResearchState, StepStatus,
        Visibility,
    },
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

fn step(key: &str, title: &str, rationale: &str) -> Value {
    json!({"action_key":key,"title":title,"status":"pending","task_id":null,"evidence_refs":[],"rationale":rationale})
}

fn checkpoint(expected: i64, state: &ResearchState) -> CheckpointRequest {
    CheckpointRequest {
        expected_revision: expected,
        request_id: Uuid::new_v4(),
        state: state.clone(),
    }
}

/// The ledger's revision `n` (revisions are listed oldest first and every one is recorded here).
fn revision(ledger: &Value, n: i64) -> &Value {
    ledger["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["revision"] == n)
        .unwrap_or_else(|| panic!("revision {n} not recorded: {ledger}"))
}

#[tokio::test]
#[ignore = "isolated PostgreSQL through qualify_remote_broker.sh plan-ledger"]
async fn each_writer_records_its_plan_changes_under_its_own_label() {
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
    api.ready_for_review_api().await.unwrap();
    owner
        .batch_execute(
            "INSERT INTO enrichedlab_agent.broker_principal_invitations(tenant_id,email,group_key,role) VALUES
        ('research-site','outsider@example.invalid','pilot-lab','researcher'),
        ('research-site','workload@example.invalid','pilot-lab','researcher');",
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
        .resolve_principal(&person("owner", "researcher@example.invalid"))
        .await
        .unwrap();
    let outsider = api
        .resolve_principal(&person("outsider", "outsider@example.invalid"))
        .await
        .unwrap();
    let workload = api
        .resolve_principal(&VerifiedIdentity {
            issuer: "kubernetes://serviceaccount".into(),
            class: PrincipalClass::ServiceAccount,
            subject: "system:serviceaccount:qual:caller".into(),
            email: "workload@example.invalid".into(),
            display_name: Some("Qualification caller".into()),
        })
        .await
        .unwrap();
    let project = Uuid::new_v4();
    api.create_project(
        me,
        &CreateProject {
            project_id: project,
            group: "pilot-lab".into(),
            title: "Plan ledger".into(),
            visibility: Visibility::Private,
        },
    )
    .await
    .unwrap();
    api.grant_project(
        me,
        project,
        &ProjectGrant {
            principal_id: workload,
            capabilities: vec![Capability::Read, Capability::Steer],
        },
    )
    .await
    .unwrap();

    // 1. A person's first checkpoint is recorded with its author and no step rows.
    let mut state = research();
    api.checkpoint_project(me, project, &checkpoint(0, &state))
        .await
        .unwrap();
    let ledger = api.project_plan_ledger(me, project).await.unwrap();
    assert_eq!(ledger["unrecorded_revisions"], 0);
    assert_eq!(ledger["truncated"], false);
    let first = revision(&ledger, 1);
    assert_eq!(first["source"], "person");
    assert_eq!(first["principal_id"], me.to_string());
    assert_eq!(
        (first["run_id"].is_null(), first["turn"].is_null()),
        (true, true)
    );
    assert_eq!(first["steps"], json!([]));

    // 2. The lead's turn plans a step and submits its analysis in one revision.
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
    state.plan.push(
        serde_json::from_value(step(
            "inspect-source",
            "Inspect the input units",
            "Confirm the experimental unit before estimating a contrast.",
        ))
        .unwrap(),
    );
    let actions = json!([{"action_key":"inspect-source","profile":"general-research",
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
    let ledger = api.project_plan_ledger(me, project).await.unwrap();
    let turn = revision(&ledger, 2);
    assert_eq!(
        (&turn["source"], &turn["run_id"], &turn["turn"]),
        (&json!("lead_turn"), &json!(run.to_string()), &json!(1))
    );
    assert!(turn["principal_id"].is_null());
    assert_eq!(
        turn["steps"],
        json!([{"action_key":"inspect-source","step_number":1,"changes":["planned"],"status_before":null,
            "status_after":"active","task_id":task,"result_count":0,"title":"Inspect the input units",
            "rationale":"Confirm the experimental unit before estimating a contrast.","task_status":null,"error_code":null}])
    );

    // 3. The analysis loses its worker; reconciliation marks the step uncertain and keeps the record it read.
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_tasks SET status='failed',error_code='worker_lease_expired',
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
    let generation_b = claim["run"]["generation"].as_i64().unwrap();
    let mut restored: ResearchState =
        serde_json::from_value(claim["project"]["checkpoint"]["state"].clone()).unwrap();
    assert!(!reconcile(&mut restored, &claim["task_facts"]).unwrap());
    assert_eq!(restored.plan[0].status, StepStatus::Uncertain);
    assert_eq!(
        coordinator
            .reconcile_research_run(
                run,
                "lead-b",
                generation_b,
                2,
                &serde_json::to_value(&restored).unwrap()
            )
            .await
            .unwrap(),
        3
    );
    let ledger = api.project_plan_ledger(me, project).await.unwrap();
    let reconciled = revision(&ledger, 3);
    assert_eq!(
        (
            &reconciled["source"],
            &reconciled["run_id"],
            &reconciled["turn"]
        ),
        (
            &json!("analysis_record"),
            &json!(run.to_string()),
            &Value::Null
        )
    );
    assert_eq!(
        reconciled["steps"],
        json!([{"action_key":"inspect-source","step_number":1,"changes":["status"],"status_before":"active",
            "status_after":"uncertain","task_id":task,"result_count":0,"title":null,"rationale":null,
            "task_status":"failed","error_code":"worker_lease_expired"}])
    );

    // 4. A person resolves the step as failed and rewrites its reason; the new text is kept.
    restored.plan[0].status = StepStatus::Failed;
    restored.plan[0].rationale = "The worker stopped before the input units were confirmed.".into();
    let resolved = checkpoint(3, &restored);
    assert_eq!(
        api.checkpoint_project(me, project, &resolved)
            .await
            .unwrap(),
        4
    );
    let ledger = api.project_plan_ledger(me, project).await.unwrap();
    assert_eq!(
        revision(&ledger, 4)["steps"],
        json!([{"action_key":"inspect-source","step_number":1,"changes":["text","status"],"status_before":"uncertain",
            "status_after":"failed","task_id":task,"result_count":0,"title":"Inspect the input units",
            "rationale":"The worker stopped before the input units were confirmed.","task_status":null,"error_code":null}])
    );
    // An idempotent repeat of that checkpoint records nothing again.
    assert_eq!(
        api.checkpoint_project(me, project, &resolved)
            .await
            .unwrap(),
        4
    );

    // 5. A service account's checkpoint adds a step and removes the failed one.
    let mut replaced = restored.clone();
    replaced.plan = vec![
        serde_json::from_value(step(
            "compare-groups",
            "Compare the groups",
            "Estimate the contrast with the animal as the unit.",
        ))
        .unwrap(),
    ];
    assert_eq!(
        api.checkpoint_project(workload, project, &checkpoint(4, &replaced))
            .await
            .unwrap(),
        5
    );
    let ledger = api.project_plan_ledger(workload, project).await.unwrap();
    let automated = revision(&ledger, 5);
    assert_eq!(
        (&automated["source"], &automated["principal_id"]),
        (&json!("service_account"), &json!(workload.to_string()))
    );
    assert_eq!(
        automated["steps"],
        json!([{"action_key":"compare-groups","step_number":1,"changes":["planned"],"status_before":null,
                "status_after":"pending","task_id":null,"result_count":0,"title":"Compare the groups",
                "rationale":"Estimate the contrast with the animal as the unit.","task_status":null,"error_code":null},
               {"action_key":"inspect-source","step_number":1,"changes":["removed"],"status_before":"failed",
                "status_after":null,"task_id":task,"result_count":0,"title":null,"rationale":null,
                "task_status":null,"error_code":null}])
    );

    // 6. The stored rows equal a fresh comparison of every pair of checkpoints, and each revision counts its rows.
    let compare = "WITH computed AS (
          SELECT c.revision, x.action_key, x.step_number, x.changes, x.status_before, x.status_after, x.task_id,
                 x.result_count, x.title, x.rationale
          FROM enrichedlab_agent.broker_project_checkpoints c
          LEFT JOIN enrichedlab_agent.broker_project_checkpoints p
            ON p.tenant_id = c.tenant_id AND p.project_id = c.project_id AND p.revision = c.revision - 1
          CROSS JOIN LATERAL enrichedlab_agent.broker_plan_changes(p.state, c.state) x
          WHERE c.tenant_id = 'research-site' AND c.project_id = $1),
        stored AS (
          SELECT revision, action_key, step_number, changes, status_before, status_after, task_id, result_count, title,
                 rationale
          FROM enrichedlab_agent.broker_plan_events WHERE tenant_id = 'research-site' AND project_id = $1)
        SELECT (SELECT count(*) FROM (SELECT * FROM computed EXCEPT ALL SELECT * FROM stored) a)
             + (SELECT count(*) FROM (SELECT * FROM stored EXCEPT ALL SELECT * FROM computed) b),
               (SELECT count(*) FROM stored),
               (SELECT count(*) FROM enrichedlab_agent.broker_plan_revisions r
                 WHERE r.tenant_id = 'research-site' AND r.project_id = $1 AND r.step_changes <> (
                   SELECT count(*) FROM enrichedlab_agent.broker_plan_events e
                   WHERE e.tenant_id = r.tenant_id AND e.project_id = r.project_id AND e.revision = r.revision))";
    let row = owner.query_one(compare, &[&project]).await.unwrap();
    assert_eq!(
        (
            row.get::<_, i64>(0),
            row.get::<_, i64>(1),
            row.get::<_, i64>(2)
        ),
        (0, 5, 0)
    );

    // 7. Revisions an older writer left unrecorded: the read counts them, and the next write records them first, as
    // the lead's when a recorded turn produced one and as unknown otherwise.
    let mut skipped = replaced.clone();
    skipped.plan[0].status = StepStatus::Skipped;
    let mut renamed = skipped.clone();
    renamed.plan[0].title = "Compare the delivery groups".into();
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_project_checkpoints VALUES
               ('research-site',$1,6,$2,gen_random_uuid(),$3,clock_timestamp()),
               ('research-site',$1,7,$2,gen_random_uuid(),$4,clock_timestamp())",
            &[
                &project,
                &me,
                &serde_json::to_value(&skipped).unwrap(),
                &serde_json::to_value(&renamed).unwrap(),
            ],
        )
        .await
        .unwrap();
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_projects SET revision=7 WHERE project_id=$1",
            &[&project],
        )
        .await
        .unwrap();
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_research_turns VALUES
               ('research-site',$1,2,$2,5,6,'{\"schema_version\":1}',clock_timestamp())",
            &[&run, &generation_b],
        )
        .await
        .unwrap();
    let ledger = api.project_plan_ledger(me, project).await.unwrap();
    assert_eq!(ledger["unrecorded_revisions"], 2);
    let mut next = renamed.clone();
    next.next_action = "Wait for a person to choose the comparison.".into();
    assert_eq!(
        api.checkpoint_project(me, project, &checkpoint(7, &next))
            .await
            .unwrap(),
        8
    );
    let ledger = api.project_plan_ledger(me, project).await.unwrap();
    assert_eq!(ledger["unrecorded_revisions"], 0);
    let older = revision(&ledger, 6);
    assert_eq!(
        (&older["source"], &older["run_id"], &older["turn"]),
        (&json!("lead_turn"), &json!(run.to_string()), &json!(2))
    );
    assert_eq!(older["steps"][0]["changes"], json!(["status"]));
    assert_eq!(
        (
            &revision(&ledger, 7)["source"],
            &revision(&ledger, 7)["steps"][0]["changes"],
            &revision(&ledger, 7)["steps"][0]["title"]
        ),
        (
            &json!("recorded_before_ledger"),
            &json!(["text"]),
            &json!("Compare the delivery groups")
        )
    );
    assert_eq!(
        (
            &revision(&ledger, 8)["source"],
            &revision(&ledger, 8)["steps"]
        ),
        (&json!("person"), &json!([]))
    );
    // Replaying again records nothing: the migration's replay is safe to repeat.
    let replayed: i32 = owner
        .query_one(
            "SELECT enrichedlab_agent.broker_plan_replay('research-site',$1,NULL)",
            &[&project],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(replayed, 0);
    assert_eq!(
        owner
            .query_one(compare, &[&project])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );

    // 8. Only the writers record: the runtime roles can call neither the recorder nor its parts, nor write the rows.
    for role in ["broker_api_qual", "broker_coordinator_qual"] {
        let (client, raw) = dsn
            .replace("user=postgres", &format!("user={role}"))
            .parse::<Config>()
            .unwrap()
            .connect(NoTls)
            .await
            .unwrap();
        let raw = tokio::spawn(raw);
        for statement in [
            "SELECT enrichedlab_agent.broker_plan_record('research-site',$1,8,'person',NULL,NULL,NULL)",
            "SELECT enrichedlab_agent.broker_plan_record_one('research-site',$1,8,'person',NULL,NULL,NULL)",
            "SELECT enrichedlab_agent.broker_plan_replay('research-site',$1,NULL)",
            "SELECT enrichedlab_agent.broker_write_checkpoint('research-site',$1,$1,8,$1,'{}','person',NULL,NULL,NULL)",
            "SELECT count(*) FROM enrichedlab_agent.broker_plan_changes('{}','{}') WHERE $1::uuid IS NOT NULL",
            "INSERT INTO enrichedlab_agent.broker_plan_revisions VALUES ('research-site',$1,9,'person',NULL,NULL,$1,0,now())",
            "SELECT count(*) FROM enrichedlab_agent.broker_plan_events WHERE project_id=$1",
        ] {
            let error = client.query(statement, &[&project]).await.unwrap_err();
            assert_eq!(
                error.code(),
                Some(&SqlState::INSUFFICIENT_PRIVILEGE),
                "{role}: {statement}"
            );
        }
        raw.abort();
    }
    // The rows cannot change, even for the owner.
    for statement in [
        "UPDATE enrichedlab_agent.broker_plan_events SET title='Changed' WHERE project_id=$1",
        "DELETE FROM enrichedlab_agent.broker_plan_events WHERE project_id=$1",
        "UPDATE enrichedlab_agent.broker_plan_revisions SET source='person' WHERE project_id=$1",
        "DELETE FROM enrichedlab_agent.broker_plan_revisions WHERE project_id=$1",
    ] {
        assert!(
            owner.execute(statement, &[&project]).await.is_err(),
            "{statement}"
        );
    }

    // 9. Only a project reader reads the ledger; a lab member outside the project is refused like a missing project.
    assert_eq!(
        api.project_plan_ledger(outsider, project)
            .await
            .unwrap_err()
            .kind,
        StoreErrorKind::NotFound
    );
    assert_eq!(
        api.project_plan_ledger(me, Uuid::new_v4())
            .await
            .unwrap_err()
            .kind,
        StoreErrorKind::NotFound
    );
    connection.abort();
}
