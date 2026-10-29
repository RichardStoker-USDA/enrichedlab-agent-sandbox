//! Per-use-case policy: the allowed file types, the size limit, the daily quota per person, retention,
//! and the action for each finding. A finding the policy does not name takes the use case's default action.
use crate::detect::FileType;
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const SCHEMA: &str = "enrichedlab.guard-policy.v1";
/// The largest file any use case may accept.
pub const HARD_MAX_BYTES: u64 = 256 * 1024 * 1024;

/// What happens to an item with a finding, in increasing severity. An item's outcome follows its most severe finding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Nothing to record beyond the finding itself.
    Allow,
    /// Recorded and shown with the result; the item passes.
    Annotate,
    /// Removed by the disarm layer; the item passes only when the rebuilt copy is clean.
    Disarm,
    /// Held and explained; an authorized person may release it with a recorded exception.
    Quarantine,
    /// Refused and never released.
    Block,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Quota {
    pub files_per_day: u64,
    pub bytes_per_day: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Retention {
    /// Days the original bytes stay in the person's quarantine scope.
    pub original_days: u32,
    /// Days the intake record stays.
    pub record_days: u32,
    /// Minutes the disarmed copy waits for its caller.
    pub disarmed_minutes: u32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Thresholds {
    /// Characters drawn white, tiny, off the page or invisible before each becomes a finding.
    pub hidden_text_chars: u64,
    /// Bytes after a file's end marker before they become a finding.
    pub trailing_bytes: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UseCase {
    pub types: Vec<FileType>,
    pub max_bytes: u64,
    pub quota: Quota,
    pub retention: Retention,
    pub thresholds: Thresholds,
    pub actions: BTreeMap<String, Action>,
    pub default_action: Action,
}

impl UseCase {
    pub fn action(&self, code: &str) -> Action {
        self.actions
            .get(code)
            .copied()
            .unwrap_or(self.default_action)
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub schema: String,
    pub version: String,
    pub use_cases: BTreeMap<String, UseCase>,
}

/// Every finding code the guard can produce. A policy may name only these, so a mistyped code cannot silently fall
/// back to the default.
pub const FINDING_CODES: &[&str] = &[
    "empty_file",
    "size_exceeded",
    "type_not_allowed",
    "type_mismatch",
    "polyglot",
    "trailing_data",
    "structure_invalid",
    "encrypted_password",
    "encrypted_owner_only",
    "active_javascript",
    "active_launch",
    "active_embedded_file",
    "active_open_action",
    "active_additional_actions",
    "active_xfa",
    "active_rich_media",
    "active_form_submit",
    "active_remote_goto",
    "hidden_layers",
    "hidden_text_invisible",
    "hidden_text_white",
    "hidden_text_tiny",
    "hidden_text_offpage",
    "analysis_truncated",
    "unicode_tags",
    "unicode_bidi",
    "unicode_zero_width",
    "metadata_text",
    "malware_signature",
    "yara_high",
    "yara_medium",
    "yara_low",
    "analysis_failed",
    "image_too_large",
    "embedded_archive",
    "content_irregular",
    "render_failed",
    "private_streams",
    "streams_unscanned",
    "disarm_failed",
    "disarm_residual",
    "disarm_text_changed",
    "disarm_text_unavailable",
    "table_too_large",
    "active_macros",
    "active_controls",
    "embedded_objects",
    "external_links",
    "xml_entities",
    "hidden_sheets",
    "formulas_without_values",
    "exception_applied",
];

impl Policy {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes =
            std::fs::read(path).with_context(|| format!("policy {} unreadable", path.display()))?;
        let policy: Policy = serde_json::from_slice(&bytes).context("policy is not valid")?;
        policy.validate()?;
        Ok(policy)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(self.schema == SCHEMA, "policy schema must be {SCHEMA}");
        ensure!(!self.version.trim().is_empty(), "policy version is empty");
        ensure!(!self.use_cases.is_empty(), "policy names no use case");
        for (name, case) in &self.use_cases {
            ensure!(
                valid_name(name),
                "use case name {name:?} is not a short lowercase name"
            );
            ensure!(!case.types.is_empty(), "use case {name} allows no type");
            for kind in &case.types {
                ensure!(
                    kind.can_disarm(),
                    "use case {name} allows {kind:?}, which the guard cannot rebuild"
                );
            }
            ensure!(
                case.max_bytes > 0 && case.max_bytes <= HARD_MAX_BYTES,
                "use case {name}: max_bytes out of range"
            );
            ensure!(
                case.quota.files_per_day > 0 && case.quota.bytes_per_day >= case.max_bytes,
                "use case {name}: the daily quota must admit at least one file of the largest size"
            );
            ensure!(
                case.retention.original_days >= 1
                    && case.retention.record_days >= case.retention.original_days,
                "use case {name}: records must outlive originals"
            );
            ensure!(
                (1..=1440).contains(&case.retention.disarmed_minutes),
                "use case {name}: disarmed_minutes out of range"
            );
            ensure!(
                matches!(case.default_action, Action::Quarantine | Action::Block),
                "use case {name}: the default action must quarantine or block"
            );
            for code in case.actions.keys() {
                if !FINDING_CODES.contains(&code.as_str()) {
                    bail!("use case {name}: unknown finding code {code:?}");
                }
            }
            ensure!(
                case.action("malware_signature") == Action::Block,
                "use case {name}: a malware signature must block"
            );
            // A file the guard could not finish checking or rebuilding never passes on its own.
            for code in [
                "analysis_failed",
                "image_too_large",
                "embedded_archive",
                "render_failed",
                "streams_unscanned",
                "disarm_failed",
                "disarm_residual",
                "table_too_large",
            ] {
                ensure!(
                    case.action(code) >= Action::Quarantine,
                    "use case {name}: {code} must quarantine or block"
                );
            }
        }
        Ok(())
    }

    pub fn use_case(&self, name: &str) -> Option<&UseCase> {
        self.use_cases.get(name)
    }
}

/// Tenant, lab, project and use-case names: lowercase letters, digits, dot, underscore and hyphen, 1 to 63 bytes.
pub fn valid_name(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 63
        && bytes[0].is_ascii_alphanumeric()
        && bytes.iter().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library_paper() -> Policy {
        serde_json::from_str(crate::fixtures::POLICY).unwrap()
    }

    #[test]
    fn the_shipped_policy_is_valid() {
        let policy = library_paper();
        policy.validate().unwrap();
        let datasets = policy.use_case("lab-dataset").unwrap();
        assert_eq!(datasets.types, [FileType::Delimited, FileType::Xlsx]);
        assert_eq!(datasets.action("table_too_large"), Action::Block);
        assert_eq!(datasets.action("active_macros"), Action::Disarm);
    }

    #[test]
    fn a_mistyped_code_or_a_soft_malware_action_is_refused() {
        let mut p = library_paper();
        let case = p.use_cases.get_mut("library-paper").unwrap();
        case.actions
            .insert("active_javascipt".into(), Action::Allow);
        assert!(
            p.validate()
                .unwrap_err()
                .to_string()
                .contains("unknown finding code")
        );
        let mut p = library_paper();
        p.use_cases
            .get_mut("library-paper")
            .unwrap()
            .actions
            .insert("malware_signature".into(), Action::Quarantine);
        assert!(p.validate().unwrap_err().to_string().contains("must block"));
        let mut p = library_paper();
        p.use_cases.get_mut("library-paper").unwrap().default_action = Action::Annotate;
        assert!(p.validate().is_err());
        let mut p = library_paper();
        p.use_cases
            .get_mut("lab-dataset")
            .unwrap()
            .actions
            .insert("table_too_large".into(), Action::Annotate);
        assert!(
            p.validate()
                .unwrap_err()
                .to_string()
                .contains("table_too_large must quarantine or block")
        );
    }

    #[test]
    fn names_are_short_and_lowercase() {
        assert!(valid_name("library-paper"));
        assert!(valid_name("qualification.lab_1"));
        assert!(!valid_name(""));
        assert!(!valid_name("Library"));
        assert!(!valid_name("../etc"));
        assert!(!valid_name(&"a".repeat(64)));
    }
}
