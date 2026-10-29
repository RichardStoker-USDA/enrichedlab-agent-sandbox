//! Each lab's methods (migration 052) in isolated PostgreSQL: a person in two labs reads, for each, only the enabled
//! methods granted to that lab; a lab they do not belong to and a disabled method never appear; the API is not ready on
//! a schema without migration 052.
use enrichedlab_agent_sandbox::broker::{
    model::{PrincipalClass, VerifiedIdentity},
    store::Store,
};
use std::{env, fs};
use tokio_postgres::{Config, NoTls, config::Host};

#[tokio::test]
#[ignore = "isolated PostgreSQL through qualify_remote_broker.sh lab-methods"]
async fn each_lab_offers_only_its_own_enabled_methods() {
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

    owner
        .execute(
            "DELETE FROM enrichedlab_agent.schema_migrations WHERE version = 52",
            &[],
        )
        .await
        .unwrap();
    assert!(api.ready_for_review_api().await.is_err());
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (52)",
            &[],
        )
        .await
        .unwrap();
    api.ready_for_review_api().await.unwrap();

    let me = api
        .resolve_principal(&VerifiedIdentity {
            issuer: "https://identity.example.invalid".into(),
            class: PrincipalClass::Person,
            subject: "researcher".into(),
            email: "researcher@example.invalid".into(),
            display_name: None,
        })
        .await
        .unwrap();
    // A second lab the person belongs to, with its own method and a disabled one; a third lab they do not belong to.
    owner
        .batch_execute(&format!(
            "INSERT INTO enrichedlab_agent.broker_research_groups (tenant_id, group_key, name, max_active_agents, \
               max_active_agents_per_principal, max_queued_tasks, retention_days, enabled) VALUES \
               ('research-site', 'second-lab', 'Second lab', 2, 2, 8, 365, true), \
               ('research-site', 'other-lab', 'Other lab', 2, 2, 8, 365, true); \
             INSERT INTO enrichedlab_agent.broker_agent_profiles \
               SELECT (jsonb_populate_record(NULL::enrichedlab_agent.broker_agent_profiles, to_jsonb(p) || \
                 jsonb_build_object('profile_key', k.key, 'name', k.name, 'enabled', k.enabled))).* \
               FROM enrichedlab_agent.broker_agent_profiles p, \
                 (VALUES ('second-method', 'Second method', true), ('retired-method', 'Retired method', false)) AS k(key, name, enabled) \
               WHERE p.tenant_id = 'research-site' AND p.profile_key = 'general-research'; \
             INSERT INTO enrichedlab_agent.broker_group_profile_grants (tenant_id, group_key, profile_key) VALUES \
               ('research-site', 'second-lab', 'second-method'), ('research-site', 'second-lab', 'retired-method'), \
               ('research-site', 'other-lab', 'general-research'); \
             INSERT INTO enrichedlab_agent.broker_group_memberships (tenant_id, group_key, principal_id, role) VALUES \
               ('research-site', 'second-lab', '{me}', 'researcher');"
        ))
        .await
        .unwrap();

    let view = api.me(me).await.unwrap();
    let mut labs: Vec<(String, Vec<String>)> = view
        .groups
        .iter()
        .map(|g| (g.key.clone(), g.profiles.clone()))
        .collect();
    labs.sort();
    assert_eq!(
        labs,
        vec![
            ("pilot-lab".into(), vec!["general-research".into()]),
            ("second-lab".into(), vec!["second-method".into()]),
        ]
    );
    // The flat list keeps every enabled method of every lab, as before, for older clients.
    let mut all: Vec<String> = view.profiles.iter().map(|p| p.key.clone()).collect();
    all.sort();
    assert_eq!(
        all,
        vec!["general-research".to_string(), "second-method".to_string()]
    );
    drop(owner);
    connection.abort();
}
