//! Tool groups granted to labs and each lab's tools (migration 080) in isolated PostgreSQL: the
//! seed stores a tool group version once and refuses one whose contents changed, a lab's profile grants are its direct
//! grants and the methods of the versions it holds, revoking a version removes only what nothing else holds, a rerun
//! changes nothing, stored versions and grants cannot be rewritten, and only a member reads the lab's tools.
use enrichedlab_agent_sandbox::broker::{
    config::SeedArgs,
    model::{PrincipalClass, VerifiedIdentity},
    store::{self, Store, StoreErrorKind},
};
use serde_json::{Value, json};
use std::{
    env, fs,
    path::{Path, PathBuf},
};
use tokio_postgres::{Client, Config, NoTls};
use uuid::Uuid;

fn person(subject: &str, email: &str) -> VerifiedIdentity {
    VerifiedIdentity {
        issuer: "https://identity.example.invalid".into(),
        class: PrincipalClass::Person,
        subject: subject.into(),
        email: email.into(),
        display_name: None,
    }
}

fn profile(key: &str, route: &str) -> Value {
    json!({"key": key, "name": format!("Method {key}"), "description": format!("The {key} method."),
        "harness": "open-code", "thinking": "on", "route": route, "controller_profile": "development",
        "gateway_profile": "kubernetes-kata",
        "image_ref": format!("ghcr.io/example/worker@sha256:{}", "a".repeat(64)),
        "cpu_limit": "2", "memory_limit": "4Gi", "state_profile": "scientific-worker",
        "role": "Research assistant.", "tool_bundle": "research-base", "max_run_seconds": 14400,
        "max_task_attempts": 3, "max_provider_attempts": 2})
}

/// The whole catalog with these direct grants, tool group versions and tool group grants.
fn catalog(grants: &[(&str, &str)], tool_groups: Value, tool_group_grants: Value) -> Value {
    json!({
        "schema_version": 1,
        "tenant": {"key": "research-site", "name": "Research site"},
        "groups": [
            {"key": "pilot-lab", "name": "Pilot lab", "max_active_agents": 3, "max_active_agents_per_principal": 2,
             "max_queued_tasks": 20, "retention_days": 30},
            {"key": "other-lab", "name": "Other lab", "max_active_agents": 1, "max_active_agents_per_principal": 1,
             "max_queued_tasks": 5, "retention_days": 30}],
        "profiles": [profile("general-research", "local-agent"), profile("count-tool", "server-query"),
                     profile("chart-tool", "server-render"), profile("extra-tool", "local-agent")],
        "grants": grants.iter().map(|(g, p)| json!({"group": g, "profile": p})).collect::<Vec<_>>(),
        "invitations": [
            {"email": "researcher@example.invalid", "group": "pilot-lab", "role": "researcher"},
            {"email": "outsider@example.invalid", "group": "other-lab", "role": "researcher"}],
        "capabilities": [
            {"email": "researcher@example.invalid", "group": "pilot-lab", "capability": "research.discovery.autonomous"}],
        "tool_groups": tool_groups,
        "tool_group_grants": tool_group_grants,
    })
}

async fn seed(dsn_file: &Path, value: &Value) -> anyhow::Result<()> {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("catalog.json");
    fs::write(&file, serde_json::to_vec(value).unwrap()).unwrap();
    store::seed(SeedArgs {
        file,
        migration_dsn_env: "AGENT_SANDBOX_BROKER_UNUSED_DSN".into(),
        migration_dsn_file: Some(dsn_file.to_path_buf()),
    })
    .await
}

async fn grants(owner: &Client, lab: &str) -> Vec<String> {
    owner
        .query(
            "SELECT profile_key FROM enrichedlab_agent.broker_group_profile_grants \
             WHERE tenant_id = 'research-site' AND group_key = $1 ORDER BY profile_key",
            &[&lab],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect()
}

/// Each tool group grant of the pilot lab: (id, key, version, still active).
async fn group_grants(owner: &Client) -> Vec<(i64, String, i32, bool)> {
    owner
        .query(
            "SELECT grant_id, tool_group_key, version, revoked_at IS NULL FROM enrichedlab_agent.broker_tool_group_grants \
             WHERE tenant_id = 'research-site' AND group_key = 'pilot-lab' ORDER BY grant_id",
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| (r.get(0), r.get(1), r.get(2), r.get(3)))
        .collect()
}

fn group(version: i32, title: &str, tools: &[&str]) -> Value {
    json!({"key": "core", "version": version, "title": title, "description": "Counting and charting the lab's tables.",
           "tools": tools})
}

fn tool<'a>(tools: &'a Value, profile: &str) -> &'a Value {
    tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["profile"] == profile)
        .unwrap_or_else(|| panic!("{profile} is listed"))
}

#[tokio::test]
#[ignore = "isolated PostgreSQL through qualify_remote_broker.sh tool-groups"]
async fn tool_group_versions_grant_methods_to_labs_and_members_read_them() {
    let dsn_file = PathBuf::from(env::var_os("BROKER_QUALIFICATION_MIGRATION_DSN_FILE").unwrap());
    let dsn = fs::read_to_string(&dsn_file).unwrap();
    let config: Config = dsn.parse().unwrap();
    assert_eq!(config.get_dbname(), Some("broker_qual"));
    let (owner, connection) = config.connect(NoTls).await.unwrap();
    tokio::spawn(connection);
    let api = Store::connect(
        &dsn.replace("user=postgres", "user=broker_api_qual"),
        "research-site".into(),
        2,
    )
    .unwrap();
    // The API does not serve until migration 080 and its grant exist.
    api.ready_for_review_api().await.unwrap();

    // Version 1 of a group holding two methods, granted to the pilot lab beside two direct grants, one of them also in
    // the group.
    let first = catalog(
        &[
            ("pilot-lab", "general-research"),
            ("pilot-lab", "count-tool"),
        ],
        json!([group(1, "Core tools", &["count-tool", "chart-tool"])]),
        json!([{"group": "pilot-lab", "tool_group": "core", "version": 1}]),
    );
    seed(&dsn_file, &first).await.unwrap();
    assert_eq!(
        grants(&owner, "pilot-lab").await,
        ["chart-tool", "count-tool", "general-research"]
    );
    assert!(grants(&owner, "other-lab").await.is_empty());
    let granted = group_grants(&owner).await;
    assert_eq!(granted.len(), 1);
    assert_eq!(
        (granted[0].1.as_str(), granted[0].2, granted[0].3),
        ("core", 1, true)
    );
    let stamps = |rows: Vec<tokio_postgres::Row>| {
        rows.iter()
            .map(|r| r.get::<_, String>(0))
            .collect::<Vec<_>>()
    };
    let stamp_query = "SELECT profile_key || '|' || created_at::text FROM enrichedlab_agent.broker_group_profile_grants \
                       WHERE tenant_id = 'research-site' ORDER BY profile_key";
    let before = stamps(owner.query(stamp_query, &[]).await.unwrap());
    // Every row the seed writes, with when each last changed: tenant, labs, methods, invitations, capabilities, and
    // once people have signed in, their principals and memberships.
    let rows_query = "SELECT 'tenant|' || tenant_id || '|' || updated_at::text FROM enrichedlab_agent.broker_tenants \
                      WHERE tenant_id = 'research-site' \
                      UNION ALL SELECT 'lab|' || group_key || '|' || updated_at::text \
                      FROM enrichedlab_agent.broker_research_groups WHERE tenant_id = 'research-site' \
                      UNION ALL SELECT 'method|' || profile_key || '|' || updated_at::text \
                      FROM enrichedlab_agent.broker_agent_profiles WHERE tenant_id = 'research-site' \
                      UNION ALL SELECT 'invitation|' || email || '|' || group_key || '|' || enabled || '|' || updated_at::text \
                      FROM enrichedlab_agent.broker_principal_invitations WHERE tenant_id = 'research-site' \
                      UNION ALL SELECT 'capability|' || email || '|' || group_key || '|' || enabled || '|' || updated_at::text \
                      FROM enrichedlab_agent.broker_capability_grants WHERE tenant_id = 'research-site' \
                      UNION ALL SELECT 'principal|' || email || '|' || enabled || '|' || updated_at::text \
                      FROM enrichedlab_agent.broker_principals WHERE tenant_id = 'research-site' \
                      UNION ALL SELECT 'membership|' || group_key || '|' || principal_id || '|' || role || '|' || updated_at::text \
                      FROM enrichedlab_agent.broker_group_memberships WHERE tenant_id = 'research-site' ORDER BY 1";
    let rows_before = stamps(owner.query(rows_query, &[]).await.unwrap());
    // One tenant, two labs, four methods, two invitations and one capability.
    assert_eq!(rows_before.len(), 10, "{}", rows_before.len());

    // The same catalog again writes nothing: no grant, tool group grant, tenant, lab, method, invitation or capability
    // row changes.
    seed(&dsn_file, &first).await.unwrap();
    assert_eq!(stamps(owner.query(stamp_query, &[]).await.unwrap()), before);
    assert_eq!(group_grants(&owner).await, granted);
    assert_eq!(
        stamps(owner.query(rows_query, &[]).await.unwrap()),
        rows_before
    );
    // A changed method description writes that method's row and no other.
    let mut described = first.clone();
    described["profiles"][1]["description"] = json!("The count-tool method, described again.");
    seed(&dsn_file, &described).await.unwrap();
    let rows_after = stamps(owner.query(rows_query, &[]).await.unwrap());
    let changed: Vec<&String> = rows_after
        .iter()
        .filter(|r| !rows_before.contains(r))
        .collect();
    assert_eq!(changed.len(), 1, "{changed:?}");
    assert!(changed[0].starts_with("method|count-tool|"));
    seed(&dsn_file, &first).await.unwrap();

    // A member reads the lab's tools: the group with its methods, and how each method is granted.
    let researcher = api
        .resolve_principal(&person("researcher", "researcher@example.invalid"))
        .await
        .unwrap();
    let outsider = api
        .resolve_principal(&person("outsider", "outsider@example.invalid"))
        .await
        .unwrap();
    // With principals and memberships present, a rerun still writes nothing.
    let signed_in = stamps(owner.query(rows_query, &[]).await.unwrap());
    assert_eq!(signed_in.len(), 14, "{}", signed_in.len());
    seed(&dsn_file, &first).await.unwrap();
    assert_eq!(
        stamps(owner.query(rows_query, &[]).await.unwrap()),
        signed_in
    );
    let tools = api.lab_tools(researcher, "pilot-lab").await.unwrap();
    assert_eq!(tools["lab"], "pilot-lab");
    assert_eq!(
        tools["tool_groups"],
        json!([{"key": "core", "version": 1, "title": "Core tools",
                "description": "Counting and charting the lab's tables.",
                "granted_at": tools["tool_groups"][0]["granted_at"], "tools": ["chart-tool", "count-tool"]}])
    );
    assert_eq!(tools["tools"].as_array().unwrap().len(), 3);
    assert_eq!(tool(&tools, "count-tool")["via_groups"], json!(["core"]));
    assert_eq!(tool(&tools, "count-tool")["kind"], "query_method");
    assert_eq!(tool(&tools, "chart-tool")["kind"], "render_method");
    assert_eq!(tool(&tools, "general-research")["via_groups"], json!([]));
    assert_eq!(tool(&tools, "general-research")["kind"], "sandbox_method");
    assert_eq!(tool(&tools, "general-research")["examples"], json!([]));
    // Catalog fields only.
    for listed in tools["tools"].as_array().unwrap() {
        let mut keys: Vec<&str> = listed
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert!(
            keys.iter().all(|k| [
                "configuration",
                "description",
                "examples",
                "kind",
                "name",
                "profile",
                "reviewed_queries",
                "route",
                "source",
                "template",
                "via_groups"
            ]
            .contains(k)),
            "{keys:?}"
        );
    }
    // Another lab's member, and a member asking about a lab they are not in, are refused alike.
    for (who, lab) in [
        (outsider, "pilot-lab"),
        (researcher, "other-lab"),
        (Uuid::new_v4(), "pilot-lab"),
    ] {
        let refused = api.lab_tools(who, lab).await.unwrap_err();
        assert_eq!(
            (refused.kind, refused.code),
            (StoreErrorKind::NotFound, "lab_not_found")
        );
    }

    // A stored version cannot change: the seed refuses, and nothing in the catalog transaction is kept.
    let changed = catalog(
        &[("pilot-lab", "general-research")],
        json!([group(
            1,
            "Renamed core tools",
            &["count-tool", "chart-tool"]
        )]),
        json!([{"group": "pilot-lab", "tool_group": "core", "version": 1}]),
    );
    let refused = seed(&dsn_file, &changed).await.unwrap_err();
    assert!(
        refused
            .to_string()
            .contains("tool group core version 1 is stored with other contents"),
        "{refused}"
    );
    let members = catalog(
        &[("pilot-lab", "general-research")],
        json!([group(1, "Core tools", &["count-tool"])]),
        json!([{"group": "pilot-lab", "tool_group": "core", "version": 1}]),
    );
    assert!(seed(&dsn_file, &members).await.is_err());
    assert_eq!(
        grants(&owner, "pilot-lab").await,
        ["chart-tool", "count-tool", "general-research"]
    );

    // Version 2 replaces version 1 for the lab and the direct grant of count-tool goes: count-tool leaves (nothing holds
    // it), extra-tool arrives, chart-tool stays. Version 1 stays stored and its grant is revoked, not deleted.
    let second = catalog(
        &[("pilot-lab", "general-research")],
        json!([
            group(1, "Core tools", &["count-tool", "chart-tool"]),
            group(2, "Core tools", &["chart-tool", "extra-tool"])
        ]),
        json!([{"group": "pilot-lab", "tool_group": "core", "version": 2}]),
    );
    seed(&dsn_file, &second).await.unwrap();
    assert_eq!(
        grants(&owner, "pilot-lab").await,
        ["chart-tool", "extra-tool", "general-research"]
    );
    let history = group_grants(&owner).await;
    assert_eq!(history.len(), 2);
    assert_eq!(
        (history[0].0, history[0].2, history[0].3),
        (granted[0].0, 1, false)
    );
    assert_eq!((history[1].2, history[1].3), (2, true));
    let stored: i64 = owner
        .query_one("SELECT count(*) FROM enrichedlab_agent.broker_tool_groups WHERE tenant_id = 'research-site'", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(stored, 2);

    // No group: a method the group alone held leaves, one with a direct grant stays.
    let third = catalog(
        &[
            ("pilot-lab", "general-research"),
            ("pilot-lab", "chart-tool"),
        ],
        json!([]),
        json!([]),
    );
    seed(&dsn_file, &third).await.unwrap();
    assert_eq!(
        grants(&owner, "pilot-lab").await,
        ["chart-tool", "general-research"]
    );
    assert!(group_grants(&owner).await.iter().all(|g| !g.3));
    let tools = api.lab_tools(researcher, "pilot-lab").await.unwrap();
    assert_eq!(tools["tool_groups"], json!([]));
    assert_eq!(tool(&tools, "chart-tool")["via_groups"], json!([]));

    // Granting the same version again adds a new grant; the earlier ones keep their revocation.
    seed(&dsn_file, &second).await.unwrap();
    let again = group_grants(&owner).await;
    assert_eq!(again.len(), 3);
    assert_eq!((again[2].2, again[2].3), (2, true));

    // Stored versions, their members and grants cannot be rewritten, deleted or truncated, even by the owner.
    for statement in [
        "UPDATE enrichedlab_agent.broker_tool_groups SET title = 'Other' WHERE tool_group_key = 'core'",
        "DELETE FROM enrichedlab_agent.broker_tool_groups WHERE tool_group_key = 'core'",
        "UPDATE enrichedlab_agent.broker_tool_group_members SET profile_key = 'general-research'",
        "DELETE FROM enrichedlab_agent.broker_tool_group_members",
        "UPDATE enrichedlab_agent.broker_tool_group_grants SET revoked_at = NULL WHERE revoked_at IS NOT NULL",
        "UPDATE enrichedlab_agent.broker_tool_group_grants SET version = 1 WHERE revoked_at IS NULL",
        "DELETE FROM enrichedlab_agent.broker_tool_group_grants",
        "TRUNCATE enrichedlab_agent.broker_tool_group_grants",
        "TRUNCATE enrichedlab_agent.broker_tool_group_members",
        "TRUNCATE enrichedlab_agent.broker_tool_groups CASCADE",
    ] {
        let error = owner.batch_execute(statement).await.unwrap_err();
        let message = error.as_db_error().map(|e| e.message().to_owned());
        assert_eq!(
            message.as_deref(),
            Some("broker:immutable_record"),
            "{statement}"
        );
    }
    // A second active version of a group for one lab is refused by the database itself.
    let error = owner
        .batch_execute(
            "INSERT INTO enrichedlab_agent.broker_tool_group_grants (tenant_id,group_key,tool_group_key,version) \
             VALUES ('research-site','pilot-lab','core',1)",
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.code(),
        Some(&tokio_postgres::error::SqlState::UNIQUE_VIOLATION)
    );

    // The API role reaches these tables only through the read.
    let mut api_config = config.clone();
    api_config.user("broker_api_qual");
    let (api_client, api_connection) = api_config.connect(NoTls).await.unwrap();
    tokio::spawn(api_connection);
    for table in [
        "broker_tool_groups",
        "broker_tool_group_members",
        "broker_tool_group_grants",
    ] {
        let error = api_client
            .query(&format!("SELECT 1 FROM enrichedlab_agent.{table}"), &[])
            .await
            .unwrap_err();
        assert_eq!(
            error.code(),
            Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE),
            "{table}"
        );
    }
    let callable: bool = api_client
        .query_one(
            "SELECT has_function_privilege('enrichedlab_agent.broker_lab_tool_json(text,uuid,text,\
             enrichedlab_agent.broker_agent_profiles)', 'EXECUTE')",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert!(
        !callable,
        "the per-method projection checks no reader and is not the API's to call"
    );

    // Leave the shared catalog as the qualification script seeded it.
    let example = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/examples/broker-catalog.yaml"
    ))
    .unwrap();
    let example: Value = serde_yaml_ng::from_str(&example).unwrap();
    seed(&dsn_file, &example).await.unwrap();
    // The example catalog lists only the researcher's pilot-lab invitation and no capability or tool group: the
    // outsider's invitation ends disabled, their principal disabled and their membership removed, the capability
    // revoked, the tool group grant revoked, and the researcher keeps access.
    let access = |sql: &'static str| {
        let owner = &owner;
        async move {
            owner
                .query(sql, &[])
                .await
                .unwrap()
                .iter()
                .map(|r| r.get::<_, String>(0))
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(
        access(
            "SELECT email || '|' || group_key || '|' || enabled FROM enrichedlab_agent.broker_principal_invitations \
             WHERE tenant_id = 'research-site' ORDER BY 1"
        )
        .await,
        [
            "outsider@example.invalid|other-lab|false",
            "researcher@example.invalid|pilot-lab|true"
        ]
    );
    assert_eq!(
        access(
            "SELECT email || '|' || enabled FROM enrichedlab_agent.broker_principals \
             WHERE tenant_id = 'research-site' ORDER BY 1"
        )
        .await,
        [
            "outsider@example.invalid|false",
            "researcher@example.invalid|true"
        ]
    );
    assert_eq!(
        access(
            "SELECT p.email || '|' || m.group_key FROM enrichedlab_agent.broker_group_memberships m \
             JOIN enrichedlab_agent.broker_principals p USING (tenant_id, principal_id) \
             WHERE m.tenant_id = 'research-site' ORDER BY 1"
        )
        .await,
        ["researcher@example.invalid|pilot-lab"]
    );
    assert_eq!(
        access(
            "SELECT email || '|' || enabled || '|' || (revoked_at IS NOT NULL) \
             FROM enrichedlab_agent.broker_capability_grants WHERE tenant_id = 'research-site' ORDER BY 1"
        )
        .await,
        ["researcher@example.invalid|false|true"]
    );
    assert!(group_grants(&owner).await.iter().all(|g| !g.3));
    let refused = api.lab_tools(outsider, "other-lab").await.unwrap_err();
    assert_eq!(
        (refused.kind, refused.code),
        (StoreErrorKind::NotFound, "lab_not_found")
    );
}
