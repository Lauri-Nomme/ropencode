use crate::acp::Event;
use crate::model::Conversation;
use anyhow::Result;
use crossterm::event::{self, Event as TermEvent, KeyCode, KeyEventKind, MouseEventKind};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap};
use ratatui::{Frame, Terminal};
use std::io;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

const STATUS_HEIGHT: usize = 2;
const ERROR_COALESCE_MS: u64 = 600;

enum Mode { Normal, ModelPicker { filter: String, models: Vec<String>, selected: usize, scroll: usize }, Help }

struct App {
    conversation: Conversation,
    scroll_offset: usize, sticky_bottom: bool, agent_busy: bool,
    input: String, mode: Mode, available_models: Vec<String>,
    viewport_height: usize, cached_lines: Vec<Line<'static>>,
    content_version: u64, last_rendered_version: u64,
    cwd: String, cmd_tx: mpsc::UnboundedSender<crate::acp::TuiCommand>,
    error_buffer: Vec<String>, last_error_flush: Option<Instant>,
    tab_tool_idx: usize,
}

impl App {
    fn new(cwd: String, cmd_tx: mpsc::UnboundedSender<crate::acp::TuiCommand>) -> Self {
        Self {
            conversation: Conversation::new(), scroll_offset: 0, sticky_bottom: true, agent_busy: false,
            input: String::new(), mode: Mode::Normal, available_models: vec![],
            viewport_height: 0, cached_lines: vec![], content_version: 0, last_rendered_version: 0,
            cwd, cmd_tx, error_buffer: vec![], last_error_flush: None,
            tab_tool_idx: 0,
        }
    }

    fn rebuild_cache(&mut self) { self.cached_lines = self.conversation.rendered_lines(); self.last_rendered_version = self.content_version; }
    fn ensure_cache(&mut self) { if self.last_rendered_version != self.content_version { self.rebuild_cache(); } }
    fn content_height(&self) -> usize { self.conversation.total_lines.max(self.cached_lines.len()) }
    fn max_scroll(&self) -> usize { self.content_height().saturating_sub(self.viewport_height.max(1)) }
    fn is_at_bottom(&self) -> bool { self.scroll_offset >= self.max_scroll() }
    fn clamp_offset(&mut self) { self.scroll_offset = self.scroll_offset.min(self.max_scroll()); }
    fn mark_dirty(&mut self) { self.content_version += 1; }
    fn auto_scroll(&mut self) { self.rebuild_cache(); if self.sticky_bottom { self.scroll_offset = self.max_scroll(); } }
    fn did_scroll_up(&mut self) { self.sticky_bottom = false; }
    fn check_sticky(&mut self) { if self.is_at_bottom() { self.sticky_bottom = true; } }

    fn flush_errors(&mut self) {
        if self.error_buffer.is_empty() { return; }
        self.conversation.error = Some(self.error_buffer.join("\n"));
        self.error_buffer.clear();
        self.last_error_flush = Some(Instant::now());
        self.mark_dirty();
        self.auto_scroll();
    }

    fn handle_event(&mut self, event: Event) {
        // Flush error buffer on non-error events or if threshold passed
        if !matches!(event, Event::Error(_)) {
            self.flush_errors();
        } else if let Some(t) = self.last_error_flush {
            if t.elapsed() >= Duration::from_millis(ERROR_COALESCE_MS) {
                self.flush_errors();
            }
        }

        match event {
            Event::AgentTextChunk { text, .. } => { self.agent_busy = true; self.conversation.append_delta(&text); self.mark_dirty(); self.auto_scroll(); }
            Event::AgentThoughtChunk { text, .. } => { self.agent_busy = true; self.conversation.append_thinking(&text); self.mark_dirty(); self.auto_scroll(); }
            Event::AgentTextDone { .. } => { self.agent_busy = false; self.conversation.finish_thinking(); self.mark_dirty(); self.auto_scroll(); }
            Event::UserMessage { text, .. } => { self.conversation.add_user_message(&text); self.mark_dirty(); self.sticky_bottom = true; self.auto_scroll(); }
            Event::ToolCallUpdate { tool, status, .. } => { self.conversation.add_tool_call(&tool, &status); self.mark_dirty(); self.auto_scroll(); }
            Event::ToolResult { tool, result, .. } => { self.conversation.complete_tool_call(&tool, &result); self.mark_dirty(); self.auto_scroll(); }
            Event::ModelList(models) => { self.available_models = models; }
            Event::UsageUpdate { ctx_pct, ctx_total, cost } => { self.conversation.info.ctx_pct = ctx_pct; self.conversation.info.ctx_total = ctx_total; self.conversation.info.cost = cost; }
            Event::ConfigUpdate { model, provider } => { if let Some(m) = model { self.conversation.info.model = m; } if let Some(p) = provider { self.conversation.info.provider = p; } }
            Event::SessionCreated { .. } => {}
            Event::Error(msg) => {
                if self.error_buffer.is_empty() {
                    self.last_error_flush = Some(Instant::now());
                }
                self.error_buffer.push(msg);
                // Don't mark dirty — we flush on next event or timeout
            }
        }
        self.clamp_offset();
    }
}

pub async fn run(
    event_rx: mpsc::UnboundedReceiver<Event>,
    cmd_tx: mpsc::UnboundedSender<crate::acp::TuiCommand>,
    cwd: String,
) -> Result<()> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    let mut app = App::new(cwd, cmd_tx);
    let res = run_loop(&mut terminal, &mut app, event_rx).await;
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), crossterm::terminal::LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    res
}

async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    mut event_rx: mpsc::UnboundedReceiver<Event>,
) -> Result<()> {
    let tick_rate = Duration::from_millis(33);
    loop {
        terminal.draw(|f| render(f, app))?;
        let timeout = tokio::time::sleep(tick_rate);
        tokio::pin!(timeout);
        tokio::select! {
            evt = event_rx.recv() => { match evt { Some(evt) => app.handle_event(evt), None => break } }
            _ = &mut timeout => {
                app.flush_errors();
                loop { match event_rx.try_recv() { Ok(evt) => app.handle_event(evt), Err(_) => break } }
            }
            crossterm_evt = poll_crossterm_event(tick_rate) => {
                if let Some(evt) = crossterm_evt { if handle_input(app, evt) { break; } }
            }
        }
    }
    Ok(())
}

async fn poll_crossterm_event(timeout: Duration) -> Option<TermEvent> {
    if event::poll(timeout).ok()? { event::read().ok() } else { None }
}

fn handle_input(app: &mut App, evt: TermEvent) -> bool {
    match &mut app.mode {
        Mode::ModelPicker { filter, models, selected, .. } => {
            match evt {
                TermEvent::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Esc => { app.mode = Mode::Normal; return false; }
                    KeyCode::Enter => {
                        let f = filter.clone();
                        let filtered: Vec<usize> = models.iter().enumerate().filter(|(_, m)| m.contains(&f) || f.is_empty()).map(|(i, _)| i).collect();
                        if let Some(idx) = filtered.get(*selected) {
                            if let Some(model) = models.get(*idx) {
                                let _ = app.cmd_tx.send(crate::acp::TuiCommand::SetModel { model: model.clone() });
                            }
                        }
                        app.mode = Mode::Normal;
                        return false;
                    }
                    KeyCode::Up => { *selected = selected.saturating_sub(1); }
                    KeyCode::Down => { *selected = selected.saturating_add(1); }
                    KeyCode::Backspace => { filter.pop(); *selected = 0; }
                    KeyCode::Char(c) => { if c != '\n' && c != '\r' { filter.push(c); *selected = 0; } }
                    _ => {}
                },
                _ => {}
            }
            let f = filter.clone();
            let filtered_len = models.iter().filter(|m| m.contains(&f) || f.is_empty()).count();
            if *selected >= filtered_len && filtered_len > 0 { *selected = filtered_len - 1; }
            return false;
        }
        Mode::Help => match evt {
            TermEvent::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Esc | KeyCode::Enter => { app.mode = Mode::Normal; }
                _ => {}
            }
            _ => {}
        },
        Mode::Normal => {}
    }

    match evt {
        TermEvent::Key(key) if key.kind == KeyEventKind::Press => match key.code {
            KeyCode::Enter => {
                app.flush_errors();
                let text = app.input.trim().to_string();
                if text == "/exit" { return true; }
                if text.starts_with("/model") {
                    app.mode = Mode::ModelPicker { filter: String::new(), models: app.available_models.clone(), selected: 0, scroll: 0 };
                    app.input.clear();
                    return false;
                }
                if text == "/help" {
                    app.mode = Mode::Help;
                    app.input.clear();
                    return false;
                }
                if !text.is_empty() {
                    app.conversation.add_user_message(&text); app.mark_dirty();
                    app.input.clear(); app.sticky_bottom = true;
                    app.rebuild_cache(); app.scroll_offset = app.max_scroll();
                    let _ = app.cmd_tx.send(crate::acp::TuiCommand::SendPrompt { content: text });
                }
                false
            }
            KeyCode::Backspace => { app.input.pop(); false }
            KeyCode::Tab => {
                // Find the last message with tool calls
                let last_msg_idx = app.conversation.messages.iter().rposition(|m| !m.tool_calls.is_empty());
                if let Some(msg_idx) = last_msg_idx {
                    let count = app.conversation.messages[msg_idx].tool_calls.len();
                    app.tab_tool_idx = app.tab_tool_idx % count;
                    app.conversation.messages[msg_idx].tool_calls[app.tab_tool_idx].collapsed =
                        !app.conversation.messages[msg_idx].tool_calls[app.tab_tool_idx].collapsed;
                    app.tab_tool_idx = (app.tab_tool_idx + 1) % count;
                    app.mark_dirty(); app.rebuild_cache();
                }
                false
            }
            KeyCode::Up => { if app.scroll_offset > 0 { app.scroll_offset -= 1; app.did_scroll_up(); } false }
            KeyCode::Down => { let max = app.max_scroll(); if app.scroll_offset < max { app.scroll_offset += 1; app.check_sticky(); } false }
            KeyCode::PageUp => { let vh = app.viewport_height.max(1); app.scroll_offset = app.scroll_offset.saturating_sub(vh); app.did_scroll_up(); false }
            KeyCode::PageDown => { let vh = app.viewport_height.max(1); let max = app.max_scroll(); app.scroll_offset = (app.scroll_offset + vh).min(max); app.check_sticky(); false }
            KeyCode::Home => { app.scroll_offset = 0; app.did_scroll_up(); false }
            KeyCode::End => { app.scroll_offset = app.max_scroll(); app.sticky_bottom = true; false }
            KeyCode::Char(c) if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) && c == 'c' => {
                if app.agent_busy {
                    app.agent_busy = false;
                    app.conversation.finish_thinking();
                    let msg = "─ cancelled ─";
                    app.conversation.add_user_message(msg);
                    app.mark_dirty();
                    app.auto_scroll();
                }
                false
            }
            KeyCode::Char(c) => { app.input.push(c); false }
            _ => false,
        },
        TermEvent::Mouse(mev) => match mev.kind {
            MouseEventKind::ScrollDown => { let max = app.max_scroll(); app.scroll_offset = (app.scroll_offset + 3).min(max); app.check_sticky(); false }
            MouseEventKind::ScrollUp => { app.scroll_offset = app.scroll_offset.saturating_sub(3); app.did_scroll_up(); false }
            _ => false,
        },
        TermEvent::Resize(_, h) => { app.viewport_height = (h as usize).saturating_sub(STATUS_HEIGHT + 3); app.clamp_offset(); if app.sticky_bottom { app.scroll_offset = app.max_scroll(); } false }
        _ => false,
    }
}

fn render(f: &mut Frame<'_>, app: &mut App) {
    let area = f.area();
    if area.width == 0 || area.height == 0 { return; }
    let convo_h = (area.height as usize).saturating_sub(STATUS_HEIGHT + 3);
    let chunks = Layout::default().direction(Direction::Vertical)
        .constraints([Constraint::Length(convo_h as u16), Constraint::Length(3), Constraint::Length(STATUS_HEIGHT as u16)])
        .split(area);
    app.viewport_height = chunks[0].height as usize;
    app.ensure_cache(); app.clamp_offset();
    render_conversation(f, chunks[0], app);
    render_input(f, chunks[1], app);
    render_status(f, chunks[2], app);
    if let Mode::ModelPicker { filter, models, selected, scroll: _ } = &app.mode {
        render_model_picker(f, area, filter.as_str(), models, *selected);
    }
    if let Mode::Help = &app.mode {
        render_help(f, area);
    }
    // Flush stale error buffer on render tick
    if !app.error_buffer.is_empty() {
        if let Some(t) = app.last_error_flush {
            if t.elapsed() >= Duration::from_millis(ERROR_COALESCE_MS) { app.flush_errors(); }
        }
    }
}

fn render_conversation(f: &mut Frame<'_>, area: Rect, app: &App) {
    let total = app.cached_lines.len();
    let offset = app.scroll_offset.min(total.saturating_sub(1));
    let start = offset;
    let end = (start + area.height as usize).min(total);
    let mut lines: Vec<Line<'static>> = if start < total { app.cached_lines[start..end].to_vec() } else { vec![] };
    if app.agent_busy && !app.conversation.messages.back().is_some_and(|m| m.streaming) {
        lines.push(Line::styled(" ● thinking…", Style::default().fg(Color::Yellow)));
    }
    f.render_widget(Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }), area);
    let mut state = ScrollbarState::default().position(offset).content_length(total);
    f.render_stateful_widget(Scrollbar::default().orientation(ScrollbarOrientation::VerticalRight).begin_symbol(Some("↑")).end_symbol(Some("↓")), area, &mut state);
}

fn render_input(f: &mut Frame<'_>, area: Rect, app: &App) {
    let block = Block::default().borders(Borders::TOP).title(" Prompt (Enter send · /help · /model · /exit)");
    let text = if app.input.is_empty() {
        Text::from(Line::from(Span::styled("Type your message…", Style::default().fg(Color::DarkGray))))
    } else { Text::from(Line::from(Span::raw(&app.input))) };
    f.render_widget(Paragraph::new(text).block(block), area);
}

fn render_status(f: &mut Frame<'_>, area: Rect, app: &App) {
    let info = &app.conversation.info;
    let ctx = if info.ctx_total > 0 { format!("ctx {:.0}%", info.ctx_pct) } else { String::new() };
    let cost = if info.cost > 0.0 { format!("${:.4}", info.cost) } else { String::new() };
    let right = [&ctx, &cost].into_iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join("  ");
    let right_w = right.len() + if right.is_empty() { 0 } else { 2 };

    let model_label = if info.provider != "—" { format!("{}/{}", info.provider, info.model) } else { info.model.clone() };
    let cwd_w = (area.width as usize).saturating_sub(model_label.len() + right_w + 4);
    let cwd = if app.cwd.len() > cwd_w && cwd_w > 5 { format!("…{}", &app.cwd[app.cwd.len().saturating_sub(cwd_w - 1)..]) } else { app.cwd.clone() };

    // Pad with spaces so right-side content is right-aligned
    let left = format!("{model_label}  ·  {cwd}");
    let pad = (area.width as usize).saturating_sub(left.len() + right.len());
    let line = format!("{left}{}{right}", " ".repeat(pad));

    f.render_widget(
        Paragraph::new(Text::from(Line::from(Span::styled(line, Style::default().fg(Color::DarkGray)))))
            .block(Block::default().borders(Borders::TOP).border_style(Style::default().fg(Color::DarkGray)))
            .style(Style::default().bg(Color::Rgb(20, 20, 28))),
        area,
    );
}

fn render_help(f: &mut Frame<'_>, area: Rect) {
    let w = area.width.saturating_sub(4).min(60);
    let h = 18u16;
    let x = (area.width - w) / 2;
    let y = (area.height - h) / 2;
    let lines = vec![
        Line::from(Span::styled(" Commands", Style::default().fg(Color::Cyan))),
        Line::from(Span::raw("")),
        Line::from(Span::styled("  /help       Show this help screen", Style::default().fg(Color::DarkGray))),
        Line::from(Span::styled("  /model      Open model picker", Style::default().fg(Color::DarkGray))),
        Line::from(Span::styled("  /exit       Quit", Style::default().fg(Color::DarkGray))),
        Line::from(Span::raw("")),
        Line::from(Span::styled(" Keybindings", Style::default().fg(Color::Cyan))),
        Line::from(Span::raw("")),
        Line::from(Span::styled("  ↑/↓         Scroll conversation", Style::default().fg(Color::DarkGray))),
        Line::from(Span::styled("  PgUp/PgDn   Page scroll", Style::default().fg(Color::DarkGray))),
        Line::from(Span::styled("  Home/End    Jump to top/bottom", Style::default().fg(Color::DarkGray))),
        Line::from(Span::styled("  Tab         Expand/collapse tool output", Style::default().fg(Color::DarkGray))),
        Line::from(Span::styled("  Esc         Close overlays", Style::default().fg(Color::DarkGray))),
        Line::from(Span::raw("")),
        Line::from(Span::styled(" Press Esc or Enter to close", Style::default().fg(Color::DarkGray))),
    ];
    f.render_widget(
        Paragraph::new(Text::from(lines)).block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::Cyan))),
        Rect::new(x, y, w, h),
    );
}

fn render_model_picker(f: &mut Frame<'_>, area: Rect, filter: &str, models: &[String], selected: usize) {
    let filtered: Vec<(usize, &String)> = models.iter().enumerate().filter(|(_, m)| m.contains(filter) || filter.is_empty()).collect();
    let view_h = ((area.height / 2).min(20) as usize).max(4);
    let list_h = view_h.saturating_sub(3); // header + filter + footer
    let picker_h = view_h as u16;
    let picker_w = area.width.saturating_sub(4).min(70);
    let x = (area.width - picker_w) / 2;
    let y = (area.height - picker_h) / 2;
    let picker_area = Rect::new(x, y, picker_w, picker_h);

    let sel = selected.min(filtered.len().saturating_sub(1));
    // Scroll offset: keep selected centered in the list viewport
    let scroll = if filtered.len() <= list_h { 0 }
        else { sel.saturating_sub(list_h / 2).min(filtered.len().saturating_sub(list_h)) };

    let mut lines = vec![
        Line::from(Span::styled(" Select Model", Style::default().fg(Color::Cyan).bg(Color::Rgb(20, 20, 28)))),
        Line::from(Span::styled(format!(" Filter: {filter}"), Style::default().fg(Color::DarkGray).bg(Color::Rgb(20, 20, 28)))),
    ];
    let visible_range = scroll..(scroll + list_h).min(filtered.len());
    for i in visible_range {
        let (_, model) = &filtered[i];
        let marker = if i == sel { " ▸" } else { "  " };
        let style = if i == sel { Style::default().fg(Color::Cyan).bg(Color::Rgb(40, 40, 60)) } else { Style::default().bg(Color::Rgb(20, 20, 28)) };
        let label = if model.len() > (picker_w as usize).saturating_sub(4) { format!("{}…", &model[..(picker_w as usize).saturating_sub(5)]) } else { model.to_string() };
        lines.push(Line::styled(format!("{marker} {label}"), style));
    }
    lines.push(Line::from(Span::styled(" Esc cancel · Enter select", Style::default().fg(Color::DarkGray).bg(Color::Rgb(20, 20, 28)))));
    f.render_widget(
        Paragraph::new(Text::from(lines)).block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::Cyan))),
        picker_area,
    );
}
