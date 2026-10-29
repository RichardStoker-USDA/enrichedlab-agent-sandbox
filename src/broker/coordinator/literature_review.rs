//! A literature review before planning: an isolated reading window, one correction, and durable checked output.
use super::*;
use crate::broker::literature_review as review;

pub(super) fn settings_valid(settings: &LabText, model: &ModelConfig) -> bool {
    settings.review_labs.is_subset(&settings.reading)
        && (reading::READING_BYTES_MIN..=reading::READING_BYTES_MAX)
            .contains(&settings.review_bytes)
        && (settings.review_labs.is_empty()
            || reading::allowance_valid(model, settings.review_bytes))
}

pub(super) fn input_digest(context: &Value, fields: &HashSet<String>, bytes: usize) -> String {
    let mut fields: Vec<_> = fields.iter().collect();
    fields.sort();
    sha(json!({"system": review::SYSTEM, "context": context, "study_fields": fields, "bytes": bytes}).to_string().as_bytes())
}

/// Only the selected parts of the lead context are inputs; results and prior turns never invalidate a review.
pub(super) fn context(lead: &Value) -> Value {
    json!({"question": lead["question"], "constraints": lead["constraints"],
        "scientist_instructions": lead["scientist_instructions"], "literature": lead["literature"], "methods": lead["methods"]})
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run(
    store: &Store,
    model: &Model,
    library: Option<&LibraryService>,
    knowledge: Option<&KnowledgeService>,
    settings: &LabText,
    run: Uuid,
    worker: &str,
    generation: i64,
    lab: &str,
    results: &Value,
    lead: &mut Value,
    budget: usize,
) -> Result<Value> {
    let mut ask = context(lead);
    let Some(mut reading) = reading::Reading::for_review(settings, lab, results, &mut ask) else {
        return Ok(json!({"status": "no_match"}));
    };
    let (summaries, withheld) = reading
        .study_summaries(store, library, knowledge, run, worker, generation, lab)
        .await?;
    ask["study_summaries"] = summaries;
    let digest = input_digest(&ask, &settings.study_fields, settings.review_bytes);
    if let Some(kept) = store
        .literature_review_for_run(run, worker, generation, &digest, None)
        .await?
    {
        return Ok(place(lead, kept, budget, "kept"));
    }
    let ceiling = model.config.max_input_bytes + reading.allowance();
    let mut calls = Vec::new();
    let mut faults = Vec::new();
    let mut retry = false;
    let mut checked = None;
    loop {
        let call = match model
            .call_within("literature_review", review::SYSTEM, &ask, ceiling)
            .await
        {
            Ok(call) => call,
            Err(error) if error.to_string() == "context_budget_too_small" => {
                faults = vec!["context_budget_too_small".into()];
                break;
            }
            Err(error) => {
                if let Some(fault) = error.downcast_ref::<CodeRefused>() {
                    faults = if fault.names.is_empty() {
                        vec![fault.code.to_owned()]
                    } else {
                        fault.names.clone()
                    };
                    if correct_once(&mut ask, &mut retry, &faults) {
                        continue;
                    }
                    break;
                }
                return Err(error);
            }
        };
        if reading::asks_to_read(&call.value) && !retry && reading.rounds_left() > 0 {
            reading
                .read(
                    store, library, knowledge, run, worker, generation, lab, &call,
                )
                .await?;
            reading.place(&mut ask);
            calls.push(call);
            continue;
        }
        let outcome = review::check(&call.value, reading.held_parts(), &ask["methods"]);
        calls.push(call);
        match outcome {
            Ok(value) => {
                checked = Some(serde_json::to_value(value)?);
                break;
            }
            Err(names) => {
                faults = names;
                if !correct_once(&mut ask, &mut retry, &faults) {
                    break;
                }
            }
        }
    }
    // Keep calls before the lead is asked: either outcome survives even a refused planning reply.
    let (mut texts, mut expected) = reading::call_texts(&calls);
    let system_digest = json!(sha(review::SYSTEM.as_bytes()));
    if !calls.is_empty() {
        texts.push(review::SYSTEM.as_bytes());
        expected.push(&system_digest);
    }
    for (texts, expected) in texts.chunks(8).zip(expected.chunks(8)) {
        let kept = store
            .keep_research_contexts(run, worker, generation, texts)
            .await?;
        if kept.len() != expected.len()
            || kept
                .iter()
                .zip(expected)
                .any(|(k, e)| e.as_str() != Some(k))
        {
            bail!("context_kept_mismatch");
        }
    }
    let mut record = json!({"schema_version": 1, "inputs_sha256": digest, "study_summaries_withheld": withheld,
        "specialist": calls.iter().map(|c| &c.receipt).collect::<Vec<_>>(), "reading": reading.record(ceiling)});
    if let Some(checked) = checked {
        record["label"] = json!(review::LABEL);
        record["review"] = checked;
        record["status"] = json!("read");
    } else {
        record["status"] = json!("refused");
        record["faults"] = json!(faults);
    }
    let stored = store
        .literature_review_for_run(run, worker, generation, &digest, Some(&record))
        .await?
        .ok_or_else(|| anyhow!("literature_review_not_kept"))?;
    Ok(place(lead, stored, budget, "read"))
}

fn correct_once(ask: &mut Value, retried: &mut bool, faults: &[String]) -> bool {
    if *retried {
        return false;
    }
    *retried = true;
    ask["prior_attempt"] = json!({"reason": "literature_review_invalid", "names": faults});
    true
}

fn place(lead: &mut Value, mut record: Value, budget: usize, status: &str) -> Value {
    if record["status"] == "refused" {
        return record;
    }
    let fits = review::place(lead, &record["review"], budget);
    // The artifact and kept contexts carry research text; the generally readable turn carries receipts only.
    record.as_object_mut().unwrap().remove("review");
    record["status"] = json!(if fits { status } else { "too_large" });
    record
}

#[cfg(test)]
#[path = "literature_review_tests.rs"]
mod tests;
