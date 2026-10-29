//! Workspace reads (migration 054) in isolated PostgreSQL: a person's last look at a project moves forward only and
//! is theirs alone; the project read holds the newest 200 analyses and a cursor that continues exactly where it stops,
//! page by page, in the same item shape; and history reads forward from a revision.
use enrichedlab_agent_sandbox::broker::{
    model::{PrincipalClass, SubmitTaskRequest, VerifiedIdentity},
    project::{CheckpointRequest, CreateProject, ResearchState, Visibility},
    store::Store,
};
use serde_json::{Value, json};
use std::{collections::HashSet, env, fs};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio_postgres::{Config, NoTls, config::Host};
use uuid::Uuid;

fn research(goal: &str) -> ResearchState {
    serde_json::from_value(json!({"schema_version":1,"goal":goal,
        "constraints":["Compute every value from the staged input."],
        "plan":[{"action_key":"fit-model","title":"Fit a linear model","status":"pending","task_id":null,
            "evidence_refs":[],"rationale":"Answer the question."}],
        "lessons":[],"next_action":"Run the analysis.",
        "workspace":{"schema_version":1,"profile":"general-research","image_digest":format!("sha256:{}","a".repeat(64)),
        "source_digest":format!("sha256:{}","b".repeat(64)),"input_refs":["input://public/toothgrowth"],
        "tool_refs":[],"preparation_receipt":null}})).unwrap()
}

fn at(look: &Value) -> OffsetDateTime {
    OffsetDateTime::parse(look["at"].as_str().unwrap(), &Rfc3339).unwrap()
}

fn ids(items: &Value) -> Vec<String> {
    items
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["task_id"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
#[ignore = "isolated PostgreSQL through qualify_remote_broker.sh workspace-reads"]
async fn the_workspace_reads_last_looks_analysis_pages_and_history_forward() {
    let dsn = fs::read_to_string(env::var_os("BROKER_QUALIFICATION_MIGRATION_DSN_FILE").unwrap())
        .unwrap();
    let config: Config = dsn.parse().unwrap();
    assert_eq!(config.get_dbname(), Some("broker_qual"));
    assert!(matches!(config.get_hosts(),[Host::Tcp(h)] if h=="127.0.0.1"));
    let (owner, connection) = config.connect(NoTls).await.unwrap();
    let connection = tokio::spawn(connection);
    let api = Store::connect(
        &dsn.replace("user=postgres", "user=broker_api_qual"),
        "research-site".into(),
        2,
    )
    .unwrap();
    // The API stays out of service until migration 054 and its grants exist.
    api.ready_for_review_api().await.unwrap();
    owner
        .batch_execute(
            "INSERT INTO enrichedlab_agent.broker_principal_invitations(tenant_id,email,group_key,role) VALUES
        ('research-site','colleague@example.invalid','pilot-lab','researcher'),
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
        .resolve_principal(&person("me", "researcher@example.invalid"))
        .await
        .unwrap();
    let colleague = api
        .resolve_principal(&person("colleague", "colleague@example.invalid"))
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
            title: "Lab study".into(),
            visibility: Visibility::Lab,
        },
    )
    .await
    .unwrap();
    for revision in 0..3 {
        api.checkpoint_project(
            me,
            project,
            &CheckpointRequest {
                expected_revision: revision,
                request_id: Uuid::new_v4(),
                state: research(&format!(
                    "Does dose change tooth length? Take {}",
                    revision + 1
                )),
            },
        )
        .await
        .unwrap();
    }
    let hidden = Uuid::new_v4();
    api.create_project(
        colleague,
        &CreateProject {
            project_id: hidden,
            group: "pilot-lab".into(),
            title: "Colleague's private study".into(),
            visibility: Visibility::Private,
        },
    )
    .await
    .unwrap();

    // Last look: none yet, then forward only, per person.
    let read = api.read_project(me, project).await.unwrap();
    assert_eq!(read["last_look"], json!({"revision": null, "at": null}));
    let first = api.project_look(me, project, 1).await.unwrap();
    assert_eq!(first["revision"], 1);
    let stale = api.project_look(me, project, 0).await.unwrap();
    assert_eq!(
        stale, first,
        "a lower revision leaves the stored look unchanged"
    );
    let again = api.project_look(me, project, 1).await.unwrap();
    assert_eq!(again["revision"], 1);
    assert!(
        at(&again) > at(&first),
        "looking again at the same revision moves the time"
    );
    let latest = api.project_look(me, project, 3).await.unwrap();
    assert_eq!(latest["revision"], 3);
    for revision in [4, -1] {
        assert_eq!(
            api.project_look(me, project, revision)
                .await
                .unwrap_err()
                .code,
            "look_revision_invalid"
        );
    }
    assert_eq!(
        api.read_project(me, project).await.unwrap()["last_look"],
        latest
    );
    let list = api.list_projects(me, 200).await.unwrap();
    let listed = list
        .iter()
        .find(|p| p["project_id"].as_str() == Some(&project.to_string()))
        .unwrap();
    assert_eq!(listed["last_look"], latest);
    assert_eq!(
        api.read_project(colleague, project).await.unwrap()["last_look"],
        json!({"revision": null, "at": null}),
        "a look is the person's own"
    );
    assert_eq!(
        api.project_look(workload, project, 1)
            .await
            .unwrap_err()
            .code,
        "look_person_only"
    );
    assert_eq!(
        api.project_look(me, hidden, 0).await.unwrap_err().code,
        "project_not_found"
    );

    // Analyses past 200: one through the API, 204 copies of it as the owner, one second apart.
    let seed = api
        .submit_project_task(
            me,
            &SubmitTaskRequest {
                group: "pilot-lab".into(),
                profile: "general-research".into(),
                objective: "Fit the model.".into(),
                project_id: Some(project),
                action_key: Some("fit-model".into()),
                parent_task_id: None,
            },
            "workspace-reads-0",
            &"d".repeat(64),
        )
        .await
        .unwrap();
    let copied = owner
        .execute(
            "WITH g AS (SELECT gen_random_uuid() AS id, n FROM generate_series(1, 204) n),
             ins AS (
               INSERT INTO enrichedlab_agent.broker_tasks
               SELECT (jsonb_populate_record(NULL::enrichedlab_agent.broker_tasks, to_jsonb(t) || jsonb_build_object(
                 'task_id', g.id, 'idempotency_key', 'workspace-reads-' || g.n,
                 'logical_agent', 'task-' || replace(g.id::text, '-', ''),
                 'submitted_at', t.submitted_at - make_interval(secs => g.n)))).*
               FROM enrichedlab_agent.broker_tasks t CROSS JOIN g
               WHERE t.tenant_id = 'research-site' AND t.task_id = $1
               RETURNING task_id, idempotency_key)
             INSERT INTO enrichedlab_agent.broker_project_tasks(tenant_id, project_id, task_id, action_key)
             SELECT 'research-site', $2, task_id, 'copy-' || split_part(idempotency_key, '-', 3) FROM ins",
            &[&seed, &project],
        )
        .await
        .unwrap();
    assert_eq!(copied, 204);

    let read = api.read_project(me, project).await.unwrap();
    let held = ids(&read["tasks"]);
    assert_eq!(held.len(), 200);
    assert_eq!(held[0], seed.to_string(), "newest first");
    assert_eq!(read["analyses_total"], 205);
    assert_eq!(read["task_count"], 205);
    let cursor = read["analyses_next"].as_str().unwrap().to_owned();
    let rest = api
        .project_analyses(me, project, Some(&cursor), 200)
        .await
        .unwrap();
    assert_eq!(rest["total"], 205);
    assert_eq!(rest["next"], Value::Null);
    let rest_ids = ids(&rest["analyses"]);
    assert_eq!(rest_ids.len(), 5);
    let everything: Vec<String> = held.iter().chain(rest_ids.iter()).cloned().collect();
    assert_eq!(everything.iter().collect::<HashSet<_>>().len(), 205);

    // Walking in pages of seven from the start gives the same order, and the first page has the read's item shape.
    let mut walked = Vec::new();
    let mut next: Option<String> = None;
    loop {
        let page = api
            .project_analyses(me, project, next.as_deref(), 7)
            .await
            .unwrap();
        walked.extend(ids(&page["analyses"]));
        match page["next"].as_str() {
            Some(n) => next = Some(n.to_owned()),
            None => break,
        }
    }
    assert_eq!(walked, everything);
    let opening = api.project_analyses(me, project, None, 200).await.unwrap();
    assert_eq!(opening["analyses"], read["tasks"]);
    assert_eq!(opening["next"], read["analyses_next"]);

    // A queued analysis carries its place in the site's line, oldest first, and what holds it back.
    let queue = |id: &str, page: &Value| -> Value {
        page["tasks"]
            .as_array()
            .or(page["analyses"].as_array())
            .unwrap()
            .iter()
            .find(|item| item["task_id"] == id)
            .map(|item| item["queue"].clone())
            .unwrap()
    };
    let idle = json!({"ahead": 204, "lab_full": false, "person_full": false});
    assert_eq!(queue(&seed.to_string(), &read), idle);
    let oldest = rest_ids.last().unwrap().clone();
    assert_eq!(queue(&oldest, &rest)["ahead"], 0);
    let start = |ids: &[String]| {
        let ids: Vec<Uuid> = ids.iter().map(|id| id.parse().unwrap()).collect();
        let owner = &owner;
        async move {
            owner
                .execute(
                    "UPDATE enrichedlab_agent.broker_tasks SET status = 'running', attempt = 1,
                       lease_owner = 'worker-q', lease_generation = lease_generation + 1,
                       lease_expires_at = now() + interval '1 hour'
                     WHERE tenant_id = 'research-site' AND task_id = ANY($1)",
                    &[&ids],
                )
                .await
                .unwrap();
        }
    };
    let seed_queue = || async {
        queue(
            &seed.to_string(),
            &api.read_project(me, project).await.unwrap(),
        )
    };
    // My oldest analysis starts: the line shortens and nothing is full (my limit is two, the lab's three).
    start(&rest_ids[4..5]).await;
    assert_eq!(
        seed_queue().await,
        json!({"ahead": 203, "lab_full": false, "person_full": false})
    );
    let started = api
        .project_analyses(me, project, Some(&cursor), 200)
        .await
        .unwrap();
    assert_eq!(
        queue(&oldest, &started),
        Value::Null,
        "a running analysis has no place in line"
    );
    // A colleague's analysis runs: two in the lab, one of them mine, so neither limit holds mine back.
    owner
        .execute(
            "WITH g AS (SELECT gen_random_uuid() AS id)
             INSERT INTO enrichedlab_agent.broker_tasks
             SELECT (jsonb_populate_record(NULL::enrichedlab_agent.broker_tasks, to_jsonb(t) || jsonb_build_object(
               'task_id', g.id, 'idempotency_key', 'workspace-reads-colleague', 'principal_id', $2::uuid,
               'logical_agent', 'task-' || replace(g.id::text, '-', ''), 'status', 'running', 'attempt', 1,
               'lease_owner', 'worker-q', 'lease_generation', 1, 'lease_expires_at', now() + interval '1 hour'))).*
             FROM enrichedlab_agent.broker_tasks t CROSS JOIN g
             WHERE t.tenant_id = 'research-site' AND t.task_id = $1",
            &[&seed, &colleague],
        )
        .await
        .unwrap();
    assert_eq!(
        seed_queue().await,
        json!({"ahead": 203, "lab_full": false, "person_full": false})
    );
    // A second of mine starts: my limit and the lab's are both reached.
    start(&rest_ids[3..4]).await;
    assert_eq!(
        seed_queue().await,
        json!({"ahead": 202, "lab_full": true, "person_full": true})
    );
    // An analysis whose attempts are spent cannot start and leaves the line.
    let spent = rest_ids[2].clone();
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_tasks SET attempt = max_task_attempts
             WHERE tenant_id = 'research-site' AND task_id = $1",
            &[&spent.parse::<Uuid>().unwrap()],
        )
        .await
        .unwrap();
    let after = api
        .project_analyses(me, project, Some(&cursor), 200)
        .await
        .unwrap();
    assert_eq!(queue(&spent, &after)["ahead"], Value::Null);
    assert_eq!(seed_queue().await["ahead"], 201);
    // An expired lease no longer counts as active: the colleague's slot frees the lab.
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_tasks SET lease_expires_at = now() - interval '1 minute'
             WHERE tenant_id = 'research-site' AND idempotency_key = 'workspace-reads-colleague'",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(
        seed_queue().await,
        json!({"ahead": 201, "lab_full": false, "person_full": true})
    );

    let task = Uuid::new_v4();
    for bad in [
        String::new(),
        "a1.x".to_owned(),
        format!("a2.20261012T000000.000000.{task}"),
        format!("a1.20261312T000000.000000.{task}"),
        format!("a1.20261012T000000.000000.{}", "z".repeat(36)),
        format!("a1.20261012T000000.000000.{task}.extra"),
    ] {
        assert_eq!(
            api.project_analyses(me, project, Some(&bad), 10)
                .await
                .unwrap_err()
                .code,
            "analyses_cursor_invalid",
            "{bad}"
        );
    }
    for limit in [0, 201] {
        assert_eq!(
            api.project_analyses(me, project, None, limit)
                .await
                .unwrap_err()
                .code,
            "list_limit_invalid"
        );
    }
    assert_eq!(
        api.project_analyses(me, hidden, None, 10)
            .await
            .unwrap_err()
            .code,
        "project_not_found"
    );

    // History forward: oldest first after a revision, with the revision to read from next.
    let forward = api.project_history_after(me, project, 0, 2).await.unwrap();
    let revisions = |page: &Value| -> Vec<i64> {
        page["checkpoints"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["revision"].as_i64().unwrap())
            .collect()
    };
    assert_eq!(revisions(&forward), vec![1, 2]);
    assert_eq!(forward["next_after"], 2);
    let tail = api.project_history_after(me, project, 2, 2).await.unwrap();
    assert_eq!(revisions(&tail), vec![3]);
    assert_eq!(tail["next_after"], Value::Null);
    let none = api.project_history_after(me, project, 3, 2).await.unwrap();
    assert_eq!(none, json!({"checkpoints": [], "next_after": null}));
    let backward = api.project_history(me, project, 0, 100).await.unwrap();
    assert_eq!(
        backward.last().unwrap(),
        &forward["checkpoints"][0],
        "the same checkpoint shape both ways"
    );
    for (after, limit) in [(-1, 10), (0, 0), (0, 101)] {
        assert_eq!(
            api.project_history_after(me, project, after, limit)
                .await
                .unwrap_err()
                .code,
            "list_limit_invalid"
        );
    }
    assert_eq!(
        api.project_history_after(me, hidden, 0, 10)
            .await
            .unwrap_err()
            .code,
        "project_not_found"
    );

    drop(owner);
    connection.abort();
}
