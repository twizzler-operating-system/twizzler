//! An append-only record of every effect the loop attempted.

use serde::{Deserialize, Serialize};

use crate::action::Action;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Ok { bytes: usize },
    Error { message: String },
    /// Not a model action outcome — a harness-generated marker for why the
    /// run ended, so the log alone can distinguish "completed" from "gave
    /// up" without reading console output.
    Stopped { reason: String },
}

impl Outcome {
    pub fn is_ok(&self) -> bool {
        matches!(self, Outcome::Ok { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectEntry {
    pub seq: u64,
    pub action: String,
    pub target: String,
    pub outcome: Outcome,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct EffectLog {
    entries: Vec<EffectEntry>,
}

impl EffectLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&mut self, action: &Action, outcome: Outcome) {
        self.push(action.kind(), action.target(), outcome);
    }

    /// Record a harness-generated event that isn't a model action, e.g. why
    /// the run stopped.
    pub fn record_event(&mut self, kind: &str, target: &str, outcome: Outcome) {
        self.push(kind, target, outcome);
    }

    fn push(&mut self, kind: &str, target: &str, outcome: Outcome) {
        let seq = self.entries.len() as u64;
        self.entries.push(EffectEntry {
            seq,
            action: kind.to_string(),
            target: target.to_string(),
            outcome,
        });
    }

    pub fn iter(&self) -> impl Iterator<Item = &EffectEntry> {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// One line per entry, for dumping to the console.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for e in &self.entries {
            let outcome = match &e.outcome {
                Outcome::Ok { bytes } => format!("ok ({bytes} bytes)"),
                Outcome::Error { message } => format!("error: {message}"),
                Outcome::Stopped { reason } => format!("stopped: {reason}"),
            };
            out.push_str(&format!(
                "{:>3}  {:<5}  {:<24}  {}\n",
                e.seq, e.action, e.target, outcome
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_entries_from_zero() {
        let mut log = EffectLog::new();
        log.record(&Action::Read("a".into()), Outcome::Ok { bytes: 3 });
        log.record(
            &Action::Write {
                path: "b".into(),
                body: "xy".into(),
            },
            Outcome::Ok { bytes: 2 },
        );
        log.record(
            &Action::Read("c".into()),
            Outcome::Error {
                message: "nope".into(),
            },
        );

        let seqs: Vec<u64> = log.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![0, 1, 2]);

        let kinds: Vec<&str> = log.iter().map(|e| e.action.as_str()).collect();
        assert_eq!(kinds, vec!["read", "write", "read"]);

        let targets: Vec<&str> = log.iter().map(|e| e.target.as_str()).collect();
        assert_eq!(targets, vec!["a", "b", "c"]);

        assert!(!log.iter().last().unwrap().outcome.is_ok());
    }

    #[test]
    fn record_event_appends_a_harness_generated_entry() {
        let mut log = EffectLog::new();
        log.record(&Action::Read("a".into()), Outcome::Ok { bytes: 1 });
        log.record_event(
            "stop",
            "",
            Outcome::Stopped {
                reason: "turn budget exhausted".into(),
            },
        );
        let last = log.iter().last().unwrap();
        assert_eq!(last.action, "stop");
        assert!(!last.outcome.is_ok());
    }

    #[test]
    fn renders_one_line_per_entry() {
        let mut log = EffectLog::new();
        log.record(&Action::Read("a".into()), Outcome::Ok { bytes: 3 });
        log.record(&Action::Done, Outcome::Ok { bytes: 0 });
        assert_eq!(log.render().lines().count(), 2);
    }
}
