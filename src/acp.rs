// Minimal ACP client — hand-rolled JSON-RPC over stdio.
//
// FIXED: `content` in session/update is `{type, text}`, not `[{type, text}]`.

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Clone)]
pub enum TuiCommand {
    SendPrompt { content: String },
    SetModel { model: String },
    ListSessions { cwd: String },
    LoadSession { session_id: String, cwd: String },
}

#[derive(Debug, Clone)]
pub struct SessionEntry {
    pub session_id: String,
    pub title: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub enum Event {
    AgentTextChunk { session_id: String, text: String },
    AgentThoughtChunk { session_id: String, text: String },
    AgentTextDone { session_id: String },
    UserMessage { session_id: String, text: String },
    ToolCallUpdate { session_id: String, tool: String, status: String },
    ToolResult { session_id: String, tool: String, result: String },
    SessionCreated { session_id: String },
    ModelList(Vec<String>),
    SessionList(Vec<SessionEntry>),
    UsageUpdate { ctx_pct: f64, ctx_total: u64, cost: f64 },
    ConfigUpdate { model: Option<String>, provider: Option<String> },
    Error(String),
}

pub struct Client {
    pub stdin: ChildStdin,
    _child: Child,
    next_id: u64,
    pub pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
}

impl Client {
    pub fn spawn(bin: &str) -> Result<(Self, BufReader<std::process::ChildStdout>, BufReader<std::process::ChildStderr>)> {
        let mut child = Command::new(bin)
            .arg("acp")
            .env("OPENCODE_DISABLE_CHANNEL_DB", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to spawn opencode acp")?;

        let stdin = child.stdin.take().context("no stdin")?;
        let stdout = child.stdout.take().context("no stdout")?;
        let stderr = child.stderr.take().context("no stderr")?;

        Ok((
            Self {
                stdin,
                _child: child,
                next_id: 1,
                pending: Arc::new(Mutex::new(HashMap::new())),
            },
            BufReader::new(stdout),
            BufReader::new(stderr),
        ))
    }

    pub async fn request(&mut self, method: &str, params: Option<Value>) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;

        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);

        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let mut buf = serde_json::to_vec(&req)?;
        buf.push(b'\n');
        self.stdin.write_all(&buf)?;
        self.stdin.flush()?;

        match rx.await {
            Ok(v) => {
                // JSON-RPC error responses have "code" (number) AND "message" (string).
                // Normal result objects might have a "message" key too, so check for code first.
                if v.get("code").and_then(|c| c.as_i64()).is_some() {
                    let msg = v["message"].as_str().unwrap_or("unknown error");
                    anyhow::bail!("{msg}")
                }
                Ok(v)
            }
            Err(_) => anyhow::bail!("response channel closed"),
        }
    }

    pub async fn initialize(&mut self) -> Result<Value> {
        self.request("initialize", Some(serde_json::json!({
            "protocolVersion": 1,
            "clientInfo": { "name": "ropencode", "version": "0.1.0" }
        }))).await
    }

    pub async fn new_session(&mut self, cwd: &str) -> Result<Value> {
        self.request("session/new", Some(serde_json::json!({
            "cwd": cwd,
            "mcpServers": [],
        }))).await
    }

    pub async fn load_session(&mut self, session_id: &str, cwd: &str) -> Result<Value> {
        self.request("session/load", Some(serde_json::json!({
            "sessionId": session_id,
            "cwd": cwd,
            "mcpServers": [],
        }))).await
    }

    pub async fn list_sessions(&mut self, cwd: Option<&str>) -> Result<Value> {
        let params = cwd.map(|dir| serde_json::json!({ "cwd": dir }));
        self.request("session/list", params).await
    }

    pub async fn set_model(&mut self, session_id: &str, model: &str) -> Result<Value> {
        self.request("session/set_config_option", Some(serde_json::json!({
            "sessionId": session_id,
            "configId": "model",
            "value": model,
        }))).await
    }

    pub async fn prompt(&mut self, session_id: &str, content: &str) -> Result<Value> {
        let r = self.request("session/prompt", Some(serde_json::json!({
            "sessionId": session_id,
            "prompt": [{"type": "text", "text": content}],
        }))).await;
        // The prompt request may return an error or hang; pass through as-is
        r
    }
}

// ---------------------------------------------------------------------------
// Reader thread
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Incoming {
    #[serde(default)]
    id: Option<u64>,
    method: Option<String>,
    result: Option<Value>,
    #[serde(default)]
    error: Option<Value>,
    #[serde(default)]
    params: Option<Value>,
}

/// Read stderr lines from the subprocess and forward as Error events.
/// Extract model/provider from a session/new or session/load response.
pub fn parse_config_options(resp: &serde_json::Value, event_tx: &mpsc::UnboundedSender<Event>) {
    if let Some(configs) = resp["configOptions"].as_array() {
        for cfg in configs {
            if cfg["id"] == "model" {
                let val = cfg["currentValue"].as_str().unwrap_or("");
                let parts: Vec<&str> = val.splitn(2, '/').collect();
                if parts.len() == 2 {
                    let _ = event_tx.send(Event::ConfigUpdate {
                        model: Some(parts[1].to_string()),
                        provider: Some(parts[0].to_string()),
                    });
                }
                if let Some(opts) = cfg["options"].as_array() {
                    let models: Vec<String> = opts.iter()
                        .filter_map(|o| o["value"].as_str().map(|s| s.to_string()))
                        .collect();
                    let _ = event_tx.send(Event::ModelList(models));
                }
            }
        }
    }
}

pub fn start_stderr_reader<R: BufRead + Send + 'static>(
    reader: R,
    tx: mpsc::UnboundedSender<Event>,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        let mut reader = reader;
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    let trimmed = line.trim().to_string();
                    if !trimmed.is_empty() {
                        let _ = tx.send(Event::Error(trimmed));
                    }
                }
                Err(_) => break,
            }
        }
    })
}

pub fn start_reader<R: BufRead + Send + 'static>(
    reader: R,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    tx: mpsc::UnboundedSender<Event>,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        let mut reader = reader;
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {}
                Err(e) => {
                    let _ = tx.send(Event::Error(format!("read error: {e}")));
                    break;
                }
            }

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let Ok(msg) = serde_json::from_str::<Incoming>(trimmed) else {
                let _ = tx.send(Event::Error(format!("parse error: {trimmed}")));
                continue;
            };

            if let Some(id) = msg.id {
                if let Some(sender) = pending.lock().unwrap().remove(&id) {
                    // Prefer error over result for error responses
                    let value = msg.error.clone().or(msg.result).unwrap_or(Value::Null);
                    let _ = sender.send(value);
                }
            } else if let Some(method) = msg.method {
                if let Some(evt) = parse_notification(&method, &msg.params.unwrap_or(Value::Null)) {
                    let _ = tx.send(evt);
                }
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Notification parser
// ---------------------------------------------------------------------------

fn parse_notification(method: &str, params: &Value) -> Option<Event> {
    let sid = params["sessionId"].as_str().unwrap_or("?").to_string();

    match method {
        "session/update" => {
            let update = &params["update"];
            let kind = update["sessionUpdate"].as_str()?;
            let text = update["content"]["text"].as_str().unwrap_or("").to_string();
            match kind {
                "agent_message_chunk" => {
                    Some(Event::AgentTextChunk { session_id: sid, text })
                }
                "agent_thought_chunk" => {
                    Some(Event::AgentThoughtChunk { session_id: sid, text })
                }
                "user_message_chunk" => {
                    Some(Event::UserMessage { session_id: sid, text })
                }
                "tool_call" => {
                    let tool = update["title"].as_str().or_else(|| update["toolCall"]["name"].as_str()).unwrap_or("?").to_string();
                    Some(Event::ToolCallUpdate { session_id: sid, tool, status: "running".into() })
                }
                "tool_call_update" | "tool_call_result" => {
                    let tool = update["title"].as_str().or_else(|| update["toolCall"]["name"].as_str()).unwrap_or("?").to_string();
                    let result = update["result"].as_str().or_else(|| {
                        update["content"][0]["content"]["text"].as_str()
                    }).unwrap_or("").to_string();
                    Some(Event::ToolResult { session_id: sid, tool, result })
                }
                "usage_update" => {
                    let size = update["size"].as_u64().unwrap_or(0);
                    let used = update["used"].as_u64().unwrap_or(0);
                    let ctx_pct = if size > 0 { used as f64 / size as f64 * 100.0 } else { 0.0 };
                    let cost = update["cost"].as_f64().unwrap_or(0.0);
                    Some(Event::UsageUpdate { ctx_pct, ctx_total: size, cost })
                }
                "config_option_update" => {
                    let model = update["model"].as_str().or_else(|| update["config"]["model"].as_str()).map(|s| s.to_string());
                    let provider = update["provider"].as_str().or_else(|| update["config"]["provider"].as_str()).map(|s| s.to_string());
                    Some(Event::ConfigUpdate { model, provider })
                }
                _ => None,
            }
        }
        "session/created" | "session/new" => {
            let sid = params["sessionId"].as_str().unwrap_or("?").to_string();
            Some(Event::SessionCreated { session_id: sid })
        }
        _ => None,
    }
}
