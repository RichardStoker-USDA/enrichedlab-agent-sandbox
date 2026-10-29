use enrichedlab_agent_sandbox::broker::{
    coordinator::StartRun,
    evidence_package::plan_package,
    model::{PrincipalClass, VerifiedIdentity},
    package_registry::{DisclosureScope, PackageRevision, build_inventory, inventory_sha256},
    project::{
        Capability, CheckpointRequest, CreateProject, ProjectGrant, ResearchState, Visibility,
    },
    store::{Store, StoreErrorKind},
};
use serde_json::json;
use std::{env, fs};
use tokio_postgres::{Config, NoTls, config::Host};
use uuid::Uuid;

#[test]
fn migration_068_is_valid_postgresql_and_plpgsql() {
    let migration = include_str!("../migrations/postgres/068_project_package_revisions.sql");
    pg_query::parse(migration).unwrap();
    pg_query::parse_plpgsql(migration).unwrap();
    let ordinary_reader = migration
        .find("broker_project_package_scope(p_tenant,p_actor,p_project)")
        .unwrap();
    let reasoning_read = migration.find("IF may_read_reasoning THEN").unwrap();
    assert!(ordinary_reader < reasoning_read);
    for boundary in [
        "p.owner_id=p_actor",
        "p.visibility='lab'",
        "cardinality(grant_record.capabilities)>0",
        "disclosure_scope<>'ordinary_with_reasoning'",
        "PRIMARY KEY (tenant_id, project_id, disclosure_scope, revision)",
        "UNIQUE (tenant_id, project_id, disclosure_scope, content_sha256)",
        "AND content_sha256=p_content_sha256",
    ] {
        assert!(
            migration.contains(boundary),
            "missing package access boundary"
        );
    }
}

fn research() -> ResearchState {
    serde_json::from_value(json!({"schema_version":1,"goal":"Describe the retained public reference input.",
        "constraints":["Do not invent or retype input values."],"plan":[],"lessons":[],"next_action":"Select an available tool.",
        "workspace":{"schema_version":1,"profile":"reviewed-r","image_digest":format!("sha256:{}","a".repeat(64)),
        "source_digest":format!("sha256:{}","b".repeat(64)),"input_refs":["input://public/example"],
        "tool_refs":["tool://reviewed/summary"],"preparation_receipt":null}})).unwrap()
}

fn export_detail() -> serde_json::Value {
    json!({"package_sha256":"c".repeat(64),"manifest_sha256":"d".repeat(64),"bytes":1024,"files":9,
        "snapshot_at":"2026-09-23T06:00:00Z","format":"tar+gzip"})
}

#[tokio::test]
#[ignore = "isolated PostgreSQL through qualify_remote_broker.sh review"]
async fn review_package_administration_and_deferrals_keep_their_boundaries() {
    let dsn = fs::read_to_string(env::var_os("BROKER_QUALIFICATION_MIGRATION_DSN_FILE").unwrap())
        .unwrap();
    let config: Config = dsn.parse().unwrap();
    assert_eq!(config.get_dbname(), Some("broker_qual"));
    assert_eq!(config.get_user(), Some("postgres"));
    assert!(matches!(config.get_hosts(),[Host::Tcp(h)] if h=="127.0.0.1"));
    let (owner, connection) = config.connect(NoTls).await.unwrap();
    let connection = tokio::spawn(connection);
    owner.batch_execute("INSERT INTO enrichedlab_agent.broker_principal_invitations(tenant_id,email,group_key,role) VALUES
        ('research-site','collaborator@example.invalid','pilot-lab','researcher'),
        ('research-site','admin@example.invalid','pilot-lab','researcher'),
        ('research-site','workload@example.invalid','pilot-lab','researcher'),
        ('research-site','outsider@example.invalid','pilot-lab','researcher');").await.unwrap();
    let api_dsn = dsn.replace("user=postgres", "user=broker_api_qual");
    let api = Store::connect(&api_dsn, "research-site".into(), 4).unwrap();
    let coordinator_dsn = dsn.replace("user=postgres", "user=broker_coordinator_qual");
    let coordinator = Store::connect(&coordinator_dsn, "research-site".into(), 3).unwrap();
    let person = |subject: &str, email: &str| VerifiedIdentity {
        issuer: "https://identity.example.invalid".into(),
        class: PrincipalClass::Person,
        subject: subject.into(),
        email: email.into(),
        display_name: None,
    };
    let a = api
        .resolve_principal(&person("owner", "researcher@example.invalid"))
        .await
        .unwrap();
    let b = api
        .resolve_principal(&person("collaborator", "collaborator@example.invalid"))
        .await
        .unwrap();
    let admin = api
        .resolve_principal(&person("administrator", "admin@example.invalid"))
        .await
        .unwrap();
    let outsider = api
        .resolve_principal(&person("outsider", "outsider@example.invalid"))
        .await
        .unwrap();
    let workload = api
        .resolve_principal(&VerifiedIdentity {
            issuer: "kubernetes://serviceaccount".into(),
            class: PrincipalClass::ServiceAccount,
            subject: "system:serviceaccount:qual:caller".into(),
            email: "workload@example.invalid".into(),
            display_name: None,
        })
        .await
        .unwrap();
    // The outsider belongs to a different research group.
    owner.batch_execute("INSERT INTO enrichedlab_agent.broker_research_groups(tenant_id,group_key,name,max_active_agents,max_active_agents_per_principal,max_queued_tasks,retention_days) VALUES('research-site','other-lab','Other lab',3,2,20,30)").await.unwrap();
    owner.execute("DELETE FROM enrichedlab_agent.broker_group_memberships WHERE tenant_id='research-site' AND principal_id=$1",&[&outsider]).await.unwrap();
    owner.execute("INSERT INTO enrichedlab_agent.broker_group_memberships(tenant_id,group_key,principal_id,role) VALUES('research-site','other-lab',$1,'researcher')",&[&outsider]).await.unwrap();

    let project = Uuid::new_v4();
    api.create_project(
        a,
        &CreateProject {
            project_id: project,
            group: "pilot-lab".into(),
            title: "Retained review".into(),
            visibility: Visibility::Private,
        },
    )
    .await
    .unwrap();
    api.checkpoint_project(
        a,
        project,
        &CheckpointRequest {
            expected_revision: 0,
            request_id: Uuid::new_v4(),
            state: research(),
        },
    )
    .await
    .unwrap();

    // Two requests for one content inventory share a revision. A new inventory
    // advances exactly once even when both requests arrive together.
    let restricted_id = "reasoning:fixture-run/1/lead/restricted-address";
    let restricted_hash = "0123456789abcdef".repeat(4);
    let mut first_inventory = json!({"project:test": "a".repeat(64)});
    first_inventory[restricted_id] = json!(restricted_hash.clone());
    let first_digest = inventory_sha256(&first_inventory).unwrap();
    let (first, retry) = tokio::join!(
        api.register_project_package(
            a,
            project,
            DisclosureScope::OrdinaryWithReasoning,
            &first_digest,
            &first_inventory
        ),
        api.register_project_package(
            a,
            project,
            DisclosureScope::OrdinaryWithReasoning,
            &first_digest,
            &first_inventory
        ),
    );
    let first = first.unwrap();
    let retry = retry.unwrap();
    assert_eq!(first["revision"], 1);
    assert_eq!(first["disclosure_scope"], "ordinary_with_reasoning");
    assert_eq!(retry["revision"], 1);
    assert_eq!(first["created_at"], retry["created_at"]);
    let second_inventory = json!({"project:test": "c".repeat(64)});
    let second_digest = inventory_sha256(&second_inventory).unwrap();
    let (second, second_retry) = tokio::join!(
        api.register_project_package(
            a,
            project,
            DisclosureScope::OrdinaryWithReasoning,
            &second_digest,
            &second_inventory
        ),
        api.register_project_package(
            a,
            project,
            DisclosureScope::OrdinaryWithReasoning,
            &second_digest,
            &second_inventory
        ),
    );
    assert_eq!(second.unwrap()["revision"], 2);
    assert_eq!(second_retry.unwrap()["revision"], 2);
    let first_again = api
        .register_project_package(
            a,
            project,
            DisclosureScope::OrdinaryWithReasoning,
            &first_digest,
            &first_inventory,
        )
        .await
        .unwrap();
    assert_eq!(first_again["revision"], 1, "X, Y, X must reuse X");
    assert_eq!(first_again["created_at"], first["created_at"]);
    let registered: i64 = owner.query_one(
        "SELECT count(*) FROM enrichedlab_agent.broker_project_packages WHERE tenant_id='research-site' AND project_id=$1",
        &[&project],
    ).await.unwrap().get(0);
    assert_eq!(registered, 2);

    // The API is ready only with migration 014 and its grant; the worker check is unchanged.
    api.ready_for_review_api().await.unwrap();
    let worker = Store::connect(
        &dsn.replace("user=postgres", "user=broker_worker_qual"),
        "research-site".into(),
        1,
    )
    .unwrap();
    worker.ready().await.unwrap();
    assert_eq!(
        worker.ready_for_review_api().await.unwrap_err().code,
        "broker_review_schema_not_ready"
    );

    // Collaborator listing is owner-only and never offers a workload identity.
    let members = api.project_members(a, project).await.unwrap();
    let eligible: Vec<String> = members["eligible"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["principal_id"].as_str().unwrap().to_owned())
        .collect();
    assert!(eligible.contains(&b.to_string()) && eligible.contains(&admin.to_string()));
    assert!(
        !eligible.contains(&a.to_string())
            && !eligible.contains(&workload.to_string())
            && !eligible.contains(&outsider.to_string())
    );
    assert_eq!(
        api.project_members(b, project).await.unwrap_err().code,
        "project_not_found"
    );

    // Grant, read, revoke, denied: the package snapshot follows current access.
    api.grant_project(
        a,
        project,
        &ProjectGrant {
            principal_id: b,
            capabilities: vec![Capability::Read],
        },
    )
    .await
    .unwrap();
    let members = api.project_members(a, project).await.unwrap();
    assert_eq!(members["grants"][0]["principal_id"], b.to_string());
    assert!(
        !members["eligible"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["principal_id"] == b.to_string())
    );
    assert!(api.project_package_snapshot(b, project).await.is_ok());
    assert_eq!(
        api.project_package_snapshot(outsider, project)
            .await
            .unwrap_err()
            .code,
        "project_not_found"
    );
    assert_eq!(
        api.project_package_snapshot(admin, project)
            .await
            .unwrap_err()
            .code,
        "project_not_found"
    );
    api.grant_project(
        a,
        project,
        &ProjectGrant {
            principal_id: b,
            capabilities: vec![],
        },
    )
    .await
    .unwrap();
    assert_eq!(
        api.get_project(b, project).await.unwrap_err().code,
        "project_not_found"
    );
    assert_eq!(
        api.project_package_snapshot(b, project)
            .await
            .unwrap_err()
            .code,
        "project_not_found"
    );
    assert!(
        api.project_members(a, project).await.unwrap()["eligible"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["principal_id"] == b.to_string())
    );

    // Administration is a separate enrollment; a workload identity never qualifies.
    assert!(!api.is_project_administrator(admin).await.unwrap());
    let refused = api.admin_project_directory(admin, 50).await.unwrap_err();
    assert_eq!(
        (refused.kind, refused.code),
        (StoreErrorKind::Forbidden, "administrator_required")
    );
    owner.execute("INSERT INTO enrichedlab_agent.broker_project_administrators(tenant_id,principal_id) VALUES('research-site',$1),('research-site',$2)",&[&admin,&workload]).await.unwrap();
    assert!(api.is_project_administrator(admin).await.unwrap());
    assert!(!api.is_project_administrator(workload).await.unwrap());
    assert!(api.admin_project_directory(workload, 50).await.is_err());
    let directory = api.admin_project_directory(admin, 50).await.unwrap();
    let entry = directory
        .iter()
        .find(|p| p["project_id"] == project.to_string())
        .unwrap();
    assert_eq!(entry["readable"], false);
    assert!(entry["inspection_expires_at"].is_null());
    let audited: i64 = owner.query_one("SELECT count(*) FROM enrichedlab_agent.broker_admin_audit WHERE actor_id=$1 AND kind='project_directory_viewed'",&[&admin]).await.unwrap().get(0);
    assert_eq!(audited, 1);
    api.inspect_project(admin, project, "Quality review of retained evidence")
        .await
        .unwrap();
    let inspected = api.get_project(admin, project).await.unwrap();
    assert_eq!(inspected["capabilities"], json!(["read"]));
    assert!(!inspected["inspection_expires_at"].is_null());
    assert!(api.get_project(a, project).await.unwrap()["inspection_expires_at"].is_null());
    let snapshot = api.project_package_snapshot(admin, project).await.unwrap();
    assert_eq!(snapshot["requester"]["capabilities"], json!(["read"]));
    assert!(!snapshot["requester"]["inspection_expires_at"].is_null());
    assert_eq!(snapshot["reasoning_access"], false);
    assert_eq!(snapshot["reasoning"], json!([]));
    assert_eq!(snapshot["disclosure_scope"], "inspection");
    let inspection_inventory = build_inventory(&snapshot, &[]).unwrap();
    let inspection_digest = inventory_sha256(&inspection_inventory).unwrap();
    let wrong_scope = api
        .register_project_package(
            admin,
            project,
            DisclosureScope::OrdinaryWithReasoning,
            &inspection_digest,
            &inspection_inventory,
        )
        .await
        .unwrap_err();
    assert_eq!(wrong_scope.code, "project_package_scope_changed");
    let inspection_record = api
        .register_project_package(
            admin,
            project,
            DisclosureScope::Inspection,
            &inspection_digest,
            &inspection_inventory,
        )
        .await
        .unwrap();
    assert_eq!(inspection_record["revision"], 1);
    assert_eq!(inspection_record["disclosure_scope"], "inspection");
    assert!(inspection_record["previous_revision"].is_null());
    assert_eq!(inspection_record["previous_inventory"], json!({}));
    let inspection_revision =
        PackageRevision::from_record(&inspection_record, inspection_inventory).unwrap();
    let inspection_package = plan_package(&snapshot, Vec::new(), &inspection_revision).unwrap();
    let preview = json!({
        "disclosure_scope": inspection_package.manifest["disclosure_scope"],
        "contents": inspection_package.manifest["contents"],
        "changes_since": inspection_package.manifest["changes_since"],
    });
    for visible in [
        serde_json::to_string(&preview).unwrap(),
        serde_json::to_string(&inspection_package.manifest).unwrap(),
    ] {
        assert!(!visible.contains(restricted_id));
        assert!(!visible.contains(&restricted_hash));
    }
    assert!(api.end_project_inspection(admin, project).await.unwrap());
    assert!(!api.end_project_inspection(admin, project).await.unwrap());
    assert_eq!(
        api.get_project(admin, project).await.unwrap_err().code,
        "project_not_found"
    );
    let kinds: Vec<String> = api
        .project_audit(a, project, 50)
        .await
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap().to_owned())
        .collect();
    assert!(
        kinds.contains(&"admin_inspection".into())
            && kinds.contains(&"admin_inspection_ended".into())
    );

    // The owner's snapshot is complete for its record classes.
    let snapshot = api.project_package_snapshot(a, project).await.unwrap();
    assert_eq!(snapshot["reasoning_access"], true);
    assert_eq!(snapshot["checkpoints"].as_array().unwrap().len(), 1);
    assert!(
        snapshot["requester"]["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("manage"))
    );
    assert_eq!(snapshot["counts"]["tasks"], 0);

    // Export records are validated, attributed and denied without access.
    api.record_project_export(a, project, &export_detail())
        .await
        .unwrap();
    let mut incomplete = export_detail();
    incomplete
        .as_object_mut()
        .unwrap()
        .remove("manifest_sha256");
    assert_eq!(
        api.record_project_export(a, project, &incomplete)
            .await
            .unwrap_err()
            .code,
        "project_export_invalid"
    );
    let mut extra = export_detail();
    extra["note"] = json!("free text");
    assert_eq!(
        api.record_project_export(a, project, &extra)
            .await
            .unwrap_err()
            .code,
        "project_export_invalid"
    );
    assert_eq!(
        api.record_project_export(outsider, project, &export_detail())
            .await
            .unwrap_err()
            .code,
        "project_not_found"
    );
    assert!(
        api.project_audit(a, project, 50)
            .await
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "evidence_package_exported")
    );

    // Deferrals keep a fixed reason code and survive the failure counter reset.
    let run = api
        .start_research_run(
            a,
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
    let claim = coordinator
        .claim_research_run("lead-a", 90)
        .await
        .unwrap()
        .unwrap();
    let generation = claim["run"]["generation"].as_i64().unwrap();
    coordinator
        .yield_research_run(
            run,
            "lead-a",
            generation,
            "model_unavailable",
            Some("model_content_not_json"),
        )
        .await
        .unwrap();
    owner.execute("UPDATE enrichedlab_agent.broker_research_runs SET available_at=clock_timestamp()-interval '1 second' WHERE run_id=$1",&[&run]).await.unwrap();
    let claim = coordinator
        .claim_research_run("lead-a", 90)
        .await
        .unwrap()
        .unwrap();
    let generation = claim["run"]["generation"].as_i64().unwrap();
    // An upgraded database still serves a coordinator that sends no reason.
    let (legacy, legacy_connection) = coordinator_dsn
        .parse::<Config>()
        .unwrap()
        .connect(NoTls)
        .await
        .unwrap();
    let legacy_connection = tokio::spawn(legacy_connection);
    legacy.execute("SELECT enrichedlab_agent.broker_yield_research_run('research-site',$1,'lead-a',$2,'proposal_invalid')",&[&run,&generation]).await.unwrap();
    // Polling while a tool works records the first wait of a turn, not every poll.
    for _ in 0..2 {
        owner.execute("UPDATE enrichedlab_agent.broker_research_runs SET available_at=clock_timestamp()-interval '1 second' WHERE run_id=$1",&[&run]).await.unwrap();
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
    }
    let events = api.research_run_events(a, run, 0, 50).await.unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[2]["code"], "waiting_for_tools");
    assert_eq!(
        (events[0]["code"].as_str(), events[0]["reason"].as_str()),
        (Some("model_unavailable"), Some("model_content_not_json"))
    );
    assert!(events[1]["reason"].is_null());
    assert_eq!(events[1]["failures"], 2);
    assert_eq!(
        api.research_run_events(outsider, run, 0, 50)
            .await
            .unwrap_err()
            .code,
        "project_not_found"
    );
    assert_eq!(
        api.project_package_snapshot(a, project).await.unwrap()["research_runs"][0]["events"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    owner.execute("UPDATE enrichedlab_agent.broker_research_runs SET status='running',lease_owner='lead-a',lease_until=clock_timestamp()+interval '1 minute' WHERE run_id=$1",&[&run]).await.unwrap();
    let current: i64 = owner
        .query_one(
            "SELECT generation FROM enrichedlab_agent.broker_research_runs WHERE run_id=$1",
            &[&run],
        )
        .await
        .unwrap()
        .get(0);
    assert!(legacy.execute("SELECT enrichedlab_agent.broker_yield_research_run('research-site',$1,'lead-a',$2,'proposal_invalid','Provider said: bad JSON')",&[&run,&current]).await.is_err());

    // Records are append-only, and the API role has no direct table authority.
    assert!(owner.execute("UPDATE enrichedlab_agent.broker_research_run_events SET reason=NULL WHERE run_id=$1",&[&run]).await.is_err());
    assert!(
        owner
            .execute(
                "DELETE FROM enrichedlab_agent.broker_admin_audit WHERE actor_id=$1",
                &[&admin]
            )
            .await
            .is_err()
    );
    let (api_raw, api_connection) = api_dsn
        .parse::<Config>()
        .unwrap()
        .connect(NoTls)
        .await
        .unwrap();
    let api_connection = tokio::spawn(api_connection);
    assert!(api_raw.execute("INSERT INTO enrichedlab_agent.broker_admin_audit(tenant_id,actor_id,kind,detail) VALUES('research-site',$1,'forged','{}')",&[&admin]).await.is_err());
    assert!(
        api_raw
            .query(
                "SELECT * FROM enrichedlab_agent.broker_research_run_events",
                &[]
            )
            .await
            .is_err()
    );
    drop((api_raw, legacy, owner));
    for task in [connection, legacy_connection, api_connection] {
        task.await.unwrap().unwrap();
    }
}
