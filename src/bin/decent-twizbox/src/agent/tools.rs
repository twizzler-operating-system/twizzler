//! Tool definitions: what tools exist, how their arguments are validated,
//! and what running each one actually does.
//!
//! To add a new tool: add an entry in [`Agent::build_tools`], a case in
//! [`Agent::validate_tool`], a case in [`Agent::dispatch_tool`], and
//! the `tool_*` method that implements it. Everything else (parsing model
//! output, prompting, approval, history) is oblivious to the tool set.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value};
use walkdir::WalkDir;

use super::{Agent, AgentConfig, ApprovalPolicy};
use crate::helpers::clip;

const IGNORED_PATH_NAMES: &[&str] = &[
    ".git",
    ".twizbox",
    "__pycache__",
    ".pytest_cache",
    ".ruff_cache",
    ".venv",
    "venv",
];

#[derive(Debug, Clone)]
pub(super) struct ToolInfo {
    pub(super) schema: BTreeMap<&'static str, &'static str>,
    pub(super) risky: bool,
    pub(super) description: &'static str,
}

impl Agent {
    pub(super) fn build_tools(depth: usize, max_depth: usize) -> BTreeMap<&'static str, ToolInfo> {
        let mut tools = BTreeMap::new();
        tools.insert(
            "list_files",
            ToolInfo {
                schema: BTreeMap::from([("path", "str='.'")]),
                risky: false,
                description: "List files in the workspace.",
            },
        );
        tools.insert(
            "read_file",
            ToolInfo {
                schema: BTreeMap::from([("path", "str"), ("start", "int=1"), ("end", "int=200")]),
                risky: false,
                description: "Read a UTF-8 file by line range.",
            },
        );
        tools.insert(
            "search",
            ToolInfo {
                schema: BTreeMap::from([("pattern", "str"), ("path", "str='.'")]),
                risky: false,
                description: "Search the workspace with rg or a simple fallback.",
            },
        );
        tools.insert(
            "run_shell",
            ToolInfo {
                schema: BTreeMap::from([("command", "str"), ("timeout", "int=20")]),
                risky: true,
                description: "Run a shell command in the repo root. Avoid destructive or irreversible commands.",
            },
        );
        tools.insert(
            "write_file",
            ToolInfo {
                schema: BTreeMap::from([("path", "str"), ("content", "str")]),
                risky: true,
                description: "Write a text file, replacing it entirely. Prefer patch_file for existing files.",
            },
        );
        tools.insert(
            "patch_file",
            ToolInfo {
                schema: BTreeMap::from([("path", "str"), ("old_text", "str"), ("new_text", "str")]),
                risky: true,
                description: "Replace one exact text block in a file. Prefer this over write_file for existing files.",
            },
        );
        if depth < max_depth {
            tools.insert(
                "delegate",
                ToolInfo {
                    schema: BTreeMap::from([("task", "str"), ("max_steps", "int=3")]),
                    risky: false,
                    description: "Ask a bounded read-only child agent to investigate.",
                },
            );
        }
        tools
    }

    pub(super) fn tool_example(name: &str) -> &'static str {
        match name {
            "list_files" => r#"<tool>{"name":"list_files","args":{"path":"."}}</tool>"#,
            "read_file" => {
                r#"<tool>{"name":"read_file","args":{"path":"README.md","start":1,"end":80}}</tool>"#
            }
            "search" => {
                r#"<tool>{"name":"search","args":{"pattern":"binary_search","path":"."}}</tool>"#
            }
            "run_shell" => {
                r#"<tool>{"name":"run_shell","args":{"command":"uv run --with pytest python -m pytest -q","timeout":20}}</tool>"#
            }
            "write_file" => {
                "<tool name=\"write_file\" path=\"binary_search.py\"><content>def binary_search(nums, target):\n    return -1\n</content></tool>"
            }
            "patch_file" => {
                "<tool name=\"patch_file\" path=\"binary_search.py\"><old_text>return -1</old_text><new_text>return mid</new_text></tool>"
            }
            "delegate" => {
                r#"<tool>{"name":"delegate","args":{"task":"inspect README.md","max_steps":3}}</tool>"#
            }
            _ => "",
        }
    }

    pub(super) fn validate_tool(&self, name: &str, args: &Map<String, Value>) -> Result<()> {
        match name {
            "list_files" => {
                let path = self.path(Self::str_arg(args, "path").unwrap_or("."))?;
                if !path.is_dir() {
                    bail!("path is not a directory");
                }
            }
            "read_file" => {
                let path = self.path(Self::required_str(args, "path")?)?;
                if !path.is_file() {
                    bail!("path is not a file");
                }
                let start = Self::int_arg(args, "start", 1)?;
                let end = Self::int_arg(args, "end", 200)?;
                if start < 1 || end < start {
                    bail!("invalid line range");
                }
            }
            "search" => {
                let pattern = Self::str_arg(args, "pattern").unwrap_or("").trim();
                if pattern.is_empty() {
                    bail!("pattern must not be empty");
                }
                self.path(Self::str_arg(args, "path").unwrap_or("."))?;
            }
            "run_shell" => {
                let command = Self::str_arg(args, "command").unwrap_or("").trim();
                if command.is_empty() {
                    bail!("command must not be empty");
                }
                let timeout = Self::int_arg(args, "timeout", 20)?;
                if !(1..=120).contains(&timeout) {
                    bail!("timeout must be in [1, 120]");
                }
            }
            "write_file" => {
                let path = self.path(Self::required_str(args, "path")?)?;
                if path.exists() && path.is_dir() {
                    bail!("path is a directory");
                }
                if !args.contains_key("content") {
                    bail!("missing content");
                }
            }
            "patch_file" => {
                let path = self.path(Self::required_str(args, "path")?)?;
                if !path.is_file() {
                    bail!("path is not a file");
                }
                let old_text = Self::str_arg(args, "old_text").unwrap_or("");
                if old_text.is_empty() {
                    bail!("old_text must not be empty");
                }
                if !args.contains_key("new_text") {
                    bail!("missing new_text");
                }
                let text = std::fs::read_to_string(&path)?;
                let count = text.matches(old_text).count();
                if count != 1 {
                    bail!("old_text must occur exactly once, found {count}");
                }
            }
            "delegate" => {
                if self.depth >= self.max_depth {
                    bail!("delegate depth exceeded");
                }
                let task = Self::str_arg(args, "task").unwrap_or("").trim();
                if task.is_empty() {
                    bail!("task must not be empty");
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Dispatches to the individual `tool_*` implementation. Kept separate
    /// from validation/approval (in `mod.rs`) so this match is the single
    /// place that has to change when a tool is added or removed.
    pub(super) fn dispatch_tool(&self, name: &str, args: &Map<String, Value>) -> Result<String> {
        match name {
            "list_files" => self.tool_list_files(args),
            "read_file" => self.tool_read_file(args),
            "search" => self.tool_search(args),
            "run_shell" => self.tool_run_shell(args),
            "write_file" => self.tool_write_file(args),
            "patch_file" => self.tool_patch_file(args),
            "delegate" => self.tool_delegate(args),
            _ => Err(anyhow!("unknown tool '{name}'")),
        }
    }

    fn path(&self, raw_path: &str) -> Result<PathBuf> {
        let raw = Path::new(raw_path);
        let absolute = if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            self.root.join(raw)
        };
        let normalized = normalize_path(&absolute);
        let resolved = if normalized.exists() {
            normalized.canonicalize()?
        } else {
            canonicalize_nonexistent(&normalized)?
        };
        let root = self.root.canonicalize()?;
        if !resolved.starts_with(&root) {
            bail!("path escapes workspace: {raw_path}");
        }
        Ok(resolved)
    }

    fn tool_list_files(&self, args: &Map<String, Value>) -> Result<String> {
        let path = self.path(Self::str_arg(args, "path").unwrap_or("."))?;
        if !path.is_dir() {
            bail!("path is not a directory");
        }
        let mut entries = std::fs::read_dir(&path)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .map(|name| !IGNORED_PATH_NAMES.contains(&name.to_string_lossy().as_ref()))
                    .unwrap_or(true)
            })
            .collect::<Vec<_>>();
        entries.sort_by_key(|path| {
            (
                path.is_file(),
                path.file_name()
                    .map(|name| name.to_string_lossy().to_lowercase()),
            )
        });
        let lines = entries
            .into_iter()
            .take(200)
            .map(|entry| {
                let kind = if entry.is_dir() { "[D]" } else { "[F]" };
                let rel = entry.strip_prefix(&self.root).unwrap_or(&entry);
                format!("{kind} {}", rel.display())
            })
            .collect::<Vec<_>>();
        Ok(if lines.is_empty() {
            "(empty)".to_string()
        } else {
            lines.join("\n")
        })
    }

    fn tool_read_file(&self, args: &Map<String, Value>) -> Result<String> {
        let path = self.path(Self::required_str(args, "path")?)?;
        if !path.is_file() {
            bail!("path is not a file");
        }
        let start = Self::int_arg(args, "start", 1)?;
        let end = Self::int_arg(args, "end", 200)?;
        if start < 1 || end < start {
            bail!("invalid line range");
        }
        let text = std::fs::read_to_string(&path).unwrap_or_else(|_| {
            String::from_utf8_lossy(&std::fs::read(&path).unwrap_or_default()).to_string()
        });
        let body = text
            .lines()
            .enumerate()
            .skip((start - 1) as usize)
            .take((end - start + 1) as usize)
            .map(|(index, line)| format!("{:>4}: {line}", index + 1))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(format!(
            "# {}\n{body}",
            path.strip_prefix(&self.root).unwrap_or(&path).display()
        ))
    }

    fn tool_search(&self, args: &Map<String, Value>) -> Result<String> {
        let pattern = Self::str_arg(args, "pattern").unwrap_or("").trim();
        if pattern.is_empty() {
            bail!("pattern must not be empty");
        }
        let path = self.path(Self::str_arg(args, "path").unwrap_or("."))?;
        if command_exists("rg") {
            let output = Command::new("rg")
                .args(["-n", "--smart-case", "--max-count", "200", pattern])
                .arg(&path)
                .current_dir(&self.root)
                .output()?;
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Ok(if !stdout.is_empty() {
                stdout
            } else if !stderr.is_empty() {
                stderr
            } else {
                "(no matches)".to_string()
            });
        }

        let files: Vec<PathBuf> = if path.is_file() {
            vec![path]
        } else {
            WalkDir::new(path)
                .into_iter()
                .filter_map(Result::ok)
                .map(|entry| entry.path().to_path_buf())
                .filter(|path| path.is_file() && !has_ignored_component(path, &self.root))
                .collect()
        };
        let pattern_lower = pattern.to_lowercase();
        let mut matches = Vec::new();
        for file_path in files {
            let text = std::fs::read_to_string(&file_path).unwrap_or_else(|_| {
                String::from_utf8_lossy(&std::fs::read(&file_path).unwrap_or_default()).to_string()
            });
            for (number, line) in text.lines().enumerate() {
                if line.to_lowercase().contains(&pattern_lower) {
                    matches.push(format!(
                        "{}:{}:{line}",
                        file_path
                            .strip_prefix(&self.root)
                            .unwrap_or(&file_path)
                            .display(),
                        number + 1
                    ));
                    if matches.len() >= 200 {
                        return Ok(matches.join("\n"));
                    }
                }
            }
        }
        Ok(if matches.is_empty() {
            "(no matches)".to_string()
        } else {
            matches.join("\n")
        })
    }

    fn tool_run_shell(&self, args: &Map<String, Value>) -> Result<String> {
        let command = Self::str_arg(args, "command").unwrap_or("").trim();
        if command.is_empty() {
            bail!("command must not be empty");
        }
        let timeout = Self::int_arg(args, "timeout", 20)?;
        if !(1..=120).contains(&timeout) {
            bail!("timeout must be in [1, 120]");
        }
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&self.root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| anyhow!("failed to run command: {command}: {error}"))?;
        let deadline = Instant::now() + Duration::from_secs(timeout as u64);
        loop {
            if child.try_wait()?.is_some() {
                let output = child.wait_with_output()?;
                return Ok(format_command_output(
                    output.status.code().unwrap_or(-1),
                    &output.stdout,
                    &output.stderr,
                ));
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let output = child.wait_with_output()?;
                return Ok(format!(
                    "{}\n{}",
                    format_command_output(
                        output.status.code().unwrap_or(-1),
                        &output.stdout,
                        &output.stderr
                    ),
                    "error: command timed out"
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn tool_write_file(&self, args: &Map<String, Value>) -> Result<String> {
        let path = self.path(Self::required_str(args, "path")?)?;
        let content = Self::value_to_string(
            args.get("content")
                .ok_or_else(|| anyhow!("missing content"))?,
        );
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, &content)?;
        Ok(format!(
            "wrote {} ({} chars)",
            path.strip_prefix(&self.root).unwrap_or(&path).display(),
            content.chars().count()
        ))
    }

    fn tool_patch_file(&self, args: &Map<String, Value>) -> Result<String> {
        let path = self.path(Self::required_str(args, "path")?)?;
        if !path.is_file() {
            bail!("path is not a file");
        }
        let old_text = Self::str_arg(args, "old_text").unwrap_or("");
        if old_text.is_empty() {
            bail!("old_text must not be empty");
        }
        let new_text = Self::value_to_string(
            args.get("new_text")
                .ok_or_else(|| anyhow!("missing new_text"))?,
        );
        let text = std::fs::read_to_string(&path)?;
        let count = text.matches(old_text).count();
        if count != 1 {
            bail!("old_text must occur exactly once, found {count}");
        }
        std::fs::write(&path, text.replacen(old_text, &new_text, 1))?;
        Ok(format!(
            "patched {}",
            path.strip_prefix(&self.root).unwrap_or(&path).display()
        ))
    }

    fn tool_delegate(&self, args: &Map<String, Value>) -> Result<String> {
        if self.depth >= self.max_depth {
            bail!("delegate depth exceeded");
        }
        let task = Self::str_arg(args, "task").unwrap_or("").trim();
        if task.is_empty() {
            bail!("task must not be empty");
        }
        let mut child = Agent::new(
            Arc::clone(&self.model_client),
            self.workspace.clone(),
            self.session_store.clone(),
            None,
            AgentConfig {
                approval_policy: ApprovalPolicy::Never,
                max_steps: Self::int_arg(args, "max_steps", 3)? as usize,
                max_new_tokens: self.max_new_tokens,
                depth: self.depth + 1,
                max_depth: self.max_depth,
                read_only: true,
            },
        )?;
        child.session.memory.task = task.to_string();
        child.session.memory.notes = vec![clip(self.history_text(), 300)];
        Ok(format!("delegate_result:\n{}", child.ask(task)?))
    }

    fn str_arg<'a>(args: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
        args.get(key).and_then(Value::as_str)
    }

    fn required_str<'a>(args: &'a Map<String, Value>, key: &str) -> Result<&'a str> {
        Self::str_arg(args, key).ok_or_else(|| anyhow!("missing {key}"))
    }

    fn int_arg(args: &Map<String, Value>, key: &str, default: i64) -> Result<i64> {
        match args.get(key) {
            None | Some(Value::Null) => Ok(default),
            Some(Value::Number(number)) => number
                .as_i64()
                .ok_or_else(|| anyhow!("{key} must be an integer")),
            Some(Value::String(text)) => text
                .parse::<i64>()
                .map_err(|_| anyhow!("{key} must be an integer")),
            _ => bail!("{key} must be an integer"),
        }
    }

    fn value_to_string(value: &Value) -> String {
        value
            .as_str()
            .map(ToString::to_string)
            .unwrap_or_else(|| value.to_string())
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn canonicalize_nonexistent(path: &Path) -> Result<PathBuf> {
    let mut probe = path;
    let mut tail = Vec::new();
    while !probe.exists() {
        let Some(parent) = probe.parent() else {
            bail!("path does not have an existing parent: {}", path.display());
        };
        if let Some(name) = probe.file_name() {
            tail.push(name.to_os_string());
        }
        if parent == probe {
            bail!("path does not have an existing parent: {}", path.display());
        }
        probe = parent;
    }
    let mut resolved = probe.canonicalize()?;
    for part in tail.into_iter().rev() {
        resolved.push(part);
    }
    Ok(resolved)
}

fn command_exists(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|path| path.join(name).is_file()))
        .unwrap_or(false)
}

fn has_ignored_component(path: &Path, root: &Path) -> bool {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .any(|component| {
            IGNORED_PATH_NAMES.contains(&component.as_os_str().to_string_lossy().as_ref())
        })
}

fn format_command_output(exit_code: i32, stdout: &[u8], stderr: &[u8]) -> String {
    let stdout = String::from_utf8_lossy(stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(stderr).trim().to_string();
    [
        format!("exit_code: {exit_code}"),
        "stdout:".to_string(),
        if stdout.is_empty() {
            "(empty)".to_string()
        } else {
            stdout
        },
        "stderr:".to_string(),
        if stderr.is_empty() {
            "(empty)".to_string()
        } else {
            stderr
        },
    ]
    .join("\n")
}
