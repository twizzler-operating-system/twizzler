//! Model backends the agent can talk to.
//!
//! Every backend implements [`ModelClient`], which reduces a chat with the
//! model down to "send this prompt, get a completion back". The agent loop
//! in `crate::agent` only ever depends on this trait, so adding a new
//! provider means writing one new file here and wiring it up in `main.rs` --
//! nothing in `crate::agent` has to change.

mod ollama;
mod openrouter;

#[cfg(test)]
mod fake;

pub use ollama::OllamaModelClient;
pub use openrouter::OpenRouterModelClient;

#[cfg(test)]
pub use fake::FakeModelClient;

use anyhow::Result;

pub trait ModelClient: Send + Sync {
    fn complete(&self, prompt: &str, max_new_tokens: usize) -> Result<String>;
}
