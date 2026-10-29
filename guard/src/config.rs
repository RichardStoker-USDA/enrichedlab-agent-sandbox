//! Service configuration and the registered principals. A caller sends files for its use cases; a fetcher only reads
//! the records and rebuilt copies of files that named callers sent for named use cases, so a service's worker can take
//! a copy its API received without holding the API's token; an operator records exceptions. Every principal is stored
//! as the SHA-256 digest of its token; the guard never holds a token value of its own.
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const CONFIG_SCHEMA: &str = "enrichedlab.guard-config.v1";
pub const CALLERS_SCHEMA: &str = "enrichedlab.guard-callers.v1";

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub schema: String,
    pub listen: String,
    pub ops_listen: String,
    pub root: PathBuf,
    pub policy_file: PathBuf,
    pub callers_file: PathBuf,
    pub rules_dir: PathBuf,
    pub scanner: String,
    pub scanner_timeout_seconds: u64,
    pub signature_max_age_hours: u64,
    pub max_concurrent: usize,
    pub queue_wait_seconds: u64,
    pub max_store_bytes: u64,
    pub qpdf: String,
    pub pdftotext: String,
    /// Draws every page of each rebuilt PDF at low resolution, so each image is decoded once within the helper limits.
    pub pdftoppm: String,
    pub helper_timeout_seconds: u64,
    pub helper_memory_mib: u64,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let config: Config =
            serde_json::from_slice(&std::fs::read(path).context("config unreadable")?)
                .context("config is not valid")?;
        ensure!(
            config.schema == CONFIG_SCHEMA,
            "config schema must be {CONFIG_SCHEMA}"
        );
        ensure!(
            (1..=64).contains(&config.max_concurrent),
            "max_concurrent out of range"
        );
        ensure!(
            config.signature_max_age_hours >= 1,
            "signature_max_age_hours must be positive"
        );
        ensure!(
            config.helper_memory_mib >= 256,
            "helper_memory_mib below 256"
        );
        Ok(config)
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Caller {
    pub id: String,
    pub token_sha256: String,
    pub use_cases: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fetcher {
    pub id: String,
    pub token_sha256: String,
    /// The callers whose intakes this fetcher may read.
    pub fetches_for: Vec<String>,
    pub use_cases: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Operator {
    pub id: String,
    pub token_sha256: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Callers {
    pub schema: String,
    pub callers: Vec<Caller>,
    #[serde(default)]
    pub fetchers: Vec<Fetcher>,
    pub operators: Vec<Operator>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Principal {
    Caller {
        id: String,
        use_cases: Vec<String>,
    },
    Fetcher {
        id: String,
        fetches_for: Vec<String>,
        use_cases: Vec<String>,
    },
    Operator {
        id: String,
    },
}

impl Principal {
    /// Whether this principal may read the record and rebuilt copy of an intake `caller` sent for `use_case`.
    pub fn reads(&self, caller: &str, use_case: &str) -> bool {
        match self {
            Principal::Caller { id, .. } => id == caller,
            Principal::Fetcher {
                fetches_for,
                use_cases,
                ..
            } => fetches_for.iter().any(|c| c == caller) && use_cases.iter().any(|u| u == use_case),
            Principal::Operator { .. } => false,
        }
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

impl Callers {
    pub fn load(path: &Path) -> Result<Self> {
        let callers: Callers =
            serde_json::from_slice(&std::fs::read(path).context("callers file unreadable")?)
                .context("callers file is not valid")?;
        ensure!(
            callers.schema == CALLERS_SCHEMA,
            "callers schema must be {CALLERS_SCHEMA}"
        );
        let digests: Vec<&String> = callers
            .callers
            .iter()
            .map(|c| &c.token_sha256)
            .chain(callers.fetchers.iter().map(|f| &f.token_sha256))
            .chain(callers.operators.iter().map(|o| &o.token_sha256))
            .collect();
        for digest in &digests {
            ensure!(
                digest.len() == 64
                    && digest
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
                "a token digest is not 64 lowercase hex characters"
            );
        }
        let distinct: std::collections::BTreeSet<&&String> = digests.iter().collect();
        ensure!(
            distinct.len() == digests.len(),
            "two principals share a token digest"
        );
        for fetcher in &callers.fetchers {
            ensure!(
                !fetcher.fetches_for.is_empty() && !fetcher.use_cases.is_empty(),
                "fetcher {} names no caller or no use case",
                fetcher.id
            );
        }
        Ok(callers)
    }

    /// The principal a bearer token belongs to. Every digest is compared, so the time taken does not reveal which.
    pub fn authenticate(&self, token: &str) -> Option<Principal> {
        if token.is_empty() || token.len() > 4096 {
            return None;
        }
        let digest = hex::encode(Sha256::digest(token.as_bytes()));
        let mut found = None;
        for caller in &self.callers {
            if constant_time_eq(caller.token_sha256.as_bytes(), digest.as_bytes())
                && found.is_none()
            {
                found = Some(Principal::Caller {
                    id: caller.id.clone(),
                    use_cases: caller.use_cases.clone(),
                });
            }
        }
        for fetcher in &self.fetchers {
            if constant_time_eq(fetcher.token_sha256.as_bytes(), digest.as_bytes())
                && found.is_none()
            {
                found = Some(Principal::Fetcher {
                    id: fetcher.id.clone(),
                    fetches_for: fetcher.fetches_for.clone(),
                    use_cases: fetcher.use_cases.clone(),
                });
            }
        }
        for operator in &self.operators {
            if constant_time_eq(operator.token_sha256.as_bytes(), digest.as_bytes())
                && found.is_none()
            {
                found = Some(Principal::Operator {
                    id: operator.id.clone(),
                });
            }
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authenticates_by_digest() {
        let digest = |t: &str| hex::encode(Sha256::digest(t.as_bytes()));
        let callers = Callers {
            schema: CALLERS_SCHEMA.into(),
            callers: vec![Caller {
                id: "workspace".into(),
                token_sha256: digest("caller-token"),
                use_cases: vec!["library-paper".into()],
            }],
            fetchers: vec![Fetcher {
                id: "worker".into(),
                token_sha256: digest("fetcher-token"),
                fetches_for: vec!["workspace".into()],
                use_cases: vec!["lab-dataset".into()],
            }],
            operators: vec![Operator {
                id: "operator".into(),
                token_sha256: digest("operator-token"),
            }],
        };
        assert_eq!(
            callers.authenticate("caller-token"),
            Some(Principal::Caller {
                id: "workspace".into(),
                use_cases: vec!["library-paper".into()]
            })
        );
        assert_eq!(
            callers.authenticate("operator-token"),
            Some(Principal::Operator {
                id: "operator".into()
            })
        );
        let fetcher = callers.authenticate("fetcher-token").unwrap();
        assert!(fetcher.reads("workspace", "lab-dataset"));
        assert!(!fetcher.reads("workspace", "library-paper"));
        assert!(!fetcher.reads("other", "lab-dataset"));
        assert_eq!(callers.authenticate("wrong"), None);
        assert_eq!(callers.authenticate(""), None);
    }

    #[test]
    fn a_shared_digest_or_an_empty_fetcher_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("callers.json");
        let digest = hex::encode(Sha256::digest(b"token"));
        let other = hex::encode(Sha256::digest(b"other"));
        let write = |fetchers: serde_json::Value| {
            std::fs::write(
                &path,
                serde_json::json!({"schema": CALLERS_SCHEMA,
                    "callers": [{"id": "workspace", "token_sha256": digest, "use_cases": ["lab-dataset"]}],
                    "fetchers": fetchers, "operators": []})
                .to_string(),
            )
            .unwrap();
        };
        write(serde_json::json!([{"id": "worker", "token_sha256": other,
            "fetches_for": ["workspace"], "use_cases": ["lab-dataset"]}]));
        Callers::load(&path).unwrap();
        write(serde_json::json!([{"id": "worker", "token_sha256": digest,
            "fetches_for": ["workspace"], "use_cases": ["lab-dataset"]}]));
        assert!(
            Callers::load(&path)
                .unwrap_err()
                .to_string()
                .contains("share a token digest")
        );
        write(serde_json::json!([{"id": "worker", "token_sha256": other,
            "fetches_for": [], "use_cases": ["lab-dataset"]}]));
        assert!(Callers::load(&path).is_err());
    }
}
