//! Public reading receipts are separate from the private corrections shown to the model.
use super::*;

impl Reading {
    /// Only refs shown by the broker may leave kept contexts. Grammar alone does not establish a ref's source.
    fn ledger_ref(&self, reference: &str) -> Value {
        let mut receipt =
            json!({"ref_sha256": sha(reference.as_bytes()), "ref_bytes": reference.len()});
        let known = parse_ref(reference).is_some_and(|(paper, target)| {
            self.shown.contains(reference)
                || (target == Target::Outline && self.papers.contains_key(&paper))
        });
        if known {
            receipt["ref"] = json!(reference);
        }
        receipt
    }

    /// What the turn and cached review record, never the model's request text. Private rounds stay available to place().
    pub(in super::super) fn record(&self, ceiling: usize) -> Value {
        let rounds: Vec<Value> = self
            .rounds
            .iter()
            .map(|round| {
                let mut receipt = json!({});
                // These fields come from code or the call receipt, not a model-supplied string.
                for key in [
                    "lead",
                    "summary_sha256",
                    "refused_request",
                    "request_sha256",
                    "request_bytes",
                ] {
                    if let Some(value) = round.get(key) {
                        receipt[key] = value.clone();
                    }
                }
                for key in [
                    "requested",
                    "refused",
                    "shown",
                    "withheld",
                    "evicted",
                    "dropped",
                ] {
                    receipt[key] = json!(
                        round[key]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(|item| {
                                let reference = item.as_str().or_else(|| item["ref"].as_str());
                                let mut entry =
                                    reference.map_or_else(|| json!({}), |r| self.ledger_ref(r));
                                // Each field is produced by admission, fitting or screening; omit all other fields.
                                for field in [
                                    "code",
                                    "part",
                                    "bytes",
                                    "part_sha256",
                                    "paragraph",
                                    "field",
                                    "reason",
                                    "text_sha256",
                                ] {
                                    if let Some(value) = item.get(field) {
                                        entry[field] = value.clone();
                                    }
                                }
                                entry
                            })
                            .collect::<Vec<_>>()
                    );
                }
                receipt
            })
            .collect();
        json!({"schema_version": 2, "bytes": self.bytes, "ceiling": ceiling, "rounds": rounds,
            "held": self.parts.iter().map(|part| {
                let mut receipt = self.ledger_ref(part["ref"].as_str().unwrap_or_default());
                receipt["bytes"] = json!(size(part));
                receipt
            }).collect::<Vec<_>>()})
    }
}
