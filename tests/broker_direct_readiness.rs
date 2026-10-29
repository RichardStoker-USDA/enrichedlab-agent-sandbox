use enrichedlab_agent_sandbox::broker::store::{Store, StoreErrorKind};
use std::{env, fs};
use tokio_postgres::{Config, NoTls, config::Host};

#[tokio::test]
#[ignore = "uses the isolated PostgreSQL database in qualify_remote_broker.sh direct-readiness"]
async fn restricted_roles_check_readiness_before_direct_execution() {
    let path = env::var_os("BROKER_QUALIFICATION_MIGRATION_DSN_FILE")
        .expect("private qualification DSN file required");
    let dsn = fs::read_to_string(path).expect("private qualification DSN file unreadable");
    let owner_config: Config = dsn
        .parse()
        .unwrap_or_else(|_| panic!("invalid qualification DSN"));
    // This test changes only the disposable fixture's grants and migration rows.
    assert_eq!(owner_config.get_dbname(), Some("broker_qual"));
    assert_eq!(owner_config.get_user(), Some("postgres"));
    assert!(matches!(owner_config.get_hosts(), [Host::Tcp(host)] if host == "127.0.0.1"));
    let (owner, connection) = owner_config
        .connect(NoTls)
        .await
        .unwrap_or_else(|_| panic!("qualification database unavailable"));
    let owner_connection = tokio::spawn(connection);
    let worker_dsn = dsn.replace("user=postgres", "user=broker_worker_qual");
    let api_dsn = dsn.replace("user=postgres", "user=broker_api_qual");
    let worker = Store::connect(&worker_dsn, "example".into(), 2)
        .unwrap_or_else(|_| panic!("worker fixture connection invalid"));
    let api = Store::connect(&api_dsn, "example".into(), 2)
        .unwrap_or_else(|_| panic!("API fixture connection invalid"));
    worker.ready().await.unwrap();
    api.ready().await.unwrap();
    worker.ready_for_direct_execution().await.unwrap();
    assert_eq!(
        api.ready_for_direct_execution().await.unwrap_err().kind,
        StoreErrorKind::Unavailable
    );

    let api_config: Config = api_dsn
        .parse()
        .unwrap_or_else(|_| panic!("API fixture DSN invalid"));
    let (api_client, connection) = api_config
        .connect(NoTls)
        .await
        .unwrap_or_else(|_| panic!("API fixture connection failed"));
    let api_connection = tokio::spawn(connection);
    assert!(
        api_client
            .query(
                "SELECT version FROM enrichedlab_agent.schema_migrations",
                &[]
            )
            .await
            .is_err()
    );
    assert!(
        api_client
            .query("SELECT task_id FROM enrichedlab_agent.broker_tasks", &[])
            .await
            .is_err()
    );

    owner.batch_execute("REVOKE EXECUTE ON FUNCTION enrichedlab_agent.broker_record_runtime_event(text,uuid,text,bigint,text,jsonb) FROM broker_worker_qual").await.unwrap();
    assert_eq!(
        worker.ready_for_direct_execution().await.unwrap_err().code,
        "broker_direct_runtime_not_ready"
    );
    owner.batch_execute("GRANT EXECUTE ON FUNCTION enrichedlab_agent.broker_record_runtime_event(text,uuid,text,bigint,text,jsonb) TO broker_worker_qual").await.unwrap();
    // Without the attempt execution record (migration 019) a worker would start compute it cannot record.
    owner.batch_execute("REVOKE EXECUTE ON FUNCTION enrichedlab_agent.broker_record_attempt_execution(text,uuid,text,bigint,text,text,text,text,text) FROM broker_worker_qual").await.unwrap();
    assert_eq!(
        worker.ready_for_direct_execution().await.unwrap_err().code,
        "broker_direct_runtime_not_ready"
    );
    owner.batch_execute("GRANT EXECUTE ON FUNCTION enrichedlab_agent.broker_record_attempt_execution(text,uuid,text,bigint,text,text,text,text,text) TO broker_worker_qual").await.unwrap();
    owner
        .batch_execute("DELETE FROM enrichedlab_agent.schema_migrations WHERE version = 3")
        .await
        .unwrap();
    worker.ready().await.unwrap();
    assert_eq!(
        worker.ready_for_direct_execution().await.unwrap_err().code,
        "broker_direct_runtime_not_ready"
    );
    owner
        .batch_execute("INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (3)")
        .await
        .unwrap();
    worker.ready_for_direct_execution().await.unwrap();
    owner
        .batch_execute("DELETE FROM enrichedlab_agent.schema_migrations WHERE version = 2")
        .await
        .unwrap();
    assert_eq!(
        api.ready().await.unwrap_err().code,
        "broker_schema_not_ready"
    );
    owner
        .batch_execute("INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (2)")
        .await
        .unwrap();
    api.ready().await.unwrap();
    // Every worker path waits for migration 073, whose reader rule keeps a lab's smallest and largest values from readers
    // that are not persons, before it writes a digest with column summaries.
    owner
        .batch_execute("DELETE FROM enrichedlab_agent.schema_migrations WHERE version = 73")
        .await
        .unwrap();
    assert_eq!(
        worker.ready().await.unwrap_err().code,
        "broker_schema_not_ready"
    );
    owner
        .batch_execute("INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (73)")
        .await
        .unwrap();
    worker.ready().await.unwrap();

    drop(api_client);
    drop(owner);
    api_connection.abort();
    owner_connection.abort();
}
