use super::*;

#[test]
fn study_absence_is_distinct_from_refusal_or_an_invalid_answer() {
    assert!(matches!(
        study::answer(
            StatusCode::NOT_FOUND,
            br#"{"error":{"code":"study_record_not_found"}}"#
        ),
        Ok(None)
    ));
    assert!(matches!(
        study::answer(
            StatusCode::NOT_FOUND,
            br#"{"error":{"code":"route_not_found"}}"#
        ),
        Err(KnowledgeError::Invalid)
    ));
    for status in [StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN] {
        assert!(matches!(
            study::answer(status, b""),
            Err(KnowledgeError::Refused)
        ));
    }
    assert!(matches!(
        study::answer(StatusCode::SERVICE_UNAVAILABLE, b""),
        Err(KnowledgeError::Unavailable)
    ));
    assert!(matches!(
        study::answer(StatusCode::OK, b"{}"),
        Err(KnowledgeError::Invalid)
    ));
}

#[tokio::test]
async fn knowledge_response_bytes_are_bounded_without_a_content_length() {
    let response = |n| reqwest::Response::from(axum::http::Response::new(vec![b'x'; n]));
    assert_eq!(
        KnowledgeService::response(response(RESPONSE_LIMIT))
            .await
            .unwrap()
            .1
            .len(),
        RESPONSE_LIMIT
    );
    assert!(matches!(
        KnowledgeService::response(response(RESPONSE_LIMIT + 1)).await,
        Err(KnowledgeError::Invalid)
    ));
}

#[test]
fn an_answer_parses_only_when_every_hit_does() {
    let id = Uuid::new_v4();
    let good = json!({"query_sha256": "a".repeat(64), "dense_profile": "embeddinggemma-300m", "hits": [
        {"resource": {"resource_id": id, "kind": "analysis", "scope": "lab", "data_class": "prior_output",
                      "title": "t", "summary": "s",
                      "source_uri": "broker://x/task/y"}, "score": 0.03, "lexical_rank": 1, "dense_rank": null}]});
    let found = parse_found(&good).unwrap();
    assert_eq!(found.hits[0].id, id);
    assert_eq!(
        (found.hits[0].lexical_rank, found.hits[0].dense_rank),
        (Some(1), None)
    );
    let mut bad = good.clone();
    bad["hits"][0]["resource"]["resource_id"] = json!("not-a-uuid");
    assert_eq!(parse_found(&bad), Err(KnowledgeError::Invalid));
    let mut short = good;
    short["query_sha256"] = json!("abc");
    assert_eq!(parse_found(&short), Err(KnowledgeError::Invalid));
}

// A section's address names the lab's corpus, a document and a key the library ingest writes; anything else is
// not a section of this lab's library.
#[test]
fn a_part_hit_carries_its_named_paper_and_a_malformed_one_fails_the_answer() {
    let id = Uuid::new_v4();
    let paper = Uuid::new_v4();
    let answer = |p: Value| {
        json!({"query_sha256": "a".repeat(64), "dense_profile": null, "hits": [
            {"resource": {"resource_id": id, "kind": "section", "scope": "commons", "data_class": "paper",
                          "title": "Methods", "summary": "s"}, "score": 0.1, "lexical_rank": 1, "dense_rank": null,
             "paper": p}]})
    };
    let named = parse_found(&answer(json!({"resource_id": paper, "title": "A paper"}))).unwrap();
    assert_eq!(
        named.hits[0].paper,
        Some(HitPaper {
            id: paper,
            title: "A paper".into()
        })
    );
    assert_eq!(
        parse_found(&answer(Value::Null)).unwrap().hits[0].paper,
        None
    );
    let mut older = answer(Value::Null);
    older["hits"][0].as_object_mut().unwrap().remove("paper");
    assert_eq!(parse_found(&older).unwrap().hits[0].paper, None);
    assert_eq!(
        parse_found(&answer(
            json!({"resource_id": "not-a-uuid", "title": "A paper"})
        )),
        Err(KnowledgeError::Invalid)
    );
}

#[test]
fn any_corpus_section_address_names_its_corpus() {
    let doc = Uuid::new_v4();
    let uri = format!("parsenriched://literature-papers/document/{doc}/section/abcdef0123456789-2");
    assert_eq!(
        section_address_any(&uri),
        Some(("literature-papers".into(), doc, "abcdef0123456789-2".into()))
    );
    assert_eq!(
        section_address_any(&uri.replace("literature-papers", "Bad Corpus")),
        None
    );
    assert_eq!(
        section_address_any(&uri.replace("/section/", "/figure/")),
        None
    );
    assert_eq!(
        section_address_any("https://example.invalid/document/x/section/y"),
        None
    );
}

#[test]
fn a_section_address_names_the_corpus_document_and_key() {
    let doc = "7d0f5c1e-6b2a-4a55-9d7e-2f4b8e1c9a10";
    let uri = format!("parsenriched://lab-a/document/{doc}/section/0123456789abcdef-2");
    assert_eq!(
        section_address(&uri, "lab-a"),
        Some((doc.parse().unwrap(), "0123456789abcdef-2".to_owned()))
    );
    assert_eq!(section_address(&uri, "lab-b"), None);
    assert_eq!(
        section_address(&uri.replace("/section/", "/figure/"), "lab-a"),
        None
    );
    assert_eq!(
        section_address(&uri.replace(doc, "not-a-uuid"), "lab-a"),
        None
    );
    assert_eq!(section_address(&format!("{uri}/../x"), "lab-a"), None);
    assert_eq!(
        section_address(&uri.replace("-2", &"f".repeat(40)), "lab-a"),
        None
    );
    assert_eq!(section_address("broker://t/task/x", "lab-a"), None);
}

#[test]
fn a_locator_gives_pages_and_headings_in_words() {
    let at = json!({"heading_path": ["Methods", "Statistics"], "pages": {"from": 3, "to": 5}});
    assert_eq!(locator_pages(Some(&at)).as_deref(), Some("pages 3 to 5"));
    assert_eq!(
        locator_headings(Some(&at)).as_deref(),
        Some("Methods › Statistics")
    );
    let one = json!({"pages": {"from": 4, "to": 4}});
    assert_eq!(locator_pages(Some(&one)).as_deref(), Some("page 4"));
    assert_eq!(locator_pages(Some(&json!({"label": "Figure 1"}))), None);
    assert_eq!(locator_pages(None), None);
    assert_eq!(locator_headings(Some(&json!({"heading_path": []}))), None);
}

// A hit carries its record's data class and locator; a locator that is not an object puts the answer out of
// contract.
#[test]
fn a_hit_carries_its_data_class_and_locator() {
    let id = Uuid::new_v4();
    let answer = |locator: Value| {
        json!({"query_sha256": "a".repeat(64), "hits": [
            {"resource": {"resource_id": id, "kind": "section", "scope": "lab", "data_class": "paper",
                          "title": "Methods", "summary": "s", "source_uri": "parsenriched://c/document/d/section/k",
                          "locator": locator}, "score": 0.1, "lexical_rank": 1, "dense_rank": 2}]})
    };
    let found = parse_found(&answer(json!({"pages": {"from": 2, "to": 2}}))).unwrap();
    assert_eq!(found.hits[0].data_class, "paper");
    assert_eq!(
        found.hits[0].locator,
        Some(json!({"pages": {"from": 2, "to": 2}}))
    );
    assert_eq!(
        parse_found(&answer(Value::Null)).unwrap().hits[0].locator,
        None
    );
    assert_eq!(
        parse_found(&answer(json!(["Methods"]))),
        Err(KnowledgeError::Invalid)
    );
}

#[test]
fn the_search_text_is_the_questions_start_trimmed() {
    assert_eq!(search_text("  protein by food  "), "protein by food");
    assert_eq!(search_text(&"x".repeat(900)).chars().count(), QUERY_CHARS);
}

// Retrieval v2: a hit is its record, read through its best passage; a second passage of one section is
// left out, and a malformed passage fails the answer.
#[test]
fn a_retrieved_section_reads_as_its_matching_passage_once() {
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    let resource = |id: Uuid| {
        json!({"resource_id": id, "kind": "section", "scope": "lab", "data_class": "paper",
        "title": "Results", "summary": null, "source_uri": "parsenriched://lab-a/document/x/section/1"})
    };
    let passage = |body: &str| json!({"ordinal": 2, "context": "Paper › Results", "body": body, "char_from": 3700, "char_to": 4800});
    let answer = json!({"query_sha256": "b".repeat(64), "dense_profile": "nemotron-embed-vl-1b-v2", "generation": "g",
        "rerank_profile": null, "hits": [
        {"resource": resource(a), "paper": null, "passage": passage("deep words"), "score": 0.1,
         "ranks": {"lexical": 1, "dense": 3, "graph": null, "rerank": null}},
        {"resource": resource(a), "paper": null, "passage": passage("other words"), "score": 0.09,
         "ranks": {"lexical": 2, "dense": null, "graph": null, "rerank": null}},
        {"resource": resource(b), "paper": {"resource_id": Uuid::new_v4(), "title": "P"}, "passage": null, "score": 0.05,
         "ranks": {"lexical": null, "dense": 1, "graph": 2, "rerank": null}}]});
    let found = parse_retrieved(&answer).unwrap();
    assert_eq!(found.hits.iter().map(|h| h.id).collect::<Vec<_>>(), [a, b]);
    assert_eq!(found.hits[0].summary, "Paper › Results\n\ndeep words");
    assert_eq!(
        (found.hits[0].lexical_rank, found.hits[0].dense_rank),
        (Some(1), Some(3))
    );
    assert_eq!(
        found.hits[1].summary, "",
        "a record hit without a passage keeps its own summary"
    );
    assert!(found.hits[1].paper.is_some());
    let mut bad = answer.clone();
    bad["hits"][0]["passage"] = json!({"ordinal": 2});
    assert_eq!(parse_retrieved(&bad), Err(KnowledgeError::Invalid));
    let mut bad = answer;
    bad["hits"][0]["passage"] = json!("text");
    assert_eq!(parse_retrieved(&bad), Err(KnowledgeError::Invalid));
}

#[test]
fn a_retrieval_generation_name_is_checked_where_the_config_is_read() {
    let dir = std::env::temp_dir().join(format!("knowledge-config-{}", Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let write = |extra: Value| {
        let mut config = json!({"schema": SCHEMA, "service_url": "http://knowledge.research-knowledge.svc.cluster.local:8080",
            "token_file": "/run/knowledge-token/token", "labs": ["example-lab"]});
        for (k, v) in extra.as_object().unwrap() {
            config[k] = v.clone();
        }
        let path = dir.join("search.json");
        std::fs::write(&path, config.to_string()).unwrap();
        load_optional(Some(&path))
    };
    assert!(write(json!({})).unwrap().unwrap().generation.is_none());
    assert_eq!(
        write(json!({"retrieval_generation": "v2-nemotron-bm25-graph-1"}))
            .unwrap()
            .unwrap()
            .generation
            .as_deref(),
        Some("v2-nemotron-bm25-graph-1")
    );
    assert!(write(json!({"retrieval_generation": "V2 bad"})).is_err());
    assert!(write(json!({"retrieval_generation": ""})).is_err());
    // A lab's own generation replaces the coordinator's for that lab only; `null` keeps that lab on search v1.
    let two_labs = |extra: Value| {
        let mut extra = extra;
        extra["labs"] = json!(["example-lab", "other-lab"]);
        write(extra)
    };
    let service =
        two_labs(json!({"lab_retrieval_generations": {"example-lab": "v2-nemotron-bm25-graph-1"}}))
            .unwrap()
            .unwrap();
    assert_eq!(
        (
            service.generation_for("example-lab"),
            service.generation_for("other-lab")
        ),
        (Some("v2-nemotron-bm25-graph-1"), None)
    );
    let service = two_labs(json!({"retrieval_generation": "v2-nemotron-bm25-graph-1",
        "lab_retrieval_generations": {"other-lab": null}}))
    .unwrap()
    .unwrap();
    assert_eq!(
        (
            service.generation_for("example-lab"),
            service.generation_for("other-lab")
        ),
        (Some("v2-nemotron-bm25-graph-1"), None)
    );
    // A lab the file does not serve, or a bad name, refuses the whole file.
    assert!(
        two_labs(json!({"lab_retrieval_generations": {"third-lab": "v2-nemotron-bm25-graph-1"}}))
            .is_err()
    );
    assert!(two_labs(json!({"lab_retrieval_generations": {"example-lab": "V2 bad"}})).is_err());
    assert!(two_labs(json!({"lab_retrieval_generations": {"example-lab": ""}})).is_err());
    assert!(two_labs(json!({"lab_retrieval_generations": ["example-lab"]})).is_err());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Each retrieval the fake service received: its body and its run header.
type Retrievals = std::sync::Arc<std::sync::Mutex<Vec<(Value, Option<String>)>>>;

/// A knowledge service whose delegation answers `delegation` and whose retrieval answers `answer`, keeping each
/// retrieval's body and run header; search v1 is refused, so a call that takes the wrong route fails.
async fn fake_retrieval(delegation: u16, answer: Value, seen: Retrievals) -> String {
    let app = axum::Router::new()
        .route(
            "/v1/runs/{run}/delegation",
            axum::routing::post(move || async move {
                axum::http::StatusCode::from_u16(delegation).unwrap()
            }),
        )
        .route(
            "/v1/search",
            axum::routing::post(|| async { axum::http::StatusCode::INTERNAL_SERVER_ERROR }),
        )
        .route(
            "/v1/retrieve",
            axum::routing::post(
                move |headers: axum::http::HeaderMap, axum::Json(body): axum::Json<Value>| {
                    let run = headers
                        .get("x-knowledge-run")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_owned);
                    seen.lock().unwrap().push((body, run));
                    let answer = answer.clone();
                    async move { axum::Json(answer) }
                },
            ),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{address}/")
}

fn retrieval_service(dir: &std::path::Path, url: &str) -> KnowledgeService {
    use std::os::unix::fs::PermissionsExt;
    let token = dir.join("token");
    std::fs::write(&token, "knowledge-token\n").unwrap();
    std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
    let config = dir.join("search.json");
    std::fs::write(
        &config,
        json!({"schema": SCHEMA, "service_url": url, "token_file": token, "labs": ["pilot-lab"],
        "retrieval_generation": "v2-nemotron-bm25-graph-1"})
        .to_string(),
    )
    .unwrap();
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
    load_optional(Some(&config)).unwrap().unwrap()
}

// The retrieval route, its body and run header, the section it names, and no retrieval after a
// refused delegation.
#[tokio::test]
async fn a_configured_generation_retrieves_sections_under_the_runs_delegation() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (run, section) = (Uuid::new_v4(), Uuid::new_v4());
    let answer = json!({"query_sha256": "c".repeat(64), "dense_profile": null, "generation": "v2-nemotron-bm25-graph-1",
        "rerank_profile": null, "hits": [{"resource": {"resource_id": section, "kind": "section", "scope": "pilot-lab",
            "data_class": "paper", "title": "Results", "summary": null,
            "source_uri": "parsenriched://lab-pilot-lab/document/x/section/1"},
            "paper": null, "passage": {"ordinal": 0, "context": "Paper › Results", "body": "passage words",
            "char_from": 0, "char_to": 13}, "score": 0.1,
            "ranks": {"lexical": 1, "dense": null, "graph": null, "rerank": null}}]});
    let dir = std::env::temp_dir().join(format!("knowledge-retrieval-{}", Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let service = retrieval_service(
        &dir,
        &fake_retrieval(204, answer.clone(), seen.clone()).await,
    );
    let found = service
        .search_sections(run, "pilot-lab", "a question")
        .await
        .unwrap();
    assert_eq!(found.hits.len(), 1);
    assert_eq!(
        (found.hits[0].id, found.hits[0].summary.as_str()),
        (section, "Paper › Results\n\npassage words")
    );
    let (body, header) = seen.lock().unwrap()[0].clone();
    assert_eq!(
        body,
        json!({"text": "a question", "kinds": ["section"], "limit": 12, "generation": "v2-nemotron-bm25-graph-1"})
    );
    assert_eq!(header, Some(run.to_string()));

    let refused_dir = dir.join("refused");
    std::fs::create_dir(&refused_dir).unwrap();
    let none = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let refused = retrieval_service(
        &refused_dir,
        &fake_retrieval(403, answer, none.clone()).await,
    );
    assert_eq!(
        refused
            .search_sections(run, "pilot-lab", "a question")
            .await,
        Err(KnowledgeError::Refused)
    );
    assert!(
        none.lock().unwrap().is_empty(),
        "nothing is retrieved without the delegation"
    );

    // A lab held on search v1 by its own entry takes the v1 route (refused by this fake) and never retrieves.
    use std::os::unix::fs::PermissionsExt;
    let held_dir = dir.join("held");
    std::fs::create_dir(&held_dir).unwrap();
    let held_seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let url = fake_retrieval(204, json!({}), held_seen.clone()).await;
    let token = held_dir.join("token");
    std::fs::write(&token, "knowledge-token\n").unwrap();
    std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
    let config = held_dir.join("search.json");
    std::fs::write(
        &config,
        json!({"schema": SCHEMA, "service_url": url, "token_file": token, "labs": ["pilot-lab"],
        "retrieval_generation": "v2-nemotron-bm25-graph-1", "lab_retrieval_generations": {"pilot-lab": null}})
        .to_string(),
    )
    .unwrap();
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
    let held = load_optional(Some(&config)).unwrap().unwrap();
    assert!(
        held.search_sections(run, "pilot-lab", "a question")
            .await
            .is_err()
    );
    assert!(
        held_seen.lock().unwrap().is_empty(),
        "a lab held on v1 never retrieves"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
