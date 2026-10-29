//! One delegated study read, from the outline through a fact to its section.
use super::*;

pub(super) async fn service(run: Uuid, document: Uuid, seen: Arc<Mutex<usize>>) -> String {
    let delegated = Arc::new(Mutex::new(false));
    let ready = delegated.clone();
    let key = enrichedlab_agent_sandbox::broker::paper::section_key(&["Results".into()], 1);
    let source_key = key.clone();
    // One section resource for every search: the knowledge service keeps a section's id, and a literature review is
    // reused only while the passages it was shown (refs built from that id) are unchanged.
    let section = Uuid::new_v4();
    let app = axum::Router::new()
        .route("/v1/runs/{run}/delegation", axum::routing::post(
            move |axum::extract::Path(id): axum::extract::Path<Uuid>, axum::Json(body): axum::Json<Value>| {
                assert_eq!(id, run);
                assert_eq!(body["scopes"], json!(["pilot-lab"]));
                *delegated.lock().unwrap() = true;
                async { axum::http::StatusCode::NO_CONTENT }
            }))
        .route("/v1/search", axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
            let hits = if body["kinds"] == json!(["section"]) { json!([{
                "resource": {"resource_id": section, "kind": "section", "scope": "pilot-lab",
                    "data_class": "paper", "title": "Synthetic section", "summary": "Synthetic passage.",
                    "source_uri": format!("parsenriched://lab-pilot-lab/document/{document}/section/{source_key}"),
                    "locator": {"heading_path": ["Results"]}}, "lexical_rank": 1, "dense_rank": null
            }]) } else { json!([]) };
            async move { axum::Json(json!({"query_sha256": "a".repeat(64), "hits": hits})) }
        }))
        .route("/v1/library/{corpus}/documents/{document}/study", axum::routing::get(
            move |axum::extract::Path((corpus, doc)): axum::extract::Path<(String, Uuid)>, headers: axum::http::HeaderMap| {
                assert!(*ready.lock().unwrap());
                assert_eq!(corpus, "lab-pilot-lab");
                assert_eq!(doc, document);
                assert_eq!(headers.get("x-knowledge-run").unwrap(), run.to_string().as_str());
                assert!(headers.contains_key("authorization"));
                *seen.lock().unwrap() += 1;
                let record = json!({"label": "model-extracted, quotes checked",
                    "provenance": {"extracted_at": "2026-01-01T00:00:00Z"}, "facts": [
                        {"field": "software", "value": "Synthetic method", "section": key, "quote": "Synthetic quote not to show"},
                        {"field": "result", "value": "Synthetic excluded value", "section": key}],
                    "links": [{"kind": "uses_tool", "assertion": "proposed", "label": "proposed, not reviewed",
                        "target_id": Uuid::nil(), "target_kind": "tool", "target_name": "Synthetic tool"}]});
                async move { axum::Json(record) }
            }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{address}/")
}

pub(super) async fn qualify(
    api: &Store,
    owner: &tokio_postgres::Client,
    reader: Uuid,
    dir: &Path,
    bodies: Arc<Mutex<Vec<Value>>>,
    script: Arc<Mutex<Script>>,
    mut args: CoordinatorArgs,
) {
    let document = Uuid::new_v4();
    let (run, _, paper) = reading_run(api, reader, document, &"1".repeat(64)).await;
    let url = fake_paper(document, Uuid::new_v4(), Arc::default(), Arc::default()).await;
    library(dir, &url);
    let seen = Arc::new(Mutex::new(0));
    let url = service(run, document, seen.clone()).await;
    knowledge_service(dir, &url);
    args.knowledge_config_file = Some(dir.join("knowledge.json"));
    args.lead_study_fields = vec!["software".into()];
    bodies.lock().unwrap().clear();
    *script.lock().unwrap() = Script::StudyRecord;
    enrichedlab_agent_sandbox::broker::coordinator::run(args)
        .await
        .unwrap();
    let turn: Value = owner.query_one(
        "SELECT record FROM enrichedlab_agent.broker_research_turns WHERE tenant_id='research-site' AND run_id=$1",
        &[&run]).await.unwrap().get(0);
    assert_eq!(turn["proposal"]["action"], "paused");
    assert_eq!(*seen.lock().unwrap(), 1);
    let calls = bodies.lock().unwrap().clone();
    let contexts: Vec<Value> = calls
        .iter()
        .filter(|b| {
            !b["messages"][0]["content"]
                .as_str()
                .unwrap()
                .starts_with("You are a methods specialist")
        })
        .map(|b| serde_json::from_str(b["messages"][1]["content"].as_str().unwrap()).unwrap())
        .collect();
    assert_eq!(contexts.len(), 4);
    assert_eq!(
        contexts[1]["reading"]["parts"][0]["study"]["fields"],
        json!({"software": 1})
    );
    let study = contexts[2]["reading"]["parts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["part"] == "study")
        .unwrap();
    assert_eq!(study["facts"].as_array().unwrap().len(), 1);
    assert_eq!(study["links"][0]["assertion"], "proposed");
    assert!(!study.to_string().contains("Synthetic quote not to show"));
    let section = study["facts"][0]["section"].as_str().unwrap();
    let expected = [
        (format!("library://{paper}"), "outline"),
        (format!("library://{paper}/study"), "study"),
        (section.to_owned(), "section"),
    ];
    let rounds = turn["reading"]["rounds"].as_array().unwrap();
    assert_eq!(rounds.len(), 3);
    for (i, (reference, kind)) in expected.iter().enumerate() {
        let receipt = &rounds[i]["shown"][0];
        assert_eq!(rounds[i]["shown"].as_array().unwrap().len(), 1);
        assert_eq!(receipt["ref"], *reference);
        assert_eq!(receipt["part"], *kind);
        let part = contexts[i + 1]["reading"]["parts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["ref"] == *reference)
            .unwrap();
        let bytes = serde_json::to_vec(part).unwrap();
        assert_eq!(receipt["bytes"], bytes.len());
        assert_eq!(
            receipt["part_sha256"],
            format!("sha256:{}", hex::encode(Sha256::digest(&bytes)))
        );
        assert_eq!(rounds[i]["refused"], json!([]));
        let digest = rounds[i]["lead"]["context_sha256"].as_str().unwrap();
        assert_eq!(
            api.research_context(reader, run, digest).await.unwrap()["sha256"],
            digest
        );
    }
    assert_eq!(turn["reading"]["held"].as_array().unwrap().len(), 3);
}
