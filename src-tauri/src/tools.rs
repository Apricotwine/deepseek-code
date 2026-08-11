//! Tool execution engine. Client-side tools (file ops, shell, search)
//! are executed locally. web_search is handled server-side by DeepSeek's
//! Anthropic endpoint — no client implementation needed.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_call_id: String,
    pub content: String,
    pub success: bool,
}

pub async fn execute_tool(
    name: &str, args: &serde_json::Value,
    tool_call_id: &str, workspace_root: &PathBuf,
) -> ToolResult {
    let result = match name {
        "read_file" => tool_read_file(args, workspace_root).await,
        "write_file" => tool_write_file(args, workspace_root).await,
        "edit_file" => tool_edit_file(args, workspace_root).await,
        "run_shell" => tool_run_shell(args, workspace_root).await,
        "search_code" => tool_search_code(args, workspace_root).await,
        "list_directory" => tool_list_directory(args, workspace_root).await,
        "read_memory" => tool_read_memory(args, workspace_root).await,
        "write_memory" => tool_write_memory(args, workspace_root).await,
        // NOTE: no client-side web_search — it is injected as a DeepSeek
        // server tool in api.rs (web_search_20250305) and never reaches here.
        "web_fetch" => tool_web_fetch(args).await,
        "todo_write" => tool_todo_write(args).await,
        _ => Err(format!("Unknown tool: {}", name)),
    };
    match result {
        Ok(content) => ToolResult { tool_call_id: tool_call_id.to_string(), content, success: true },
        Err(err) => ToolResult { tool_call_id: tool_call_id.to_string(), content: format!("Error: {}", err), success: false },
    }
}

/// Return tool definitions in OpenAI-compatible JSON format.
pub fn get_tool_definitions_json() -> Vec<serde_json::Value> {
    vec![
        tool_json("read_file", "Read the contents of a file at the given path.",
            vec![("path", "string", "Absolute or relative path to the file")], &["path"]),
        tool_json("write_file", "Write content to a file, creating it if it doesn't exist.",
            vec![("path", "string", "Path to the file"), ("content", "string", "Content to write")], &["path", "content"]),
        tool_json("edit_file", "Perform exact string replacement in a file.",
            vec![("path", "string", "Path to the file"), ("old_string", "string", "Exact text to replace"), ("new_string", "string", "Replacement text")], &["path", "old_string", "new_string"]),
        tool_json("run_shell", "Execute a shell command in the workspace directory.",
            vec![("command", "string", "The shell command to execute")], &["command"]),
        tool_json("search_code", "Search for a pattern in project files using grep.",
            vec![("pattern", "string", "The search pattern (regex or literal)"), ("path", "string", "Directory or file to search (default: root)")], &["pattern"]),
        tool_json("list_directory", "List contents of a directory.",
            vec![("path", "string", "Directory path (default: root)")], &[]),
        tool_json("read_memory", "Read project memory files from .deepseek-code/memory/.",
            vec![("name", "string", "Optional: specific memory file name")], &[]),
        tool_json("write_memory", "Write a new memory entry to project knowledge base.",
            vec![("name", "string", "Short kebab-case filename"), ("content", "string", "Markdown content")], &["name", "content"]),
        // web_search: not a client tool — DeepSeek's server tool is injected
        // in api.rs; the model can use it without a local definition.
        tool_json("web_fetch", "Fetch and read the content of a web page.",
            vec![("url", "string", "The URL to fetch")], &["url"]),
        tool_json("todo_write", "Create and manage a structured task list for your current coding session. Use this to plan complex multi-step tasks, track progress, and demonstrate thoroughness. Tasks have status: pending, in_progress, completed, cancelled.",
            vec![("tasks_json", "string", "JSON array of tasks: [{\"id\":\"1\",\"content\":\"Fix bug\",\"status\":\"in_progress\"}]")], &["tasks_json"]),
        // Goal + plan tools (Codex-style): handled by the agent harness, not
        // the filesystem executor. They mutate persisted agent state.
        tool_json("set_goal", "Create or replace the active goal. Use when the user states a concrete objective to pursue across turns, or explicitly asks to set a goal. The goal persists across turns, restarts, and model switches.",
            vec![
                ("objective", "string", "The concrete objective to pursue. Keep the user's full intent; do not shrink it."),
                ("token_budget", "integer", "Optional: positive token budget (only when the user explicitly requests one)"),
            ], &["objective"]),
        tool_json("update_goal", "Update the active goal's status. 'complete' only after verifying the objective against current state, requirement by requirement. 'blocked' only after the same blocker has persisted 3 consecutive turns.",
            vec![("status", "string", "complete or blocked")], &["status"]),
        tool_json("update_plan", "Create or update the task plan for the active goal. Steps keep identity across updates (match by id or content); timestamps are managed by the harness. At most one step may be in_progress. Use estimate_sec for your own duration estimate — the harness calibrates it against measured throughput.",
            vec![
                ("explanation", "string", "Optional short note on why the plan changed"),
                ("plan", "string", "JSON array of steps: [{\"id\"?:\"1\",\"content\" or \"step\":\"Fix bug\",\"status\":\"in_progress\",\"estimate_sec\"?:12,\"blocked_reason\"?:\"\"}]"),
            ], &["plan"]),
        tool_json("get_goal", "Read the current goal: objective, status, token/time usage, and plan steps.",
            vec![], &[]),
    ]
}

fn tool_json(name: &str, desc: &str, props: Vec<(&str, &str, &str)>, required: &[&str]) -> serde_json::Value {
    let mut properties = serde_json::Map::new();
    for (n, t, d) in props {
        let mut prop = serde_json::Map::new();
        prop.insert("type".to_string(), serde_json::Value::String(t.to_string()));
        prop.insert("description".to_string(), serde_json::Value::String(d.to_string()));
        properties.insert(n.to_string(), serde_json::Value::Object(prop));
    }
    let required: Vec<serde_json::Value> = required.iter().map(|r| serde_json::Value::String(r.to_string())).collect();
    serde_json::json!({
        "type": "function",
        "function": {
            "name": name,
            "description": desc,
            "parameters": {
                "type": "object",
                "properties": properties,
                "required": required,
            }
        }
    })
}

// ── Tool implementations ──

async fn tool_read_file(args: &serde_json::Value, root: &PathBuf) -> Result<String, String> {
    let path = args["path"].as_str().ok_or("Missing path")?;
    let full = resolve_path(path, root)?;
    // Size guard — reading a multi-GB file would blow the agent's context.
    const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
    if let Ok(meta) = tokio::fs::metadata(&full).await {
        if meta.len() > MAX_FILE_BYTES {
            return Err(format!(
                "File too large ({:.1} MB, max 2 MB). Use run_shell to inspect it in parts.",
                meta.len() as f64 / 1024.0 / 1024.0
            ));
        }
    }
    tokio::fs::read_to_string(&full).await.map_err(|e| format!("Cannot read {}: {}", full.display(), e))
}

async fn tool_write_file(args: &serde_json::Value, root: &PathBuf) -> Result<String, String> {
    let path = args["path"].as_str().ok_or("Missing path")?;
    let content = args["content"].as_str().ok_or("Missing content")?;
    let full = resolve_path(path, root)?;
    if let Some(p) = full.parent() { tokio::fs::create_dir_all(p).await.map_err(|e| format!("Cannot create dir: {}", e))?; }
    tokio::fs::write(&full, content).await.map_err(|e| format!("Cannot write {}: {}", full.display(), e))?;
    Ok(format!("File written: {}", full.display()))
}

async fn tool_edit_file(args: &serde_json::Value, root: &PathBuf) -> Result<String, String> {
    let path = args["path"].as_str().ok_or("Missing path")?;
    let old = args["old_string"].as_str().ok_or("Missing old_string")?;
    let new = args["new_string"].as_str().ok_or("Missing new_string")?;
    let full = resolve_path(path, root)?;
    let content = tokio::fs::read_to_string(&full).await.map_err(|e| format!("Cannot read {}: {}", full.display(), e))?;
    if !content.contains(old) { return Err("old_string not found".to_string()); }
    let new_content = content.replacen(old, new, 1);
    tokio::fs::write(&full, new_content).await.map_err(|e| format!("Cannot write {}: {}", full.display(), e))?;
    Ok(format!("File edited: {}", full.display()))
}

async fn tool_run_shell(args: &serde_json::Value, root: &PathBuf) -> Result<String, String> {
    let cmd = args["command"].as_str().ok_or("Missing command")?;
    // Hard timeout — a runaway command (`sleep 1000`, infinite loop) must never
    // hang the agent turn forever.
    let t0 = std::time::Instant::now();
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        tokio::process::Command::new("bash").arg("-c").arg(cmd).current_dir(root).output(),
    )
    .await
    .map_err(|_| "Shell command timed out after 20s.".to_string())?
    .map_err(|e| format!("Shell failed: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut r = String::new();
    if !stdout.is_empty() { r.push_str(&stdout); }
    if !stderr.is_empty() { if !r.is_empty() { r.push('\n'); } r.push_str("STDERR:\n"); r.push_str(&stderr); }
    if r.is_empty() { r = format!("Exit: {}", output.status); }
    // Duration calibration (L2 前摄): the model sees how long the command
    // actually ran, anchoring future duration estimates.
    r.push_str(&format!("\n[duration] command took {:.2}s wall-clock", t0.elapsed().as_secs_f64()));
    // Output cap — 50K chars is plenty for any tool result.
    if r.len() > 50_000 {
        let head: String = r.chars().take(50_000).collect();
        r = format!("{}...\n(output truncated at 50K chars)", head);
    }
    Ok(r)
}

async fn tool_search_code(args: &serde_json::Value, root: &PathBuf) -> Result<String, String> {
    let pattern = args["pattern"].as_str().ok_or("Missing pattern")?;
    let sp = args["path"].as_str().map(|p| resolve_path(p, root)).transpose()?.unwrap_or_else(|| root.clone());
    let output = tokio::process::Command::new("grep").arg("-rn")
        .arg("--exclude-dir=node_modules").arg("--exclude-dir=target").arg("--exclude-dir=dist")
        .arg("--exclude-dir=build").arg("--exclude-dir=.git").arg("--exclude-dir=__pycache__")
        .arg("--include=*.rs").arg("--include=*.ts").arg("--include=*.tsx").arg("--include=*.js").arg("--include=*.py")
        .arg("--include=*.html").arg("--include=*.css").arg("--include=*.json").arg("--include=*.md").arg("--include=*.toml")
        .arg("-E").arg(pattern).arg(&sp)
        .output().await.map_err(|e| format!("Search failed: {}", e))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.is_empty() { Ok("No matches found.".to_string()) } else { Ok(stdout.lines().take(100).collect::<Vec<_>>().join("\n")) }
}

async fn tool_list_directory(args: &serde_json::Value, root: &PathBuf) -> Result<String, String> {
    let dp = args["path"].as_str().map(|p| resolve_path(p, root)).transpose()?.unwrap_or_else(|| root.clone());
    let mut entries = tokio::fs::read_dir(&dp).await.map_err(|e| format!("Cannot read dir: {}", e))?;
    let mut listing = Vec::new();
    while let Ok(Some(e)) = entries.next_entry().await {
        let name = e.file_name().to_string_lossy().to_string();
        let is_dir = e.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
        listing.push(format!("{} {}", if is_dir { "D" } else { "F" }, name));
    }
    listing.sort();
    Ok(listing.join("\n"))
}

async fn tool_read_memory(args: &serde_json::Value, root: &PathBuf) -> Result<String, String> {
    let mem = root.join(".deepseek-code").join("memory");
    if let Some(n) = args["name"].as_str() {
        tokio::fs::read_to_string(mem.join(format!("{}.md", n))).await.map_err(|_| format!("Memory '{}' not found.", n))
    } else {
        match tokio::fs::read_dir(&mem).await {
            Ok(mut entries) => {
                let mut listing = Vec::new();
                while let Ok(Some(e)) = entries.next_entry().await {
                    let n = e.file_name().to_string_lossy().to_string();
                    if n.ends_with(".md") { listing.push(n.trim_end_matches(".md").to_string()); }
                }
                Ok(if listing.is_empty() { "No memories.".to_string() } else { listing.join("\n") })
            }
            Err(_) => Ok("No memory directory.".to_string()),
        }
    }
}

async fn tool_write_memory(args: &serde_json::Value, root: &PathBuf) -> Result<String, String> {
    let name = args["name"].as_str().ok_or("Missing name")?;
    let content = args["content"].as_str().ok_or("Missing content")?;
    let mem = root.join(".deepseek-code").join("memory");
    tokio::fs::create_dir_all(&mem).await.map_err(|e| format!("Cannot create dir: {}", e))?;
    tokio::fs::write(mem.join(format!("{}.md", name)), content).await.map_err(|e| format!("Cannot write: {}", e))?;
    Ok(format!("Memory '{}' saved.", name))
}

async fn tool_web_fetch(args: &serde_json::Value) -> Result<String, String> {
    let url = args["url"].as_str().ok_or("Missing url")?;
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("URL must start with http:// or https://".to_string());
    }
    let client = reqwest::Client::builder().user_agent("DeepSeek Code/0.1")
        .timeout(std::time::Duration::from_secs(15)).build().map_err(|e| format!("HTTP client: {}", e))?;
    let resp = client.get(url).send().await.map_err(|e| format!("Fetch failed: {}", e))?;
    if !resp.status().is_success() { return Err(format!("HTTP {}", resp.status())); }

    // Decode with the page's charset (GBK/gb2312 sites like 中新网 are common
    // in CJK web) instead of assuming UTF-8 — reqwest's text() would mangle them.
    let charset: Option<String> = resp.headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|ct| ct.split(';').nth(1))
        .and_then(|p| p.trim().strip_prefix("charset="))
        .map(|s| s.to_string());
    let bytes = resp.bytes().await.map_err(|e| format!("Read failed: {}", e))?;
    let charset = charset.or_else(|| sniff_meta_charset(&bytes));
    let html = match charset.as_deref().map(str::as_bytes).and_then(encoding_rs::Encoding::for_label) {
        Some(enc) => enc.decode(&bytes).0.into_owned(),
        None => String::from_utf8_lossy(&bytes).into_owned(),
    };
    let text = strip_html(&html);
    // char-safe truncation — byte slicing panics on multi-byte UTF-8 (CJK)
    let mut text = if text.len() > 8000 {
        let t: String = text.chars().take(8000).collect();
        format!("{}...", t)
    } else { text };
    // Content-level freshness: a fetch can be fresh while the page is not
    // (cached pages, republished articles). Attach an explicit hint so the
    // model prefers content-embedded timestamps over fetch time.
    if let Some(hint) = sniff_content_time(&text, &url) {
        text.push_str(&format!("\n[content_time_hint={}]", hint));
    }
    Ok(text)
}

/// Detect content-embedded age evidence: relative time phrases ("21 minutes
/// ago", "3天前") and cache-busting epoch parameters in the URL/text (e.g.
/// `r=1504921726666` → 2017). Returns a compact hint, or None. Manual scanner
/// on purpose — patterns are tiny and we avoid a regex dependency.
fn sniff_content_time(text: &str, url: &str) -> Option<String> {
    const CN_UNITS: &[&str] = &["分钟", "小时", "天", "星期", "个月"];
    for unit in CN_UNITS {
        if let Some(idx) = text.find(&format!("{}前", unit)) {
            let before = &text[..idx];
            let n: String = before.chars().rev().take_while(|c| c.is_ascii_digit()).collect();
            if !n.is_empty() {
                let n = n.chars().rev().collect::<String>();
                return Some(format!("relative_time={} {}前 (content)", n, unit));
            }
        }
    }
    for unit in ["minute", "hour", "day", "week", "month"] {
        let marker = format!("{} ago", unit);
        if let Some(idx) = text.to_ascii_lowercase().find(&marker) {
            let before = &text[..idx];
            let n: String = before
                .chars()
                .rev()
                .skip_while(|c| c.is_whitespace())
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if !n.is_empty() {
                let n = n.chars().rev().collect::<String>();
                return Some(format!("relative_time={} {} ago (content)", n, unit));
            }
        }
    }
    // cache-busting epoch ms in URL or text: r=1504921726666
    for hay in [url, text] {
        let bytes = hay.as_bytes();
        for i in 0..bytes.len().saturating_sub(13) {
            if bytes[i] == b'=' && bytes[i + 1..i + 14].iter().all(|b| b.is_ascii_digit()) {
                if let Ok(ms) = hay[i + 1..i + 14].parse::<i64>() {
                    if let Some(dt) = chrono::DateTime::from_timestamp_millis(ms) {
                        let local = dt.with_timezone(&chrono::Local);
                        let now = chrono::Local::now();
                        let age_days = (now - local).num_days().max(0);
                        let when = if age_days > 730 {
                            format!("{} years", age_days / 365)
                        } else if age_days > 60 {
                            format!("{} months", age_days / 30)
                        } else if age_days > 1 {
                            format!("{} days", age_days)
                        } else {
                            format!("{} hours", (now - local).num_hours().max(0))
                        };
                        return Some(format!("cache_param={} (page ~{})", ms, when));
                    }
                }
            }
        }
    }
    None
}

/// Sniff `<meta charset="...">` from the first 4KB of HTML — used by pages
/// whose HTTP header omits the charset.
fn sniff_meta_charset(bytes: &[u8]) -> Option<String> {
    let head = &bytes[..bytes.len().min(4096)];
    let head = String::from_utf8_lossy(head);
    let lower = head.to_ascii_lowercase();
    let idx = lower.find("charset=")?;
    let rest = &lower[idx + "charset=".len()..];
    let value: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '_').collect();
    if value.is_empty() { None } else { Some(value) }
}

fn strip_html(html: &str) -> String {
    let mut r = String::new(); let mut in_tag = false;
    for c in html.chars() {
        if c == '<' { in_tag = true; } else if c == '>' { in_tag = false; } else if !in_tag { r.push(c); }
    }
    let t = r.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#x27;", "'").replace("&nbsp;", " ");
    t.lines().map(|l| l.trim()).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n")
}

async fn tool_todo_write(args: &serde_json::Value) -> Result<String, String> {
    let tasks_json = args["tasks_json"].as_str().ok_or("Missing tasks_json")?;
    // Validate JSON
    serde_json::from_str::<serde_json::Value>(tasks_json)
        .map_err(|e| format!("Invalid JSON: {}", e))?;
    Ok(format!("Tasks updated: {}", tasks_json))
}

fn resolve_path(path: &str, root: &PathBuf) -> Result<PathBuf, String> {
    let p = PathBuf::from(path);
    if p.is_absolute() {
        // Canonicalize the parent so symlinks inside the workspace cannot
        // escape to paths outside it (also covers not-yet-created files).
        let root_c = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
        let parent = p.parent().unwrap_or(&p).to_path_buf();
        let parent_c = std::fs::canonicalize(&parent).unwrap_or(parent);
        let resolved = parent_c.join(p.file_name().unwrap_or_default());
        if resolved.starts_with(&root_c) {
            Ok(resolved)
        } else {
            Err(format!("Access denied: {}", p.display()))
        }
    } else {
        Ok(root.join(p))
    }
}
