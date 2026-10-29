use std::{env, fs, time::Duration};
use tokio_postgres::{Config, NoTls, config::Host};
use uuid::Uuid;

const RESOLVE: &str = "SELECT enrichedlab_agent.broker_resolve_principal('research-site', 'https://identity.example.invalid', 'concurrent-enrollment', 'researcher@example.invalid', '', 'person')";

#[tokio::test]
#[ignore = "uses isolated PostgreSQL in qualify_remote_broker.sh identity-enrollment"]
async fn concurrent_first_requests_share_one_principal_and_preserve_revocation() {
    let dsn = fs::read_to_string(
        env::var_os("BROKER_QUALIFICATION_MIGRATION_DSN_FILE").expect("private test DSN required"),
    )
    .expect("private test DSN unreadable");
    let config: Config = dsn.parse().unwrap_or_else(|_| panic!("invalid test DSN"));
    assert_eq!(config.get_dbname(), Some("broker_qual"));
    assert_eq!(config.get_user(), Some("postgres"));
    assert!(matches!(config.get_hosts(), [Host::Tcp(h)] if h == "127.0.0.1"));
    let (owner, connection) = config.connect(NoTls).await.unwrap();
    let owner_connection = tokio::spawn(connection);
    let mut api_config = config.clone();
    api_config.user("broker_api_qual");
    let (mut first, connection) = api_config.connect(NoTls).await.unwrap();
    let first_connection = tokio::spawn(connection);
    let (second, connection) = api_config.connect(NoTls).await.unwrap();
    let second_connection = tokio::spawn(connection);
    let pid: i32 = second
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .unwrap()
        .get(0);

    // Hold the first enrollment uncommitted. The old lookup-then-insert function
    // lets the second request miss that row and later fail its unique insert.
    let transaction = first.transaction().await.unwrap();
    let principal: Uuid = transaction.query_one(RESOLVE, &[]).await.unwrap().get(0);
    let contender = tokio::spawn(async move {
        let row = second.query_one(RESOLVE, &[]).await?;
        Ok::<Uuid, tokio_postgres::Error>(row.get(0))
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let blocked: bool = owner
                .query_one(
                    "SELECT coalesce(wait_event_type='Lock',false) FROM pg_stat_activity WHERE pid=$1",
                    &[&pid],
                )
                .await
                .unwrap()
                .get(0);
            if blocked {
                break;
            }
            assert!(!contender.is_finished(), "second request must overlap enrollment");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("second enrollment must reach the held transaction");
    transaction.commit().await.unwrap();
    assert_eq!(contender.await.unwrap().unwrap(), principal);
    let row = owner.query_one(
        "SELECT count(*) FROM enrichedlab_agent.broker_principals WHERE tenant_id='research-site' AND issuer='https://identity.example.invalid' AND subject='concurrent-enrollment'",
        &[],
    ).await.unwrap();
    assert_eq!(row.get::<_, i64>(0), 1);
    let consumed: Uuid = owner.query_one(
        "SELECT consumed_by FROM enrichedlab_agent.broker_principal_invitations WHERE tenant_id='research-site' AND email='researcher@example.invalid' AND group_key='pilot-lab'",
        &[],
    ).await.unwrap().get(0);
    assert_eq!(consumed, principal);
    let membership_count: i64 = owner.query_one(
        "SELECT count(*) FROM enrichedlab_agent.broker_group_memberships WHERE tenant_id='research-site' AND principal_id=$1",
        &[&principal],
    ).await.unwrap().get(0);
    assert_eq!(membership_count, 1);
    owner.execute(
        "UPDATE enrichedlab_agent.broker_principals SET enabled=false WHERE tenant_id='research-site' AND principal_id=$1",
        &[&principal],
    ).await.unwrap();
    let error = first.query_one(RESOLVE, &[]).await.unwrap_err();
    assert_eq!(error.code().unwrap().code(), "42501");
    assert_eq!(
        error.as_db_error().unwrap().message(),
        "broker:principal_disabled"
    );
    drop(first);
    drop(owner);
    first_connection.abort();
    second_connection.abort();
    owner_connection.abort();
}

/// The class comes from the authenticator. A service identity from another issuer
/// (SPIFFE here) is not a person, an unknown class is refused, a new identity without a class is
/// refused, and an identity never changes class.
#[tokio::test]
#[ignore = "uses isolated PostgreSQL in qualify_remote_broker.sh identity-enrollment"]
async fn principal_class_is_positive_and_unknown_is_denied() {
    let dsn = fs::read_to_string(
        env::var_os("BROKER_QUALIFICATION_MIGRATION_DSN_FILE").expect("private test DSN required"),
    )
    .expect("private test DSN unreadable");
    let config: Config = dsn.parse().unwrap_or_else(|_| panic!("invalid test DSN"));
    assert_eq!(config.get_dbname(), Some("broker_qual"));
    let (owner, connection) = config.connect(NoTls).await.unwrap();
    let owner_connection = tokio::spawn(connection);
    owner
        .batch_execute(
            "INSERT INTO enrichedlab_agent.broker_principal_invitations (tenant_id,email,group_key,role) VALUES \
             ('research-site','class-spiffe@example.invalid','pilot-lab','researcher'), \
             ('research-site','class-oidc@example.invalid','pilot-lab','researcher'), \
             ('research-site','class-unknown@example.invalid','pilot-lab','researcher'), \
             ('research-site','class-missing@example.invalid','pilot-lab','researcher')",
        )
        .await
        .unwrap();
    let mut api_config = config.clone();
    api_config.user("broker_api_qual");
    let (api, connection) = api_config.connect(NoTls).await.unwrap();
    let api_connection = tokio::spawn(connection);
    let resolve = |issuer: &'static str,
                   subject: &'static str,
                   email: &'static str,
                   class: Option<&'static str>| {
        let api = &api;
        async move {
            match class {
                Some(class) => api.query_one(
                    "SELECT enrichedlab_agent.broker_resolve_principal('research-site',$1,$2,$3,'',$4)",
                    &[&issuer, &subject, &email, &class],
                ).await,
                None => api.query_one(
                    "SELECT enrichedlab_agent.broker_resolve_principal('research-site',$1,$2,$3,'')",
                    &[&issuer, &subject, &email],
                ).await,
            }
        }
    };
    let refused = |result: Result<tokio_postgres::Row, tokio_postgres::Error>| {
        let error = result.unwrap_err();
        assert_eq!(error.code().unwrap().code(), "42501");
        error.as_db_error().unwrap().message().to_owned()
    };
    let person = |id: Uuid| {
        let owner = &owner;
        async move {
            owner
                .query_one(
                    "SELECT enrichedlab_agent.broker_is_person('research-site',$1)",
                    &[&id],
                )
                .await
                .unwrap()
                .get::<_, bool>(0)
        }
    };

    let workload: Uuid = resolve(
        "spiffe://site.example.invalid",
        "ns/tools/sa/indexer",
        "class-spiffe@example.invalid",
        Some("service_account"),
    )
    .await
    .unwrap()
    .get(0);
    assert!(
        !person(workload).await,
        "a service identity from another issuer is not a person"
    );
    let researcher: Uuid = resolve(
        "https://oidc.example.invalid",
        "oidc-person",
        "class-oidc@example.invalid",
        Some("person"),
    )
    .await
    .unwrap()
    .get(0);
    assert!(person(researcher).await);
    assert_eq!(
        refused(
            resolve(
                "https://robot.example.invalid",
                "robot",
                "class-unknown@example.invalid",
                Some("robot")
            )
            .await
        ),
        "broker:principal_class_unknown"
    );
    assert_eq!(
        refused(
            resolve(
                "https://identity.example.invalid",
                "no-class",
                "class-missing@example.invalid",
                None
            )
            .await
        ),
        "broker:principal_class_required"
    );
    // An API from before the class existed still resolves identities that already have one.
    let again: Uuid = resolve(
        "spiffe://site.example.invalid",
        "ns/tools/sa/indexer",
        "class-spiffe@example.invalid",
        None,
    )
    .await
    .unwrap()
    .get(0);
    assert_eq!(again, workload);
    assert_eq!(
        refused(
            resolve(
                "spiffe://site.example.invalid",
                "ns/tools/sa/indexer",
                "class-spiffe@example.invalid",
                Some("person")
            )
            .await
        ),
        "broker:principal_class_mismatch"
    );
    let unknown: i64 = owner
        .query_one("SELECT count(*) FROM enrichedlab_agent.broker_principals WHERE tenant_id='research-site' AND email IN ('class-unknown@example.invalid','class-missing@example.invalid')", &[])
        .await.unwrap().get(0);
    assert_eq!(unknown, 0, "a refused identity leaves no principal");
    let raw = owner
        .execute("INSERT INTO enrichedlab_agent.broker_principals(tenant_id,issuer,subject,email) VALUES ('research-site','https://x.example.invalid','raw','raw@example.invalid')", &[])
        .await
        .unwrap_err();
    assert_eq!(
        raw.code().unwrap().code(),
        "23502",
        "a row without a class cannot exist"
    );
    drop(api);
    drop(owner);
    api_connection.abort();
    owner_connection.abort();
}
