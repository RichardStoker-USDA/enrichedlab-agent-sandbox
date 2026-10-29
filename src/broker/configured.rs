//! Configured methods: a reviewed block template with `{{slot}}` markers and
//! named settings, each a closed list of choices. The research lead picks one choice per
//! setting and writes no code; the block is rendered by substitution and then admitted like
//! any other block. The turn commit renders it again in PostgreSQL and refuses a difference.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const SCHEMA: &str = "enrichedlab.configured-block.v1";
const MAX_SETTINGS: usize = 8;
const MAX_CHOICES: usize = 12;
const MAX_LABEL_CHARS: usize = 120;
const MAX_SLOT_BYTES: usize = 256;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Choice {
    pub label: String,
    /// The exact R text each slot receives when this choice is made.
    pub slots: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Setting {
    pub label: String,
    pub choices: BTreeMap<String, Choice>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Configuration {
    pub schema: String,
    pub block: String,
    pub block_sha256: String,
    pub settings: BTreeMap<String, Setting>,
}

pub fn digest(text: &str) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(text.as_bytes())))
}

fn setting_key(key: &str) -> bool {
    let mut chars = key.chars();
    (1..=32).contains(&key.len())
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn choice_key(key: &str) -> bool {
    (1..=32).contains(&key.len())
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

fn label(text: &str) -> bool {
    !text.trim().is_empty()
        && text.chars().count() <= MAX_LABEL_CHARS
        && !text.chars().any(char::is_control)
}

/// The slot names a block template marks, in order of first appearance.
fn marked_slots(block: &str) -> Option<BTreeSet<String>> {
    let mut slots = BTreeSet::new();
    let mut rest = block;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let end = after.find("}}")?;
        let name = &after[..end];
        if !setting_key(name) {
            return None;
        }
        slots.insert(name.to_owned());
        rest = &after[end + 2..];
    }
    Some(slots)
}

impl Configuration {
    /// The template row's `contract.configuration`, when the contract declares one. `Some(Err)`
    /// means it is declared but unusable, which refuses every proposal for that method.
    pub fn from_contract(contract: &Value) -> Option<Result<Configuration, &'static str>> {
        let value = contract.get("configuration")?;
        Some(
            serde_json::from_value::<Configuration>(value.clone())
                .map_err(|_| "configuration_invalid")
                .and_then(|c| c.check().map(|()| c)),
        )
    }

    /// Every slot the block marks is filled by exactly one setting, every choice of a setting
    /// fills the same slots, and no slot text can open another slot.
    pub fn check(&self) -> Result<(), &'static str> {
        if self.schema != SCHEMA
            || self.block_sha256 != digest(&self.block)
            || self.block.is_empty()
            || self.block.len() > crate::broker::codegen::MAX_BLOCK_BYTES
            || self.settings.is_empty()
            || self.settings.len() > MAX_SETTINGS
        {
            return Err("configuration_invalid");
        }
        let marked = marked_slots(&self.block).ok_or("configuration_invalid")?;
        let mut filled = BTreeSet::new();
        for (key, setting) in &self.settings {
            if !setting_key(key)
                || !label(&setting.label)
                || setting.choices.is_empty()
                || setting.choices.len() > MAX_CHOICES
            {
                return Err("configuration_invalid");
            }
            let mut slots: Option<BTreeSet<&String>> = None;
            for (choice, value) in &setting.choices {
                let these: BTreeSet<&String> = value.slots.keys().collect();
                if !choice_key(choice)
                    || !label(&value.label)
                    || these.is_empty()
                    || slots.as_ref().is_some_and(|s| *s != these)
                    || value.slots.values().any(|text| {
                        text.len() > MAX_SLOT_BYTES
                            || text.contains("{{")
                            || text.contains("}}")
                            || text.contains('\n')
                    })
                {
                    return Err("configuration_invalid");
                }
                slots = Some(these);
            }
            for slot in slots.into_iter().flatten() {
                if !filled.insert(slot.clone()) {
                    return Err("configuration_invalid");
                }
            }
        }
        if filled != marked {
            return Err("configuration_invalid");
        }
        Ok(())
    }

    /// One known choice for every setting and nothing else.
    pub fn validate(&self, chosen: &BTreeMap<String, String>) -> Result<(), &'static str> {
        if chosen.len() != self.settings.len()
            || chosen.iter().any(|(key, choice)| {
                !self
                    .settings
                    .get(key)
                    .is_some_and(|s| s.choices.contains_key(choice))
            })
        {
            return Err("proposal_settings_invalid");
        }
        Ok(())
    }

    /// The block with every slot filled from the chosen settings. The commit function renders
    /// it the same way and refuses any difference.
    pub fn render(&self, chosen: &BTreeMap<String, String>) -> Result<String, &'static str> {
        self.validate(chosen)?;
        let mut block = self.block.clone();
        for (key, choice) in chosen {
            for (slot, text) in &self.settings[key].choices[choice].slots {
                block = block.replace(&format!("{{{{{slot}}}}}"), text);
            }
        }
        if block.contains("{{") {
            return Err("proposal_settings_invalid");
        }
        Ok(block)
    }

    /// What the lead and the methods check see: settings and choices by label, never the block.
    pub fn view(&self) -> Value {
        Value::Object(
            self.settings
                .iter()
                .map(|(key, setting)| {
                    let choices: serde_json::Map<String, Value> = setting
                        .choices
                        .iter()
                        .map(|(k, c)| (k.clone(), json!(c.label)))
                        .collect();
                    (
                        key.clone(),
                        json!({"label": setting.label, "choices": choices}),
                    )
                })
                .collect(),
        )
    }

    /// Every label a method's settings show, for the number check.
    pub fn labels(&self) -> Vec<&str> {
        self.settings
            .values()
            .flat_map(|s| {
                std::iter::once(s.label.as_str())
                    .chain(s.choices.values().map(|c| c.label.as_str()))
            })
            .collect()
    }

    /// The record kept with the task: which recipe and which choices, never the block again.
    pub fn record(&self, chosen: &BTreeMap<String, String>) -> Value {
        json!({"schema": self.schema, "block_sha256": self.block_sha256, "settings": chosen})
    }
}

/// The `configuration` object for a template contract, from a block template and its settings
/// file (`profiles/configured/<name>/`).
pub fn contract_configuration(block: &str, settings_file: &Value) -> Value {
    json!({"schema": SCHEMA, "block": block, "block_sha256": digest(block),
        "settings": settings_file["settings"]})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::codegen;

    const BLOCK: &str = include_str!("../../profiles/configured/bootstrap-interval-v2/block.R");
    const SETTINGS: &str =
        include_str!("../../profiles/configured/bootstrap-interval-v2/configuration.json");

    fn configuration() -> Configuration {
        let settings: Value = serde_json::from_str(SETTINGS).unwrap();
        let contract = json!({"configuration": contract_configuration(BLOCK, &settings)});
        Configuration::from_contract(&contract).unwrap().unwrap()
    }

    fn every_choice(c: &Configuration) -> Vec<BTreeMap<String, String>> {
        let mut all = vec![BTreeMap::new()];
        for (key, setting) in &c.settings {
            all = all
                .into_iter()
                .flat_map(|partial| {
                    setting.choices.keys().map(move |choice| {
                        let mut next = partial.clone();
                        next.insert(key.clone(), choice.clone());
                        next
                    })
                })
                .collect();
        }
        all
    }

    #[test]
    fn every_combination_renders_and_is_admitted() {
        let c = configuration();
        let columns = ["len", "supp", "dose"].map(String::from);
        let all = every_choice(&c);
        assert_eq!(all.len(), 3 * 2 * 3 * 3);
        for chosen in all {
            let block = c.render(&chosen).unwrap();
            assert!(!block.contains("{{"), "{chosen:?}");
            let report = codegen::admit(&block, "template://r-generated-analysis/v2", &columns);
            assert!(report.accepted(), "{chosen:?}: {:?}", report.reasons);
        }
    }

    #[test]
    fn settings_must_name_one_known_choice_each() {
        let c = configuration();
        let good: BTreeMap<String, String> = [
            ("groups", "supplement"),
            ("statistic", "mean"),
            ("resamples", "10000"),
            ("level", "95"),
        ]
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .into();
        assert!(
            c.render(&good)
                .unwrap()
                .contains("split(data$len, data$supp, drop = TRUE)")
        );
        let mut missing = good.clone();
        missing.remove("level");
        assert_eq!(c.validate(&missing), Err("proposal_settings_invalid"));
        let mut unknown = good.clone();
        unknown.insert("groups".into(), "data$len".into());
        assert_eq!(c.validate(&unknown), Err("proposal_settings_invalid"));
        let mut extra = good.clone();
        extra.insert("seed".into(), "1".into());
        assert_eq!(c.validate(&extra), Err("proposal_settings_invalid"));
        // The lead sees labels, never the block or the R text of a choice.
        let view = c.view().to_string();
        assert!(view.contains("each delivery method (supplement)") && !view.contains("data$supp"));
        assert!(c.labels().contains(&"95%"));
    }

    #[test]
    fn a_declared_configuration_must_be_whole() {
        let settings: Value = serde_json::from_str(SETTINGS).unwrap();
        let mut value = contract_configuration(BLOCK, &settings);
        value["block_sha256"] = json!(digest("something else"));
        assert_eq!(
            Configuration::from_contract(&json!({"configuration": value})),
            Some(Err("configuration_invalid"))
        );
        // A slot the block marks but no setting fills.
        let block = format!("{BLOCK}\n# {{{{unfilled}}}}\n");
        assert_eq!(
            Configuration::from_contract(
                &json!({"configuration": contract_configuration(&block, &settings)})
            ),
            Some(Err("configuration_invalid"))
        );
        // A choice whose text could open another slot.
        let mut nested = settings.clone();
        nested["settings"]["statistic"]["choices"]["mean"]["slots"]["statistic"] =
            json!("{{level}}");
        assert_eq!(
            Configuration::from_contract(
                &json!({"configuration": contract_configuration(BLOCK, &nested)})
            ),
            Some(Err("configuration_invalid"))
        );
        assert!(Configuration::from_contract(&json!({"dataset": {}})).is_none());
    }
}
