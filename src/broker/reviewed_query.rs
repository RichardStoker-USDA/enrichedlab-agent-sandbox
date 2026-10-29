//! Reviewed SELECT templates. Registration is convenience; every use lowers and admits again.
use super::sql_guard::{self, Allowed};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

#[path = "reviewed_query_schema.rs"]
mod schema;
pub use schema::{Parameter, ParameterType};
#[path = "reviewed_query_ast.rs"]
mod ast;
#[path = "reviewed_query_registration.rs"]
pub mod registration;
#[path = "reviewed_query_size.rs"]
mod size;
pub use size::{MAX_DEFINITION_BYTES, jsonb_text_bytes};

pub const OFFER: &str = "This method offers reviewed_queries. An action may carry reviewed_query: {\"tool\": a listed key, \"parameters\": an object of typed values} instead of sql, with no code, settings or analysis. The parameter schemas give required values, defaults, bounds and choices. Use the tool's purpose and parameters to check its fit to the question. The system supplies and checks its SQL; results still need validation.";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Tool {
    pub key: String,
    pub version: i32,
    pub source: String,
    pub title: String,
    pub purpose: String,
    pub parameters: Vec<Parameter>,
    pub output_columns: Vec<Column>,
    pub row_cap: u64,
    pub state: State,
    #[serde(default)]
    pub template: String,
    #[serde(default)]
    pub template_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Enabled,
    Retired,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Column {
    pub name: String,
    pub r#type: String,
    pub description: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub tool: String,
    pub parameters: Value,
}

/// Kept with the admitted query. The template and schema come from the registry, never the lead.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub tool: String,
    pub version: i32,
    pub template: String,
    pub template_sha256: String,
    pub schema_sha256: String,
    pub parameters: Vec<Parameter>,
    pub requested: Value,
    pub normalized: Value,
    pub row_cap: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Fault {
    pub code: &'static str,
    pub parameter: Option<String>,
}
impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code)
    }
}
impl std::error::Error for Fault {}
fn fault(code: &'static str) -> Fault {
    Fault {
        code,
        parameter: None,
    }
}
pub fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
pub fn key(value: &str) -> bool {
    (3..=64).contains(&value.len())
        && value.as_bytes()[0].is_ascii_lowercase()
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}
fn text(value: &str, max: usize) -> bool {
    !value.is_empty() && value.chars().count() <= max && !value.chars().any(char::is_control)
}

impl Tool {
    pub fn validate(&self, allowed: &Allowed) -> Result<(), Fault> {
        if !key(&self.key)
            || !key(&self.source)
            || self.version < 1
            || !text(&self.title, 120)
            || !text(&self.purpose, 1000)
            || self.row_cap == 0
            || self.row_cap > allowed.row_cap
            || self.template.is_empty()
            || self.template.len() > sql_guard::MAX_SQL_BYTES
            || digest(self.template.as_bytes()) != self.template_sha256
            || self.parameters.len() > 16
            || self.output_columns.is_empty()
            || self.output_columns.len() > 64
        {
            return Err(fault("reviewed_query_definition_invalid"));
        }
        let mut names = BTreeSet::new();
        for p in &self.parameters {
            p.validate()?;
            if !names.insert(&p.name) {
                return Err(fault("reviewed_query_definition_invalid"));
            }
        }
        names.clear();
        for c in &self.output_columns {
            if !schema::name(&c.name)
                || !text(&c.r#type, 64)
                || !text(&c.description, 300)
                || !names.insert(&c.name)
            {
                return Err(fault("reviewed_query_definition_invalid"));
            }
        }
        Ok(())
    }

    pub fn view(&self) -> Value {
        json!({"key": self.key, "version": self.version, "title": self.title, "purpose": self.purpose,
            "parameters": self.parameters, "output_columns": self.output_columns, "row_cap": self.row_cap})
    }

    /// Run the identical admission used at proposal time on defaults or typed samples before registering, and refuse a
    /// definition the registry could not store.
    pub fn registration_check(&self, allowed: &Allowed) -> Result<(), Fault> {
        self.validate(allowed)?;
        let body =
            serde_json::to_value(self).map_err(|_| fault("reviewed_query_definition_invalid"))?;
        if jsonb_text_bytes(&body) > MAX_DEFINITION_BYTES {
            return Err(fault("reviewed_query_definition_too_large"));
        }
        let samples: serde_json::Map<String, Value> = self
            .parameters
            .iter()
            .map(|p| (p.name.clone(), p.sample()))
            .collect();
        self.lower(&Value::Object(samples), allowed).map(|_| ())
    }

    pub fn lower(
        &self,
        requested: &Value,
        allowed: &Allowed,
    ) -> Result<(sql_guard::Admitted, Record), Fault> {
        self.validate(allowed)?;
        if self.state != State::Enabled {
            return Err(fault("reviewed_query_retired"));
        }
        let normalized = schema::normalize(&self.parameters, requested)?;
        let sql = ast::lower(&self.template, &self.parameters, &normalized)?;
        let admitted = sql_guard::admit(
            &sql,
            &Allowed {
                row_cap: self.row_cap,
                ..*allowed
            },
        )
        // Parser diagnostics can echo values; reviewed-query faults never do.
        .map_err(|r| fault(r.code))?;
        let record = Record {
            tool: self.key.clone(),
            version: self.version,
            template: self.template.clone(),
            template_sha256: self.template_sha256.clone(),
            schema_sha256: digest(
                &serde_json::to_vec(&self.parameters)
                    .map_err(|_| fault("reviewed_query_definition_invalid"))?,
            ),
            parameters: self.parameters.clone(),
            requested: requested.clone(),
            normalized,
            row_cap: self.row_cap,
        };
        Ok((admitted, record))
    }
}

impl Record {
    /// Bind provenance to execution as well as re-admitting the stored SQL on the existing worker path.
    pub fn check(&self, sql: &str, allowed: &Allowed) -> Result<(), Fault> {
        if !key(&self.tool)
            || self.version < 1
            || self.parameters.len() > 16
            || self
                .parameters
                .iter()
                .map(|p| &p.name)
                .collect::<BTreeSet<_>>()
                .len()
                != self.parameters.len()
            || self.template.len() > sql_guard::MAX_SQL_BYTES
            || self.row_cap == 0
            || self.row_cap > allowed.row_cap
            || digest(self.template.as_bytes()) != self.template_sha256
            || digest(
                &serde_json::to_vec(&self.parameters)
                    .map_err(|_| fault("reviewed_query_record_invalid"))?,
            ) != self.schema_sha256
        {
            return Err(fault("reviewed_query_record_invalid"));
        }
        for p in &self.parameters {
            p.validate()?;
        }
        let normalized = schema::normalize(&self.parameters, &self.requested)?;
        let lowered = ast::lower(&self.template, &self.parameters, &normalized)?;
        let admitted = sql_guard::admit(
            &lowered,
            &Allowed {
                row_cap: self.row_cap,
                ..*allowed
            },
        )
        .map_err(|r| fault(r.code))?;
        if normalized != self.normalized || admitted.sql != sql {
            return Err(fault("reviewed_query_record_invalid"));
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "reviewed_query_tests.rs"]
mod tests;
