//! A checked review survives a refused lead reply and reads back only through the lab-member API role.
use super::*;

pub(super) async fn qualify(
    api: &Store,
    owner: &tokio_postgres::Client,
    reader: Uuid,
    dir: &Path,
    args: CoordinatorArgs,
) {
    for (refuse, fail_lead) in [(false, true), (false, false), (true, false)] {
        scenario(api, owner, reader, dir, args.clone(), refuse, fail_lead).await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn scenario(
    api: &Store,
    owner: &tokio_postgres::Client,
    reader: Uuid,
    dir: &Path,
    mut args: CoordinatorArgs,
    refuse: bool,
    fail_lead: bool,
) {
    let document = Uuid::new_v4();
    let (run, project, _) = reading_run(
        api,
        reader,
        document,
        &hex::encode(Sha256::digest(document.as_bytes())),
    )
    .await;
    let url = fake_paper(document, Uuid::new_v4(), Arc::default(), Arc::default()).await;
    library(dir, &url);
    let url = study_reading::service(run, document, Arc::default()).await;
    knowledge_service(dir, &url);
    args.knowledge_config_file = Some(dir.join("knowledge.json"));
    args.lead_study_fields = vec!["software".into()];
    args.literature_review_labs = vec!["pilot-lab".into()];
    let ready_path = dir.join("coordinator.ready");
    args.ready_file = Some(ready_path.clone());
    let observed_ready = ready_path.clone();
    let bodies: Arc<Mutex<Vec<Value>>> = Arc::default();
    let kept = bodies.clone();
    let app = axum::Router::new().route("/v1/chat/completions", axum::routing::post(
        move |axum::Json(body): axum::Json<Value>| {
            assert!(observed_ready.is_file());
            let mut calls = kept.lock().unwrap(); calls.push(body.clone());
            let system = body["messages"][0]["content"].as_str().unwrap();
            let context: Value = serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
            let specialist = "You are a literature review specialist";
            let content = if system.starts_with(specialist) {
                let n = calls.iter().filter(|c| c["messages"][0]["content"].as_str().unwrap().starts_with(specialist)).count();
                match n {
                    1 => {
                        assert_eq!(context["study_summaries"][0]["fields"], json!({"software": 1}));
                        json!({"action": "read", "summary": "Read the outline.", "reads": [context["literature"][0]["paper_ref"], disclosure::REVIEW_MARKER]})
                    }
                    2 => {
                        assert_eq!(context["reading"]["refused"][0]["ref"], disclosure::REVIEW_MARKER);
                        json!({"action": "read", "summary": "Read the study.", "reads": [context["reading"]["parts"][0]["study"]["ref"]]})
                    },
                    3 => {
                        let study = context["reading"]["parts"].as_array().unwrap().iter().find(|p| p["part"] == "study").unwrap();
                        json!({"action": "read", "summary": "Read the section.", "reads": [study["facts"][0]["section"]]})
                    }
                    4 => json!({"approaches": [{"text": "Synthetic approach", "refs": ["library://unread/section/a"]}],
                        "methods_and_data": [], "expected": [], "pitfalls": [], "gaps": [], "papers": []}),
                    _ => {
                        assert_eq!(context["prior_attempt"]["names"], json!(["approaches 1: ref_unread"]));
                        let section = context["reading"]["parts"].as_array().unwrap().iter().find(|p| p["part"] == "section").unwrap();
                        let item = json!({"text": "Synthetic earlier approach", "refs": [if refuse { json!("library://unread/section/a") } else { section["ref"].clone() }]});
                        json!({"approaches": [item.clone()], "methods_and_data": [], "expected": [], "pitfalls": [], "gaps": [], "papers": [item]})
                    }
                }
            } else if system.starts_with("You are a methods specialist") {
                json!({"decision": "proceed", "reason": "Waiting for the scientist is sound."})
            } else {
                if refuse { assert!(context.get("literature_review").is_none()); }
                else { assert!(context["literature_review"]["review"]["approaches"].is_array()); }
                let n = calls.iter().filter(|c| c["messages"][0]["content"].as_str().unwrap().starts_with("You are a scientific research lead")).count();
                if n % 2 == 1 {
                    assert!(context.get("reading").is_none());
                    json!({"action": "read", "summary": "Read a synthetic part.", "reads": [disclosure::LEAD_MARKER]})
                } else {
                    assert_eq!(context["reading"]["refused"][0]["ref"], disclosure::LEAD_MARKER);
                    if fail_lead && n == 2 { json!({"action": "paused", "summary": ""}) } else { paused() }
                }
            };
            async move { axum::Json(json!({"model": "qualification-model", "choices": [{"message": {
                "content": content.to_string(), "reasoning_content": "Synthetic reasoning."}, "finish_reason": "stop"}]})) }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    args.model_config_file = dir.join("review_model.json");
    private(&args.model_config_file, &json!({"schema_version": 1, "endpoint": format!("http://127.0.0.1:{port}/v1/chat/completions"),
        "model": "qualification-model", "allow_internal_http": true, "max_input_bytes": 65536,
        "max_output_tokens": 1024, "context_window_tokens": 262144, "timeout_seconds": 30}).to_string());
    let mut bad = args.clone();
    bad.literature_review_labs = vec!["other-lab".into()];
    assert_eq!(
        enrichedlab_agent_sandbox::broker::coordinator::run(bad)
            .await
            .unwrap_err()
            .to_string(),
        "coordinator_config_invalid"
    );
    owner
        .execute(
            "DELETE FROM enrichedlab_agent.schema_migrations WHERE version=70",
            &[],
        )
        .await
        .unwrap();
    assert!(
        enrichedlab_agent_sandbox::broker::coordinator::run(args.clone())
            .await
            .is_err()
    );
    assert!(!ready_path.exists());
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.schema_migrations(version) VALUES(70)",
            &[],
        )
        .await
        .unwrap();
    enrichedlab_agent_sandbox::broker::coordinator::run(args.clone())
        .await
        .unwrap();
    assert!(!ready_path.exists());
    let row = owner.query_one("SELECT inputs_sha256 FROM enrichedlab_agent.broker_literature_reviews WHERE tenant_id='research-site' AND run_id=$1", &[&run]).await.unwrap();
    let digest: String = row.get(0);
    let artifact = if refuse {
        assert!(api.literature_review(reader, run, &digest).await.is_err());
        owner.query_one("SELECT record FROM enrichedlab_agent.broker_literature_reviews WHERE tenant_id='research-site' AND run_id=$1", &[&run]).await.unwrap().get::<_, Value>(0)
    } else {
        api.literature_review(reader, run, &digest).await.unwrap()
    };
    assert_eq!(artifact["reading"]["rounds"].as_array().unwrap().len(), 3);
    assert_eq!(artifact["specialist"].as_array().unwrap().len(), 5);
    for receipt in artifact["specialist"].as_array().unwrap() {
        for key in [
            "system_sha256",
            "context_sha256",
            "content_sha256",
            "reasoning_sha256",
        ] {
            let digest = receipt[key].as_str().unwrap();
            assert_eq!(
                api.research_context(reader, run, digest).await.unwrap()["sha256"],
                digest
            );
        }
    }
    if !fail_lead {
        let turns = api.research_turns(reader, run, 0, 10).await.unwrap();
        assert_eq!(
            turns[0]["record"]["literature_review"]["status"],
            if refuse { "refused" } else { "read" }
        );
        disclosure::check(api, owner, reader, run, project).await;
        // Resume the paused synthetic run without changing the inputs the review read.
        owner.execute("UPDATE enrichedlab_agent.broker_research_runs SET status='queued' WHERE tenant_id='research-site' AND run_id=$1", &[&run]).await.unwrap();
    }
    // A new invocation reuses either outcome, including after a failed first lead reply.
    owner.execute("UPDATE enrichedlab_agent.broker_research_runs SET available_at=clock_timestamp() WHERE tenant_id='research-site' AND run_id=$1", &[&run]).await.unwrap();
    enrichedlab_agent_sandbox::broker::coordinator::run(args)
        .await
        .unwrap();
    let turn: Value = owner.query_one("SELECT record FROM enrichedlab_agent.broker_research_turns WHERE tenant_id='research-site' AND run_id=$1 ORDER BY turn DESC LIMIT 1", &[&run]).await.unwrap().get(0);
    assert_eq!(
        turn["literature_review"]["status"],
        if refuse { "refused" } else { "kept" }
    );
    if refuse {
        assert_eq!(turn["literature_review"]["faults"], artifact["faults"]);
    }
    assert!(turn["literature_review"].get("review").is_none());
    assert_eq!(
        bodies
            .lock()
            .unwrap()
            .iter()
            .filter(|b| b["messages"][0]["content"]
                .as_str()
                .unwrap()
                .starts_with("You are a literature review specialist"))
            .count(),
        5
    );
    disclosure::check(api, owner, reader, run, project).await;
    let package = api.project_package_snapshot(reader, project).await.unwrap();
    assert_eq!(package["literature_reviews"].as_array().unwrap().len(), 1);
    assert_eq!(
        package["literature_reviews"][0]["status"],
        if refuse { "refused" } else { "read" }
    );
    if !refuse {
        assert!(
            enrichedlab_agent_sandbox::broker::literature_review::markdown(&artifact["review"])
                .contains("Synthetic earlier approach")
        );
    }
    // The project owner still has project access after lab membership is removed, but cannot read the review.
    let membership = owner.query_one("DELETE FROM enrichedlab_agent.broker_group_memberships WHERE tenant_id='research-site' AND principal_id=$1 AND group_key='pilot-lab' RETURNING role", &[&reader]).await.unwrap();
    let role: String = membership.get(0);
    assert!(api.literature_review(reader, run, &digest).await.is_err());
    owner.execute("INSERT INTO enrichedlab_agent.broker_group_memberships(tenant_id,principal_id,group_key,role) VALUES('research-site',$1,'pilot-lab',$2)", &[&reader,&role]).await.unwrap();
    if refuse {
        guards::qualify(api, owner, reader, run, project, &artifact).await;
    }
}

mod disclosure;
mod guards;
