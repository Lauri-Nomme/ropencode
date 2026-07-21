// Rope'n'Code — minimal ACP TUI client.
//
// Usage:
//   ropencode                                    # new session in CWD
//   ropencode --session-id <id>                  # load existing session
//   ropencode --session-id <id> --cwd /path      # with explicit CWD
//   ropencode --list-sessions                    # list available sessions

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
            "--session-id" => {
                args.session_id = raw.next();
            }
            "--cwd" => {
                args.cwd = raw.next();
            }
            "--list-sessions" => {
                args.list_sessions = true;
            }
            "--help" | "-h" => {
                eprintln!("Usage: ropencode [--session-id <id>] [--cwd <dir>] [--list-sessions]");
                std::process::exit(0);
            }
            _ => {
                eprintln!("Unknown flag: {arg}");
                std::process::exit(1);
            }
        }
    }
    args
}

mod cli {
    #[derive(Default)]
    pub struct Args {
        pub session_id: Option<String>,
        pub cwd: Option<String>,
        pub list_sessions: bool,
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = parse_args();
    let cwd = args.cwd.unwrap_or_else(|| {
        std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string())
    });

    let (event_tx, event_rx) = mpsc::unbounded_channel::<acp::Event>();

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
                let id = s["sessionId"].as_str().unwrap_or("?");
                let title = s["title"].as_str().unwrap_or("(untitled)");
                let updated = s["updatedAt"].as_str().unwrap_or("?");
                eprintln!("  {id}  {title}  [{updated}]");
            }
        }
        return Ok(());
    }

    match &args.session_id {
        Some(sid) => {
            eprintln!("Loading session {sid} in {cwd} …");
            let resp = client.load_session(sid, &cwd).await?;
            // Parse model/provider from configOptions
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
                    }
                }
            }
            eprintln!("Session loaded — history events are streaming via ACP");
        }
        None => {
            eprintln!("Creating new session in {cwd} …");
            let resp = client.new_session(&cwd).await?;
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
                    }
                }
            }
            match resp["sessionId"].as_str() {
                Some(sid) => eprintln!("Session {sid} created"),
                None => eprintln!("Session created (no ID in response)"),
            }
        }
    }

    tui::run(event_rx, cwd).await
}
