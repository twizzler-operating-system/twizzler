//! The closed set of things a model turn is allowed to ask for.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// One step the model asked us to take.
///
/// Adjacently tagged so a newtype variant (`Read`) and a struct variant
/// (`Write`) can coexist in one JSON shape:
///
/// ```json
/// {"action": "read",  "arg": "task.md"}
/// {"action": "write", "arg": {"path": "src/lib.rs", "body": "..."}}
/// {"action": "done"}
/// ```
///
/// The enum is closed: an unrecognized `action` is a parse error, not a
/// silently ignored step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", content = "arg", rename_all = "snake_case")]
pub enum Action {
    Read(String),
    Write { path: String, body: String },
    Done,
}

impl Action {
    /// Short name for the effect log.
    pub fn kind(&self) -> &'static str {
        match self {
            Action::Read(_) => "read",
            Action::Write { .. } => "write",
            Action::Done => "done",
        }
    }

    /// What the action operates on, for the effect log.
    pub fn target(&self) -> &str {
        match self {
            Action::Read(path) => path,
            Action::Write { path, .. } => path,
            Action::Done => "",
        }
    }
}

/// Parse one model turn into the actions it requested.
///
/// A turn is a JSON array of actions. A bare object is accepted as a
/// one-action turn, since that is the shape a model most often emits when it
/// only wants to do one thing.
pub fn parse_turn(text: &str) -> Result<Vec<Action>> {
    let text = text.trim();
    let value: serde_json::Value =
        serde_json::from_str(text).context("model turn was not valid JSON")?;
    match value {
        serde_json::Value::Array(_) => {
            serde_json::from_value(value).context("model turn was not a list of actions")
        }
        _ => {
            let one: Action =
                serde_json::from_value(value).context("model turn was not an action")?;
            Ok(vec![one])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_each_variant() {
        let turn = r#"[
            {"action": "read", "arg": "task.md"},
            {"action": "write", "arg": {"path": "src/lib.rs", "body": "fn main() {}"}},
            {"action": "done"}
        ]"#;
        assert_eq!(
            parse_turn(turn).unwrap(),
            vec![
                Action::Read("task.md".into()),
                Action::Write {
                    path: "src/lib.rs".into(),
                    body: "fn main() {}".into(),
                },
                Action::Done,
            ]
        );
    }

    #[test]
    fn accepts_a_bare_object_as_one_action() {
        assert_eq!(
            parse_turn(r#"{"action": "done"}"#).unwrap(),
            vec![Action::Done]
        );
    }

    #[test]
    fn rejects_unknown_action() {
        let err = parse_turn(r#"[{"action": "exec", "arg": "rm -rf /"}]"#).unwrap_err();
        assert!(format!("{err:#}").contains("action"), "{err:#}");
    }

    #[test]
    fn rejects_malformed_json() {
        assert!(parse_turn("I'll write that file for you!").is_err());
    }

    #[test]
    fn rejects_write_missing_body() {
        assert!(parse_turn(r#"[{"action": "write", "arg": {"path": "a.rs"}}]"#).is_err());
    }

    #[test]
    fn round_trips() {
        let actions = vec![
            Action::Read("a".into()),
            Action::Write {
                path: "b".into(),
                body: "c".into(),
            },
            Action::Done,
        ];
        let text = serde_json::to_string(&actions).unwrap();
        assert_eq!(parse_turn(&text).unwrap(), actions);
    }
}
