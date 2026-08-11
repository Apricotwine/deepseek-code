//! Session manager — persistent conversation history like Cursor.
//!
//! Each session is stored as a JSON file in .deepseek-code/sessions/.
//! An index.json tracks all sessions with metadata.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const SESSIONS_DIR: &str = ".deepseek-code/sessions";
const INDEX_FILE: &str = "index.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionIndex {
    pub sessions: Vec<SessionMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub message_count: usize,
    pub workspace: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub meta: SessionMeta,
    pub messages: Vec<StoredMessage>,
    /// Optional persisted goal + plan (Codex-style thread goal). Legacy
    /// sessions without one deserialize to None.
    #[serde(default)]
    pub goal: Option<SessionGoal>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMessage {
    pub role: String,
    pub content: String,
    pub timestamp: i64,
    pub thinking_content: Option<String>,
    /// Full content blocks (tool_use / tool_result / web search) for messages
    /// saved since the P2 fidelity upgrade. Absent on legacy sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocks: Option<Vec<crate::api::ContentBlock>>,
}

/// Codex-style persisted goal: an objective the agent keeps working toward
/// across turns, with token/time accounting and a plan of steps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionGoal {
    pub id: String,
    pub objective: String,
    pub status: GoalStatus,
    #[serde(default)]
    pub token_budget: Option<u64>,
    #[serde(default)]
    pub tokens_used: u64,
    #[serde(default)]
    pub time_used_seconds: u64,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default)]
    pub plan: Vec<PlanStep>,
    /// Consecutive turns blocked on the same condition (3-turn audit).
    #[serde(default)]
    pub consecutive_blocked_turns: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    Active,
    Paused,
    Blocked,
    BudgetLimited,
    Complete,
}

impl GoalStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Blocked => "blocked",
            Self::BudgetLimited => "budget_limited",
            Self::Complete => "complete",
        }
    }
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::BudgetLimited | Self::Complete)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
    Blocked,
}

impl StepStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Blocked => "blocked",
        }
    }
}

/// One plan step. Timestamps are maintained by the BACKEND when status
/// transitions happen — the model never supplies clocks (that is the whole
/// point of the Time Awareness Layer).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    pub id: String,
    pub content: String,
    pub status: StepStatus,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub started_at: Option<i64>,
    #[serde(default)]
    pub completed_at: Option<i64>,
    #[serde(default)]
    pub blocked_reason: Option<String>,
    /// Model's own duration estimate in seconds — shown raw AND calibrated
    /// against the harness's measured overestimation bias (T2 probe).
    #[serde(default)]
    pub estimate_sec: Option<u64>,
}

pub struct SessionManager {
    sessions_dir: PathBuf,
}

impl SessionManager {
    pub fn new(workspace_root: &PathBuf) -> Self {
        Self {
            sessions_dir: workspace_root.join(SESSIONS_DIR),
        }
    }

    /// List all saved sessions, newest first.
    pub async fn list(&self) -> Result<Vec<SessionMeta>, String> {
        let index_path = self.sessions_dir.join(INDEX_FILE);
        if !index_path.exists() {
            return Ok(Vec::new());
        }
        let data = tokio::fs::read_to_string(&index_path)
            .await
            .map_err(|e| format!("Failed to read session index: {}", e))?;
        let index: SessionIndex =
            serde_json::from_str(&data).unwrap_or(SessionIndex { sessions: vec![] });
        let mut sessions = index.sessions;
        sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(sessions)
    }

    /// Load a full session by ID.
    pub async fn load(&self, id: &str) -> Result<Session, String> {
        let path = self.sessions_dir.join(format!("{}.json", id));
        let data = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| format!("Session not found: {}", e))?;
        serde_json::from_str(&data).map_err(|e| format!("Invalid session data: {}", e))
    }

    /// Save a session. Creates or updates the file and index.
    pub async fn save(&self, session: &Session) -> Result<(), String> {
        tokio::fs::create_dir_all(&self.sessions_dir)
            .await
            .map_err(|e| format!("Cannot create sessions dir: {}", e))?;

        // Write session file
        let path = self.sessions_dir.join(format!("{}.json", session.meta.id));
        // Keep the original created_at on update — auto-save rewrites the file
        // after every turn and must not reset the session's birth date.
        let existing_created = tokio::fs::read_to_string(&path)
            .await
            .ok()
            .and_then(|raw| serde_json::from_str::<Session>(&raw).ok())
            .map(|s| s.meta.created_at);
        let meta = match existing_created {
            Some(created) => SessionMeta { created_at: created, ..session.meta.clone() },
            None => session.meta.clone(),
        };
        let data = serde_json::to_string_pretty(&Session { meta: meta.clone(), messages: session.messages.clone(), goal: session.goal.clone() })
            .map_err(|e| format!("Failed to serialize session: {}", e))?;
        tokio::fs::write(&path, &data)
            .await
            .map_err(|e| format!("Failed to write session: {}", e))?;

        // Update index
        let index_path = self.sessions_dir.join(INDEX_FILE);
        let mut index: SessionIndex = if index_path.exists() {
            let raw = tokio::fs::read_to_string(&index_path).await.unwrap_or_default();
            serde_json::from_str(&raw).unwrap_or(SessionIndex { sessions: vec![] })
        } else {
            SessionIndex { sessions: vec![] }
        };

        // Remove old entry if exists
        index.sessions.retain(|s| s.id != meta.id);
        index.sessions.push(meta);

        let index_data = serde_json::to_string_pretty(&index)
            .map_err(|e| format!("Failed to serialize index: {}", e))?;
        tokio::fs::write(&index_path, &index_data)
            .await
            .map_err(|e| format!("Failed to write index: {}", e))?;

        Ok(())
    }

    /// Delete a session by ID.
    pub async fn delete(&self, id: &str) -> Result<(), String> {
        let path = self.sessions_dir.join(format!("{}.json", id));
        if path.exists() {
            tokio::fs::remove_file(&path)
                .await
                .map_err(|e| format!("Failed to delete session: {}", e))?;
        }

        let index_path = self.sessions_dir.join(INDEX_FILE);
        if index_path.exists() {
            let raw = tokio::fs::read_to_string(&index_path).await.unwrap_or_default();
            let mut index: SessionIndex =
                serde_json::from_str(&raw).unwrap_or(SessionIndex { sessions: vec![] });
            index.sessions.retain(|s| s.id != id);
            let data = serde_json::to_string_pretty(&index).unwrap_or_default();
            tokio::fs::write(&index_path, &data)
                .await
                .map_err(|e| format!("Failed to write index: {}", e))?;
        }

        Ok(())
    }

    /// Generate a title from the first user message.
    #[allow(dead_code)]
    pub fn generate_title(first_message: &str) -> String {
        let cleaned = first_message.trim().replace('\n', " ");
        if cleaned.len() <= 50 {
            cleaned
        } else {
            format!("{}...", &cleaned[..47])
        }
    }
}
