//! Context Engine — the "no-compression-first" strategy.
//!
//! Unlike Claude Code (5-stage compaction pipeline) or Codex (API-side
//! /responses/compact), DeepSeek Code leverages the DeepSeek V4 Pro's
//! 1M-token context window to keep the ENTIRE project in view.
//!
//! Key design decisions:
//! - Preload entire project on session start (tree + key files)
//! - Only trigger "soft forget" at >90% usage (900K tokens)
//! - Soft forget: trim old tool outputs, preserve user messages + reasoning
//! - Three-layer memory: Session → Project → Knowledge Graph
//!
//! Token estimation: ~4 chars ≈ 1 token (rough heuristic)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// The context engine manages what goes into each API call.
///
/// DeepSeek V4 Pro has a 1M total window but reserves 384K for output.
/// Effective usable input: ~650K tokens. We trigger soft-forget at 580K
/// to leave headroom before the API's 700K rejection point.
#[allow(dead_code)]
pub struct ContextEngine {
    /// Current estimated token usage
    pub token_usage: u64,
    /// Maximum tokens before soft-forget triggers (580K — safe below API 700K cap)
    pub soft_forget_threshold: u64,
    /// API effective limit (input + 384K output reservation = 1M total)
    pub max_tokens: u64,
    /// DeepSeek's hardcoded output reservation
    pub output_reservation: u64,
    /// Path to workspace root
    workspace_root: PathBuf,
    /// Project file index (path → estimated tokens)
    file_index: HashMap<PathBuf, u64>,
    /// Project structure summary token count
    structure_tokens: u64,
    /// System prompt token count
    system_prompt_tokens: u64,
}

/// Structured representation of the project for sending to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectContext {
    pub workspace_root: String,
    pub structure: String,
    pub key_files: Vec<FileContext>,
    pub git_status: Option<String>,
    pub recent_memories: Vec<String>,
    pub total_estimated_tokens: u64,
}

/// What's actually occupying the context window, per section — powers the
/// 1M-window dashboard in the Monitor panel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextBreakdown {
    pub system_prompt_tokens: u64,
    pub structure_tokens: u64,
    pub key_files_tokens: u64,
    pub key_files: Vec<KeyFileBreakdown>,
    pub conversation_tokens: u64,
    pub tool_definitions_tokens: u64,
    pub total_tokens: u64,
    pub max_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyFileBreakdown {
    pub path: String,
    pub tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileContext {
    pub path: String,
    pub content: String,
    pub language: String,
    pub estimated_tokens: u64,
}

/// System prompt — concise, action-oriented. Avoids encouraging verbose markdown.
pub const SYSTEM_PROMPT: &str = r#"You are DeepSeek Code — 小鲸鱼 (Little Whale), a versatile AI assistant with a 1M-token context window. You have access to tools for reading and writing files, running shell commands, searching code and the web, and managing persistent memory.

You work in a workspace — a folder on the user's computer. You can explore it, create and edit files, run commands, search the web, and remember things across sessions. Adapt to whatever the user needs: software development, research, writing, data analysis, or general problem-solving.

Guidelines:
- Read files before editing them. Don't guess.
- Use the right tool for the job. Shell commands for building/testing. Web search for current information. File tools for content.
- Use markdown naturally when it helps readability — but keep it minimal. No formal report structures or decorative sections.
- Match the user's language. If they write in Chinese, respond in Chinese.
- If you're unsure about something, ask or search rather than assuming.

Persona:
- You go by "小鲸鱼" ("Little Whale") and greet/wrap up with an occasional light ocean-flavored touch — but never at the cost of clarity, precision, or professionalism. A rare whale pun or 摸鱼 joke is welcome; emoji spam is not.
- Stay a calm, reliable companion: cheerful depth, no drama."#;

/// Time Awareness Layer (L0 clock + L1 freshness semantics). Appended to the
/// system prompt when the harness is enabled.
///
/// DELIBERATELY clock-free: the system prompt is built once per session, so an
/// embedded `now` would go stale and create a second, conflicting clock source.
/// The single authoritative clock is the per-request `[time_harness now=...]`
/// stamp on the last user message (agent.rs::stamp_messages_for_wire).
pub fn time_harness_system_section() -> String {
    format!(
        r#"
## Time Awareness Layer

The authoritative wall-clock time is stamped on the current user message as
[time_harness now=...]. Ignore any other time hints unless they are data
timestamps on tool results.

Every tool result in this conversation carries a [data_time=...] annotation
marking the moment the data was produced. Follow these rules:

- Data within its freshness horizon is still valid — reuse it, do not re-query.
- Data older than the horizon is STALE — re-fetch before presenting it, and
  never present stale data as current.
- Decision procedure for every observation (the TAL decision chain):
  1. Read data_time and the current [time_harness now=...] stamp.
  2. Compute the age yourself: now minus data_time. Timestamps carry local
     timezone offsets (e.g. +0800) — convert to one zone before subtracting,
     and do not let a calendar-date rollover fool you (Aug 6 09:00 +0800 is
     still Aug 5 in New York).
  3. Treat the freshness label as potentially wrong — verify from the raw age
     against the horizon, never from the label alone.
  4. Age < horizon → reuse; age >= horizon → re-fetch; never present stale
     data as current.
- Freshness horizons: stock/weather quotes ≈ 15–30 min; package tracking ≈ 6 h;
  web search ≈ 24 h; shell/system state ≈ 1 h; file contents have no expiry
  unless the user asks about current state (then re-read the file).
- Search results can contain pages older than the fetch itself — when content
  carries its own timestamp (e.g. "21 minutes ago", a publication date, or a
  cache-busting URL parameter) that conflicts with fetch time, prefer the
  content-embedded evidence and say so.
- If a claim asserts that something is still valid based on an observation
  older than its horizon, that claim is stale — flag it instead of repeating it.
- When you report a value, mention how fresh it is when it matters.
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_harness_section_is_clock_free_and_has_decision_procedure() {
        let section = time_harness_system_section();
        // Regression guard: a frozen clock in the system prompt created a
        // second, conflicting time source (the 2026-08 audit caught this).
        // The system prompt must stay clock-free; the end-of-input stamp is
        // the single authoritative clock.
        assert!(!section.contains("Current wall-clock time"), "frozen clock leaked back: {section}");
        assert!(section.contains("[time_harness now=...]"), "authoritative clock not referenced: {section}");
        // Decision chain (T1-dc) must stay in the production rules.
        assert!(section.contains("Decision procedure"), "decision chain missing: {section}");
        assert!(section.contains("potentially wrong"), "label-verification rule missing: {section}");
        // Content-level freshness rule (moji-style 2017 cache finding).
        assert!(section.contains("content-embedded"), "content freshness rule missing: {section}");
    }
}

impl ContextEngine {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            token_usage: 0,
            // 1M total - 384K output reserve = 664K usable. Trigger at 580K for headroom.
            soft_forget_threshold: 580_000,
            max_tokens: 1_048_576,
            output_reservation: 384_000,
            workspace_root,
            file_index: HashMap::new(),
            structure_tokens: 0,
            system_prompt_tokens: estimate_tokens(SYSTEM_PROMPT),
        }
    }

    /// Scan the workspace and build the project context.
    /// This is the "preload everything" strategy — load the full project
    /// into the context window at session start.
    pub async fn scan_workspace(&mut self) -> Result<ProjectContext, String> {
        let structure = self.build_structure_summary().await?;
        self.structure_tokens = estimate_tokens(&structure);

        let key_files = self.collect_key_files(50).await?;
        let git_status = self.get_git_status().await;

        let total = self.system_prompt_tokens
            + self.structure_tokens
            + key_files.iter().map(|f| f.estimated_tokens).sum::<u64>();

        self.token_usage = total;

        Ok(ProjectContext {
            workspace_root: self.workspace_root.display().to_string(),
            structure,
            key_files,
            git_status,
            recent_memories: self.load_recent_memories().await,
            total_estimated_tokens: total,
        })
    }

    /// Build a concise directory tree with file annotations.
    async fn build_structure_summary(&self) -> Result<String, String> {
        let mut summary = String::from("## Project Structure\n\n```\n");
        self.walk_dir(&self.workspace_root, &mut summary, 0, 3)
            .await?;
        summary.push_str("```\n");
        Ok(summary)
    }

    async fn walk_dir(
        &self,
        dir: &PathBuf,
        output: &mut String,
        depth: usize,
        max_depth: usize,
    ) -> Result<(), String> {
        if depth > max_depth {
            return Ok(());
        }

        let mut entries = tokio::fs::read_dir(dir)
            .await
            .map_err(|e| format!("Cannot read dir: {}", e))?;

        let mut items = Vec::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            items.push(entry);
        }
        items.sort_by_key(|e| e.file_name());

        for entry in items {
            let name = entry.file_name().to_string_lossy().to_string();
            // Skip hidden dirs and common ignores
            if name.starts_with('.') && name != ".deepseek-code" {
                continue;
            }
            if matches!(
                name.as_str(),
                "node_modules" | "target" | "dist" | "build" | "__pycache__" | ".git"
            ) {
                continue;
            }

            let indent = "  ".repeat(depth);
            if entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
                output.push_str(&format!("{}{}/\n", indent, name));
                Box::pin(self.walk_dir(&entry.path(), output, depth + 1, max_depth)).await?;
            } else {
                output.push_str(&format!("{}{}\n", indent, name));
            }
        }
        Ok(())
    }

    /// Collect key source files for context injection.
    /// Prioritizes: config files, entry points, recently modified files.
    async fn collect_key_files(&mut self, max_files: usize) -> Result<Vec<FileContext>, String> {
        let mut files = Vec::new();
        let priority_exts = [
            "toml", "json", "yaml", "yml", "rs", "ts", "tsx", "js", "py", "md", "html", "css",
        ];

        for ext in &priority_exts {
            if files.len() >= max_files {
                break;
            }
            self.find_files_by_ext(&self.workspace_root, ext, &mut files, max_files)
                .await;
        }

        // Sort by estimated tokens (prefer smaller files for context efficiency)
        files.sort_by_key(|f| f.estimated_tokens);
        files.truncate(max_files);

        // Record the index — used by the context dashboard to show what the
        // workspace scan holds (contents are read on demand via tools).
        self.file_index = files
            .iter()
            .map(|f| (PathBuf::from(&f.path), f.estimated_tokens))
            .collect();

        Ok(files)
    }

    /// Section-level view of the context window. The agent fills in the
    /// sections that live outside this engine (current prompt, conversation,
    /// tool definitions) before sending it to the UI.
    pub fn breakdown(&self) -> ContextBreakdown {
        let mut key_files: Vec<KeyFileBreakdown> = self
            .file_index
            .iter()
            .map(|(p, t)| KeyFileBreakdown { path: p.display().to_string(), tokens: *t })
            .collect();
        key_files.sort_by(|a, b| b.tokens.cmp(&a.tokens));
        let key_files_tokens = key_files.iter().map(|k| k.tokens).sum();
        ContextBreakdown {
            system_prompt_tokens: 0,
            structure_tokens: self.structure_tokens,
            key_files_tokens,
            key_files,
            conversation_tokens: 0,
            tool_definitions_tokens: 0,
            total_tokens: 0,
            max_tokens: self.max_tokens,
        }
    }

    async fn find_files_by_ext(
        &self,
        dir: &PathBuf,
        ext: &str,
        files: &mut Vec<FileContext>,
        max: usize,
    ) {
        if files.len() >= max {
            return;
        }

        let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
            return;
        };

        while let Ok(Some(entry)) = entries.next_entry().await {
            if files.len() >= max {
                return;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            if matches!(
                name.as_str(),
                "node_modules" | "target" | "dist" | "build" | "__pycache__" | ".git"
            ) {
                continue;
            }

            let Ok(ft) = entry.file_type().await else { continue };
            if ft.is_dir() {
                Box::pin(self.find_files_by_ext(&entry.path(), ext, files, max)).await;
            } else if name.ends_with(&format!(".{}", ext)) {
                if let Ok(content) = tokio::fs::read_to_string(entry.path()).await {
                    let tokens = estimate_tokens(&content);
                    // Skip very large files (>50K tokens)
                    if tokens < 50_000 {
                        files.push(FileContext {
                            path: entry
                                .path()
                                .strip_prefix(&self.workspace_root)
                                .unwrap_or(&entry.path())
                                .display()
                                .to_string(),
                            content,
                            language: ext.to_string(),
                            estimated_tokens: tokens,
                        });
                    }
                }
            }
        }
    }

    async fn get_git_status(&self) -> Option<String> {
        let output = tokio::process::Command::new("git")
            .args(["status", "--short"])
            .current_dir(&self.workspace_root)
            .output()
            .await
            .ok()?;

        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if stdout.is_empty() {
                Some("Clean working tree".to_string())
            } else {
                Some(stdout.to_string())
            }
        } else {
            None
        }
    }

    async fn load_recent_memories(&self) -> Vec<String> {
        let memory_dir = self.workspace_root.join(".deepseek-code").join("memory");
        let Ok(mut entries) = tokio::fs::read_dir(&memory_dir).await else {
            return vec![];
        };

        let mut memories = Vec::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".md") {
                if let Ok(content) = tokio::fs::read_to_string(entry.path()).await {
                    memories.push(format!("### {}\n\n{}", name.trim_end_matches(".md"), content));
                }
            }
        }
        memories
    }

    /// Check if compaction is needed. AgentLoop triggers Cursor-style
    /// summarize-based compaction when this fires — the 1M window is huge,
    /// so this only happens in very long sessions.
    pub fn should_compact(&self) -> bool {
        self.token_usage > self.soft_forget_threshold
    }

}

/// Rough token estimator: ~4 characters per token for English text,
/// ~1.5 characters per token for code.
pub fn estimate_tokens(text: &str) -> u64 {
    let len = text.len() as u64;
    // For mixed content (code + English + Chinese), use a blended ratio
    let chinese_chars = text.chars().filter(|c| u32::from(*c) > 0x2000).count() as u64;
    let non_chinese = len.saturating_sub(chinese_chars);

    // Chinese: ~1 char/token, non-Chinese: ~4 chars/token
    chinese_chars + (non_chinese / 4)
}
