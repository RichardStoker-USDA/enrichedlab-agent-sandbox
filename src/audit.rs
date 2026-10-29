use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::Path,
};

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::config::{RuntimeConfig, set_mode};

const GENESIS_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AuditRecord {
    pub schema_version: u32,
    pub sequence: u64,
    pub observed_at: String,
    pub actor: String,
    pub action: String,
    pub resource: String,
    pub outcome: String,
    pub metadata: Value,
    pub previous_hash: String,
    pub hash: String,
}

#[derive(Serialize)]
struct AuditUnsigned<'a> {
    schema_version: u32,
    sequence: u64,
    observed_at: &'a str,
    actor: &'a str,
    action: &'a str,
    resource: &'a str,
    outcome: &'a str,
    metadata: &'a Value,
    previous_hash: &'a str,
}

pub fn append(
    config: &RuntimeConfig,
    actor: &str,
    action: &str,
    resource: &str,
    outcome: &str,
    metadata: Value,
) -> Result<AuditRecord> {
    validate_text(actor, "actor")?;
    validate_text(action, "action")?;
    validate_text(resource, "resource")?;
    validate_text(outcome, "outcome")?;
    let path = log_path(config);
    let mut options = OpenOptions::new();
    options.create(true).read(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&path)
        .with_context(|| format!("failed to open audit log {}", path.display()))?;
    file.lock_exclusive()?;
    set_mode(&path, 0o600)?;

    file.seek(SeekFrom::Start(0))?;
    let mut body = String::new();
    file.read_to_string(&mut body)?;
    let prior = body.lines().last().map(parse_line).transpose()?;
    let sequence = prior.as_ref().map_or(1, |record| record.sequence + 1);
    let previous_hash = prior.map_or_else(|| GENESIS_HASH.into(), |record| record.hash);
    let observed_at = OffsetDateTime::now_utc().format(&Rfc3339)?;
    let metadata = redact(metadata);
    let hash = calculate_hash(&AuditUnsigned {
        schema_version: 1,
        sequence,
        observed_at: &observed_at,
        actor,
        action,
        resource,
        outcome,
        metadata: &metadata,
        previous_hash: &previous_hash,
    })?;
    let record = AuditRecord {
        schema_version: 1,
        sequence,
        observed_at,
        actor: actor.into(),
        action: action.into(),
        resource: resource.into(),
        outcome: outcome.into(),
        metadata,
        previous_hash,
        hash,
    };
    file.seek(SeekFrom::End(0))?;
    serde_json::to_writer(&mut file, &record)?;
    file.write_all(b"\n")?;
    file.sync_data()?;
    file.unlock()?;
    Ok(record)
}

pub fn verify(config: &RuntimeConfig) -> Result<Vec<AuditRecord>> {
    let path = log_path(config);
    if !path.exists() {
        return Ok(Vec::new());
    }
    verify_path(&path)
}

pub fn verify_path(path: &Path) -> Result<Vec<AuditRecord>> {
    let file = fs::File::open(path)?;
    let mut records = Vec::new();
    let mut previous = GENESIS_HASH.to_string();
    for (index, line) in BufReader::new(file).lines().enumerate() {
        let record = parse_line(&line?)?;
        let expected_sequence = index as u64 + 1;
        if record.sequence != expected_sequence || record.previous_hash != previous {
            bail!("audit chain discontinuity at sequence {}", record.sequence);
        }
        let expected = calculate_hash(&AuditUnsigned {
            schema_version: 1,
            sequence: record.sequence,
            observed_at: &record.observed_at,
            actor: &record.actor,
            action: &record.action,
            resource: &record.resource,
            outcome: &record.outcome,
            metadata: &record.metadata,
            previous_hash: &record.previous_hash,
        })?;
        if record.hash != expected {
            bail!("audit hash mismatch at sequence {}", record.sequence);
        }
        previous.clone_from(&record.hash);
        records.push(record);
    }
    Ok(records)
}

pub fn tail(config: &RuntimeConfig, count: usize) -> Result<Vec<AuditRecord>> {
    let records = verify(config)?;
    let start = records.len().saturating_sub(count);
    Ok(records[start..].to_vec())
}

pub fn log_path(config: &RuntimeConfig) -> std::path::PathBuf {
    config.audit_dir().join("events.jsonl")
}

fn parse_line(line: &str) -> Result<AuditRecord> {
    let record: AuditRecord = serde_json::from_str(line).context("invalid audit JSONL record")?;
    if record.schema_version != 1 {
        bail!("unsupported audit schema {}", record.schema_version);
    }
    Ok(record)
}

fn calculate_hash(unsigned: &AuditUnsigned<'_>) -> Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(unsigned)?)))
}

fn validate_text(value: &str, field: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > 512 || value.contains(['\n', '\r']) {
        bail!("invalid audit {field}");
    }
    Ok(())
}

fn redact(value: Value) -> Value {
    match value {
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| {
                    let lowered = key.to_ascii_lowercase();
                    let blocked = [
                        "authorization",
                        "cookie",
                        "password",
                        "secret",
                        "token",
                        "private_key",
                    ]
                    .iter()
                    .any(|needle| lowered.contains(needle));
                    (
                        key,
                        if blocked {
                            Value::String("[redacted]".into())
                        } else {
                            redact(value)
                        },
                    )
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.into_iter().map(redact).collect()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;
    use tempfile::tempdir;

    use super::{AuditRecord, AuditUnsigned, calculate_hash, verify_path};

    #[test]
    fn verifies_a_two_record_chain_and_rejects_tampering() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("audit.jsonl");
        let observed = "2026-08-29T00:00:00Z";
        let first_metadata = json!({});
        let first_hash = calculate_hash(&AuditUnsigned {
            schema_version: 1,
            sequence: 1,
            observed_at: observed,
            actor: "test",
            action: "create",
            resource: "sandbox/a",
            outcome: "passed",
            metadata: &first_metadata,
            previous_hash: super::GENESIS_HASH,
        })
        .unwrap();
        let first = AuditRecord {
            schema_version: 1,
            sequence: 1,
            observed_at: observed.into(),
            actor: "test".into(),
            action: "create".into(),
            resource: "sandbox/a".into(),
            outcome: "passed".into(),
            metadata: json!({}),
            previous_hash: super::GENESIS_HASH.into(),
            hash: first_hash.clone(),
        };
        let second_metadata = json!({});
        let second_hash = calculate_hash(&AuditUnsigned {
            schema_version: 1,
            sequence: 2,
            observed_at: observed,
            actor: "test",
            action: "stop",
            resource: "sandbox/a",
            outcome: "passed",
            metadata: &second_metadata,
            previous_hash: &first_hash,
        })
        .unwrap();
        let second = AuditRecord {
            schema_version: 1,
            sequence: 2,
            observed_at: observed.into(),
            actor: "test".into(),
            action: "stop".into(),
            resource: "sandbox/a".into(),
            outcome: "passed".into(),
            metadata: json!({}),
            previous_hash: first_hash,
            hash: second_hash,
        };
        fs::write(
            &path,
            format!(
                "{}\n{}\n",
                serde_json::to_string(&first).unwrap(),
                serde_json::to_string(&second).unwrap()
            ),
        )
        .unwrap();
        assert_eq!(verify_path(&path).unwrap().len(), 2);
        let mut broken = second;
        broken.outcome = "changed".into();
        fs::write(
            &path,
            format!(
                "{}\n{}\n",
                serde_json::to_string(&first).unwrap(),
                serde_json::to_string(&broken).unwrap()
            ),
        )
        .unwrap();
        assert!(verify_path(&path).is_err());
    }
}
