//! Turns a raw model reply into a [`ParsedResponse`].
//!
//! The agent's wire format is deliberately small: a `<tool>` call (JSON or
//! attribute/XML style, for multi-line content), a `<final>` answer, or
//! (falling back) untagged text. Anything actually malformed becomes `Retry`
//! with a notice telling the model how to correct itself on the next turn --
//! the loop in `mod.rs` just records that notice and asks again.
//!
//! Untagged text is its own variant, not folded into `Final`, because it's
//! ambiguous: it might be a complete chat-style answer ("Hello!"), or it
//! might be the model narrating an action it never actually took ("I'll
//! create the file now, let me check the directory first."). `mod.rs`
//! nudges once on the first untagged reply per turn and only accepts it as
//! final after that, so the common narrated-but-not-executed case gets a
//! chance to actually call the tool instead of silently ending the turn.

use std::collections::BTreeMap;

use regex::Regex;
use serde_json::{Map, Value};

#[derive(Debug, Clone)]
pub(super) enum ParsedResponse {
    Tool {
        name: String,
        args: Map<String, Value>,
    },
    Retry(String),
    Final(String),
    Untagged(String),
}

pub(super) fn parse(raw: &str) -> ParsedResponse {
    let raw = raw.to_string();
    if raw.contains("<tool>") && (!raw.contains("<final>") || raw.find("<tool>") < raw.find("<final>"))
    {
        let body = extract(&raw, "tool");
        let payload: Value = match serde_json::from_str(&body) {
            Ok(payload) => payload,
            Err(_) => {
                return ParsedResponse::Retry(retry_notice(Some("model returned malformed tool JSON")));
            }
        };
        let Some(mut object) = payload.as_object().cloned() else {
            return ParsedResponse::Retry(retry_notice(Some("tool payload must be a JSON object")));
        };
        let Some(name) = object
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(ToString::to_string)
        else {
            return ParsedResponse::Retry(retry_notice(Some("tool payload is missing a tool name")));
        };
        let args = match object.remove("args") {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(args)) => args,
            _ => return ParsedResponse::Retry(retry_notice(None)),
        };
        return ParsedResponse::Tool { name, args };
    }
    if raw.contains("<tool") && (!raw.contains("<final>") || raw.find("<tool") < raw.find("<final>")) {
        if let Some((name, args)) = parse_xml_tool(&raw) {
            return ParsedResponse::Tool { name, args };
        }
        return ParsedResponse::Retry(retry_notice(None));
    }
    if raw.contains("<final>") {
        let final_answer = extract(&raw, "final").trim().to_string();
        if !final_answer.is_empty() {
            return ParsedResponse::Final(final_answer);
        }
        return ParsedResponse::Retry(retry_notice(Some("model returned an empty <final> answer")));
    }
    let raw = raw.trim().to_string();
    if !raw.is_empty() {
        return ParsedResponse::Untagged(raw);
    }
    ParsedResponse::Retry(retry_notice(Some("model returned an empty response")))
}

fn retry_notice(problem: Option<&str>) -> String {
    let prefix = problem
        .map(|problem| format!("Runtime notice: {problem}"))
        .unwrap_or_else(|| "Runtime notice: model returned malformed tool output".to_string());
    format!(
        "{prefix}. Reply with a valid <tool> call or a non-empty <final> answer. For multi-line files, prefer <tool name=\"write_file\" path=\"file.py\"><content>...</content></tool>."
    )
}

/// Shown once per turn when the model's first untagged reply looks like it
/// might be narrating an action instead of taking it. If the very next
/// reply is untagged too, `mod.rs` gives up nudging and accepts it as the
/// final answer -- this must never loop forever for models that just don't
/// use tags for plain chat.
pub(super) fn unfinished_action_notice() -> String {
    "Runtime notice: that reply described an action without taking it. If you intend to use a \
     tool, respond now with the actual <tool>...</tool> call instead of describing it. If that \
     was actually your complete answer, resend it wrapped in <final>...</final>."
        .to_string()
}

fn parse_xml_tool(raw: &str) -> Option<(String, Map<String, Value>)> {
    let re = Regex::new(r#"(?s)<tool(?P<attrs>[^>]*)>(?P<body>.*?)</tool>"#).ok()?;
    let captures = re.captures(raw)?;
    let mut attrs = parse_attrs(captures.name("attrs").map(|m| m.as_str()).unwrap_or(""));
    let name = attrs.remove("name")?.trim().to_string();
    if name.is_empty() {
        return None;
    }
    let body = captures.name("body").map(|m| m.as_str()).unwrap_or("");
    let mut args = attrs
        .into_iter()
        .map(|(key, value)| (key, Value::String(value)))
        .collect::<Map<_, _>>();
    for key in [
        "content", "old_text", "new_text", "command", "task", "pattern", "path",
    ] {
        if body.contains(&format!("<{key}>")) {
            args.insert(key.to_string(), Value::String(extract_raw(body, key)));
        }
    }
    let body_text = body.trim_matches('\n');
    if name == "write_file" && !args.contains_key("content") && !body_text.is_empty() {
        args.insert("content".to_string(), Value::String(body_text.to_string()));
    }
    if name == "delegate" && !args.contains_key("task") && !body_text.is_empty() {
        args.insert("task".to_string(), Value::String(body_text.trim().to_string()));
    }
    Some((name, args))
}

fn parse_attrs(text: &str) -> BTreeMap<String, String> {
    let re = Regex::new(r#"([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(?:"([^"]*)"|'([^']*)')"#)
        .expect("attribute regex is valid");
    re.captures_iter(text)
        .filter_map(|captures| {
            let key = captures.get(1)?.as_str().to_string();
            let value = captures
                .get(2)
                .or_else(|| captures.get(3))
                .map(|m| m.as_str().to_string())?;
            Some((key, value))
        })
        .collect()
}

fn extract(text: &str, tag: &str) -> String {
    let start_tag = format!("<{tag}>");
    let end_tag = format!("</{tag}>");
    let Some(start) = text.find(&start_tag) else {
        return text.to_string();
    };
    let body_start = start + start_tag.len();
    let Some(end) = text[body_start..].find(&end_tag).map(|end| body_start + end) else {
        return text[body_start..].trim().to_string();
    };
    text[body_start..end].trim().to_string()
}

fn extract_raw(text: &str, tag: &str) -> String {
    let start_tag = format!("<{tag}>");
    let end_tag = format!("</{tag}>");
    let Some(start) = text.find(&start_tag) else {
        return text.to_string();
    };
    let body_start = start + start_tag.len();
    let Some(end) = text[body_start..].find(&end_tag).map(|end| body_start + end) else {
        return text[body_start..].to_string();
    };
    text[body_start..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::{ParsedResponse, parse};

    #[test]
    fn parses_json_tool_call() {
        let parsed = parse(r#"<tool>{"name":"list_files","args":{"path":"."}}</tool>"#);
        match parsed {
            ParsedResponse::Tool { name, args } => {
                assert_eq!(name, "list_files");
                assert_eq!(args.get("path").and_then(|v| v.as_str()), Some("."));
            }
            _ => panic!("expected tool response"),
        }
    }

    #[test]
    fn bare_text_without_tags_is_untagged_not_final() {
        let parsed = parse("I'll create the calculator now. Let me check the directory first.");
        match parsed {
            ParsedResponse::Untagged(text) => {
                assert_eq!(text, "I'll create the calculator now. Let me check the directory first.");
            }
            _ => panic!("expected untagged response"),
        }
    }

    #[test]
    fn parses_xml_tool_call() {
        let parsed = parse(
            r#"<tool name="write_file" path="x.txt"><content>hello
world</content></tool>"#,
        );
        match parsed {
            ParsedResponse::Tool { name, args } => {
                assert_eq!(name, "write_file");
                assert_eq!(args.get("path").and_then(|v| v.as_str()), Some("x.txt"));
                assert_eq!(
                    args.get("content").and_then(|v| v.as_str()),
                    Some("hello\nworld")
                );
            }
            _ => panic!("expected tool response"),
        }
    }
}
