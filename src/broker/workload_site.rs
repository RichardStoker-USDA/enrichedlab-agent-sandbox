//! Site-owned metadata for fixed custody helpers. Never supplied by a task.
use anyhow::{Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkloadSiteConfig {
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub image_pull_secrets: Vec<String>,
}

impl WorkloadSiteConfig {
    pub fn validate(&self) -> Result<()> {
        use super::input_workload::{dns_label, label_key, label_value};
        ensure!(
            self.labels.len() <= 32
                && self.labels.iter().all(|(key, value)| {
                    label_key(key)
                        && label_value(value)
                        && !key.starts_with("sandbox.enrichedlab.org/")
                        && !key.starts_with("app.kubernetes.io/")
                        && !key.starts_with("kubernetes.io/")
                }),
            "custody_site_labels_invalid"
        );
        ensure!(
            self.image_pull_secrets.len() <= 8
                && self.image_pull_secrets.iter().all(|name| dns_label(name))
                && self
                    .image_pull_secrets
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    == self.image_pull_secrets.len(),
            "custody_site_pull_references_invalid"
        );
        Ok(())
    }

    pub fn extend_labels(&self, labels: &mut BTreeMap<String, String>) {
        labels.extend(self.labels.clone());
    }

    pub fn pull_references(&self) -> Vec<Value> {
        self.image_pull_secrets
            .iter()
            .map(|name| json!({"name": name}))
            .collect()
    }
}

/// Emit only typed error identifiers. Nested transport text can contain secrets.
pub fn safe_error_code(error: &anyhow::Error) -> String {
    error
        .chain()
        .map(ToString::to_string)
        .find(|code| is_safe_error_code(code))
        .unwrap_or_else(|| "custody_operation_failed_recovery_required".into())
}

pub fn is_safe_error_code(code: &str) -> bool {
    code.len() <= 128
        && code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
        && [
            "input_",
            "output_",
            "export_",
            "admission_",
            "publication_journal_",
            "custody_",
            "catalog_",
        ]
        .iter()
        .any(|prefix| code.starts_with(prefix))
}
