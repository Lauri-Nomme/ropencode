# Ropencode — Improvement Plan

## Bugs (start here)

### B1. `now_str()` uses UTC epoch math, not local time
**File:** `src/model.rs:26-35`

`SystemTime::now().duration_since(UNIX_EPOCH)` gives seconds since epoch. Dividing by 3600 for hours gives **UTC** hours (epoch is UTC midnight). In non-UTC timezones (e.g., Finland UTC+2/+3), the displayed HH:MM:SS on messages is wrong.

**Fix:** Use `chrono::Local::now().format("%H:%M:%S")` or hand-roll with `localtime_r`/`gmtime_r` — chrono is cleaner but adds a dep. Alternative: use std `SystemTime` + manual UTC→local offset calculation (fragile). Best: add chrono.

**Severity:** High — timestamps on every message are misleading.

---

### B2. `SendPrompt` carries empty `session_id` field
**File:** `src/tui.rs:186`

```rust
let _ = app.cmd_tx.send(crate::acp::TuiCommand::SendPrompt { session_id: String::new(), content: text });
```

The `session_id` field is never set — it's always `""`. The command handler in `main.rs:109` closes over `sid_for_cmd` directly and never reads the field. Dead weight and confusing.

**Fix:** Remove `session_id` from the `TuiCommand::SendPrompt` variant.

**Severity:** High — dead code that could mislead future work.

---

### B3. `finish_thinking()` marks the *last* thinking message, not the correct one
**File:** `src/model.rs:132-136`

```rust
pub fn finish_thinking(&mut self) {
    if let Some(msg) = self.messages.iter_mut().rev().find(|m| m.is_thinking) {
        msg.is_thinking = false;
    }
}
```

`append_thinking()` adds a new assistant message and appends rendered lines to it, marking it `is_thinking = true`. But if called multiple times (multiple thinking segments), `finish_thinking()` only finds the last one — earlier segments stay `is_thinking = true` forever, rendering in dim gray.

**Fix:** Track a mutating reference or index to the current thinking message. Simpler: `append_delta()` already checks `is_thinking` and calls `finish_thinking()` before starting response text. The real issue is if `finish_thinking` is called externally (e.g., `AgentTextDone`). Could instead store the thinking message index and clear that.

**Severity:** Medium — affects multi-segment thinking responses.

---

### B4. `Tab` toggles only the *last* tool call
**File:** `src/tui.rs:192-196`

```rust
if let Some(msg) = app.conversation.messages.iter_mut().rev().find(|m| !m.tool_calls.is_empty()) {
    let idx = msg.tool_calls.len() - 1;
    msg.tool_calls[idx].collapsed = !msg.tool_calls[idx].collapsed;
```

Only toggles the last tool call in the last message with tools. Messages with multiple tool calls can only expand/collapse the final one.

**Fix:** Cycle through tool calls in the last message on each Tab press, or add a visual index and let the user target a specific call.

**Severity:** Low — most messages have one tool call.

---

### B5. No error handling for `tui-markdown::from_str`
**File:** `src/model.rs:233-244`

```rust
fn render_text_lines(text: &str) -> Vec<Line<'static>> {
    let text = tui_markdown::from_str(text);
```

`tui-markdown::from_str` could panic on pathological or extremely large markdown input (no public guarantees in its docs). Any panic here kills the reader thread (via `spawn_blocking`) or the TUI, losing the session.

**Fix:** Wrap in `std::panic::catch_unwind` or check upstream crate for fallible API. At minimum, log the panic and return a placeholder.

**Severity:** Medium — silent crash on bad input.

---

## UX Gaps

### U1. No `/help` command
**Files:** `src/tui.rs` (input handling), `src/main.rs` (commands)

No way to discover available commands (`/exit`, `/model`, future additions) from within the TUI. User must read docs or source.

**Implementation:** Handle `/help` in the input handler to set a help overlay similar to model picker, listing commands and keybindings.

**Priority:** High

---

### U2. No abort for in-flight prompts
**File:** `src/tui.rs` (input handling), `src/acp.rs` (no cancel method)

Once a `session/prompt` is sent, the user is locked in until the ACP returns. Should support `Ctrl+C` or Escape to send cancellation (opencode has no ACP cancel yet, but we could at minimum stop accepting input or send a notification).

**Implementation:** 
1. ACP doesn't have `session/cancel` yet — but we can detect `Ctrl+C` and not send further prompts while busy, perhaps show a cancel indicator.
2. If opencode adds ACP cancellation later, wire to `session/cancel` RPC.

**Priority:** High

---

### U3. No current-model indicator in model picker
**File:** `src/tui.rs` (model picker rendering)

The model picker shows all available models but doesn't indicate which one is currently active. User must close picker and look at the status bar.

**Fix:** Mark the current model in the list (e.g., `▸ anthropic/claude-sonnet-4-20250514  ← active`).

**Priority:** Medium

---

### U4. Static "thinking" indicator
**File:** `src/tui.rs:247-249`

```rust
lines.push(Line::styled(" ● thinking…", Style::default().fg(Color::Yellow)));
```

Shows a static dot. A simple frame-based animation (spinning dots `⣷ ⣯ ⣟ ⡿ ⢿ ⣻ ⣽ ⣾`) would make waiting feel less stuck.

**Implementation:** Global tick counter in `App`, modulo 8 to select animation frame. Increment on each render call.

**Priority:** Low

---

### U5. No context-warning visual in status bar
**File:** `src/tui.rs:263-285`

Context percentage is shown numerically but never highlighted when approaching the limit. Users running long sessions may hit the context ceiling without warning.

**Fix:** Change color to yellow at >70%, red at >90%.

**Priority:** Medium

---

### U6. No multi-line input
**File:** `src/tui.rs` (input handling)

`Enter` always sends. No way to type multi-line prompts. Standard chat UI convention: `Enter` to send, `Alt+Enter` for newline.

**Fix:** Distinguish `Alt+Enter` from plain `Enter` in the key handler.

**Priority:** Medium

---

## Polish

### P1. Configuration file support
**New file:** `~/.config/ropencode.toml`

Hardcoded colors, font choices, default model, keybindings. A config file with theming support would make it feel like a real app.

**Proposed schema:**
```toml
[theme]
status_bar_bg = "#14141C"
user_color = "#FF00FF"
assistant_color = "#FFFFFF"
error_color = "#FF0000"
thinking_color = "#555555"
accent_color = "#00FFFF"

[defaults]
model = "anthropic/claude-sonnet-4-20250514"
cwd = "."

[keybindings]
send = "Enter"
newline = "Alt+Enter"
abort = "Ctrl+C"
model_picker = "/model"
quit = "/exit"
```

**Priority:** Medium

---

### P2. Session listing in-TUI
**File:** `src/tui.rs`, `src/acp.rs`

`/sessions` command to list, switch, and delete sessions from within the TUI without restarting.

**Implementation:** Add `/sessions` command → send `session/list` ACP → render list overlay → select with arrow keys → call `session/load` on selection. Requires `restart` capability or loading a new session into the same TUI state.

**Priority:** Medium

---

### P3. Smoother streaming (block-buffered)
**File:** `src/tui.rs` (event handling), `src/model.rs` (append)

Currently every `AgentTextChunk` character triggers a full re-render of the entire message. For fast streams this causes visible jank and high CPU.

**Fix:** Buffer incoming chunks and flush to the line cache every ~50ms or on word boundary. Use a `StreamBuffer` that accumulates raw text and flushes at tick boundaries.

**Priority:** Medium

---

## Technical Debt

### T1. `parse_config_options` in wrong file
**File:** `src/main.rs:33-55`

This function parses ACP response data but lives in `main.rs`. It should be a method on or near the ACP client in `acp.rs`.

**Severity:** Low — cosmetic.

---

### T2. Hardcoded `OPENCODE_DISABLE_CHANNEL_DB=1`
**File:** `src/acp.rs:46`

The env var is hardcoded into `Client::spawn()`. This was a workaround for local development. Should be configurable via CLI flag or environment passthrough.

**Severity:** Low

---

### T3. No tests
The project has zero tests. Even a basic smoke test (spawn the ACP binary, initialize, create a session) would prevent regressions.

**Severity:** Low — project is early-stage.

---

### T4. Many unwrap() calls
Throughout the codebase. E.g., `acp.rs:74` (`pending.lock().unwrap()`), `tui.rs:99-101` (terminal setup). These panic on failure without context.

**Severity:** Low — acceptable for early prototype, but should be addressed before v1.

---

## Rendering & Visual Polish

### ✅ R1. Code block background fill
**Files:** `src/model.rs`, `src/config.rs`

Code blocks now get `code_bg` (configurable, default `#191923`) background fill. Detected via `\`\`\`` fence tracking in `render_text_lines()` — all lines between open/close fences get the background patched onto them.

**Priority:** High

**Status:** ✅ Done — 2026-07-24

---

### ✅ R2. Custom tui-markdown stylesheet
**Files:** `src/model.rs`, `src/config.rs`

`ThemeStyleSheet` implements `StyleSheet`, constructed from `Theme`. Switched `from_str()` → `from_str_with_options()`. Headings, inline code, links, blockquotes, and metadata styles are now driven by theme colors.

**Priority:** High

**Status:** ✅ Done — 2026-07-24

---

### ✅ R3. Configurable syntax theme
**Files:** `src/config.rs`

Added `syntax_theme` field to `Theme` (default `"base16-ocean.dark"`). Config field accepted but **not wired** — `tui-markdown` hardcodes the theme name. Requires crate patch or fork to actually switch themes.

**Priority:** Medium

**Status:** ✅ Config field added (crate limitation documented) — 2026-07-24

---

### ✅ R4. Search within conversation
**Files:** `src/tui.rs`

`/search <query>` matches against `cached_lines`. Current match line highlighted with `selection_bg`; matched text segments highlighted with inverted colors (preserving per-span styling). `n`/`N` navigates matches with centered scrolling. Status bar shows `search(3/15): query`.

**Priority:** Medium

**Status:** ✅ Done — 2026-07-24

---

### ✅ R5. Diff rendering
**Files:** `src/model.rs`

`\`\`\`diff` code blocks detected in `render_text_lines()`. `+` lines (excluding `+++`) colored green, `-` lines (excluding `---`) colored red, `@@` hunk headers colored with `heading_fg` (accent). Overrides syntect's coloring for diff blocks.

**Priority:** High

**Status:** ✅ Done — 2026-07-24

---

### ✅ R6. Clickable links
**Files:** `src/tui.rs`

`Ctrl+O` detects the first `https://` or `http://` URL in the line at viewport center and opens it via `xdg-open`. Link styling from the custom stylesheet (theme `link_fg`, default `#50A0FF` underlined).

**Priority:** Low

**Status:** ✅ Done — 2026-07-24

---

### ✅ R7. Table rendering
**Files:** `src/model.rs`

`tui-markdown` silently drops tables (`Tag::Table => warn!("Table not yet supported")`). Added `preprocess_tables()` that detects markdown table lines (`|...|...|` pattern with `---|---|` divider) and wraps them in a `\`\`\`` code block so they render as monospace table text.

**Priority:** Low

**Status:** ✅ Done — 2026-07-24

---

### ✅ R8. Better code block header
**Files:** `src/model.rs`

Raw `\`\`\`lang` / `\`\`\`` fences replaced with rounded corner box-drawing characters:
```
 ╭─ rust ─╮
 │ code    │
 ╰─╯
```
Opening header: `╭─ lang ─╮` in `heading_fg`. Closing footer: `╰─╯` in `DarkGray`. Both get `code_bg` background. Content lines keep their background fill.

**Priority:** Medium

**Status:** ✅ Done — 2026-07-24

---

### ✅ R9. Selection / copy support
**Files:** `src/tui.rs`

`v` toggles selection at viewport center. Selection spans from anchor line to current viewport center — selected lines highlighted with `selection_bg`. `y` yanks the concatenated text via OSC 52 escape sequence (system clipboard, no external dep). `Esc` cancels selection.

**Priority:** Low

**Status:** ✅ Done — 2026-07-24

---

## Execution Order

1. **Bugs** (B1→B2→B3→B4→B5)
2. **High-priority UX** (U1→U2→U3→U5→U6→U4)
3. **Rendering** (R2→R1→R5→R3→R8→R4→R6→R7→R9)
4. **Polish** (P1→P2→P3)
5. **Technical debt** (T1→T2→T3→T4)

Each item is a single commit.
