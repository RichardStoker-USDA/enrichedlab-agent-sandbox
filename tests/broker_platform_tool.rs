use enrichedlab_agent_sandbox::broker::{
    model::ClaimedTask, platform_tool::AcceptedPlatformJob, store::Store,
};
use serde_json::json;
use std::{env, fs};
use tokio_postgres::{Config, NoTls, config::Host};
use uuid::Uuid;

const MIGRATION: &str = include_str!("../migrations/postgres/008_broker_platform_tool_job.sql");

#[tokio::test]
#[ignore = "uses the isolated PostgreSQL database with a private qualification DSN"]
async fn platform_job_binding_is_exactly_fenced_and_idempotent() {
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
    owner
        .batch_execute(MIGRATION)
        .await
        .expect("migration 008 failed");
    owner.batch_execute(
        "DROP ROLE IF EXISTS broker_platform_worker_qual;
         CREATE ROLE broker_platform_worker_qual LOGIN;
         GRANT USAGE ON SCHEMA enrichedlab_agent TO broker_platform_worker_qual;
         GRANT EXECUTE ON FUNCTION enrichedlab_agent.broker_bind_platform_tool_job(text,uuid,text,integer,bigint,uuid,uuid,text,text,text,text,timestamptz) TO broker_platform_worker_qual;",
    ).await.expect("qualification role setup failed");

    let tenant = format!("platform-{}", &Uuid::new_v4().simple().to_string()[..12]);
    let principal = Uuid::new_v4();
    let task_id = Uuid::new_v4();
    let logical_agent = format!("task-{}", task_id.simple());
    let snapshot = json!({
        "harness":"pi","thinking":"off","route":"local","controller_profile":"controller",
        "gateway_profile":"gateway","image_ref":"registry.test/controller@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "cpu_limit":"500m","memory_limit":"512Mi","state_profile":"reference","role":"research",
        "tool_bundle":"reference","max_run_seconds":300,"max_provider_attempts":2
    });
    owner.execute("INSERT INTO enrichedlab_agent.broker_tenants(tenant_id,name) VALUES ($1,'Platform qualification')", &[&tenant]).await.unwrap();
    owner.execute("INSERT INTO enrichedlab_agent.broker_principals(tenant_id,principal_id,issuer,subject,email,principal_class) VALUES ($1,$2,'issuer',$3,'owner@example.invalid','person')", &[&tenant,&principal,&principal.to_string()]).await.unwrap();
    owner.execute("INSERT INTO enrichedlab_agent.broker_research_groups(tenant_id,group_key,name,max_active_agents,max_active_agents_per_principal,max_queued_tasks,retention_days) VALUES ($1,'research','Research',1,1,10,30)", &[&tenant]).await.unwrap();
    owner.execute("INSERT INTO enrichedlab_agent.broker_agent_profiles(tenant_id,profile_key,name,description,harness,route,controller_profile,gateway_profile,image_ref,cpu_limit,memory_limit,state_profile,role,tool_bundle,max_run_seconds,max_task_attempts,max_provider_attempts) VALUES ($1,'reference','Reference','Reference profile','pi','local','controller','gateway','registry.test/controller@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','500m','512Mi','reference','research','reference',300,3,2)", &[&tenant]).await.unwrap();
    owner.execute("INSERT INTO enrichedlab_agent.broker_tasks(tenant_id,task_id,principal_id,group_key,profile_key,objective,status,desired_status,attempt,max_task_attempts,logical_agent,profile_snapshot,idempotency_key,request_sha256,lease_owner,lease_generation,lease_expires_at,event_sequence) VALUES ($1,$2,$3,'research','reference','objective remains private','leased','run',1,3,$4,$5,'platform-binding',repeat('a',64),'platform-worker',1,clock_timestamp()+interval '5 minutes',0)", &[&tenant,&task_id,&principal,&logical_agent,&snapshot]).await.unwrap();

    let worker_dsn = dsn.replace("user=postgres", "user=broker_platform_worker_qual");
    let worker = Store::connect(&worker_dsn, tenant.clone(), 1).unwrap();
    let task = claimed_task(task_id, principal, logical_agent);
    let accepted = accepted_job();

    assert!(
        worker
            .bind_platform_tool_job(&task, "platform-worker", &accepted)
            .await
            .unwrap()
    );
    assert!(
        worker
            .bind_platform_tool_job(&task, "platform-worker", &accepted)
            .await
            .unwrap()
    );

    owner.execute("UPDATE enrichedlab_agent.broker_tasks SET attempt=2, lease_generation=2, lease_expires_at=clock_timestamp()+interval '5 minutes' WHERE tenant_id=$1 AND task_id=$2", &[&tenant,&task_id]).await.unwrap();
    assert!(
        !worker
            .bind_platform_tool_job(&task, "platform-worker", &accepted)
            .await
            .unwrap()
    );

    let worker_config: Config = worker_dsn.parse().unwrap();
    let (limited, limited_connection) = worker_config.connect(NoTls).await.unwrap();
    let limited_connection = tokio::spawn(limited_connection);
    assert!(
        limited
            .query(
                "SELECT * FROM enrichedlab_agent.broker_task_platform_tool_jobs",
                &[]
            )
            .await
            .is_err()
    );
    let public: bool = owner.query_one("SELECT has_function_privilege('public', 'enrichedlab_agent.broker_bind_platform_tool_job(text,uuid,text,integer,bigint,uuid,uuid,text,text,text,text,timestamptz)', 'EXECUTE')", &[]).await.unwrap().get(0);
    assert!(!public);

    drop(limited);
    drop(owner);
    limited_connection.abort();
    owner_connection.abort();
}

fn accepted_job() -> AcceptedPlatformJob {
    AcceptedPlatformJob {
        job_id: format!("job_{}", "a".repeat(26)),
        request_id: Uuid::new_v4(),
        conversation_id: Uuid::new_v4(),
        tool_id: "profile_columns".into(),
        catalog_digest: format!("sha256:{}", "b".repeat(64)),
        input_sha256: "c".repeat(64),
        accepted_at: "2026-09-10T06:25:00Z".into(),
    }
}

fn claimed_task(id: Uuid, principal_id: Uuid, logical_agent: String) -> ClaimedTask {
    ClaimedTask {
        id, principal_id, group_key: "research".into(), profile_key: "reference".into(), objective: String::new(),
        logical_agent, lease_generation: 1, attempt: 1, max_task_attempts: 3, max_provider_attempts: 2,
        harness: "pi".into(), thinking: None, route: "local".into(), controller_profile: "controller".into(),
        gateway_profile: "gateway".into(),
        image_ref: "registry.test/controller@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
        cpu_limit: "500m".into(), memory_limit: "512Mi".into(), state_profile: "reference".into(), role: "research".into(), max_run_seconds: 300,
    }
}
