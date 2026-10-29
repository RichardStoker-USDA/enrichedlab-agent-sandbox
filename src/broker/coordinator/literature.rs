//! Literature, lab library, knowledge and research catalog reads for the lead's context.
use super::*;

/// References actually supplied to the lead, including retained lessons and the results it was
/// shown. A pruned lesson must not silently expand the proposal's evidence vocabulary.
pub fn context_evidence(context: &Value) -> HashSet<String> {
    let mut refs = HashSet::new();
    let strings = |v: &Value| -> Vec<String> {
        v.as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    };
    refs.extend(strings(&context["starting_point"]["data"]));
    if let Some(value) = context["starting_point"]["prepared_reference"].as_str() {
        refs.insert(value.to_owned());
    }
    for analysis in context["analyses"].as_array().into_iter().flatten() {
        refs.extend(strings(&analysis["result_refs"]));
        for result in analysis["results"].as_array().into_iter().flatten() {
            if let Some(r) = result["ref"].as_str() {
                refs.insert(r.to_owned());
            }
        }
    }
    for lesson in context["earlier_readings"]["lessons"]
        .as_array()
        .into_iter()
        .flatten()
    {
        refs.extend(strings(&lesson["evidence_refs"]));
    }
    refs
}

/// The library passages a context showed, directly or through the literature specialist's claims,
/// and the parts of papers the lead read in its turn: the only refs an action may
/// cite, together with checked review item refs.
pub fn context_literature(context: &Value) -> HashSet<String> {
    let mut refs: HashSet<String> = ["literature", "literature_claims"]
        .iter()
        .flat_map(|section| context[section].as_array().into_iter().flatten())
        .chain(context["reading"]["parts"].as_array().into_iter().flatten())
        .filter_map(|p| p["ref"].as_str())
        .map(str::to_owned)
        .collect();
    refs.extend(crate::broker::literature_review::refs(
        &context["literature_review"]["review"],
    ));
    refs
}

/// The literature specialist's reading for one lead call, applied to the lead's context in place of
/// the passages only when the context with it still fits the lead's `budget`: kept from an earlier call of this run
/// that read the same passages, or asked for now, once more with the faults named when a reply breaks a rule. A reply
/// that still cannot be read or checked, a request too large for the model, and a reading too large for the lead
/// leave the passages; a model that gives no answer fails the turn, as any model call does. Returns what the turn
/// records (the status, the passages' digest, the reading and the receipts of the calls that made it, the original ones
/// when kept) and those calls, whose texts the turn keeps.
pub(super) async fn literature_reading(
    model: &Model,
    kept: &literature_claims::Kept,
    run: Uuid,
    context: &mut Value,
    budget: usize,
) -> Result<(Value, Vec<literature_claims::SpecialistCall>)> {
    let (shown, digest) = literature_claims::passages_digest(&context["literature"]);
    let (mut record, calls) = match kept.get(run, &digest) {
        Some(found) => {
            let mut record = found.record;
            record["status"] = json!("kept");
            (record, found.calls)
        }
        None => {
            let mut ask = literature_claims::specialist_context(context);
            let mut calls: Vec<literature_claims::SpecialistCall> = Vec::new();
            let mut faults = Vec::new();
            let mut ended = None;
            for _ in 0..2 {
                let call = match model
                    .readable("literature_specialist", literature_claims::LITERATURE, &ask)
                    .await
                {
                    Ok(call) => call,
                    Err(error) if error.to_string() == "context_budget_too_small" => {
                        ended = Some(
                            json!({"status": "too_large", "code": "context_budget_too_small"}),
                        );
                        break;
                    }
                    Err(error) => match error.downcast_ref::<CodeRefused>() {
                        Some(refused) if refused.code != "model_content_missing" => {
                            ended = Some(json!({"status": "unreadable", "code": refused.code}));
                            break;
                        }
                        _ => return Err(error),
                    },
                };
                calls.push(literature_claims::SpecialistCall {
                    receipt: call.receipt.clone(),
                    context: call.context.clone(),
                    content: call.content.clone(),
                    reasoning: call.reasoning.clone(),
                });
                match literature_claims::check_reading(&call.value, &shown) {
                    Ok((reading, withheld)) => {
                        ended = Some(
                            json!({"status": "read", "reading": reading, "withheld": withheld}),
                        );
                        break;
                    }
                    Err(error) => {
                        faults = error
                            .downcast_ref::<CodeRefused>()
                            .map(|r| r.names.clone())
                            .unwrap_or_default();
                        ask["prior_attempt"] =
                            json!({"reason": "literature_claims_invalid", "names": faults});
                    }
                }
            }
            let mut record = ended.unwrap_or_else(|| json!({"status": "refused", "names": faults}));
            record["passages_sha256"] = json!(digest);
            record["specialist"] = json!(calls.iter().map(|c| &c.receipt).collect::<Vec<_>>());
            if record["status"] == "read" {
                kept.put(
                    run,
                    &digest,
                    literature_claims::KeptReading {
                        record: record.clone(),
                        calls: calls.clone(),
                    },
                );
            }
            (record, calls)
        }
    };
    if record.get("reading").is_some() {
        let reading: literature_claims::Reading =
            serde_json::from_value(record["reading"].clone())?;
        let withheld = record["withheld"].as_u64().unwrap_or(0) as usize;
        let mut with_claims = context.clone();
        literature_claims::apply_reading(&mut with_claims, &reading, withheld);
        if serde_json::to_vec(&with_claims).is_ok_and(|b| b.len() <= budget) {
            *context = with_claims;
        } else {
            // The claims and their papers do not fit beside the rest of this call's context; the
            // lead reads the passages, which the compiled context already fitted.
            record["status"] = json!("too_large");
            record["code"] = json!("literature_claims_too_large");
        }
    }
    Ok((record, calls))
}

/// The texts a turn keeps for the literature specialist's calls: its instructions,
/// and each call's context and the reply that fits a kept text, once each, with the receipt field naming each.
pub(super) fn specialist_texts(
    calls: &[literature_claims::SpecialistCall],
) -> (Vec<&[u8]>, Vec<&Value>) {
    let mut texts: Vec<&[u8]> = Vec::new();
    let mut expected: Vec<&Value> = Vec::new();
    let mut seen = HashSet::new();
    for call in calls {
        for (text, sha) in [
            (
                literature_claims::LITERATURE,
                &call.receipt["system_sha256"],
            ),
            (call.context.as_str(), &call.receipt["context_sha256"]),
            (call.content.as_str(), &call.receipt["content_sha256"]),
            (call.reasoning.as_str(), &call.receipt["reasoning_sha256"]),
        ] {
            if !text.is_empty() && text.len() <= MAX_KEPT_TEXT && seen.insert(sha.to_string()) {
                texts.push(text.as_bytes());
                expected.push(sha);
            }
        }
    }
    (texts, expected)
}

/// The lab library's passages for a run: the Library's word search on the lab's
/// corpus with the question's first 1,000 characters, kept to papers the lab's library still holds
/// while the run's starter is a member, in the search's order. With `accepted_only`
/// a passage also needs its paper accepted by a lab curator; the rest are counted as
/// `not_accepted`. A lab without a configured library,
/// a starter who left the lab and a service that does not answer are recorded, never a failed turn;
/// only the database read, fenced by the run's lease, can fail it.
/// The library query is the question's first characters; the turn records its hash.
pub fn literature_query(question: &str) -> (String, String) {
    let query: String = question.chars().take(LITERATURE_QUERY_CHARS).collect();
    let digest = sha(query.as_bytes());
    (query, digest)
}

/// Where a passage is in its paper, as the search that found it gave it: a chunk of the document service's word search,
/// or a section of the knowledge service. Both are cited as `library://<paper>/<uuid>`: a section by its
/// knowledge record, which keeps the section's key in the document service beside it.
pub(super) enum Place {
    Chunk(Uuid),
    Section { id: Uuid, key: String },
}

pub(super) struct Candidate {
    pub(super) document_id: Uuid,
    pub(super) place: Place,
    pub(super) headings: Vec<String>,
    pub(super) pages: Option<String>,
    pub(super) text: String,
    /// For a commons literature section: its paper's knowledge record and title.
    pub(super) commons: Option<(Uuid, String)>,
}

/// The sections a knowledge search found, as passage candidates in its ranking. The hit's scope decides what a
/// section is: in the lab's scope, a section of the lab's own corpus; in the commons, a section of
/// the commons baseline (which a person marked) whose paper the service named, in whatever corpus holds it. A
/// corpus never makes a commons section a library passage. A record of any other kind, scope or corpus is left out
/// and counted.
pub fn section_candidates(
    found: &knowledge::Found,
    lab: &str,
    corpus: &str,
) -> (usize, Vec<Value>) {
    let mut other = 0;
    let mut out = Vec::new();
    for hit in &found.hits {
        let uri = hit.source_uri.as_deref().filter(|_| hit.kind == "section");
        let (own, commons) = if hit.scope == lab {
            (
                uri.and_then(|uri| knowledge::section_address(uri, corpus)),
                None,
            )
        } else if hit.scope == "commons" {
            let paper = hit.paper.as_ref();
            (
                None,
                uri.and_then(knowledge::section_address_any).zip(paper),
            )
        } else {
            (None, None)
        };
        let (document, key) = match (own, &commons) {
            (Some(address), _) => address,
            (None, Some(((_, document, key), _))) => (*document, key.clone()),
            (None, None) => {
                other += 1;
                continue;
            }
        };
        let headings: Vec<String> = hit
            .locator
            .as_ref()
            .and_then(|l| l.get("heading_path"))
            .and_then(Value::as_array)
            .map(|path| {
                path.iter()
                    .filter_map(|h| h.as_str().map(str::to_owned))
                    .take(8)
                    .collect()
            })
            .unwrap_or_else(|| vec![hit.title.clone()]);
        let mut candidate = json!({"document_id": document, "section_id": hit.id, "section_key": key,
            "headings": headings, "pages": knowledge::locator_pages(hit.locator.as_ref()),
            "text": hit.summary.chars().take(LIBRARY_TEXT_CHARS).collect::<String>()});
        if let Some((_, paper)) = commons {
            candidate["commons_paper"] = json!(paper.id);
            candidate["commons_title"] = json!(paper.title.chars().take(300).collect::<String>());
        }
        out.push(candidate);
    }
    (other, out)
}

/// The most of a section's text a passage keeps, as for the document service's passages.
pub(super) const LIBRARY_TEXT_CHARS: usize = 1500;

/// Adds where a passage sits to its turn record: a chunk by its id, a section by its knowledge record and its key in
/// the document service, and a commons passage's origin.
pub(super) fn passage_place(passage: &Value, mut record: Value) -> Value {
    if passage["section_id"].is_string() {
        record["section_id"] = passage["section_id"].clone();
        record["section_key"] = passage["section_key"].clone();
    } else {
        record["chunk_id"] = passage["chunk_id"].clone();
    }
    if passage["origin"] == "commons" {
        record["origin"] = json!("commons");
    }
    record
}

/// The first field of a passage the lead would read that the untrusted-text screen holds back, in the order shown
/// (the paper's title, each section heading, the pages label, the excerpt), with the rule and the field's hash. The
/// pages label is free text from the library's word search, so it is screened like the rest.
pub(super) fn screened_field(
    passage: &Value,
    text: &str,
) -> Option<(&'static str, &'static str, String)> {
    let title = passage["title"].as_str().unwrap_or_default();
    let headings = passage["section"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str);
    let pages = passage["pages"].as_str().unwrap_or_default();
    std::iter::once(("title", title))
        .chain(headings.map(|heading| ("section", heading)))
        .chain(std::iter::once(("pages", pages)))
        .chain(std::iter::once(("text", text)))
        .find_map(|(field, value)| {
            screen::screen(value).map(|reason| (field, reason, sha(value.as_bytes())))
        })
}

#[allow(clippy::too_many_arguments)]
pub async fn read_library(
    store: &Store,
    library: Option<&LibraryService>,
    knowledge: Option<&KnowledgeService>,
    run: Uuid,
    worker: &str,
    generation: i64,
    lab: &str,
    question: &str,
    accepted_only: bool,
) -> Result<Value> {
    let (query, mut query_sha256) = literature_query(question);
    let Some(service) = library.filter(|service| service.configured(lab)) else {
        return Ok(json!({"status": "not_configured", "query_sha256": query_sha256}));
    };
    // The knowledge service's sections replace the word search for a lab it serves; it reads the
    // question's first characters, and the turn records the hash of the text it sent.
    let sections = knowledge
        .filter(|k| k.configured(lab))
        .zip(service.corpus(lab));
    if sections.is_some() {
        query_sha256 = sha(knowledge::search_text(question).as_bytes());
    }
    // Membership is checked before the document service sees the question, and
    // again after the search for the papers it returned.
    let member = store
        .research_run_library(run, worker, generation, &[])
        .await?;
    if member["member"] != true {
        return Ok(json!({"status": "not_member", "query_sha256": query_sha256}));
    }
    let mut other_records = 0;
    let found: Vec<Candidate> = if let Some((knowledge, corpus)) = sections {
        let text = knowledge::search_text(question);
        let hits = match knowledge.search_sections(run, lab, &text).await {
            Ok(found) => found,
            Err(error) => {
                return Ok(
                    json!({"status": "unavailable", "code": error.code(), "via": "knowledge",
                           "query_sha256": query_sha256}),
                );
            }
        };
        let (other, candidates) = section_candidates(&hits, lab, corpus);
        other_records = other;
        candidates
            .into_iter()
            .filter_map(|c| {
                Some(Candidate {
                    document_id: c["document_id"].as_str()?.parse().ok()?,
                    place: Place::Section {
                        id: c["section_id"].as_str()?.parse().ok()?,
                        key: c["section_key"].as_str()?.to_owned(),
                    },
                    headings: serde_json::from_value(c["headings"].clone()).ok()?,
                    pages: c["pages"].as_str().map(str::to_owned),
                    text: c["text"].as_str()?.to_owned(),
                    commons: match c["commons_paper"].as_str() {
                        Some(paper) => {
                            Some((paper.parse().ok()?, c["commons_title"].as_str()?.to_owned()))
                        }
                        None => None,
                    },
                })
            })
            .collect()
    } else {
        match service.search(lab, query.trim()).await {
            Ok(found) => found
                .into_iter()
                .filter_map(|p| {
                    Some(Candidate {
                        document_id: p.document_id,
                        place: Place::Chunk(p.chunk_id?),
                        headings: p.heading_path,
                        pages: p.pages,
                        text: p.text,
                        commons: None,
                    })
                })
                .collect(),
            Err(error) => {
                return Ok(
                    json!({"status": "unavailable", "code": error.code(), "query_sha256": query_sha256}),
                );
            }
        }
    };
    let mut documents: Vec<Uuid> = found
        .iter()
        .filter(|p| p.commons.is_none())
        .map(|p| p.document_id)
        .collect();
    documents.sort();
    documents.dedup();
    let held = store
        .research_run_library(run, worker, generation, &documents)
        .await?;
    if held["member"] != true {
        return Ok(json!({"status": "not_member", "query_sha256": query_sha256}));
    }
    let papers: HashMap<&str, &Value> = held["papers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| Some((p["document_id"].as_str()?, p)))
        .collect();
    let mut not_accepted = 0;
    let passages: Vec<Value> = found
        .iter()
        .filter_map(|p| {
            // A commons literature section reached the run only because a person marked it (the service's baseline),
            // so it needs no lab curator's acceptance; it is cited by its paper's knowledge record.
            if let (Some((paper, title)), Place::Section { id, key }) = (&p.commons, &p.place) {
                return Some(json!({"paper_id": paper, "title": title, "year": null, "origin": "commons",
                    "section": p.headings, "pages": p.pages, "text": p.text,
                    "ref": format!("literature://{paper}/{id}"), "section_id": id, "section_key": key}));
            }
            let paper = papers.get(p.document_id.to_string().as_str())?;
            if accepted_only && paper["accepted"] != true {
                not_accepted += 1;
                return None;
            }
            let paper_id = paper["paper_id"].as_str()?;
            let mut passage = json!({"paper_id": paper_id, "title": paper["title"], "year": paper["year"],
                "section": p.headings, "pages": p.pages, "text": p.text, "document_id": p.document_id});
            match &p.place {
                Place::Chunk(chunk) => {
                    passage["ref"] = json!(format!("library://{paper_id}/{chunk}"));
                    passage["chunk_id"] = json!(chunk);
                }
                Place::Section { id, key } => {
                    passage["ref"] = json!(format!("library://{paper_id}/{id}"));
                    passage["section_id"] = json!(id);
                    passage["section_key"] = json!(key);
                }
            }
            Some(passage)
        })
        .collect();
    let status = match (passages.is_empty(), not_accepted) {
        (false, _) => "found",
        (true, 0) => "no_match",
        (true, _) => "not_accepted",
    };
    let mut read = json!({"status": status, "query_sha256": query_sha256, "passages": passages});
    if accepted_only {
        read["not_accepted"] = json!(not_accepted);
    }
    if sections.is_some() {
        read["via"] = json!("knowledge");
        if other_records > 0 {
            read["other_records"] = json!(other_records);
        }
    }
    Ok(read)
}

/// The knowledge foundation's records for a run: the lab's earlier analyses, figures and
/// methods that match the question, in the service's ranking. The service ranks only the lab's records and the commons
/// baseline, under a delegation of the run to the lab alone; a record from any other scope, which the service should
/// never return, is dropped and counted, and so is a record of this project's own analyses, which the context already
/// shows. A lab without the service and a service that does not answer are recorded, never a failed turn.
pub async fn read_knowledge(
    service: Option<&KnowledgeService>,
    run: Uuid,
    lab: &str,
    question: &str,
    own_tasks: &HashSet<String>,
) -> Value {
    let text = knowledge::search_text(question);
    let query_sha256 = sha(text.as_bytes());
    let Some(service) = service.filter(|s| s.configured(lab)) else {
        return json!({"status": "not_configured", "query_sha256": query_sha256});
    };
    if text.is_empty() {
        return json!({"status": "no_match", "query_sha256": query_sha256});
    }
    let found = match service.search(run, lab, &text).await {
        Ok(found) => found,
        Err(error) => {
            return json!({"status": "unavailable", "code": error.code(), "query_sha256": query_sha256});
        }
    };
    let (hits, out_of_scope, this_project, not_earlier_work) =
        knowledge_hits(&found, lab, own_tasks);
    let mut read = json!({"status": if hits.is_empty() { "no_match" } else { "found" }, "query_sha256": query_sha256,
        "service_query_sha256": found.query_sha256, "dense_profile": found.dense_profile, "hits": hits,
        "out_of_scope": out_of_scope, "this_project": this_project});
    if not_earlier_work > 0 {
        read["not_earlier_work"] = json!(not_earlier_work);
    }
    read
}

/// Research-facing catalog search; its results are bound to current run methods only when the context compiles.
pub async fn read_research_catalog(
    service: Option<&KnowledgeService>,
    run: Uuid,
    lab: &str,
    question: &str,
) -> Value {
    let text = knowledge::search_text(question);
    let digest = sha(text.as_bytes());
    let Some(service) = service.filter(|s| s.configured(lab)) else {
        return json!({"status": "not_configured", "query_sha256": digest});
    };
    if text.is_empty() {
        return json!({"status": "no_match", "query_sha256": digest});
    }
    match service.search_catalog(run, lab, &text).await {
        Ok(found) => research_catalog::found(&found, lab),
        Err(error) => {
            json!({"status": "unavailable", "code": error.code(), "query_sha256": digest})
        }
    }
}

/// What the lab's earlier work is: its analyses and figures (`prior_output`) and its methods (`method`). A library
/// paper's figure is a `figure` record too, of class `paper`, so the kind alone cannot tell them apart.
pub(super) const EARLIER_WORK: [&str; 2] = ["prior_output", "method"];

/// The hits a lead may read: records of the run's lab or the commons that are earlier work, less this project's own
/// analyses. Returns them with the counts of records dropped for another scope (which the service should never
/// return), for this project, and for not being earlier work (a library paper's figure).
pub fn knowledge_hits(
    found: &knowledge::Found,
    lab: &str,
    own_tasks: &HashSet<String>,
) -> (Vec<Value>, usize, usize, usize) {
    let (mut out_of_scope, mut this_project, mut not_earlier_work) = (0, 0, 0);
    let hits = found
        .hits
        .iter()
        .filter(|h| {
            if h.scope != lab && h.scope != "commons" {
                out_of_scope += 1;
                return false;
            }
            if !EARLIER_WORK.contains(&h.data_class.as_str()) {
                not_earlier_work += 1;
                return false;
            }
            let own = h
                .source_uri
                .as_deref()
                .and_then(|uri| uri.rsplit_once("/task/"))
                .is_some_and(|(_, task)| own_tasks.contains(task));
            if own {
                this_project += 1;
            }
            !own
        })
        .map(|h| {
            let task = h
                .source_uri
                .as_deref()
                .and_then(|uri| uri.rsplit_once("/task/"))
                .and_then(|(_, task)| task.parse::<Uuid>().ok());
            json!({"id": h.id, "kind": h.kind, "scope": h.scope, "title": h.title, "summary": h.summary,
                "lexical_rank": h.lexical_rank, "dense_rank": h.dense_rank, "task": task})
        })
        .collect();
    (hits, out_of_scope, this_project, not_earlier_work)
}
