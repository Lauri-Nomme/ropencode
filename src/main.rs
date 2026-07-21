mod acp;
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

fn parse_config_options(resp: &serde_json::Value, event_tx: &mpsc::UnboundedSender<acp::Event>) {
    if let Some(configs) = resp["configOptions"].as_array() {
        for cfg in configs {
            if cfg["id"] == "model" {
                let val = cfg["currentValue"].as_str().unwrap_or("");
                let parts: Vec<&str> = val.splitn(2, '/').collect();
                if parts.len() == 2 {
                    let _ = event_tx.send(acp::Event::ConfigUpdate {
                        model: Some(parts[1].to_string()),
                        provider: Some(parts[0].to_string()),
                    });
                }
                // Send available model list
                if let Some(opts) = cfg["options"].as_array() {
                    let models: Vec<String> = opts.iter()
                        .filter_map(|o| o["value"].as_str().map(|s| s.to_string()))
                        .collect();
                    let _ = event_tx.send(acp::Event::ModelList(models));
                }
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = parse_args();
    let cwd = args.cwd.unwrap_or_else(|| {
        std::env::current_dir().map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|_| ".".to_string())
    });

    let (event_tx, event_rx) = mpsc::unbounded_channel::<acp::Event>();
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<acp::TuiCommand>();

    let (mut client, stdout_reader) = acp::Client::spawn(ACP_BIN)?;
    let pending = std::sync::Arc::clone(&client.pending);
    let _reader = acp::start_reader(stdout_reader, pending, event_tx.clone());

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
            parse_config_options(&resp, &event_tx);
            sid.clone()
        }
        None => {
            eprintln!("Creating new session …");
            let resp = client.new_session(&cwd).await?;
            parse_config_options(&resp, &event_tx);
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
            match cmd {
                acp::TuiCommand::SendPrompt { content, .. } => {
                    if let Err(e) = client.prompt(&sid_for_cmd, &content).await {
                        eprintln!("prompt error: {e}");
                    }
                }
                acp::TuiCommand::SetModel { model } => {
                    if let Err(e) = client.set_model(&sid_for_cmd, &model).await {
                        eprintln!("set_model error: {e}");
                    } else {
                        let parts: Vec<&str> = model.splitn(2, '/').collect();
                        let (provider, model_name) = if parts.len() == 2 {
                            (Some(parts[0].to_string()), Some(parts[1].to_string()))
                        } else {
                            (None, Some(model.clone()))
                        };
                        let _ = cmd_event_tx.send(acp::Event::ConfigUpdate { model: model_name, provider });
                    }
                }
            }
        }
    });

    tui::run(event_rx, cmd_tx.clone(), cwd).await
}
