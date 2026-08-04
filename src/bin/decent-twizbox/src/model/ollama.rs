use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use ureq::Agent;

use super::ModelClient;

/// Talks to a local Ollama server's `/api/generate` text-completion endpoint.
pub struct OllamaModelClient {
    model: String,
    host: String,
    temperature: f64,
    top_p: f64,
    agent: Agent,
}

impl OllamaModelClient {
    pub fn new(
        model: String,
        host: String,
        temperature: f64,
        top_p: f64,
        // Twizzler's socket layer doesn't implement read/write timeouts yet
        // (see OpenRouterModelClient), so ureq's `timeout_global` can't be
        // wired up without every request failing before it's even sent.
        // Accepted for CLI/API compatibility but currently unused.
        _timeout_seconds: u64,
    ) -> Self {
        let config = Agent::config_builder()
            // Ollama's error payloads live in the JSON body even on non-2xx
            // responses; read the body ourselves instead of having ureq
            // turn the status into an opaque error.
            .http_status_as_error(false)
            // See OpenRouterModelClient: Twizzler's smoltcp socket layer
            // breaks when TCP_NODELAY is enabled (ureq's default).
            .no_delay(false)
            .build();
        Self {
            model,
            host: host.trim_end_matches('/').to_string(),
            temperature,
            top_p,
            agent: config.into(),
        }
    }
}

impl ModelClient for OllamaModelClient {
    fn complete(&self, prompt: &str, max_new_tokens: usize) -> Result<String> {
        let payload = json!({
            "model": self.model,
            "prompt": prompt,
            "stream": false,
            "raw": false,
            "think": false,
            "options": {
                "num_predict": max_new_tokens,
                "temperature": self.temperature,
                "top_p": self.top_p,
            }
        });

        let mut response = self
            .agent
            .post(format!("{}/api/generate", self.host))
            .send_json(&payload)
            .with_context(|| {
                format!(
                    "Could not reach Ollama.\nMake sure `ollama serve` is running and the model is available.\nHost: {}\nModel: {}",
                    self.host, self.model
                )
            })?;

        let status = response.status();
        let value: Value = response
            .body_mut()
            .read_json()
            .with_context(|| format!("Ollama request failed with invalid JSON response, HTTP {status}"))?;

        if !status.is_success() {
            bail!("Ollama request failed with HTTP {status}: {value}");
        }
        if let Some(error) = value.get("error").and_then(Value::as_str) {
            bail!("Ollama error: {error}");
        }
        Ok(value
            .get("response")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
    }
}
