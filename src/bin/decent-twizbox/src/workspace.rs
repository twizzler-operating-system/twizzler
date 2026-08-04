use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::helpers::clip;

const DOC_NAMES: &[&str] = &["AGENTS.md", "README.md", "pyproject.toml", "package.json"];

#[derive(Debug, Clone)]
pub struct WorkspaceContext {
    pub cwd: PathBuf,
    pub repo_root: PathBuf,
    pub branch: String,
    pub default_branch: String,
    pub status: String,
    pub recent_commits: Vec<String>,
    pub project_docs: BTreeMap<String, String>,
}

impl WorkspaceContext {
    pub fn build(cwd: impl AsRef<Path>) -> Self {
        let cwd = cwd
            .as_ref()
            .canonicalize()
            .unwrap_or_else(|_| cwd.as_ref().to_path_buf());

        let git = |args: &[&str], fallback: &str| -> String {
            Command::new("git")
                .args(args)
                .current_dir(&cwd)
                .output()
                .ok()
                .filter(|output| output.status.success())
                .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
                .filter(|text| !text.is_empty())
                .unwrap_or_else(|| fallback.to_string())
        };

        let repo_root = PathBuf::from(git(
            &["rev-parse", "--show-toplevel"],
            &cwd.to_string_lossy(),
        ))
        .canonicalize()
        .unwrap_or_else(|_| cwd.clone());

        let mut docs = BTreeMap::new();
        for base in [&repo_root, &cwd] {
            for name in DOC_NAMES {
                let path = base.join(name);
                if !path.exists() {
                    continue;
                }
                let Ok(relative) = path.strip_prefix(&repo_root) else {
                    continue;
                };
                let key = relative.to_string_lossy().to_string();
                docs.entry(key).or_insert_with(|| {
                    let text = std::fs::read_to_string(&path).unwrap_or_default();
                    clip(text, 1200)
                });
            }
        }

        let branch = git(&["branch", "--show-current"], "-");
        let default_ref = git(
            &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
            "origin/main",
        );
        let default_branch = default_ref
            .strip_prefix("origin/")
            .unwrap_or(&default_ref)
            .to_string();
        let status = git(&["status", "--short"], "clean");
        let recent_commits = git(&["log", "--oneline", "-5"], "")
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(ToString::to_string)
            .collect();

        Self {
            cwd,
            repo_root,
            branch,
            default_branch,
            status: clip(if status.is_empty() { "clean" } else { &status }, 1500),
            recent_commits,
            project_docs: docs,
        }
    }

    pub fn text(&self) -> String {
        let commits = if self.recent_commits.is_empty() {
            "- none".to_string()
        } else {
            self.recent_commits
                .iter()
                .map(|line| format!("- {line}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let docs = if self.project_docs.is_empty() {
            "- none".to_string()
        } else {
            self.project_docs
                .iter()
                .map(|(path, snippet)| format!("- {path}\n{snippet}"))
                .collect::<Vec<_>>()
                .join("\n")
        };

        [
            "Workspace:".to_string(),
            format!("- cwd: {}", self.cwd.display()),
            format!("- repo_root: {}", self.repo_root.display()),
            format!("- branch: {}", self.branch),
            format!("- default_branch: {}", self.default_branch),
            "- status:".to_string(),
            self.status.clone(),
            "- recent_commits:".to_string(),
            commits,
            "- project_docs:".to_string(),
            docs,
        ]
        .join("\n")
    }
}
