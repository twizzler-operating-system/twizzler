//! The agent's core request/response loop.
//!
//! This module ties the other pieces together but tries not to know their
//! details:
//! - [`prompt`] renders the model prompt (system rules + workspace + memory + transcript).
//! - [`parse`] turns a raw model reply into a [`parse::ParsedResponse`].
//! - [`tools`] defines what tools exist and how they run.
//! - [`welcome`] renders the startup banner.
//!
//! [`Agent::ask`] is the loop: build a prompt, ask the model, either run
//! a tool and go again or return a final answer, up to `max_steps` tool
//! calls. Every step is appended to the session's history and persisted
//! immediately so a crash never loses more than the in-flight step.

mod parse;
mod prompt;
mod tools;
mod welcome;

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::Result;
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::helpers::{clip, now};
use crate::model::ModelClient;
use crate::session::{HistoryItem, Memory, Session, SessionStore};
use crate::workspace::WorkspaceContext;
use parse::ParsedResponse;
use tools::ToolInfo;

pub use welcome::build_welcome;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalPolicy {
    Ask,
    Auto,
    Never,
}

impl ApprovalPolicy {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "ask" => Ok(Self::Ask),
            "auto" => Ok(Self::Auto),
            "never" => Ok(Self::Never),
            _ => anyhow::bail!("unknown approval policy: {value}"),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Auto => "auto",
            Self::Never => "never",
        }
    }
}

pub struct Agent {
    model_client: Arc<dyn ModelClient>,
    workspace: WorkspaceContext,
    root: std::path::PathBuf,
    session_store: SessionStore,
    approval_policy: ApprovalPolicy,
    max_steps: usize,
    max_new_tokens: usize,
    depth: usize,
    max_depth: usize,
    read_only: bool,
    session: Session,
    tools: BTreeMap<&'static str, ToolInfo>,
    prefix: String,
    session_path: std::path::PathBuf,
}

pub struct AgentConfig {
    pub approval_policy: ApprovalPolicy,
    pub max_steps: usize,
    pub max_new_tokens: usize,
    pub depth: usize,
    pub max_depth: usize,
    pub read_only: bool,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            approval_policy: ApprovalPolicy::Ask,
            max_steps: 6,
            max_new_tokens: 512,
            depth: 0,
            max_depth: 1,
            read_only: false,
        }
    }
}

impl Agent {
    pub fn new(
        model_client: Arc<dyn ModelClient>,
        workspace: WorkspaceContext,
        session_store: SessionStore,
        session: Option<Session>,
        config: AgentConfig,
    ) -> Result<Self> {
        let session = session.unwrap_or_else(|| Session {
            id: format!(
                "{}-{}",
                chrono::Utc::now().format("%Y%m%d-%H%M%S"),
                &Uuid::new_v4().simple().to_string()[..6]
            ),
            created_at: now(),
            workspace_root: workspace.repo_root.to_string_lossy().to_string(),
            history: Vec::new(),
            memory: Memory::default(),
        });
        let tools = Self::build_tools(config.depth, config.max_depth);
        let root = workspace.repo_root.clone();
        let mut agent = Self {
            model_client,
            workspace,
            root,
            session_store,
            approval_policy: config.approval_policy,
            max_steps: config.max_steps,
            max_new_tokens: config.max_new_tokens,
            depth: config.depth,
            max_depth: config.max_depth,
            read_only: config.read_only,
            session,
            tools,
            prefix: String::new(),
            session_path: std::path::PathBuf::new(),
        };
        agent.prefix = agent.build_prefix();
        agent.session_path = agent.session_store.save(&agent.session)?;
        Ok(agent)
    }

    pub fn from_session(
        model_client: Arc<dyn ModelClient>,
        workspace: WorkspaceContext,
        session_store: SessionStore,
        session_id: &str,
        config: AgentConfig,
    ) -> Result<Self> {
        let session = session_store.load(session_id)?;
        Self::new(
            model_client,
            workspace,
            session_store,
            Some(session),
            config,
        )
    }

    pub fn workspace(&self) -> &WorkspaceContext {
        &self.workspace
    }

    pub fn session_id(&self) -> &str {
        &self.session.id
    }

    pub fn session_path(&self) -> &std::path::Path {
        &self.session_path
    }

    pub fn approval_policy(&self) -> ApprovalPolicy {
        self.approval_policy
    }

    fn remember(bucket: &mut Vec<String>, item: impl Into<String>, limit: usize) {
        let item = item.into();
        if item.is_empty() {
            return;
        }
        bucket.retain(|existing| existing != &item);
        bucket.push(item);
        if bucket.len() > limit {
            bucket.drain(0..bucket.len() - limit);
        }
    }

    pub fn memory_text(&self) -> String {
        let memory = &self.session.memory;
        let notes = if memory.notes.is_empty() {
            "- none".to_string()
        } else {
            memory
                .notes
                .iter()
                .map(|note| format!("- {note}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        [
            "Memory:".to_string(),
            format!(
                "- task: {}",
                if memory.task.is_empty() {
                    "-"
                } else {
                    &memory.task
                }
            ),
            format!(
                "- files: {}",
                if memory.files.is_empty() {
                    "-".to_string()
                } else {
                    memory.files.join(", ")
                }
            ),
            "- notes:".to_string(),
            notes,
        ]
        .join("\n")
    }

    fn record(&mut self, item: HistoryItem) -> Result<()> {
        self.session.history.push(item);
        self.session_path = self.session_store.save(&self.session)?;
        Ok(())
    }

    fn note_tool(&mut self, name: &str, args: &Map<String, Value>, result: &str) {
        if matches!(name, "read_file" | "write_file" | "patch_file")
            && let Some(path) = args.get("path").and_then(Value::as_str)
        {
            Self::remember(&mut self.session.memory.files, path, 8);
        }
        let note = format!("{}: {}", name, clip(result.replace('\n', " "), 220));
        Self::remember(&mut self.session.memory.notes, note, 5);
    }

    pub fn ask(&mut self, user_message: &str) -> Result<String> {
        if self.session.memory.task.is_empty() {
            self.session.memory.task = clip(user_message.trim(), 300);
        }
        self.record(HistoryItem {
            role: "user".to_string(),
            name: None,
            args: Map::new(),
            content: user_message.to_string(),
            created_at: now(),
        })?;

        let mut tool_steps = 0;
        let mut attempts = 0;
        let max_attempts = std::cmp::max(self.max_steps * 3, self.max_steps + 4);
        // Whether we've already spent one turn nudging an untagged reply
        // into either calling a tool or wrapping itself in <final>. Only
        // ever fires once per `ask()` call, so a model that never uses tags
        // for plain chat still gets a normal answer after a single retry,
        // instead of looping until it hits max_attempts.
        let mut nudged_untagged_reply = false;

        while tool_steps < self.max_steps && attempts < max_attempts {
            attempts += 1;
            let raw = self.complete_with_spinner(&self.prompt(user_message))?;
            match parse::parse(&raw) {
                ParsedResponse::Tool { name, args } => {
                    tool_steps += 1;
                    let result = self.run_tool(&name, &args);
                    self.record(HistoryItem {
                        role: "tool".to_string(),
                        name: Some(name.clone()),
                        args: args.clone(),
                        content: result.clone(),
                        created_at: now(),
                    })?;
                    self.note_tool(&name, &args, &result);
                }
                ParsedResponse::Retry(payload) => {
                    self.record(HistoryItem {
                        role: "assistant".to_string(),
                        name: None,
                        args: Map::new(),
                        content: payload,
                        created_at: now(),
                    })?;
                }
                ParsedResponse::Untagged(_) if !nudged_untagged_reply => {
                    nudged_untagged_reply = true;
                    self.record(HistoryItem {
                        role: "assistant".to_string(),
                        name: None,
                        args: Map::new(),
                        content: parse::unfinished_action_notice(),
                        created_at: now(),
                    })?;
                }
                ParsedResponse::Final(payload) | ParsedResponse::Untagged(payload) => {
                    let final_answer = payload.trim().to_string();
                    self.record(HistoryItem {
                        role: "assistant".to_string(),
                        name: None,
                        args: Map::new(),
                        content: final_answer.clone(),
                        created_at: now(),
                    })?;
                    Self::remember(&mut self.session.memory.notes, clip(&final_answer, 220), 5);
                    return Ok(final_answer);
                }
            }
        }

        let final_answer = if attempts >= max_attempts && tool_steps < self.max_steps {
            "Stopped after too many malformed model responses without a valid tool call or final answer."
        } else {
            "Stopped after reaching the step limit without a final answer."
        }
        .to_string();
        self.record(HistoryItem {
            role: "assistant".to_string(),
            name: None,
            args: Map::new(),
            content: final_answer.clone(),
            created_at: now(),
        })?;
        Ok(final_answer)
    }

    fn run_tool(&mut self, name: &str, args: &Map<String, Value>) -> String {
        let Some(tool) = self.tools.get(name) else {
            return format!("error: unknown tool '{name}'");
        };
        if let Err(error) = self.validate_tool(name, args) {
            let mut message = format!("error: invalid arguments for {name}: {error}");
            let example = Self::tool_example(name);
            if !example.is_empty() {
                message.push_str(&format!("\nexample: {example}"));
            }
            return message;
        }
        if self.repeated_tool_call(name, args) {
            return format!(
                "error: repeated identical tool call for {name}; choose a different tool or return a final answer"
            );
        }
        if tool.risky && !self.approve(name, args) {
            return format!("error: approval denied for {name}");
        }
        let result = self.dispatch_tool(name, args);
        match result {
            Ok(output) => clip(output, crate::helpers::MAX_TOOL_OUTPUT),
            Err(error) => format!("error: tool {name} failed: {error}"),
        }
    }

    fn repeated_tool_call(&self, name: &str, args: &Map<String, Value>) -> bool {
        let tool_events = self
            .session
            .history
            .iter()
            .filter(|item| item.role == "tool")
            .collect::<Vec<_>>();
        if tool_events.len() < 2 {
            return false;
        }
        tool_events[tool_events.len() - 2..]
            .iter()
            .all(|item| item.name.as_deref() == Some(name) && &item.args == args)
    }

    /// Runs the model call on a background thread and prints a spinner to
    /// stderr (so it never pollutes stdout/redirected output) until it
    /// finishes. Every `ModelClient` is `Send + Sync`, so this is just a
    /// channel handoff -- no shared state to worry about.
    fn complete_with_spinner(&self, prompt: &str) -> Result<String> {
        let (tx, rx) = mpsc::channel();
        let client = Arc::clone(&self.model_client);
        let prompt = prompt.to_string();
        let max_new_tokens = self.max_new_tokens;
        std::thread::spawn(move || {
            let _ = tx.send(client.complete(&prompt, max_new_tokens));
        });

        let frames = ['|', '/', '-', '\\'];
        let mut frame = 0;
        let result = loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(result) => break result,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    eprint!("\r{} thinking...", frames[frame % frames.len()]);
                    let _ = io::stderr().flush();
                    frame += 1;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    break Err(anyhow::anyhow!("model thread ended without a response"));
                }
            }
        };
        eprint!("\r{}\r", " ".repeat(20));
        let _ = io::stderr().flush();
        result
    }

    fn approve(&self, name: &str, args: &Map<String, Value>) -> bool {
        if self.read_only {
            return false;
        }
        match self.approval_policy {
            ApprovalPolicy::Auto => true,
            ApprovalPolicy::Never => false,
            ApprovalPolicy::Ask => {
                print!(
                    "approve {name} {}? [y/N] ",
                    serde_json::to_string(&Value::Object(args.clone()))
                        .unwrap_or_else(|_| "{}".to_string())
                );
                let _ = io::stdout().flush();
                let mut answer = String::new();
                io::stdin()
                    .read_line(&mut answer)
                    .is_ok_and(|_| matches!(answer.trim().to_lowercase().as_str(), "y" | "yes"))
            }
        }
    }

    pub fn reset(&mut self) -> Result<()> {
        self.session.history.clear();
        self.session.memory = Memory::default();
        self.session_store.save(&self.session)?;
        Ok(())
    }
}
