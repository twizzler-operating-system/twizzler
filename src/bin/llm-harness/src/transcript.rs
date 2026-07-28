//! Conversation history.
//!
//! Backed by a `Vec` for now. The API is deliberately narrow — append and
//! iterate, no indexing and no handing out the inner `Vec` — so the backing
//! store can become a persistent Twizzler object without touching callers.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Harness-supplied framing.
    System,
    /// The task description.
    User,
    /// A model turn.
    Assistant,
    /// The result of an effect we ran on the model's behalf.
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Msg {
    pub role: Role,
    pub content: String,
}

impl Msg {
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self::new(Role::System, content)
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::new(Role::User, content)
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self::new(Role::Assistant, content)
    }

    pub fn tool(content: impl Into<String>) -> Self {
        Self::new(Role::Tool, content)
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Transcript {
    msgs: Vec<Msg>,
}

impl Transcript {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn append(&mut self, msg: Msg) {
        self.msgs.push(msg);
    }

    pub fn iter(&self) -> impl Iterator<Item = &Msg> {
        self.msgs.iter()
    }

    pub fn len(&self) -> usize {
        self.msgs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.msgs.is_empty()
    }

    /// The most recent message, if any.
    pub fn last(&self) -> Option<&Msg> {
        self.msgs.last()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_in_order() {
        let mut t = Transcript::new();
        assert!(t.is_empty());
        t.append(Msg::user("task"));
        t.append(Msg::assistant("ok"));
        assert_eq!(t.len(), 2);
        let roles: Vec<_> = t.iter().map(|m| m.role).collect();
        assert_eq!(roles, vec![Role::User, Role::Assistant]);
        assert_eq!(t.last().unwrap().content, "ok");
    }
}
