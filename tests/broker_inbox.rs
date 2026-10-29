//! What waits for each person (migration 037) in isolated PostgreSQL: the caller's items across
//! kinds, each only for someone who may act on it; a person marks an item as seen, idempotently, and a changed item
//! reads as new; a service account reads its items but cannot mark one; a project reader sees who saw a review
//! request; only the API role may call the readers, and the rows cannot change.
use enrichedlab_agent_sandbox::broker::{
    coordinator::StartRun,
    model::{PrincipalClass, VerifiedIdentity},
    project::{
        Capability, CheckpointRequest, CreateProject, ProjectGrant, ResearchState, Visibility,
    },
    store::{Store, StoreErrorKind},
};
use serde_json::{Value, json};
use std::{env, fs};
use tokio_postgres::{Config, NoTls, config::Host, error::SqlState};
use uuid::Uuid;

fn research(steps: Value) -> ResearchState {
    serde_json::from_value(json!({"schema_version":1,"goal":"Inspect the retained input and estimate a treatment contrast.",
        "constraints":["Use the animal as the experimental unit."],"plan":steps,"lessons":[],"next_action":"Select an available tool.",
        "workspace":{"schema_version":1,"profile":"general-research","image_digest":format!("sha256:{}","a".repeat(64)),
        "source_digest":format!("sha256:{}","b".repeat(64)),"input_refs":["input://public/example"],"tool_refs":[],
        "preparation_receipt":null}})).unwrap()
}

fn items<'a>(inbox: &'a Value, kind: &str) -> Vec<&'a Value> {
    inbox["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["kind"] == kind)
        .collect()
}

#[tokio::test]
#[ignore = "isolated PostgreSQL through qualify_remote_broker.sh inbox"]
async fn each_person_sees_what_waits_for_them_and_marks_it_seen() {
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
        ('research-site','curator@example.invalid','pilot-lab','lead'),
        ('research-site','workload@example.invalid','pilot-lab','researcher');",
        )
        .await
        .unwrap();
    let person = |subject: &str, email: &str, name: Option<&str>| VerifiedIdentity {
        issuer: "https://identity.example.invalid".into(),
        class: PrincipalClass::Person,
        subject: subject.into(),
        email: email.into(),
        display_name: name.map(String::from),
    };
    let me = api
        .resolve_principal(&person(
            "owner",
            "researcher@example.invalid",
            Some("Owner One"),
        ))
        .await
        .unwrap();
    let outsider = api
        .resolve_principal(&person("outsider", "outsider@example.invalid", None))
        .await
        .unwrap();
    let curator = api
        .resolve_principal(&person(
            "curator",
            "curator@example.invalid",
            Some("Lab Lead"),
        ))
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
            title: "Waiting items".into(),
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
            capabilities: vec![Capability::Read, Capability::Review, Capability::Steer],
        },
    )
    .await
    .unwrap();
    // An empty inbox for everyone before anything waits.
    for who in [me, outsider, curator, workload] {
        assert_eq!(api.inbox(who).await.unwrap()["items"], json!([]));
    }

    // 1. A step the system could not confirm waits for whoever may steer, versioned by the revision that marked it.
    let uncertain = json!([{"action_key":"check-units","title":"Check the units","status":"uncertain","task_id":null,
        "evidence_refs":[],"rationale":"Confirm the unit."}]);
    api.checkpoint_project(
        me,
        project,
        &CheckpointRequest {
            expected_revision: 0,
            request_id: Uuid::new_v4(),
            state: research(uncertain),
        },
    )
    .await
    .unwrap();
    let inbox = api.inbox(me).await.unwrap();
    assert_eq!(inbox["can_acknowledge"], true);
    let steps = items(&inbox, "step_check");
    assert_eq!(steps.len(), 1);
    assert_eq!(
        (
            &steps[0]["ref"],
            &steps[0]["version"],
            &steps[0]["title"],
            &steps[0]["seen_at"]
        ),
        (
            &json!(format!("{project}:check-units")),
            &json!("1"),
            &json!("Check the units"),
            &Value::Null
        )
    );
    // A lab member outside the private project sees nothing of it.
    assert_eq!(api.inbox(outsider).await.unwrap()["items"], json!([]));

    // 2. A finished analysis without a person's review waits for people who may review; the agent's stop for review
    //    waits for whoever may steer or review, with the recorded reason.
    api.checkpoint_project(
        me,
        project,
        &CheckpointRequest {
            expected_revision: 1,
            request_id: Uuid::new_v4(),
            state: research(json!([])),
        },
    )
    .await
    .unwrap();
    let run = api
        .start_research_run(
            me,
            project,
            &StartRun {
                run_id: Uuid::new_v4(),
                expected_revision: 2,
                allowed_profiles: vec!["general-research".into()],
                max_turns: 4,
                max_parallel: 1,
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
    let state = research(
        json!([{"action_key":"inspect","title":"Inspect the input","status":"pending","task_id":null,
        "evidence_refs":[],"rationale":"Look first."}]),
    );
    let committed = coordinator
        .commit_research_turn(
            run,
            "lead-a",
            generation,
            2,
            &serde_json::to_value(&state).unwrap(),
            &json!([{"action_key":"inspect","profile":"general-research","objective":"Inspect.","request_sha256":"c".repeat(64)}]),
            &json!({"schema_version":1}),
            "delegate",
        )
        .await
        .unwrap();
    let task: Uuid = serde_json::from_value(committed["tasks"][0]["task_id"].clone()).unwrap();
    owner.execute("UPDATE enrichedlab_agent.broker_tasks SET status='completed', finished_at=clock_timestamp() WHERE task_id=$1", &[&task]).await.unwrap();
    owner.execute("UPDATE enrichedlab_agent.broker_research_runs SET available_at=clock_timestamp()-interval '1 second' WHERE run_id=$1", &[&run]).await.unwrap();
    let claim = coordinator
        .claim_research_run("lead-b", 90)
        .await
        .unwrap()
        .unwrap();
    let generation = claim["run"]["generation"].as_i64().unwrap();
    let revision = claim["project"]["checkpoint"]["revision"].as_i64().unwrap();
    coordinator
        .commit_research_turn(
            run,
            "lead-b",
            generation,
            revision,
            &claim["project"]["checkpoint"]["state"],
            &json!([]),
            &json!({"schema_version":1,"proposal":{"summary":"Stop here."},
                "method_review":{"decision":"needs_review","reason":"The data do not name the experimental unit."}}),
            "review_required",
        )
        .await
        .unwrap();
    let inbox = api.inbox(me).await.unwrap();
    let requests = items(&inbox, "review_request");
    assert_eq!(requests.len(), 1);
    assert_eq!(
        (
            &requests[0]["ref"],
            &requests[0]["reason_source"],
            &requests[0]["reason"]
        ),
        (
            &json!(run.to_string()),
            &json!("methods_check"),
            &json!("The data do not name the experimental unit.")
        )
    );
    let analyses = items(&inbox, "analysis_review");
    assert_eq!(analyses.len(), 1);
    assert_eq!(analyses[0]["task_id"], task.to_string());
    // Nothing of the private project reaches a lab member outside it.
    assert_eq!(api.inbox(outsider).await.unwrap()["items"], json!([]));
    // The service account sees the request and the step it may steer, never the review of an analysis, and may not mark.
    let automated = api.inbox(workload).await.unwrap();
    assert_eq!(automated["can_acknowledge"], false);
    assert_eq!(items(&automated, "review_request").len(), 1);
    assert_eq!(items(&automated, "analysis_review").len(), 0);
    let refused = api
        .inbox_acknowledge(
            workload,
            "review_request",
            &run.to_string(),
            &run.to_string(),
            Uuid::new_v4(),
        )
        .await
        .unwrap_err();
    assert_eq!(refused.kind, StoreErrorKind::Forbidden);

    // 3. Marking as seen: idempotent per request, refused for an item that is not the caller's or not current.
    let request = Uuid::new_v4();
    let seen = api
        .inbox_acknowledge(
            me,
            "review_request",
            &run.to_string(),
            &run.to_string(),
            request,
        )
        .await
        .unwrap();
    assert_eq!(
        api.inbox_acknowledge(
            me,
            "review_request",
            &run.to_string(),
            &run.to_string(),
            request
        )
        .await
        .unwrap(),
        seen
    );
    assert_eq!(
        api.inbox_acknowledge(
            me,
            "review_request",
            &run.to_string(),
            &run.to_string(),
            Uuid::new_v4()
        )
        .await
        .unwrap()["seen_at"],
        seen["seen_at"]
    );
    assert_eq!(
        api.inbox_acknowledge(
            me,
            "step_check",
            &format!("{project}:check-units"),
            "1",
            request
        )
        .await
        .unwrap_err()
        .kind,
        StoreErrorKind::Conflict
    );
    for (kind, reference, version) in [
        (
            "review_request",
            Uuid::new_v4().to_string(),
            "x".to_string(),
        ),
        ("analysis_review", task.to_string(), "stale".to_string()),
    ] {
        assert_eq!(
            api.inbox_acknowledge(me, kind, &reference, &version, Uuid::new_v4())
                .await
                .unwrap_err()
                .kind,
            StoreErrorKind::NotFound
        );
    }
    let inbox = api.inbox(me).await.unwrap();
    assert_eq!(
        items(&inbox, "review_request")[0]["seen_at"],
        seen["seen_at"]
    );
    // A project reader sees who saw the request; a lab member outside the project is refused.
    let seen_by = api
        .project_acknowledgements(workload, project)
        .await
        .unwrap();
    assert_eq!(
        (
            &seen_by[0]["run_id"],
            &seen_by[0]["principal_id"],
            &seen_by[0]["display_name"]
        ),
        (
            &json!(run.to_string()),
            &json!(me.to_string()),
            &json!("Owner One")
        )
    );
    assert_eq!(
        api.project_acknowledgements(outsider, project)
            .await
            .unwrap_err()
            .kind,
        StoreErrorKind::NotFound
    );

    // 4. A changed item reads as new: a result file added after the analysis was marked seen changes its version,
    //    and a person's review covering the current files takes it off the list.
    let before = items(&inbox, "analysis_review")[0].clone();
    api.inbox_acknowledge(
        me,
        "analysis_review",
        &task.to_string(),
        before["version"].as_str().unwrap(),
        Uuid::new_v4(),
    )
    .await
    .unwrap();
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_artifacts (tenant_id, task_id, role, label, relative_path, media_type,
               size_bytes, sha256, visibility)
             VALUES ('research-site', $1, 'result', 'Summary', 'tasks/x/summary.csv', 'text/csv', 10, $2, 'researcher')",
            &[&task, &"d".repeat(64)],
        )
        .await
        .unwrap();
    let after = api.inbox(me).await.unwrap();
    let changed = items(&after, "analysis_review")[0].clone();
    assert_ne!(changed["version"], before["version"]);
    assert_eq!(changed["seen_at"], Value::Null);
    api.record_task_review(me, task, "accepted", None, Uuid::new_v4())
        .await
        .unwrap();
    assert_eq!(
        items(&api.inbox(me).await.unwrap(), "analysis_review").len(),
        0
    );

    // 5. A lab's curators see papers waiting for acceptance and notes to renew or retire; others do not.
    let paper: Uuid = owner
        .query_one(
            "INSERT INTO enrichedlab_agent.broker_library_papers (tenant_id, group_key, document_id, source_sha256,
               byte_size, title, authors, parse, added_by, request_id)
             VALUES ('research-site', 'pilot-lab', gen_random_uuid(), $1, 100, 'A paper', '[]', '{}', $2, gen_random_uuid())
             RETURNING paper_id",
            &[&"e".repeat(64), &me],
        )
        .await
        .unwrap()
        .get(0);
    let note: Uuid = owner
        .query_one(
            "INSERT INTO enrichedlab_agent.broker_lab_notes (tenant_id, group_key, project_id, revision, kind, statement,
               statement_sha256, evidence, methods, data, shared_by, request_id, review_by, created_at)
             VALUES ('research-site', 'pilot-lab', $1, 1, 'observation', 'A shared observation.', $2,
               jsonb_build_array(jsonb_build_object('task_id', $3::text, 'outputs', '[]'::jsonb)), ARRAY['general-research'],
               ARRAY[]::text[], $4, gen_random_uuid(), clock_timestamp() - interval '1 day', clock_timestamp() - interval '40 days')
             RETURNING note_id",
            &[&project, &"f".repeat(64), &task.to_string(), &curator],
        )
        .await
        .unwrap()
        .get(0);
    let lead = api.inbox(curator).await.unwrap();
    assert_eq!(
        (
            items(&lead, "paper_acceptance")[0]["ref"].clone(),
            items(&lead, "paper_acceptance")[0]["papers"].clone()
        ),
        (json!("pilot-lab"), json!(1))
    );
    let notes = items(&lead, "note_attention");
    assert_eq!(
        (&notes[0]["ref"], &notes[0]["state"]),
        (&json!(note.to_string()), &json!("expired"))
    );
    for who in [me, outsider, workload] {
        let other = api.inbox(who).await.unwrap();
        assert_eq!(
            items(&other, "paper_acceptance").len() + items(&other, "note_attention").len(),
            0
        );
    }
    api.library_accept(curator, "pilot-lab", paper, true, None, Uuid::new_v4())
        .await
        .unwrap();
    assert_eq!(
        items(&api.inbox(curator).await.unwrap(), "paper_acceptance").len(),
        0
    );

    // 6. Only the API role may call the readers; nobody writes or changes the rows directly.
    let (client, raw) = dsn
        .replace("user=postgres", "user=broker_coordinator_qual")
        .parse::<Config>()
        .unwrap()
        .connect(NoTls)
        .await
        .unwrap();
    let raw = tokio::spawn(raw);
    for statement in [
        "SELECT enrichedlab_agent.broker_inbox('research-site', $1)",
        "SELECT enrichedlab_agent.broker_project_acknowledgements('research-site', $1, $1)",
        "SELECT count(*) FROM enrichedlab_agent.broker_inbox_acknowledgements WHERE principal_id = $1",
    ] {
        assert_eq!(
            client.query(statement, &[&me]).await.unwrap_err().code(),
            Some(&SqlState::INSUFFICIENT_PRIVILEGE),
            "{statement}"
        );
    }
    raw.abort();
    for statement in [
        "UPDATE enrichedlab_agent.broker_inbox_acknowledgements SET item_version = 'x' WHERE principal_id = $1",
        "DELETE FROM enrichedlab_agent.broker_inbox_acknowledgements WHERE principal_id = $1",
    ] {
        assert!(
            owner.execute(statement, &[&me]).await.is_err(),
            "{statement}"
        );
    }
    connection.abort();
}
