//! Lab memory (migration 027) in isolated PostgreSQL: a person who is a lab lead or
//! reviewer shares an observation from a lab-visible project's current saved version when every
//! analysis it rests on has a person's acceptance of its current result files; the note is bound to
//! those files and acceptances; a fixed rule decides whether it is active, paused, expired or
//! retired; lab members read it; the coordinator reads the active notes relevant to its run in a
//! fixed order; and its results read names each person's review by id.
use enrichedlab_agent_sandbox::broker::{
    coordinator::StartRun,
    model::{PrincipalClass, VerifiedIdentity},
    project::{
        Capability, CheckpointRequest, CreateProject, ProjectGrant, ResearchState, Visibility,
    },
    store::{Store, StoreError, StoreErrorKind},
};
use serde_json::{Value, json};
use std::{env, fs};
use tokio_postgres::{Config, NoTls, config::Host};
use uuid::Uuid;

const STATEMENT: &str = "Orange juice gave longer teeth than ascorbic acid at every dose.";

fn research(input: &str) -> ResearchState {
    serde_json::from_value(json!({"schema_version":1,"goal":"Does the delivery method change tooth length?",
        "constraints":["Compute every value from the staged input."],
        "plan":[],"lessons":[],"next_action":"Run the analysis.",
        "workspace":{"schema_version":1,"profile":"general-research","image_digest":format!("sha256:{}","a".repeat(64)),
        "source_digest":format!("sha256:{}","b".repeat(64)),"input_refs":[input],
        "tool_refs":[],"preparation_receipt":null}})).unwrap()
}

fn code(e: StoreError) -> (StoreErrorKind, &'static str) {
    (e.kind, e.code)
}

#[tokio::test]
#[ignore = "isolated PostgreSQL through qualify_remote_broker.sh lab-notes"]
async fn a_lab_shares_reviewed_notes_and_the_lead_reads_them_by_a_fixed_rule() {
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
    // Neither the API nor the coordinator serves until migration 027 and its grants exist.
    api.ready_for_review_api().await.unwrap();
    coordinator.ready_for_research_results().await.unwrap();
    owner
        .batch_execute(
            "INSERT INTO enrichedlab_agent.broker_principal_invitations(tenant_id,email,group_key,role) VALUES
        ('research-site','lead@example.invalid','pilot-lab','lead'),
        ('research-site','reviewer@example.invalid','pilot-lab','reviewer'),
        ('research-site','member@example.invalid','pilot-lab','researcher'),
        ('research-site','workload@example.invalid','pilot-lab','lead');",
        )
        .await
        .unwrap();
    let person = |subject: &str, email: &str, name: &str| VerifiedIdentity {
        issuer: "https://identity.example.invalid".into(),
        class: PrincipalClass::Person,
        subject: subject.into(),
        email: email.into(),
        display_name: Some(name.into()),
    };
    let me = api
        .resolve_principal(&person("owner", "researcher@example.invalid", "Owner"))
        .await
        .unwrap();
    let lead = api
        .resolve_principal(&person("lead", "lead@example.invalid", "Lab Lead"))
        .await
        .unwrap();
    let reviewer = api
        .resolve_principal(&person(
            "reviewer",
            "reviewer@example.invalid",
            "Lab Reviewer",
        ))
        .await
        .unwrap();
    let member = api
        .resolve_principal(&person("member", "member@example.invalid", "Member"))
        .await
        .unwrap();
    // A workload account in the lead role is still not a person.
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

    // A lab-visible project with one finished analysis.
    let project = Uuid::new_v4();
    api.create_project(
        me,
        &CreateProject {
            project_id: project,
            group: "pilot-lab".into(),
            title: "Delivery method".into(),
            visibility: Visibility::Lab,
        },
    )
    .await
    .unwrap();
    api.grant_project(
        me,
        project,
        &ProjectGrant {
            principal_id: reviewer,
            capabilities: vec![Capability::Read, Capability::Review],
        },
    )
    .await
    .unwrap();
    api.checkpoint_project(
        me,
        project,
        &CheckpointRequest {
            expected_revision: 0,
            request_id: Uuid::new_v4(),
            state: research("input://public/toothgrowth"),
        },
    )
    .await
    .unwrap();
    let start = |revision: i64| StartRun {
        run_id: Uuid::new_v4(),
        expected_revision: revision,
        allowed_profiles: vec!["general-research".into()],
        max_turns: 4,
        max_parallel: 1,
    };
    let run = api
        .start_research_run(me, project, &start(1))
        .await
        .unwrap();
    let claim = coordinator
        .claim_research_run("lead-a", 90)
        .await
        .unwrap()
        .unwrap();
    let generation = claim["run"]["generation"].as_i64().unwrap();
    let mut state = research("input://public/toothgrowth");
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
    let path = format!("tasks/{task}/attempts/1-{g}/exports/payload");
    let artifact = worker
        .register_artifact(
            task,
            "worker-1",
            g,
            "researcher",
            "result",
            "Package",
            &path,
            "text/plain",
            9,
            &"1".repeat(64),
        )
        .await
        .unwrap();
    assert!(
        worker
            .finish_task(task, "worker-1", g, true, "completed", "")
            .await
            .unwrap()
    );
    api.pause_research_run(me, run).await.unwrap();

    // The owner saves the analysis as finished with an observation, a hypothesis and a note that
    // cites only the starting data.
    let reference = format!("artifact://tasks/{task}/{artifact}");
    let current = |value: Value| value["checkpoint"]["revision"].as_i64().unwrap();
    let revision = current(api.get_project(me, project).await.unwrap());
    let mut saved = state.clone();
    saved.plan[0] = serde_json::from_value(json!({"action_key":"means","title":"Bootstrap means",
        "status":"completed","task_id":task,"evidence_refs":[reference],"rationale":"Answer the question."}))
    .unwrap();
    saved.lessons = serde_json::from_value(json!([
        {"statement":STATEMENT,"kind":"observation","evidence_refs":[reference,"input://public/toothgrowth"]},
        {"statement":"The effect may shrink at the highest dose.","kind":"hypothesis","evidence_refs":[reference]},
        {"statement":"The input has sixty rows.","kind":"observation","evidence_refs":["input://public/toothgrowth"]}
    ]))
    .unwrap();
    let revision = api
        .checkpoint_project(
            me,
            project,
            &CheckpointRequest {
                expected_revision: revision,
                request_id: Uuid::new_v4(),
                state: saved,
            },
        )
        .await
        .unwrap();
    let refs = json!([reference, "input://public/toothgrowth"]);
    let share = |actor: Uuid, statement: &str, refs: &Value, revision: i64, days: Option<i32>| {
        let api = api.clone();
        let (statement, refs) = (statement.to_owned(), refs.clone());
        async move {
            api.share_lab_note(
                actor,
                project,
                revision,
                &statement,
                &refs,
                Some(" Checked twice. "),
                days,
                Uuid::new_v4(),
            )
            .await
        }
    };

    // Nothing a person accepted yet.
    assert_eq!(
        code(
            share(lead, STATEMENT, &refs, revision, None)
                .await
                .unwrap_err()
        ),
        (StoreErrorKind::Conflict, "note_evidence_unaccepted")
    );
    // A service account's acceptance does not count.
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_task_reviews (tenant_id, task_id, attempt, lease_generation, reviewer_id,
               request_id, decision, outputs)
             VALUES ('research-site', $1, 1, $2, $3, gen_random_uuid(), 'accepted',
               enrichedlab_agent.broker_task_review_outputs('research-site', $1))",
            &[&task, &g, &workload],
        )
        .await
        .unwrap();
    assert_eq!(
        code(
            share(lead, STATEMENT, &refs, revision, None)
                .await
                .unwrap_err()
        ),
        (StoreErrorKind::Conflict, "note_evidence_unaccepted")
    );
    let accepted = api
        .record_task_review(reviewer, task, "accepted", None, Uuid::new_v4())
        .await
        .unwrap();

    // Who may share, and from what.
    assert_eq!(
        code(
            share(workload, STATEMENT, &refs, revision, None)
                .await
                .unwrap_err()
        ),
        (StoreErrorKind::Forbidden, "note_person_only")
    );
    assert_eq!(
        code(
            share(member, STATEMENT, &refs, revision, None)
                .await
                .unwrap_err()
        ),
        (StoreErrorKind::Forbidden, "note_forbidden")
    );
    assert_eq!(
        code(
            share(lead, STATEMENT, &refs, revision - 1, None)
                .await
                .unwrap_err()
        ),
        (StoreErrorKind::Conflict, "note_stale")
    );
    for (statement, refs, days) in [
        ("Not in the saved version.", refs.clone(), None),
        (
            "The effect may shrink at the highest dose.",
            json!([reference]),
            None,
        ),
        (STATEMENT, json!([reference]), None),
        (
            "The input has sixty rows.",
            json!(["input://public/toothgrowth"]),
            None,
        ),
        (STATEMENT, refs.clone(), Some(10)),
        (STATEMENT, refs.clone(), Some(731)),
    ] {
        assert_eq!(
            code(
                share(lead, statement, &refs, revision, days)
                    .await
                    .unwrap_err()
            ),
            (StoreErrorKind::Invalid, "note_invalid"),
            "{statement} {days:?}"
        );
    }
    let private = Uuid::new_v4();
    api.create_project(
        lead,
        &CreateProject {
            project_id: private,
            group: "pilot-lab".into(),
            title: "Private".into(),
            visibility: Visibility::Private,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        code(
            api.share_lab_note(
                lead,
                private,
                0,
                STATEMENT,
                &refs,
                None,
                None,
                Uuid::new_v4()
            )
            .await
            .unwrap_err()
        ),
        (StoreErrorKind::Conflict, "note_project_private")
    );
    // A project the sharer cannot read does not exist for them.
    assert_eq!(
        code(
            api.share_lab_note(
                reviewer,
                private,
                0,
                STATEMENT,
                &refs,
                None,
                None,
                Uuid::new_v4()
            )
            .await
            .unwrap_err()
        ),
        (StoreErrorKind::NotFound, "project_not_found")
    );

    // A lab lead shares; the note binds the result files and the acceptance.
    let request = Uuid::new_v4();
    let note = api
        .share_lab_note(
            lead,
            project,
            revision,
            STATEMENT,
            &refs,
            Some(" Checked twice. "),
            None,
            request,
        )
        .await
        .unwrap();
    let note_id: Uuid = serde_json::from_value(note["note_id"].clone()).unwrap();
    assert_eq!(note["statement"], STATEMENT);
    assert_eq!(note["reason"], "Checked twice.");
    assert_eq!(note["lab"], "pilot-lab");
    assert_eq!(note["project_title"], "Delivery method");
    assert_eq!(note["revision"], revision);
    assert_eq!(note["methods"], json!(["general-research"]));
    assert_eq!(note["data"], json!(["input://public/toothgrowth"]));
    assert_eq!(note["shared_by"]["name"], "Lab Lead");
    assert_eq!(note["shared_by"]["kind"], "person");
    assert_eq!(note["status"]["state"], "active");
    assert_eq!(
        note["evidence"],
        json!([{"task_id":task,"refs":[reference],"title":"Bootstrap means","method":"general-research",
            "outputs":accepted["outputs"],"review_id":accepted["review_id"]}])
    );
    let days: f64 = owner
        .query_one(
            "SELECT (extract(epoch FROM review_by - created_at) / 86400)::float8 FROM enrichedlab_agent.broker_lab_notes WHERE note_id = $1",
            &[&note_id],
        )
        .await
        .unwrap()
        .get::<_, f64>(0);
    assert!((179.99..180.01).contains(&days), "{days}");
    // The same request again returns the same note; the same request changed is refused.
    assert_eq!(
        api.share_lab_note(
            lead,
            project,
            revision,
            STATEMENT,
            &refs,
            Some("Checked twice."),
            None,
            request
        )
        .await
        .unwrap()["note_id"],
        note["note_id"]
    );
    assert_eq!(
        code(
            api.share_lab_note(
                lead,
                project,
                revision,
                STATEMENT,
                &refs,
                Some("Other."),
                None,
                request
            )
            .await
            .unwrap_err()
        ),
        (StoreErrorKind::Conflict, "idempotency_conflict")
    );
    // A second share of the same note is refused while the first stands.
    assert_eq!(
        code(
            share(reviewer, STATEMENT, &refs, revision, None)
                .await
                .unwrap_err()
        ),
        (StoreErrorKind::Conflict, "note_already_shared")
    );

    // Every lab member reads the lab's notes; only a lead or reviewer who is a person may curate.
    for (actor, curate) in [
        (lead, true),
        (reviewer, true),
        (member, false),
        (workload, false),
    ] {
        let listed = api.lab_notes(actor, "pilot-lab").await.unwrap();
        assert_eq!(listed["can_curate"], curate);
        assert_eq!(listed["notes"][0]["note_id"], note["note_id"]);
    }
    assert_eq!(
        code(api.lab_notes(me, "another-lab").await.unwrap_err()),
        (StoreErrorKind::NotFound, "lab_not_found")
    );

    // The coordinator reads relevant active notes for a run in another project, never its own.
    let other = Uuid::new_v4();
    api.create_project(
        me,
        &CreateProject {
            project_id: other,
            group: "pilot-lab".into(),
            title: "Another question".into(),
            visibility: Visibility::Lab,
        },
    )
    .await
    .unwrap();
    api.checkpoint_project(
        me,
        other,
        &CheckpointRequest {
            expected_revision: 0,
            request_id: Uuid::new_v4(),
            state: research("input://public/other"),
        },
    )
    .await
    .unwrap();
    let other_run = api.start_research_run(me, other, &start(1)).await.unwrap();
    let claim = coordinator
        .claim_research_run("lead-b", 90)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim["run"]["run_id"], json!(other_run));
    let g2 = claim["run"]["generation"].as_i64().unwrap();
    let read = || coordinator.research_run_lab_notes(other_run, "lead-b", g2);
    let notes = read().await.unwrap();
    assert_eq!(notes.as_array().unwrap().len(), 1);
    assert_eq!(notes[0]["note_id"], note["note_id"]);
    assert_eq!(notes[0]["same_data"], false);
    assert_eq!(notes[0]["matching_methods"], json!(["general-research"]));
    assert_eq!(notes[0]["statement_sha256"], note["statement_sha256"]);
    assert!(notes[0].get("shared_by").is_none() && notes[0].get("reason").is_none());
    let shared_at = notes[0]["shared_at"].as_str().unwrap();
    assert!(
        shared_at.len() == 27 && shared_at.ends_with('Z'),
        "{shared_at}"
    );
    // A lost lease reads nothing.
    assert_eq!(
        code(
            coordinator
                .research_run_lab_notes(other_run, "lead-b", g2 + 1)
                .await
                .unwrap_err()
        ),
        (StoreErrorKind::LeaseLost, "lease_fence_lost")
    );

    // A newer person's review that is not an acceptance pauses the note; a new acceptance restores it.
    api.record_task_review(
        reviewer,
        task,
        "needs_changes",
        Some("Split by dose."),
        Uuid::new_v4(),
    )
    .await
    .unwrap();
    let state_of = |actor: Uuid| {
        let api = api.clone();
        async move { api.lab_notes(actor, "pilot-lab").await.unwrap()["notes"][0]["status"].clone() }
    };
    assert_eq!(
        state_of(member).await,
        json!({"state":"paused","cause":"evidence_review_changed","task_id":task,"review_by":note["status"]["review_by"]})
    );
    assert_eq!(read().await.unwrap(), json!([]));
    api.record_task_review(reviewer, task, "accepted", None, Uuid::new_v4())
        .await
        .unwrap();
    assert_eq!(state_of(member).await["state"], "active");
    assert_eq!(read().await.unwrap().as_array().unwrap().len(), 1);

    // Retiring and renewing: people only, with the rules for who may.
    assert_eq!(
        code(
            api.retire_lab_note(workload, note_id, "Superseded.", Uuid::new_v4())
                .await
                .unwrap_err()
        ),
        (StoreErrorKind::Forbidden, "note_person_only")
    );
    assert_eq!(
        code(
            api.retire_lab_note(member, note_id, "Superseded.", Uuid::new_v4())
                .await
                .unwrap_err()
        ),
        (StoreErrorKind::Forbidden, "note_forbidden")
    );
    assert_eq!(
        code(
            api.retire_lab_note(lead, note_id, " \n ", Uuid::new_v4())
                .await
                .unwrap_err()
        ),
        (StoreErrorKind::Invalid, "note_invalid")
    );
    assert_eq!(
        code(
            api.renew_lab_note(member, note_id, 90, Uuid::new_v4())
                .await
                .unwrap_err()
        ),
        (StoreErrorKind::Forbidden, "note_forbidden")
    );
    assert_eq!(
        code(
            api.renew_lab_note(reviewer, note_id, 20, Uuid::new_v4())
                .await
                .unwrap_err()
        ),
        (StoreErrorKind::Invalid, "note_invalid")
    );
    let renewed = api
        .renew_lab_note(reviewer, note_id, 365, Uuid::new_v4())
        .await
        .unwrap();
    assert_eq!(renewed["events"][0]["kind"], "renewed");
    assert_eq!(renewed["events"][0]["actor_name"], "Lab Reviewer");
    assert_ne!(renewed["status"]["review_by"], note["status"]["review_by"]);

    // A review-by date in the past expires the note; a later renewal brings it back.
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_lab_note_events (tenant_id, note_id, kind, actor_id, request_id, review_by)
             VALUES ('research-site', $1, 'renewed', $2, gen_random_uuid(), statement_timestamp() - interval '1 day')",
            &[&note_id, &lead],
        )
        .await
        .unwrap();
    assert_eq!(state_of(member).await["state"], "expired");
    assert_eq!(read().await.unwrap(), json!([]));
    api.renew_lab_note(lead, note_id, 30, Uuid::new_v4())
        .await
        .unwrap();
    assert_eq!(state_of(member).await["state"], "active");

    // Changed result files pause it.
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_artifacts (tenant_id, artifact_id, task_id, visibility, role, label,
               relative_path, media_type, size_bytes, sha256)
             SELECT tenant_id, gen_random_uuid(), task_id, 'researcher', 'result', 'Later', relative_path || '-later',
               media_type, size_bytes, repeat('3', 64)
             FROM enrichedlab_agent.broker_artifacts WHERE task_id = $1 AND visibility = 'researcher'",
            &[&task],
        )
        .await
        .unwrap();
    assert_eq!(state_of(member).await["cause"], "evidence_files_changed");
    assert_eq!(read().await.unwrap(), json!([]));

    // The sharer retires it; it stays retired and may then be shared again.
    let retire = Uuid::new_v4();
    let retired = api
        .retire_lab_note(lead, note_id, " Superseded by the dose analysis. ", retire)
        .await
        .unwrap();
    assert_eq!(retired["status"]["state"], "retired");
    assert_eq!(
        retired["status"]["reason"],
        "Superseded by the dose analysis."
    );
    assert_eq!(
        api.retire_lab_note(lead, note_id, "Superseded by the dose analysis.", retire)
            .await
            .unwrap()["status"]["state"],
        "retired"
    );
    assert_eq!(
        code(
            api.retire_lab_note(reviewer, note_id, "Again.", Uuid::new_v4())
                .await
                .unwrap_err()
        ),
        (StoreErrorKind::Conflict, "note_retired")
    );
    assert_eq!(
        code(
            api.renew_lab_note(lead, note_id, 90, Uuid::new_v4())
                .await
                .unwrap_err()
        ),
        (StoreErrorKind::Conflict, "note_retired")
    );

    // Records are immutable, and the origin project's audit names each change.
    for statement in [
        "UPDATE enrichedlab_agent.broker_lab_notes SET statement = 'x'",
        "DELETE FROM enrichedlab_agent.broker_lab_note_events",
    ] {
        assert!(owner.execute(statement, &[]).await.is_err(), "{statement}");
    }
    let kinds: Vec<String> = owner
        .query(
            "SELECT kind FROM enrichedlab_agent.broker_project_audit WHERE project_id = $1 AND kind LIKE 'note_%'
             ORDER BY occurred_at, event_id",
            &[&project],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    assert_eq!(
        kinds,
        [
            "note_shared",
            "note_renewed",
            "note_renewed",
            "note_retired"
        ]
    );

    // The coordinator's results read names the newest person's review of each analysis by id.
    let project_run = api
        .start_research_run(me, project, &start(revision))
        .await
        .unwrap();
    let claim = coordinator
        .claim_research_run("lead-c", 90)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim["run"]["run_id"], json!(project_run));
    let g3 = claim["run"]["generation"].as_i64().unwrap();
    let results = coordinator
        .research_run_results(project_run, "lead-c", g3)
        .await
        .unwrap();
    let newest: Uuid = owner
        .query_one(
            "SELECT review_id FROM enrichedlab_agent.broker_task_reviews WHERE task_id = $1 ORDER BY created_at DESC, review_id DESC LIMIT 1",
            &[&task],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(results["reviews"][0]["review_id"], json!(newest));

    drop(owner);
    connection.abort();
}
