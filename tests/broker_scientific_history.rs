use enrichedlab_agent_sandbox::broker::{
    model::ClaimedTask,
    store::{ScientificHistoryEvidenceInput, Store, StoreErrorKind},
};
use serde_json::json;
use std::{env, fs};
use tokio_postgres::{Config, NoTls, config::Host};
use uuid::Uuid;

const MIGRATION: &str = include_str!("../migrations/postgres/007_broker_scientific_history.sql");
const EVIDENCE_MIGRATION: &str =
    include_str!("../migrations/postgres/009_broker_scientific_history_evidence.sql");

#[tokio::test]
#[ignore = "uses the isolated PostgreSQL database with a private qualification DSN"]
async fn scientific_history_is_owner_fenced_paged_and_attempt_bound() {
    let path = env::var_os("BROKER_QUALIFICATION_MIGRATION_DSN_FILE")
        .expect("private qualification DSN file required");
    let dsn = fs::read_to_string(path).expect("private qualification DSN file unreadable");
    let owner_config: Config = dsn.parse().expect("invalid qualification DSN");
    assert_eq!(owner_config.get_dbname(), Some("broker_qual"));
    assert_eq!(owner_config.get_user(), Some("postgres"));
    assert!(matches!(owner_config.get_hosts(), [Host::Tcp(host)] if host == "127.0.0.1"));
    let qualification_password = std::str::from_utf8(
        owner_config
            .get_password()
            .expect("qualification DSN password required"),
    )
    .expect("qualification DSN password must be UTF-8");
    let (owner, connection) = owner_config
        .connect(NoTls)
        .await
        .expect("qualification database unavailable");
    let owner_connection = tokio::spawn(connection);

    owner
        .batch_execute(MIGRATION)
        .await
        .expect("migration 007 failed");
    owner
        .batch_execute(EVIDENCE_MIGRATION)
        .await
        .expect("migration 009 failed");
    owner
        .batch_execute(
            "DO $$ BEGIN \
                 IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'broker_history_api_qual') THEN \
                   DROP OWNED BY broker_history_api_qual; \
                   DROP ROLE broker_history_api_qual; \
                 END IF; \
                 IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'broker_history_worker_qual') THEN \
                   DROP OWNED BY broker_history_worker_qual; \
                   DROP ROLE broker_history_worker_qual; \
                 END IF; \
               END $$; \
             CREATE ROLE broker_history_api_qual LOGIN; \
             CREATE ROLE broker_history_worker_qual LOGIN; \
             GRANT USAGE ON SCHEMA enrichedlab_agent TO broker_history_api_qual, broker_history_worker_qual; \
             GRANT EXECUTE ON FUNCTION enrichedlab_agent.broker_scientific_history_status(text,uuid,uuid,integer,bigint) TO broker_history_api_qual; \
             GRANT EXECUTE ON FUNCTION enrichedlab_agent.broker_list_scientific_history_events_with_evidence(text,uuid,uuid,integer,bigint,bigint,integer) TO broker_history_api_qual; \
             GRANT EXECUTE ON FUNCTION enrichedlab_agent.broker_list_scientific_history_evidence(text,uuid,uuid,integer,bigint,uuid,uuid,integer) TO broker_history_api_qual; \
             GRANT EXECUTE ON FUNCTION enrichedlab_agent.broker_bind_scientific_history(text,uuid,text,integer,bigint,uuid,text,text,text,bigint) TO broker_history_worker_qual; \
             GRANT EXECUTE ON FUNCTION enrichedlab_agent.broker_finalize_scientific_history(text,uuid,text,integer,bigint,bigint) TO broker_history_worker_qual; \
             GRANT EXECUTE ON FUNCTION enrichedlab_agent.broker_record_scientific_history_evidence(text,uuid,text,integer,bigint,uuid,bigint,text,uuid,uuid,text,bigint,text) TO broker_history_worker_qual;",
        )
        .await
        .expect("qualification role setup failed");
    for role in ["broker_history_api_qual", "broker_history_worker_qual"] {
        let password_statement: String = owner
            .query_one(
                "SELECT format('ALTER ROLE %I PASSWORD %L', $1::text, $2::text)",
                &[&role, &qualification_password],
            )
            .await
            .expect("qualification role password statement failed")
            .get(0);
        owner
            .batch_execute(&password_statement)
            .await
            .expect("qualification role password setup failed");
    }

    let tenant = format!("history-{}", &Uuid::new_v4().simple().to_string()[..12]);
    let principal = Uuid::new_v4();
    let foreign_principal = Uuid::new_v4();
    let task_id = Uuid::new_v4();
    let logical_agent = format!("task-{}", task_id.simple());
    let state_run_id = format!("agent-{}", Uuid::new_v4().simple());
    let snapshot = json!({
        "harness":"pi","thinking":"off","route":"local","controller_profile":"controller",
        "gateway_profile":"gateway","image_ref":"registry.test/controller@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "cpu_limit":"500m","memory_limit":"512Mi","state_profile":"reference","role":"research",
        "tool_bundle":"reference","max_run_seconds":300,"max_provider_attempts":2
    });
    owner.execute("INSERT INTO enrichedlab_agent.broker_tenants(tenant_id,name) VALUES ($1,'History qualification')", &[&tenant]).await.unwrap();
    for (id, email) in [
        (principal, "owner@example.invalid"),
        (foreign_principal, "foreign@example.invalid"),
    ] {
        owner.execute("INSERT INTO enrichedlab_agent.broker_principals(tenant_id,principal_id,issuer,subject,email,principal_class) VALUES ($1,$2,'issuer',$3,$4,'person')", &[&tenant,&id,&id.to_string(),&email]).await.unwrap();
    }
    owner.execute("INSERT INTO enrichedlab_agent.broker_research_groups(tenant_id,group_key,name,max_active_agents,max_active_agents_per_principal,max_queued_tasks,retention_days) VALUES ($1,'research','Research',1,1,10,30)", &[&tenant]).await.unwrap();
    owner.execute("INSERT INTO enrichedlab_agent.broker_agent_profiles(tenant_id,profile_key,name,description,harness,route,controller_profile,gateway_profile,image_ref,cpu_limit,memory_limit,state_profile,role,tool_bundle,max_run_seconds,max_task_attempts,max_provider_attempts) VALUES ($1,'reference','Reference','Reference profile','pi','local','controller','gateway','registry.test/controller@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','500m','512Mi','reference','research','reference',300,3,2)", &[&tenant]).await.unwrap();
    owner.execute("INSERT INTO enrichedlab_agent.broker_tasks(tenant_id,task_id,principal_id,group_key,profile_key,objective,status,desired_status,attempt,max_task_attempts,logical_agent,profile_snapshot,idempotency_key,request_sha256,lease_owner,lease_generation,lease_expires_at,event_sequence) VALUES ($1,$2,$3,'research','reference','research objective must not leave broker storage','running','run',1,3,$4,$5,'history-lookup',repeat('a',64),'history-worker',1,clock_timestamp()+interval '5 minutes',11)", &[&tenant,&task_id,&principal,&logical_agent,&snapshot]).await.unwrap();

    let worker_dsn = dsn.replace("user=postgres", "user=broker_history_worker_qual");
    let api_dsn = dsn.replace("user=postgres", "user=broker_history_api_qual");
    let worker = Store::connect(&worker_dsn, tenant.clone(), 1).unwrap();
    let api = Store::connect(&api_dsn, tenant.clone(), 1).unwrap();
    let task = claimed_task(task_id, principal, &logical_agent);

    assert!(
        worker
            .bind_scientific_history(&task, "history-worker", &state_run_id, &state_run_id, 1)
            .await
            .unwrap()
    );
    let pending = api
        .scientific_history_page(principal, task_id, None, 1)
        .await
        .unwrap();
    assert_eq!(pending.coverage, "pending");
    assert!(pending.events.is_empty());

    owner.execute("INSERT INTO enrichedlab_agent.provenance_runs(tenant_id,run_id,subject,status,created_at,updated_at) VALUES ($1,$2,$3,'active',clock_timestamp(),clock_timestamp())", &[&tenant,&state_run_id,&state_run_id]).await.unwrap();
    let first_event = insert_event(&owner, &tenant, &state_run_id, 1, 1).await;
    insert_event(&owner, &tenant, &state_run_id, 2, 1).await;
    let first_artifact = Uuid::new_v4();
    let second_artifact = Uuid::new_v4();
    for (artifact, path, sha256) in [
        (
            first_artifact,
            "state-export/artifacts/sha256/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ),
        (
            second_artifact,
            "state-export/artifacts/sha256/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        ),
    ] {
        owner.execute("INSERT INTO enrichedlab_agent.broker_artifacts(tenant_id,artifact_id,task_id,visibility,role,label,relative_path,media_type,size_bytes,sha256) VALUES ($1,$2,$3,'researcher','state','Observed version',$4,'text/plain',4,$5)", &[&tenant,&artifact,&task_id,&path,&sha256]).await.unwrap();
    }
    for (artifact_id, sha256) in [
        (
            first_artifact,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ),
        (
            second_artifact,
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        ),
    ] {
        assert!(
            worker
                .record_scientific_history_evidence(
                    &task,
                    "history-worker",
                    &ScientificHistoryEvidenceInput {
                        event_id: first_event,
                        event_sequence: 1,
                        event_hash: &format!("{:064x}", 1),
                        snapshot_event_id: Uuid::new_v4(),
                        artifact_id,
                        artifact_sha256: sha256,
                        artifact_size_bytes: 4,
                        artifact_role: "result",
                    },
                )
                .await
                .unwrap()
        );
    }
    assert!(!worker
        .record_scientific_history_evidence(
            &task,
            "history-worker",
            &ScientificHistoryEvidenceInput {
                event_id: first_event,
                event_sequence: 1,
                event_hash: "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                snapshot_event_id: Uuid::new_v4(),
                artifact_id: first_artifact,
                artifact_sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                artifact_size_bytes: 4,
                artifact_role: "result",
            },
        )
        .await
        .unwrap());
    assert!(
        worker
            .finalize_scientific_history(&task, "history-worker", 1)
            .await
            .unwrap()
    );

    let first = api
        .scientific_history_page(principal, task_id, None, 1)
        .await
        .unwrap();
    assert_eq!(first.coverage, "available");
    assert_eq!(first.events.len(), 1);
    assert_eq!(first.events[0].sequence, 1);
    assert_eq!(first.events[0].evidence_count, 2);
    assert!(
        !serde_json::to_string(&first)
            .unwrap()
            .contains("metadata-secret")
    );
    assert!(
        !serde_json::to_string(&first)
            .unwrap()
            .contains("restricted secret")
    );
    let second = api
        .scientific_history_page(principal, task_id, Some((1, 1, 1)), 1)
        .await
        .unwrap();
    assert_eq!(second.events.len(), 1);
    assert_eq!(second.events[0].sequence, 2);

    let evidence_first = api
        .scientific_history_evidence_page(principal, task_id, 1, 1, first_event, None, 1)
        .await
        .unwrap();
    assert_eq!(evidence_first.evidence.len(), 1);
    let evidence_second = api
        .scientific_history_evidence_page(
            principal,
            task_id,
            1,
            1,
            first_event,
            Some(evidence_first.evidence[0].artifact_id),
            1,
        )
        .await
        .unwrap();
    assert_eq!(evidence_second.evidence.len(), 1);
    assert_ne!(
        evidence_first.evidence[0].artifact_id,
        evidence_second.evidence[0].artifact_id
    );
    assert!(matches!(
        api.scientific_history_evidence_page(foreign_principal, task_id, 1, 1, first_event, None, 1)
            .await,
        Err(error) if error.kind == StoreErrorKind::NotFound
    ));

    let foreign = api
        .scientific_history_page(foreign_principal, task_id, None, 1)
        .await;
    assert!(matches!(foreign, Err(error) if error.kind == StoreErrorKind::NotFound));

    owner.execute("UPDATE enrichedlab_agent.broker_tasks SET attempt=2,lease_generation=2,lease_expires_at=clock_timestamp()+interval '5 minutes' WHERE tenant_id=$1 AND task_id=$2", &[&tenant,&task_id]).await.unwrap();
    assert!(
        !worker
            .finalize_scientific_history(&task, "history-worker", 3)
            .await
            .unwrap()
    );
    let retry = claimed_task(task_id, principal, &logical_agent).with_attempt(2, 2);
    assert!(
        worker
            .bind_scientific_history(&retry, "history-worker", &state_run_id, &state_run_id, 2)
            .await
            .unwrap()
    );
    insert_event(&owner, &tenant, &state_run_id, 3, 2).await;
    assert!(
        worker
            .finalize_scientific_history(&retry, "history-worker", 2)
            .await
            .unwrap()
    );
    let current = api
        .scientific_history_page(principal, task_id, None, 10)
        .await
        .unwrap();
    assert_eq!(current.attempt, Some(2));
    assert_eq!(
        current
            .events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![3]
    );
    let prior = api
        .scientific_history_page(principal, task_id, Some((1, 1, 0)), 10)
        .await
        .unwrap();
    assert_eq!(prior.attempt, Some(1));
    assert_eq!(
        prior
            .events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    let stale = api
        .scientific_history_page(principal, task_id, Some((1, 2, 0)), 10)
        .await;
    assert!(matches!(stale, Err(error) if error.code == "scientific_history_cursor_stale"));

    let api_config: Config = api_dsn.parse().unwrap();
    let (limited, limited_connection) = api_config.connect(NoTls).await.unwrap();
    let limited_connection = tokio::spawn(limited_connection);
    assert!(
        limited
            .query("SELECT * FROM enrichedlab_agent.provenance_events", &[])
            .await
            .is_err()
    );
    let public: bool = owner.query_one("SELECT has_function_privilege('public', 'enrichedlab_agent.broker_scientific_history_status(text,uuid,uuid,integer,bigint)', 'EXECUTE')", &[]).await.unwrap().get(0);
    assert!(!public);
    drop(limited);
    drop(owner);
    limited_connection.abort();
    owner_connection.abort();
}

fn claimed_task(task_id: Uuid, principal_id: Uuid, logical_agent: &str) -> ClaimedTask {
    ClaimedTask {
        id: task_id,
        principal_id,
        group_key: "research".into(),
        profile_key: "reference".into(),
        objective: "not used".into(),
        logical_agent: logical_agent.into(),
        lease_generation: 1,
        attempt: 1,
        max_task_attempts: 3,
        max_provider_attempts: 2,
        harness: "pi".into(),
        thinking: Some("off".into()),
        route: "local".into(),
        controller_profile: "controller".into(),
        gateway_profile: "gateway".into(),
        image_ref: "registry.test/controller@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
        cpu_limit: "500m".into(),
        memory_limit: "512Mi".into(),
        state_profile: "reference".into(),
        role: "research".into(),
        max_run_seconds: 300,
    }
}

trait AttemptFixture {
    fn with_attempt(self, attempt: i32, lease_generation: i64) -> Self;
}

impl AttemptFixture for ClaimedTask {
    fn with_attempt(mut self, attempt: i32, lease_generation: i64) -> Self {
        self.attempt = attempt;
        self.lease_generation = lease_generation;
        self
    }
}

async fn insert_event(
    owner: &tokio_postgres::Client,
    tenant: &str,
    run_id: &str,
    sequence: i64,
    segment: i64,
) -> Uuid {
    let event_id = Uuid::new_v4();
    owner.execute("INSERT INTO enrichedlab_agent.provenance_events(tenant_id,run_id,sequence,event_id,subject,segment,harness,provider,model,engine,parser,session_id,kind,evidence_role,visibility,actor,occurred_at,metadata,restricted_content,content_sha256,previous_hash,hash) VALUES ($1,$2,$3,$4,$2,$5,'pi','provider','model','engine','parser','session','agent.tool.finished','tool-io','owner','controller','2026-09-10T00:00:00Z',$6,'restricted secret','sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',$7,$8)", &[&tenant,&run_id,&sequence,&event_id,&segment,&json!({"private":"metadata-secret"}), &format!("previous-{sequence}"),&format!("{sequence:064x}")]).await.unwrap();
    event_id
}
