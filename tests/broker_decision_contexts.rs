//! Kept decision texts (migration 032) in isolated PostgreSQL: the coordinator
//! keeps the exact texts a decision's models were given under its lease, by SHA-256; a project reader who
//! is a member of the lab reads back the same bytes; a text no decision of the run names is not served;
//! an administrator inspecting the project reads its turns but not the lab's texts; kept bytes never change. Each lead
//! reply is kept too (migration 044): a committed turn's receipt names it, and a deferral that
//! refused a readable reply names that reply and the context it answered.
use enrichedlab_agent_sandbox::broker::{
    coordinator::{StartRun, request_body, request_body_with},
    model::{PrincipalClass, VerifiedIdentity},
    project::{CheckpointRequest, CreateProject, ResearchState, Visibility},
    store::{Store, StoreError, StoreErrorKind},
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{env, fs};
use tokio_postgres::{Config, NoTls, config::Host};
use uuid::Uuid;

fn research() -> ResearchState {
    serde_json::from_value(json!({"schema_version":1,"goal":"Does the delivery method change tooth length?",
        "constraints":["Compute every value from the staged input."],
        "plan":[],"lessons":[],"next_action":"Run the analysis.",
        "workspace":{"schema_version":1,"profile":"general-research","image_digest":format!("sha256:{}","a".repeat(64)),
        "source_digest":format!("sha256:{}","b".repeat(64)),"input_refs":["input://public/toothgrowth"],
        "tool_refs":[],"preparation_receipt":null}})).unwrap()
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

fn code(e: StoreError) -> (StoreErrorKind, &'static str) {
    (e.kind, e.code)
}

#[tokio::test]
#[ignore = "isolated PostgreSQL through qualify_remote_broker.sh decision-contexts"]
async fn decisions_keep_their_exact_texts_for_lab_members_only() {
    let dsn = fs::read_to_string(env::var_os("BROKER_QUALIFICATION_MIGRATION_DSN_FILE").unwrap())
        .unwrap();
    let config: Config = dsn.parse().unwrap();
    assert_eq!(config.get_dbname(), Some("broker_qual"));
    assert!(matches!(config.get_hosts(),[Host::Tcp(h)] if h=="127.0.0.1"));
    let (owner, connection) = config.connect(NoTls).await.unwrap();
    let connection = tokio::spawn(connection);
    let store = |user: &str| {
        Store::connect(
            &dsn.replace("user=postgres", &format!("user={user}")),
            "research-site".into(),
            2,
        )
        .unwrap()
    };
    let api = store("broker_api_qual");
    let coordinator = store("broker_coordinator_qual");
    // Both stay out of service until migration 032 and their grants exist.
    coordinator.ready_for_research_results().await.unwrap();
    // The coordinator writes a refused methods reply's receipt that only migration 063's reader serves, so it is not
    // ready without 063.
    owner
        .batch_execute("DELETE FROM enrichedlab_agent.schema_migrations WHERE version = 63")
        .await
        .unwrap();
    assert_eq!(
        coordinator
            .ready_for_research_results()
            .await
            .unwrap_err()
            .code,
        "broker_results_schema_not_ready"
    );
    owner
        .batch_execute("INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (63)")
        .await
        .unwrap();
    coordinator.ready_for_research_results().await.unwrap();
    // The coordinator writes answer tables that only migration 064's results function returns to the next lead call, so the
    // coordinator is not ready without 064 either.
    owner
        .batch_execute("DELETE FROM enrichedlab_agent.schema_migrations WHERE version = 64")
        .await
        .unwrap();
    assert_eq!(
        coordinator
            .ready_for_research_results()
            .await
            .unwrap_err()
            .code,
        "broker_results_schema_not_ready"
    );
    owner
        .batch_execute("INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES (64)")
        .await
        .unwrap();
    coordinator.ready_for_research_results().await.unwrap();
    api.ready_for_review_api().await.unwrap();
    owner
        .batch_execute(
            "INSERT INTO enrichedlab_agent.broker_research_groups(tenant_id,group_key,name,max_active_agents,max_active_agents_per_principal,max_queued_tasks,retention_days) \
             VALUES ('research-site','administration','Administration',1,1,1,30); \
             INSERT INTO enrichedlab_agent.broker_principal_invitations(tenant_id,email,group_key,role) VALUES \
             ('research-site','context-owner@example.invalid','pilot-lab','researcher'), \
             ('research-site','context-colleague@example.invalid','pilot-lab','researcher'), \
             ('research-site','context-admin@example.invalid','administration','researcher');",
        )
        .await
        .unwrap();
    let person = |subject: &str, email: &str| VerifiedIdentity {
        issuer: "https://identity.example.invalid".into(),
        class: PrincipalClass::Person,
        subject: subject.into(),
        email: email.into(),
        display_name: None,
    };
    let me = api
        .resolve_principal(&person("context-owner", "context-owner@example.invalid"))
        .await
        .unwrap();
    let colleague = api
        .resolve_principal(&person(
            "context-colleague",
            "context-colleague@example.invalid",
        ))
        .await
        .unwrap();
    let admin = api
        .resolve_principal(&person("context-admin", "context-admin@example.invalid"))
        .await
        .unwrap();
    let project = Uuid::new_v4();
    api.create_project(
        me,
        &CreateProject {
            project_id: project,
            group: "pilot-lab".into(),
            title: "Kept decision texts".into(),
            visibility: Visibility::Private,
        },
    )
    .await
    .unwrap();
    api.checkpoint_project(
        me,
        project,
        &CheckpointRequest {
            expected_revision: 0,
            request_id: Uuid::new_v4(),
            state: research(),
        },
    )
    .await
    .unwrap();
    let run = api
        .start_research_run(
            me,
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
    assert_eq!(claim["run"]["run_id"], run.to_string());
    let generation = claim["run"]["generation"].as_i64().unwrap();

    // A refused lead reply is kept with the context it answered, and the deferral names both.
    let refused_context =
        r#"{"question":"Does the delivery method change tooth length?","prior_attempt":null}"#;
    let refused_reply = r#"{"action":"delegate","answer":"not_yet","summary":"Run it.","findings":[],"actions":[],"lessons":[]}"#;
    let refused_reasoning =
        "The question compares two delivery methods, so the first step is a mean by method.";
    let refused = coordinator
        .keep_research_contexts(
            run,
            "lead-a",
            generation,
            &[
                refused_context.as_bytes(),
                refused_reply.as_bytes(),
                refused_reasoning.as_bytes(),
            ],
        )
        .await
        .unwrap();
    let named = Some((
        refused[0].as_str(),
        refused[1].as_str(),
        Some(refused[2].as_str()),
    ));
    // Only a refused proposal names a reply, and only one whose bytes were kept; a refused yield keeps the lease.
    let yield_error = |e: StoreError| e.code;
    assert_eq!(
        yield_error(
            coordinator
                .yield_research_run_with_reply(
                    run,
                    "lead-a",
                    generation,
                    "model_unavailable",
                    Some("model_unavailable"),
                    named
                )
                .await
                .unwrap_err()
        ),
        "research_run_invalid"
    );
    let unkept = digest(b"a reply never kept");
    assert_eq!(
        yield_error(
            coordinator
                .yield_research_run_with_reply(
                    run,
                    "lead-a",
                    generation,
                    "proposal_invalid",
                    Some("proposal_action_invalid"),
                    Some((refused[0].as_str(), unkept.as_str(), None))
                )
                .await
                .unwrap_err()
        ),
        "context_not_kept"
    );
    // Reasoning never kept, or named without a reply, is refused too (migration 061).
    assert_eq!(
        yield_error(
            coordinator
                .yield_research_run_with_reply(
                    run,
                    "lead-a",
                    generation,
                    "proposal_invalid",
                    Some("proposal_action_invalid"),
                    Some((
                        refused[0].as_str(),
                        refused[1].as_str(),
                        Some(unkept.as_str())
                    ))
                )
                .await
                .unwrap_err()
        ),
        "context_not_kept"
    );
    let reasoning_alone: [&(dyn tokio_postgres::types::ToSql + Sync); 3] =
        [&run, &generation, &refused[2]];
    assert!(
        owner
            .query_one(
                "SELECT enrichedlab_agent.broker_yield_research_run('research-site',$1,'lead-a',$2,'proposal_invalid',\
                 'proposal_action_invalid',NULL::text,NULL::text,$3::text)",
                &reasoning_alone,
            )
            .await
            .is_err()
    );
    coordinator
        .yield_research_run_with_reply(
            run,
            "lead-a",
            generation,
            "proposal_invalid",
            Some("proposal_action_invalid"),
            named,
        )
        .await
        .unwrap();
    let events = api.research_run_events(me, run, 0, 10).await.unwrap();
    let deferral = events
        .iter()
        .find(|e| e["code"] == "proposal_invalid")
        .unwrap();
    assert_eq!(deferral["lead_context_sha256"], refused[0].as_str());
    assert_eq!(deferral["lead_reply_sha256"], refused[1].as_str());
    assert_eq!(deferral["lead_reasoning_sha256"], refused[2].as_str());
    assert_eq!(
        api.research_context(me, run, &refused[2]).await.unwrap()["text"],
        refused_reasoning
    );
    assert_eq!(
        api.research_context(me, run, &refused[1]).await.unwrap()["text"],
        refused_reply
    );
    assert_eq!(
        api.research_context(me, run, &refused[0]).await.unwrap()["text"],
        refused_context
    );
    // An event names both addresses or neither.
    assert!(
        owner
            .execute(
                "INSERT INTO enrichedlab_agent.broker_research_run_events(tenant_id,run_id,sequence,code,reason,generation,turn,failures,status_after,lead_context_sha256,lead_reply_sha256) \
                 VALUES ('research-site',$1,999,'proposal_invalid','proposal_action_invalid',1,1,1,'waiting',NULL,$2)",
                &[&run, &refused[1]],
            )
            .await
            .is_err()
    );
    // The run waits out its deferral; make it due and claim it again for the decision below.
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_research_runs SET available_at = clock_timestamp() WHERE run_id = $1",
            &[&run],
        )
        .await
        .unwrap();
    let claim = coordinator
        .claim_research_run("lead-a", 90)
        .await
        .unwrap()
        .unwrap();
    let generation = claim["run"]["generation"].as_i64().unwrap();

    // The texts a decision's models were given, kept by address under the lease.
    let instructions = "Fixed instructions for the research lead.";
    let context = r#"{"question":"Does the delivery method change tooth length?","literature":[{"text":"A passage from the lab's paper."}]}"#;
    let reply = r#"{"action":"review_required","answer":"not_yet","summary":"Ask the scientist.","findings":[],"actions":[],"lessons":[]}"#;
    let kept = coordinator
        .keep_research_contexts(
            run,
            "lead-a",
            generation,
            &[
                instructions.as_bytes(),
                context.as_bytes(),
                reply.as_bytes(),
            ],
        )
        .await
        .unwrap();
    assert_eq!(
        kept,
        vec![
            digest(instructions.as_bytes()),
            digest(context.as_bytes()),
            digest(reply.as_bytes())
        ]
    );
    // Keeping the same text again is harmless; a holder without the lease keeps nothing.
    assert_eq!(
        coordinator
            .keep_research_contexts(run, "lead-a", generation, &[context.as_bytes()])
            .await
            .unwrap(),
        vec![digest(context.as_bytes())]
    );
    assert!(
        coordinator
            .keep_research_contexts(run, "lead-b", generation, &[context.as_bytes()])
            .await
            .is_err()
    );
    // The request body rebuilds byte for byte from the kept texts and the receipt's settings, after
    // PostgreSQL's jsonb has stored the receipt (it reorders keys and rewrites numbers such as 1.0).
    let settings = json!({"temperature":1.0,"top_p":0.95,"top_k":64,"min_p":0.0,"reasoning_effort":"high",
        "chat_template_kwargs":{"enable_thinking":true,"reasoning_effort":"max"}});
    let settings = settings.as_object().unwrap();
    let body = request_body_with("research-lead", instructions, context, 4096, settings).unwrap();
    assert_ne!(
        body,
        request_body("research-lead", instructions, context, 4096).unwrap()
    );
    let never_kept = digest(b"never kept");
    // A methods reply refused and corrected in the same turn (migration 063) keeps its texts under its
    // own receipt.
    let (refused_context, refused_reply, refused_reasoning) = (
        "{\"question\":\"Does the delivery method change tooth length?\"}",
        "{\"decision\":\"revise\",\"reason\":\"Too long.\"}",
        "The detail ran past its limit.",
    );
    let refused_kept = coordinator
        .keep_research_contexts(
            run,
            "lead-a",
            generation,
            &[
                refused_context.as_bytes(),
                refused_reply.as_bytes(),
                refused_reasoning.as_bytes(),
            ],
        )
        .await
        .unwrap();
    let unlisted = coordinator
        .keep_research_contexts(
            run,
            "lead-a",
            generation,
            &[b"kept under no receipt".as_slice()],
        )
        .await
        .unwrap();
    let record = json!({"schema_version":1,
        "lead":{"system_sha256":kept[0],"context_sha256":kept[1],"content_sha256":kept[2],"request_sha256":digest(&body),
            "model_role":"research-lead","max_output_tokens":4096,"settings":settings},
        "methods_specialist":{"system_sha256":never_kept,"context_sha256":never_kept},
        "methods_specialist_refused":{"code":"method_review_findings_invalid",
            "receipt":{"system_sha256":never_kept,"context_sha256":refused_kept[0],"content_sha256":refused_kept[1],
                "reasoning_sha256":refused_kept[2]},"other_sha256":unlisted[0]},
        "proposal":{"answer":"not_yet","findings":[]}});
    coordinator
        .commit_research_turn(
            run,
            "lead-a",
            generation,
            claim["project"]["checkpoint"]["revision"].as_i64().unwrap(),
            &claim["project"]["checkpoint"]["state"],
            &json!([]),
            &record,
            "review_required",
        )
        .await
        .unwrap();

    let read = api.research_context(me, run, &kept[1]).await.unwrap();
    assert_eq!(read["text"], context);
    // The reply the receipt names reads back as it was received.
    assert_eq!(
        api.research_context(me, run, &kept[2]).await.unwrap()["text"],
        reply
    );
    assert_eq!(read["bytes"], context.len());
    assert_eq!(read["sha256"], kept[1].as_str());
    let turn = &api.research_turns(me, run, 0, 10).await.unwrap()[0];
    let system = api.research_context(me, run, &kept[0]).await.unwrap();
    let rebuilt = request_body_with(
        turn["record"]["lead"]["model_role"].as_str().unwrap(),
        system["text"].as_str().unwrap(),
        read["text"].as_str().unwrap(),
        turn["record"]["lead"]["max_output_tokens"]
            .as_u64()
            .unwrap() as usize,
        turn["record"]["lead"]["settings"].as_object().unwrap(),
    )
    .unwrap();
    assert_eq!(
        digest(&rebuilt),
        turn["record"]["lead"]["request_sha256"].as_str().unwrap()
    );

    // The refused methods reply's context, reply and reasoning read back by the addresses its receipt names; a text
    // kept under no receipt field the reader lists is not served.
    for (sha, text) in refused_kept
        .iter()
        .zip([refused_context, refused_reply, refused_reasoning])
    {
        assert_eq!(
            api.research_context(me, run, sha).await.unwrap()["text"],
            text
        );
    }
    assert_eq!(
        code(
            api.research_context(me, run, &unlisted[0])
                .await
                .unwrap_err()
        ),
        (StoreErrorKind::NotFound, "context_not_found")
    );
    assert_eq!(
        code(
            api.research_context(colleague, run, &refused_kept[0])
                .await
                .unwrap_err()
        ),
        (StoreErrorKind::NotFound, "project_not_found")
    );
    // Only texts a decision of this run names are served, and only when their bytes were kept.
    assert_eq!(
        code(
            api.research_context(me, run, &digest(b"unnamed"))
                .await
                .unwrap_err()
        ),
        (StoreErrorKind::NotFound, "context_not_found")
    );
    assert_eq!(
        code(
            api.research_context(me, run, &never_kept)
                .await
                .unwrap_err()
        ),
        (StoreErrorKind::NotFound, "context_not_kept")
    );
    // A lab member who cannot read this private project learns nothing about it.
    assert_eq!(
        code(
            api.research_context(colleague, run, &kept[1])
                .await
                .unwrap_err()
        ),
        (StoreErrorKind::NotFound, "project_not_found")
    );
    // An administrator inspecting the project reads its decisions, not the lab's texts.
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_project_administrators(tenant_id,principal_id) VALUES('research-site',$1)",
            &[&admin],
        )
        .await
        .unwrap();
    api.inspect_project(admin, project, "Quality review of retained evidence")
        .await
        .unwrap();
    assert_eq!(
        api.research_turns(admin, run, 0, 10).await.unwrap().len(),
        1
    );
    assert_eq!(
        code(
            api.research_context(admin, run, &kept[1])
                .await
                .unwrap_err()
        ),
        (StoreErrorKind::Forbidden, "context_lab_members_only")
    );
    // Kept bytes never change, and no runtime role reads the table directly.
    assert!(
        owner
            .execute(
                "UPDATE enrichedlab_agent.broker_research_contexts SET content = 'x'::bytea",
                &[]
            )
            .await
            .is_err()
    );
    for role in [
        "broker_api_qual",
        "broker_coordinator_qual",
        "broker_worker_qual",
    ] {
        let (direct, direct_connection) = dsn
            .replace("user=postgres", &format!("user={role}"))
            .parse::<Config>()
            .unwrap()
            .connect(NoTls)
            .await
            .unwrap();
        let direct_connection = tokio::spawn(direct_connection);
        assert!(
            direct
                .query(
                    "SELECT content FROM enrichedlab_agent.broker_research_contexts",
                    &[]
                )
                .await
                .is_err(),
            "{role}"
        );
        drop(direct);
        direct_connection.await.unwrap().unwrap();
    }
    drop(owner);
    connection.await.unwrap().unwrap();
}
