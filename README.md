# Rope'n'Code

*In the bustling town of Devville, a legendary team called Rope'n'Code was known for building bridges between ideas and reality. Led by a wise engineer named Lauri, they didn't just write code—they wove it like rope, strand by strand, each thread a function, each knot a clever algorithm.*

A minimal ACP-native TUI client for [opencode](https://opencode.ai). Written in Rust with [Ratatui](https://ratatui.rs). Connects to the opencode agent over the [Agent Client Protocol](https://agentclientprotocol.com) — the open standard for editor–agent communication.

## Features

- **ACP-native** – speaks `session/new`, `session/load`, `session/prompt`, `session/list`, `session/set_config_option` directly over JSON-RPC 2.0/stdin
- **Session replay** – loads the full conversation history from any opencode session via `session/load` with streaming `session/update` notifications
- **Markdown rendering** – assistant responses rendered with `tui-markdown` (pulldown-cmark + syntect code highlighting)
- **Streaming text** – word-by-word display with sticky auto-scroll (viewport stays put when you scroll up to read)
- **Thinking separation** – `agent_thought_chunk` renders in dim gray, separate from the final response
- **Collapsible tool output** – tool results truncated to 5 lines by default, expand with `Tab`
- **Model picker** – `/model` opens a searchable popup with all available models; selection calls `session/set_config_option`
- **Status bar** – model/provider, working directory, context window %, and cumulative cost (right-aligned)
- **Error aggregation** – errors within a 600ms window coalesce into one red message block

## Usage

```
ropencode                          # new session in CWD
ropencode --session-id <id>        # load existing session
ropencode --session-id <id> --cwd /path
ropencode --list-sessions          # list sessions in CWD
ropencode --list-sessions --cwd /path
```

### Keybindings

| Key | Action |
|---|---|
| `Enter` | Send prompt |
| `/exit` + `Enter` | Quit |
| `/model` + `Enter` | Open model picker |
| `Tab` | Expand/collapse last tool output |
| `↑`/`↓` | Scroll conversation |
| `PgUp`/`PgDn` | Page scroll |
| `Home`/`End` | Jump to top/bottom |

## Architecture

```
┌─────────────┐     JSON-RPC 2.0      ┌──────────────────┐
│  ropencode  │ ◄────── stdin/stdout ───► │  opencode acp   │
│  (Rust TUI) │     line-delimited    │  (TypeScript)    │
└──────┬──────┘                        └──────────────────┘
       │
       ├─ acp.rs        — JSON-RPC transport, oneshot response routing
       ├─ model.rs      — conversation state, line cache, markdown→Line
       └─ tui.rs        — Ratatui rendering, scroll management, model picker
```

### Key design decisions

- **No alternate screen dependency**. Despite using alternate screen for the TUI, the scrollback model is straightforward: pre-rendered `Line` cache per message, O(viewport) frame copy, `Paragraph::wrap()` for word wrap.
- **Response routing via oneshot**. Each JSON-RPC request gets a `oneshot::Sender` stored in `Arc<Mutex<HashMap>>`. A background reader thread dispatches incoming lines: responses go to the matching oneshot, notifications go to the TUI event channel. No locks held during await.
- **Command channel**. The TUI can't own the ACP client (it's blocked on response futures), so commands flow back through a second `mpsc::UnboundedChannel<TuiCommand>` to a background task that owns the client.

## Building

```bash
# normal build
cargo build --release

# requires opencode built locally for the ACP server
bun build --compile --target=bun-linux-x64 \
  --outfile=/tmp/opencode-full \
  packages/opencode/src/index.ts

# deploy (or symlink)
sudo cp /tmp/opencode-full /usr/local/bin/opencode
```

The ACP server expects `opencode` in PATH. Set `OPENCODE_DISABLE_CHANNEL_DB=1` if you're running a locally-built opencode against the stable database.

## Dependencies

- [ratatui](https://ratatui.rs) — TUI framework
- [crossterm](https://github.com/crossterm-rs/crossterm) — terminal backend
- [tui-markdown](https://crates.io/crates/tui-markdown) — markdown → styled Text (pulldown-cmark + syntect)
- [tokio](https://tokio.rs) — async runtime
- [serde_json](https://crates.io/crates/serde_json) — JSON-RPC wire protocol

~3600 lines of Rust, 1 binary, zero config files.

*"Leave it to Rope'n'Code—they'll tie it up nicely."*
