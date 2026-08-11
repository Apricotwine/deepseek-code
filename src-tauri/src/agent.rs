//! Agent Loop Engine — PERCEIVE → REASON → VERIFY.
//! Uses DeepSeek's Anthropic-compatible endpoint for native web_search, thinking blocks.

use crate::api::{self, ContentBlock, DeepSeekClient, Message, Role, ThinkingMode};
use crate::context::ContextEngine;
use crate::session::{GoalStatus, PlanStep, SessionGoal, StepStatus, StoredMessage};
use crate::tools;

use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::Emitter;
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinSet;
use uuid::Uuid;

const OVERTHINK_TOKENS: u64 = 30_000;
const MAX_DOWNGRADES: u32 = 2;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentStreamEvent {
    TurnStart { thinking_mode: String },
    Thinking { text: String },
    ToolStart { id: String, name: String, args: String },
    ToolDone { id: String, name: String, summary: String },
    ToolError { id: String, name: String, error: String },
    Text { content: String },
    DiffCreated { path: String, original: String, modified: String },
    TaskList { tasks_json: String },
    GoalUpdate { goal_json: String },
    /// Goal-mode auto-advance: emitted right before each continuation turn.
    AutoTurn { index: u32, max: u32 },
    /// Goal-mode auto-advance ended: reason is goal_complete / max_turns /
    /// cancelled / stalled / goal_mode_off / goal_paused / goal_blocked /
    /// budget_limited.
    AutoTurnEnd { reason: String },
    TurnEnd { finish_reason: String, token_usage: TokenUsageInfo, context_usage: u64 },
}

pub struct AgentState {
    pub system_prompt: String,
    pub messages: Vec<Message>,
    pub context_engine: ContextEngine,
    /// Active model ID (flash / pro) — switchable live without resetting the session.
    pub model: String,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cached_tokens: u64,
    pub total_cost: f64,
    /// USD saved by prompt cache hits (vs paying full input price).
    pub total_cache_savings: f64,
    /// Tokens spent on reasoning (thinking) rather than the final answer.
    pub total_thinking_tokens: u64,
    /// Reasoning budget per mode — user-tunable, DeepSeek bills thinking.
    pub think_budget: u32,
    pub deep_budget: u32,
    /// Time Awareness Layer: injects a live clock + freshness stamps so the
    /// model reasons about staleness instead of guessing. Toggleable for
    /// ablation benchmarking.
    pub time_harness: bool,
    /// Codex-style persisted goal (objective + plan). Survives turns,
    /// restarts, and model switches; stamped on the wire every request.
    pub goal: Option<SessionGoal>,
    /// Measured output throughput (tokens/sec, EMA). Lets the model ground
    /// duration estimates instead of guessing (T2 probe: ~3.85x overestimate).
    pub turn_tokens_per_sec: Option<f64>,
    /// Goal mode: after a turn ends with an active goal, the harness
    /// automatically starts continuation turns until complete / blocked /
    /// budget-limited / max turns / user cancels / the model asks a question.
    pub goal_mode: bool,
    pub goal_max_auto_turns: u32,
    /// Auto turns used since the last manual user message (reset per burst).
    pub auto_turns_used: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentTurnResult {
    pub message: String,
    pub thinking_content: Option<String>,
    pub tool_calls: Option<Vec<ToolCallInfo>>,
    pub token_usage: TokenUsageInfo,
    pub total_cost: f64,
    pub finish_reason: String,
    pub context_usage: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolCallInfo {
    pub id: String, pub name: String,
    pub arguments: serde_json::Value,
    pub result: Option<String>, pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TokenUsageInfo {
    pub input: u64, pub output: u64, pub cached: u64, pub cache_hit_rate: f64,
    pub cache_savings: f64,
    pub thinking_tokens: u64,
}

/// Result of one model call, plus whether the live stream actually delivered
/// content — a batch fallback needs to re-emit the full text/thinking since
/// no deltas reached the UI.
struct ChatTurnOutput {
    response: api::ApiResponse,
    streamed: bool,
}

pub struct AgentLoop {
    client: DeepSeekClient,
    state: Arc<Mutex<AgentState>>,
    workspace_root: PathBuf,
    app_handle: tauri::AppHandle,
    pub cancelled: Arc<AtomicBool>,
}

impl AgentLoop {
    pub fn new(api_key: String, model: String, workspace_root: PathBuf, app_handle: tauri::AppHandle) -> Self {
        let context_engine = ContextEngine::new(workspace_root.clone());
        Self {
            client: DeepSeekClient::new(api_key),
            state: Arc::new(Mutex::new(AgentState {
                system_prompt: String::new(),
                messages: Vec::new(),
                context_engine,
                model,
                total_input_tokens: 0, total_output_tokens: 0,
                total_cached_tokens: 0, total_cost: 0.0,
                total_cache_savings: 0.0,
                total_thinking_tokens: 0,
                think_budget: 16_000,
                deep_budget: 32_000,
                time_harness: true,
                goal: None,
                turn_tokens_per_sec: None,
                goal_mode: true,
                // "Set as goal" semantics: keep working until done. DeepSeek's
                // prefix cache makes each continuation turn cheap (measured
                // ~99% cached share), so a generous safety cap is affordable.
                goal_max_auto_turns: 20,
                auto_turns_used: 0,
            })),
            workspace_root, app_handle,
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn cancel(&self) { self.cancelled.store(true, Ordering::SeqCst); }
    pub fn reset_cancel(&self) { self.cancelled.store(false, Ordering::SeqCst); }

    /// Live model switch — keeps the current session and all messages.
    /// Takes effect from the next API call onward.
    pub async fn switch_model(&self, model: String) -> Result<String, String> {
        if model != api::MODEL_FLASH && model != api::MODEL_PRO {
            return Err(format!("Unknown model: {}", model));
        }
        let mut state = self.state.lock().await;
        state.model = model.clone();

        // Cursor-style takeover note: tell the incoming model it is continuing
        // another model's work, and what its own character is. Costs a cache
        // miss on the next call — Cursor accepts the same tradeoff.
        let label = if model == api::MODEL_FLASH {
            "DeepSeek V4 Flash — agent-tuned, fast and cost-efficient"
        } else {
            "DeepSeek V4 Pro — full-size, deliberate"
        };
        state.system_prompt = format!(
            "{}\n\n## Model Takeover\n\nYou have taken over this conversation from another DeepSeek model. Current model: {}. The complete conversation history — every user message, your predecessor's responses, thinking traces, and tool results — is in your context window; nothing was lost or reset by the switch. Continue the existing work seamlessly: keep the user's goals, prior decisions, and file changes in mind. Do not announce the switch. If the user asks whether you can see the conversation, confirm from context and summarize it directly — do not re-read files to verify.",
            state.system_prompt, label
        );
        Ok(model)
    }

    /// Update the reasoning budgets for Think / Deep modes. Takes effect from
    /// the next turn onward; values are clamped to DeepSeek's sane range.
    pub async fn set_thinking_budgets(&self, think_budget: u32, deep_budget: u32) -> Result<String, String> {
        let think = think_budget.clamp(1_000, 64_000);
        let deep = deep_budget.clamp(4_000, 196_608);
        {
            let mut state = self.state.lock().await;
            state.think_budget = think;
            state.deep_budget = deep;
        }
        Ok(format!("Thinking budgets updated: Think={} tok, Deep={} tok.", think, deep))
    }

    /// Toggle the Time Awareness Layer. Re-initializes the system prompt so
    /// the harness section appears / disappears for the next turn.
    pub async fn set_time_harness(&self, enabled: bool) -> Result<String, String> {
        {
            let mut state = self.state.lock().await;
            state.time_harness = enabled;
            // Rebuild the prompt with/without the time layer.
            let project_ctx = state.context_engine.scan_workspace().await?;
            state.system_prompt = build_system_prompt(&project_ctx, enabled);
        }
        Ok(if enabled {
            "Time Awareness Layer enabled — tool results now carry freshness stamps.".to_string()
        } else {
            "Time Awareness Layer disabled.".to_string()
        })
    }

    /// Text-space context compaction (Cursor-style): when the window is nearly
    /// full, summarize the older history via a cheap API call and keep the most
    /// recent messages verbatim. Far better than dropping messages outright.
    /// Returns a human-readable outcome (used by /compact).
    async fn compact_context(&self) -> Result<String, String> {
        let (model, to_summarize, keep_verbatim) = {
            let state = self.state.lock().await;
            let mut split = state.messages.len().saturating_sub(6);
            // Never split an assistant tool_use message from its user
            // tool_result: if the kept suffix starts with a tool_result whose
            // tool_use was just summarized away, the wire would 400. Move the
            // boundary back so the whole pair stays verbatim.
            while split > 0 && split < state.messages.len() {
                let prev_is_tool_use = matches!(
                    &state.messages[split - 1].content,
                    api::Content::Blocks(bs)
                        if bs.iter().any(|b| matches!(b, ContentBlock::ToolUse { .. }))
                );
                let cur_is_tool_result = matches!(
                    &state.messages[split].content,
                    api::Content::Blocks(bs)
                        if bs.iter().any(|b| matches!(b, ContentBlock::ToolResult { .. }))
                );
                if prev_is_tool_use && cur_is_tool_result {
                    split -= 1;
                } else {
                    break;
                }
            }
            (
                state.model.clone(),
                state.messages[..split].to_vec(),
                state.messages[split..].to_vec(),
            )
        };
        if to_summarize.is_empty() {
            return Ok("Nothing to compact — context is healthy.".to_string());
        }

        let summarizer_prompt = "You are a conversation summarizer. Compress the conversation below into a dense summary preserving: the user's goals and requirements, all decisions made, file paths touched, tool outputs that still matter, and outstanding tasks. Output compact plain prose with at most a few short bullets. No markdown headers, no code fences. Target under 600 tokens.";
        let summary = self
            .client
            .chat(
                summarizer_prompt,
                to_summarize,
                None,
                api::ThinkingMode::NonThink,
                &model,
                Some(self.cancelled.clone()),
            )
            .await?
            .content;

        let mut state = self.state.lock().await;
        let mut new_messages: Vec<Message> = Vec::new();
        if !summary.trim().is_empty() {
            new_messages.push(Message {
                role: Role::User,
                content: api::Content::Text(format!(
                    "## Conversation Summary (compacted earlier)\n\n{}",
                    summary
                )),
                timestamp: chrono::Local::now().timestamp_millis(),
                internal: false,
            });
        }
        new_messages.extend(keep_verbatim);

        let estimated: u64 = new_messages.iter().map(estimate_message_tokens).sum();
        state.messages = new_messages;
        state.context_engine.token_usage = estimated;

        self.emit(AgentStreamEvent::ToolStart {
            id: "ctx-compact".to_string(),
            name: "compact_context".to_string(),
            args: "Summarizing history to keep the context window healthy".to_string(),
        });
        self.emit(AgentStreamEvent::ToolDone {
            id: "ctx-compact".to_string(),
            name: "compact_context".to_string(),
            summary: format!("History compressed to ~{} tokens", estimated),
        });
        Ok(format!("Context compacted to ~{} tokens.", estimated))
    }

    /// Manual compaction — invoked by the /compact slash command.
    pub async fn compact_now(&self) -> Result<String, String> {
        self.compact_context().await
    }

    /// Restore a stored conversation into the agent's context so continuing
    /// a loaded history actually has its context (Cursor-style resume).
    pub async fn restore_session(&self, stored: Vec<StoredMessage>) -> Result<String, String> {
        let mut messages = Vec::with_capacity(stored.len());
        for m in stored {
            // Full fidelity: sessions saved since the P2 upgrade carry their
            // content blocks (tool_use / tool_result / web search). Time
            // stamps are applied later, at request time, so stored history
            // stays raw and never double-stamped.
            let msg = match m.role.as_str() {
                "user" => {
                    let content = match m.blocks {
                        Some(blocks) => api::Content::Blocks(blocks),
                        None => api::Content::Text(m.content),
                    };
                    Message { role: Role::User, content, timestamp: m.timestamp, internal: false }
                }
                "assistant" => {
                    if let Some(blocks) = m.blocks {
                        Message {
                            role: Role::Assistant,
                            content: api::Content::Blocks(blocks),
                            timestamp: m.timestamp,
                            internal: false,
                        }
                    } else {
                        let mut blocks = Vec::new();
                        if let Some(t) = m.thinking_content {
                            if !t.is_empty() {
                                blocks.push(ContentBlock::Thinking { thinking: t, signature: None });
                            }
                        }
                        if !m.content.is_empty() {
                            blocks.push(ContentBlock::Text { text: m.content });
                        }
                        Message {
                            role: Role::Assistant,
                            content: api::Content::Blocks(blocks),
                            timestamp: m.timestamp,
                            internal: false,
                        }
                    }
                }
                _ => continue,
            };
            messages.push(msg);
        }
        if messages.is_empty() {
            return Err("Session has no usable messages.".to_string());
        }
        let mut state = self.state.lock().await;
        state.messages = messages;
        state.context_engine.token_usage = state.messages.iter().map(estimate_message_tokens).sum();
        Ok(format!("Restored {} messages into context.", state.messages.len()))
    }

    fn emit(&self, event: AgentStreamEvent) {
        let _ = self.app_handle.emit("agent-stream", event);
    }

    /// Run one model call with live SSE streaming. Text/thinking deltas are
    /// forwarded to the UI the moment they arrive. If the stream dies before
    /// producing any content (proxy incompatibility, network blip), fall back
    /// to the batch endpoint so the turn still completes.
    async fn chat_turn(
        &self,
        system: String,
        messages: Vec<api::Message>,
        tools_json: Vec<serde_json::Value>,
        mode: api::ThinkingMode,
        model: String,
        time_harness: bool,
    ) -> Result<ChatTurnOutput, String> {
        // Stamp ages + the current clock at request time (L0/L1). Stored
        // history stays raw; every request re-derives ages, so a long turn
        // keeps the model's sense of elapsed time fresh.
        let (goal, calibration) = {
            let state = self.state.lock().await;
            (state.goal.clone(), duration_calibration_factor(&state.model))
        };
        let messages = stamp_messages_for_wire(
            messages,
            chrono::Local::now().timestamp_millis(),
            time_harness,
            goal.as_ref(),
            calibration,
        );
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<api::ApiStreamEvent>();
        let client = self.client.clone();
        let cancelled = self.cancelled.clone();
        // Clone before the `async move` block: the originals stay available
        // for the batch fallback below.
        let (system_c, messages_c, tools_c, mode_c, model_c) = (
            system.clone(),
            messages.clone(),
            tools_json.clone(),
            mode.clone(),
            model.clone(),
        );
        let handle = tokio::spawn(async move {
            client
                .chat_stream(
                    &system_c,
                    messages_c,
                    Some(tools_c),
                    mode_c,
                    &model_c,
                    Some(cancelled),
                    tx,
                )
                .await
        });

        // Batch deltas into ~40ms windows before emitting Tauri events —
        // per-delta emits flood the IPC channel and force a React render per
        // event. Coalescing keeps the UI smooth without losing any content.
        let mut streamed = false;
        let mut pending_text = String::new();
        let mut pending_thinking = String::new();
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(40));

        let flush = |pending_text: &mut String, pending_thinking: &mut String| {
            if !pending_thinking.is_empty() {
                self.emit(AgentStreamEvent::Thinking {
                    text: std::mem::take(pending_thinking),
                });
            }
            if !pending_text.is_empty() {
                self.emit(AgentStreamEvent::Text {
                    content: std::mem::take(pending_text),
                });
            }
        };

        loop {
            tokio::select! {
                ev = rx.recv() => {
                    match ev {
                        Some(ev) => {
                            streamed = true;
                            match ev {
                                api::ApiStreamEvent::TextDelta { text } => pending_text.push_str(&text),
                                api::ApiStreamEvent::ThinkingDelta { text } => pending_thinking.push_str(&text),
                            }
                        }
                        None => break,
                    }
                }
                _ = ticker.tick() => {
                    if self.cancelled.load(Ordering::SeqCst) {
                        handle.abort();
                        return Err("Cancelled by user.".to_string());
                    }
                    flush(&mut pending_text, &mut pending_thinking);
                }
            }
        }
        flush(&mut pending_text, &mut pending_thinking);

        let inner: Result<api::ApiResponse, String> = handle
            .await
            .map_err(|_| "Stream task panicked (internal error).".to_string())?;
        match inner {
            Ok(response) => Ok(ChatTurnOutput { response, streamed }),
            Err(_e) if !streamed => {
                // Nothing reached the user yet — retry the same call as a
                // plain batch request rather than failing the turn.
                let response = self
                    .client
                    .chat(&system, messages, Some(tools_json), mode, &model, Some(self.cancelled.clone()))
                    .await?;
                Ok(ChatTurnOutput { response, streamed: false })
            }
            Err(e) => Err(e),
        }
    }

    /// Blocks injected by DeepSeek's server-side web search. They must be
    /// echoed back in the assistant message: `server_tool_use` pairs with its
    /// `web_search_tool_result` via `tool_use_id`, and DeepSeek rejects
    /// conversation history that drops either side.
    fn server_tool_blocks(response: &api::ApiResponse) -> Vec<ContentBlock> {
        let mut blocks = Vec::new();
        for st in &response.server_tool_uses {
            let input: serde_json::Value =
                serde_json::from_str(&st.function.arguments).unwrap_or(serde_json::json!({}));
            blocks.push(ContentBlock::ServerToolUse {
                id: st.id.clone(),
                name: st.function.name.clone(),
                input,
            });
        }
        for wb in &response.web_search_results {
            // L1 stamp on server-side search results. Web results are fetched
            // by DeepSeek's own tool at ~now, so they carry the 24h horizon —
            // but the CONTENT inside may be far older (page caches), which the
            // freshness rules tell the model to check via embedded timestamps.
            let content_str = match &wb.content {
                serde_json::Value::String(s) => s.clone(),
                other => serde_json::to_string(other).unwrap_or_default(),
            };
            let stamped = annotate_tool_result("web_search", &content_str, &now_str());
            blocks.push(ContentBlock::WebSearchResult {
                tool_use_id: wb.tool_use_id.clone().unwrap_or_default(),
                title: wb.title.clone(),
                url: wb.url.clone(),
                content: serde_json::Value::String(stamped),
            });
        }
        blocks
    }

    pub async fn initialize(&self) -> Result<String, String> {
        let mut state = self.state.lock().await;
        let project_ctx = state.context_engine.scan_workspace().await?;
        state.system_prompt = build_system_prompt(&project_ctx, state.time_harness);
        state.messages.clear();
        // New session = no goal. (restore_goal re-attaches one when loading.)
        state.goal = None;
        Ok(format!(
            "Session initialized. {} files indexed, {} tokens loaded.",
            project_ctx.key_files.len(), project_ctx.total_estimated_tokens
        ))
    }

    pub async fn run_turn(
        &self,
        user_message: &str,
        thinking_mode_override: Option<String>,
        internal: bool,
    ) -> Result<AgentTurnResult, String> {
        // Time Awareness Layer flag + clock for this turn (L0).
        let time_harness = {
            let state = self.state.lock().await;
            state.time_harness
        };
        let turn_now = now_str();
        let turn_start = std::time::Instant::now();

        // Resolve the UI's mode label against the user-tunable reasoning
        // budgets stored on the agent (DeepSeek bills thinking tokens, so the
        // budget is a real cost dial, not decoration).
        let mut effective_mode = {
            let state = self.state.lock().await;
            match thinking_mode_override.as_deref() {
                Some("non-think") => ThinkingMode::NonThink,
                Some("think-max") => ThinkingMode::ThinkMax { budget_tokens: state.deep_budget },
                _ => ThinkingMode::ThinkHigh { budget_tokens: state.think_budget },
            }
        };

        self.emit(AgentStreamEvent::TurnStart {
            thinking_mode: format!("{:?}", effective_mode),
        });

        // Add user message
        {
            let mut state = self.state.lock().await;
            state.messages.push(Message {
                role: Role::User,
                content: api::Content::Text(user_message.to_string()),
                timestamp: chrono::Local::now().timestamp_millis(),
                internal,
            });
            // A manual (non-internal) message starts a fresh auto-advance burst.
            if !internal {
                state.auto_turns_used = 0;
            }
            // A fresh user message resumes the goal: reset the blocked audit
            // (Codex: resumed runs start a fresh audit) and clear paused.
            if let Some(goal) = state.goal.as_mut() {
                match goal.status {
                    GoalStatus::Blocked => goal.consecutive_blocked_turns = 0,
                    GoalStatus::Paused => goal.status = GoalStatus::Active,
                    _ => {}
                }
            }
        }

        let tools_json = tools::get_tool_definitions_json();
        let mut cumulative_thinking_tokens: u64 = 0;
        let mut downgrade_count: u32 = 0;

        self.reset_cancel();

        // Window nearly full? Summarize history before this turn's calls
        // (text-space compaction — keeps long sessions healthy).
        {
            let state = self.state.lock().await;
            let needs = state.context_engine.should_compact() && state.messages.len() > 10;
            drop(state);
            if needs {
                let _ = self.compact_context().await?;
            }
        }

        let mut last_call_at = turn_start;
        let final_response = loop {
            if self.cancelled.load(Ordering::SeqCst) {
                return Err("Cancelled by user.".to_string());
            }
            if cumulative_thinking_tokens > OVERTHINK_TOKENS * 50 {
                return Err("Agent loop exceeded maximum iterations.".to_string());
            }

            let (system, messages, model) = {
                let state = self.state.lock().await;
                (state.system_prompt.clone(), state.messages.clone(), state.model.clone())
            };

            let ChatTurnOutput { response, streamed } = self
                .chat_turn(system, messages, tools_json.clone(), effective_mode.clone(), model, time_harness)
                .await?;

            // Streaming already forwarded thinking/text deltas live; the batch
            // fallback path re-emits the complete payload here.
            if !streamed {
                if let Some(ref t) = response.thinking_content {
                    if !t.is_empty() {
                        self.emit(AgentStreamEvent::Thinking { text: t.clone() });
                    }
                }
            }

            // Emit web_search activity if DeepSeek used it server-side —
            // surface the actual query so the user knows what was searched.
            if response.web_search_used {
                let query = response.server_tool_uses.first()
                    .and_then(|st| serde_json::from_str::<serde_json::Value>(&st.function.arguments).ok())
                    .and_then(|v| v["query"].as_str().map(String::from))
                    .unwrap_or_else(|| "Searching the web...".to_string());
                let search_id = format!("ws-{}", Uuid::new_v4());
                self.emit(AgentStreamEvent::ToolStart {
                    id: search_id.clone(),
                    name: "web_search".to_string(),
                    args: query.clone(),
                });
                self.emit(AgentStreamEvent::ToolDone {
                    id: search_id,
                    name: "web_search".to_string(),
                    summary: format!("Searching: {}", query),
                });
            }

            // Track tokens
            {
                let mut state = self.state.lock().await;
                let call_elapsed = last_call_at.elapsed().as_secs_f64();
                last_call_at = std::time::Instant::now();
                state.total_input_tokens += response.usage.input_tokens as u64;
                state.total_output_tokens += response.usage.output_tokens as u64;
                state.total_cached_tokens += response.usage.cache_read_input_tokens as u64;
                let pricing = api::Pricing::for_model(&state.model);
                state.total_cost += pricing.calculate(&response.usage);
                state.total_cache_savings += pricing.cache_savings(&response.usage);
                // Track the CURRENT context size (the API's input_tokens is
                // exactly the live prompt the model saw), not a cumulative
                // counter — otherwise soft-forget fires on total tokens
                // consumed instead of the actual 1M window usage.
                state.context_engine.token_usage = response.usage.input_tokens as u64;
                // Measured throughput (L2 grounding): EMA of tokens/sec.
                if call_elapsed > 0.1 && response.usage.output_tokens > 0 {
                    let tps = response.usage.output_tokens as f64 / call_elapsed;
                    state.turn_tokens_per_sec = Some(match state.turn_tokens_per_sec {
                        Some(prev) => prev * 0.7 + tps * 0.3,
                        None => tps,
                    });
                }
                // Goal budget accounting, once per model call.
                if let Some(goal) = state.goal.as_mut() {
                    goal.tokens_used = goal
                        .tokens_used
                        .saturating_add(response.usage.input_tokens as u64)
                        .saturating_add(response.usage.output_tokens as u64)
                        .saturating_add(response.usage.cache_read_input_tokens as u64);
                    goal.time_used_seconds =
                        goal.time_used_seconds.saturating_add(call_elapsed as u64);
                    if let Some(b) = goal.token_budget {
                        if goal.tokens_used >= b && goal.status == GoalStatus::Active {
                            goal.status = GoalStatus::BudgetLimited;
                        }
                    }
                    goal.updated_at = chrono::Local::now().timestamp_millis();
                }
            }

            let thinking_tokens_this_turn = response.usage.output_tokens.saturating_sub(
                estimate_text_tokens(&response.content)
            ) as u64;
            cumulative_thinking_tokens += thinking_tokens_this_turn;
            {
                let mut state = self.state.lock().await;
                state.total_thinking_tokens += thinking_tokens_this_turn;
            }

            let has_tools = response.tool_calls.as_ref().map(|t| !t.is_empty()).unwrap_or(false);

            // Overthinking detection
            if !has_tools && response.content.trim().is_empty()
                && thinking_tokens_this_turn > OVERTHINK_TOKENS
                && downgrade_count < MAX_DOWNGRADES
            {
                downgrade_count += 1;
                cumulative_thinking_tokens = 0;
                self.emit(AgentStreamEvent::Thinking {
                    text: format!("Overthinking detected. Downgrading to Non-think ({}/{}).", downgrade_count, MAX_DOWNGRADES),
                });
                effective_mode = ThinkingMode::NonThink;
                continue;
            }

            if !has_tools {
                // Text response — done
                if !streamed && !response.content.is_empty() {
                    self.emit(AgentStreamEvent::Text { content: response.content.clone() });
                }
                // Build assistant message with content blocks
                let mut blocks: Vec<ContentBlock> = Vec::new();
                if let Some(ref t) = response.thinking_content {
                    if !t.is_empty() {
                        blocks.push(ContentBlock::Thinking {
                            thinking: t.clone(),
                            signature: response.thinking_signature.clone(),
                        });
                    }
                }
                if !response.content.is_empty() {
                    blocks.push(ContentBlock::Text { text: response.content.clone() });
                }
                // Server-side web search blocks must round-trip verbatim —
                // DeepSeek pairs server_tool_use ↔ web_search_tool_result by
                // tool_use_id and rejects history missing either side.
                blocks.extend(Self::server_tool_blocks(&response));
                let mut state = self.state.lock().await;
                state.messages.push(Message {
                    role: Role::Assistant,
                    content: api::Content::Blocks(blocks),
                    timestamp: chrono::Local::now().timestamp_millis(),
                    internal: false,
                });
                break response;
            }

            // Tool calls
            let tool_calls = response.tool_calls.clone().unwrap();

            // Stream text prefix
            if !streamed && !response.content.is_empty() {
                self.emit(AgentStreamEvent::Text { content: response.content.clone() });
            }

            // Build assistant content blocks
            let mut blocks: Vec<ContentBlock> = Vec::new();
            if let Some(ref t) = response.thinking_content {
                if !t.is_empty() {
                    blocks.push(ContentBlock::Thinking {
                        thinking: t.clone(),
                        signature: response.thinking_signature.clone(),
                    });
                }
            }
            if !response.content.is_empty() {
                blocks.push(ContentBlock::Text { text: response.content.clone() });
            }
            for tc in &tool_calls {
                // DeepSeek sometimes emits parameterless tool_use blocks —
                // serialize `input` as an object, never null (schema rejects
                // null and the next request would 400).
                let input: serde_json::Value = serde_json::from_str(&tc.function.arguments).unwrap_or(serde_json::json!({}));
                blocks.push(ContentBlock::ToolUse {
                    id: tc.id.clone(),
                    name: tc.function.name.clone(),
                    input,
                });
            }
            // Same server-side web search round-trip as the text branch.
            blocks.extend(Self::server_tool_blocks(&response));
            {
                let mut state = self.state.lock().await;
                state.messages.push(Message {
                    role: Role::Assistant,
                    content: api::Content::Blocks(blocks),
                    timestamp: chrono::Local::now().timestamp_millis(),
                    internal: false,
                });
            }

            // Execute tools IN PARALLEL — DeepSeek V4 Flash natively supports
            // parallel tool calls. All results still merge into ONE user message
            // as the Anthropic protocol requires. Concurrency capped at 4.
            //
            // Protocol safety: every tool_use MUST get a tool_result. We register
            // every id in a shared map BEFORE spawning, so even a panicked task
            // (which JoinSet swallows) can be answered with its REAL id — never a
            // fabricated one, which would 400 the whole turn.
            let semaphore = Arc::new(Semaphore::new(4));
            let mut set = JoinSet::new();
            let results: Arc<std::sync::Mutex<HashMap<String, Option<ContentBlock>>>> =
                Arc::new(std::sync::Mutex::new(HashMap::new()));
            for tc in &tool_calls {
                let id = tc.id.clone();
                let name = tc.function.name.clone();
                let args_json = tc.function.arguments.clone();
                let args: serde_json::Value =
                    serde_json::from_str(&args_json).unwrap_or(serde_json::Value::Null);
                let root = self.workspace_root.clone();
                let app_handle = self.app_handle.clone();
                let cancelled = self.cancelled.clone();
                let sem = semaphore.clone();
                let results = results.clone();
                let turn_now = turn_now.clone();
                let goal_state = self.state.clone();

                results.lock().unwrap().insert(id.clone(), None);

                set.spawn(async move {
                    let block = async {
                        let _permit = match sem.acquire().await {
                            Ok(p) => p,
                            Err(_) => return tool_result_err(&id, "Tool semaphore closed."),
                        };

                        app_handle
                            .emit("agent-stream", AgentStreamEvent::ToolStart {
                                id: id.clone(), name: name.clone(), args: args_json,
                            })
                            .ok();

                        // ESC pressed while queued? Skip the work, answer cancelled.
                        if cancelled.load(Ordering::SeqCst) {
                            return tool_result_err(&id, "Cancelled by user.");
                        }

                        // Goal/plan tools are handled by the harness: they mutate
                        // agent state (not the filesystem) and are state, not
                        // observations, so they never carry [data_time] stamps.
                        if is_goal_tool(&name) {
                            return handle_goal_tool(&name, &args, &id, goal_state.clone(), &app_handle).await;
                        }

                        // Read original content before modification for diff tracking
                        let original = match name.as_str() {
                            "edit_file" | "write_file" => {
                                let path = args["path"].as_str().unwrap_or("");
                                tool_read_file_for_diff(path, &root).await
                            }
                            _ => None,
                        };

                        let result = tools::execute_tool(&name, &args, &id, &root).await;

                        // Long outputs spill to a file; the agent can read more on demand
                        let content = if result.success {
                            let c = store_long_output_if_needed(&name, &result.content, &id, &root).await;
                            if time_harness {
                                annotate_tool_result(&name, &c, &turn_now)
                            } else {
                                c
                            }
                        } else {
                            result.content.clone()
                        };

                        if result.success {
                            let summary = summarize_tool_result(&name, &content);
                            app_handle
                                .emit("agent-stream", AgentStreamEvent::ToolDone {
                                    id: id.clone(), name: name.clone(), summary,
                                })
                                .ok();

                            // Emit diff for file modifications
                            if name == "edit_file" || name == "write_file" {
                                if let (Some(orig), Some(path)) = (original, args["path"].as_str()) {
                                    if let Ok(full) = resolve_path(path, &root) {
                                        if let Ok(new_content) = tokio::fs::read_to_string(&full).await {
                                            if orig != new_content {
                                                app_handle
                                                    .emit("agent-stream", AgentStreamEvent::DiffCreated {
                                                        path: path.to_string(),
                                                        original: orig,
                                                        modified: new_content,
                                                    })
                                                    .ok();
                                            }
                                        }
                                    }
                                }
                            }

                            // Emit task list for todo_write
                            if name == "todo_write" {
                                if let Some(tasks_json) = args["tasks_json"].as_str() {
                                    app_handle
                                        .emit("agent-stream", AgentStreamEvent::TaskList {
                                            tasks_json: tasks_json.to_string(),
                                        })
                                        .ok();
                                }
                            }
                        } else {
                            app_handle
                                .emit("agent-stream", AgentStreamEvent::ToolError {
                                    id: id.clone(), name: name.clone(), error: content.clone(),
                                })
                                .ok();
                        }

                        ContentBlock::ToolResult { tool_use_id: id.clone(), content }
                    }
                    .await;

                    if let Ok(mut map) = results.lock() {
                        map.insert(id, Some(block));
                    }
                });
            }

            // Wait for all tasks, then collect in declaration order.
            // A panicked task leaves its slot as None → answer with its real id.
            while set.join_next().await.is_some() {}
            let mut tool_result_blocks: Vec<ContentBlock> = Vec::new();
            for tc in &tool_calls {
                let block = results
                    .lock()
                    .map(|m| m.get(&tc.id).and_then(|b| b.clone()))
                    .unwrap_or(None)
                    .unwrap_or_else(|| tool_result_err(&tc.id, "Tool execution panicked (internal error)."));
                tool_result_blocks.push(block);
            }


            // Push a SINGLE user message with ALL tool_result blocks
            {
                let mut state = self.state.lock().await;
                state.messages.push(Message {
                    role: Role::User,
                    content: api::Content::Blocks(tool_result_blocks),
                    timestamp: chrono::Local::now().timestamp_millis(),
                    internal: false,
                });
            }
        };

        // Build result
        let state = self.state.lock().await;
        let cache_hit_rate = if state.total_input_tokens > 0 {
            state.total_cached_tokens as f64 / state.total_input_tokens as f64
        } else { 0.0 };

        let usage = TokenUsageInfo {
            input: state.total_input_tokens,
            output: state.total_output_tokens,
            cached: state.total_cached_tokens,
            cache_hit_rate,
            cache_savings: state.total_cache_savings,
            thinking_tokens: state.total_thinking_tokens,
        };
        let ctx = state.context_engine.token_usage;

        self.emit(AgentStreamEvent::TurnEnd {
            finish_reason: final_response.finish_reason.clone(),
            token_usage: usage.clone(),
            context_usage: ctx,
        });

        Ok(AgentTurnResult {
            message: final_response.content,
            thinking_content: final_response.thinking_content,
            tool_calls: None,
            token_usage: usage,
            total_cost: state.total_cost,
            finish_reason: final_response.finish_reason,
            context_usage: ctx,
        })
    }

    pub async fn get_token_usage(&self) -> u64 {
        let state = self.state.lock().await;
        state.context_engine.token_usage
    }

    /// Section-level snapshot of the 1M context window — powers the Monitor
    /// dashboard. Estimates of what each request actually carries.
    pub async fn get_context_breakdown(&self) -> Result<crate::context::ContextBreakdown, String> {
        let state = self.state.lock().await;
        let system_total = crate::context::estimate_tokens(&state.system_prompt);
        let conversation = state.messages.iter().map(estimate_message_tokens).sum::<u64>();
        let tool_defs = crate::context::estimate_tokens(
            &serde_json::to_string(&tools::get_tool_definitions_json()).unwrap_or_default(),
        );
        let mut b = state.context_engine.breakdown();
        // The system prompt embeds the structure tree (plus git + memory);
        // split the base persona out so the dashboard shows both.
        b.system_prompt_tokens = system_total.saturating_sub(b.structure_tokens);
        b.conversation_tokens = conversation;
        b.tool_definitions_tokens = tool_defs;
        b.total_tokens = system_total + conversation + tool_defs;
        Ok(b)
    }

    /// Serialize the current conversation (including tool blocks) for session
    /// storage — the backend is the authority on full context; the frontend
    /// only decides WHEN to save.
    pub async fn snapshot_messages(&self) -> Vec<StoredMessage> {
        let state = self.state.lock().await;
        state
            .messages
            .iter()
            .filter(|m| !m.internal)
            .map(stored_from_message)
            .collect()
    }

    /// Serialize the current goal for session storage.
    pub async fn snapshot_goal(&self) -> Option<SessionGoal> {
        let state = self.state.lock().await;
        state.goal.clone()
    }

    /// Restore a persisted goal into the agent (called after restore_session).
    pub async fn restore_goal(&self, goal: Option<SessionGoal>) {
        let mut state = self.state.lock().await;
        state.goal = goal;
    }

    /// UI entry point: set/replace the active goal without a model call.
    pub async fn set_goal(&self, objective: String, token_budget: Option<u64>) -> Result<String, String> {
        if objective.trim().is_empty() {
            return Err("Objective must not be empty.".to_string());
        }
        let now = chrono::Local::now().timestamp_millis();
        let goal = SessionGoal {
            id: Uuid::new_v4().to_string(),
            objective: objective.trim().to_string(),
            status: GoalStatus::Active,
            token_budget,
            tokens_used: 0,
            time_used_seconds: 0,
            created_at: now,
            updated_at: now,
            plan: Vec::new(),
            consecutive_blocked_turns: 0,
        };
        {
            let mut state = self.state.lock().await;
            state.goal = Some(goal.clone());
            state.auto_turns_used = 0;
        }
        emit_goal_update(&self.app_handle, &goal);
        Ok(goal.objective)
    }

    /// Goal mode: allow automatic continuation turns toward the active goal.
    pub async fn set_goal_mode(&self, enabled: bool) -> Result<String, String> {
        let mut state = self.state.lock().await;
        state.goal_mode = enabled;
        if !enabled {
            state.auto_turns_used = 0;
        }
        Ok(if enabled { "Goal mode enabled".to_string() } else { "Goal mode disabled".to_string() })
    }

    pub async fn set_goal_max_auto_turns(&self, max: u32) -> Result<String, String> {
        let mut state = self.state.lock().await;
        state.goal_max_auto_turns = max.clamp(1, 100);
        Ok(format!("Max auto turns: {}", state.goal_max_auto_turns))
    }

    /// User-controlled pause/resume (Codex ThreadGoalStatus::Paused semantics):
    /// paused stops auto-advance; resume clears it and starts a fresh blocked
    /// audit (same rule as resuming after a manual user message).
    pub async fn set_goal_paused(&self, paused: bool) -> Result<String, String> {
        let mut state = self.state.lock().await;
        let goal = state
            .goal
            .as_mut()
            .ok_or_else(|| "No active goal.".to_string())?;
        goal.status = if paused {
            GoalStatus::Paused
        } else {
            goal.consecutive_blocked_turns = 0;
            GoalStatus::Active
        };
        goal.updated_at = chrono::Local::now().timestamp_millis();
        let g = goal.clone();
        drop(state);
        emit_goal_update(&self.app_handle, &g);
        Ok(if paused { "Goal paused".to_string() } else { "Goal resumed".to_string() })
    }

    /// Snapshot for the UI: goal-mode toggle state + burst counter.
    pub async fn goal_mode_state(&self) -> Result<serde_json::Value, String> {
        let state = self.state.lock().await;
        Ok(serde_json::json!({
            "goal_mode": state.goal_mode,
            "goal_max_auto_turns": state.goal_max_auto_turns,
            "auto_turns_used": state.auto_turns_used,
        }))
    }

    /// Should the auto-advance loop start (or keep going) after a turn?
    pub async fn should_auto_advance(&self) -> Result<bool, String> {
        let state = self.state.lock().await;
        if !state.goal_mode {
            return Ok(false);
        }
        let goal = match state.goal.as_ref() {
            Some(g) => g,
            None => return Ok(false),
        };
        if goal.status != GoalStatus::Active {
            return Ok(false);
        }
        if state.auto_turns_used >= state.goal_max_auto_turns {
            return Ok(false);
        }
        Ok(true)
    }

    /// Hidden trigger message for a continuation turn (Codex-style internal
    /// context fragment; never stored, never rendered as a user bubble).
    pub async fn auto_continuation_message(&self) -> String {
        let objective = {
            let state = self.state.lock().await;
            state.goal.as_ref().map(|g| g.objective.clone()).unwrap_or_default()
        };
        format!(
            "（目标模式自动续作）继续推进当前目标，不要为了等待输入而停步。\
             基于现有上下文与目标直接推进；若确实被阻塞、需要用户输入或无法再取得进展，\
             停止并明确说明。当前目标：{objective}"
        )
    }

    pub async fn emit_auto_turn(&self, index: u32) {
        let max = {
            let state = self.state.lock().await;
            state.goal_max_auto_turns
        };
        let _ = self.emit(AgentStreamEvent::AutoTurn { index, max });
    }

    /// Account one finished auto-continuation turn (burst budget).
    pub async fn note_auto_turn(&self) {
        let mut state = self.state.lock().await;
        state.auto_turns_used = state.auto_turns_used.saturating_add(1);
    }

    pub async fn emit_auto_turn_end(&self, reason: &str) {
        let _ = self.emit(AgentStreamEvent::AutoTurnEnd { reason: reason.to_string() });
    }

    /// Re-broadcast the current goal so the UI's accounting (tokens / time)
    /// stays fresh after every auto turn, not only on plan/goal changes.
    pub async fn emit_current_goal(&self) {
        let goal = {
            let state = self.state.lock().await;
            state.goal.clone()
        };
        if let Some(g) = goal {
            emit_goal_update(&self.app_handle, &g);
        }
    }
}

fn stored_from_message(m: &Message) -> StoredMessage {
    let (content, blocks, thinking) = match &m.content {
        api::Content::Text(t) => (t.clone(), None, None),
        api::Content::Blocks(blocks) => {
            let text = blocks
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            let thinking = blocks.iter().find_map(|b| match b {
                ContentBlock::Thinking { thinking, .. } => Some(thinking.clone()),
                _ => None,
            });
            (text, Some(blocks.clone()), thinking)
        }
    };
    StoredMessage {
        role: match m.role {
            Role::User => "user".to_string(),
            Role::Assistant => "assistant".to_string(),
        },
        content,
        timestamp: m.timestamp,
        thinking_content: thinking,
        blocks,
    }
}

fn estimate_text_tokens(text: &str) -> u32 {
    (text.len() as u32) / 4
}

/// Local wall-clock string for the Time Awareness Layer (L0 clock).
fn now_str() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S %z").to_string()
}

/// Compact age for harness stamps ("45min", "3h", "2d"). Empty for missing
/// timestamps — the stamp is simply omitted then.
fn human_age_ms(ms: i64) -> String {
    if ms <= 0 {
        return String::new();
    }
    let mins = ms / 60_000;
    if mins < 1 {
        return "0min".to_string();
    }
    if mins < 60 {
        return format!("{}min", mins);
    }
    let hours = mins / 60;
    // Keep hour precision inside ~2 days — "26h" vs the 24h freshness
    // horizon is a meaningful distinction that "1d" would blur.
    if hours < 48 {
        return format!("{}h", hours);
    }
    format!("{}d", hours / 24)
}

/// Local wall-clock string for a stored message timestamp (ms epoch).
fn msg_time_str(ms: i64) -> String {
    if ms <= 0 {
        return String::new();
    }
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|dt| dt.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S %z").to_string())
        .unwrap_or_default()
}

/// Stamp a freshly produced tool result with its production time (L1).
fn annotate_tool_result(tool: &str, content: &str, now: &str) -> String {
    format!(
        "[data_time={now} age=0min freshness=just_fetched horizon={}]\n{content}",
        freshness_horizon(tool)
    )
}

/// Per-tool freshness horizon, exposed in the stamp so the model compares
/// directly instead of guessing (L1 semantics, matched in the system prompt).
fn freshness_horizon(tool: &str) -> &'static str {
    match tool {
        "get_stock_price" | "get_weather" => "30min",
        "track_package" => "6h",
        "web_search" => "24h",
        "run_shell" => "1h",
        // File contents do not expire; only re-read when the user asks about
        // current state (matches the rules table in the system prompt).
        "read_file" => "none (file contents don't expire)",
        _ => "1h",
    }
}

/// T2 probe result (9-task large benchmark): DeepSeek V4 Flash overestimates
/// its own generation duration ~3.71x; V4 Pro UNDERestimates ~0.56x (its
/// predictions are ~1.8x too short). Calibration is model-specific.
fn duration_calibration_factor(model: &str) -> f64 {
    if model.to_ascii_lowercase().contains("pro") { 0.56 } else { 3.71 }
}

/// Goal/plan tools mutate agent state rather than the filesystem. They are
/// intercepted before `tools::execute_tool` in the agent loop.
fn is_goal_tool(name: &str) -> bool {
    matches!(name, "set_goal" | "update_goal" | "update_plan" | "get_goal")
}

/// Build the goal/plan block appended to the wire on user turns. Ages are
/// computed live exactly like message stamps; steps that have sat idle past
/// the session horizon are flagged STALE so the model re-verifies before
/// resuming (Codex's "work from evidence", made quantitative).
fn goal_section_for_wire(goal: &SessionGoal, now_ms: i64, temporal: bool, calibration: f64) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "[goal id={} status={} tokens_used={}",
        goal.id,
        goal.status.as_str(),
        goal.tokens_used
    ));
    if let Some(b) = goal.token_budget {
        s.push_str(&format!(
            " token_budget={} remaining={}",
            b,
            b.saturating_sub(goal.tokens_used)
        ));
    }
    s.push_str(&format!(" time_used={}s]\n", goal.time_used_seconds));
    // A freshly set goal must take over the conversation: the model has
    // strong inertia from previous work, so make the switch explicit.
    if goal.created_at > 0 && now_ms - goal.created_at < 120_000 {
        s.push_str("[NOTE: this goal was JUST SET — it is the current task. \
                    Switch to it now; do not continue unrelated previous work.]\n");
    }
    s.push_str(&format!("Objective: {}\n", goal.objective));

    if goal.plan.is_empty() {
        s.push_str("Plan: (none yet — call update_plan if the work is multi-step.)\n");
    } else {
        s.push_str("Plan:\n");
        for (i, step) in goal.plan.iter().enumerate() {
            let age = if temporal { human_age_ms(now_ms - step.created_at) } else { String::new() };
            let mut meta = if temporal {
                match step.status {
                    StepStatus::InProgress => {
                        let started = step
                            .started_at
                            .map(|t| human_age_ms(now_ms - t))
                            .unwrap_or_default();
                        let mut m = format!(
                            "started {} ago",
                            if started.is_empty() { "just now" } else { &started }
                        );
                        if let Some(est) = step.estimate_sec {
                            let calibrated = est as f64 / calibration;
                            m.push_str(&format!(
                                " · model ETA {}s · calibrated {:.0}s",
                                est, calibrated
                            ));
                        }
                        m
                    }
                    StepStatus::Completed => step
                        .completed_at
                        .map(|t| format!("finished {} ago", human_age_ms(now_ms - t)))
                        .unwrap_or_default(),
                    _ => String::new(),
                }
            } else {
                String::new()
            };
            if step.status == StepStatus::Blocked {
                if let Some(r) = &step.blocked_reason {
                    meta = if meta.is_empty() { r.clone() } else { format!("{} · {}", meta, r) };
                }
            }
            s.push_str(&format!(
                "  {}. [{}] {}{}{}\n",
                i + 1,
                step.status.as_str(),
                step.content,
                if age.is_empty() { String::new() } else { format!(" (age={})", age) },
                if meta.is_empty() { String::new() } else { format!(" ({})", meta) },
            ));
            if temporal
                && !age.is_empty()
                && step.status != StepStatus::Completed
                && step.status != StepStatus::InProgress
            {
                s.push_str("     STALE — re-verify the worktree before resuming.\n");
            }
        }
    }

    s.push_str("Rules:\n");
    s.push_str("- Work from current evidence: the worktree and external state are authoritative; inspect before relying on older context.\n");
    s.push_str("- Keep the plan current as steps complete or the next best action changes; at most one step in_progress.\n");
    s.push_str("- Mark the goal complete only after verifying the objective against the current state, requirement by requirement.\n");
    s.push_str("- You may be auto-continuing (goal mode): keep making concrete progress each turn; do not stall by asking the user unless truly blocked, and do not loop on the same step.\n");
    s.push_str(&format!(
        "- Mark blocked only after the same blocker persists 3 consecutive turns (currently {}/3).\n",
        goal.consecutive_blocked_turns
    ));
    if goal.status == GoalStatus::BudgetLimited {
        s.push_str("- Budget exhausted: wrap up gracefully, summarize what remains, and do not start new work.\n");
    }
    s
}

fn parse_step_status(v: &str) -> Result<StepStatus, String> {
    match v {
        "pending" => Ok(StepStatus::Pending),
        "in_progress" => Ok(StepStatus::InProgress),
        "completed" => Ok(StepStatus::Completed),
        "cancelled" => Ok(StepStatus::Cancelled),
        "blocked" => Ok(StepStatus::Blocked),
        other => Err(format!("Invalid step status: {other}")),
    }
}

/// Pure core of the update_plan tool: merge the model's step list into the
/// goal's plan, preserving identity (id or content match) and backend-owned
/// timestamps (created/started/completed are only ever set by the harness).
/// Enforces Codex's "at most one in_progress" rule and resets the blocked
/// audit when the plan makes progress. Extracted so the merge semantics are
/// unit-testable without a Tauri handle.
fn apply_plan_update(
    goal: &mut SessionGoal,
    steps: &[serde_json::Value],
    now: i64,
) -> Result<String, String> {
    let mut next: Vec<PlanStep> = Vec::with_capacity(steps.len());
    for st in steps {
        // Accept both "content" and "step" (Codex's canonical field name) —
        // models trained on Codex-style tools often emit "step".
        let content = st["content"]
            .as_str()
            .or_else(|| st["step"].as_str())
            .ok_or("Missing step content")?;
        let status = parse_step_status(st["status"].as_str().ok_or("Missing step status")?)?;
        let id = st["id"].as_str().map(|v| v.to_string());
        // Accept integer or float estimates (models sometimes emit 12.5).
        let estimate = st["estimate_sec"]
            .as_u64()
            .or_else(|| st["estimate_sec"].as_f64().map(|f| f.round() as u64));
        let prev = goal.plan.iter().find(|p| {
            Some(p.id.as_str()) == id.as_deref() || (id.is_none() && p.content == content)
        });
        let mut step = match prev {
            Some(p) => p.clone(),
            None => PlanStep {
                id: id.unwrap_or_else(|| Uuid::new_v4().to_string()),
                content: content.to_string(),
                status: StepStatus::Pending,
                created_at: now,
                started_at: None,
                completed_at: None,
                blocked_reason: None,
                estimate_sec: None,
            },
        };
        if step.status != status {
            if status == StepStatus::InProgress {
                step.started_at = Some(step.started_at.unwrap_or(now));
            }
            if status == StepStatus::Completed && step.completed_at.is_none() {
                step.completed_at = Some(now);
            }
            if status == StepStatus::Blocked {
                step.blocked_reason = st["blocked_reason"]
                    .as_str()
                    .map(|v| v.to_string())
                    .or_else(|| step.blocked_reason.clone());
            }
            step.status = status;
        }
        if let Some(e) = estimate {
            step.estimate_sec = Some(e);
        }
        next.push(step);
    }
    let in_progress = next
        .iter()
        .filter(|p| p.status == StepStatus::InProgress)
        .count();
    if in_progress > 1 {
        return Err("At most one step can be in_progress at a time.".to_string());
    }
    let made_progress = next
        .iter()
        .any(|p| matches!(p.status, StepStatus::Completed | StepStatus::Cancelled));
    goal.plan = next;
    if made_progress {
        goal.consecutive_blocked_turns = 0;
        if goal.status == GoalStatus::Blocked {
            goal.status = GoalStatus::Active;
        }
    }
    goal.updated_at = now;
    Ok(format!("Plan updated: {} steps", goal.plan.len()))
}

fn emit_goal_update(app_handle: &tauri::AppHandle, goal: &SessionGoal) {
    if let Ok(json) = serde_json::to_string(goal) {
        let _ = app_handle.emit(
            "agent-stream",
            AgentStreamEvent::GoalUpdate { goal_json: json },
        );
    }
}

/// Safety valve for the auto-advance burst: a genuinely stalled turn (no
/// content produced) stops the loop.
pub(crate) fn is_stalled_turn(result: &AgentTurnResult) -> bool {
    result.message.trim().is_empty()
}

/// Question heuristic for AUTO turns: once real work has happened (burst > 0),
/// a short question ending the turn means the model genuinely needs input —
/// stop instead of burning turns. Never applied to the first (user/kickoff)
/// turn, so goal setting always starts working immediately.
pub(crate) fn should_stop_after_turn(result: &AgentTurnResult) -> bool {
    let text = result.message.trim();
    if text.is_empty() {
        return false; // handled by is_stalled_turn
    }
    let ends_with_question = text.ends_with('?') || text.ends_with('？');
    text.chars().count() < 300 && ends_with_question
}

/// Execute a goal/plan tool against agent state. Returns the tool result
/// content (success) or an error string. Emits GoalUpdate events so the UI
/// pipeline stays live.
async fn handle_goal_tool(
    name: &str,
    args: &serde_json::Value,
    tool_call_id: &str,
    state: Arc<Mutex<AgentState>>,
    app_handle: &tauri::AppHandle,
) -> ContentBlock {
    let result: Result<String, String> = async {
        match name {
            "get_goal" => {
                let s = state.lock().await;
                let goal = s
                    .goal
                    .as_ref()
                    .ok_or_else(|| "No active goal — call set_goal first.".to_string())?;
                serde_json::to_string_pretty(goal).map_err(|e| e.to_string())
            }
            "set_goal" => {
                let objective = args["objective"].as_str().ok_or("Missing objective")?;
                {
                    let s = state.lock().await;
                    if let Some(existing) = s.goal.as_ref() {
                        if !existing.status.is_terminal() {
                            return Err(format!(
                                "An unfinished goal already exists (status={}). \
                                 Finish it (update_goal complete) or ask the user to replace it \
                                 from the UI before setting a new one.",
                                existing.status.as_str()
                            ));
                        }
                    }
                }
                let now = chrono::Local::now().timestamp_millis();
                let goal = SessionGoal {
                    id: Uuid::new_v4().to_string(),
                    objective: objective.to_string(),
                    status: GoalStatus::Active,
                    token_budget: args["token_budget"].as_u64(),
                    tokens_used: 0,
                    time_used_seconds: 0,
                    created_at: now,
                    updated_at: now,
                    plan: Vec::new(),
                    consecutive_blocked_turns: 0,
                };
                {
                    let mut s = state.lock().await;
                    s.goal = Some(goal.clone());
                    s.auto_turns_used = 0;
                }
                emit_goal_update(app_handle, &goal);
                Ok(format!("Goal set: {}", goal.objective))
            }
            "update_goal" => {
                let status = args["status"].as_str().ok_or("Missing status")?;
                let mut s = state.lock().await;
                let goal = s
                    .goal
                    .as_mut()
                    .ok_or_else(|| "No active goal — call set_goal first.".to_string())?;
                match status {
                    "complete" => {
                        goal.status = GoalStatus::Complete;
                        let now = chrono::Local::now().timestamp_millis();
                        for step in &mut goal.plan {
                            if step.status == StepStatus::InProgress {
                                step.status = StepStatus::Completed;
                                step.completed_at = Some(now);
                            }
                        }
                    }
                    "blocked" => {
                        goal.status = GoalStatus::Blocked;
                        goal.consecutive_blocked_turns += 1;
                        for step in &mut goal.plan {
                            if step.status == StepStatus::InProgress {
                                step.status = StepStatus::Blocked;
                                step.blocked_reason = Some(
                                    step.blocked_reason
                                        .clone()
                                        .unwrap_or_else(|| {
                                            "Blocked by the same condition across consecutive turns.".to_string()
                                        }),
                                );
                            }
                        }
                    }
                    other => {
                        return Err(format!(
                            "Invalid status: {other} (use \"complete\" or \"blocked\")"
                        ))
                    }
                }
                goal.updated_at = chrono::Local::now().timestamp_millis();
                let g = goal.clone();
                drop(s);
                emit_goal_update(app_handle, &g);
                Ok(format!("Goal status: {}", g.status.as_str()))
            }
            "update_plan" => {
                let steps = args["plan"].as_array().ok_or("Missing plan array")?;
                let explanation = args["explanation"].as_str().unwrap_or("");
                let now = chrono::Local::now().timestamp_millis();
                let mut s = state.lock().await;
                let goal = s
                    .goal
                    .as_mut()
                    .ok_or_else(|| "No active goal — call set_goal first.".to_string())?;
                let summary = apply_plan_update(goal, steps, now)?;
                let g = goal.clone();
                drop(s);
                emit_goal_update(app_handle, &g);
                let suffix = if explanation.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", explanation)
                };
                Ok(format!("{}{}", summary, suffix))
            }
            _ => Err("Unknown goal tool".to_string()),
        }
    }
    .await;

    match result {
        Ok(content) => {
            let summary: String = content.chars().take(80).collect();
            let _ = app_handle.emit(
                "agent-stream",
                AgentStreamEvent::ToolDone {
                    id: tool_call_id.to_string(),
                    name: name.to_string(),
                    summary,
                },
            );
            ContentBlock::ToolResult {
                tool_use_id: tool_call_id.to_string(),
                content,
            }
        }
        Err(e) => {
            let _ = app_handle.emit(
                "agent-stream",
                AgentStreamEvent::ToolError {
                    id: tool_call_id.to_string(),
                    name: name.to_string(),
                    error: e.clone(),
                },
            );
            ContentBlock::ToolResult {
                tool_use_id: tool_call_id.to_string(),
                content: format!("Error: {e}"),
            }
        }
    }
}

fn build_system_prompt(project_ctx: &crate::context::ProjectContext, time_harness: bool) -> String {
    let mut system = format!(
        "{}\n\n## Current Workspace\n\nPath: {}\n\n{}\n\n## Git Status\n\n{}\n\n## Project Memory\n\n{}\n\nContext usage: {} / 1,048,576 tokens",
        crate::context::SYSTEM_PROMPT,
        project_ctx.workspace_root,
        project_ctx.structure,
        project_ctx.git_status.as_deref().unwrap_or("Not a git repository"),
        project_ctx.recent_memories.join("\n\n"),
        project_ctx.total_estimated_tokens,
    );
    if time_harness {
        system.push_str(&crate::context::time_harness_system_section());
    }
    system
}

/// Apply Time Awareness Layer stamps at request time (L0 clock + L1 ages):
/// every message gets [message_time=... age=...], the last user message gets
/// the live [time_harness now=...], long conversations get a span note on the
/// first message, and an active goal gets its own stamped plan block at the
/// tail. Stored history is never modified — this only affects what the model
/// sees on the wire, and ages refresh every request.
fn stamp_messages_for_wire(
    messages: Vec<Message>,
    now_ms: i64,
    enabled: bool,
    goal: Option<&SessionGoal>,
    calibration: f64,
) -> Vec<Message> {
    if messages.is_empty() {
        return messages;
    }
    let oldest = messages.iter().filter(|m| m.timestamp > 0).map(|m| m.timestamp).min();
    let span_note = if enabled {
        oldest
            .map(|o| human_age_ms(now_ms - o))
            // Only flag conversations that are meaningfully old (≥ 1h).
            .filter(|a| a.ends_with('h') || a.ends_with('d'))
            .map(|a| {
                format!(
                    "[time_harness: this conversation spans from {} ago. Live data mentioned in \
                     older messages may be stale — re-verify per the freshness rules before \
                     relying on it.]\n",
                    a
                )
            })
            .unwrap_or_default()
    } else {
        String::new()
    };

    let last_idx = messages.len() - 1;
    let mut first = true;
    messages
        .into_iter()
        .enumerate()
        .map(|(i, mut m)| {
            let age = human_age_ms(now_ms - m.timestamp);
            let mtime = msg_time_str(m.timestamp);
            let mut stamp = String::new();
            if enabled && !age.is_empty() && !mtime.is_empty() {
                stamp = format!("[message_time={} age={}]", mtime, age);
            }
            if i == last_idx {
                if !stamp.is_empty() {
                    stamp.push(' ');
                }
                if enabled {
                    stamp.push_str(&format!("[time_harness now={}]", now_str()));
                }
                if let Some(g) = goal {
                    if enabled {
                        stamp.push('\n');
                    }
                    stamp.push_str(&goal_section_for_wire(g, now_ms, enabled, calibration));
                }
            }
            let mut prefix = if first { span_note.clone() } else { String::new() };
            first = false;
            if !stamp.is_empty() {
                prefix.push_str(&stamp);
                prefix.push('\n');
            }
            if !prefix.is_empty() {
                match &mut m.content {
                    api::Content::Text(t) => *t = format!("{prefix}{t}"),
                    api::Content::Blocks(blocks) => {
                        // DeepSeek's Anthropic-compatible endpoint pairs every
                        // assistant tool_use with a tool_result in the
                        // IMMEDIATELY following user message. Inserting a text
                        // stamp before the tool_result blocks makes the
                        // validator treat the result as missing → 400
                        // ("tool_use ids were found without tool_result blocks
                        // immediately after"). Tool results already carry
                        // their own [data_time=...] annotation, so skip
                        // message-level stamps for tool_result messages.
                        let is_tool_result_msg = blocks
                            .iter()
                            .any(|b| matches!(b, ContentBlock::ToolResult { .. }));
                        if !is_tool_result_msg {
                            blocks.insert(0, ContentBlock::Text { text: prefix.trim_end().to_string() });
                        }
                    }
                }
            }
            m
        })
        .collect()
}

fn estimate_message_tokens(msg: &Message) -> u64 {
    match &msg.content {
        api::Content::Text(t) => crate::context::estimate_tokens(t),
        api::Content::Blocks(blocks) => blocks
            .iter()
            .map(|b| match b {
                ContentBlock::Text { text } | ContentBlock::Thinking { thinking: text, .. } => {
                    crate::context::estimate_tokens(text)
                }
                _ => 0,
            })
            .sum(),
    }
}

/// ToolResult block for a tool that never ran (cancelled / internal failure).
/// Every tool_use MUST be answered — the API rejects unmatched ids.
fn tool_result_err(tool_use_id: &str, msg: &str) -> ContentBlock {
    ContentBlock::ToolResult {
        tool_use_id: tool_use_id.to_string(),
        content: msg.to_string(),
    }
}

/// Long tool outputs (huge shell logs, big file reads) spill to a file under
/// `.deepseek-code/tmp/` — the agent gets a tail + a path to read more on
/// demand. Cursor does the same ("long tool responses → files") to avoid
/// flooding the context window.
const LONG_OUTPUT_THRESHOLD: usize = 16_384;
async fn store_long_output_if_needed(name: &str, content: &str, id: &str, root: &PathBuf) -> String {
    if content.len() <= LONG_OUTPUT_THRESHOLD {
        return content.to_string();
    }
    let tmp_dir = root.join(".deepseek-code").join("tmp");
    if tokio::fs::create_dir_all(&tmp_dir).await.is_err() {
        return content.to_string();
    }
    let file_name = format!("tool-{}-{}.txt", name.replace(' ', "_"), id);
    let file_path = tmp_dir.join(&file_name);
    if tokio::fs::write(&file_path, content).await.is_err() {
        return content.to_string();
    }
    let rel = format!(".deepseek-code/tmp/{}", file_name);
    let lines: Vec<&str> = content.lines().collect();
    let tail_start = lines.len().saturating_sub(200);
    let tail = lines[tail_start..].join("\n");
    format!(
        "[Output too long — full result saved to {} ({:.1} KB). Read the file for the complete output.]\n\n{}",
        rel,
        content.len() as f64 / 1024.0,
        tail
    )
}

async fn tool_read_file_for_diff(path: &str, root: &PathBuf) -> Option<String> {
    let full = resolve_path(path, root).ok()?;
    tokio::fs::read_to_string(&full).await.ok()
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

fn summarize_tool_result(name: &str, result: &str) -> String {
    match name {
        "read_file" => format!("Read {} lines ({} chars)", result.lines().count(), result.len()),
        "write_file" | "edit_file" => result.lines().next().unwrap_or("File updated").to_string(),
        "run_shell" => {
            let preview: String = result.lines().take(5).collect::<Vec<_>>().join("\n");
            if result.lines().count() > 5 { format!("{}\n... ({} more lines)", preview, result.lines().count() - 5) } else { preview }
        }
        "search_code" => format!("Found {} matches", result.lines().count()),
        "list_directory" => format!("{} entries", result.lines().count()),
        "read_memory" | "write_memory" => result.lines().next().unwrap_or("Memory updated").to_string(),
        "web_search" => format!("Search completed ({} chars)", result.len()),
        "web_fetch" => format!("Fetched {} chars", result.len()),
        // char-safe truncation — byte slicing panics on multi-byte UTF-8 (CJK)
        _ => if result.len() > 200 {
            let t: String = result.chars().take(200).collect();
            format!("{}...", t)
        } else { result.to_string() },
    }
}

#[cfg(test)]
mod tests {
    use super::{
        goal_section_for_wire, human_age_ms, msg_time_str, stamp_messages_for_wire, ContentBlock,
        Message, Role, TokenUsageInfo, apply_plan_update, is_stalled_turn, should_stop_after_turn,
    };

    #[test]
    fn age_units_are_compact() {
        assert_eq!(human_age_ms(0), "");
        assert_eq!(human_age_ms(-5), "");
        assert_eq!(human_age_ms(30_000), "0min");
        assert_eq!(human_age_ms(10 * 60_000), "10min");
        assert_eq!(human_age_ms(90 * 60_000), "1h");
        assert_eq!(human_age_ms(26 * 3_600_000), "26h");
        assert_eq!(human_age_ms(50 * 24 * 3_600_000), "50d");
    }

    #[test]
    fn message_time_format_is_local_and_readable() {
        use chrono::TimeZone;
        let ts = chrono::Local
            .with_ymd_and_hms(2026, 8, 6, 9, 0, 0)
            .unwrap()
            .timestamp_millis();
        let s = msg_time_str(ts);
        assert!(s.starts_with("2026-08-06 09:00:00"), "got {s}");
        assert_eq!(msg_time_str(0), "");
    }

    #[test]
    fn wire_stamps_apply_ages_and_clock() {
        use crate::api::Content;
        use chrono::TimeZone;

        let now = chrono::Local
            .with_ymd_and_hms(2026, 8, 6, 9, 5, 0)
            .unwrap()
            .timestamp_millis();
        let two_days = 2 * 24 * 3_600_000i64;
        let msgs = vec![
            Message {
                role: Role::Assistant,
                content: Content::Text("build is fine".to_string()),
                timestamp: now - two_days + 60_000,
                internal: false,
            },
            Message {
                role: Role::User,
                content: Content::Text("check the build".to_string()),
                timestamp: now - two_days,
                internal: false,
            },
        ];

        let stamped = stamp_messages_for_wire(msgs.clone(), now, true, None, 3.71);
        match &stamped[0].content {
            Content::Text(t) => {
                assert!(t.contains("spans from 2d ago"), "span note missing: {t}");
                assert!(t.contains("[message_time="), "assistant age stamp missing: {t}");
            }
            _ => panic!("expected text"),
        }
        match &stamped[1].content {
            Content::Text(t) => {
                assert!(t.contains("age=2d"), "expected age=2d: {t}");
                assert!(t.contains("[time_harness now="), "clock stamp missing: {t}");
            }
            _ => panic!("expected text"),
        }

        // Disabled → raw messages pass through untouched.
        let raw = stamp_messages_for_wire(msgs, now, false, None, 3.71);
        match &raw[1].content {
            Content::Text(t) => assert_eq!(t, "check the build"),
            _ => panic!("expected text"),
        }
    }

    #[test]
    fn wire_stamps_preserve_tool_result_blocks() {
        use crate::api::Content;
        use chrono::TimeZone;

        let now = chrono::Local
            .with_ymd_and_hms(2026, 8, 6, 9, 5, 0)
            .unwrap()
            .timestamp_millis();
        let msgs = vec![Message {
            role: Role::User,
            content: Content::Blocks(vec![ContentBlock::ToolResult {
                tool_use_id: "tu_1".to_string(),
                content: "42".to_string(),
            }]),
            timestamp: now - 60_000,
            internal: false,
        }];

        let stamped = stamp_messages_for_wire(msgs, now, true, None, 3.71);
        match &stamped[0].content {
            Content::Blocks(b) => {
                // Tool-result messages must stay clean on the wire: DeepSeek
                // pairs tool_use ↔ tool_result by adjacency, and a leading
                // text stamp makes the validator report a missing tool_result
                // (400). No stamp block may be inserted here.
                assert!(
                    matches!(b[0], ContentBlock::ToolResult { .. }),
                    "tool result must stay first, got {:?}",
                    b[0]
                );
            }
            _ => panic!("expected blocks"),
        }
    }

    #[test]
    fn goal_section_stamps_ages_and_flags_stale_steps() {
        use crate::session::{GoalStatus, PlanStep, SessionGoal, StepStatus};
        use chrono::TimeZone;

        let now = chrono::Local
            .with_ymd_and_hms(2026, 8, 6, 9, 0, 0)
            .unwrap()
            .timestamp_millis();
        let two_days = 2 * 24 * 3_600_000i64;
        let goal = SessionGoal {
            id: "g1".to_string(),
            objective: "Ship the temporal benchmark paper".to_string(),
            status: GoalStatus::Active,
            token_budget: Some(20_000),
            tokens_used: 12_000,
            time_used_seconds: 3_600,
            created_at: now - two_days,
            updated_at: now,
            consecutive_blocked_turns: 1,
            plan: vec![
                PlanStep {
                    id: "s1".to_string(),
                    content: "Scale T1 to 30 cases".to_string(),
                    status: StepStatus::Pending,
                    created_at: now - two_days,
                    started_at: None,
                    completed_at: None,
                    blocked_reason: None,
                    estimate_sec: None,
                },
                PlanStep {
                    id: "s2".to_string(),
                    content: "Run cross-model sweep".to_string(),
                    status: StepStatus::InProgress,
                    created_at: now - 60_000,
                    started_at: Some(now - 60_000),
                    completed_at: None,
                    blocked_reason: None,
                    estimate_sec: Some(300),
                },
            ],
        };

        let section = goal_section_for_wire(&goal, now, true, 3.71);
        // Freshly set goal (created 2 days ago in this fixture) must NOT show
        // the "just set" takeover note...
        assert!(!section.contains("JUST SET"), "old goal flagged as just set: {section}");
        assert!(section.contains("status=active"), "missing status: {section}");
        assert!(section.contains("remaining=8000"), "budget math wrong: {section}");
        assert!(section.contains("age=2d"), "missing step age: {section}");
        assert!(section.contains("STALE"), "stale step not flagged: {section}");
        assert!(section.contains("calibrated 81s"), "calibrated ETA wrong: {section}");
        assert!(section.contains("(currently 1/3)"), "blocked audit counter wrong: {section}");
        assert!(section.contains("at most one step in_progress"), "rules missing: {section}");

        // Temporal off: goal still visible, timestamps stripped.
        let plain = goal_section_for_wire(&goal, now, false, 3.71);
        assert!(plain.contains("Objective:"), "objective missing without harness: {plain}");
        assert!(!plain.contains("age=2d"), "age leaked without harness: {plain}");

        // ...while a goal created seconds ago must show it (switch signal).
        let fresh_goal = SessionGoal {
            created_at: now - 5_000,
            ..goal.clone()
        };
        let fresh_section = goal_section_for_wire(&fresh_goal, now, true, 3.71);
        assert!(
            fresh_section.contains("JUST SET"),
            "fresh goal missing takeover note: {fresh_section}"
        );
    }

    #[test]
    fn auto_advance_stop_conditions() {
        use super::AgentTurnResult;
        let result = |msg: &str| AgentTurnResult {
            message: msg.to_string(),
            thinking_content: None,
            tool_calls: None,
            token_usage: TokenUsageInfo {
                input: 0, output: 0, cached: 0, cache_hit_rate: 0.0,
                cache_savings: 0.0, thinking_tokens: 0,
            },
            total_cost: 0.0,
            finish_reason: "end_turn".to_string(),
            context_usage: 0,
        };

        // Only a genuinely stalled turn (no content) is a hard stop.
        assert!(is_stalled_turn(&result("  ")));
        assert!(!is_stalled_turn(&result("progress")));
        // Question heuristic applies only to AUTO turns (burst > 0): a short
        // question stops after real work; the kickoff turn is never stopped.
        assert!(should_stop_after_turn(&result("需要确认一下，用哪个方案？")));
        assert!(should_stop_after_turn(&result("Which API key should I use?")));
        assert!(!should_stop_after_turn(&result("已修复编译错误，并补充了 3 个测试。接下来验证构建……")));
    }

    #[test]
    fn plan_update_preserves_identity_and_timestamps() {
        use crate::session::{GoalStatus, SessionGoal, StepStatus};
        use chrono::TimeZone;

        let t0 = chrono::Local
            .with_ymd_and_hms(2026, 8, 6, 9, 0, 0)
            .unwrap()
            .timestamp_millis();
        let t1 = t0 + 60_000;
        let t2 = t0 + 120_000;
        let mut goal = SessionGoal {
            id: "g".to_string(),
            objective: "x".to_string(),
            status: GoalStatus::Active,
            token_budget: None,
            tokens_used: 0,
            time_used_seconds: 0,
            created_at: t0,
            updated_at: t0,
            plan: vec![],
            consecutive_blocked_turns: 0,
        };

        // Create two steps.
        let create = serde_json::json!([
            {"content": "Write fib.py", "status": "pending"},
            {"content": "Run and verify", "status": "pending"},
        ]);
        apply_plan_update(&mut goal, create.as_array().unwrap(), t0).unwrap();
        assert_eq!(goal.plan.len(), 2);
        let id1 = goal.plan[0].id.clone();
        assert_eq!(goal.plan[0].created_at, t0);

        // Upsert by content: in_progress stamps started_at, same id.
        let progress = serde_json::json!([
            {"content": "Write fib.py", "status": "in_progress", "estimate_sec": 30},
            {"content": "Run and verify", "status": "pending"},
        ]);
        apply_plan_update(&mut goal, progress.as_array().unwrap(), t1).unwrap();
        assert_eq!(goal.plan[0].id, id1, "identity must survive content match");
        assert_eq!(goal.plan[0].started_at, Some(t1));
        assert_eq!(goal.plan[0].created_at, t0, "created_at is backend-owned");
        assert_eq!(goal.plan[0].estimate_sec, Some(30));

        // Complete: completed_at stamped once, created_at untouched.
        let done = serde_json::json!([
            {"content": "Write fib.py", "status": "completed"},
            {"content": "Run and verify", "status": "in_progress"},
        ]);
        apply_plan_update(&mut goal, done.as_array().unwrap(), t2).unwrap();
        assert_eq!(goal.plan[0].completed_at, Some(t2));
        assert_eq!(goal.plan[0].created_at, t0);
    }

    #[test]
    fn plan_update_enforces_one_in_progress_and_resets_blocked_audit() {
        use crate::session::{GoalStatus, SessionGoal, StepStatus};
        use chrono::TimeZone;

        let t0 = chrono::Local
            .with_ymd_and_hms(2026, 8, 6, 9, 0, 0)
            .unwrap()
            .timestamp_millis();
        let mut goal = SessionGoal {
            id: "g".to_string(),
            objective: "x".to_string(),
            status: GoalStatus::Blocked,
            token_budget: None,
            tokens_used: 0,
            time_used_seconds: 0,
            created_at: t0,
            updated_at: t0,
            plan: vec![],
            consecutive_blocked_turns: 3,
        };

        let two_ip = serde_json::json!([
            {"content": "a", "status": "in_progress"},
            {"content": "b", "status": "in_progress"},
        ]);
        assert!(apply_plan_update(&mut goal, two_ip.as_array().unwrap(), t0).is_err());

        let progress = serde_json::json!([
            {"content": "a", "status": "completed"},
            {"content": "b", "status": "in_progress"},
        ]);
        apply_plan_update(&mut goal, progress.as_array().unwrap(), t0).unwrap();
        assert_eq!(goal.consecutive_blocked_turns, 0, "progress resets blocked audit");
        assert_eq!(goal.status, GoalStatus::Active, "progress reactivates a blocked goal");
        assert_eq!(goal.plan[1].status, StepStatus::InProgress);
    }

    #[test]
    fn plan_update_accepts_step_alias_for_content() {
        use crate::session::{GoalStatus, SessionGoal, StepStatus};
        use chrono::TimeZone;

        let t0 = chrono::Local
            .with_ymd_and_hms(2026, 8, 6, 9, 0, 0)
            .unwrap()
            .timestamp_millis();
        let mut goal = SessionGoal {
            id: "g".to_string(),
            objective: "x".to_string(),
            status: GoalStatus::Active,
            token_budget: None,
            tokens_used: 0,
            time_used_seconds: 0,
            created_at: t0,
            updated_at: t0,
            plan: vec![],
            consecutive_blocked_turns: 0,
        };
        // Codex-trained models often emit "step" instead of "content".
        let steps = serde_json::json!([
            {"step": "Write tests", "status": "in_progress"}
        ]);
        apply_plan_update(&mut goal, steps.as_array().unwrap(), t0).unwrap();
        assert_eq!(goal.plan.len(), 1);
        assert_eq!(goal.plan[0].content, "Write tests");
        assert_eq!(goal.plan[0].status, StepStatus::InProgress);
    }
}
