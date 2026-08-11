//! DeepSeek V4 API client — Anthropic Messages format.
//!
//! Uses the Anthropic-compatible endpoint for native server-side tools:
//! - web_search — DeepSeek handles search, decryption, answer generation
//! - thinking — native reasoning blocks
//! - 1M context window
//! - Automatic prompt caching
//!
//! Model selection is per-client (Flash / Pro), and pricing follows the
//! selected model so cost estimates stay accurate.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc::UnboundedSender;
use tokio_stream::StreamExt;

const DEEPSEEK_ANTHROPIC_BASE: &str = "https://api.deepseek.com/anthropic";

/// Supported model IDs on DeepSeek's Anthropic-compatible endpoint.
pub const MODEL_FLASH: &str = "deepseek-v4-flash";
pub const MODEL_PRO: &str = "deepseek-v4-pro";

// ── Thinking mode ──

#[derive(Debug, Clone)]
pub enum ThinkingMode {
    NonThink,
    /// Per-mode reasoning budget (tokens) — DeepSeek bills thinking tokens,
    /// so the user controls how much the model "thinks" before answering.
    ThinkHigh { budget_tokens: u32 },
    ThinkMax { budget_tokens: u32 },
}

// ── Anthropic-format types ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text { text: String },
    ToolUse { id: String, name: String, input: serde_json::Value },
    ToolResult { tool_use_id: String, content: String },
    /// Search result block from the server-side web search. Per the Anthropic
    /// protocol, `content` is an ARRAY of text blocks (plus id/title/url).
    /// `tool_use_id` pairs it with the matching `server_tool_use` block and is
    /// REQUIRED when echoing the block back in the next request — DeepSeek
    /// rejects replay without it ("missing field tool_use_id").
    #[serde(rename = "web_search_tool_result")]
    WebSearchResult {
        #[serde(default)]
        tool_use_id: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        url: Option<String>,
        #[serde(default)]
        content: serde_json::Value,
    },
    /// Server-side tool use (web_search) — DeepSeek executes it and injects
    /// the results itself; the client only observes, no tool_result needed.
    #[serde(rename = "server_tool_use")]
    ServerToolUse { id: String, name: String, input: serde_json::Value },
    Thinking {
        thinking: String,
        /// DeepSeek signs thinking blocks and requires the signature when the
        /// block is replayed in a later request. Omitted when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    RedactedThinking { data: String },
    /// Catch-all: an unknown block type must never 400 the parse.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Content, // Can be string or vec of blocks
    /// Wall-clock ms epoch when this message entered the conversation. The
    /// Time Awareness Layer stamps ages from it at request time; kept off
    /// the wire and filled by the agent loop.
    #[serde(skip_serializing, default)]
    pub timestamp: i64,
    /// Goal-mode auto-continuation trigger: kept in live context so the model
    /// sees the turn, but excluded from session storage (the persisted goal
    /// carries the context) and never rendered as a user bubble.
    #[serde(skip_serializing, default)]
    pub internal: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Content {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolDefinition {
    /// Server tools carry an Anthropic tool type (e.g. "web_search_20250305")
    /// and no schema — DeepSeek runs them server-side.
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub tool_type: Option<String>,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_uses: Option<u32>,
}

impl ToolDefinition {
    pub fn client_tool(name: &str, description: &str, input_schema: serde_json::Value) -> Self {
        Self { tool_type: None, name: name.to_string(), description: Some(description.to_string()), input_schema: Some(input_schema), max_uses: None }
    }
}

// ── Request ──

#[derive(Debug, Clone, Serialize)]
struct MessagesRequest {
    model: String,
    system: String,
    messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ToolDefinition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<ThinkingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    max_tokens: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
struct ThinkingConfig {
    #[serde(rename = "type")]
    thinking_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    budget_tokens: Option<u32>,
}

// ── Response ──

#[derive(Debug, Clone, Deserialize)]
pub struct MessagesResponse {
    pub content: Vec<ContentBlock>,
    pub stop_reason: Option<String>,
    pub usage: Usage,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    #[serde(default)]
    pub cache_read_input_tokens: u32,
    #[serde(default)]
    pub cache_creation_input_tokens: u32,
}

impl Default for Usage {
    fn default() -> Self {
        Self { input_tokens: 0, output_tokens: 0, cache_read_input_tokens: 0, cache_creation_input_tokens: 0 }
    }
}

// ── Our internal response ──

/// Raw web_search_tool_result data — must round-trip through the protocol
/// so the MODEL sees the search results, not just the UI.
#[derive(Debug, Clone)]
pub struct WebSearchBlock {
    pub tool_use_id: Option<String>,
    pub title: Option<String>,
    pub url: Option<String>,
    pub content: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct ApiResponse {
    pub content: String,
    pub thinking_content: Option<String>,
    /// DeepSeek signs thinking blocks; the signature must be replayed with
    /// the block in the next request or the API rejects the history.
    pub thinking_signature: Option<String>,
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Server-side tool uses (web_search) — these still require a tool_result
    /// round-trip in the next user message, exactly like client tools.
    pub server_tool_uses: Vec<ToolCall>,
    /// Search result blocks — saved into the assistant message so the model
    /// sees the actual results on the next turn.
    pub web_search_results: Vec<WebSearchBlock>,
    pub web_search_used: bool,
    pub usage: Usage,
    pub finish_reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

// ── Streaming ──

/// Real-time events pushed to the agent while an SSE response is in flight.
/// The agent forwards these to the UI immediately — no waiting for the turn
/// to finish, unlike the batch path.
#[derive(Debug, Clone)]
pub enum ApiStreamEvent {
    TextDelta { text: String },
    ThinkingDelta { text: String },
}

/// Per-block accumulator for a streamed response. Content arrives as a
/// sequence of `content_block_start` → `content_block_delta`* →
/// `content_block_stop`, so we rebuild the final blocks from these states.
enum StreamBlockState {
    Text { text: String },
    Thinking { text: String, signature: Option<String> },
    ToolUse { id: String, name: String, json_buf: String },
    ServerToolUse { id: String, name: String, json_buf: String },
    WebSearchResult {
        tool_use_id: Option<String>,
        title: Option<String>,
        url: Option<String>,
        content: serde_json::Value,
    },
}

impl StreamBlockState {
    fn into_content_block(self) -> Option<ContentBlock> {
        match self {
            StreamBlockState::Text { text } => Some(ContentBlock::Text { text }),
            StreamBlockState::Thinking { text, signature } => Some(ContentBlock::Thinking { thinking: text, signature }),
            StreamBlockState::ToolUse { id, name, json_buf } => Some(ContentBlock::ToolUse {
                id: sanitize_tool_use_id(&id),
                name,
                input: parse_partial_json(&json_buf),
            }),
            StreamBlockState::ServerToolUse { id, name, json_buf } => Some(ContentBlock::ServerToolUse {
                id: sanitize_tool_use_id(&id),
                name,
                input: parse_partial_json(&json_buf),
            }),
            StreamBlockState::WebSearchResult { tool_use_id, title, url, content } => Some(ContentBlock::WebSearchResult {
                tool_use_id: tool_use_id.unwrap_or_default(),
                title,
                url,
                content: normalize_search_content(content),
            }),
        }
    }
}

/// Tracks the state of one SSE stream until `message_stop`.
#[derive(Default)]
struct StreamState {
    data_lines: Vec<String>,
    /// Streamed blocks keyed by the protocol `index`; converted into
    /// `ContentBlock`s in `blocks` as each one stops.
    active: Vec<(usize, StreamBlockState)>,
    blocks: Vec<ContentBlock>,
    usage: Usage,
    stop_reason: String,
    error: Option<String>,
}

impl StreamState {
    fn flush_event(&mut self, tx: &UnboundedSender<ApiStreamEvent>) -> Result<(), String> {
        if self.data_lines.is_empty() {
            return Ok(());
        }
        let data = self.data_lines.join("\n");
        self.data_lines.clear();
        self.handle_event(&data, tx)
    }

    fn handle_event(&mut self, data: &str, tx: &UnboundedSender<ApiStreamEvent>) -> Result<(), String> {
        let value: serde_json::Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => return Ok(()), // ping / keep-alive noise
        };
        let event_type = value.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match event_type {
            "message_start" => {
                if let Some(u) = value.pointer("/message/usage") {
                    self.usage.input_tokens = u.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
                    self.usage.cache_read_input_tokens = u.get("cache_read_input_tokens").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
                    self.usage.cache_creation_input_tokens = u.get("cache_creation_input_tokens").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
                }
            }
            "content_block_start" => {
                let index = value.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                let cb = value.get("content_block").cloned().unwrap_or(serde_json::Value::Null);
                let block_type = cb.get("type").and_then(|t| t.as_str()).unwrap_or("");
                let state = match block_type {
                    "text" => StreamBlockState::Text { text: String::new() },
                    "thinking" => StreamBlockState::Thinking { text: String::new(), signature: None },
                    "tool_use" => StreamBlockState::ToolUse {
                        id: cb.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                        name: cb.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                        json_buf: String::new(),
                    },
                    "server_tool_use" => StreamBlockState::ServerToolUse {
                        id: cb.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                        name: cb.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                        json_buf: String::new(),
                    },
                    "web_search_tool_result" => StreamBlockState::WebSearchResult {
                        tool_use_id: cb.get("tool_use_id").and_then(|x| x.as_str()).map(String::from),
                        title: cb.get("title").and_then(|x| x.as_str()).map(String::from),
                        url: cb.get("url").and_then(|x| x.as_str()).map(String::from),
                        content: cb.get("content").cloned().unwrap_or(serde_json::Value::Null),
                    },
                    _ => return Ok(()), // redacted_thinking & co — display-only
                };
                self.active.push((index, state));
            }
            "content_block_delta" => {
                let index = value.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                let delta = value.get("delta").cloned().unwrap_or(serde_json::Value::Null);
                let delta_type = delta.get("type").and_then(|t| t.as_str()).unwrap_or("");
                if let Some((_, state)) = self.active.iter_mut().find(|(i, _)| *i == index) {
                    match (state, delta_type) {
                        (StreamBlockState::Text { text }, "text_delta") => {
                            if let Some(chunk) = delta.get("text").and_then(|x| x.as_str()) {
                                text.push_str(chunk);
                                let _ = tx.send(ApiStreamEvent::TextDelta { text: chunk.to_string() });
                            }
                        }
                        (StreamBlockState::Thinking { text, .. }, "thinking_delta") => {
                            if let Some(chunk) = delta.get("thinking").and_then(|x| x.as_str()) {
                                text.push_str(chunk);
                                let _ = tx.send(ApiStreamEvent::ThinkingDelta { text: chunk.to_string() });
                            }
                        }
                        (StreamBlockState::Thinking { signature, .. }, "signature_delta") => {
                            if let Some(sig) = delta.get("signature").and_then(|x| x.as_str()) {
                                *signature = Some(sig.to_string());
                            }
                        }
                        (StreamBlockState::ToolUse { json_buf, .. } | StreamBlockState::ServerToolUse { json_buf, .. }, "input_json_delta") => {
                            if let Some(partial) = delta.get("partial_json").and_then(|x| x.as_str()) {
                                json_buf.push_str(partial);
                            }
                        }
                        (StreamBlockState::WebSearchResult { content, .. }, "text_delta") => {
                            if let Some(chunk) = delta.get("text").and_then(|x| x.as_str()) {
                                append_web_search_text(content, chunk);
                            }
                        }
                        _ => {}
                    }
                }
            }
            "content_block_stop" => {
                let index = value.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                if let Some(pos) = self.active.iter().position(|(i, _)| *i == index) {
                    let (_, block_state) = self.active.remove(pos);
                    if let Some(block) = block_state.into_content_block() {
                        self.blocks.push(block);
                    }
                }
            }
            "message_delta" => {
                if let Some(stop) = value.pointer("/delta/stop_reason").and_then(|s| s.as_str()) {
                    self.stop_reason = stop.to_string();
                }
                if let Some(out) = value.pointer("/usage/output_tokens").and_then(|o| o.as_u64()) {
                    // Anthropic protocol: cumulative across the message.
                    self.usage.output_tokens = out as u32;
                }
            }
            "error" => {
                let msg = value
                    .get("error").and_then(|e| e.get("message")).and_then(|m| m.as_str())
                    .unwrap_or("Unknown stream error");
                self.error = Some(msg.to_string());
            }
            _ => {} // ping, message_stop, ...
        }
        Ok(())
    }
}

fn parse_partial_json(s: &str) -> serde_json::Value {
    // Empty/malformed input must still serialize as an OBJECT — Anthropic's
    // schema rejects `null`, and DeepSeek occasionally emits parameterless
    // tool_use blocks (charmbracelet/fantasy#219 hits the same quirk).
    serde_json::from_str(s).unwrap_or_else(|_| serde_json::json!({}))
}

/// DeepSeek occasionally emits tool_use blocks with an empty id — the wire
/// round-trip then breaks because the tool_result can't be paired. Assign a
/// stable synthetic id so both sides stay consistent.
fn sanitize_tool_use_id(id: &str) -> String {
    if id.trim().is_empty() {
        format!("toolu_{}", uuid::Uuid::new_v4().simple())
    } else {
        id.to_string()
    }
}

/// The server may send `content: null` (or a plain string); the request-side
/// schema expects an array of text blocks.
fn normalize_search_content(content: serde_json::Value) -> serde_json::Value {
    match content {
        serde_json::Value::Null => serde_json::json!([]),
        other => other,
    }
}

/// Some servers stream the web_search_tool_result text as nested deltas;
/// append them into the block's content (array of text blocks or plain string).
fn append_web_search_text(content: &mut serde_json::Value, chunk: &str) {
    match content {
        serde_json::Value::Array(arr) => match arr.last_mut() {
            Some(serde_json::Value::String(s)) => s.push_str(chunk),
            Some(serde_json::Value::Object(obj)) => {
                if let Some(serde_json::Value::String(s)) = obj.get_mut("text") {
                    s.push_str(chunk);
                } else {
                    arr.push(serde_json::json!({"type": "text", "text": chunk}));
                }
            }
            _ => arr.push(serde_json::json!({"type": "text", "text": chunk})),
        },
        serde_json::Value::String(s) => s.push_str(chunk),
        _ => *content = serde_json::Value::String(chunk.to_string()),
    }
}

// ── Client ──

#[derive(Clone)]
pub struct DeepSeekClient {
    api_key: String,
    client: reqwest::Client,
}

impl DeepSeekClient {
    pub fn new(api_key: String) -> Self {
        Self { api_key, client: reqwest::Client::new() }
    }

    /// Build the Anthropic-format request shared by batch and streaming paths.
    fn build_request(
        &self,
        system: &str,
        messages: Vec<Message>,
        tools: Option<Vec<serde_json::Value>>,
        thinking_mode: ThinkingMode,
        model: &str,
        stream: bool,
    ) -> MessagesRequest {
        // Self-heal in-memory history: blocks created before tool_use_id was
        // captured (old sessions) would 400 on the wire. Drop orphaned
        // web_search_tool_result blocks and server_tool_use blocks that have
        // no paired result — the search text already lives in the visible
        // assistant content, so nothing is lost.
        let messages = Self::sanitize_messages_for_wire(messages);

        // Convert OpenAI-style tool defs to Anthropic format
        let mut anthropic_tools: Vec<ToolDefinition> = tools.map(|ts| {
            ts.into_iter().filter_map(|t| {
                let obj = t.as_object()?;
                let func = obj.get("function")?.as_object()?;
                Some(ToolDefinition::client_tool(
                    func.get("name")?.as_str()?,
                    func.get("description")?.as_str()?,
                    func.get("parameters")?.clone(),
                ))
            }).collect()
        }).unwrap_or_default();

        // DeepSeek's server-side web search — an Anthropic server tool, NOT a
        // client tool. The model never "calls" it: the server runs the search,
        // streams a server_tool_use block, and injects the results as a
        // web_search_tool_result block in the same response.
        // web_search_20260209 is DeepSeek's documented native tool type on its
        // Anthropic-compatible endpoint (the older 20250305 also works but is
        // Anthropic's own version, not DeepSeek's).
        anthropic_tools.push(ToolDefinition {
            tool_type: Some("web_search_20260209".to_string()),
            name: "web_search".to_string(),
            description: None,
            input_schema: None,
            max_uses: Some(8),
        });

        // Thinking config
        let thinking = match thinking_mode {
            ThinkingMode::NonThink => None,
            ThinkingMode::ThinkHigh { budget_tokens } => Some(ThinkingConfig {
                thinking_type: "enabled".to_string(),
                budget_tokens: Some(budget_tokens),
            }),
            ThinkingMode::ThinkMax { budget_tokens } => Some(ThinkingConfig {
                thinking_type: "enabled".to_string(),
                budget_tokens: Some(budget_tokens),
            }),
        };

        // Output limit
        let max_tokens = match thinking_mode {
            ThinkingMode::NonThink => 8_192u32,
            ThinkingMode::ThinkHigh { .. } => 65_536u32,
            ThinkingMode::ThinkMax { .. } => 196_608u32,
        };

        MessagesRequest {
            model: model.to_string(),
            system: system.to_string(),
            messages,
            tools: Some(anthropic_tools),
            thinking,
            stream: if stream { Some(true) } else { None },
            max_tokens,
        }
    }

    /// Filter blocks that would fail DeepSeek's request-side deserializer.
    ///
    /// Beyond the web_search cleanup, this enforces the protocol invariant
    /// that every assistant `tool_use` is answered by a `tool_result` in the
    /// immediately following user message. History from old sessions,
    /// compaction boundaries, or cancelled turns can contain orphans —
    /// dropping the orphan side keeps the wire valid instead of 400ing.
    fn sanitize_messages_for_wire(messages: Vec<Message>) -> Vec<Message> {
        let mut messages: Vec<Message> = messages
            .into_iter()
            .map(|mut m| {
                if let Content::Blocks(blocks) = &mut m.content {
                    let paired: std::collections::HashSet<String> = blocks
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::WebSearchResult { tool_use_id, .. } if !tool_use_id.is_empty() => {
                                Some(tool_use_id.clone())
                            }
                            _ => None,
                        })
                        .collect();
                    blocks.retain(|b| match b {
                        ContentBlock::WebSearchResult { tool_use_id, .. } => !tool_use_id.is_empty(),
                        ContentBlock::ServerToolUse { id, .. } => paired.contains(id),
                        _ => true,
                    });
                }
                m
            })
            .collect();

        // Collect tool_use ids per message BEFORE any mutation, so both
        // repair passes reason about the same original pairing.
        let tool_use_ids: Vec<std::collections::HashSet<String>> = messages
            .iter()
            .map(|m| match &m.content {
                Content::Blocks(blocks) => blocks
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::ToolUse { id, .. } => Some(id.clone()),
                        _ => None,
                    })
                    .collect(),
                _ => std::collections::HashSet::new(),
            })
            .collect();

        // Pass 1 — orphan tool_use: an assistant tool_use whose immediately
        // following message has no matching tool_result must not reach the
        // wire. Drop the block; keep the message non-empty for protocol
        // safety.
        for i in 0..messages.len() {
            if messages[i].role != Role::Assistant {
                continue;
            }
            let expected = &tool_use_ids[i];
            if expected.is_empty() {
                continue;
            }
            let answered = messages
                .get(i + 1)
                .map(|next| match &next.content {
                    Content::Blocks(blocks) => blocks.iter().any(|b| match b {
                        ContentBlock::ToolResult { tool_use_id, .. } => expected.contains(tool_use_id),
                        _ => false,
                    }),
                    _ => false,
                })
                .unwrap_or(false);
            if answered {
                continue;
            }
            if let Content::Blocks(blocks) = &mut messages[i].content {
                blocks.retain(|b| !matches!(b, ContentBlock::ToolUse { .. }));
                if blocks.is_empty() {
                    blocks.push(ContentBlock::Text {
                        text: "[tool call omitted: result unavailable in history]".to_string(),
                    });
                }
            }
        }

        // Pass 2 — orphan tool_result: a user tool_result with no preceding
        // assistant tool_use is equally invalid. Drop it, and keep the user
        // message non-empty.
        for i in 0..messages.len() {
            if messages[i].role != Role::User {
                continue;
            }
            let expected = if i > 0 { &tool_use_ids[i - 1] } else { &std::collections::HashSet::new() };
            if let Content::Blocks(blocks) = &mut messages[i].content {
                blocks.retain(|b| match b {
                    ContentBlock::ToolResult { tool_use_id, .. } => expected.contains(tool_use_id),
                    _ => true,
                });
                if blocks.is_empty() {
                    blocks.push(ContentBlock::Text {
                        text: "[tool result omitted: no matching tool use in history]".to_string(),
                    });
                }
            }
        }

        messages
    }

    /// POST with retry-on-transient-failure, cancellation-aware send. Both the
    /// batch and streaming paths share this; the streaming path then consumes
    /// the response body as an SSE stream.
    async fn send_with_retry(
        &self,
        request: &MessagesRequest,
        cancelled: &Option<Arc<AtomicBool>>,
    ) -> Result<reqwest::Response, String> {
        let url = format!("{}/v1/messages", DEEPSEEK_ANTHROPIC_BASE);
        // Retry transient failures (429 rate limits, 5xx, network blips) with
        // exponential backoff — Cursor-classifies these as "expected" errors.
        // The whole send is CANCELLATION-AWARE: ESC must interrupt a hung
        // request within ~150ms, and a 300s hard timeout bounds even a
        // server-side web search that never completes.
        const MAX_RETRIES: u32 = 3;
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let sent = self
                .client
                .post(&url)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2023-06-01")
                .header("Content-Type", "application/json")
                .json(request)
                .timeout(std::time::Duration::from_secs(300))
                .send();

            tokio::pin!(sent);
            let mut ticker = tokio::time::interval(std::time::Duration::from_millis(150));
            let outcome = loop {
                tokio::select! {
                    res = &mut sent => break res,
                    _ = ticker.tick() => {
                        if is_cancelled(cancelled) {
                            return Err("Cancelled by user.".to_string());
                        }
                    }
                }
            };

            match outcome {
                Ok(resp) if resp.status().is_success() => return Ok(resp),
                Ok(resp) => {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    let transient = status.as_u16() == 429 || status.is_server_error();
                    if !transient || attempt >= MAX_RETRIES {
                        return Err(format!("API error {}: {}", status, body));
                    }
                    cancel_aware_sleep(cancelled, 500u64 * 2u64.pow(attempt - 1)).await?;
                }
                Err(e) => {
                    if attempt >= MAX_RETRIES {
                        return Err(format!("API request failed: {}", e));
                    }
                    cancel_aware_sleep(cancelled, 500u64 * 2u64.pow(attempt - 1)).await?;
                }
            }
        }
    }

    /// Send a message using DeepSeek's Anthropic-compatible endpoint.
    ///
    /// This endpoint natively supports:
    /// - Server-side web_search (no client-side implementation needed)
    /// - thinking blocks for preserved reasoning
    /// - Prompt caching (automatic)
    /// - Tool use with proper content block format
    pub async fn chat(
        &self,
        system: &str,
        messages: Vec<Message>,
        tools: Option<Vec<serde_json::Value>>,
        thinking_mode: ThinkingMode,
        model: &str,
        cancelled: Option<Arc<AtomicBool>>,
    ) -> Result<ApiResponse, String> {
        // Used for non-interactive paths (context compaction) — the agent loop
        // streams via `chat_stream` instead.
        let request = self.build_request(system, messages, tools, thinking_mode, model, false);
        let response = self.send_with_retry(&request, &cancelled).await?;
        let resp: MessagesResponse = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;
        Ok(api_response_from_blocks(
            resp.content,
            resp.usage,
            resp.stop_reason.unwrap_or_else(|| "end_turn".to_string()),
        ))
    }

    /// SSE streaming request — the agent loop's main path.
    ///
    /// Same protocol, `stream: true`; every text/thinking delta is forwarded
    /// to `event_tx` the moment it arrives so the UI renders it live. Returns
    /// the fully accumulated response when the stream completes.
    pub async fn chat_stream(
        &self,
        system: &str,
        messages: Vec<Message>,
        tools: Option<Vec<serde_json::Value>>,
        thinking_mode: ThinkingMode,
        model: &str,
        cancelled: Option<Arc<AtomicBool>>,
        event_tx: UnboundedSender<ApiStreamEvent>,
    ) -> Result<ApiResponse, String> {
        let request = self.build_request(system, messages, tools, thinking_mode, model, true);
        let response = self.send_with_retry(&request, &cancelled).await?;

        let mut state = StreamState::default();
        let mut buf: Vec<u8> = Vec::new();
        let mut stream = response.bytes_stream();
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(150));

        loop {
            tokio::select! {
                chunk = stream.next() => {
                    match chunk {
                        Some(Ok(bytes)) => {
                            buf.extend_from_slice(&bytes);
                            // Flush complete lines; a blank line ends an event.
                            while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                                let line_bytes: Vec<u8> = buf.drain(..=pos).collect();
                                let line = String::from_utf8_lossy(&line_bytes[..line_bytes.len() - 1]);
                                let line = line.trim_end_matches('\r');
                                if line.is_empty() {
                                    state.flush_event(&event_tx)?;
                                } else if let Some(data) = line.strip_prefix("data:") {
                                    state.data_lines.push(data.trim().to_string());
                                }
                                // `event:` lines duplicate the payload `type`
                                // field — we dispatch on the JSON itself.
                            }
                        }
                        Some(Err(e)) => return Err(format!("Stream read failed: {}", e)),
                        None => break,
                    }
                }
                _ = ticker.tick() => {
                    if is_cancelled(&cancelled) {
                        return Err("Cancelled by user.".to_string());
                    }
                }
            }
        }
        state.flush_event(&event_tx)?;
        if let Some(err) = state.error.take() {
            return Err(err);
        }

        let blocks = std::mem::take(&mut state.blocks);
        let usage = std::mem::take(&mut state.usage);
        let stop_reason = if state.stop_reason.is_empty() {
            "end_turn".to_string()
        } else {
            state.stop_reason.clone()
        };
        Ok(api_response_from_blocks(blocks, usage, stop_reason))
    }
}

/// Shared extraction: content blocks → the ApiResponse the agent loop consumes.
fn api_response_from_blocks(blocks: Vec<ContentBlock>, usage: Usage, finish_reason: String) -> ApiResponse {
    let mut text = String::new();
    let mut thinking_text: Option<String> = None;
    let mut thinking_signature: Option<String> = None;
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut server_tool_uses: Vec<ToolCall> = Vec::new();
    let mut web_search_results: Vec<WebSearchBlock> = Vec::new();
    let mut web_search_used = false;

    for block in &blocks {
        match block {
            ContentBlock::Text { text: t } => text.push_str(t),
            ContentBlock::Thinking { thinking: t, signature } => {
                thinking_text = Some(t.clone());
                thinking_signature = signature.clone();
            }
            ContentBlock::ToolUse { id, name, input } => {
                let args = serde_json::to_string(input).unwrap_or_default();
                if name == "web_search" { web_search_used = true; }
                tool_calls.push(ToolCall {
                    id: sanitize_tool_use_id(id),
                    call_type: "tool_use".to_string(),
                    function: FunctionCall { name: name.clone(), arguments: args },
                });
            }
            ContentBlock::WebSearchResult { tool_use_id, title, url, content } => {
                web_search_used = true;
                web_search_results.push(WebSearchBlock {
                    tool_use_id: if tool_use_id.is_empty() { None } else { Some(tool_use_id.clone()) },
                    title: title.clone(),
                    url: url.clone(),
                    content: content.clone(),
                });
                // Surface the results as visible text in the bubble too.
                let body = extract_search_text(content);
                let mut part = String::from("\n\n### 搜索结果\n");
                if let (Some(t), Some(u)) = (title, url) {
                    part.push_str(&format!("**[{}]({})**\n\n", t, u));
                }
                part.push_str(&body);
                text.push_str(&part);
            }
            // Server-side tool use — kept for the web_search activity display.
            ContentBlock::ServerToolUse { id, name, input } => {
                web_search_used = true;
                server_tool_uses.push(ToolCall {
                    id: sanitize_tool_use_id(id),
                    call_type: "server_tool_use".to_string(),
                    function: FunctionCall {
                        name: name.clone(),
                        arguments: serde_json::to_string(input).unwrap_or_default(),
                    },
                });
            }
            _ => {}
        }
    }

    ApiResponse {
        content: text,
        thinking_content: thinking_text,
        thinking_signature,
        tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
        server_tool_uses,
        web_search_results,
        web_search_used,
        usage,
        finish_reason,
    }
}

fn is_cancelled(cancelled: &Option<Arc<AtomicBool>>) -> bool {
    cancelled.as_ref().map(|c| c.load(Ordering::SeqCst)).unwrap_or(false)
}

/// Sleep that aborts early when the user cancels — keeps ESC responsive even
/// between retries.
async fn cancel_aware_sleep(
    cancelled: &Option<Arc<AtomicBool>>,
    millis: u64,
) -> Result<(), String> {
    let wait = tokio::time::sleep(std::time::Duration::from_millis(millis));
    tokio::pin!(wait);
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(150));
    loop {
        tokio::select! {
            _ = &mut wait => return Ok(()),
            _ = ticker.tick() => {
                if is_cancelled(cancelled) {
                    return Err("Cancelled by user.".to_string());
                }
            }
        }
    }
}

/// Extract plain text from a web_search_tool_result `content` value.
/// The protocol sends an array of `{type: "text", text: "..."}` blocks;
/// tolerate a plain string or anything else without failing the turn.
fn extract_search_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|it| it.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_events(events: &[&str]) -> (StreamState, Vec<ApiStreamEvent>) {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ApiStreamEvent>();
        let mut state = StreamState::default();
        for e in events {
            state.handle_event(e, &tx).unwrap();
        }
        drop(tx);
        let deltas: Vec<ApiStreamEvent> = std::iter::from_fn(|| rx.blocking_recv()).collect();
        (state, deltas)
    }

    #[test]
    fn accumulates_text_thinking_and_usage_from_sse_events() {
        let events = [
            r#"{"type":"message_start","message":{"usage":{"input_tokens":10,"cache_read_input_tokens":5,"cache_creation_input_tokens":2}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Let me think"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig_abc123"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Hello"}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":" world"}}"#,
            r#"{"type":"content_block_stop","index":1}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":42}}"#,
            r#"{"type":"message_stop"}"#,
        ];
        let (state, deltas) = run_events(&events);

        let resp = api_response_from_blocks(state.blocks, state.usage, state.stop_reason);
        assert_eq!(resp.content, "Hello world");
        assert_eq!(resp.thinking_content.as_deref(), Some("Let me think"));
        assert_eq!(resp.thinking_signature.as_deref(), Some("sig_abc123"));
        assert_eq!(resp.usage.input_tokens, 10);
        assert_eq!(resp.usage.cache_read_input_tokens, 5);
        assert_eq!(resp.usage.output_tokens, 42);
        assert_eq!(resp.finish_reason, "end_turn");
        // Live deltas must arrive in order for the UI.
        assert!(matches!(deltas[0], ApiStreamEvent::ThinkingDelta { ref text } if text == "Let me think"));
        assert!(matches!(deltas[1], ApiStreamEvent::TextDelta { ref text } if text == "Hello"));
        assert!(matches!(deltas[2], ApiStreamEvent::TextDelta { ref text } if text == " world"));
    }

    #[test]
    fn accumulates_tool_use_partial_json() {
        let events = [
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tu_1","name":"read_file","input":{}}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"src"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"/main.rs\"}"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_stop"}"#,
        ];
        let (state, _) = run_events(&events);
        assert_eq!(state.blocks.len(), 1);
        match &state.blocks[0] {
            ContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "tu_1");
                assert_eq!(name, "read_file");
                assert_eq!(input["path"], "src/main.rs");
            }
            other => panic!("expected tool_use block, got {:?}", other),
        }
    }

    #[test]
    fn captures_server_side_web_search_and_stream_errors() {
        let events = [
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"server_tool_use","id":"st_1","name":"web_search","input":{}}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"query\":\"deepseek\"}"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"web_search_tool_result","tool_use_id":"toolu_ws1","title":"Result","url":"https://example.com","content":[{"type":"text","text":"DeepSeek rocks"}]}}"#,
            r#"{"type":"content_block_stop","index":1}"#,
            r#"{"type":"message_stop"}"#,
        ];
        let (state, _) = run_events(&events);
        let resp = api_response_from_blocks(state.blocks, state.usage, state.stop_reason);
        assert!(resp.web_search_used);
        assert_eq!(resp.server_tool_uses.len(), 1);
        assert_eq!(resp.server_tool_uses[0].function.name, "web_search");
        assert_eq!(resp.web_search_results.len(), 1);
        assert_eq!(resp.web_search_results[0].tool_use_id.as_deref(), Some("toolu_ws1"));
        assert_eq!(resp.web_search_results[0].title.as_deref(), Some("Result"));
        assert!(resp.content.contains("DeepSeek rocks"));
    }

    #[test]
    fn echoes_web_search_result_with_tool_use_id() {
        // The wire format requires tool_use_id on replay — this guards the
        // exact 400 the user hit ("missing field tool_use_id").
        let block = ContentBlock::WebSearchResult {
            tool_use_id: "toolu_ws1".to_string(),
            title: Some("Result".to_string()),
            url: Some("https://example.com".to_string()),
            content: serde_json::json!([{"type": "text", "text": "DeepSeek rocks"}]),
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "web_search_tool_result");
        assert_eq!(json["tool_use_id"], "toolu_ws1");
        assert!(json.get("tool_use_id").is_some());
    }

    #[test]
    fn empty_tool_use_input_serializes_as_object() {
        let block = ContentBlock::ToolUse {
            id: "toolu_1".to_string(),
            name: "read_file".to_string(),
            input: parse_partial_json(""),
        };
        let json = serde_json::to_value(&block).unwrap();
        assert!(json["input"].is_object());
        assert!(json["input"].as_object().unwrap().is_empty());
    }

    #[test]
    fn sanitizes_orphaned_web_search_blocks_from_old_history() {
        let msg = Message {
            role: Role::Assistant,
            content: Content::Blocks(vec![
                ContentBlock::Text { text: "Searching...".to_string() },
                // Pre-fix block: no tool_use_id → must be dropped on the wire.
                ContentBlock::WebSearchResult {
                    tool_use_id: String::new(),
                    title: None,
                    url: None,
                    content: serde_json::json!([]),
                },
                // New block with a pair → both survive.
                ContentBlock::ServerToolUse {
                    id: "toolu_ws9".to_string(),
                    name: "web_search".to_string(),
                    input: serde_json::json!({"query": "x"}),
                },
                ContentBlock::WebSearchResult {
                    tool_use_id: "toolu_ws9".to_string(),
                    title: Some("T".to_string()),
                    url: None,
                    content: serde_json::json!([{"type": "text", "text": "result"}]),
                },
            ]),
            timestamp: 0,
            internal: false,
        };
        let cleaned = DeepSeekClient::sanitize_messages_for_wire(vec![msg]);
        let Content::Blocks(blocks) = &cleaned[0].content else {
            panic!("expected blocks");
        };
        assert_eq!(blocks.len(), 3);
        assert!(matches!(blocks[0], ContentBlock::Text { .. }));
        assert!(matches!(blocks[1], ContentBlock::ServerToolUse { .. }));
        assert!(matches!(blocks[2], ContentBlock::WebSearchResult { ref tool_use_id, .. } if tool_use_id == "toolu_ws9"));
    }

    #[test]
    fn keeps_paired_client_tool_use_and_result() {
        let messages = vec![
            Message {
                role: Role::Assistant,
                content: Content::Blocks(vec![ContentBlock::ToolUse {
                    id: "call_00_paired".to_string(),
                    name: "read_file".to_string(),
                    input: serde_json::json!({}),
                }]),
                timestamp: 0,
                internal: false,
            },
            Message {
                role: Role::User,
                content: Content::Blocks(vec![ContentBlock::ToolResult {
                    tool_use_id: "call_00_paired".to_string(),
                    content: "ok".to_string(),
                }]),
                timestamp: 0,
                internal: false,
            },
        ];
        let cleaned = DeepSeekClient::sanitize_messages_for_wire(messages);
        let Content::Blocks(b0) = &cleaned[0].content else {
            panic!("expected blocks");
        };
        assert!(b0.iter().any(|b| matches!(b, ContentBlock::ToolUse { id, .. } if id == "call_00_paired")));
        let Content::Blocks(b1) = &cleaned[1].content else {
            panic!("expected blocks");
        };
        assert!(b1.iter().any(|b| matches!(b, ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "call_00_paired")));
    }

    #[test]
    fn sanitizes_orphaned_client_tool_use_and_result() {
        let messages = vec![
            Message {
                role: Role::Assistant,
                content: Content::Blocks(vec![
                    ContentBlock::ToolUse {
                        id: "call_00_orphan".to_string(),
                        name: "read_file".to_string(),
                        input: serde_json::json!({}),
                    },
                    ContentBlock::Text { text: "reading...".to_string() },
                ]),
                timestamp: 0,
                internal: false,
            },
            // Next message is plain text — no tool_result for call_00_orphan.
            Message {
                role: Role::User,
                content: Content::Text("no result".to_string()),
                timestamp: 0,
                internal: false,
            },
            // A tool_result with no preceding tool_use must be dropped too.
            Message {
                role: Role::User,
                content: Content::Blocks(vec![ContentBlock::ToolResult {
                    tool_use_id: "call_00_missing_use".to_string(),
                    content: "orphan result".to_string(),
                }]),
                timestamp: 0,
                internal: false,
            },
        ];
        let cleaned = DeepSeekClient::sanitize_messages_for_wire(messages);
        let Content::Blocks(b0) = &cleaned[0].content else {
            panic!("expected blocks");
        };
        assert!(
            !b0.iter().any(|b| matches!(b, ContentBlock::ToolUse { .. })),
            "orphan tool_use must be dropped"
        );
        assert!(
            b0.iter().any(|b| matches!(b, ContentBlock::Text { .. })),
            "message must stay non-empty"
        );
        let Content::Blocks(b2) = &cleaned[2].content else {
            panic!("expected blocks");
        };
        assert!(
            !b2.iter().any(|b| matches!(b, ContentBlock::ToolResult { .. })),
            "orphan tool_result must be dropped"
        );
        assert!(
            b2.iter().any(|b| matches!(b, ContentBlock::Text { .. })),
            "message must stay non-empty"
        );
    }

    #[test]
    fn cache_savings_reflects_cache_hit_rate_differential() {
        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 0,
            cache_read_input_tokens: 1_000_000,
            cache_creation_input_tokens: 0,
        };
        // Flash: $0.14 input vs $0.0028 cache hit → $0.1372 saved per 1M.
        let savings = Pricing::for_model(MODEL_FLASH).cache_savings(&usage);
        assert!((savings - 0.1372).abs() < 1e-9);
        // Pro: $0.435 vs $0.003625 → $0.431375 saved per 1M.
        let savings = Pricing::for_model(MODEL_PRO).cache_savings(&usage);
        assert!((savings - 0.431375).abs() < 1e-9);
    }
}

/// Per-model token pricing (USD per 1M tokens).
///
/// - Pro: original V4 Pro rates
/// - Flash (0731 GA): ¥1 / ¥2 / ¥0.2 per 1M → ≈ $0.14 / $0.28 / $0.0028
///   (cache miss / output / cache hit) — ~9x cheaper than Pro.
pub struct Pricing {
    input_per_m: f64,
    output_per_m: f64,
    cache_hit_per_m: f64,
}

impl Pricing {
    const PRO: Pricing = Pricing {
        input_per_m: 0.435,
        output_per_m: 0.87,
        cache_hit_per_m: 0.003625,
    };
    const FLASH: Pricing = Pricing {
        input_per_m: 0.14,
        output_per_m: 0.28,
        cache_hit_per_m: 0.0028,
    };

    pub fn for_model(model: &str) -> Pricing {
        if model.contains("flash") { Self::FLASH } else { Self::PRO }
    }

    pub fn calculate(&self, usage: &Usage) -> f64 {
        let cache_hit = (usage.cache_read_input_tokens as f64 / 1_000_000.0) * self.cache_hit_per_m;
        let cache_miss = (usage.cache_creation_input_tokens as f64 / 1_000_000.0) * self.input_per_m;
        let non_cached = (usage.input_tokens.saturating_sub(usage.cache_read_input_tokens + usage.cache_creation_input_tokens) as f64 / 1_000_000.0) * self.input_per_m;
        let output = (usage.output_tokens as f64 / 1_000_000.0) * self.output_per_m;
        cache_hit + cache_miss + non_cached + output
    }

    /// USD saved because cached input tokens were billed at the cache-hit
    /// rate instead of the full input rate.
    pub fn cache_savings(&self, usage: &Usage) -> f64 {
        (usage.cache_read_input_tokens as f64 / 1_000_000.0) * (self.input_per_m - self.cache_hit_per_m)
    }
}
