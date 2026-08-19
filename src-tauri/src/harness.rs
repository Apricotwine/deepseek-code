//! DeepSeek Harness SDK stdio JSON-RPC driver.
//!
//! Drives a Harness runtime subprocess over newline-delimited JSON-RPC 2.0 on
//! stdio, per the `@deepseek-ai/dsh-sdk-protocol` wire contract:
//!
//! - requests:   `initialize`, `session/prompt`, `shutdown`
//! - notifications: `session.event`, `session.status`, `subagent.started`,
//!   `subagent.finished`
//!
//! stdout carries only JSON-RPC frames; diagnostics belong on stderr. This
//! module is protocol-only and transport-safe: `encode_request` and
//! `parse_line` are pure functions so the framing can be unit-tested without a
//! live runtime or a network credential.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc::{self, UnboundedReceiver};

/// Request parameters for the process-wide `initialize` handshake.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub cwd: String,
    pub provider: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
}

/// Request parameters for one user turn on one SDK session.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionPromptParams {
    pub session_id: String,
    pub content_blocks: Vec<Value>,
}

/// A decoded inbound stdio frame: either a response to an outstanding request
/// or a server-pushed notification.
#[derive(Debug, Clone)]
pub enum HarnessFrame {
    Response {
        id: u64,
        result: Value,
        error: Option<Value>,
    },
    Notification {
        method: String,
        params: Value,
    },
}

/// Server→client notification methods we surface directly.
pub const NOTIFY_SESSION_EVENT: &str = "session.event";
pub const NOTIFY_SESSION_STATUS: &str = "session.status";
pub const NOTIFY_SUBAGENT_STARTED: &str = "subagent.started";
pub const NOTIFY_SUBAGENT_FINISHED: &str = "subagent.finished";

/// Text content block for a `session/prompt` request.
pub fn text_block(text: impl Into<String>) -> Value {
    json!({ "type": "text", "text": text.into() })
}

/// Serialize a JSON-RPC 2.0 request as a single `\n`-terminated line.
pub fn encode_request(id: u64, method: &str, params: Value) -> String {
    let mut line = serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    }))
    .expect("request serialization cannot fail");
    line.push('\n');
    line
}

/// Parse one line from the child stdout into a response or notification.
///
/// Malformed / ignored lines return `None` (the transport contract says
/// malformed JSON lines are ignored; stdout purity is deployment-enforced).
pub fn parse_line(line: &str) -> Option<HarnessFrame> {
    let value: Value = serde_json::from_str(line).ok()?;
    let obj = value.as_object()?;
    let version_ok = obj.get("jsonrpc").and_then(Value::as_str) == Some("2.0");
    if !version_ok {
        return None;
    }
    // A frame with `id` is a response (result) or an error; a frame with only
    // `method` is a notification.
    if let Some(id) = obj.get("id").and_then(Value::as_u64) {
        if obj.contains_key("result") || obj.contains_key("error") {
            return Some(HarnessFrame::Response {
                id,
                result: obj.get("result").cloned().unwrap_or(Value::Null),
                error: obj.get("error").cloned(),
            });
        }
        return None;
    }
    if let Some(method) = obj.get("method").and_then(Value::as_str) {
        return Some(HarnessFrame::Notification {
            method: method.to_string(),
            params: obj.get("params").cloned().unwrap_or(Value::Null),
        });
    }
    None
}

/// Extract the session id carried by the three session-scoped notifications.
pub fn notification_session_id(frame: &HarnessFrame) -> Option<&str> {
    match frame {
        HarnessFrame::Notification { method, params } if matches!(
            method.as_str(),
            NOTIFY_SESSION_EVENT | NOTIFY_SESSION_STATUS
        ) => params.get("sessionId").and_then(Value::as_str),
        HarnessFrame::Notification { method, params } if matches!(
            method.as_str(),
            NOTIFY_SUBAGENT_STARTED | NOTIFY_SUBAGENT_FINISHED
        ) => params.get("childSessionId").and_then(Value::as_str),
        _ => None,
    }
}

/// The owned stdio child plus the inbound frame stream.
///
/// Spawning is intentionally separate from protocol parsing so the pure path
/// stays testable; call [`HarnessProcess::spawn`] with the built runtime and a
/// `cordis.yml` path.
pub struct HarnessProcess {
    child: Child,
    next_id: u64,
    /// Bounded not to back-pressure the child when a caller ignores frames.
    pub frames: UnboundedReceiver<HarnessFrame>,
    /// stderr is relayed here for diagnostics; it never mixes with stdout.
    pub stderr: UnboundedReceiver<String>,
}

impl HarnessProcess {
    /// Spawn `node <node_args...> <bin.js> <cordis.yml>` with piped stdio and
    /// begin pumping stdout frames and stderr lines into channels.
    ///
    /// `node_args` carries pre-bin flags such as `--import tsx` for source
    /// launch; `env` is merged over the inherited environment so the caller can
    /// inject `DEEPSEEK_API_KEY`, `DSH_CWD`, `DSH_SESSION_ROOT`, etc.
    pub fn spawn(
        node_bin: &str,
        bin: PathBuf,
        cordis: PathBuf,
        node_args: &[&str],
        env: &[(&str, &str)],
        cwd: &str,
    ) -> std::io::Result<Self> {
        let mut cmd = Command::new(node_bin);
        cmd.current_dir(cwd)
            .args(node_args)
            .arg(&bin)
            .arg(&cordis);
        for (key, value) in env {
            cmd.env(key, value);
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let (frame_tx, frame_rx) = mpsc::unbounded_channel();
        let (err_tx, err_rx) = mpsc::unbounded_channel();

        if let Some(out) = stdout {
            tokio::spawn(async move {
                let mut lines = BufReader::new(out).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if line.trim().is_empty() {
                        continue;
                    }
                    if let Some(frame) = parse_line(&line) {
                        let _ = frame_tx.send(frame);
                    }
                }
            });
        }
        if let Some(err) = stderr {
            tokio::spawn(async move {
                let mut lines = BufReader::new(err).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let _ = err_tx.send(line);
                }
            });
        }

        Ok(Self {
            child,
            next_id: 0,
            frames: frame_rx,
            stderr: err_rx,
        })
    }

    fn take_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        id
    }

    /// Write a request line to the child stdin.
    pub async fn write_request(&mut self, method: &str, params: Value) -> std::io::Result<u64> {
        use tokio::io::AsyncWriteExt;
        let id = self.take_id();
        let line = encode_request(id, method, params);
        self.child
            .stdin
            .as_mut()
            .expect("stdin is piped")
            .write_all(line.as_bytes())
            .await?;
        Ok(id)
    }

    /// Run the `initialize` handshake and return the assigned request id.
    pub async fn initialize(&mut self, params: InitializeParams) -> std::io::Result<u64> {
        self.write_request("initialize", serde_json::to_value(params).unwrap())
            .await
    }

    /// Queue one user turn and return the assigned request id.
    pub async fn prompt(&mut self, params: SessionPromptParams) -> std::io::Result<u64> {
        self.write_request("session/prompt", serde_json::to_value(params).unwrap())
            .await
    }

    /// Request graceful shutdown.
    pub async fn shutdown(&mut self) -> std::io::Result<u64> {
        self.write_request("shutdown", json!({})).await
    }

    /// Wait for the child to exit, returning its exit status.
    pub async fn wait(mut self) -> std::io::Result<std::process::ExitStatus> {
        self.child.wait().await
    }
}

/// A standalone text prompt for a fresh or continued session.
pub fn text_prompt(session_id: impl Into<String>, text: impl Into<String>) -> SessionPromptParams {
    SessionPromptParams {
        session_id: session_id.into(),
        content_blocks: vec![text_block(text)],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_encoding_is_newline_delimited_jsonrpc() {
        let line = encode_request(7, "initialize", json!({"model": "deepseek-v4-flash"}));
        assert!(line.ends_with('\n'));
        let v: Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["id"], 7);
        assert_eq!(v["method"], "initialize");
        assert_eq!(v["params"]["model"], "deepseek-v4-flash");
    }

    #[test]
    fn notification_parses_with_session_id() {
        let line = r#"{"jsonrpc":"2.0","method":"session.status","params":{"sessionId":"s1","status":"running"}}"#;
        let frame = parse_line(line).unwrap();
        assert_eq!(notification_session_id(&frame), Some("s1"));
        match frame {
            HarnessFrame::Notification { method, params } => {
                assert_eq!(method, "session.status");
                assert_eq!(params["status"], "running");
            }
            _ => panic!("expected notification"),
        }
    }

    #[test]
    fn response_parses_result_and_error() {
        let ok = r#"{"jsonrpc":"2.0","id":3,"result":{"messageId":"m9"}}"#;
        match parse_line(ok).unwrap() {
            HarnessFrame::Response { id, result, error } => {
                assert_eq!(id, 3);
                assert_eq!(result["messageId"], "m9");
                assert!(error.is_none());
            }
            _ => panic!("expected response"),
        }
        let err = r#"{"jsonrpc":"2.0","id":4,"error":{"code":-32603,"message":"boom"}}"#;
        match parse_line(err).unwrap() {
            HarnessFrame::Response { error, .. } => {
                assert_eq!(error.unwrap()["code"], -32603);
            }
            _ => panic!("expected error response"),
        }
    }

    #[test]
    fn malformed_and_foreign_lines_are_ignored() {
        assert!(parse_line("").is_none());
        assert!(parse_line("not json").is_none());
        // Wrong protocol version is ignored.
        assert!(parse_line(r#"{"jsonrpc":"1.0","method":"x"}"#).is_none());
        // A bare log line with no method/id is ignored.
        assert!(parse_line(r#"{"hello":"world"}"#).is_none());
    }

    #[test]
    fn text_prompt_builds_verbatim_text_block() {
        let p = text_prompt("sess-1", "say hi");
        assert_eq!(p.session_id, "sess-1");
        assert_eq!(p.content_blocks.len(), 1);
        assert_eq!(p.content_blocks[0]["type"], "text");
        assert_eq!(p.content_blocks[0]["text"], "say hi");
    }

    #[test]
    fn params_serialize_to_camel_case_wire_names() {
        let p = text_prompt("s1", "hi");
        let v: Value = serde_json::to_value(&p).unwrap();
        assert_eq!(v["sessionId"], "s1");
        assert_eq!(v["contentBlocks"][0]["text"], "hi");
        assert!(v.get("session_id").is_none(), "snake_case leaked: {v}");
        assert!(v.get("content_blocks").is_none(), "snake_case leaked: {v}");

        let init = InitializeParams {
            cwd: "/x".to_string(),
            provider: "deepseek-official".to_string(),
            model: "deepseek-v4-flash".to_string(),
            max_tokens: Some(9),
        };
        let iv: Value = serde_json::to_value(&init).unwrap();
        assert_eq!(iv["maxTokens"], 9);
        assert!(iv.get("max_tokens").is_none(), "snake_case leaked: {iv}");
    }

    #[test]
    fn subagent_notifications_map_to_child_session() {
        let line = r#"{"jsonrpc":"2.0","method":"subagent.finished","params":{"parentSessionId":"p","childSessionId":"c","status":"ok"}}"#;
        let frame = parse_line(line).unwrap();
        assert_eq!(notification_session_id(&frame), Some("c"));
    }

    #[tokio::test]
    #[ignore = "requires a live Harness source checkout and DEEPSEEK_API_KEY"]
    async fn live_round_trip_against_real_runtime() {
        use std::fs;

        let repo = std::env::var("HARNESS_REPO")
            .unwrap_or_else(|_| "/tmp/dsh-harness-upstream".to_string());
        // Unique session id per run so a stale JSONL log from a previous
        // failed run never collides with this live session.
        let session_id = format!("rust-live-{}", uuid::Uuid::new_v4());
        let session_root = "/tmp/dsh-rust-smoke-sessions";
        let _ = fs::remove_dir_all(session_root);
        let key = match std::env::var("DEEPSEEK_API_KEY") {
            Ok(k) if !k.is_empty() => k,
            _ => {
                eprintln!("skip: DEEPSEEK_API_KEY is not set");
                return;
            }
        };

        // Bare plugins in `cordis.yml` resolve from the configuration project,
        // so the config must live inside the Harness checkout during the test.
        let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../harness/cordis.yml");
        let tal = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../harness/tal-tool-result.mjs");
        let cfg_dir = PathBuf::from(&repo).join("examples/whale-rust-smoke");
        fs::create_dir_all(&cfg_dir).unwrap();
        let cfg = cfg_dir.join("cordis.yml");
        fs::copy(&src, &cfg).unwrap();
        fs::copy(&tal, cfg_dir.join("tal-tool-result.mjs")).unwrap();
        let bin = PathBuf::from(&repo).join("packages/examples/jsonrpc-demo/src/bin.ts");

        let mut proc = HarnessProcess::spawn(
            "/opt/homebrew/bin/node",
            bin,
            cfg,
            &["--import", "tsx"],
            &[
                ("DEEPSEEK_API_KEY", key.as_str()),
                ("DSH_CWD", "/tmp"),
                ("DSH_SESSION_ROOT", session_root),
                ("DSH_SYSTEM_PROMPT", "Reply with exactly: RUST_OK"),
                ("PATH", "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"),
            ],
            &repo,
        )
        .unwrap();

        proc.initialize(InitializeParams {
            cwd: "/tmp".to_string(),
            provider: "deepseek-official".to_string(),
            model: "deepseek-v4-flash".to_string(),
            max_tokens: Some(512),
        })
        .await
        .unwrap();
        proc.prompt(text_prompt(&session_id, "Reply with exactly: RUST_OK"))
            .await
            .unwrap();

        let mut saw_completed = false;
        let mut saw_text = false;
        let sleep = tokio::time::sleep(std::time::Duration::from_secs(90));
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                frame = proc.frames.recv() => {
                    match frame {
                        Some(HarnessFrame::Response { id, result, error }) => {
                            if let Some(err) = error {
                                eprintln!("[rpc error] id={id} {err}");
                            } else {
                                eprintln!("[rpc ok] id={id} {result}");
                            }
                        }
                        Some(HarnessFrame::Notification { method, params }) => {
                            if method == "session.event" {
                                let event = &params["event"];
                                let ty = event["type"].as_str().unwrap_or("?");
                                if ty == "turn/end" {
                                    eprintln!("[turn/end] {}", event["data"]["reason"]);
                                    saw_completed = true;
                                }
                                if ty == "assistant/message"
                                    && event["data"]["message"]["content"].to_string().contains("RUST_OK")
                                {
                                    saw_text = true;
                                }
                            }
                        }
                        None => break,
                    }
                }
                err = proc.stderr.recv() => {
                    if let Some(line) = err { eprintln!("[stderr] {line}"); }
                }
                _ = &mut sleep => { eprintln!("[timeout]"); break; }
            }
            if saw_completed {
                break;
            }
        }
        let _ = proc.shutdown().await;
        assert!(saw_completed, "turn did not reach turn/end");
        assert!(saw_text, "assistant message did not contain RUST_OK");
    }
}
