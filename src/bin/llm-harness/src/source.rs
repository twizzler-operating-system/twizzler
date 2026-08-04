//! Where model turns come from.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::transcript::{Role, Transcript};

/// Produces the next model turn given the conversation so far.
pub trait TokenSource {
    fn next_turn(&mut self, t: &Transcript) -> Result<String>;
}

/// Completions captured ahead of time and baked into an object at build time.
///
/// Turns are handed out in order and the transcript is ignored — a recording
/// is a fixed script, not a model.
///
/// Serializes as (and deserializes from) a bare JSON array of turns — the
/// same shape [`from_json`](Self::from_json) expects — since a recording is a
/// script to replay from the start, not paused mid-playback state. `next`
/// exists only in memory, not in the two-way-verified wire representation.
#[derive(Debug, Clone)]
pub struct RecordedSource {
    turns: Vec<String>,
    next: usize,
}

impl Serialize for RecordedSource {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        self.turns.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RecordedSource {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let turns = Vec::<String>::deserialize(deserializer)?;
        Ok(Self::new(turns))
    }
}

impl RecordedSource {
    pub fn new(turns: Vec<String>) -> Self {
        Self { turns, next: 0 }
    }

    /// Load a recording: a JSON array of raw model turns.
    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        let turns: Vec<String> =
            serde_json::from_slice(bytes).context("recording was not a JSON array of turns")?;
        Ok(Self::new(turns))
    }

    /// Turns not yet consumed.
    pub fn remaining(&self) -> usize {
        self.turns.len().saturating_sub(self.next)
    }
}

impl TokenSource for RecordedSource {
    fn next_turn(&mut self, _t: &Transcript) -> Result<String> {
        let Some(turn) = self.turns.get(self.next) else {
            bail!(
                "recording exhausted after {} turn(s); the loop asked for another",
                self.turns.len()
            );
        };
        self.next += 1;
        Ok(turn.clone())
    }
}

/// Turns from a live Ollama server, over its `/api/chat` endpoint.
///
/// Host-only: reaching an Ollama daemon means real sockets, which this crate
/// only pulls in off the Twizzler target. Unlike `RecordedSource`, this reads
/// the transcript — the whole point of a live model is that it responds to
/// what happened so far.
#[cfg(not(target_os = "twizzler"))]
pub struct LiveSource {
    client: reqwest::blocking::Client,
    base_url: String,
    model: String,
}

#[cfg(not(target_os = "twizzler"))]
impl LiveSource {
    /// Connect to Ollama on its default local port.
    pub fn new(model: impl Into<String>) -> Result<Self> {
        Self::with_url("http://localhost:11434", model)
    }

    pub fn with_url(base_url: impl Into<String>, model: impl Into<String>) -> Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .context("building HTTP client for Ollama")?;
        Ok(Self {
            client,
            base_url: base_url.into(),
            model: model.into(),
        })
    }
}

#[cfg(not(target_os = "twizzler"))]
#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage>,
    stream: bool,
    /// Ollama's JSON mode: constrains decoding to syntactically valid JSON,
    /// which in particular rules out a model wrapping its reply in a
    /// markdown code fence. It does not enforce our `Action` schema — that's
    /// still on `parse_turn` — just that the top-level output parses as JSON
    /// at all.
    format: &'static str,
}

#[cfg(not(target_os = "twizzler"))]
#[derive(Serialize)]
struct ChatMessage {
    role: &'static str,
    content: String,
}

#[cfg(not(target_os = "twizzler"))]
impl ChatMessage {
    fn role_str(role: Role) -> &'static str {
        match role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
    }
}

#[cfg(not(target_os = "twizzler"))]
fn to_chat_messages(t: &Transcript) -> Vec<ChatMessage> {
    t.iter()
        .map(|m| ChatMessage {
            role: ChatMessage::role_str(m.role),
            content: m.content.clone(),
        })
        .collect()
}

#[cfg(not(target_os = "twizzler"))]
#[derive(Deserialize)]
struct ChatResponse {
    message: ChatResponseMessage,
}

#[cfg(not(target_os = "twizzler"))]
#[derive(Deserialize)]
struct ChatResponseMessage {
    content: String,
}

#[cfg(not(target_os = "twizzler"))]
impl TokenSource for LiveSource {
    fn next_turn(&mut self, t: &Transcript) -> Result<String> {
        let request = ChatRequest {
            model: &self.model,
            messages: to_chat_messages(t),
            stream: false,
            format: "json",
        };

        let response = self
            .client
            .post(format!("{}/api/chat", self.base_url))
            .json(&request)
            .send()
            .with_context(|| format!("request to Ollama at {} failed", self.base_url))?
            .error_for_status()
            .context("Ollama returned an error status")?
            .json::<ChatResponse>()
            .context("Ollama response was not the expected chat shape")?;

        Ok(response.message.content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hands_out_turns_in_order_then_errors() {
        let mut s = RecordedSource::new(vec!["one".into(), "two".into()]);
        let t = Transcript::new();
        assert_eq!(s.remaining(), 2);
        assert_eq!(s.next_turn(&t).unwrap(), "one");
        assert_eq!(s.next_turn(&t).unwrap(), "two");
        assert_eq!(s.remaining(), 0);
        assert!(s.next_turn(&t).is_err());
    }

    #[test]
    fn loads_from_json() {
        let mut s = RecordedSource::from_json(br#"["a", "b"]"#).unwrap();
        assert_eq!(s.remaining(), 2);
        assert_eq!(s.next_turn(&Transcript::new()).unwrap(), "a");
    }

    #[test]
    fn rejects_a_malformed_recording() {
        assert!(RecordedSource::from_json(b"not json").is_err());
        assert!(RecordedSource::from_json(br#"{"turns": []}"#).is_err());
    }

    #[test]
    fn serializes_as_the_bare_array_from_json_expects() {
        let s = RecordedSource::new(vec!["a".into(), "b".into()]);
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, r#"["a","b"]"#);

        // What we serialize must be exactly what from_json can load back —
        // otherwise the derives lie about the wire format.
        let mut reloaded = RecordedSource::from_json(json.as_bytes()).unwrap();
        assert_eq!(reloaded.remaining(), 2);
        assert_eq!(reloaded.next_turn(&Transcript::new()).unwrap(), "a");
    }

    #[cfg(not(target_os = "twizzler"))]
    #[test]
    fn transcript_roles_map_to_ollama_chat_roles() {
        use crate::transcript::Msg;

        let mut t = Transcript::new();
        t.append(Msg::system("sys"));
        t.append(Msg::user("task"));
        t.append(Msg::assistant("[]"));
        t.append(Msg::tool("wrote a.rs"));

        let messages = to_chat_messages(&t);
        let roles: Vec<&str> = messages.iter().map(|m| m.role).collect();
        assert_eq!(roles, vec!["system", "user", "assistant", "tool"]);

        let json = serde_json::to_value(ChatRequest {
            model: "qwen2.5-coder",
            messages,
            stream: false,
            format: "json",
        })
        .unwrap();
        assert_eq!(json["model"], "qwen2.5-coder");
        assert_eq!(json["stream"], false);
        assert_eq!(json["format"], "json");
        assert_eq!(json["messages"][1]["content"], "task");
    }
}
