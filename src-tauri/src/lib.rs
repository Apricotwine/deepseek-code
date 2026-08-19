mod agent;
mod api;
mod context;
pub mod harness;
mod harness_backend;
mod session;
mod terminal;
mod tools;

use agent::{AgentLoop, AgentTurnResult};
use session::{Session, SessionManager};
use std::path::PathBuf;
use std::sync::Arc;
use tauri::Manager;
use tokio::sync::Mutex;

struct AppState {
    agent: Option<Arc<AgentLoop>>,
    api_key: Option<String>,
    workspace_root: PathBuf,
    /// Stable session store (app data dir), independent of the workspace —
    /// so history survives workspace changes and app updates.
    sessions_dir: PathBuf,
    #[allow(dead_code)]
    current_session_id: Option<String>,
    terminal: Arc<terminal::TerminalManager>,
}

/// Workspace fallback for packaged builds — a Finder-launched app has no
/// meaningful cwd, so default to the user's home directory instead of `/`.
fn default_workspace_root() -> PathBuf {
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")));
    // Never scan $HOME wholesale: macOS TCC protects Desktop/Documents/
    // Downloads, and a Finder-launched (packaged) app lacks the terminal's
    // inherited permissions — scanning home fails with EPERM. Use a dedicated
    // safe workspace folder instead.
    home.join("DeepSeekCodeWorkspace")
}

/// One-time migration: sessions used to live under the workspace root's
/// `.deepseek-code/sessions`. Copy them into the stable app-data store so
/// existing history isn't lost after the move.
async fn migrate_workspace_sessions(workspace_root: &PathBuf, sessions_dir: &PathBuf) {
    if sessions_dir.join("index.json").exists() {
        return; // already migrated or a fresh store
    }
    let old = workspace_root.join(".deepseek-code").join("sessions");
    let Ok(mut entries) = tokio::fs::read_dir(&old).await else {
        return;
    };
    while let Ok(Some(e)) = entries.next_entry().await {
        if e.file_type().await.map(|t| t.is_file()).unwrap_or(false) {
            let name = e.file_name();
            let _ = tokio::fs::copy(e.path(), sessions_dir.join(&name)).await;
        }
    }
}

// ── Agent commands ──

/// 取 agent 引用后立即释放全局状态锁——绝不跨 await 持有。
/// 这是取消路径可用的前提：run_turn 挂起时 cancel_agent 必须能立刻拿到 agent。
macro_rules! with_agent {
    ($state:expr) => {{
        let app = $state.lock().await;
        let agent = app.agent.clone().ok_or("Agent not initialized.")?;
        drop(app);
        agent
    }};
}

#[tauri::command]
async fn init_agent(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
    app_handle: tauri::AppHandle,
    api_key: String,
    workspace_path: Option<String>,
    model: Option<String>,
) -> Result<String, String> {
    let root = match workspace_path {
        Some(ref p) if !p.is_empty() => PathBuf::from(p),
        _ => default_workspace_root(),
    };
    if !root.exists() {
        tokio::fs::create_dir_all(&root)
            .await
            .map_err(|e| format!("Cannot create workspace: {}", e))?;
    }
    let sessions_dir = app_handle
        .path()
        .app_data_dir()
        .map(|d| d.join("sessions"))
        .unwrap_or_else(|_| root.join(".deepseek-code").join("sessions"));
    tokio::fs::create_dir_all(&sessions_dir)
        .await
        .map_err(|e| format!("Cannot create sessions dir: {}", e))?;
    migrate_workspace_sessions(&root, &sessions_dir).await;
    // Flash is the GA agent-tuned model; Pro stays available for max quality.
    let model = match model.as_deref() {
        Some(api::MODEL_FLASH) | Some(api::MODEL_PRO) => model.unwrap(),
        _ => api::MODEL_FLASH.to_string(),
    };
    let agent_loop = AgentLoop::new(api_key.clone(), model, root.clone(), app_handle);
    let msg = agent_loop.initialize().await?;
    {
        let mut app = state.lock().await;
        app.api_key = Some(api_key);
        app.workspace_root = root;
        app.sessions_dir = sessions_dir;
        app.agent = Some(Arc::new(agent_loop));
    }
    Ok(msg)
}

#[tauri::command]
async fn send_message(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
    message: String,
    thinking_mode: Option<String>,
) -> Result<AgentTurnResult, String> {
    let agent = with_agent!(state);
    // Mode label + reasoning budgets are resolved inside the agent, which
    // owns the user-tunable Think/Deep budgets.
    let mut result = agent.run_turn(&message, thinking_mode, false).await?;

    // Goal mode: auto-advance toward the active goal (Codex-style). Each
    // continuation turn is an internal trigger (never stored/rendered as a
    // user bubble); the goal stamp on the wire carries the objective + rules.
    let mut burst = 0u32;
    // Keep the last turn that produced real substance so a trailing
    // "how should I advance?" question never replaces the actual deliverable.
    let mut last_substantive = result.clone();
    let end_reason: Option<String>;
    loop {
        if !agent.should_auto_advance().await? {
            end_reason = Some({
                let mode = agent.goal_mode_state().await?;
                if mode["goal_mode"] != serde_json::Value::Bool(true) {
                    "goal_mode_off".to_string()
                } else {
                    let status = agent
                        .snapshot_goal()
                        .await
                        .map(|g| g.status.as_str().to_string())
                        .unwrap_or_default();
                    match status.as_str() {
                        "complete" => "goal_complete".to_string(),
                        "blocked" => "goal_blocked".to_string(),
                        "budget_limited" => "budget_limited".to_string(),
                        "paused" => "goal_paused".to_string(),
                        _ if burst > 0 => "max_turns".to_string(),
                        _ => "no_goal".to_string(),
                    }
                }
            });
            break;
        }
        if crate::agent::is_stalled_turn(&result) {
            end_reason = Some("stalled".to_string());
            break;
        }
        // Only auto turns (after real work) may stop on a short question;
        // the kickoff/first turn never does, so setting a goal always starts
        // working immediately.
        if burst > 0 && crate::agent::should_stop_after_turn(&result) {
            end_reason = Some("needs_input".to_string());
            result = last_substantive.clone();
            break;
        }
        // Give the user a small window to press ESC between turns.
        tokio::time::sleep(std::time::Duration::from_millis(700)).await;
        if agent.cancelled.load(std::sync::atomic::Ordering::SeqCst) {
            end_reason = Some("cancelled".to_string());
            break;
        }
        burst += 1;
        agent.emit_auto_turn(burst).await;
        let trigger = agent.auto_continuation_message().await;
        match agent.run_turn(&trigger, None, true).await {
            Ok(r) => {
                agent.note_auto_turn().await;
                agent.emit_current_goal().await;
                if !r.message.trim().is_empty() && !crate::agent::should_stop_after_turn(&r) {
                    last_substantive = r.clone();
                }
                result = r;
            }
            Err(_e) => {
                // ESC / cancel is the normal way to stop a burst — return the
                // last good result instead of surfacing a cancel error.
                end_reason = Some(if agent.cancelled.load(std::sync::atomic::Ordering::SeqCst) {
                    "cancelled".to_string()
                } else {
                    "error".to_string()
                });
                break;
            }
        }
    }
    // Only surface the end reason when a burst actually ran. Without this
    // guard, every ordinary message with no goal would emit a spurious
    // "no_goal" event and the UI would print "暂无活跃目标" after every turn.
    if burst > 0 {
        if let Some(reason) = end_reason {
            agent.emit_auto_turn_end(&reason).await;
        }
    }

    Ok(result)
}

#[tauri::command]
async fn set_goal_mode_cmd(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
    enabled: bool,
) -> Result<String, String> {
    let agent = with_agent!(state);
    agent.set_goal_mode(enabled).await
}

#[tauri::command]
async fn set_goal_max_auto_turns_cmd(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
    max: u32,
) -> Result<String, String> {
    let agent = with_agent!(state);
    agent.set_goal_max_auto_turns(max).await
}

#[tauri::command]
async fn get_goal_mode_cmd(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
) -> Result<serde_json::Value, String> {
    let agent = with_agent!(state);
    agent.goal_mode_state().await
}

#[tauri::command]
async fn set_goal_paused_cmd(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
    paused: bool,
) -> Result<String, String> {
    let agent = with_agent!(state);
    agent.set_goal_paused(paused).await
}

#[tauri::command]
async fn switch_model(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
    model: String,
) -> Result<String, String> {
    let agent = with_agent!(state);
    agent.switch_model(model).await
}

#[tauri::command]
async fn send_harness_message(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
    message: String,
    thinking_mode: Option<String>,
    session_id: Option<String>,
    mode: Option<String>,
    sandbox: Option<String>,
) -> Result<harness_backend::HarnessTurnResult, String> {
    let (api_key, workspace_root) = {
        let st = state.lock().await;
        (
            st.api_key.clone().unwrap_or_default(),
            st.workspace_root.display().to_string(),
        )
    };
    if api_key.is_empty() {
        return Err("no DeepSeek API key configured".to_string());
    }

    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mode = mode.unwrap_or_else(|| "standard".to_string());
    let sandbox = sandbox.unwrap_or_else(|| "workspace-write".to_string());
    if !matches!(sandbox.as_str(), "read-only" | "workspace-write" | "danger-full-access") {
        return Err(format!("unknown sandbox mode: {sandbox}"));
    }
    if !matches!(mode.as_str(), "standard" | "minimal" | "ptc" | "creative" | "ralph") {
        return Err(format!("unknown harness mode: {mode}"));
    }
    // Stage the selected preset + zero-dependency TAL plugin into a private
    // config directory. Bare plugins resolve from the runtime (packaged tree or
    // tsx workspace); the relative `./tal-tool-result.mjs` resolves from here.
    let cfg_dir = std::env::temp_dir().join(format!("dsh-config-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&cfg_dir).map_err(|e| e.to_string())?;
    std::fs::copy(
        base.join("../harness/presets").join(format!("{mode}.yml")),
        cfg_dir.join("cordis.yml"),
    )
    .map_err(|e| e.to_string())?;
    std::fs::copy(
        base.join("../harness/tal-tool-result.mjs"),
        cfg_dir.join("tal-tool-result.mjs"),
    )
    .map_err(|e| e.to_string())?;

    let runtime = std::env::var("HARNESS_RUNTIME").ok().filter(|v| !v.is_empty());
    let (bin, cordis, node_args, cwd) = if let Some(rt) = runtime {
        let rt_path = PathBuf::from(&rt);
        (
            rt_path.join("node_modules/@deepseek-ai/dsh-sdk-jsonrpc-demo/lib/packaged-bin.js"),
            cfg_dir.join("cordis.yml"),
            Vec::<String>::new(),
            rt,
        )
    } else {
        let repo = std::env::var("HARNESS_REPO")
            .unwrap_or_else(|_| "/tmp/dsh-harness-upstream".to_string());
        let repo_path = PathBuf::from(&repo);
        (
            repo_path.join("packages/examples/jsonrpc-demo/src/bin.ts"),
            cfg_dir.join("cordis.yml"),
            vec!["--import".to_string(), "tsx".to_string()],
            repo,
        )
    };

    let effort = match thinking_mode.as_deref() {
        Some("non-think") => "off",
        Some("think-max") => "max",
        _ => "high",
    }
    .to_string();
    let persona = std::fs::read_to_string(base.join("../harness/persona.md")).unwrap_or_default();
    let tal = std::fs::read_to_string(base.join("../harness/time-awareness.md"))
        .unwrap_or_default();

    let cfg = harness_backend::HarnessTurnConfig {
        node_bin: "/opt/homebrew/bin/node".to_string(),
        bin,
        cordis,
        node_args,
        cwd,
        api_key,
        workspace: workspace_root,
        session_root: "/tmp/dsh-app-sessions".to_string(),
        model: "deepseek-v4-flash".to_string(),
        effort,
        sandbox,
        max_tokens: 4096,
        system_prompt: format!("{persona}\n\n{tal}"),
    };
    let session_id = session_id.unwrap_or_else(|| "main".to_string());
    harness_backend::run_harness_turn(&app, cfg, &session_id, &message).await
}

#[tauri::command]
async fn set_thinking_budgets(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
    think_budget: u32,
    deep_budget: u32,
) -> Result<String, String> {
    let agent = with_agent!(state);
    agent.set_thinking_budgets(think_budget, deep_budget).await
}

#[tauri::command]
async fn set_time_harness(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
    enabled: bool,
) -> Result<String, String> {
    let agent = with_agent!(state);
    agent.set_time_harness(enabled).await
}

#[tauri::command]
async fn compact_now(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
) -> Result<String, String> {
    let agent = with_agent!(state);
    agent.compact_now().await
}

#[tauri::command]
async fn cancel_agent(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
) -> Result<(), String> {
    // 取消必须即时——同步设置标志后立刻返回，绝不等待任何 async 完成
    let app = state.lock().await;
    if let Some(ref agent) = app.agent {
        agent.cancel();
    }
    Ok(())
}

#[tauri::command]
async fn get_context_usage(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
) -> Result<u64, String> {
    let agent = with_agent!(state);
    Ok(agent.get_token_usage().await)
}

#[tauri::command]
async fn get_context_breakdown(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
) -> Result<context::ContextBreakdown, String> {
    let agent = with_agent!(state);
    agent.get_context_breakdown().await
}

// ── Terminal commands (sidebar PTY) ──

#[tauri::command]
async fn spawn_terminal(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
    app_handle: tauri::AppHandle,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let terminal = { state.lock().await.terminal.clone() };
    terminal.spawn(&app_handle, &id, cols, rows)
}

#[tauri::command]
async fn terminal_input(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
    id: String,
    data: String,
) -> Result<(), String> {
    let terminal = { state.lock().await.terminal.clone() };
    terminal.input(&id, &data)
}

#[tauri::command]
async fn terminal_resize(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let terminal = { state.lock().await.terminal.clone() };
    terminal.resize(&id, cols, rows)
}

#[tauri::command]
async fn kill_terminal(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
    id: String,
) -> Result<(), String> {
    let terminal = { state.lock().await.terminal.clone() };
    terminal.kill(&id);
    Ok(())
}

#[tauri::command]
async fn clear_conversation(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
) -> Result<String, String> {
    let agent = with_agent!(state);
    agent.initialize().await
}

// ── Session commands ──

#[derive(serde::Serialize)]
struct SessionListItem {
    id: String,
    title: String,
    created_at: String,
    updated_at: String,
    message_count: usize,
}

#[tauri::command]
async fn list_sessions(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
) -> Result<Vec<SessionListItem>, String> {
    let root = {
        let app = state.lock().await;
        app.sessions_dir.clone()
    };
    let mgr = SessionManager::new(&root);
    let sessions = mgr.list().await?;
    Ok(sessions
        .into_iter()
        .map(|m| SessionListItem {
            id: m.id,
            title: m.title,
            created_at: m.created_at,
            updated_at: m.updated_at,
            message_count: m.message_count,
        })
        .collect())
}

#[tauri::command]
async fn save_current_session(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
    session_id: String,
    title: String,
    _messages_json: String,
) -> Result<(), String> {
    // The backend is the authority on full conversation context (including
    // tool blocks); the frontend's JSON only triggers the save.
    let agent = with_agent!(state);
    let (root, workspace_str) = {
        let app = state.lock().await;
        (app.sessions_dir.clone(), app.workspace_root.display().to_string())
    };
    let mgr = SessionManager::new(&root);
    let messages = agent.snapshot_messages().await;
    let goal = agent.snapshot_goal().await;

    let now = chrono::Utc::now().to_rfc3339();
    let session = Session {
        meta: session::SessionMeta {
            id: session_id.clone(),
            title,
            created_at: now.clone(),
            updated_at: now,
            message_count: messages.len(),
            workspace: workspace_str,
        },
        messages,
        goal,
    };

    mgr.save(&session).await
}

#[tauri::command]
async fn load_session(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
    session_id: String,
) -> Result<String, String> {
    let root = {
        let app = state.lock().await;
        app.sessions_dir.clone()
    };
    let mgr = SessionManager::new(&root);
    let session = mgr.load(&session_id).await?;
    serde_json::to_string(&session.messages).map_err(|e| format!("Serialize error: {}", e))
}

/// Restore a session's messages into the agent's context — so continuing a
/// loaded history resumes with full context, not a blank slate.
#[tauri::command]
async fn restore_session(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
    session_id: String,
) -> Result<String, String> {
    let (agent, root) = {
        let app = state.lock().await;
        (app.agent.clone().ok_or("Agent not initialized.")?, app.sessions_dir.clone())
    };
    let mgr = SessionManager::new(&root);
    let session = mgr.load(&session_id).await?;
    let restored = agent.restore_session(session.messages).await?;
    agent.restore_goal(session.goal).await;
    Ok(restored)
}

/// Restore the most recent session whose workspace matches the current root
/// — so after an app update/restart the conversation comes back instead of
/// starting from a blank slate. Returns the session id (None if none match).
#[tauri::command]
async fn restore_last_session(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
) -> Result<Option<String>, String> {
    let (agent, sessions_dir, workspace) = {
        let app = state.lock().await;
        (
            app.agent.clone().ok_or("Agent not initialized.")?,
            app.sessions_dir.clone(),
            app.workspace_root.display().to_string(),
        )
    };
    let mgr = SessionManager::new(&sessions_dir);
    let sessions = mgr.list().await?;
    let Some(meta) = sessions.into_iter().find(|m| m.workspace == workspace) else {
        return Ok(None);
    };
    let session = mgr.load(&meta.id).await?;
    let session::Session { messages, goal, .. } = session;
    agent.restore_session(messages).await?;
    agent.restore_goal(goal).await;
    Ok(Some(meta.id))
}

/// UI entry point for setting the active goal directly (no model call).
#[tauri::command]
async fn set_goal_cmd(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
    objective: String,
    token_budget: Option<u64>,
) -> Result<String, String> {
    let agent = with_agent!(state);
    agent.set_goal(objective, token_budget).await
}

/// Read the current goal state for the UI (session switch / startup).
#[tauri::command]
async fn get_goal_cmd(state: tauri::State<'_, Arc<Mutex<AppState>>>) -> Result<Option<serde_json::Value>, String> {
    let agent = with_agent!(state);
    let goal = agent.snapshot_goal().await;
    Ok(goal.map(|g| serde_json::to_value(g).unwrap_or(serde_json::Value::Null)))
}

#[tauri::command]
async fn delete_session(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
    session_id: String,
) -> Result<(), String> {
    let root = {
        let app = state.lock().await;
        app.sessions_dir.clone()
    };
    let mgr = SessionManager::new(&root);
    mgr.delete(&session_id).await
}

// ── File commands ──

#[derive(serde::Serialize)]
struct FileEntry {
    name: String,
    is_dir: bool,
}

#[tauri::command]
async fn list_workspace_files(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
    path: Option<String>,
) -> Result<Vec<FileEntry>, String> {
    let root = {
        let app = state.lock().await;
        app.workspace_root.clone()
    };
    let dir = match &path {
        Some(p) => root.join(p),
        None => root,
    };
    let mut entries = tokio::fs::read_dir(&dir)
        .await
        .map_err(|e| format!("Cannot read directory: {}", e))?;
    let mut files = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || matches!(name.as_str(), "node_modules" | "target" | "dist") {
            continue;
        }
        let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
        files.push(FileEntry { name, is_dir });
    }
    files.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name)));
    Ok(files)
}

#[tauri::command]
async fn read_workspace_file(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
    path: String,
) -> Result<String, String> {
    let root = {
        let app = state.lock().await;
        app.workspace_root.clone()
    };
    let full_path = root.join(&path);
    if !full_path.starts_with(&root) {
        return Err("Access denied.".to_string());
    }
    tokio::fs::read_to_string(&full_path)
        .await
        .map_err(|e| format!("Cannot read {}: {}", full_path.display(), e))
}

#[tauri::command]
async fn write_workspace_file(
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
    path: String,
    content: String,
) -> Result<String, String> {
    let root = {
        let app = state.lock().await;
        app.workspace_root.clone()
    };
    let full_path = root.join(&path);
    if !full_path.starts_with(&root) {
        return Err("Access denied.".to_string());
    }
    if let Some(parent) = full_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| format!("Cannot create dir: {}", e))?;
    }
    tokio::fs::write(&full_path, &content)
        .await
        .map_err(|e| format!("Cannot write {}: {}", full_path.display(), e))?;
    Ok(format!("File written: {}", full_path.display()))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .setup(|app| {
            let state = Arc::new(Mutex::new(AppState {
                agent: None,
                api_key: None,
                workspace_root: default_workspace_root(),
                sessions_dir: default_workspace_root().join(".deepseek-code").join("sessions"),
                current_session_id: None,
                terminal: Arc::new(terminal::TerminalManager::new()),
            }));
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            init_agent,
            send_message,
            send_harness_message,
            switch_model,
            set_thinking_budgets,
            set_time_harness,
            compact_now,
            cancel_agent,
            get_context_usage,
            get_context_breakdown,
            spawn_terminal,
            terminal_input,
            terminal_resize,
            kill_terminal,
            clear_conversation,
            list_sessions,
            save_current_session,
            load_session,
            restore_session,
            restore_last_session,
            set_goal_cmd,
            get_goal_cmd,
            set_goal_mode_cmd,
            set_goal_max_auto_turns_cmd,
            get_goal_mode_cmd,
            set_goal_paused_cmd,
            delete_session,
            list_workspace_files,
            read_workspace_file,
            write_workspace_file,
        ])
        .run(tauri::generate_context!())
        .expect("error while running DeepSeek Code");
}
