//! A person's review of an analysis (migration 023) in isolated PostgreSQL: only a
//! principal with the project's `review` capability records a decision; it binds the analysis's
//! current researcher-visible outputs; requests are idempotent; rows are immutable; the newest review
//! is current; anyone who may read the analysis reads the history. Migration 024: a workload account never records
//! one, and every review names its reviewer's kind.
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
use tokio_postgres::{Config, NoTls, config::Host};
use uuid::Uuid;

fn research() -> ResearchState {
    serde_json::from_value(json!({"schema_version":1,"goal":"Does the delivery method change tooth length?",
        "constraints":["Compute every value from the staged input."],
        "plan":[],"lessons":[],"next_action":"Run the analysis.",
        "workspace":{"schema_version":1,"profile":"general-research","image_digest":format!("sha256:{}","a".repeat(64)),
        "source_digest":format!("sha256:{}","b".repeat(64)),"input_refs":["input://public/toothgrowth"],
        "tool_refs":[],"preparation_receipt":null}})).unwrap()
}

#[tokio::test]
#[ignore = "isolated PostgreSQL through qualify_remote_broker.sh person-review"]
async fn a_named_reviewer_records_decisions_bound_to_the_outputs() {
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
    let worker = store("broker_worker_qual");
    let coordinator = store("broker_coordinator_qual");
    // The API stays out of service until migration 023 and its grants exist.
    api.ready_for_review_api().await.unwrap();
    owner
        .batch_execute(
            "INSERT INTO enrichedlab_agent.broker_principal_invitations(tenant_id,email,group_key,role) VALUES
        ('research-site','reviewer@example.invalid','pilot-lab','researcher'),
        ('research-site','reader@example.invalid','pilot-lab','researcher'),
        ('research-site','outsider@example.invalid','pilot-lab','researcher'),
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
        .resolve_principal(&person("owner", "researcher@example.invalid", None))
        .await
        .unwrap();
    let reviewer = api
        .resolve_principal(&person(
            "reviewer",
            "reviewer@example.invalid",
            Some("Reviewer One"),
        ))
        .await
        .unwrap();
    let reader = api
        .resolve_principal(&person("reader", "reader@example.invalid", None))
        .await
        .unwrap();
    let outsider = api
        .resolve_principal(&person("outsider", "outsider@example.invalid", None))
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
            title: "Review".into(),
            visibility: Visibility::Private,
        },
    )
    .await
    .unwrap();
    for (principal, capabilities) in [
        (reviewer, vec![Capability::Read, Capability::Review]),
        (reader, vec![Capability::Read]),
        (workload, vec![Capability::Read, Capability::Review]),
    ] {
        api.grant_project(
            me,
            project,
            &ProjectGrant {
                principal_id: principal,
                capabilities,
            },
        )
        .await
        .unwrap();
    }
    api.checkpoint_project(
        me,
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
            me,
            project,
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
    let claim = coordinator
        .claim_research_run("lead-a", 90)
        .await
        .unwrap()
        .unwrap();
    let generation = claim["run"]["generation"].as_i64().unwrap();
    let mut state = research();
    state.plan.push(
        serde_json::from_value(
            json!({"action_key":"means","title":"Bootstrap means","status":"pending",
        "task_id":null,"evidence_refs":[],"rationale":"Answer the question."}),
        )
        .unwrap(),
    );
    let actions = json!([{"action_key":"means","profile":"general-research","objective":"Estimate the means.","request_sha256":"c".repeat(64)}]);
    let committed = coordinator
        .commit_research_turn(
            run,
            "lead-a",
            generation,
            1,
            &serde_json::to_value(&state).unwrap(),
            &actions,
            &json!({"schema_version":1,"proposal":{"answer":"not_yet","findings":[]}}),
            "delegate",
        )
        .await
        .unwrap();
    let task: Uuid = serde_json::from_value(committed["tasks"][0]["task_id"].clone()).unwrap();
    let claimed = worker.claim_task("worker-1", 300).await.unwrap().unwrap();
    assert_eq!(claimed.id, task);
    let g = claimed.lease_generation;
    let package = "1".repeat(64);
    let path = format!("tasks/{task}/attempts/1-{g}/exports/payload");
    for (visibility, role, label, relative, sha) in [
        (
            "researcher",
            "result",
            "Package",
            path.clone(),
            package.clone(),
        ),
        (
            "operator",
            "stdout",
            "Log",
            format!("tasks/{task}/log.txt"),
            "2".repeat(64),
        ),
    ] {
        worker
            .register_artifact(
                task,
                "worker-1",
                g,
                visibility,
                role,
                label,
                &relative,
                "text/plain",
                9,
                &sha,
            )
            .await
            .unwrap();
    }
    let code = |e: enrichedlab_agent_sandbox::broker::store::StoreError| (e.kind, e.code);

    // A running analysis cannot be reviewed yet.
    assert_eq!(
        code(
            api.record_task_review(reviewer, task, "accepted", None, Uuid::new_v4())
                .await
                .unwrap_err()
        ),
        (StoreErrorKind::Conflict, "review_not_finished")
    );
    assert!(
        worker
            .finish_task(task, "worker-1", g, true, "completed", "")
            .await
            .unwrap()
    );

    // Only a reviewer records; a reader is refused and an outsider cannot see the analysis.
    assert_eq!(
        code(
            api.record_task_review(reader, task, "accepted", None, Uuid::new_v4())
                .await
                .unwrap_err()
        ),
        (StoreErrorKind::Forbidden, "review_forbidden")
    );
    assert_eq!(
        code(
            api.record_task_review(outsider, task, "accepted", None, Uuid::new_v4())
                .await
                .unwrap_err()
        ),
        (StoreErrorKind::NotFound, "task_not_found")
    );
    // A workload account holding the capability is still not a person.
    assert_eq!(
        code(
            api.record_task_review(workload, task, "accepted", None, Uuid::new_v4())
                .await
                .unwrap_err()
        ),
        (StoreErrorKind::Forbidden, "review_person_only")
    );
    // A decision other than acceptance needs a note; an unknown decision is refused.
    for (decision, note) in [
        ("needs_changes", None),
        ("rejected", Some("  ")),
        ("rejected", Some("\n\t ")),
        ("approved", None),
    ] {
        assert_eq!(
            code(
                api.record_task_review(reviewer, task, decision, note, Uuid::new_v4())
                    .await
                    .unwrap_err()
            ),
            (StoreErrorKind::Invalid, "review_invalid"),
            "{decision}"
        );
    }

    // Acceptance binds the researcher-visible outputs, by path and digest, and names the reviewer.
    let request = Uuid::new_v4();
    let accepted = api
        .record_task_review(
            reviewer,
            task,
            "accepted",
            Some(" Checked the code and table. "),
            request,
        )
        .await
        .unwrap();
    assert_eq!(accepted["decision"], "accepted");
    assert_eq!(accepted["note"], "Checked the code and table.");
    assert_eq!(accepted["reviewer_name"], "Reviewer One");
    assert_eq!(accepted["reviewer_kind"], "person");
    assert_eq!(accepted["attempt"], claimed.attempt);
    assert_eq!(
        accepted["outputs"],
        json!([{"artifact_id":accepted["outputs"][0]["artifact_id"],"relative_path":path,"sha256":package}])
    );
    assert_eq!(accepted["covers_current_outputs"], true);
    // The same request again returns the same review; the same request with another decision is refused.
    assert_eq!(
        api.record_task_review(
            reviewer,
            task,
            "accepted",
            Some("Checked the code and table."),
            request
        )
        .await
        .unwrap()["review_id"],
        accepted["review_id"]
    );
    assert_eq!(
        code(
            api.record_task_review(
                reviewer,
                task,
                "rejected",
                Some("Changed my mind."),
                request
            )
            .await
            .unwrap_err()
        ),
        (StoreErrorKind::Conflict, "idempotency_conflict")
    );

    // A later review is a new row and the newest is current; readers see the history.
    let later = api
        .record_task_review(
            me,
            task,
            "needs_changes",
            Some("Pool the doses instead."),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    let history = api.task_reviews(reader, task).await.unwrap();
    assert_eq!(history.as_array().unwrap().len(), 2);
    assert_eq!(history[0]["review_id"], later["review_id"]);
    assert_eq!(history[1]["review_id"], accepted["review_id"]);
    // The project list counts the finished analysis as reviewed by a person, with its newest decision.
    let listed = api.list_projects(reader, 50).await.unwrap();
    let row = listed
        .iter()
        .find(|p| p["project_id"] == json!(project))
        .unwrap();
    assert_eq!(
        row["task_counts"],
        json!({"total":1,"active":0,"finished":1,"failed":0,"reviewed":1,"rejected":0,"changes":1})
    );
    let newest = api.project_reviews(reader, project).await.unwrap();
    assert_eq!(newest.as_array().unwrap().len(), 1);
    assert_eq!(newest[0]["decision"], "needs_changes");
    // The project package carries every review, oldest first for each analysis.
    let snapshot = api.project_package_snapshot(reader, project).await.unwrap();
    let packaged = snapshot["reviews"].as_array().unwrap();
    assert_eq!(packaged.len(), 2);
    assert_eq!(packaged[0]["review_id"], accepted["review_id"]);
    assert_eq!(packaged[1]["review_id"], later["review_id"]);
    assert_eq!(
        code(api.task_reviews(outsider, task).await.unwrap_err()),
        (StoreErrorKind::NotFound, "task_not_found")
    );
    assert_eq!(
        code(api.project_reviews(outsider, project).await.unwrap_err()),
        (StoreErrorKind::NotFound, "project_not_found")
    );

    // Rows are immutable; outputs that change after a review leave it no longer covering them.
    assert!(
        owner
            .execute(
                "UPDATE enrichedlab_agent.broker_task_reviews SET decision='rejected'",
                &[]
            )
            .await
            .is_err()
    );
    assert!(
        owner
            .execute("DELETE FROM enrichedlab_agent.broker_task_reviews", &[])
            .await
            .is_err()
    );
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_artifacts(tenant_id,task_id,visibility,role,label,relative_path,media_type,size_bytes,sha256)
             VALUES('research-site',$1,'researcher','result','Later','tasks/later.txt','text/plain',1,$2)",
            &[&task, &"3".repeat(64)],
        )
        .await
        .unwrap();
    let after = api.task_reviews(me, task).await.unwrap();
    assert!(
        after
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["covers_current_outputs"] == false)
    );

    // A row a workload account recorded before migration 024 is read with its kind.
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_task_reviews(tenant_id,task_id,attempt,lease_generation,reviewer_id,request_id,decision,note,outputs)
             VALUES('research-site',$1,1,$2,$3,$4,'rejected','Recorded before migration 024.','[]'::jsonb)",
            &[&task, &g, &workload, &Uuid::new_v4()],
        )
        .await
        .unwrap();
    let kinds: Vec<Value> = api
        .task_reviews(reader, task)
        .await
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["reviewer_kind"].clone())
        .collect();
    assert_eq!(
        kinds,
        vec![json!("service_account"), json!("person"), json!("person")]
    );

    // The coordinator holding the run reads the newest person's review of each planned analysis
    // (migration 025): never a service account's, no reviewer identity, and a note only with a
    // decision other than acceptance. Since migration 027 it names the review by id, so a turn can
    // record which review it read.
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_research_runs SET available_at=clock_timestamp()-interval '1 second' WHERE run_id=$1",
            &[&run],
        )
        .await
        .unwrap();
    let claim = coordinator
        .claim_research_run("lead-b", 90)
        .await
        .unwrap()
        .unwrap();
    let generation_b = claim["run"]["generation"].as_i64().unwrap();
    let results = coordinator
        .research_run_results(run, "lead-b", generation_b)
        .await
        .unwrap();
    assert_eq!(
        results["reviews"],
        json!([{"task_id": task, "review_id": later["review_id"], "decision": "needs_changes",
            "note": "Pool the doses instead.", "recorded_at": later["created_at"], "covers_current_outputs": false}])
    );

    drop(owner);
    connection.abort();
}
