//! Calls to the lead and the methods specialist: request bodies, kept reasoning and fixed refusal codes.
use super::*;

/// How long a model call waits before it asks again after a server error.
const SERVER_ERROR_WAIT: Duration = Duration::from_secs(3);

pub(crate) struct Model {
    pub(super) client: Client,
    pub(super) config: ModelConfig,
    pub(super) url: Url,
}

pub(crate) struct Call {
    pub(super) value: Value,
    pub(crate) receipt: Value,
    /// The exact context text sent, kept with the decision.
    pub(crate) context: String,
    /// The reply's text, whose SHA-256 the receipt names as `content_sha256`, kept with the decision or with the
    /// deferral that refused it.
    pub(crate) content: String,
    /// The model's reasoning text, at most `MAX_KEPT_TEXT` bytes, kept beside the reply; the receipt
    /// names it as `reasoning_sha256`. Empty when the model gave none.
    pub(super) reasoning: String,
}

/// The exact request body a model call sends. The turn keeps its SHA-256 and the texts it is built
/// from, so the body can be rebuilt byte for byte from the record.
pub fn request_body(
    model: &str,
    system: &str,
    context: &str,
    max_tokens: usize,
) -> Result<Vec<u8>> {
    request_body_with(model, system, context, max_tokens, &serde_json::Map::new())
}

/// `request_body` with the model config's request settings, which replace the default temperature and add
/// their own fields. The receipt keeps the settings beside the texts' digests, so a body with settings
/// rebuilds byte for byte too; with none the body is exactly `request_body`'s.
pub fn request_body_with(
    model: &str,
    system: &str,
    context: &str,
    max_tokens: usize,
    settings: &serde_json::Map<String, Value>,
) -> Result<Vec<u8>> {
    let mut body = json!({"model":model,"messages":[{"role":"system","content":system},{"role":"user","content":context}],
        "temperature":0,"max_tokens":max_tokens,"response_format":{"type":"json_object"},"stream":false});
    for (key, value) in settings {
        body[key.as_str()] = value.clone();
    }
    Ok(serde_json::to_vec(&body)?)
}

impl Model {
    pub(crate) fn load(path: &PathBuf) -> Result<Self> {
        let config: ModelConfig = serde_json::from_slice(&private_bytes(path, 16384)?)
            .map_err(|_| anyhow!("model_config_invalid"))?;
        let url = Url::parse(&config.endpoint).map_err(|_| anyhow!("model_config_invalid"))?;
        let internal = url.host_str().is_some_and(|h| {
            h.ends_with(".svc.cluster.local")
                || h == "localhost"
                || h == "127.0.0.1"
                || h == "[::1]"
        });
        if config.schema_version != 1
            || !clean_text(&config.model, 128)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.host_str().is_none()
            || !(url.scheme() == "https"
                || (url.scheme() == "http" && internal && config.allow_internal_http))
            || !(SYSTEM.len() + REVIEW_RESERVE + LEAD_BUDGET_MIN..=MAX_INPUT_BYTES)
                .contains(&config.max_input_bytes)
            || !(512..=65536).contains(&config.max_output_tokens)
            || config.max_input_bytes + config.max_output_tokens + 1024
                > config.context_window_tokens
            || !config.sections_valid()
            || !(10..=900).contains(&config.timeout_seconds)
            || !settings_valid(&config.settings)
        {
            bail!("model_config_invalid")
        }
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(config.timeout_seconds))
            .build()
            .map_err(|_| anyhow!("model_client_invalid"))?;
        Ok(Self {
            client,
            config,
            url,
        })
    }
    /// A call that asks again when the reply cannot be read (`READ_ATTEMPTS`). The receipt of the
    /// reply that was read names the faults of the replies before it.
    pub(super) async fn readable(&self, role: &str, system: &str, context: &Value) -> Result<Call> {
        self.readable_within(role, system, context, self.config.max_input_bytes)
            .await
    }

    /// `readable` within an input ceiling other than the model's own: a lead call that also carries what the lead read.
    /// The ceiling never passes `MAX_INPUT_BYTES`.
    pub(super) async fn readable_within(
        &self,
        role: &str,
        system: &str,
        context: &Value,
        ceiling: usize,
    ) -> Result<Call> {
        let mut earlier = Vec::new();
        loop {
            let asked = with_unreadable_note(context, &earlier);
            match self
                .call_within(role, system, asked.as_ref().unwrap_or(context), ceiling)
                .await
            {
                Ok(mut call) => {
                    if !earlier.is_empty() {
                        call.receipt["unreadable_before"] = json!(earlier);
                    }
                    return Ok(call);
                }
                Err(error) => match ask_again(&error, earlier.len() + 1) {
                    Some(fault) => earlier.push(fault),
                    None => return Err(error),
                },
            }
        }
    }
    pub(crate) async fn call(&self, role: &str, system: &str, context: &Value) -> Result<Call> {
        self.call_within(role, system, context, self.config.max_input_bytes)
            .await
    }

    pub(crate) async fn call_within(
        &self,
        role: &str,
        system: &str,
        context: &Value,
        ceiling: usize,
    ) -> Result<Call> {
        let context = serde_json::to_string(context)?;
        // UTF-8 bytes plus a fixed wrapper allowance is deliberately conservative.
        // The context window is an operator-declared model contract, not inferred
        // from a display name. Actual token usage is recorded when returned.
        if context.len() + system.len() > ceiling.min(MAX_INPUT_BYTES) {
            bail!("context_budget_too_small")
        }
        let body = request_body_with(
            &self.config.model,
            system,
            &context,
            self.config.max_output_tokens,
            &self.config.settings,
        )?;
        let request_sha256 = sha(&body);
        // A server error is asked again once with the same body after a short wait: the reasoner's structured-output
        // decoding can end a request with an internal error (three in one suite attempt, one of which cost
        // a goal its run), and the fault is not the run's. A transport failure or timeout is not asked again, so a call
        // never waits twice its timeout. The receipt names the status of the request asked again.
        let mut retried = None;
        let response = loop {
            let mut request = self
                .client
                .post(self.url.clone())
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body.clone());
            if let Some(path) = &self.config.credential_file {
                let secret = private_bytes(path, 16384)?;
                let secret = std::str::from_utf8(&secret)
                    .map_err(|_| anyhow!("model_credential_invalid"))?
                    .trim();
                if secret.is_empty() || secret.contains(['\r', '\n']) {
                    bail!("model_credential_invalid")
                }
                request = request.bearer_auth(secret);
            }
            let response = request
                .send()
                .await
                .map_err(|_| anyhow!("model_unavailable"))?;
            let status = response.status();
            if status.is_success() {
                break response;
            }
            if status.is_server_error() && retried.is_none() {
                retried = Some(status.as_u16());
                tracing::warn!(
                    role,
                    status = status.as_u16(),
                    "model call asked again after a server error"
                );
                sleep(SERVER_ERROR_WAIT).await;
                continue;
            }
            bail!("model_unavailable")
        };
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| anyhow!("model_unavailable"))?;
            // A reply at 65,536 output tokens, with its reasoning, can pass 256 KiB.
            if bytes.len() + chunk.len() > 1024 * 1024 {
                bail!("model_response_too_large")
            };
            bytes.extend_from_slice(&chunk);
        }
        let envelope: Value =
            serde_json::from_slice(&bytes).map_err(|_| anyhow!("model_envelope_invalid"))?;
        let choice = &envelope["choices"][0];
        let usage = &envelope["usage"];
        let prompt = usage["prompt_tokens"].as_u64();
        let completion = usage["completion_tokens"].as_u64();
        let content = choice["message"]["content"].as_str();
        let read = read_finished_reply(choice["finish_reason"].as_str(), content);
        let (value, normalized) = match read {
            Ok(read) => read,
            Err(fault) => {
                // Sizes and fixed words only: a reply's text never reaches a log.
                let finish = match choice["finish_reason"].as_str() {
                    Some(f @ ("stop" | "length" | "content_filter" | "tool_calls")) => f,
                    Some(_) => "other",
                    None => "absent",
                };
                let reasoning_bytes = ["reasoning", "reasoning_content"]
                    .iter()
                    .find_map(|k| choice["message"][*k].as_str())
                    .map_or(0, str::len);
                tracing::warn!(
                    role,
                    fault,
                    finish,
                    content_bytes = content.map_or(0, str::len),
                    reasoning_bytes,
                    prompt_tokens = prompt.unwrap_or(0),
                    completion_tokens = completion.unwrap_or(0),
                    "model reply unreadable"
                );
                return Err(unreadable_reply(role, fault).into());
            }
        };
        let content = content.unwrap_or_default();
        let model = envelope["model"]
            .as_str()
            .filter(|s| clean_text(s, 256))
            .ok_or_else(|| anyhow!("model_identity_invalid"))?;
        if prompt
            .zip(completion)
            .is_some_and(|(p, c)| p + c > self.config.context_window_tokens as u64)
        {
            bail!("model_budget_exceeded")
        }
        let mut receipt = json!({"role":role,"model_role":self.config.model,"reported_model":model,
            "context_sha256":sha(context.as_bytes()),"system_sha256":sha(system.as_bytes()),"content_sha256":sha(content.as_bytes()),
            "request_sha256":request_sha256,"max_output_tokens":self.config.max_output_tokens,
            "prompt_tokens":prompt,"completion_tokens":completion,"finish_reason":"stop","capture":"validated_structured_content_only"});
        if let Some(wrapper) = normalized {
            receipt["normalized"] = json!(wrapper);
        }
        if let Some(status) = retried {
            receipt["server_error_before"] = json!(status);
        }
        if !self.config.settings.is_empty() {
            receipt["settings"] = Value::Object(self.config.settings.clone());
        }
        let (reasoning, truncated, reasoning_bytes) = kept_reasoning(&choice["message"]);
        if !reasoning.is_empty() {
            receipt["reasoning_sha256"] = json!(sha(reasoning.as_bytes()));
            receipt["reasoning_bytes"] = json!(reasoning_bytes);
            if truncated {
                receipt["reasoning_truncated"] = json!(true);
            }
        }
        Ok(Call {
            value,
            receipt,
            context,
            content: content.to_owned(),
            reasoning,
        })
    }
}

pub(super) fn sha(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

/// A reply's reasoning as kept: the `reasoning` or `reasoning_content` text without NUL characters (the kept bytes
/// are read back as PostgreSQL text, which cannot hold one), cut at a character boundary to `MAX_KEPT_TEXT` bytes,
/// with whether it was cut and its full size.
pub(super) fn kept_reasoning(message: &Value) -> (String, bool, usize) {
    let Some(text) = ["reasoning", "reasoning_content"]
        .iter()
        .find_map(|k| message[*k].as_str())
        .map(|t| t.replace('\0', ""))
        .filter(|t| !t.trim().is_empty())
    else {
        return (String::new(), false, 0);
    };
    if text.len() <= MAX_KEPT_TEXT {
        let size = text.len();
        return (text, false, size);
    }
    let mut end = MAX_KEPT_TEXT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), true, text.len())
}

/// A fixed reason code as the run events keep it (`^[a-z][a-z0-9_]{1,63}$`), never free text.
pub(super) fn fixed_code(code: &str) -> bool {
    (2..=64).contains(&code.len())
        && code.starts_with(|c: char| c.is_ascii_lowercase())
        && code
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

pub(super) fn clean_text(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.contains('\0')
}

/// What is wrong with a ledger text the lead must give, in fixed words: missing, empty, a null character, or its length
/// in characters against the limit (a byte count refused text within the stated
/// limit, and an overlong reason read as an absent one).
pub(super) fn text_fault(value: Option<&str>, max: usize) -> Option<String> {
    let Some(value) = value else {
        return Some("missing".to_owned());
    };
    if value.trim().is_empty() {
        return Some("empty".to_owned());
    }
    if value.contains('\0') {
        return Some("a null character".to_owned());
    }
    let count = value.chars().count();
    (count > max).then(|| format!("{count} characters, at most {max}"))
}
