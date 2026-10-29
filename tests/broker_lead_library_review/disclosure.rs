//! Invalid refs stay in member-only kept texts, including after a durable review is reused.
use super::*;

pub(super) const LEAD_MARKER: &str = "SYNTHETIC_PRIVATE_LEAD_REF_é";
pub(super) const REVIEW_MARKER: &str = "SYNTHETIC_PRIVATE_REVIEW_REF_é";

fn no_markers(value: &Value) {
    let text = value.to_string();
    for marker in [LEAD_MARKER, REVIEW_MARKER] {
        assert!(!text.contains(marker));
    }
}

pub(super) async fn check(
    api: &Store,
    owner: &tokio_postgres::Client,
    reader: Uuid,
    run: Uuid,
    project: Uuid,
) {
    let turns = api.research_turns(reader, run, 0, 10).await.unwrap();
    assert!(!turns.is_empty());
    let mut private_digests = Vec::new();
    for turn in &turns {
        let record = &turn["record"];
        no_markers(record);
        for (reading, marker) in [
            (&record["reading"], LEAD_MARKER),
            (&record["literature_review"]["reading"], REVIEW_MARKER),
        ] {
            let refused = &reading["rounds"][0]["refused"][0];
            assert!(refused.get("ref").is_none());
            assert_eq!(refused["code"], "read_ref_invalid");
            assert_eq!(
                refused["ref_sha256"],
                format!("sha256:{}", hex::encode(Sha256::digest(marker.as_bytes())))
            );
            assert_eq!(refused["ref_bytes"], marker.len());
            let digest = reading["rounds"][0]["lead"]["content_sha256"]
                .as_str()
                .unwrap();
            let kept = api.research_context(reader, run, digest).await.unwrap();
            assert!(kept["text"].as_str().unwrap().contains(marker));
            private_digests.push(digest.to_owned());
        }
        // Corrections with the original ref also survive in the next call's private context.
        for (receipt, marker) in [
            (&record["lead"], LEAD_MARKER),
            (&record["literature_review"]["specialist"][1], REVIEW_MARKER),
        ] {
            let digest = receipt["context_sha256"].as_str().unwrap();
            let kept = api.research_context(reader, run, digest).await.unwrap();
            assert!(kept["text"].as_str().unwrap().contains(marker));
            private_digests.push(digest.to_owned());
        }
    }
    no_markers(&api.project_package_snapshot(reader, project).await.unwrap());
    // Every turn record above is free of request text even for a member, so no reader of turns or packages sees it. A
    // project grant needs lab membership (`group_forbidden` otherwise) and losing membership loses the project, so the
    // other readers are a former member (refused below) and an administrator's inspection (after this block).
    owner
        .batch_execute(
            "INSERT INTO enrichedlab_agent.broker_research_groups(tenant_id,group_key,name,max_active_agents,
               max_active_agents_per_principal,max_queued_tasks,retention_days)
             VALUES ('research-site','disclosure-other-lab','Disclosure other lab',1,1,1,30) ON CONFLICT DO NOTHING;
             INSERT INTO enrichedlab_agent.broker_principal_invitations(tenant_id,email,group_key,role) VALUES
               ('research-site','reading-inspector@example.invalid','disclosure-other-lab','researcher')
             ON CONFLICT DO NOTHING;",
        )
        .await
        .unwrap();
    let membership = owner.query_one("DELETE FROM enrichedlab_agent.broker_group_memberships WHERE tenant_id='research-site' AND principal_id=$1 AND group_key='pilot-lab' RETURNING role", &[&reader]).await.unwrap();
    let role: String = membership.get(0);
    assert!(api.research_turns(reader, run, 0, 10).await.is_err());
    for digest in &private_digests {
        assert!(api.research_context(reader, run, digest).await.is_err());
    }
    owner.execute("INSERT INTO enrichedlab_agent.broker_group_memberships(tenant_id,principal_id,group_key,role) VALUES('research-site',$1,'pilot-lab',$2)", &[&reader, &role]).await.unwrap();

    let admin = api
        .resolve_principal(&VerifiedIdentity {
            issuer: "https://identity.example.invalid".into(),
            class: PrincipalClass::Person,
            subject: "reading-inspector".into(),
            email: "reading-inspector@example.invalid".into(),
            display_name: Some("Synthetic inspector".into()),
        })
        .await
        .unwrap();
    owner.execute("INSERT INTO enrichedlab_agent.broker_project_administrators(tenant_id,principal_id) VALUES('research-site',$1) ON CONFLICT DO NOTHING", &[&admin]).await.unwrap();
    api.inspect_project(admin, project, "Synthetic disclosure check")
        .await
        .unwrap();
    no_markers(&api.project_package_snapshot(admin, project).await.unwrap());
    for digest in &private_digests {
        assert!(api.research_context(admin, run, digest).await.is_err());
    }
}
