//! Result digests (migration 021) in isolated PostgreSQL: the worker records one
//! digest per result package under its lease, bound to the package's bytes and immutable; the
//! coordinator holding a run reads the digests of its analyses and the project's newest findings;
//! a reader of the artifact reads its digest and anyone else is refused.
use enrichedlab_agent_sandbox::broker::{
    coordinator::StartRun,
    digest,
    model::{PrincipalClass, VerifiedIdentity},
    project::{CheckpointRequest, CreateProject, ResearchState, Visibility},
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

fn digest(cells: &[[&str; 2]]) -> Value {
    json!({"schema":"enrichedlab.result-digest.v1","status":"summarized",
        "package":{"members":3,"checksums_listed":true,"all_matched":true},
        "tables":[{"path":"results/means.csv","sha256":"e".repeat(64),"bytes":40,"rows":cells.len(),
            "columns":["supp","mean"],"cells":cells}],"tables_omitted":0,"figures":[]})
}

#[tokio::test]
#[ignore = "isolated PostgreSQL through qualify_remote_broker.sh result-digests"]
async fn digests_are_fenced_bound_immutable_and_read_only_by_their_readers() {
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
    // The coordinator stays out of service until migration 021 and its grant exist.
    coordinator.ready_for_research_results().await.unwrap();
    api.ready_for_review_api().await.unwrap();
    owner
        .batch_execute(
            "INSERT INTO enrichedlab_agent.broker_principal_invitations(tenant_id,email,group_key,role) VALUES
        ('research-site','outsider@example.invalid','pilot-lab','researcher');",
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
    let project = Uuid::new_v4();
    api.create_project(
        me,
        &CreateProject {
            project_id: project,
            group: "pilot-lab".into(),
            title: "Digests".into(),
            visibility: Visibility::Private,
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

    // Turn 1 plans one analysis; the lead holds the run.
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

    // The worker registers the package and records its digest under its lease.
    let claimed = worker.claim_task("worker-1", 300).await.unwrap().unwrap();
    assert_eq!(claimed.id, task);
    let g = claimed.lease_generation;
    let package = "1".repeat(64);
    let path = format!("tasks/{task}/attempts/1-{g}/exports/payload");
    worker
        .register_artifact(
            task,
            "worker-1",
            g,
            "researcher",
            "result",
            "Package",
            &path,
            "application/gzip",
            900,
            &package,
        )
        .await
        .unwrap();
    worker
        .register_artifact(
            task,
            "worker-1",
            g,
            "operator",
            "stdout",
            "Log",
            &format!("tasks/{task}/log.txt"),
            "text/plain",
            9,
            &"2".repeat(64),
        )
        .await
        .unwrap();
    let first = digest(&[["OJ", "20.66"], ["VC", "16.96"]]);
    let sha = worker
        .record_artifact_digest(task, "worker-1", g, &path, &package, "summarized", &first)
        .await
        .unwrap();
    assert_eq!(sha.len(), 64);
    // The same digest again is accepted and returns the same SHA-256; a different one is refused.
    assert_eq!(
        worker
            .record_artifact_digest(task, "worker-1", g, &path, &package, "summarized", &first)
            .await
            .unwrap(),
        sha
    );
    let other = digest(&[["OJ", "99"]]);
    assert_eq!(
        worker
            .record_artifact_digest(task, "worker-1", g, &path, &package, "summarized", &other)
            .await
            .unwrap_err()
            .kind,
        StoreErrorKind::Conflict
    );
    // Only this lease, only the registered result bytes, only a well-formed digest.
    let refused = |e: enrichedlab_agent_sandbox::broker::store::StoreError| e.code;
    assert_eq!(
        refused(
            worker
                .record_artifact_digest(task, "worker-2", g, &path, &package, "summarized", &first)
                .await
                .unwrap_err()
        ),
        "lease_fence_lost"
    );
    assert_eq!(
        refused(
            worker
                .record_artifact_digest(
                    task,
                    "worker-1",
                    g,
                    &path,
                    &"9".repeat(64),
                    "summarized",
                    &first
                )
                .await
                .unwrap_err()
        ),
        "artifact_digest_target_invalid"
    );
    assert_eq!(
        refused(
            worker
                .record_artifact_digest(
                    task,
                    "worker-1",
                    g,
                    &format!("tasks/{task}/log.txt"),
                    &"2".repeat(64),
                    "summarized",
                    &first
                )
                .await
                .unwrap_err()
        ),
        "artifact_digest_target_invalid"
    );
    assert_eq!(
        refused(
            worker
                .record_artifact_digest(task, "worker-1", g, &path, &package, "unreadable", &first)
                .await
                .unwrap_err()
        ),
        "artifact_digest_invalid"
    );
    // Other roles cannot write digests, and the row cannot be changed or removed.
    assert!(
        api.record_artifact_digest(task, "worker-1", g, &path, &package, "summarized", &first)
            .await
            .is_err()
    );
    assert!(
        coordinator
            .record_artifact_digest(task, "worker-1", g, &path, &package, "summarized", &first)
            .await
            .is_err()
    );
    for statement in [
        "UPDATE enrichedlab_agent.broker_artifact_digests SET status='unreadable'",
        "DELETE FROM enrichedlab_agent.broker_artifact_digests",
    ] {
        assert!(owner.execute(statement, &[]).await.is_err(), "{statement}");
    }
    let stored: String = owner
        .query_one(
            "SELECT encode(sha256(convert_to(digest::text,'UTF8')),'hex') FROM enrichedlab_agent.broker_artifact_digests",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(stored, sha, "anyone can check the stored digest's SHA-256");

    // A reader gets the digest for the artifact's current bytes; an outsider gets nothing.
    let artifact: Uuid = owner
        .query_one(
            "SELECT artifact_id FROM enrichedlab_agent.broker_artifacts WHERE relative_path=$1",
            &[&path],
        )
        .await
        .unwrap()
        .get(0);
    let read = api.artifact_digest(me, task, artifact).await.unwrap();
    assert_eq!(read["status"], "summarized");
    assert_eq!(read["digest_sha256"], sha.as_str());
    assert_eq!(
        read["digest"]["tables"][0]["cells"][0],
        json!(["OJ", "20.66"])
    );
    assert_eq!(
        api.artifact_digest(outsider, task, artifact)
            .await
            .unwrap_err()
            .kind,
        StoreErrorKind::NotFound
    );

    // Only the coordinator holding the run reads its results, and only for planned analyses.
    worker
        .finish_task(task, "worker-1", g, true, "success", "")
        .await
        .unwrap();
    assert_eq!(
        coordinator
            .research_run_results(run, "lead-a", generation)
            .await
            .unwrap_err()
            .kind,
        StoreErrorKind::LeaseLost,
        "the committed turn released the first lease"
    );
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
    assert!(
        api.research_run_results(run, "lead-b", generation_b)
            .await
            .is_err()
    );
    let results = coordinator
        .research_run_results(run, "lead-b", generation_b)
        .await
        .unwrap();
    let items = results["results"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0]["ref"],
        format!("artifact://tasks/{task}/{artifact}")
    );
    assert_eq!(items[0]["digest_sha256"], sha.as_str());
    assert!(results["earlier_findings"].is_null());

    // A turn with findings becomes the project's newest findings, read by the next run.
    let findings = json!([{"statement":"Orange juice ({a}).","bindings":{"a":{"ref":items[0]["ref"],"table":"results/means.csv","row":1,"column":"mean"}}}]);
    let second = coordinator
        .commit_research_turn(
            run,
            "lead-b",
            generation_b,
            claim["project"]["checkpoint"]["revision"].as_i64().unwrap(),
            &claim["project"]["checkpoint"]["state"],
            &json!([]),
            &json!({"schema_version":1,"proposal":{"answer":"answered","findings":findings}}),
            "review_required",
        )
        .await
        .unwrap();
    let next = api
        .start_research_run(
            me,
            project,
            &StartRun {
                run_id: Uuid::new_v4(),
                expected_revision: second["revision"].as_i64().unwrap(),
                allowed_profiles: vec!["general-research".into()],
                max_turns: 4,
                max_parallel: 1,
            },
        )
        .await
        .unwrap();
    let claim = coordinator
        .claim_research_run("lead-c", 90)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim["run"]["run_id"], next.to_string());
    let results = coordinator
        .research_run_results(next, "lead-c", claim["run"]["generation"].as_i64().unwrap())
        .await
        .unwrap();
    assert_eq!(results["earlier_findings"]["answer"], "answered");
    assert_eq!(results["earlier_findings"]["findings"], findings);
    assert_eq!(results["earlier_findings"]["run_id"], run.to_string());
    assert_eq!(results["latest_answer"]["answer"], "answered");
    assert_eq!(results["latest_answer"]["findings"], 1);
    // Migration 064: a decision without answer tables returns none and counts none.
    assert_eq!(results["earlier_findings"]["tables"], json!([]));
    assert_eq!(results["latest_answer"]["tables"], 0);
    assert_eq!(results["results"].as_array().unwrap().len(), 1);

    // A newer decision that answers differently, with nothing to state, is the current
    // answer; the older findings no longer stand for it in the next run's context.
    let generation_c = claim["run"]["generation"].as_i64().unwrap();
    let third = coordinator
        .commit_research_turn(
            next,
            "lead-c",
            generation_c,
            claim["project"]["checkpoint"]["revision"].as_i64().unwrap(),
            &claim["project"]["checkpoint"]["state"],
            &json!([]),
            &json!({"schema_version":1,"proposal":{"answer":"not_answered","findings":[]}}),
            "review_required",
        )
        .await
        .unwrap();
    let latest = api
        .start_research_run(
            me,
            project,
            &StartRun {
                run_id: Uuid::new_v4(),
                expected_revision: third["revision"].as_i64().unwrap(),
                allowed_profiles: vec!["general-research".into()],
                max_turns: 4,
                max_parallel: 1,
            },
        )
        .await
        .unwrap();
    let claim = coordinator
        .claim_research_run("lead-d", 90)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim["run"]["run_id"], latest.to_string());
    let results = coordinator
        .research_run_results(
            latest,
            "lead-d",
            claim["run"]["generation"].as_i64().unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(results["latest_answer"]["answer"], "not_answered");
    assert_eq!(results["latest_answer"]["findings"], 0);
    assert_eq!(results["latest_answer"]["run_id"], next.to_string());
    // The findings stated before stay as dated history, older than the answer that replaced them.
    assert_eq!(results["earlier_findings"]["findings"], findings);
    assert_eq!(results["earlier_findings"]["run_id"], run.to_string());
    assert!(
        results["earlier_findings"]["recorded_at"].as_str().unwrap()
            < results["latest_answer"]["recorded_at"].as_str().unwrap()
    );

    // A result package registered before the worker built digests gets one from
    // the operator, through the same builder, marked as backfilled; no runtime role may record one.
    let generation_d = claim["run"]["generation"].as_i64().unwrap();
    let mut state: ResearchState =
        serde_json::from_value(claim["project"]["checkpoint"]["state"].clone()).unwrap();
    state.plan.push(
        serde_json::from_value(
            json!({"action_key":"medians","title":"Bootstrap medians","status":"pending",
        "task_id":null,"evidence_refs":[],"rationale":"Answer the question."}),
        )
        .unwrap(),
    );
    let committed = coordinator
        .commit_research_turn(
            latest,
            "lead-d",
            generation_d,
            claim["project"]["checkpoint"]["revision"].as_i64().unwrap(),
            &serde_json::to_value(&state).unwrap(),
            &json!([{"action_key":"medians","profile":"general-research","objective":"Estimate the medians.","request_sha256":"d".repeat(64)}]),
            &json!({"schema_version":1,"proposal":{"answer":"not_yet","findings":[]}}),
            "delegate",
        )
        .await
        .unwrap();
    let old_task: Uuid = serde_json::from_value(committed["tasks"][0]["task_id"].clone()).unwrap();
    let claimed = worker.claim_task("worker-1", 300).await.unwrap().unwrap();
    assert_eq!(claimed.id, old_task);
    let og = claimed.lease_generation;
    let old_package = "3".repeat(64);
    let old_path = format!("tasks/{old_task}/attempts/1-{og}/exports/payload");
    worker
        .register_artifact(
            old_task,
            "worker-1",
            og,
            "researcher",
            "result",
            "Package",
            &old_path,
            "application/gzip",
            700,
            &old_package,
        )
        .await
        .unwrap();
    let backfill = "SELECT enrichedlab_agent.broker_backfill_artifact_digest($1,$2,$3,$4,$5,$6)";
    let message = |e: tokio_postgres::Error| e.as_db_error().map(|d| d.message().to_owned());
    let old = digest(&[["OJ", "22.7"]]);
    let tenant = "research-site";
    let summarized = "summarized";
    // Not while the task can still record its own.
    assert_eq!(
        message(
            owner
                .query_one(
                    backfill,
                    &[
                        &tenant,
                        &old_task,
                        &old_path,
                        &old_package,
                        &summarized,
                        &old
                    ]
                )
                .await
                .unwrap_err()
        )
        .as_deref(),
        Some("broker:artifact_digest_target_invalid")
    );
    worker
        .finish_task(old_task, "worker-1", og, true, "success", "")
        .await
        .unwrap();
    let old_sha: String = owner
        .query_one(
            backfill,
            &[
                &tenant,
                &old_task,
                &old_path,
                &old_package,
                &summarized,
                &old,
            ],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(old_sha.len(), 64);
    // The same digest again is accepted; another for the same bytes is refused.
    let again: String = owner
        .query_one(
            backfill,
            &[
                &tenant,
                &old_task,
                &old_path,
                &old_package,
                &summarized,
                &old,
            ],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(again, old_sha);
    let changed = digest(&[["OJ", "1"]]);
    assert_eq!(
        message(
            owner
                .query_one(
                    backfill,
                    &[
                        &tenant,
                        &old_task,
                        &old_path,
                        &old_package,
                        &summarized,
                        &changed
                    ]
                )
                .await
                .unwrap_err()
        )
        .as_deref(),
        Some("broker:artifact_digest_conflict")
    );
    // Only the registered result bytes of a known task, and only a well-formed digest.
    let wrong = "9".repeat(64);
    let log = format!("tasks/{task}/log.txt");
    let log_sha = "2".repeat(64);
    let unknown = Uuid::new_v4();
    let unreadable = "unreadable";
    for (task_id, path, sha, status, code) in [
        (
            old_task,
            &old_path,
            &wrong,
            summarized,
            "broker:artifact_digest_target_invalid",
        ),
        (
            task,
            &log,
            &log_sha,
            summarized,
            "broker:artifact_digest_target_invalid",
        ),
        (
            unknown,
            &old_path,
            &old_package,
            summarized,
            "broker:artifact_digest_target_invalid",
        ),
        (
            old_task,
            &old_path,
            &old_package,
            unreadable,
            "broker:artifact_digest_invalid",
        ),
    ] {
        assert_eq!(
            message(
                owner
                    .query_one(backfill, &[&tenant, &task_id, path, sha, &status, &old])
                    .await
                    .unwrap_err()
            )
            .as_deref(),
            Some(code)
        );
    }
    // The row says it was backfilled and carries the task's attempt and generation.
    let rows = owner
        .query(
            "SELECT d.task_id, d.source, d.attempt = t.attempt, d.lease_generation
               FROM enrichedlab_agent.broker_artifact_digests d
               JOIN enrichedlab_agent.broker_tasks t ON t.tenant_id = d.tenant_id AND t.task_id = d.task_id
              ORDER BY d.recorded_at",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get::<_, Uuid>(0), task);
    assert_eq!(rows[0].get::<_, String>(1), "worker");
    assert_eq!(rows[1].get::<_, Uuid>(0), old_task);
    assert_eq!(rows[1].get::<_, String>(1), "operator_backfill");
    assert!(rows[1].get::<_, bool>(2));
    assert_eq!(rows[1].get::<_, i64>(3), og);
    for role in [
        "broker_api_qual",
        "broker_worker_qual",
        "broker_coordinator_qual",
    ] {
        let can: bool = owner
            .query_one(
                "SELECT has_function_privilege($1, 'enrichedlab_agent.broker_backfill_artifact_digest(text,uuid,text,text,text,jsonb)', 'EXECUTE')",
                &[&role],
            )
            .await
            .unwrap()
            .get(0);
        assert!(!can, "{role} must not record a backfilled digest");
    }
    // The artifact's readers read it like any other digest.
    let old_artifact: Uuid = owner
        .query_one(
            "SELECT artifact_id FROM enrichedlab_agent.broker_artifacts WHERE relative_path=$1",
            &[&old_path],
        )
        .await
        .unwrap()
        .get(0);
    let read = api
        .artifact_digest(me, old_task, old_artifact)
        .await
        .unwrap();
    assert_eq!(read["digest_sha256"], old_sha.as_str());
    assert_eq!(
        read["digest"]["tables"][0]["cells"][0],
        json!(["OJ", "22.7"])
    );

    drop(owner);
    connection.abort();
}

/// A package the worker digests near the stored bound: twelve tables whose missing-value counts alone pass it as
/// compact JSON, and pass it further with the spaces PostgreSQL prints.
fn near_the_bound() -> Value {
    use sha2::{Digest, Sha256};
    let tables: Vec<(String, Vec<u8>)> = (0..12)
        .map(|t| {
            let names: Vec<String> = (0..16)
                .map(|c| format!("\"t{t}_c{c}_{}\"", "x".repeat(18 + t)))
                .collect();
            (
                format!("results/t{t:02}.csv"),
                format!("{}\n{}\n", names.join(","), vec![""; 16].join(",")).into_bytes(),
            )
        })
        .collect();
    let sums: String = tables
        .iter()
        .map(|(path, bytes)| format!("{}  {path}\n", hex::encode(Sha256::digest(bytes))))
        .collect();
    let mut builder = tar::Builder::new(
        flate2::GzBuilder::new().write(Vec::new(), flate2::Compression::default()),
    );
    for (path, bytes) in tables
        .iter()
        .map(|(p, b)| (p.as_str(), b.as_slice()))
        .chain([("SHA256SUMS", sums.as_bytes())])
    {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o600);
        header.set_cksum();
        builder.append_data(&mut header, path, bytes).unwrap();
    }
    let package = builder.into_inner().unwrap().finish().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let task = Uuid::new_v4();
    let relative = format!("tasks/{task}/attempts/1-1/exports/payload");
    let path = dir.path().join(&relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, &package).unwrap();
    let sha = hex::encode(Sha256::digest(&package));
    digest::build(dir.path(), task, &relative, package.len() as u64, &sha).value
}

#[tokio::test]
#[ignore = "isolated PostgreSQL through qualify_remote_broker.sh result-digests"]
async fn the_digest_bound_is_measured_as_postgresql_stores_it() {
    // Migration 021 bounds `octet_length(digest::text)`, the jsonb as PostgreSQL prints it; the worker's measure must
    // give the same number for a digest near the bound and for numbers and text that print differently.
    let dsn = fs::read_to_string(env::var_os("BROKER_QUALIFICATION_MIGRATION_DSN_FILE").unwrap())
        .unwrap();
    let config: Config = dsn.parse().unwrap();
    assert_eq!(config.get_dbname(), Some("broker_qual"));
    let (client, connection) = config.connect(NoTls).await.unwrap();
    let connection = tokio::spawn(connection);
    let near = near_the_bound();
    assert_eq!(near["tables"].as_array().unwrap().len(), 12);
    let printed = json!({"a": [1, "x\n\u{1}é"], "b": {}, "c": [], "d": null,
        "n": [-0.0, 123.0, 0.05, 1e-7, 1e21, 1.5e300, -25]});
    for value in [&near, &printed] {
        let stored: i32 = client
            .query_one("SELECT octet_length($1::jsonb::text)", &[value])
            .await
            .unwrap()
            .get(0);
        assert_eq!(
            usize::try_from(stored).unwrap(),
            digest::stored_bytes(value),
            "{value}"
        );
    }
    assert!(digest::stored_bytes(&near) <= digest::MAX_DIGEST_BYTES);
    drop(client);
    connection.abort();
}
