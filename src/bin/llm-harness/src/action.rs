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
/// only wants to do one thing. A turn fenced in a markdown code block (with
/// or without a `json` language tag) is unwrapped first — chat models that
/// aren't running in a strict JSON mode reach for this by reflex even when
/// told to emit raw JSON.
pub fn parse_turn(text: &str) -> Result<Vec<Action>> {
    let text = strip_code_fence(text.trim());
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

/// Strip a leading/trailing ` ``` ` (or ` ```json `) fence, if the whole turn
/// is wrapped in one. Text that doesn't look fenced is returned unchanged, so
/// this is safe to run unconditionally ahead of the JSON parse.
fn strip_code_fence(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("```") else {
        return text;
    };
    let Some(rest) = rest.strip_suffix("```") else {
        return text;
    };
    let rest = rest.strip_prefix("json").unwrap_or(rest);
    rest.trim()
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
        // `{err:#}` always contains the word "action" via the anyhow context
        // message ("... was not a list of actions"), regardless of what the
        // underlying cause actually was — so that alone doesn't prove the
        // *unknown variant* was what got rejected. Assert on the serde
        // detail that names the rejected variant instead.
        let err = parse_turn(r#"[{"action": "exec", "arg": "rm -rf /"}]"#).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("exec"), "{msg}");
        assert!(msg.contains("unknown variant"), "{msg}");

        // A recognized action with a bad shape must be rejected for a
        // different reason, so the "exec" assertion above isn't trivially
        // true for any parse failure.
        let shape_err = parse_turn(r#"[{"action": "read", "arg": 5}]"#).unwrap_err();
        assert!(!format!("{shape_err:#}").contains("unknown variant"));
    }

    #[test]
    fn rejects_malformed_json() {
        assert!(parse_turn("I'll write that file for you!").is_err());
    }

    #[test]
    fn unwraps_a_markdown_json_fence() {
        let fenced = "```json\n[{\"action\": \"done\"}]\n```";
        assert_eq!(parse_turn(fenced).unwrap(), vec![Action::Done]);
    }

    #[test]
    fn unwraps_a_bare_markdown_fence() {
        let fenced = "```\n[{\"action\": \"done\"}]\n```";
        assert_eq!(parse_turn(fenced).unwrap(), vec![Action::Done]);
    }

    #[test]
    fn a_fence_around_prose_is_still_rejected() {
        let fenced = "```\nI'll write that file for you!\n```";
        assert!(parse_turn(fenced).is_err());
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
