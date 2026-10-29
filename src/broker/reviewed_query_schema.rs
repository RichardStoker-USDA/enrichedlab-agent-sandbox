//! Closed parameter types and normalization. Faults name only registered parameter names.
use super::*;
use unicode_normalization::UnicodeNormalization;

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Parameter {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    #[serde(flatten)]
    pub kind: ParameterType,
}

impl<'de> Deserialize<'de> for Parameter {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let mut value = Value::deserialize(deserializer)?;
        let invalid = || D::Error::custom("reviewed_query_definition_invalid");
        let map = value.as_object_mut().ok_or_else(invalid)?;
        let name = map
            .remove("name")
            .and_then(|v| v.as_str().map(str::to_owned))
            .ok_or_else(invalid)?;
        let description = map
            .remove("description")
            .and_then(|v| v.as_str().map(str::to_owned))
            .ok_or_else(invalid)?;
        let required = match map.remove("required") {
            None => false,
            Some(Value::Bool(v)) => v,
            _ => return Err(invalid()),
        };
        let default = map.remove("default");
        let fields: &[&str] = match map.get("type").and_then(Value::as_str) {
            Some("text") => &["type", "max_length"],
            Some("integer" | "number") => &["type", "minimum", "maximum"],
            Some("enum") => &["type", "choices"],
            _ => return Err(invalid()),
        };
        if map.keys().any(|k| !fields.contains(&k.as_str())) {
            return Err(invalid());
        }
        let kind = serde_json::from_value(value).map_err(|_| invalid())?;
        Ok(Self {
            name,
            description,
            required,
            default,
            kind,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ParameterType {
    Text { max_length: usize },
    Integer { minimum: i64, maximum: i64 },
    Number { minimum: f64, maximum: f64 },
    Enum { choices: Vec<String> },
}

pub(super) fn name(s: &str) -> bool {
    (1..=63).contains(&s.len())
        && s.as_bytes()[0].is_ascii_lowercase()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

impl Parameter {
    fn fault(&self, code: &'static str) -> Fault {
        Fault {
            code,
            parameter: Some(self.name.clone()),
        }
    }
    pub(super) fn validate(&self) -> Result<(), Fault> {
        // An invalid registration name is not safe to echo as a fault location.
        if !name(&self.name) {
            return Err(fault("reviewed_query_definition_invalid"));
        }
        let invalid = || self.fault("reviewed_query_definition_invalid");
        if !text(&self.description, 300) || self.required == self.default.is_some() {
            return Err(invalid());
        }
        let valid = match &self.kind {
            ParameterType::Text { max_length } => (1..=256).contains(max_length),
            ParameterType::Integer { minimum, maximum } => minimum <= maximum,
            ParameterType::Number { minimum, maximum } => {
                minimum.is_finite() && maximum.is_finite() && minimum <= maximum
            }
            ParameterType::Enum { choices } => {
                (1..=64).contains(&choices.len())
                    && choices.iter().collect::<BTreeSet<_>>().len() == choices.len()
                    && choices
                        .iter()
                        .all(|c| text(c, 256) && c.nfc().eq(c.chars()))
            }
        };
        if !valid {
            return Err(invalid());
        }
        if let Some(default) = &self.default {
            self.normalize(default).map_err(|_| invalid())?;
        }
        Ok(())
    }

    pub(super) fn sample(&self) -> Value {
        self.default.clone().unwrap_or_else(|| match &self.kind {
            ParameterType::Text { .. } => json!("x"),
            ParameterType::Integer { minimum, .. } => json!(minimum),
            ParameterType::Number { minimum, .. } => json!(minimum),
            ParameterType::Enum { choices } => json!(choices[0]),
        })
    }

    pub(super) fn sql_type(&self) -> &'static str {
        match self.kind {
            ParameterType::Integer { .. } => "int8",
            ParameterType::Number { .. } => "numeric",
            _ => "text",
        }
    }

    fn normalize(&self, value: &Value) -> Result<Value, Fault> {
        let wrong = || self.fault("reviewed_query_parameter_type");
        let bounds = || self.fault("reviewed_query_parameter_bounds");
        match &self.kind {
            ParameterType::Text { max_length } => {
                let raw = value.as_str().ok_or_else(wrong)?;
                if raw.chars().any(char::is_control) {
                    return Err(self.fault("reviewed_query_parameter_control"));
                }
                let normalized: String = raw.nfc().collect();
                if normalized.chars().count() > *max_length {
                    return Err(bounds());
                }
                Ok(json!(normalized))
            }
            ParameterType::Enum { choices } => {
                let raw = value.as_str().ok_or_else(wrong)?;
                if !choices.iter().any(|c| c == raw) {
                    return Err(self.fault("reviewed_query_parameter_choice"));
                }
                Ok(value.clone())
            }
            ParameterType::Integer { minimum, maximum } => {
                let number = value.as_i64().ok_or_else(wrong)?;
                if number < *minimum || number > *maximum {
                    return Err(bounds());
                }
                Ok(json!(number))
            }
            ParameterType::Number { minimum, maximum } => {
                let number = value.as_f64().filter(|n| n.is_finite()).ok_or_else(wrong)?;
                if number < *minimum || number > *maximum {
                    return Err(bounds());
                }
                Ok(json!(number))
            }
        }
    }
}

pub(super) fn normalize(parameters: &[Parameter], requested: &Value) -> Result<Value, Fault> {
    let map = requested
        .as_object()
        .ok_or_else(|| fault("reviewed_query_parameters_invalid"))?;
    if serde_json::to_vec(requested).map_or(true, |b| b.len() > 8192) {
        return Err(fault("reviewed_query_parameters_too_large"));
    }
    if map
        .keys()
        .any(|name| !parameters.iter().any(|p| p.name == *name))
    {
        return Err(fault("reviewed_query_parameter_unknown"));
    }
    let mut normalized = serde_json::Map::new();
    for p in parameters {
        let value = map
            .get(&p.name)
            .or(p.default.as_ref())
            .ok_or_else(|| p.fault("reviewed_query_parameter_missing"))?;
        normalized.insert(p.name.clone(), p.normalize(value)?);
    }
    Ok(Value::Object(normalized))
}
