use std::{collections::BTreeMap, fs};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    cli::Thinking,
    config::{RuntimeConfig, write_private},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RouteCatalog {
    pub schema_version: u32,
    pub routes: BTreeMap<String, RouteConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RouteConfig {
    pub workspace: String,
    pub pool_cap: usize,
    pub model: Option<String>,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub context_limit: u64,
    #[serde(default)]
    pub output_limit: u64,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub attachment: bool,
    #[serde(default = "default_interleaved_field")]
    pub interleaved_field: String,
    #[serde(default = "default_modalities")]
    pub input_modalities: Vec<String>,
    #[serde(default)]
    pub options: Value,
    #[serde(default)]
    pub variants: BTreeMap<String, Value>,
    pub default_variant: Option<String>,
    #[serde(default = "default_pi_api")]
    pub pi_api: String,
    #[serde(default = "default_pi_compat")]
    pub pi_compat: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inference_engine: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_parser: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_revision: Option<String>,
}

fn default_interleaved_field() -> String {
    "reasoning".into()
}
fn default_modalities() -> Vec<String> {
    vec!["text".into()]
}
fn default_pi_api() -> String {
    "openai-completions".into()
}
fn default_pi_compat() -> Value {
    json!({
        "supportsDeveloperRole": false,
        "supportsReasoningEffort": true
    })
}

impl Default for RouteCatalog {
    fn default() -> Self {
        Self {
            schema_version: 1,
            routes: BTreeMap::from([(
                "none".into(),
                RouteConfig {
                    workspace: "default".into(),
                    pool_cap: 10,
                    model: None,
                    display_name: "No inference route".into(),
                    context_limit: 0,
                    output_limit: 0,
                    reasoning: false,
                    attachment: false,
                    interleaved_field: "reasoning".into(),
                    input_modalities: vec!["text".into()],
                    options: json!({}),
                    variants: BTreeMap::new(),
                    default_variant: None,
                    pi_api: default_pi_api(),
                    pi_compat: default_pi_compat(),
                    inference_engine: None,
                    reasoning_parser: None,
                    model_revision: None,
                },
            )]),
        }
    }
}

impl RouteCatalog {
    pub fn load(config: &RuntimeConfig) -> Result<Self> {
        let path = config.routes_path();
        if !path.exists() {
            let catalog = Self::default();
            catalog.save(config)?;
            return Ok(catalog);
        }
        let body = fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
        let catalog: Self = serde_json::from_slice(&body)
            .with_context(|| format!("invalid route catalog {}", path.display()))?;
        catalog.validate()?;
        Ok(catalog)
    }

    pub fn save(&self, config: &RuntimeConfig) -> Result<()> {
        self.validate()?;
        let mut body = serde_json::to_vec_pretty(self)?;
        body.push(b'\n');
        write_private(&config.routes_path(), &body)
    }

    pub fn get(&self, name: &str) -> Result<&RouteConfig> {
        self.routes
            .get(name)
            .with_context(|| format!("unknown route {name}"))
    }

    fn validate(&self) -> Result<()> {
        if self.schema_version != 1 || self.routes.is_empty() {
            bail!("route catalog schema must be 1 and contain at least one route");
        }
        for (name, route) in &self.routes {
            validate_key(name, "route")?;
            if route.workspace.trim().is_empty() || route.pool_cap == 0 || route.pool_cap > 64 {
                bail!("route {name} has an invalid workspace or pool cap");
            }
            if let Some(model) = &route.model
                && (model.trim().is_empty() || route.context_limit == 0 || route.output_limit == 0)
            {
                bail!("model route {name} needs model and nonzero limits");
            }
            for variant in route.variants.keys() {
                validate_key(variant, "variant")?;
            }
            if let Some(default) = &route.default_variant
                && !route.variants.contains_key(default)
            {
                bail!("route {name} default variant {default} is not defined");
            }
            if route.pi_api.trim().is_empty() || !route.pi_compat.is_object() {
                bail!("route {name} has an invalid Pi provider configuration");
            }
            for (field, value) in [
                ("inference_engine", route.inference_engine.as_deref()),
                ("reasoning_parser", route.reasoning_parser.as_deref()),
                ("model_revision", route.model_revision.as_deref()),
            ] {
                if let Some(value) = value
                    && (value.trim().is_empty()
                        || value.len() > 256
                        || value.chars().any(char::is_control))
                {
                    bail!("route {name} has an invalid {field}");
                }
            }
        }
        Ok(())
    }
}

impl RouteConfig {
    pub fn resolve_variant(&self, requested: Option<Thinking>) -> Result<Option<String>> {
        let variant = requested
            .map(|value| value.as_str().to_string())
            .or_else(|| self.default_variant.clone());
        if let Some(name) = &variant
            && !self.variants.contains_key(name)
        {
            bail!("thinking variant {name} is not supported by this route");
        }
        Ok(variant)
    }

    pub fn opencode_config(&self) -> Result<Value> {
        let model = self.model.as_ref().context("this route has no model")?;
        Ok(json!({
            "$schema": "https://opencode.ai/config.json",
            "model": format!("openshell/{model}"),
            "provider": {
                "openshell": {
                    "npm": "@ai-sdk/openai-compatible",
                    "name": "OpenShell local route",
                    "options": {
                        "baseURL": "https://inference.local/v1",
                        "apiKey": "openshell",
                        "timeout": false
                    },
                    "models": {
                        model: {
                            "name": self.display_name,
                            "attachment": self.attachment,
                            "reasoning": self.reasoning,
                            "tool_call": true,
                            "interleaved": {"field": self.interleaved_field},
                            "modalities": {"input": self.input_modalities, "output": ["text"]},
                            "limit": {"context": self.context_limit, "output": self.output_limit},
                            "options": self.options,
                            "variants": self.variants
                        }
                    }
                }
            }
        }))
    }

    pub fn pi_models_config(&self) -> Result<Value> {
        let model = self.model.as_ref().context("this route has no model")?;
        let mut thinking_level_map = serde_json::Map::new();
        for (route_level, pi_level) in [
            ("off", "off"),
            ("low", "low"),
            ("medium", "medium"),
            ("on", "high"),
            ("xhigh", "xhigh"),
        ] {
            if let Some(variant) = self.variants.get(route_level) {
                let provider_level = variant
                    .get("reasoningEffort")
                    .and_then(Value::as_str)
                    .unwrap_or(route_level);
                thinking_level_map.insert(pi_level.into(), json!(provider_level));
            }
        }
        Ok(json!({
            "providers": {
                "openshell": {
                    "baseUrl": "https://inference.local/v1",
                    "apiKey": "openshell",
                    "api": self.pi_api,
                    "models": [{
                        "id": model,
                        "name": self.display_name,
                        "reasoning": self.reasoning,
                        "input": self.input_modalities,
                        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
                        "contextWindow": self.context_limit,
                        "maxTokens": self.output_limit,
                        "thinkingLevelMap": thinking_level_map,
                        "compat": self.pi_compat
                    }]
                }
            }
        }))
    }
}

fn validate_key(value: &str, field: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 64
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
        })
    {
        bail!("{field} names must be lowercase alphanumeric identifiers");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::RouteCatalog;

    #[test]
    fn default_catalog_is_safe_and_route_less() {
        let catalog = RouteCatalog::default();
        assert!(catalog.validate().is_ok());
        assert!(catalog.get("none").unwrap().model.is_none());
    }

    #[test]
    fn shipped_route_example_is_valid() {
        let catalog: RouteCatalog =
            serde_json::from_str(include_str!("../examples/routes.json")).unwrap();
        catalog.validate().unwrap();
        assert!(catalog.get("local-agent").unwrap().reasoning);
    }
}
