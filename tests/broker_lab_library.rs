//! Lab library (migration 028) in isolated PostgreSQL: only lab members read or
//! change a lab's library; a paper is recorded once per lab by its bytes and restored when added
//! again after removal; a lab lead or whoever added a paper may remove it with a reason; labels
//! are typed, read as proposed until a person sets them, and only a person removes one; search
//! results resolve only papers still in the library; every row is immutable.
use enrichedlab_agent_sandbox::broker::{
    model::{PrincipalClass, VerifiedIdentity},
    store::{LibraryPaperRecord, Store, StoreError, StoreErrorKind},
};
use serde_json::{Value, json};
use std::{env, fs};
use tokio_postgres::{Config, NoTls, config::Host};
use uuid::Uuid;

fn code(e: StoreError) -> (StoreErrorKind, &'static str) {
    (e.kind, e.code)
}

fn record(document: Uuid, sha: &str) -> LibraryPaperRecord {
    LibraryPaperRecord {
        document_id: document,
        source_sha256: sha.into(),
        byte_size: 1000,
        title: Some("A synthetic paper about tooth length".into()),
        authors: json!(["Ada Example", "Ben Sample"]),
        year: Some(2024),
        doi: Some("10.5555/example.1".into()),
        page_count: Some(3),
        parse: json!({"state": "indexed", "chunks": 4, "figures": 1}),
    }
}

fn labels(paper: &Value) -> Vec<(String, String, String)> {
    paper["labels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| {
            (
                l["namespace"].as_str().unwrap().into(),
                l["value"].as_str().unwrap().into(),
                l["state"].as_str().unwrap().into(),
            )
        })
        .collect()
}

#[tokio::test]
#[ignore = "isolated PostgreSQL through qualify_remote_broker.sh lab-library"]
async fn a_lab_keeps_its_papers_labels_and_removals_to_itself() {
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
    // The API does not serve until migration 028 and its grants exist.
    api.ready_for_review_api().await.unwrap();
    owner
        .batch_execute(
            "INSERT INTO enrichedlab_agent.broker_research_groups(tenant_id,group_key,name,max_active_agents,
               max_active_agents_per_principal,max_queued_tasks,retention_days)
             VALUES ('research-site','other-lab','Other lab',1,1,1,30);
             INSERT INTO enrichedlab_agent.broker_principal_invitations(tenant_id,email,group_key,role) VALUES
               ('research-site','lead@example.invalid','pilot-lab','lead'),
               ('research-site','member@example.invalid','pilot-lab','researcher'),
               ('research-site','workload@example.invalid','pilot-lab','researcher'),
               ('research-site','outsider@example.invalid','other-lab','researcher');",
        )
        .await
        .unwrap();
    let person = |subject: &str, email: &str, name: &str| VerifiedIdentity {
        issuer: "https://identity.example.invalid".into(),
        class: PrincipalClass::Person,
        subject: subject.into(),
        email: email.into(),
        display_name: Some(name.into()),
    };
    let lead = api
        .resolve_principal(&person("lead", "lead@example.invalid", "Lab Lead"))
        .await
        .unwrap();
    let member = api
        .resolve_principal(&person("member", "member@example.invalid", "Member"))
        .await
        .unwrap();
    let outsider = api
        .resolve_principal(&person("outsider", "outsider@example.invalid", "Outsider"))
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

    // An empty library; a non-member and an unknown lab learn nothing.
    let empty = api.library(member, "pilot-lab").await.unwrap();
    assert_eq!(
        (
            empty["role"].clone(),
            empty["papers"].clone(),
            empty["removed"].clone()
        ),
        (json!("researcher"), json!([]), json!(0))
    );
    for (actor, lab) in [
        (outsider, "pilot-lab"),
        (member, "other-lab"),
        (member, "no-such-lab"),
    ] {
        assert_eq!(
            code(api.library(actor, lab).await.unwrap_err()),
            (StoreErrorKind::NotFound, "lab_not_found")
        );
    }

    // A service account adds a paper; the same request, and the same bytes from anyone, return it.
    let sha = "a".repeat(64);
    let document = Uuid::new_v4();
    let first = Uuid::new_v4();
    let added = api
        .library_record(workload, "pilot-lab", &record(document, &sha), first)
        .await
        .unwrap();
    assert_eq!(
        (added["created"].clone(), added["restored"].clone()),
        (json!(true), json!(false))
    );
    let paper = added["paper"].clone();
    let paper_id: Uuid = serde_json::from_value(paper["paper_id"].clone()).unwrap();
    assert_eq!(paper["added_by"]["kind"], "service_account");
    assert_eq!(paper["added_by"]["you"], true);
    assert_eq!(paper["can_remove"], true);
    assert_eq!(paper["title"], "A synthetic paper about tooth length");
    assert_eq!(paper["doi"], "10.5555/example.1");
    assert!(paper["removed"].is_null());
    let again = api
        .library_record(workload, "pilot-lab", &record(document, &sha), first)
        .await
        .unwrap();
    assert_eq!(
        (again["created"].clone(), again["paper"]["paper_id"].clone()),
        (json!(false), paper["paper_id"].clone())
    );
    let same_bytes = api
        .library_record(member, "pilot-lab", &record(document, &sha), Uuid::new_v4())
        .await
        .unwrap();
    assert_eq!(same_bytes["created"], false);
    assert_eq!(same_bytes["paper"]["paper_id"], paper["paper_id"]);
    assert_eq!(same_bytes["paper"]["can_remove"], false);
    assert_eq!(
        code(
            api.library_record(
                workload,
                "pilot-lab",
                &record(document, &"b".repeat(64)),
                first
            )
            .await
            .unwrap_err()
        ),
        (StoreErrorKind::Conflict, "idempotency_conflict")
    );
    // One document under other bytes, and fields out of bounds, are refused.
    assert_eq!(
        code(
            api.library_record(
                member,
                "pilot-lab",
                &record(document, &"c".repeat(64)),
                Uuid::new_v4()
            )
            .await
            .unwrap_err()
        ),
        (StoreErrorKind::Conflict, "library_conflict")
    );
    let mut bad = record(Uuid::new_v4(), &"d".repeat(64));
    bad.doi = Some("not-a-doi".into());
    assert_eq!(
        code(
            api.library_record(member, "pilot-lab", &bad, Uuid::new_v4())
                .await
                .unwrap_err()
        ),
        (StoreErrorKind::Invalid, "library_invalid")
    );
    let mut bad = record(Uuid::new_v4(), &"d".repeat(64));
    bad.year = Some(3000);
    assert_eq!(
        code(
            api.library_record(member, "pilot-lab", &bad, Uuid::new_v4())
                .await
                .unwrap_err()
        ),
        (StoreErrorKind::Invalid, "library_invalid")
    );
    assert_eq!(
        code(
            api.library_record(
                outsider,
                "pilot-lab",
                &record(Uuid::new_v4(), &"e".repeat(64)),
                Uuid::new_v4()
            )
            .await
            .unwrap_err()
        ),
        (StoreErrorKind::NotFound, "lab_not_found")
    );

    // Finding by bytes is for members of the lab only.
    let (found, found_document) = api
        .library_find(member, "pilot-lab", &sha)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (found["paper_id"].clone(), found_document),
        (paper["paper_id"].clone(), document)
    );
    assert!(
        api.library_find(member, "pilot-lab", &"f".repeat(64))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        code(
            api.library_find(outsider, "pilot-lab", &sha)
                .await
                .unwrap_err()
        ),
        (StoreErrorKind::NotFound, "lab_not_found")
    );

    // Labels: a service account's reads as proposed, a person's as set; only a person removes one.
    let label = |actor: Uuid,
                 kind: &'static str,
                 namespace: &'static str,
                 value: &'static str,
                 request: Uuid| {
        let api = &api;
        async move {
            api.library_label(
                actor,
                "pilot-lab",
                paper_id,
                kind,
                namespace,
                value,
                request,
            )
            .await
        }
    };
    let proposed = label(workload, "added", "topic", "tooth length", Uuid::new_v4())
        .await
        .unwrap();
    assert_eq!(
        labels(&proposed),
        vec![("topic".into(), "tooth length".into(), "proposed".into())]
    );
    label(workload, "added", "topic", "tooth length", Uuid::new_v4())
        .await
        .unwrap();
    let set = label(member, "added", "topic", "  tooth length ", Uuid::new_v4())
        .await
        .unwrap();
    assert_eq!(
        labels(&set),
        vec![("topic".into(), "tooth length".into(), "set".into())]
    );
    let events: i64 = owner
        .query_one(
            "SELECT count(*) FROM enrichedlab_agent.broker_library_label_events WHERE paper_id = $1",
            &[&paper_id],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(events, 2, "a repeated proposal appends nothing");
    let organism = Uuid::new_v4();
    label(member, "added", "organism", "Cavia porcellus", organism)
        .await
        .unwrap();
    // The same request returns the same answer; a different label under it is a conflict.
    label(member, "added", "organism", "Cavia porcellus", organism)
        .await
        .unwrap();
    assert_eq!(
        code(
            label(member, "added", "organism", "Mus musculus", organism)
                .await
                .unwrap_err()
        ),
        (StoreErrorKind::Conflict, "idempotency_conflict")
    );
    assert_eq!(
        code(
            label(workload, "removed", "topic", "tooth length", Uuid::new_v4())
                .await
                .unwrap_err()
        ),
        (StoreErrorKind::Forbidden, "label_person_only")
    );
    for (namespace, value) in [("colour", "blue"), ("topic", ""), ("topic", "a\u{7}b")] {
        assert_eq!(
            code(
                api.library_label(
                    member,
                    "pilot-lab",
                    paper_id,
                    "added",
                    namespace,
                    value,
                    Uuid::new_v4()
                )
                .await
                .unwrap_err()
            ),
            (StoreErrorKind::Invalid, "label_invalid"),
            "{namespace} {value:?}"
        );
    }
    let long = "x".repeat(81);
    assert_eq!(
        code(
            api.library_label(
                member,
                "pilot-lab",
                paper_id,
                "added",
                "topic",
                &long,
                Uuid::new_v4()
            )
            .await
            .unwrap_err()
        ),
        (StoreErrorKind::Invalid, "label_invalid")
    );
    let cleared = label(member, "removed", "topic", "tooth length", Uuid::new_v4())
        .await
        .unwrap();
    assert_eq!(
        labels(&cleared),
        vec![("organism".into(), "Cavia porcellus".into(), "set".into())]
    );
    // A service account's proposal after a removal reads as proposed again.
    let reproposed = label(workload, "added", "topic", "tooth length", Uuid::new_v4())
        .await
        .unwrap();
    assert!(labels(&reproposed).contains(&(
        "topic".into(),
        "tooth length".into(),
        "proposed".into()
    )));

    // Search resolves only the lab's papers still in the library.
    let other_document = Uuid::new_v4();
    let resolved = api
        .library_resolve(member, "pilot-lab", &[document, other_document])
        .await
        .unwrap();
    assert_eq!(resolved.as_array().unwrap().len(), 1);
    assert_eq!(resolved[0]["paper_id"], paper["paper_id"]);
    assert_eq!(resolved[0]["document_id"], json!(document));
    assert_eq!(
        code(
            api.library_resolve(outsider, "pilot-lab", &[document])
                .await
                .unwrap_err()
        ),
        (StoreErrorKind::NotFound, "lab_not_found")
    );
    let many: Vec<Uuid> = (0..101).map(|_| Uuid::new_v4()).collect();
    assert_eq!(
        code(
            api.library_resolve(member, "pilot-lab", &many)
                .await
                .unwrap_err()
        ),
        (StoreErrorKind::Invalid, "library_invalid")
    );

    // Removal: a lab lead or whoever added it, with a reason.
    assert_eq!(
        code(
            api.library_remove(member, "pilot-lab", paper_id, "Not ours.", Uuid::new_v4())
                .await
                .unwrap_err()
        ),
        (StoreErrorKind::Forbidden, "library_forbidden")
    );
    assert_eq!(
        code(
            api.library_remove(lead, "pilot-lab", paper_id, "   ", Uuid::new_v4())
                .await
                .unwrap_err()
        ),
        (StoreErrorKind::Invalid, "library_invalid")
    );
    let removal = Uuid::new_v4();
    let removed = api
        .library_remove(
            lead,
            "pilot-lab",
            paper_id,
            "Added to the wrong lab.",
            removal,
        )
        .await
        .unwrap();
    assert_eq!(removed["removed"]["reason"], "Added to the wrong lab.");
    assert_eq!(removed["removed"]["by"], "Lab Lead");
    api.library_remove(
        lead,
        "pilot-lab",
        paper_id,
        "Added to the wrong lab.",
        removal,
    )
    .await
    .unwrap();
    assert_eq!(
        code(
            api.library_remove(lead, "pilot-lab", paper_id, "Again.", Uuid::new_v4())
                .await
                .unwrap_err()
        ),
        (StoreErrorKind::Conflict, "paper_removed")
    );
    assert_eq!(
        code(
            label(member, "added", "method", "bootstrap", Uuid::new_v4())
                .await
                .unwrap_err()
        ),
        (StoreErrorKind::Conflict, "paper_removed")
    );
    let listed = api.library(member, "pilot-lab").await.unwrap();
    assert_eq!(
        (listed["papers"].clone(), listed["removed"].clone()),
        (json!([]), json!(1))
    );
    assert_eq!(
        api.library_resolve(member, "pilot-lab", &[document])
            .await
            .unwrap(),
        json!([])
    );
    let (kept, _) = api
        .library_paper(member, "pilot-lab", paper_id)
        .await
        .unwrap();
    assert_eq!(kept["removed"]["reason"], "Added to the wrong lab.");
    assert_eq!(
        code(
            api.library_paper(outsider, "pilot-lab", paper_id)
                .await
                .unwrap_err()
        ),
        (StoreErrorKind::NotFound, "lab_not_found")
    );
    assert_eq!(
        code(
            api.library_paper(member, "pilot-lab", Uuid::new_v4())
                .await
                .unwrap_err()
        ),
        (StoreErrorKind::NotFound, "paper_not_found")
    );

    // The same bytes again restore it, once per request.
    let restore = Uuid::new_v4();
    let restored = api
        .library_record(member, "pilot-lab", &record(document, &sha), restore)
        .await
        .unwrap();
    assert_eq!(
        (restored["created"].clone(), restored["restored"].clone()),
        (json!(false), json!(true))
    );
    assert!(restored["paper"]["removed"].is_null());
    let replay = api
        .library_record(member, "pilot-lab", &record(document, &sha), restore)
        .await
        .unwrap();
    assert_eq!(replay["restored"], true);
    let listed = api.library(member, "pilot-lab").await.unwrap();
    assert_eq!(listed["papers"].as_array().unwrap().len(), 1);
    assert_eq!(listed["removed"], 0);
    // Labels survive removal and restoration.
    assert!(labels(&listed["papers"][0]).contains(&(
        "organism".into(),
        "Cavia porcellus".into(),
        "set".into()
    )));

    // Rows are immutable, and the API role reaches them only through the functions.
    for statement in [
        "UPDATE enrichedlab_agent.broker_library_papers SET title = 'changed'",
        "DELETE FROM enrichedlab_agent.broker_library_label_events",
        "DELETE FROM enrichedlab_agent.broker_library_paper_events",
    ] {
        assert!(owner.batch_execute(statement).await.is_err(), "{statement}");
    }
    let api_config: Config = dsn
        .replace("user=postgres", "user=broker_api_qual")
        .parse()
        .unwrap();
    let (direct, direct_connection) = api_config.connect(NoTls).await.unwrap();
    let direct_connection = tokio::spawn(direct_connection);
    assert!(
        direct
            .query("SELECT * FROM enrichedlab_agent.broker_library_papers", &[])
            .await
            .is_err()
    );
    assert!(
        direct
            .query(
                "SELECT enrichedlab_agent.broker_library_member('research-site', $1, 'pilot-lab')",
                &[&member]
            )
            .await
            .is_err(),
        "helpers are not granted"
    );
    drop(direct);
    direct_connection.await.unwrap().unwrap();
    drop(owner);
    connection.await.unwrap().unwrap();
}
