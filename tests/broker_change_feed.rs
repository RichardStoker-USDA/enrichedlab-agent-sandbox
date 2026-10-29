//! The change feed (migration 061) in isolated PostgreSQL: a change row in the transaction of each
//! change a view shows and none for a lease renewal, a repeated look, a sign-in rewrite or a working copy observation;
//! each reader's keys under that reader's access rule; `access_changed` for the person whose access changed and for
//! a lab whose methods or project visibility changed; delivery in commit order, holding back what commits behind a
//! running transaction; one notification per transaction; `behind` for a pruned, unknown or overlong position; whether
//! work runs in each named project; each lab's dataset signal; and the readers granted to the API role only. Every
//! value is synthetic.
use enrichedlab_agent_sandbox::broker::{
    coordinator::StartRun,
    model::{PrincipalClass, SubmitTaskRequest, VerifiedIdentity},
    project::{CheckpointRequest, CreateProject, ResearchState, Visibility},
    store::{ChangeCursor, ChangeRead, Store, StoreErrorKind},
};
use serde_json::json;
use std::{env, fs, time::Duration};
use tokio_postgres::{Client, Config, NoTls, config::Host, error::SqlState};
use uuid::Uuid;

fn research() -> ResearchState {
    serde_json::from_value(json!({"schema_version":1,"goal":"Compare tooth length by supplement.","constraints":[],
        "plan":[],"lessons":[],"next_action":"Select an available tool.",
        "workspace":{"schema_version":1,"profile":"general-research","image_digest":format!("sha256:{}","a".repeat(64)),
        "source_digest":format!("sha256:{}","b".repeat(64)),"input_refs":["input://public/example"],
        "tool_refs":["tool://reviewed/query"],"preparation_receipt":null}}))
    .unwrap()
}

async fn rows(owner: &Client) -> i64 {
    owner
        .query_one(
            "SELECT count(*) FROM enrichedlab_agent.broker_changes WHERE tenant_id = 'research-site'",
            &[],
        )
        .await
        .unwrap()
        .get(0)
}

/// Reads from `from` and moves it to the new position.
async fn read(store: &Store, who: Uuid, from: &mut ChangeCursor) -> ChangeRead {
    let read = store.read_changes(who, Some(from), 2_000).await.unwrap();
    *from = read.position.clone();
    read
}

fn has(read: &ChangeRead, key: &str) -> bool {
    read.keys.iter().any(|k| k == key)
}

#[tokio::test]
#[ignore = "isolated PostgreSQL through qualify_remote_broker.sh change-feed"]
async fn each_reader_learns_what_changed_for_them_in_commit_order() {
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
            4,
        )
        .unwrap()
    };
    let api = store("broker_api_qual");
    let worker = store("broker_worker_qual");
    let coordinator = store("broker_coordinator_qual");
    api.ready_for_change_feed().await.unwrap();
    api.ready_for_review_api().await.unwrap();
    coordinator.ready_for_research_results().await.unwrap();
    // Only the API role reads the feed.
    assert_eq!(
        worker.ready_for_change_feed().await.unwrap_err().code,
        "broker_schema_not_ready"
    );

    owner
        .batch_execute(
            "INSERT INTO enrichedlab_agent.broker_research_groups(tenant_id,group_key,name,max_active_agents,
               max_active_agents_per_principal,max_queued_tasks,retention_days)
             VALUES ('research-site','other-lab','Other lab',3,2,20,30);
             INSERT INTO enrichedlab_agent.broker_principal_invitations(tenant_id,email,group_key,role) VALUES
               ('research-site','labmate@example.invalid','pilot-lab','researcher'),
               ('research-site','outsider@example.invalid','other-lab','researcher');",
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
    let labmate = api
        .resolve_principal(&person("labmate", "labmate@example.invalid"))
        .await
        .unwrap();
    let outsider = api
        .resolve_principal(&person("outsider", "outsider@example.invalid"))
        .await
        .unwrap();

    // A first read replays nothing and names the newest position.
    let first = api.read_changes(me, None, 2_000).await.unwrap();
    assert!(first.keys.is_empty() && !first.behind && !first.access_changed);
    let (mut at_me, mut at_mate, mut at_out) = (
        first.position.clone(),
        first.position.clone(),
        first.position.clone(),
    );

    // 1. A lab project reaches its lab, a private one its owner; a reader outside both moves past them.
    let shared = Uuid::new_v4();
    let private = Uuid::new_v4();
    for (id, visibility) in [(shared, Visibility::Lab), (private, Visibility::Private)] {
        api.create_project(
            me,
            &CreateProject {
                project_id: id,
                group: "pilot-lab".into(),
                title: "Tooth growth".into(),
                visibility,
            },
        )
        .await
        .unwrap();
    }
    let mine = read(&api, me, &mut at_me).await;
    assert!(
        has(&mine, "projects")
            && has(&mine, &format!("project:{shared}"))
            && has(&mine, &format!("project:{private}"))
    );
    assert!(!mine.access_changed && !mine.behind);
    let theirs = read(&api, labmate, &mut at_mate).await;
    assert!(
        has(&theirs, &format!("project:{shared}")) && !has(&theirs, &format!("project:{private}"))
    );
    let outside = read(&api, outsider, &mut at_out).await;
    assert!(outside.keys.is_empty());
    assert_eq!(outside.position, mine.position);

    // 2. A saved version names the project and the inbox.
    api.checkpoint_project(
        me,
        shared,
        &CheckpointRequest {
            expected_revision: 0,
            request_id: Uuid::new_v4(),
            state: research(),
        },
    )
    .await
    .unwrap();
    let saved = read(&api, labmate, &mut at_mate).await;
    assert!(has(&saved, &format!("project:{shared}")) && has(&saved, "inbox"));
    read(&api, me, &mut at_me).await;
    read(&api, outsider, &mut at_out).await;

    // 3. A task outside any project reaches its owner only; a lease renewal adds no row.
    let task = api
        .submit_task(
            me,
            "pilot-lab",
            "general-research",
            "Summarize the example input.",
            "direct-1",
            &"c".repeat(64),
        )
        .await
        .unwrap();
    let direct = read(&api, me, &mut at_me).await;
    assert!(
        has(&direct, "tasks") && has(&direct, &format!("task:{task}")) && has(&direct, "inbox")
    );
    assert!(read(&api, labmate, &mut at_mate).await.keys.is_empty());
    let claimed = worker.claim_task("worker-a", 60).await.unwrap().unwrap();
    assert!(has(
        &read(&api, me, &mut at_me).await,
        &format!("task:{task}")
    ));
    let before = rows(&owner).await;
    assert!(
        worker
            .renew_task(task, "worker-a", claimed.lease_generation, 60)
            .await
            .unwrap()
    );
    assert_eq!(
        rows(&owner).await,
        before,
        "a lease renewal is not a change"
    );

    // 4. A project's task names its source analysis too.
    let source = api
        .submit_project_task(
            me,
            &SubmitTaskRequest {
                group: "pilot-lab".into(),
                profile: "general-research".into(),
                objective: "Mean tooth length by supplement.".into(),
                project_id: Some(shared),
                action_key: Some("mean-length".into()),
                parent_task_id: None,
            },
            "project-1",
            &"d".repeat(64),
        )
        .await
        .unwrap();
    let version = api
        .submit_project_task(
            me,
            &SubmitTaskRequest {
                group: "pilot-lab".into(),
                profile: "general-research".into(),
                objective: "The same by dose.".into(),
                project_id: Some(shared),
                action_key: Some("mean-length-dose".into()),
                parent_task_id: Some(source),
            },
            "project-2",
            &"e".repeat(64),
        )
        .await
        .unwrap();
    let linked = read(&api, labmate, &mut at_mate).await;
    for key in [
        format!("task:{source}"),
        format!("task:{version}"),
        format!("project:{shared}"),
    ] {
        assert!(has(&linked, &key), "{key} in {:?}", linked.keys);
    }
    // Whether work runs, for each named project the reader may read.
    let busy = api
        .projects_busy(labmate, &[shared, private])
        .await
        .unwrap();
    assert_eq!(busy, json!({ shared.to_string(): true }));
    assert_eq!(
        api.projects_busy(me, &[private]).await.unwrap(),
        json!({ private.to_string(): false })
    );
    read(&api, me, &mut at_me).await;
    read(&api, outsider, &mut at_out).await;

    // 5. A look at a newer revision reaches only the person who looked; the same look again adds no row.
    api.project_look(me, shared, 1).await.unwrap();
    assert!(has(&read(&api, me, &mut at_me).await, "inbox"));
    assert!(read(&api, labmate, &mut at_mate).await.keys.is_empty());
    let before = rows(&owner).await;
    api.project_look(me, shared, 1).await.unwrap();
    assert_eq!(
        rows(&owner).await,
        before,
        "a repeated look is not a change"
    );

    // A look names its project only while the looker may read it: an inspection that ended writes no row, so the rule
    // is checked when the feed is read. The labmate cannot read the private project; its owner can.
    let look = "SELECT enrichedlab_agent.broker_add_change('research-site', 'look', $1::uuid, NULL::uuid, NULL::uuid, \
                NULL::text, $2::uuid, true, NULL::text[])";
    owner.execute(look, &[&private, &labmate]).await.unwrap();
    assert!(
        read(&api, labmate, &mut at_mate).await.keys.is_empty(),
        "a look at a project the looker cannot read names nothing"
    );
    owner.execute(look, &[&private, &me]).await.unwrap();
    assert!(has(
        &read(&api, me, &mut at_me).await,
        &format!("project:{private}")
    ));

    // 5b. A shared note's state follows the analyses it cites, so a change to one of them tells the note's lab, and
    // a reader in the lab who may not read the project still learns that the lab's notes changed.
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_lab_notes(tenant_id, group_key, project_id, revision, kind, statement,
               statement_sha256, evidence, methods, data, shared_by, request_id, review_by)
             VALUES ('research-site', 'pilot-lab', $1, 1, 'observation', 'Supplement changes tooth length.', $2,
               jsonb_build_array(jsonb_build_object('task_id', $3::text, 'outputs', '[]'::jsonb)),
               ARRAY['general-research'], ARRAY[]::text[], $4, gen_random_uuid(), clock_timestamp() + interval '30 days')",
            &[&shared, &"f".repeat(64), &source.to_string(), &me],
        )
        .await
        .unwrap();
    assert!(has(
        &read(&api, labmate, &mut at_mate).await,
        "lab:pilot-lab"
    ));
    api.cancel_task(me, source).await.unwrap();
    let cited = read(&api, labmate, &mut at_mate).await;
    assert!(
        has(&cited, "lab:pilot-lab") && has(&cited, &format!("task:{source}")),
        "{:?}",
        cited.keys
    );
    assert!(
        !has(&read(&api, outsider, &mut at_out).await, "lab:pilot-lab"),
        "another lab learns nothing"
    );
    read(&api, me, &mut at_me).await;
    // A note's row names its project too, but that key reaches only the project's readers: a labmate learns that the
    // lab's notes changed, not which private project the note came from.
    api.checkpoint_project(
        me,
        private,
        &CheckpointRequest {
            expected_revision: 0,
            request_id: Uuid::new_v4(),
            state: research(),
        },
    )
    .await
    .unwrap();
    read(&api, me, &mut at_me).await;
    read(&api, labmate, &mut at_mate).await;
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_lab_notes(tenant_id, group_key, project_id, revision, kind, statement,
               statement_sha256, evidence, methods, data, shared_by, request_id, review_by)
             VALUES ('research-site', 'pilot-lab', $1, 1, 'observation', 'Dose changes tooth length.', $2,
               jsonb_build_array(jsonb_build_object('task_id', $3::text, 'outputs', '[]'::jsonb)),
               ARRAY['general-research'], ARRAY[]::text[], $4, gen_random_uuid(), clock_timestamp() + interval '30 days')",
            &[&private, &"a".repeat(64), &task.to_string(), &me],
        )
        .await
        .unwrap();
    let noted = read(&api, labmate, &mut at_mate).await;
    assert!(
        has(&noted, "lab:pilot-lab") && !has(&noted, &format!("project:{private}")),
        "{:?}",
        noted.keys
    );
    assert!(has(
        &read(&api, me, &mut at_me).await,
        &format!("project:{private}")
    ));
    read(&api, outsider, &mut at_out).await;

    // 5c. Access that ends without a grant row: an inspection begun or ended, and a project's owner or lab moving,
    // tell the people affected to read everything again.
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_project_administrators(tenant_id, principal_id)
             VALUES ('research-site', $1)",
            &[&labmate],
        )
        .await
        .unwrap();
    assert!(read(&api, labmate, &mut at_mate).await.access_changed);
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_project_inspections(tenant_id, project_id, principal_id, expires_at)
             VALUES ('research-site', $1, $2, clock_timestamp() + interval '1 hour')",
            &[&private, &labmate],
        )
        .await
        .unwrap();
    assert!(read(&api, labmate, &mut at_mate).await.access_changed);
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_project_inspections SET expires_at = clock_timestamp()
             WHERE project_id = $1 AND principal_id = $2",
            &[&private, &labmate],
        )
        .await
        .unwrap();
    assert!(read(&api, labmate, &mut at_mate).await.access_changed);
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_project_inspections(tenant_id, project_id, principal_id, expires_at)
             VALUES ('research-site', $1, $2, clock_timestamp() + interval '1 hour')
             ON CONFLICT (tenant_id, project_id, principal_id) DO UPDATE SET expires_at = EXCLUDED.expires_at",
            &[&private, &labmate],
        )
        .await
        .unwrap();
    read(&api, labmate, &mut at_mate).await;
    owner
        .execute(
            "DELETE FROM enrichedlab_agent.broker_project_inspections WHERE project_id = $1 AND principal_id = $2",
            &[&private, &labmate],
        )
        .await
        .unwrap();
    assert!(
        read(&api, labmate, &mut at_mate).await.access_changed,
        "an inspection removed"
    );
    read(&api, me, &mut at_me).await;
    read(&api, outsider, &mut at_out).await;
    // The outsider is in no lab of the project, so only the owner rows can reach them: as its new owner, then as the
    // owner it left.
    let owner_to =
        "UPDATE enrichedlab_agent.broker_projects SET owner_id = $2 WHERE project_id = $1";
    owner
        .execute(owner_to, &[&private, &outsider])
        .await
        .unwrap();
    assert!(
        read(&api, outsider, &mut at_out).await.access_changed,
        "a new owner reads everything again"
    );
    assert!(read(&api, me, &mut at_me).await.access_changed);
    assert!(read(&api, labmate, &mut at_mate).await.access_changed);
    owner.execute(owner_to, &[&private, &me]).await.unwrap();
    assert!(
        read(&api, outsider, &mut at_out).await.access_changed,
        "the owner a project left reads everything again"
    );
    read(&api, me, &mut at_me).await;
    read(&api, labmate, &mut at_mate).await;

    // 6. A sign-in rewrite of a membership is not a change; a new role is, and tells that person to read again.
    let before = rows(&owner).await;
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_group_memberships SET updated_at = clock_timestamp()
             WHERE tenant_id = 'research-site' AND principal_id = $1",
            &[&labmate],
        )
        .await
        .unwrap();
    assert_eq!(rows(&owner).await, before);
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_group_memberships SET role = 'reviewer'
             WHERE tenant_id = 'research-site' AND principal_id = $1",
            &[&labmate],
        )
        .await
        .unwrap();
    assert!(read(&api, labmate, &mut at_mate).await.access_changed);
    let lab = read(&api, me, &mut at_me).await;
    assert!(!lab.access_changed && has(&lab, "lab:pilot-lab"));
    assert!(read(&api, outsider, &mut at_out).await.keys.is_empty());

    // 7. A lab's methods: one enabled dataset method gives it lab data, and every member reads again.
    let lab_data = |groups: &[enrichedlab_agent_sandbox::broker::model::GroupView]| {
        groups
            .iter()
            .find(|g| g.key == "pilot-lab")
            .unwrap()
            .lab_data
    };
    assert!(!lab_data(&api.me(me).await.unwrap().groups));
    owner
        .batch_execute(
            "INSERT INTO enrichedlab_agent.broker_agent_profiles(tenant_id,profile_key,name,description,harness,thinking,
               route,controller_profile,gateway_profile,image_ref,cpu_limit,memory_limit,state_profile,role,tool_bundle,
               max_run_seconds,max_task_attempts,max_provider_attempts)
             SELECT tenant_id,k,'Read a lab dataset',description,harness,thinking,'server-dataset',controller_profile,
               gateway_profile,image_ref,cpu_limit,memory_limit,state_profile,role,tool_bundle,max_run_seconds,1,
               max_provider_attempts
             FROM enrichedlab_agent.broker_agent_profiles, (VALUES ('lab-dataset'),('lab-dataset-b')) v(k)
             WHERE tenant_id='research-site' AND profile_key='general-research';
             INSERT INTO enrichedlab_agent.broker_group_profile_grants(tenant_id,group_key,profile_key)
               VALUES ('research-site','pilot-lab','lab-dataset');",
        )
        .await
        .unwrap();
    assert!(lab_data(&api.me(me).await.unwrap().groups));
    assert!(read(&api, me, &mut at_me).await.access_changed);
    assert!(read(&api, labmate, &mut at_mate).await.access_changed);
    assert!(!read(&api, outsider, &mut at_out).await.access_changed);
    owner
        .batch_execute(
            "INSERT INTO enrichedlab_agent.broker_group_profile_grants(tenant_id,group_key,profile_key)
               VALUES ('research-site','pilot-lab','lab-dataset-b');",
        )
        .await
        .unwrap();
    assert!(
        !lab_data(&api.me(me).await.unwrap().groups),
        "two dataset methods are not one"
    );
    owner
        .batch_execute(
            "UPDATE enrichedlab_agent.broker_agent_profiles SET enabled = false
             WHERE tenant_id='research-site' AND profile_key='lab-dataset-b';",
        )
        .await
        .unwrap();
    assert!(
        lab_data(&api.me(me).await.unwrap().groups),
        "a disabled method does not count"
    );
    read(&api, me, &mut at_me).await;
    read(&api, labmate, &mut at_mate).await;
    read(&api, outsider, &mut at_out).await;

    // 8. A project made private: its lab reads everything again, whoever may still read it.
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_projects SET visibility = 'private' WHERE project_id = $1",
            &[&shared],
        )
        .await
        .unwrap();
    let hidden = read(&api, labmate, &mut at_mate).await;
    assert!(hidden.access_changed && !has(&hidden, &format!("project:{shared}")));
    assert!(read(&api, me, &mut at_me).await.access_changed);
    assert!(!read(&api, outsider, &mut at_out).await.access_changed);

    // 9. A research run: a new run and a new state are changes; a claim and hand-back within one turn while tools
    // work, and a lease renewal, are not.
    let run = Uuid::new_v4();
    let revision: i64 = owner
        .query_one(
            "SELECT revision FROM enrichedlab_agent.broker_projects WHERE project_id = $1",
            &[&shared],
        )
        .await
        .unwrap()
        .get(0);
    coordinator_start(&api, me, shared, run, revision).await;
    assert!(has(
        &read(&api, me, &mut at_me).await,
        &format!("project:{shared}")
    ));
    let claim = coordinator
        .claim_research_run("lead-a", 90)
        .await
        .unwrap()
        .unwrap();
    let generation = claim["run"]["generation"].as_i64().unwrap();
    assert!(has(
        &read(&api, me, &mut at_me).await,
        &format!("project:{shared}")
    ));
    let before = rows(&owner).await;
    assert!(
        coordinator
            .renew_research_run(run, "lead-a", generation, 90)
            .await
            .unwrap()
    );
    assert_eq!(
        rows(&owner).await,
        before,
        "a run's lease renewal is not a change"
    );
    coordinator
        .yield_research_run(run, "lead-a", generation, "waiting_for_tools", None)
        .await
        .unwrap();
    let waiting = rows(&owner).await;
    assert!(waiting > before, "the first wait is a change");
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_research_runs SET available_at = clock_timestamp() WHERE run_id = $1",
            &[&run],
        )
        .await
        .unwrap();
    let claim = coordinator
        .claim_research_run("lead-a", 90)
        .await
        .unwrap()
        .unwrap();
    let generation = claim["run"]["generation"].as_i64().unwrap();
    coordinator
        .yield_research_run(run, "lead-a", generation, "waiting_for_tools", None)
        .await
        .unwrap();
    assert_eq!(
        rows(&owner).await,
        waiting,
        "claiming and waiting again in one turn is not a change"
    );
    read(&api, me, &mut at_me).await;
    read(&api, labmate, &mut at_mate).await;

    // 10. A working copy kept and cleared are changes; the custody dispatcher's observation is not.
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_task_working_copies(tenant_id,volume,task_id,role,retention_until,observed_at)
             VALUES ('research-site','artifacts-run-qual-1',$1,'durable-artifacts',clock_timestamp()+interval '1 day',clock_timestamp())",
            &[&task],
        )
        .await
        .unwrap();
    assert!(has(
        &read(&api, me, &mut at_me).await,
        &format!("task:{task}")
    ));
    let before = rows(&owner).await;
    owner
        .batch_execute(
            "UPDATE enrichedlab_agent.broker_task_working_copies SET observed_at = clock_timestamp()
             WHERE volume = 'artifacts-run-qual-1'",
        )
        .await
        .unwrap();
    assert_eq!(rows(&owner).await, before);

    // 11. Commit order: a change that commits after a later transaction's is still delivered, because nothing past a
    // running transaction is delivered until it ends.
    let (first_writer, first_connection) = config.connect(NoTls).await.unwrap();
    let first_connection = tokio::spawn(first_connection);
    let (second_writer, second_connection) = config.connect(NoTls).await.unwrap();
    let second_connection = tokio::spawn(second_connection);
    first_writer.batch_execute("BEGIN").await.unwrap();
    first_writer
        .execute(
            "UPDATE enrichedlab_agent.broker_projects SET title = 'First writer' WHERE project_id = $1",
            &[&private],
        )
        .await
        .unwrap();
    second_writer
        .execute(
            "UPDATE enrichedlab_agent.broker_projects SET title = 'Second writer' WHERE project_id = $1",
            &[&shared],
        )
        .await
        .unwrap();
    let held = read(&api, me, &mut at_me).await;
    assert!(
        held.keys.is_empty(),
        "a row behind a running transaction waits: {:?}",
        held.keys
    );
    // The second writer's row is kept but not yet given out: a position naming it would skip the first writer's
    // row when that commits, so it reads as behind, at a position below the running transaction.
    let ahead = owner
        .query_one(
            "SELECT 'x' || txid::text || '-' || change_id::text FROM enrichedlab_agent.broker_changes
             WHERE project_id = $1 ORDER BY change_id DESC LIMIT 1",
            &[&shared],
        )
        .await
        .unwrap()
        .get::<_, String>(0);
    let early = api
        .read_changes(me, Some(&ChangeCursor::parse(&ahead).unwrap()), 2_000)
        .await
        .unwrap();
    assert!(early.behind && early.position != ChangeCursor::parse(&ahead).unwrap());
    first_writer.batch_execute("COMMIT").await.unwrap();
    let both = read(&api, me, &mut at_me).await;
    assert!(has(&both, &format!("project:{shared}")) && has(&both, &format!("project:{private}")));

    // 12. One notification per transaction, whatever it changed, carrying only the tenant.
    let mut listener = api.listen_for_changes().await.unwrap();
    second_writer.batch_execute("BEGIN").await.unwrap();
    // Two projects, so the transaction writes distinct rows and each notifies.
    for (project, title) in [(shared, "One"), (private, "Two"), (shared, "Three")] {
        second_writer
            .execute(
                "UPDATE enrichedlab_agent.broker_projects SET title = $2 WHERE project_id = $1",
                &[&project, &title],
            )
            .await
            .unwrap();
    }
    second_writer.batch_execute("COMMIT").await.unwrap();
    let note = tokio::time::timeout(Duration::from_secs(5), listener.notifications.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(note, "research-site");
    assert!(
        tokio::time::timeout(Duration::from_millis(500), listener.notifications.recv())
            .await
            .is_err(),
        "one transaction, one notification"
    );
    read(&api, me, &mut at_me).await;

    // 13. Behind: a position past every transaction, a read with more rows than its limit, and a position older than
    // the pruned rows.
    let future = ChangeCursor {
        txid: at_me.txid + 1_000_000,
        id: 1,
    };
    assert!(
        api.read_changes(me, Some(&future), 2_000)
            .await
            .unwrap()
            .behind
    );
    let start = at_me.clone();
    for title in ["Four", "Five"] {
        owner
            .execute(
                "UPDATE enrichedlab_agent.broker_projects SET title = $2 WHERE project_id = $1",
                &[&shared, &title],
            )
            .await
            .unwrap();
    }
    let crowded = api.read_changes(me, Some(&start), 1).await.unwrap();
    assert!(crowded.behind && crowded.keys.is_empty());
    let newest = api.read_changes(me, Some(&start), 2_000).await.unwrap();
    assert_eq!(crowded.position, newest.position);
    // A well-formed position the feed never gave out (a restored database, an edited address) is behind too.
    let unknown = ChangeCursor {
        txid: newest.position.txid,
        id: newest.position.id + 1_000_000,
    };
    let refused = api.read_changes(me, Some(&unknown), 2_000).await.unwrap();
    assert!(refused.behind && refused.position == newest.position);
    // Pruning removes rows older than a day and keeps a younger one, even one below an older row in feed order.
    owner
        .batch_execute(
            "UPDATE enrichedlab_agent.broker_changes SET at = at - interval '2 days' WHERE tenant_id = 'research-site'
               AND change_id <> (SELECT min(change_id) FROM enrichedlab_agent.broker_changes)",
        )
        .await
        .unwrap();
    assert!(api.prune_changes().await.unwrap() > 0);
    assert_eq!(rows(&owner).await, 1, "the young row stays");
    owner
        .batch_execute(
            "UPDATE enrichedlab_agent.broker_changes SET at = at - interval '2 days' WHERE tenant_id = 'research-site'",
        )
        .await
        .unwrap();
    assert_eq!(api.prune_changes().await.unwrap(), 1);
    assert_eq!(rows(&owner).await, 0);
    assert!(
        api.read_changes(me, Some(&first.position), 2_000)
            .await
            .unwrap()
            .behind
    );
    let current = api
        .read_changes(me, Some(&newest.position), 2_000)
        .await
        .unwrap();
    assert!(!current.behind && current.keys.is_empty());
    // With every row pruned the horizon is the newest position: a first read starts there, the empty start is
    // behind and moves there, and a reader there is not sent back again.
    let fresh = api.read_changes(me, None, 2_000).await.unwrap();
    assert_eq!(fresh.position, newest.position);
    let empty = ChangeCursor { txid: 0, id: 0 };
    let moved = api.read_changes(me, Some(&empty), 2_000).await.unwrap();
    assert!(moved.behind && moved.position == newest.position);
    let settled = api
        .read_changes(me, Some(&moved.position), 2_000)
        .await
        .unwrap();
    assert!(!settled.behind && settled.position == newest.position);

    // A file registered on its own (a recovery) and a project's audit (an export among it) are changes too.
    let mut at_direct = api.read_changes(me, None, 2_000).await.unwrap().position;
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_artifacts(tenant_id, task_id, role, label, relative_path, media_type,
               size_bytes, sha256) VALUES ('research-site', $1, 'result', 'Recovered table', 'results/recovered.csv',
               'text/csv', 1, $2)",
            &[&task, &"b".repeat(64)],
        )
        .await
        .unwrap();
    assert!(has(
        &read(&api, me, &mut at_direct).await,
        &format!("task:{task}")
    ));
    // The shared project is private since step 8, so its owner is the reader here.
    let mut at_audit = api.read_changes(me, None, 2_000).await.unwrap().position;
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_project_audit(tenant_id, project_id, actor_id, kind, detail)
             VALUES ('research-site', $1, $2, 'project_exported', '{}'::jsonb)",
            &[&shared, &me],
        )
        .await
        .unwrap();
    assert!(has(
        &read(&api, me, &mut at_audit).await,
        &format!("project:{shared}")
    ));

    // 14. A method edited by the catalog seed tells each lab granted it; a rerun that changes only the time does not.
    // A lab disabled tells its members, though they no longer read it.
    let before = rows(&owner).await;
    owner
        .batch_execute(
            "UPDATE enrichedlab_agent.broker_agent_profiles SET updated_at = clock_timestamp()
             WHERE tenant_id = 'research-site' AND profile_key = 'general-research'",
        )
        .await
        .unwrap();
    assert_eq!(rows(&owner).await, before, "a seed rerun is not a change");
    let mut at_mate = api
        .read_changes(labmate, None, 2_000)
        .await
        .unwrap()
        .position;
    let mut at_out = api
        .read_changes(outsider, None, 2_000)
        .await
        .unwrap()
        .position;
    owner
        .batch_execute(
            "UPDATE enrichedlab_agent.broker_agent_profiles SET description = 'General research, revised.'
             WHERE tenant_id = 'research-site' AND profile_key = 'general-research'",
        )
        .await
        .unwrap();
    assert!(read(&api, labmate, &mut at_mate).await.access_changed);
    assert!(!read(&api, outsider, &mut at_out).await.access_changed);
    // An administrator from another lab inspects one of other-lab's projects; disabling other-lab ends that too.
    let elsewhere = Uuid::new_v4();
    api.create_project(
        outsider,
        &CreateProject {
            project_id: elsewhere,
            group: "other-lab".into(),
            title: "Elsewhere".into(),
            visibility: Visibility::Lab,
        },
    )
    .await
    .unwrap();
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_project_administrators(tenant_id, principal_id)
             VALUES ('research-site', $1) ON CONFLICT DO NOTHING",
            &[&me],
        )
        .await
        .unwrap();
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_project_inspections(tenant_id, project_id, principal_id, expires_at)
             VALUES ('research-site', $1, $2, clock_timestamp() + interval '1 hour')",
            &[&elsewhere, &me],
        )
        .await
        .unwrap();
    let mut at_inspector = api.read_changes(me, None, 2_000).await.unwrap().position;
    read(&api, outsider, &mut at_out).await;
    owner
        .batch_execute(
            "UPDATE enrichedlab_agent.broker_research_groups SET enabled = false
             WHERE tenant_id = 'research-site' AND group_key = 'other-lab'",
        )
        .await
        .unwrap();
    assert!(
        read(&api, me, &mut at_inspector).await.access_changed,
        "an inspector outside the lab"
    );
    assert!(read(&api, outsider, &mut at_out).await.access_changed);
    assert!(!read(&api, labmate, &mut at_mate).await.access_changed);

    // 14b. A transaction adds one change per distinct row: four files of one analysis make one task change, and two
    // identical changes one. A task a row names besides its own reaches only those who may read it.
    let before = rows(&owner).await;
    owner
        .batch_execute(&format!(
            "BEGIN;
             INSERT INTO enrichedlab_agent.broker_artifacts(tenant_id, task_id, role, label, relative_path, media_type,
               size_bytes, sha256)
             SELECT 'research-site', '{task}', 'result', 'Part ' || i, 'results/part-' || i || '.csv', 'text/csv', 1,
               repeat('d', 64) FROM generate_series(1, 3) AS i;
             INSERT INTO enrichedlab_agent.broker_artifacts(tenant_id, task_id, role, label, relative_path, media_type,
               size_bytes, sha256)
             VALUES ('research-site', '{task}', 'result', 'Part 4', 'results/part-4.csv', 'text/csv', 1, repeat('d', 64));
             COMMIT;"
        ))
        .await
        .unwrap();
    // One task change, and one for the lab whose note (step 5b) cites the analysis.
    assert_eq!(
        rows(&owner).await,
        before + 2,
        "one analysis's files in one transaction"
    );
    owner
        .batch_execute(&format!(
            "BEGIN;
             SELECT enrichedlab_agent.broker_add_change('research-site', 'audit', '{shared}', NULL, NULL, NULL, NULL,
               false, NULL);
             SELECT enrichedlab_agent.broker_add_change('research-site', 'audit', '{shared}', NULL, NULL, NULL, NULL,
               false, NULL);
             SELECT enrichedlab_agent.broker_add_change('research-site', 'audit', '{private}', NULL, NULL, NULL, NULL,
               false, NULL);
             COMMIT;"
        ))
        .await
        .unwrap();
    assert_eq!(
        rows(&owner).await,
        before + 4,
        "two identical changes and a third"
    );
    // A file only operators see is no workspace change; a file registered again with a new label is, and the same
    // registration again is not.
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_artifacts(tenant_id, task_id, visibility, role, label, relative_path,
               media_type, size_bytes, sha256)
             VALUES ('research-site', $1, 'operator', 'stdout', 'Log', 'logs/run.log', 'text/plain', 1, repeat('f', 64))",
            &[&task],
        )
        .await
        .unwrap();
    assert_eq!(rows(&owner).await, before + 4, "an operator-only file");
    let relabel = "UPDATE enrichedlab_agent.broker_artifacts SET label = $2
         WHERE task_id = $1 AND relative_path = 'results/part-1.csv'";
    owner
        .execute(relabel, &[&task, &"Part 1, relabelled"])
        .await
        .unwrap();
    assert_eq!(
        rows(&owner).await,
        before + 6,
        "a relabelled file (and its citing note)"
    );
    owner
        .execute(relabel, &[&task, &"Part 1, relabelled"])
        .await
        .unwrap();
    assert_eq!(
        rows(&owner).await,
        before + 6,
        "the same registration again"
    );
    let stranger = Uuid::new_v4();
    let mut at_named = api.read_changes(me, None, 2_000).await.unwrap().position;
    owner
        .execute(
            "SELECT enrichedlab_agent.broker_add_change('research-site', 'request', NULL, $1, NULL, NULL, NULL, false,
               ARRAY['task:' || $2::uuid::text])",
            &[&task, &stranger],
        )
        .await
        .unwrap();
    let named = read(&api, me, &mut at_named).await;
    assert!(
        has(&named, &format!("task:{task}")) && !has(&named, &format!("task:{stranger}")),
        "{:?}",
        named.keys
    );

    // 14c. A person's new name reaches their labs and project lists; a sign-in that keeps the name adds nothing. A new
    // render catalog reaches every enabled lab; the service reporting the same catalog again adds nothing.
    let mut at_name = api.read_changes(me, None, 2_000).await.unwrap().position;
    let before = rows(&owner).await;
    let rename =
        "UPDATE enrichedlab_agent.broker_principals SET display_name = $2 WHERE principal_id = $1";
    let kept: Option<String> = owner
        .query_one(
            "SELECT display_name FROM enrichedlab_agent.broker_principals WHERE principal_id = $1",
            &[&labmate],
        )
        .await
        .unwrap()
        .get(0);
    owner.execute(rename, &[&labmate, &kept]).await.unwrap();
    assert_eq!(rows(&owner).await, before, "the same name again");
    owner
        .execute(rename, &[&labmate, &Some("Lab mate, renamed")])
        .await
        .unwrap();
    let renamed = read(&api, me, &mut at_name).await;
    assert!(
        has(&renamed, "lab:pilot-lab") && has(&renamed, "projects"),
        "{:?}",
        renamed.keys
    );
    let catalog = "INSERT INTO enrichedlab_agent.broker_render_catalogs(tenant_id, catalog_sha256, service_version, features)
         VALUES ('research-site', repeat('e', 64), '0.6.0', jsonb_build_array('points'))
         ON CONFLICT (tenant_id, catalog_sha256) DO UPDATE SET last_seen_at = clock_timestamp()";
    owner.batch_execute(catalog).await.unwrap();
    assert!(has(&read(&api, me, &mut at_name).await, "catalogs"));
    // A person disabled changes who may be invited in their labs.
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_principals SET enabled = false WHERE principal_id = $1",
            &[&labmate],
        )
        .await
        .unwrap();
    assert!(has(&read(&api, me, &mut at_name).await, "lab:pilot-lab"));
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_principals SET enabled = true WHERE principal_id = $1",
            &[&labmate],
        )
        .await
        .unwrap();
    read(&api, me, &mut at_name).await;
    // A rerun of the catalog seed writes only what differs: the second of two runs is no change.
    let seed = || {
        let status = std::process::Command::new(env!("CARGO_BIN_EXE_agent-sandbox-broker"))
            .args([
                "seed",
                "--file",
                "examples/broker-catalog.yaml",
                "--migration-dsn-file",
            ])
            .arg(env::var_os("BROKER_QUALIFICATION_MIGRATION_DSN_FILE").unwrap())
            .env("RUST_LOG", "warn")
            .status()
            .unwrap();
        assert!(status.success());
    };
    seed();
    let before = rows(&owner).await;
    seed();
    assert_eq!(rows(&owner).await, before, "the same catalog seeded again");
    let before = rows(&owner).await;
    owner.batch_execute(catalog).await.unwrap();
    assert_eq!(rows(&owner).await, before, "the same catalog again");

    // 15. A disabled person reads nothing.
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_principals SET enabled = false WHERE principal_id = $1",
            &[&outsider],
        )
        .await
        .unwrap();
    assert_eq!(
        api.read_changes(outsider, Some(&at_out), 2_000)
            .await
            .unwrap_err()
            .kind,
        StoreErrorKind::Unauthorized
    );

    // 21. Run again while a service transaction holds a table it locks, migration 061 fails within its own lock
    // timeout, leaving nothing; once the table is free it applies.
    let migration = include_str!("../migrations/postgres/061_change_feed.sql");
    second_writer
        .batch_execute("BEGIN; LOCK TABLE enrichedlab_agent.broker_tasks IN ROW EXCLUSIVE MODE")
        .await
        .unwrap();
    let started = std::time::Instant::now();
    let refused = tokio::time::timeout(Duration::from_secs(10), owner.batch_execute(migration))
        .await
        .expect("the migration waits no longer than its lock timeout")
        .unwrap_err();
    assert_eq!(
        refused.code(),
        Some(&SqlState::LOCK_NOT_AVAILABLE),
        "{refused}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
    owner.batch_execute("ROLLBACK").await.unwrap();
    second_writer.batch_execute("ROLLBACK").await.unwrap();
    owner.batch_execute(migration).await.unwrap();

    drop(listener);
    drop((first_writer, second_writer, owner));
    for c in [first_connection, second_connection, connection] {
        c.await.unwrap().unwrap();
    }
}

async fn coordinator_start(api: &Store, me: Uuid, project: Uuid, run: Uuid, revision: i64) {
    api.start_research_run(
        me,
        project,
        &StartRun {
            run_id: run,
            expected_revision: revision,
            allowed_profiles: vec!["general-research".into()],
            max_turns: 4,
            max_parallel: 1,
        },
    )
    .await
    .unwrap();
}
