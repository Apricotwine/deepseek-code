//! Run one prompt through the DeepSeek Harness kernel using our Rust driver.
//!
//! This is the event-forwarding layer that `lib.rs` will reuse: it spawns the
//! runtime, drives `initialize` + `session/prompt`, and renders the
//! `session.event` stream (reasoning, text, tools, usage) as it arrives.
//!
//! Dev-mode launcher (source-launched runtime); production will point at the
//! built `lib/bin.js` sidecar.

use deepseek_code_lib::harness::{HarnessFrame, HarnessProcess, InitializeParams};
use serde_json::Value;
use std::path::PathBuf;

fn get<'a>(v: &'a Value, key: &str) -> &'a Value {
    v.get(key).unwrap_or(&Value::Null)
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn display(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn read_key(cli: Option<&str>) -> String {
    if let Some(k) = cli {
        if !k.is_empty() {
            return k.to_string();
        }
    }
    if let Ok(k) = std::env::var("DEEPSEEK_API_KEY") {
        if !k.is_empty() {
            return k;
        }
    }
    let p = PathBuf::from(
        std::env::var("HOME").unwrap_or_default(),
    )
    .join("Library/Application Support/com.deepseek.code/settings.json");
    if let Ok(raw) = std::fs::read_to_string(&p) {
        if let Ok(v) = serde_json::from_str::<Value>(&raw) {
            if let Some(k) = v.get("api_key").and_then(Value::as_str) {
                return k.to_string();
            }
        }
    }
    String::new()
}

fn args_map() -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if let Some(flag) = a.strip_prefix("--") {
            let value = args.next().unwrap_or_default();
            map.insert(flag.to_string(), value);
        } else {
            map.entry("prompt".to_string()).or_insert(a);
        }
    }
    map
}

fn render_chunk(chunk: &Value) {
    match get(chunk, "type").as_str() {
        Some("text-delta") => {
            if let Some(t) = get(chunk, "text").as_str() {
                print!("{t}");
            }
        }
        Some("reasoning-delta") => {
            if let Some(t) = get(chunk, "text").as_str() {
                eprint!("\x1b[90m{t}\x1b[0m");
            }
        }
        Some("block-end") => {
            if let Some(b) = get(get(chunk, "block"), "text").as_str() {
                if get(chunk, "block")["type"] == "text" && b.trim().len() > 0 {
                    print!("{b}");
                }
            }
        }
        Some("usage") => {
            let u = get(chunk, "usage");
            eprintln!(
                "\n[usage] in={} out={} cache={} reasoning={}",
                display(&get(u, "inputTokens")),
                display(&get(u, "outputTokens")),
                display(&get(u, "cacheReadTokens")),
                display(&get(u, "reasoningTokens")),
            );
        }
        _ => {}
    }
}

fn render_event(params: &Value) {
    let event = get(params, "event");
    match get(event, "type").as_str() {
        Some("assistant/chunk") => render_chunk(get(get(event, "data"), "chunk")),
        Some("tool/call") => {
            let d = get(event, "data");
            eprintln!("\n[tool] {} {}", s(&get(d, "name")), s(&get(d, "arguments")));
        }
        Some("tool/result") => {
            let d = get(event, "data");
            let preview: String = d.to_string().chars().take(260).collect();
            eprintln!("\n[tool-result] {preview}");
        }
        Some("turn/end") => {
            eprintln!("\n[turn/end] {}", s(&get(get(event, "data"), "reason")));
        }
        _ => {}
    }
}

#[tokio::main]
async fn main() {
    let map = args_map();
    let prompt = map.get("prompt").cloned().unwrap_or_else(|| {
        eprintln!("usage: harness_run 'your task' [--model ...] [--effort off|high|max] [--repo /path/to/harness]");
        std::process::exit(2);
    });
    let model = map.get("model").cloned().unwrap_or_else(|| "deepseek-v4-flash".into());
    let effort = map.get("effort").cloned().unwrap_or_else(|| "high".into());
    let workspace = map.get("workspace").cloned().unwrap_or_else(|| "/tmp".into());
    let session = map.get("session").cloned().unwrap_or_else(|| "main".into());
    let repo = map.get("repo").cloned().unwrap_or_else(|| "/tmp/dsh-harness-upstream".into());
    let key = read_key(map.get("key").map(String::as_str));
    if key.is_empty() {
        eprintln!("no DeepSeek API key found (--key, DEEPSEEK_API_KEY, or app settings)");
        std::process::exit(3);
    }

    let (bin, cordis, node_args, cwd) = if let Some(rt) = map.get("runtime") {
        let rt = PathBuf::from(rt);
        (
            rt.join("node_modules/@deepseek-ai/dsh-sdk-jsonrpc-demo/lib/packaged-bin.js"),
            rt.join("cordis.yml"),
            Vec::<String>::new(),
            rt.display().to_string(),
        )
    } else {
        let cordis_src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../harness/cordis.yml");
        let tal_src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../harness/tal-tool-result.mjs");
        let cfg_dir = PathBuf::from(&repo).join("examples/whale-run");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        let cordis = cfg_dir.join("cordis.yml");
        std::fs::copy(&cordis_src, &cordis).unwrap();
        std::fs::copy(&tal_src, cfg_dir.join("tal-tool-result.mjs")).unwrap();
        (
            PathBuf::from(&repo).join("packages/examples/jsonrpc-demo/src/bin.ts"),
            cordis,
            vec!["--import".to_string(), "tsx".to_string()],
            repo.clone(),
        )
    };

    let node_args_refs: Vec<&str> = node_args.iter().map(String::as_str).collect();
    let mut proc = HarnessProcess::spawn(
        "/opt/homebrew/bin/node",
        bin,
        cordis,
        &node_args_refs,
        &[
            ("DEEPSEEK_API_KEY", key.as_str()),
            ("DSH_CWD", workspace.as_str()),
            ("DSH_SESSION_ROOT", "/tmp/dsh-run-sessions"),
            ("DSH_REASONING_EFFORT", effort.as_str()),
            ("PATH", "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"),
        ],
        &cwd,
    )
    .unwrap();

    proc.initialize(InitializeParams {
        cwd: workspace.clone(),
        provider: "deepseek-official".to_string(),
        model: model.clone(),
        max_tokens: Some(4096),
    })
    .await
    .unwrap();

    proc.prompt(deepseek_code_lib::harness::text_prompt(session, prompt))
        .await
        .unwrap();

    let mut done = false;
    let sleep = tokio::time::sleep(std::time::Duration::from_secs(300));
    tokio::pin!(sleep);
    while !done {
        tokio::select! {
            frame = proc.frames.recv() => {
                match frame {
                    Some(HarnessFrame::Response { id, error, result }) => {
                        if let Some(err) = error {
                            eprintln!("\n[rpc error] id={id} {err}");
                            done = true;
                        } else {
                            eprintln!("\n[rpc ok] id={id} {result}");
                        }
                    }
                    Some(HarnessFrame::Notification { method, params }) => {
                        if method == "session.event" {
                            render_event(&params);
                            if get(&get(&params, "event"), "type") == "turn/end" {
                                done = true;
                            }
                        }
                    }
                    None => done = true,
                }
            }
            err = proc.stderr.recv() => {
                if let Some(line) = err { eprintln!("[stderr] {line}"); }
            }
            _ = &mut sleep => { eprintln!("\n[timeout]"); done = true; }
        }
    }
    let _ = proc.shutdown().await;
    println!();
}
