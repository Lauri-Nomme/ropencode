mod acp;
mod config;
mod model;
mod tui;

use anyhow::{Context, Result};
use tokio::sync::mpsc;

const ACP_BIN: &str = "opencode";

fn parse_args() -> cli::Args {
    let mut args = cli::Args::default();
    let mut raw = std::env::args().skip(1).peekable();
    while let Some(arg) = raw.next() {
        match arg.as_str() {
            "--session-id" => { args.session_id = raw.next(); }
            "--cwd" => { args.cwd = raw.next(); }
            "--list-sessions" => { args.list_sessions = true; }
            "--help" | "-h" => {
                eprintln!("Usage: ropencode [--session-id <id>] [--cwd <dir>] [--list-sessions]");
                std::process::exit(0);
            }
            _ => { eprintln!("Unknown flag: {arg}"); std::process::exit(1); }
        }
    }
    args
}

mod cli {
    #[derive(Default)]
    pub struct Args { pub session_id: Option<String>, pub cwd: Option<String>, pub list_sessions: bool }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = parse_args();
    let cwd = args.cwd.unwrap_or_else(|| {
        std::env::current_dir().map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|_| ".".to_string())
    });

    let (event_tx, event_rx) = mpsc::unbounded_channel::<acp::Event>();
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<acp::TuiCommand>();

    let (mut client, stdout_reader, stderr_reader) = acp::Client::spawn(ACP_BIN)?;
    let pending = std::sync::Arc::clone(&client.pending);
    let _reader = acp::start_reader(stdout_reader, pending, event_tx.clone());
    let _stderr_reader = acp::start_stderr_reader(stderr_reader, event_tx.clone());

    let _caps = client.initialize().await.context("initialize failed")?;

    if args.list_sessions {
        let resp = client.list_sessions(Some(&cwd)).await?;
        let sessions = &resp["sessions"];
        if sessions.as_array().map_or(true, |a| a.is_empty()) {
            eprintln!("No sessions found in {cwd}");
        } else {
            eprintln!("Sessions in {cwd}:");
            for s in sessions.as_array().unwrap() {
                eprintln!("  {}  {}  [{}]",
                    s["sessionId"].as_str().unwrap_or("?"),
                    s["title"].as_str().unwrap_or("(untitled)"),
                    s["updatedAt"].as_str().unwrap_or("?"));
            }
        }
        return Ok(());
    }

    let session_id = match &args.session_id {
        Some(sid) => {
            eprintln!("Loading session {sid} …");
            let resp = client.load_session(sid, &cwd).await?;
            acp::parse_config_options(&resp, &event_tx);
            sid.clone()
        }
        None => {
            eprintln!("Creating new session …");
            let resp = client.new_session(&cwd).await?;
            acp::parse_config_options(&resp, &event_tx);
            let sid = resp["sessionId"].as_str().context("missing sessionId")?.to_string();
            eprintln!("Session {sid} created");
            sid
        }
    };

    // Command handler: forwards prompts from TUI to ACP
    let sid_for_cmd = session_id.clone();
    let cmd_event_tx = event_tx.clone();
    tokio::spawn(async move {
        let mut client = client;
        while let Some(cmd) = cmd_rx.recv().await {
            fn send_error(tx: &mpsc::UnboundedSender<acp::Event>, msg: String) {
                let _ = tx.send(acp::Event::Error(msg));
            }
            match cmd {
                acp::TuiCommand::SendPrompt { content } => {
                    match client.prompt(&sid_for_cmd, &content).await {
                        Ok(_) => { let _ = cmd_event_tx.send(acp::Event::AgentTextDone { session_id: sid_for_cmd.clone() }); }
                        Err(e) => {
                            // Extract user-friendly message from ACP error response
                            let err_val = format!("{e}");
                            let msg = if let Some(start) = err_val.find("\"message\":\"") {
                                let after = &err_val[start + 11..];
                                if let Some(end) = after.find('"') {
                                    after[..end].to_string()
                                } else { err_val }
                            } else { err_val };
                            send_error(&cmd_event_tx, msg);
                        }
                    }
                }
                acp::TuiCommand::SetModel { model } => {
                    match client.set_model(&sid_for_cmd, &model).await {
                        Ok(_) => {
                            let parts: Vec<&str> = model.splitn(2, '/').collect();
                            let (provider, model_name) = if parts.len() == 2 {
                                (Some(parts[0].to_string()), Some(parts[1].to_string()))
                            } else {
                                (None, Some(model.clone()))
                            };
                            let _ = cmd_event_tx.send(acp::Event::ConfigUpdate { model: model_name, provider });
                        }
                        Err(e) => send_error(&cmd_event_tx, format!("set_model: {e}")),
                    }
                }
                acp::TuiCommand::ListSessions { cwd } => {
                    match client.list_sessions(Some(&cwd)).await {
                        Ok(resp) => {
                            let sessions: Vec<acp::SessionEntry> = resp["sessions"].as_array().map(|arr| {
                                arr.iter().map(|s| acp::SessionEntry {
                                    session_id: s["sessionId"].as_str().unwrap_or("").to_string(),
                                    title: s["title"].as_str().unwrap_or("(untitled)").to_string(),
                                    updated_at: s["updatedAt"].as_str().unwrap_or("").to_string(),
                                }).collect()
                            }).unwrap_or_default();
                            let _ = cmd_event_tx.send(acp::Event::SessionList(sessions));
                        }
                        Err(e) => send_error(&cmd_event_tx, format!("list_sessions: {e}")),
                    }
                }
                acp::TuiCommand::LoadSession { session_id, cwd } => {
                    match client.load_session(&session_id, &cwd).await {
                        Ok(resp) => {
                            let _ = cmd_event_tx.send(acp::Event::SessionCreated { session_id: session_id.clone() });
                            acp::parse_config_options(&resp, &cmd_event_tx);
                        }
                        Err(e) => send_error(&cmd_event_tx, format!("load_session: {e}")),
                    }
                }
            }
        }
    });

    let cfg = config::Config::load();
    tui::run(event_rx, cmd_tx.clone(), cwd, cfg.theme).await
}
