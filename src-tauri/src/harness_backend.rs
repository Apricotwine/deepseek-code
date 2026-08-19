//! Harness-backed turn runner.
//!
//! Bridges the Harness SDK runtime to the existing `agent-stream` event
//! vocabulary so the React frontend keeps its current rendering. This is the
//! minimal single-turn path; goal-mode auto-advance, cancel, and session
//! restore are layered on top later.

use crate::agent::{AgentStreamEvent, TokenUsageInfo};
use crate::harness::{HarnessFrame, HarnessProcess, InitializeParams, text_prompt};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use tauri::{AppHandle, Emitter};

/// Everything needed to launch one harness runtime subprocess.
pub struct HarnessTurnConfig {
    pub node_bin: String,
    pub bin: PathBuf,
    pub cordis: PathBuf,
    pub node_args: Vec<String>,
    pub cwd: String,
    pub api_key: String,
    pub workspace: String,
    pub session_root: String,
    pub model: String,
    pub effort: String,
    pub sandbox: String,
    pub max_tokens: u32,
    pub system_prompt: String,
}

/// The finished turn, shaped to match what `send_message` returns so the
/// frontend can consume either path unchanged.
#[derive(Debug, Clone, Serialize)]
pub struct HarnessTurnResult {
    pub message: String,
    pub thinking_content: String,
    pub token_usage: TokenUsageInfo,
    pub finish_reason: String,
    pub context_usage: u64,
}

fn get<'a>(v: &'a Value, key: &str) -> &'a Value {
    v.get(key).unwrap_or(&Value::Null)
}

/// Join the `texts` fragments of a `text-chunks` / `reasoning-chunks` event.
fn join_texts(value: &Value) -> Option<String> {
    let joined = value
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("");
    if joined.is_empty() {
        None
    } else {
        Some(joined)
    }
}

/// A short human-readable summary for the Trajectory view.
fn trajectory_summary(event: &Value) -> String {
    let ty = get(event, "type").as_str().unwrap_or("?");
    let data = get(event, "data");
    let trunc = |s: &str, n: usize| {
        if s.chars().count() > n {
            let head: String = s.chars().take(n).collect();
            format!("{head}…")
        } else {
            s.to_string()
        }
    };
    match ty {
        "user/message" => trunc(text_of(get(data, "content")).trim(), 140),
        "assistant/message" => trunc(text_of(get(get(data, "message"), "content")).trim(), 140),
        "tool/call" => format!(
            "{} {}",
            get(data, "name").as_str().unwrap_or(""),
            trunc(get(data, "arguments").as_str().unwrap_or(""), 100)
        ),
        "tool/result" => "→ tool result".to_string(),
        "text-chunks" | "reasoning-chunks" => {
            trunc(&join_texts(get(data, "texts")).unwrap_or_default(), 140)
        }
        "session/title" => format!("title: {}", get(data, "title").as_str().unwrap_or("")),
        "time-context" => "clock injected".to_string(),
        _ => ty.to_string(),
    }
}

/// Concatenate the text of a content-block list into one string.
fn text_of(content: &Value) -> String {
    let mut out = String::new();
    if let Some(blocks) = content.as_array() {
        for b in blocks {
            if get(b, "type") == "text" {
                if let Some(t) = get(b, "text").as_str() {
                    out.push_str(t);
                }
            }
        }
    }
    out
}

/// Translate one `session.event` envelope into a frontend event.
///
/// `names` maps a `callId` to the tool name seen in its preceding `tool/call`,
/// so `tool/result` can be paired without re-reading the log.
pub fn translate_harness_event(
    event: &Value,
    effort: &str,
    names: &mut HashMap<String, String>,
) -> Option<AgentStreamEvent> {
    let ty = get(event, "type").as_str()?;
    let data = get(event, "data");
    match ty {
        "turn/start" => Some(AgentStreamEvent::TurnStart {
            thinking_mode: effort.to_string(),
        }),
        "assistant/chunk" => {
            let chunk = get(data, "chunk");
            match get(chunk, "type").as_str() {
                Some("reasoning-delta") => get(chunk, "text")
                    .as_str()
                    .map(|t| AgentStreamEvent::Thinking { text: t.to_string() }),
                Some("text-delta") => get(chunk, "text")
                    .as_str()
                    .map(|t| AgentStreamEvent::Text { content: t.to_string() }),
                Some("block-end") => {
                    let block = get(chunk, "block");
                    if get(block, "type") == "text" {
                        get(block, "text")
                            .as_str()
                            .map(|t| AgentStreamEvent::Text { content: t.to_string() })
                    } else {
                        None
                    }
                }
                _ => None,
            }
        }
        "tool/call" => {
            let id = get(data, "callId").as_str().unwrap_or("").to_string();
            let name = get(data, "name").as_str().unwrap_or("").to_string();
            names.insert(id.clone(), name.clone());
            let args = get(data, "arguments").as_str().unwrap_or("").to_string();
            Some(AgentStreamEvent::ToolStart { id, name, args })
        }
        "tool/result" => {
            // data.message.content is a list of tool-result blocks; take the first.
            let block = get(get(data, "message"), "content")
                .as_array()
                .and_then(|a| a.first())
                .cloned()
                .unwrap_or(Value::Null);
            let id = get(&block, "toolCallId").as_str().unwrap_or("").to_string();
            let name = names.get(&id).cloned().unwrap_or_default();
            let summary = text_of(get(&block, "content"));
            if get(&block, "isError") == &Value::Bool(true) {
                Some(AgentStreamEvent::ToolError { id, name, error: summary })
            } else {
                Some(AgentStreamEvent::ToolDone { id, name, summary })
            }
        }
        "text-chunks" => join_texts(get(data, "texts"))
            .map(|text| AgentStreamEvent::Text { content: text }),
        "reasoning-chunks" => join_texts(get(data, "texts"))
            .map(|text| AgentStreamEvent::Thinking { text }),
        _ => None,
    }
}

/// Run one user turn through the Harness kernel, forwarding events to the UI.
///
/// Returns the accumulated final assistant text.
pub async fn run_harness_turn(
    app: &AppHandle,
    cfg: HarnessTurnConfig,
    session_id: &str,
    message: &str,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<HarnessTurnResult, String> {
    let mut proc = HarnessProcess::spawn(
        &cfg.node_bin,
        cfg.bin.clone(),
        cfg.cordis.clone(),
        &cfg.node_args.iter().map(String::as_str).collect::<Vec<_>>(),
        &[
            ("DEEPSEEK_API_KEY", cfg.api_key.as_str()),
            ("DSH_CWD", cfg.workspace.as_str()),
            ("DSH_SESSION_ROOT", cfg.session_root.as_str()),
            ("DSH_REASONING_EFFORT", cfg.effort.as_str()),
            ("DSH_PERMISSION_MODE", cfg.sandbox.as_str()),
            ("DSH_SYSTEM_PROMPT", cfg.system_prompt.as_str()),
            ("PATH", "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"),
        ],
        &cfg.cwd,
    )
    .map_err(|e| format!("spawn harness: {e}"))?;

    proc.initialize(InitializeParams {
        cwd: cfg.workspace.clone(),
        provider: "deepseek-official".to_string(),
        model: cfg.model.clone(),
        max_tokens: Some(cfg.max_tokens),
    })
    .await
    .map_err(|e| format!("initialize harness: {e}"))?;
    proc.prompt(text_prompt(session_id, message))
        .await
        .map_err(|e| format!("prompt harness: {e}"))?;

    let mut names: HashMap<String, String> = HashMap::new();
    let mut final_text = String::new();
    let mut thinking_content = String::new();
    let mut finish_reason = "completed".to_string();
    let mut usage = TokenUsageInfo {
        input: 0,
        output: 0,
        cached: 0,
        cache_hit_rate: 0.0,
        cache_savings: 0.0,
        thinking_tokens: 0,
    };
    let mut done = false;
    let sleep = tokio::time::sleep(std::time::Duration::from_secs(150));
    tokio::pin!(sleep);
    let mut cancel_ticker = tokio::time::interval(std::time::Duration::from_millis(150));
    cancel_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    while !done {
        tokio::select! {
            _ = cancel_ticker.tick() => {
                if cancel.load(std::sync::atomic::Ordering::SeqCst) {
                    // Dropping the process kills the child (kill_on_drop).
                    drop(proc);
                    return Err("Cancelled by user.".to_string());
                }
            }
            frame = proc.frames.recv() => {
                match frame {
                    Some(HarnessFrame::Response { id, error, .. }) => {
                        if let Some(err) = error {
                            return Err(format!("harness rpc error id={id}: {err}"));
                        }
                    }
                    Some(HarnessFrame::Notification { method, params }) => {
                        if method == "subagent.started" {
                            let _ = app.emit(
                                "agent-stream",
                                AgentStreamEvent::SubagentStarted {
                                    parent_session_id: get(&params, "parentSessionId").as_str().unwrap_or("").to_string(),
                                    child_session_id: get(&params, "childSessionId").as_str().unwrap_or("").to_string(),
                                },
                            );
                            continue;
                        }
                        if method == "subagent.finished" {
                            let _ = app.emit(
                                "agent-stream",
                                AgentStreamEvent::SubagentFinished {
                                    parent_session_id: get(&params, "parentSessionId").as_str().unwrap_or("").to_string(),
                                    child_session_id: get(&params, "childSessionId").as_str().unwrap_or("").to_string(),
                                    status: get(&params, "status").as_str().unwrap_or("").to_string(),
                                    summary: text_of(get(&params, "lastAssistantMessage")),
                                },
                            );
                            continue;
                        }
                        if method != "session.event" {
                            continue;
                        }
                        // Only the root session drives the main conversation.
                        // Subagent sessions stream their own `session.event`
                        // notifications here; keep those out of the chat (the
                        // Agents panel summarizes them via subagent.finished).
                        if get(&params, "sessionId").as_str() != Some(session_id) {
                            continue;
                        }
                        let event = get(&params, "event");
                        let ty = get(event, "type").as_str().unwrap_or("");
                        if !matches!(ty, "text-chunks" | "reasoning-chunks" | "assistant/chunk") {
                            let _ = app.emit(
                                "agent-stream",
                                AgentStreamEvent::Trajectory {
                                    event_type: ty.to_string(),
                                    summary: trajectory_summary(event),
                                },
                            );
                        }
                        if ty == "assistant/chunk" {
                            if let Some(u) = get(get(event, "data"), "chunk").get("usage") {
                                usage.input += get(u, "inputTokens").as_u64().unwrap_or(0);
                                usage.output += get(u, "outputTokens").as_u64().unwrap_or(0);
                                usage.cached += get(u, "cacheReadTokens").as_u64().unwrap_or(0);
                                usage.thinking_tokens += get(u, "reasoningTokens").as_u64().unwrap_or(0);
                            }
                        }
                        if ty == "assistant/chunk" {
                            let chunk = get(get(event, "data"), "chunk");
                            match get(chunk, "type").as_str() {
                                Some("text-delta") => {
                                    if let Some(t) = get(chunk, "text").as_str() {
                                        final_text.push_str(t);
                                    }
                                }
                                Some("reasoning-delta") => {
                                    if let Some(t) = get(chunk, "text").as_str() {
                                        thinking_content.push_str(t);
                                    }
                                }
                                _ => {}
                            }
                        }
                        if ty == "text-chunks" {
                            if let Some(t) = join_texts(get(get(event, "data"), "texts")) {
                                final_text.push_str(&t);
                            }
                        }
                        if ty == "reasoning-chunks" {
                            if let Some(t) = join_texts(get(get(event, "data"), "texts")) {
                                thinking_content.push_str(&t);
                            }
                        }
                        if ty == "assistant/message" {
                            for block in get(get(get(event, "data"), "message"), "content")
                                .as_array()
                                .into_iter()
                                .flatten()
                            {
                                if let Some(t) = get(block, "text").as_str() {
                                    match get(block, "type").as_str() {
                                        Some("text") if final_text.is_empty() => final_text.push_str(t),
                                        Some("reasoning") if thinking_content.is_empty() => thinking_content.push_str(t),
                                        _ => {}
                                    }
                                }
                            }
                        }
                        if let Some(ev) = translate_harness_event(event, &cfg.effort, &mut names) {
                            let _ = app.emit("agent-stream", ev);
                        }
                        if ty == "turn/end" {
                            let kind = get(get(get(event, "data"), "reason"), "kind");
                            if let Some(r) = kind.as_str() {
                                finish_reason = r.to_string();
                            }
                            done = true;
                        }
                    }
                    None => done = true,
                }
            }
            err = proc.stderr.recv() => {
                if let Some(line) = err {
                    eprintln!("[harness stderr] {line}");
                }
            }
            _ = &mut sleep => { done = true; }
        }
    }

    if usage.input > 0 {
        usage.cache_hit_rate = usage.cached as f64 / usage.input as f64;
    }
    let _ = app.emit(
        "agent-stream",
        AgentStreamEvent::TurnEnd {
            finish_reason: finish_reason.clone(),
            token_usage: usage.clone(),
            context_usage: 0,
        },
    );
    let _ = proc.shutdown().await;
    Ok(HarnessTurnResult {
        message: final_text,
        thinking_content,
        token_usage: usage,
        finish_reason,
        context_usage: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn translates_tool_call_and_result_with_name_pairing() {
        let mut names = HashMap::new();
        let call = json!({"type":"tool/call","data":{"callId":"c1","name":"bash","arguments":"{\"command\":\"ls\"}"}});
        let ev = translate_harness_event(&call, "max", &mut names).unwrap();
        match ev {
            AgentStreamEvent::ToolStart { id, name, args } => {
                assert_eq!(id, "c1");
                assert_eq!(name, "bash");
                assert!(args.contains("ls"));
            }
            _ => panic!("expected ToolStart"),
        }

        let result = json!({"type":"tool/result","data":{"message":{"content":[
            {"type":"tool-result","toolCallId":"c1","isError":false,"content":[
                {"type":"text","text":"[data_time=...]\n"},{"type":"text","text":"ok\n"}]}
        ]}}});
        let ev = translate_harness_event(&result, "max", &mut names).unwrap();
        match ev {
            AgentStreamEvent::ToolDone { id, name, summary } => {
                assert_eq!(id, "c1");
                assert_eq!(name, "bash");
                assert!(summary.contains("ok"));
            }
            _ => panic!("expected ToolDone"),
        }
    }

    #[test]
    fn translates_reasoning_and_text_deltas() {
        let mut names = HashMap::new();
        let reasoning = json!({"type":"assistant/chunk","data":{"chunk":{"type":"reasoning-delta","text":"think"}}});
        assert!(matches!(
            translate_harness_event(&reasoning, "high", &mut names),
            Some(AgentStreamEvent::Thinking { .. })
        ));
        let text = json!({"type":"assistant/chunk","data":{"chunk":{"type":"text-delta","text":"hi"}}});
        match translate_harness_event(&text, "high", &mut names) {
            Some(AgentStreamEvent::Text { content }) => assert_eq!(content, "hi"),
            _ => panic!("expected Text"),
        }
    }
}
