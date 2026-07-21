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
    SendPrompt { session_id: String, content: String },
    SetModel { model: String },
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
    pub fn spawn(bin: &str) -> Result<(Self, BufReader<std::process::ChildStdout>)> {
        let mut child = Command::new(bin)
            .arg("acp")
            .env("OPENCODE_DISABLE_CHANNEL_DB", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("failed to spawn opencode acp")?;

        let stdin = child.stdin.take().context("no stdin")?;
        let stdout = child.stdout.take().context("no stdout")?;

        Ok((
            Self {
                stdin,
                _child: child,
                next_id: 1,
                pending: Arc::new(Mutex::new(HashMap::new())),
            },
            BufReader::new(stdout),
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
            Ok(v) => Ok(v),
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
        self.request("session/prompt", Some(serde_json::json!({
            "sessionId": session_id,
            "prompt": [{"type": "text", "text": content}],
        }))).await
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
                    let ctx_pct = update["contextPercentage"].as_f64().or_else(|| update["usage"]["contextPercentage"].as_f64()).unwrap_or(0.0);
                    let ctx_total = update["contextTotal"].as_u64().or_else(|| update["usage"]["contextTotal"].as_u64()).unwrap_or(0);
                    let cost = update["cost"].as_f64().or_else(|| update["usage"]["cost"].as_f64()).unwrap_or(0.0);
                    Some(Event::UsageUpdate { ctx_pct, ctx_total, cost })
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
