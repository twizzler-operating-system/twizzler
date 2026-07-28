//! The loop.
//!
//! Ask the source for a turn, parse it into actions, run each action through
//! `Effects`, feed the results back into the transcript, repeat until the model
//! says `Done`. Nothing in here touches a Twizzler API directly.

use anyhow::{bail, Result};

use crate::action::{parse_turn, Action};
use crate::effects::Effects;
use crate::log::{EffectLog, Outcome};
use crate::source::TokenSource;
use crate::transcript::{Msg, Transcript};

const DEFAULT_MAX_TURNS: usize = 16;

const SYSTEM_PROMPT: &str = "\
You are a coding agent. Reply with a JSON array of actions. \
Actions: {\"action\":\"read\",\"arg\":PATH}, \
{\"action\":\"write\",\"arg\":{\"path\":PATH,\"body\":TEXT}}, \
{\"action\":\"done\"}. Emit done when the task is complete.";

/// Why the loop stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// The model emitted `Done`.
    Done,
    /// The turn budget ran out first.
    MaxTurns,
}

/// Whether to keep running after an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    Done,
}

pub struct Agent<S, E> {
    source: S,
    effects: E,
    transcript: Transcript,
    log: EffectLog,
    max_turns: usize,
}

impl<S: TokenSource, E: Effects> Agent<S, E> {
    pub fn new(source: S, effects: E) -> Self {
        Self {
            source,
            effects,
            transcript: Transcript::new(),
            log: EffectLog::new(),
            max_turns: DEFAULT_MAX_TURNS,
        }
    }

    pub fn with_max_turns(mut self, max_turns: usize) -> Self {
        self.max_turns = max_turns;
        self
    }

    pub fn transcript(&self) -> &Transcript {
        &self.transcript
    }

    pub fn log(&self) -> &EffectLog {
        &self.log
    }

    pub fn effects(&self) -> &E {
        &self.effects
    }

    /// Take the effects backend back, to inspect what the run left behind.
    pub fn into_effects(self) -> E {
        self.effects
    }

    /// Run until the model is done or the turn budget is spent.
    ///
    /// A malformed turn is fatal. A failed *effect* is not: it is recorded and
    /// reported back to the model, which is how an agent gets a chance to
    /// recover.
    pub fn run(&mut self, task: &str) -> Result<Stop> {
        self.transcript.append(Msg::system(SYSTEM_PROMPT));
        self.transcript.append(Msg::user(task));

        for _ in 0..self.max_turns {
            let text = self.source.next_turn(&self.transcript)?;
            self.transcript.append(Msg::assistant(&text));

            let actions = parse_turn(&text)?;
            if actions.is_empty() {
                bail!("model turn requested no actions");
            }

            for action in actions {
                if self.apply(&action)? == Flow::Done {
                    return Ok(Stop::Done);
                }
            }
        }

        Ok(Stop::MaxTurns)
    }

    /// Run one action.
    fn apply(&mut self, action: &Action) -> Result<Flow> {
        match action {
            Action::Read(path) => {
                let result = self.effects.open(path).and_then(|h| self.effects.read(h));
                match result {
                    Ok(bytes) => {
                        let body = String::from_utf8_lossy(&bytes).into_owned();
                        self.log.record(action, Outcome::Ok { bytes: bytes.len() });
                        self.transcript
                            .append(Msg::tool(format!("read {path}:\n{body}")));
                    }
                    Err(e) => self.fail(action, &format!("read {path}"), e),
                }
                Ok(Flow::Continue)
            }
            Action::Write { path, body } => {
                let bytes = body.as_bytes();
                let result = self
                    .effects
                    .open(path)
                    .and_then(|h| self.effects.write(h, bytes));
                match result {
                    Ok(()) => {
                        self.log.record(action, Outcome::Ok { bytes: bytes.len() });
                        self.transcript
                            .append(Msg::tool(format!("wrote {path} ({} bytes)", bytes.len())));
                    }
                    Err(e) => self.fail(action, &format!("write {path}"), e),
                }
                Ok(Flow::Continue)
            }
            Action::Done => {
                self.log.record(action, Outcome::Ok { bytes: 0 });
                Ok(Flow::Done)
            }
        }
    }

    fn fail(&mut self, action: &Action, what: &str, e: anyhow::Error) {
        let message = format!("{e:#}");
        self.log.record(
            action,
            Outcome::Error {
                message: message.clone(),
            },
        );
        self.transcript
            .append(Msg::tool(format!("{what} failed: {message}")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::MemEffects;
    use crate::source::RecordedSource;
    use crate::transcript::Role;

    fn recorded(turns: &[&str]) -> RecordedSource {
        RecordedSource::new(turns.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn read_then_write_then_done() {
        let mut effects = MemEffects::new();
        effects.preload("task.md", "write a greeter");

        let source = recorded(&[
            r#"[{"action": "read", "arg": "task.md"}]"#,
            r#"[{"action": "write", "arg": {"path": "src/greet.rs", "body": "fn greet() {}"}}]"#,
            r#"[{"action": "done"}]"#,
        ]);

        let mut agent = Agent::new(source, effects);
        assert_eq!(agent.run("write a greeter").unwrap(), Stop::Done);

        // The action sequence landed in the log, in order.
        let seq: Vec<(u64, &str, &str)> = agent
            .log()
            .iter()
            .map(|e| (e.seq, e.action.as_str(), e.target.as_str()))
            .collect();
        assert_eq!(
            seq,
            vec![
                (0, "read", "task.md"),
                (1, "write", "src/greet.rs"),
                (2, "done", ""),
            ]
        );
        assert!(agent.log().iter().all(|e| e.outcome.is_ok()));

        // The write actually reached the effects backend.
        assert_eq!(agent.effects().get("src/greet.rs").unwrap(), b"fn greet() {}");
    }

    #[test]
    fn multiple_actions_in_one_turn() {
        let source = recorded(&[r#"[
            {"action": "write", "arg": {"path": "a.rs", "body": "a"}},
            {"action": "write", "arg": {"path": "b.rs", "body": "bb"}},
            {"action": "done"}
        ]"#]);

        let mut agent = Agent::new(source, MemEffects::new());
        assert_eq!(agent.run("two files").unwrap(), Stop::Done);
        assert_eq!(agent.effects().names(), vec!["a.rs", "b.rs"]);
        assert_eq!(agent.log().len(), 3);
    }

    #[test]
    fn read_content_reaches_the_transcript() {
        let mut effects = MemEffects::new();
        effects.preload("task.md", "the task body");
        let source = recorded(&[
            r#"[{"action": "read", "arg": "task.md"}]"#,
            r#"[{"action": "done"}]"#,
        ]);

        let mut agent = Agent::new(source, effects);
        agent.run("go").unwrap();

        let tool: Vec<&str> = agent
            .transcript()
            .iter()
            .filter(|m| m.role == Role::Tool)
            .map(|m| m.content.as_str())
            .collect();
        assert_eq!(tool.len(), 1);
        assert!(tool[0].contains("the task body"), "{}", tool[0]);
    }

    #[test]
    fn transcript_starts_with_system_then_task() {
        let source = recorded(&[r#"[{"action": "done"}]"#]);
        let mut agent = Agent::new(source, MemEffects::new());
        agent.run("my task").unwrap();

        let mut it = agent.transcript().iter();
        assert_eq!(it.next().unwrap().role, Role::System);
        let user = it.next().unwrap();
        assert_eq!(user.role, Role::User);
        assert_eq!(user.content, "my task");
        assert_eq!(it.next().unwrap().role, Role::Assistant);
    }

    #[test]
    fn actions_after_done_are_ignored() {
        let source = recorded(&[r#"[
            {"action": "done"},
            {"action": "write", "arg": {"path": "late.rs", "body": "x"}}
        ]"#]);
        let mut agent = Agent::new(source, MemEffects::new());
        agent.run("stop early").unwrap();
        assert!(agent.effects().get("late.rs").is_none());
        assert_eq!(agent.log().len(), 1);
    }

    #[test]
    fn stops_at_the_turn_budget() {
        // Never emits done.
        let turns = vec![r#"[{"action": "write", "arg": {"path": "a", "body": "x"}}]"#; 10];
        let source = recorded(&turns);
        let mut agent = Agent::new(source, MemEffects::new()).with_max_turns(3);
        assert_eq!(agent.run("loop forever").unwrap(), Stop::MaxTurns);
        assert_eq!(agent.log().len(), 3);
    }

    #[test]
    fn malformed_turn_is_fatal() {
        let source = recorded(&["I'd be happy to help with that!"]);
        let mut agent = Agent::new(source, MemEffects::new());
        assert!(agent.run("go").is_err());
    }

    #[test]
    fn failed_effect_is_logged_and_reported_but_not_fatal() {
        /// Every write fails; opens and reads succeed.
        struct FlakyEffects(MemEffects);
        impl Effects for FlakyEffects {
            fn read(&mut self, h: crate::effects::Handle) -> Result<Vec<u8>> {
                self.0.read(h)
            }
            fn write(&mut self, _h: crate::effects::Handle, _b: &[u8]) -> Result<()> {
                bail!("object is read-only")
            }
            fn open(&mut self, name: &str) -> Result<crate::effects::Handle> {
                self.0.open(name)
            }
        }

        let source = recorded(&[
            r#"[{"action": "write", "arg": {"path": "a.rs", "body": "x"}}]"#,
            r#"[{"action": "done"}]"#,
        ]);
        let mut agent = Agent::new(source, FlakyEffects(MemEffects::new()));
        assert_eq!(agent.run("write it").unwrap(), Stop::Done);

        let first = agent.log().iter().next().unwrap();
        assert!(!first.outcome.is_ok());

        let reported = agent
            .transcript()
            .iter()
            .any(|m| m.role == Role::Tool && m.content.contains("read-only"));
        assert!(reported, "failure should be visible to the model");
    }
}
