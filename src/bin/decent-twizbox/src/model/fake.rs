use anyhow::{Result, bail};
use std::sync::Mutex;

use super::ModelClient;

/// Test double that plays back a scripted sequence of completions.
pub struct FakeModelClient {
    outputs: Mutex<Vec<String>>,
}

impl FakeModelClient {
    pub fn new(outputs: Vec<String>) -> Self {
        Self {
            outputs: Mutex::new(outputs),
        }
    }
}

impl ModelClient for FakeModelClient {
    fn complete(&self, _prompt: &str, _max_new_tokens: usize) -> Result<String> {
        let mut outputs = self.outputs.lock().expect("fake model mutex poisoned");
        if outputs.is_empty() {
            bail!("fake model ran out of outputs");
        }
        Ok(outputs.remove(0))
    }
}
