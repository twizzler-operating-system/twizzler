//! Where model turns come from.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::transcript::Transcript;

/// Produces the next model turn given the conversation so far.
pub trait TokenSource {
    fn next_turn(&mut self, t: &Transcript) -> Result<String>;
}

/// Completions captured ahead of time and baked into an object at build time.
///
/// Turns are handed out in order and the transcript is ignored — a recording
/// is a fixed script, not a model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordedSource {
    turns: Vec<String>,
    #[serde(default)]
    next: usize,
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

// LiveSource goes here.

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
}
