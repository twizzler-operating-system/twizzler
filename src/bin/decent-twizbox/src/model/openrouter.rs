use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use ureq::Agent;

use super::ModelClient;

/// Talks to any model available through [OpenRouter](https://openrouter.ai)'s
/// OpenAI-compatible `/chat/completions` endpoint over HTTPS.
///
/// The agent's whole prompt (rules, workspace context, transcript, request)
/// is sent as a single user message, matching how `OllamaModelClient` sends
/// it as a single raw completion prompt -- callers don't need to know which
/// backend they're talking to.
pub struct OpenRouterModelClient {
    model: String,
    base_url: String,
    api_key: String,
    temperature: f64,
    top_p: f64,
    agent: Agent,
}

impl OpenRouterModelClient {
    pub fn new(
        model: String,
        base_url: String,
        api_key: String,
        temperature: f64,
        top_p: f64,
        // Twizzler's socket layer doesn't implement read/write timeouts yet
        // (SocketKind::set_config only handles IO_REGISTER_SOCKET_FLAGS; any
        // other register, including the one set_read/write_timeout use,
        // fails with ArgumentError::InvalidArgument) -- so ureq's
        // `timeout_global` can't be wired up without every request failing
        // before it's even sent. Accepted for CLI/API compatibility but
        // currently unused.
        _timeout_seconds: u64,
    ) -> Self {
        let config = Agent::config_builder()
            .http_status_as_error(false)
            // Twizzler's smoltcp-backed socket layer breaks when
            // TCP_NODELAY is enabled (ureq's default); net-test found the
            // same thing. Keep Nagle's algorithm on.
            .no_delay(false)
            .build();
        Self {
            model,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
            temperature,
            top_p,
            agent: config.into(),
        }
    }
}

impl ModelClient for OpenRouterModelClient {
    fn complete(&self, prompt: &str, max_new_tokens: usize) -> Result<String> {
        let payload = json!({
            "model": self.model,
            "messages": [{"role": "user", "content": prompt}],
            "max_tokens": max_new_tokens,
            "temperature": self.temperature,
            "top_p": self.top_p,
            // Mirrors OllamaModelClient's `"think": false`: this agent's
            // reasoning already happens across tool-call turns, so an
            // internal chain-of-thought just eats into `max_tokens` and can
            // leave nothing for the actual <tool>/<final> reply. Some
            // models (e.g. Kimi's coder endpoint) reject `enabled: false`
            // outright with "reasoning is mandatory", so only turn it down
            // via `effort`, and drop the trace from the response since we
            // never read it.
            "reasoning": {"effort": "low", "exclude": true},
        });

        let mut response = self
            .agent
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", &format!("Bearer {}", self.api_key))
            // Recommended by OpenRouter for request attribution; harmless if ignored.
            .header("X-Title", "twizbox")
            .send_json(&payload)
            .with_context(|| {
                format!(
                    "Could not reach OpenRouter.\nBase URL: {}\nModel: {}",
                    self.base_url, self.model
                )
            })?;

        let status = response.status();
        let value: Value = response.body_mut().read_json().with_context(|| {
            format!("OpenRouter request failed with invalid JSON response, HTTP {status}")
        })?;

        if let Some(error) = value.get("error") {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            bail!("OpenRouter error: {message}");
        }
        if !status.is_success() {
            bail!("OpenRouter request failed with HTTP {status}: {value}");
        }

        let choice = value.get("choices").and_then(|choices| choices.get(0));
        let content = choice
            .and_then(|choice| choice.get("message"))
            .and_then(|message| message.get("content"))
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty());
        if let Some(content) = content {
            return Ok(content.to_string());
        }

        let finish_reason = choice
            .and_then(|choice| choice.get("finish_reason"))
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        if finish_reason == "length" {
            bail!(
                "OpenRouter model {} used its whole token budget ({max_new_tokens} max_tokens) without producing a reply (finish_reason=length). Try a higher --max-new-tokens, or a less reasoning-heavy model.",
                self.model
            );
        }
        bail!("OpenRouter response did not contain a completion (finish_reason={finish_reason}): {value}");
    }
}
