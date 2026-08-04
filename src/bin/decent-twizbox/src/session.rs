use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Memory {
    pub task: String,
    pub files: Vec<String>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryItem {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub args: Map<String, Value>,
    pub content: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub created_at: String,
    pub workspace_root: String,
    pub history: Vec<HistoryItem>,
    pub memory: Memory,
}

#[derive(Debug, Clone)]
pub struct SessionStore {
    root: PathBuf,
}

impl SessionStore {
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        std::fs::create_dir_all(&root)
            .with_context(|| format!("failed to create session directory {}", root.display()))?;
        Ok(Self { root })
    }

    pub fn path(&self, session_id: &str) -> PathBuf {
        self.root.join(format!("{session_id}.json"))
    }

    pub fn save(&self, session: &Session) -> Result<PathBuf> {
        let path = self.path(&session.id);
        let text = serde_json::to_string_pretty(session)?;
        std::fs::write(&path, text)
            .with_context(|| format!("failed to write session {}", path.display()))?;
        Ok(path)
    }

    pub fn load(&self, session_id: &str) -> Result<Session> {
        let path = self.path(session_id);
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read session {}", path.display()))?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn latest(&self) -> Result<Option<String>> {
        let mut files = std::fs::read_dir(&self.root)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .collect::<Vec<_>>();
        files.sort_by_key(|path| path.metadata().and_then(|meta| meta.modified()).ok());
        Ok(files
            .last()
            .and_then(|path| path.file_stem())
            .map(|stem| stem.to_string_lossy().to_string()))
    }
}
