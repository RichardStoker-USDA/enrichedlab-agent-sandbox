use std::collections::BTreeMap;

use anyhow::{Result, bail};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const MAX_RETURNED_CONTENT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum HarnessKind {
    #[default]
    OpenCode,
    Pi,
    OpenAiCompatible,
    Native,
}

impl HarnessKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenCode => "opencode",
            Self::Pi => "pi",
            Self::OpenAiCompatible => "openai-compatible",
            Self::Native => "native",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceVisibility {
    Metadata,
    Restricted,
}

#[derive(Clone, Debug, Serialize)]
pub struct CanonicalHarnessEvent {
    pub kind: String,
    pub evidence_role: String,
    pub visibility: EvidenceVisibility,
    pub source_event_id: Option<String>,
    pub parent_source_event_id: Option<String>,
    pub metadata: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restricted_content: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct UsageDelta {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub total_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct HarnessLineObservation {
    pub events: Vec<CanonicalHarnessEvent>,
    pub malformed: bool,
    pub tool_calls: u64,
    pub step_finishes: u64,
    pub errors: u64,
    pub usage: UsageDelta,
}

#[derive(Default)]
struct OpenAiBuffer {
    reasoning: String,
    text: String,
    tool_arguments: BTreeMap<String, String>,
}

pub struct HarnessEventAdapter {
    kind: HarnessKind,
    openai: OpenAiBuffer,
}

impl HarnessEventAdapter {
    pub fn new(kind: HarnessKind) -> Self {
        Self {
            kind,
            openai: OpenAiBuffer::default(),
        }
    }

    pub fn kind(&self) -> HarnessKind {
        self.kind
    }

    pub fn observe_line(&mut self, line: &[u8]) -> Result<HarnessLineObservation> {
        let Some(line) = normalized_wire_line(self.kind, line) else {
            return Ok(HarnessLineObservation::default());
        };
        let value: Value = match serde_json::from_slice(line) {
            Ok(value) => value,
            Err(_) => {
                return Ok(HarnessLineObservation {
                    malformed: true,
                    ..HarnessLineObservation::default()
                });
            }
        };
        match self.kind {
            HarnessKind::OpenCode => observe_opencode(&value),
            HarnessKind::Pi => observe_pi(&value),
            HarnessKind::OpenAiCompatible => self.observe_openai(&value),
            HarnessKind::Native => observe_native(&value),
        }
    }

    pub fn finish(&mut self) -> Result<Vec<CanonicalHarnessEvent>> {
        if self.kind != HarnessKind::OpenAiCompatible {
            return Ok(Vec::new());
        }
        flush_openai(&mut self.openai)
    }

    fn observe_openai(&mut self, value: &Value) -> Result<HarnessLineObservation> {
        let mut observation = HarnessLineObservation::default();
        let choice = value
            .pointer("/choices/0")
            .or_else(|| value.pointer("/output/0"));
        if let Some(choice) = choice {
            let content = choice
                .get("delta")
                .or_else(|| choice.get("message"))
                .unwrap_or(choice);
            append_bounded(
                &mut self.openai.reasoning,
                first_string(
                    content,
                    &["reasoning_content", "reasoning", "reasoning_text"],
                ),
            )?;
            append_bounded(
                &mut self.openai.text,
                content.get("content").and_then(Value::as_str),
            )?;
            if let Some(calls) = content.get("tool_calls").and_then(Value::as_array) {
                for call in calls {
                    let index = call
                        .get("index")
                        .and_then(Value::as_u64)
                        .map(|value| value.to_string())
                        .or_else(|| call.get("id").and_then(Value::as_str).map(str::to_owned))
                        .unwrap_or_else(|| "0".into());
                    if let Some(arguments) =
                        call.pointer("/function/arguments").and_then(Value::as_str)
                    {
                        let target = self.openai.tool_arguments.entry(index).or_default();
                        append_bounded(target, Some(arguments))?;
                    }
                }
            }
            if choice
                .get("finish_reason")
                .is_some_and(|value| !value.is_null())
            {
                observation.events.extend(flush_openai(&mut self.openai)?);
                observation.step_finishes = 1;
            }
        }
        observation.usage = usage_from_openai(value);
        if observation.step_finishes > 0 {
            observation.events.push(step_event(&observation.usage));
        }
        if let Some(error) = value.get("error") {
            observation.errors = 1;
            observation.events.push(restricted_json_event(
                "agent.error.observed",
                "error",
                error,
                json!({"source": "openai-compatible"}),
            )?);
        }
        Ok(observation)
    }
}

fn normalized_wire_line(kind: HarnessKind, line: &[u8]) -> Option<&[u8]> {
    let line = line.trim_ascii();
    if kind != HarnessKind::OpenAiCompatible {
        return Some(line);
    }
    if line.starts_with(b":")
        || line.starts_with(b"event:")
        || line.starts_with(b"id:")
        || line.starts_with(b"retry:")
    {
        return None;
    }
    let payload = line
        .strip_prefix(b"data:")
        .map(<[u8]>::trim_ascii)
        .unwrap_or(line);
    if payload == b"[DONE]" {
        None
    } else {
        Some(payload)
    }
}

fn observe_opencode(value: &Value) -> Result<HarnessLineObservation> {
    let mut observation = HarnessLineObservation::default();
    let event_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match event_type {
        "tool_use" => {
            let part = value.get("part").unwrap_or(&Value::Null);
            let state = part.get("state").unwrap_or(&Value::Null);
            let tool = bounded_text(part.get("tool"), 128, "unknown");
            let status = bounded_text(state.get("status"), 64, "unknown");
            let content = json!({
                "input": state.get("input"),
                "output": state.get("output"),
                "metadata": state.get("metadata")
            });
            observation.events.push(restricted_json_event(
                "agent.tool.observed",
                "tool-io",
                &content,
                json!({"tool": tool, "status": status}),
            )?);
            observation.tool_calls = 1;
        }
        "step_finish" => {
            observation.usage = usage_from_opencode(value.pointer("/part/tokens"));
            observation.events.push(step_event(&observation.usage));
            observation.step_finishes = 1;
        }
        "text" | "reasoning" => {
            let part = value.get("part").unwrap_or(&Value::Null);
            let part_type = part
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or(event_type);
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                observation.events.push(returned_content_event(
                    part_type == "reasoning" || event_type == "reasoning",
                    text,
                    "opencode",
                )?);
            }
        }
        "error" | "session.error" => {
            observation.events.push(restricted_json_event(
                "agent.error.observed",
                "error",
                value,
                json!({"source": "opencode"}),
            )?);
            observation.errors = 1;
        }
        "message.part.updated" => {
            if let Some(part) = value.pointer("/properties/part") {
                observe_opencode_part(part, &mut observation)?;
            }
        }
        "session.next.tool.called" | "session.next.tool.success" | "session.next.tool.failed" => {
            observation.events.push(restricted_json_event(
                "agent.tool.observed",
                "tool-io",
                value.get("properties").unwrap_or(&Value::Null),
                json!({"source_type": event_type}),
            )?);
            if event_type == "session.next.tool.called" {
                observation.tool_calls = 1;
            }
            if event_type == "session.next.tool.failed" {
                observation.errors = 1;
            }
        }
        "session.next.step.ended" => {
            observation.usage = usage_from_opencode(value.pointer("/properties/tokens"));
            observation.events.push(step_event(&observation.usage));
            observation.step_finishes = 1;
        }
        "session.next.reasoning.ended" | "session.next.text.ended" => {
            if let Some(text) = value
                .pointer("/properties/text")
                .or_else(|| value.pointer("/properties/content"))
                .and_then(Value::as_str)
            {
                observation.events.push(returned_content_event(
                    event_type.contains("reasoning"),
                    text,
                    "opencode",
                )?);
            }
        }
        "session.next.compaction.started"
        | "session.next.compaction.ended"
        | "session.compacted" => {
            observation.events.push(metadata_event(
                "agent.context.compaction",
                "context",
                json!({"source_type": event_type}),
            ));
        }
        _ => {}
    }
    Ok(observation)
}

fn observe_opencode_part(part: &Value, observation: &mut HarnessLineObservation) -> Result<()> {
    match part.get("type").and_then(Value::as_str) {
        Some("reasoning") | Some("text") => {
            if let Some(text) = part.get("text").and_then(Value::as_str) {
                observation.events.push(returned_content_event(
                    part.get("type").and_then(Value::as_str) == Some("reasoning"),
                    text,
                    "opencode",
                )?);
            }
        }
        Some("tool") => {
            observation.events.push(restricted_json_event(
                "agent.tool.observed",
                "tool-io",
                part,
                json!({"tool": bounded_text(part.get("tool"), 128, "unknown")}),
            )?);
            observation.tool_calls = 1;
        }
        Some("step-finish") => {
            observation.usage = usage_from_opencode(part.get("tokens"));
            observation.events.push(step_event(&observation.usage));
            observation.step_finishes = 1;
        }
        _ => {}
    }
    Ok(())
}

fn observe_pi(value: &Value) -> Result<HarnessLineObservation> {
    let mut observation = HarnessLineObservation::default();
    let event_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match event_type {
        "session" => observation.events.push(metadata_event(
            "harness.session.started",
            "session",
            json!({
                "session_id": value.get("id"),
                "wire_version": value.get("version"),
                "cwd_sha256": value.get("cwd").and_then(Value::as_str).map(hash_text)
            }),
        )),
        "agent_start" | "turn_start" => observation.events.push(metadata_event(
            if event_type == "agent_start" {
                "agent.segment.started"
            } else {
                "agent.turn.started"
            },
            "lifecycle",
            json!({"source": "pi"}),
        )),
        "turn_end" => {
            observation.usage = usage_from_pi(value.pointer("/message/usage"));
            observation.events.push(step_event(&observation.usage));
            observation.step_finishes = 1;
        }
        "message_end" => {
            if value.pointer("/message/role").and_then(Value::as_str) == Some("assistant") {
                if let Some(blocks) = value.pointer("/message/content").and_then(Value::as_array) {
                    for block in blocks {
                        match block.get("type").and_then(Value::as_str) {
                            Some("thinking") => {
                                if let Some(text) = block.get("thinking").and_then(Value::as_str) {
                                    observation
                                        .events
                                        .push(returned_content_event(true, text, "pi")?);
                                }
                            }
                            Some("text") => {
                                if let Some(text) = block.get("text").and_then(Value::as_str) {
                                    observation
                                        .events
                                        .push(returned_content_event(false, text, "pi")?);
                                }
                            }
                            Some("toolCall") => observation.events.push(restricted_json_event(
                                "agent.tool.requested",
                                "tool-io",
                                block,
                                json!({"tool": bounded_text(block.get("name"), 128, "unknown")}),
                            )?),
                            _ => {}
                        }
                    }
                }
                let stop_reason = value
                    .pointer("/message/stopReason")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if matches!(stop_reason, "error" | "aborted") {
                    observation.errors = 1;
                }
            }
        }
        "tool_execution_start" | "tool_execution_end" => {
            let content = if event_type == "tool_execution_start" {
                value.get("args").unwrap_or(&Value::Null)
            } else {
                value.get("result").unwrap_or(&Value::Null)
            };
            observation.events.push(restricted_json_event(
                if event_type == "tool_execution_start" {
                    "agent.tool.started"
                } else {
                    "agent.tool.finished"
                },
                "tool-io",
                content,
                json!({
                    "tool": bounded_text(value.get("toolName"), 128, "unknown"),
                    "call_id_sha256": value.get("toolCallId").and_then(Value::as_str).map(hash_text),
                    "is_error": value.get("isError").and_then(Value::as_bool).unwrap_or(false)
                }),
            )?);
            if event_type == "tool_execution_start" {
                observation.tool_calls = 1;
            }
            if value.get("isError").and_then(Value::as_bool) == Some(true) {
                observation.errors = 1;
            }
        }
        "compaction_start" | "compaction_end" => observation.events.push(metadata_event(
            "agent.context.compaction",
            "context",
            json!({"source_type": event_type}),
        )),
        "auto_retry_start" | "auto_retry_end" => observation.events.push(metadata_event(
            "agent.retry.observed",
            "lifecycle",
            json!({"source_type": event_type}),
        )),
        _ => {}
    }
    Ok(observation)
}

fn observe_native(value: &Value) -> Result<HarnessLineObservation> {
    let event: NativeWireEvent = serde_json::from_value(value.clone())?;
    if event.schema_version != 1 {
        bail!("unsupported native harness event schema");
    }
    Ok(HarnessLineObservation {
        events: vec![CanonicalHarnessEvent {
            kind: event.kind,
            evidence_role: event.evidence_role,
            visibility: event.visibility,
            source_event_id: event.source_event_id,
            parent_source_event_id: event.parent_source_event_id,
            metadata: event.metadata,
            restricted_content: event.restricted_content,
        }],
        ..HarnessLineObservation::default()
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeWireEvent {
    schema_version: u32,
    kind: String,
    evidence_role: String,
    visibility: EvidenceVisibility,
    source_event_id: Option<String>,
    parent_source_event_id: Option<String>,
    #[serde(default)]
    metadata: Value,
    restricted_content: Option<String>,
}

fn flush_openai(buffer: &mut OpenAiBuffer) -> Result<Vec<CanonicalHarnessEvent>> {
    let mut events = Vec::new();
    if !buffer.reasoning.is_empty() {
        events.push(returned_content_event(
            true,
            &std::mem::take(&mut buffer.reasoning),
            "openai-compatible",
        )?);
    }
    if !buffer.text.is_empty() {
        events.push(returned_content_event(
            false,
            &std::mem::take(&mut buffer.text),
            "openai-compatible",
        )?);
    }
    for (call, arguments) in std::mem::take(&mut buffer.tool_arguments) {
        events.push(CanonicalHarnessEvent {
            kind: "agent.tool.requested".into(),
            evidence_role: "tool-io".into(),
            visibility: EvidenceVisibility::Restricted,
            source_event_id: None,
            parent_source_event_id: None,
            metadata: json!({
                "call_id_sha256": hash_text(&call),
                "bytes": arguments.len(),
                "sha256": hash_text(&arguments)
            }),
            restricted_content: Some(arguments),
        });
    }
    Ok(events)
}

fn returned_content_event(
    reasoning: bool,
    text: &str,
    source: &str,
) -> Result<CanonicalHarnessEvent> {
    if text.len() > MAX_RETURNED_CONTENT_BYTES {
        bail!("provider-returned content exceeds the per-event evidence limit");
    }
    Ok(CanonicalHarnessEvent {
        kind: if reasoning {
            "agent.reasoning.returned"
        } else {
            "agent.output.returned"
        }
        .into(),
        evidence_role: if reasoning { "reasoning" } else { "output" }.into(),
        visibility: EvidenceVisibility::Restricted,
        source_event_id: None,
        parent_source_event_id: None,
        metadata: json!({
            "source": source,
            "bytes": text.len(),
            "sha256": hash_text(text),
            "provider_emitted": true,
            "internal_activations_captured": false,
            "content_in_prompt_state": false
        }),
        restricted_content: Some(text.into()),
    })
}

fn restricted_json_event(
    kind: &str,
    role: &str,
    content: &Value,
    mut metadata: Value,
) -> Result<CanonicalHarnessEvent> {
    let content = serde_json::to_string(content)?;
    if content.len() > MAX_RETURNED_CONTENT_BYTES {
        bail!("harness evidence exceeds the per-event limit");
    }
    if let Some(object) = metadata.as_object_mut() {
        object.insert("bytes".into(), json!(content.len()));
        object.insert("sha256".into(), json!(hash_text(&content)));
    }
    Ok(CanonicalHarnessEvent {
        kind: kind.into(),
        evidence_role: role.into(),
        visibility: EvidenceVisibility::Restricted,
        source_event_id: None,
        parent_source_event_id: None,
        metadata,
        restricted_content: Some(content),
    })
}

fn metadata_event(kind: &str, role: &str, metadata: Value) -> CanonicalHarnessEvent {
    CanonicalHarnessEvent {
        kind: kind.into(),
        evidence_role: role.into(),
        visibility: EvidenceVisibility::Metadata,
        source_event_id: None,
        parent_source_event_id: None,
        metadata,
        restricted_content: None,
    }
}

fn step_event(usage: &UsageDelta) -> CanonicalHarnessEvent {
    metadata_event(
        "agent.step.observed",
        "usage",
        serde_json::to_value(usage).expect("usage serialization is infallible"),
    )
}

fn usage_from_opencode(tokens: Option<&Value>) -> UsageDelta {
    let tokens = tokens.unwrap_or(&Value::Null);
    let input = tokens.get("input").and_then(Value::as_u64).unwrap_or(0);
    let output = tokens.get("output").and_then(Value::as_u64).unwrap_or(0);
    let reasoning = tokens.get("reasoning").and_then(Value::as_u64).unwrap_or(0);
    let cache_read = tokens
        .pointer("/cache/read")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cache_write = tokens
        .pointer("/cache/write")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let derived = input
        .saturating_add(output)
        .saturating_add(reasoning)
        .saturating_add(cache_read)
        .saturating_add(cache_write);
    UsageDelta {
        input_tokens: input,
        output_tokens: output,
        reasoning_tokens: reasoning,
        total_tokens: tokens
            .get("total")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .max(derived),
        cache_read_tokens: cache_read,
        cache_write_tokens: cache_write,
    }
}

fn usage_from_pi(usage: Option<&Value>) -> UsageDelta {
    let usage = usage.unwrap_or(&Value::Null);
    let input = usage.get("input").and_then(Value::as_u64).unwrap_or(0);
    let output = usage.get("output").and_then(Value::as_u64).unwrap_or(0);
    let cache_read = usage.get("cacheRead").and_then(Value::as_u64).unwrap_or(0);
    let cache_write = usage.get("cacheWrite").and_then(Value::as_u64).unwrap_or(0);
    let reasoning = usage.get("reasoning").and_then(Value::as_u64).unwrap_or(0);
    UsageDelta {
        input_tokens: input,
        output_tokens: output,
        reasoning_tokens: reasoning,
        total_tokens: usage
            .get("totalTokens")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| {
                input
                    .saturating_add(output)
                    .saturating_add(reasoning)
                    .saturating_add(cache_read)
                    .saturating_add(cache_write)
            }),
        cache_read_tokens: cache_read,
        cache_write_tokens: cache_write,
    }
}

fn usage_from_openai(value: &Value) -> UsageDelta {
    let usage = value.get("usage").unwrap_or(&Value::Null);
    let input = usage
        .get("prompt_tokens")
        .or_else(|| usage.get("input_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output = usage
        .get("completion_tokens")
        .or_else(|| usage.get("output_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let reasoning = usage
        .pointer("/completion_tokens_details/reasoning_tokens")
        .or_else(|| usage.get("reasoning_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    UsageDelta {
        input_tokens: input,
        output_tokens: output,
        reasoning_tokens: reasoning,
        total_tokens: usage
            .get("total_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| input.saturating_add(output)),
        cache_read_tokens: usage
            .pointer("/prompt_tokens_details/cached_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_write_tokens: 0,
    }
}

fn first_string<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
}

fn append_bounded(target: &mut String, value: Option<&str>) -> Result<()> {
    if let Some(value) = value {
        if target.len().saturating_add(value.len()) > MAX_RETURNED_CONTENT_BYTES {
            bail!("streamed provider content exceeds the per-event evidence limit");
        }
        target.push_str(value);
    }
    Ok(())
}

fn bounded_text(value: Option<&Value>, max_bytes: usize, fallback: &str) -> String {
    let text = value.and_then(Value::as_str).unwrap_or(fallback);
    if text.len() <= max_bytes {
        text.into()
    } else {
        format!("sha256:{}", hash_text(text))
    }
}

fn hash_text(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::{HarnessEventAdapter, HarnessKind};

    #[test]
    fn pi_jsonl_returns_reasoning_and_tool_evidence() {
        let mut adapter = HarnessEventAdapter::new(HarnessKind::Pi);
        let reasoning = adapter
            .observe_line(br#"{"type":"message_end","message":{"role":"assistant","content":[{"type":"thinking","thinking":"check the schema"},{"type":"text","text":"done"}],"stopReason":"stop"}}"#)
            .unwrap();
        assert_eq!(reasoning.events.len(), 2);
        assert_eq!(reasoning.events[0].kind, "agent.reasoning.returned");
        assert_eq!(
            reasoning.events[0].restricted_content.as_deref(),
            Some("check the schema")
        );

        let tool = adapter
            .observe_line(br#"{"type":"tool_execution_end","toolCallId":"call-1","toolName":"bash","result":{"content":"ok"},"isError":false}"#)
            .unwrap();
        assert_eq!(tool.events[0].kind, "agent.tool.finished");
        assert!(tool.events[0].restricted_content.is_some());
    }

    #[test]
    fn opencode_legacy_stream_stays_supported() {
        let mut adapter = HarnessEventAdapter::new(HarnessKind::OpenCode);
        let step = adapter
            .observe_line(br#"{"type":"step_finish","part":{"tokens":{"input":10,"output":4,"reasoning":2,"total":16,"cache":{"read":3,"write":0}}}}"#)
            .unwrap();
        assert_eq!(step.step_finishes, 1);
        assert_eq!(step.usage.total_tokens, 19);
    }

    #[test]
    fn openai_reasoning_is_flushed_as_restricted_evidence() {
        let mut adapter = HarnessEventAdapter::new(HarnessKind::OpenAiCompatible);
        let first = adapter
            .observe_line(
                br#"{"choices":[{"delta":{"reasoning_content":"inspect "},"finish_reason":null}]}"#,
            )
            .unwrap();
        assert!(first.events.is_empty());
        let last = adapter
            .observe_line(br#"{"choices":[{"delta":{"reasoning_content":"input","content":"answer"},"finish_reason":"stop"}],"usage":{"prompt_tokens":7,"completion_tokens":3,"total_tokens":10}}"#)
            .unwrap();
        assert_eq!(
            last.events[0].restricted_content.as_deref(),
            Some("inspect input")
        );
        assert_eq!(last.events[1].restricted_content.as_deref(), Some("answer"));
        assert_eq!(last.step_finishes, 1);
    }

    #[test]
    fn openai_adapter_accepts_standard_sse_lines() {
        let mut adapter = HarnessEventAdapter::new(HarnessKind::OpenAiCompatible);
        assert!(
            adapter
                .observe_line(b"event: message")
                .unwrap()
                .events
                .is_empty()
        );
        let event = adapter
            .observe_line(
                br#"data: {"choices":[{"delta":{"content":"done"},"finish_reason":"stop"}]}"#,
            )
            .unwrap();
        assert_eq!(event.events[0].restricted_content.as_deref(), Some("done"));
        assert!(
            adapter
                .observe_line(b"data: [DONE]")
                .unwrap()
                .events
                .is_empty()
        );
    }

    #[test]
    fn native_events_are_strict_and_preserve_source_lineage() {
        let mut adapter = HarnessEventAdapter::new(HarnessKind::Native);
        let event = adapter
            .observe_line(br#"{"schema_version":1,"kind":"agent.tool.finished","evidence_role":"tool-io","visibility":"restricted","source_event_id":"tool-1","parent_source_event_id":"step-1","metadata":{"exit_code":0},"restricted_content":"bounded output"}"#)
            .unwrap();
        assert_eq!(event.events.len(), 1);
        assert_eq!(event.events[0].source_event_id.as_deref(), Some("tool-1"));
        assert_eq!(
            event.events[0].parent_source_event_id.as_deref(),
            Some("step-1")
        );
        assert_eq!(
            event.events[0].restricted_content.as_deref(),
            Some("bounded output")
        );

        let unknown = adapter.observe_line(br#"{"schema_version":1,"kind":"agent.tool.finished","evidence_role":"tool-io","visibility":"metadata","source_event_id":null,"parent_source_event_id":null,"metadata":{},"restricted_content":null,"unreviewed":true}"#);
        assert!(unknown.is_err());
    }
}
