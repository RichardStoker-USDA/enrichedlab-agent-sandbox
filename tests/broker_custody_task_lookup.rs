use enrichedlab_agent_sandbox::broker::store::Store;
use serde_json::json;
use std::{env, fs};
use tokio_postgres::{Config, NoTls, config::Host};
use uuid::Uuid;

#[tokio::test]
#[ignore = "uses the isolated PostgreSQL database with a private qualification DSN"]
async fn custody_lookup_returns_only_current_worker_lease_without_mutation() {
    let path = env::var_os("BROKER_QUALIFICATION_MIGRATION_DSN_FILE")
        .expect("private qualification DSN file required");
    let dsn = fs::read_to_string(path).expect("private qualification DSN file unreadable");
    let owner_config: Config = dsn.parse().expect("invalid qualification DSN");
    assert_eq!(owner_config.get_dbname(), Some("broker_qual"));
    assert_eq!(owner_config.get_user(), Some("postgres"));
    assert!(matches!(owner_config.get_hosts(), [Host::Tcp(host)] if host == "127.0.0.1"));
    let (owner, connection) = owner_config
        .connect(NoTls)
        .await
        .expect("qualification database unavailable");
    let owner_connection = tokio::spawn(connection);

    owner.batch_execute("CREATE ROLE broker_custody_qual LOGIN; GRANT USAGE ON SCHEMA enrichedlab_agent TO broker_custody_qual; GRANT EXECUTE ON FUNCTION enrichedlab_agent.broker_custody_task(text,uuid,text,integer,bigint) TO broker_custody_qual;")
        .await.expect("custody qualification role setup failed");
    let tenant = "custody-site";
    let principal = Uuid::new_v4();
    let task = Uuid::new_v4();
    let snapshot = json!({
        "harness":"pi","thinking":"off","route":"local","controller_profile":"controller",
        "gateway_profile":"gateway","image_ref":"registry.test/custodian@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "cpu_limit":"500m","memory_limit":"512Mi","state_profile":"reference","role":"research",
        "tool_bundle":"reference","max_run_seconds":300,"max_provider_attempts":1
    });
    owner.execute("INSERT INTO enrichedlab_agent.broker_tenants(tenant_id,name) VALUES ($1,'Custody qualification')", &[&tenant]).await.unwrap();
    owner.execute("INSERT INTO enrichedlab_agent.broker_principals(tenant_id,principal_id,issuer,subject,email,principal_class) VALUES ($1,$2,'issuer','subject','custody@example.invalid','person')", &[&tenant,&principal]).await.unwrap();
    owner.execute("INSERT INTO enrichedlab_agent.broker_research_groups(tenant_id,group_key,name,max_active_agents,max_active_agents_per_principal,max_queued_tasks,retention_days) VALUES ($1,'research','Research',1,1,10,30)", &[&tenant]).await.unwrap();
    owner.execute("INSERT INTO enrichedlab_agent.broker_agent_profiles(tenant_id,profile_key,name,description,harness,route,controller_profile,gateway_profile,image_ref,cpu_limit,memory_limit,state_profile,role,tool_bundle,max_run_seconds,max_task_attempts,max_provider_attempts) VALUES ($1,'reference','Reference','Reference profile','pi','local','controller','gateway','registry.test/custodian@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','500m','512Mi','reference','research','reference',300,3,1)", &[&tenant]).await.unwrap();
    owner.execute("INSERT INTO enrichedlab_agent.broker_tasks(tenant_id,task_id,principal_id,group_key,profile_key,objective,status,desired_status,attempt,max_task_attempts,logical_agent,profile_snapshot,idempotency_key,request_sha256,lease_owner,lease_generation,lease_expires_at,event_sequence) VALUES ($1,$2,$3,'research','reference','private objective must never reach custody','leased','run',2,3,$4,$5,'custody-lookup',repeat('a',64),'custody-worker',7,clock_timestamp()+interval '5 minutes',11)", &[&tenant,&task,&principal,&format!("task-{}",task.simple()),&snapshot]).await.unwrap();

    let worker_dsn = dsn.replace("user=postgres", "user=broker_custody_qual");
    let store = Store::connect(&worker_dsn, tenant.into(), 1).unwrap();
    let before: String = owner.query_one("SELECT row_to_json(t)::text FROM (SELECT objective,status,desired_status,lease_owner,lease_generation,attempt,event_sequence FROM enrichedlab_agent.broker_tasks WHERE tenant_id=$1 AND task_id=$2) AS t", &[&tenant,&task]).await.unwrap().get(0);
    let found = store
        .custody_task(task, "custody-worker", 2, 7)
        .await
        .unwrap()
        .expect("current lease missing");
    assert_eq!(found.id, task);
    assert_eq!(found.objective, "");
    assert_eq!(found.group_key, "research");
    assert_eq!(found.profile_key, "reference");
    assert_eq!(
        (found.attempt, found.lease_generation, found.max_run_seconds),
        (2, 7, 300)
    );
    assert_eq!(
        found.image_ref,
        "registry.test/custodian@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    );
    assert!(
        store
            .custody_task(task, "other-worker", 2, 7)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .custody_task(task, "custody-worker", 3, 7)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .custody_task(task, "custody-worker", 2, 8)
            .await
            .unwrap()
            .is_none()
    );
    let other_tenant = Store::connect(&worker_dsn, "other-site".into(), 1).unwrap();
    assert!(
        other_tenant
            .custody_task(task, "custody-worker", 2, 7)
            .await
            .unwrap()
            .is_none()
    );
    let after: String = owner
        .query_one(
            "SELECT row_to_json(t)::text FROM (SELECT objective,status,desired_status,lease_owner,lease_generation,attempt,event_sequence FROM enrichedlab_agent.broker_tasks WHERE tenant_id=$1 AND task_id=$2) AS t",
            &[&tenant, &task],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(before, after);

    owner.execute("UPDATE enrichedlab_agent.broker_tasks SET lease_expires_at=clock_timestamp()-interval '1 second' WHERE tenant_id=$1 AND task_id=$2", &[&tenant,&task]).await.unwrap();
    assert!(
        store
            .custody_task(task, "custody-worker", 2, 7)
            .await
            .unwrap()
            .is_none()
    );
    owner.execute("UPDATE enrichedlab_agent.broker_tasks SET lease_expires_at=clock_timestamp()+interval '5 minutes',desired_status='cancel' WHERE tenant_id=$1 AND task_id=$2", &[&tenant,&task]).await.unwrap();
    assert!(
        store
            .custody_task(task, "custody-worker", 2, 7)
            .await
            .unwrap()
            .is_none()
    );
    owner.execute("UPDATE enrichedlab_agent.broker_tasks SET desired_status='run',status='completed',lease_owner=NULL,lease_expires_at=NULL WHERE tenant_id=$1 AND task_id=$2", &[&tenant,&task]).await.unwrap();
    assert!(
        store
            .custody_task(task, "custody-worker", 2, 7)
            .await
            .unwrap()
            .is_none()
    );

    let worker_config: Config = worker_dsn.parse().unwrap();
    let (worker, connection) = worker_config.connect(NoTls).await.unwrap();
    let worker_connection = tokio::spawn(connection);
    assert!(
        worker
            .query("SELECT * FROM enrichedlab_agent.broker_tasks", &[])
            .await
            .is_err()
    );
    let public: bool = owner.query_one("SELECT has_function_privilege('public', 'enrichedlab_agent.broker_custody_task(text,uuid,text,integer,bigint)', 'EXECUTE')", &[]).await.unwrap().get(0);
    assert!(!public);
    drop(worker);
    drop(owner);
    worker_connection.abort();
    owner_connection.abort();
}
