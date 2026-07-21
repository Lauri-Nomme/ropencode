use crate::acp::Event;
use crate::model::Conversation;
use anyhow::Result;
use crossterm::event::{self, Event as TermEvent, KeyCode, KeyEventKind, MouseEventKind};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap};
use ratatui::{Frame, Terminal};
use std::io;
use std::time::Duration;
use tokio::sync::mpsc;

const STATUS_HEIGHT: usize = 2;

struct App {
    conversation: Conversation,
    scroll_offset: usize,
    sticky_bottom: bool,
    agent_busy: bool,
    input: String,
    viewport_height: usize,
    cached_lines: Vec<Line<'static>>,
    content_version: u64,
    last_rendered_version: u64,
    cwd: String,
}

impl App {
    fn new(cwd: String) -> Self {
        Self {
            conversation: Conversation::new(),
            scroll_offset: 0,
            sticky_bottom: true,
            agent_busy: false,
            input: String::new(),
            viewport_height: 0,
            cached_lines: Vec::new(),
            content_version: 0,
            last_rendered_version: 0,
            cwd,
        }
    }

    fn rebuild_cache(&mut self) {
        self.cached_lines = self.conversation.rendered_lines();
        self.last_rendered_version = self.content_version;
    }

    fn ensure_cache(&mut self) {
        if self.last_rendered_version != self.content_version {
            self.rebuild_cache();
        }
    }

    fn content_height(&self) -> usize { self.conversation.total_lines.max(self.cached_lines.len()) }

    fn max_scroll(&self) -> usize {
        self.content_height().saturating_sub(self.viewport_height.max(1))
    }

    fn is_at_bottom(&self) -> bool { self.scroll_offset >= self.max_scroll() }
    fn clamp_offset(&mut self) { self.scroll_offset = self.scroll_offset.min(self.max_scroll()); }
    fn mark_dirty(&mut self) { self.content_version += 1; }

    fn auto_scroll(&mut self) {
        self.rebuild_cache();
        if self.sticky_bottom {
            self.scroll_offset = self.max_scroll();
        }
    }

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::AgentTextChunk { text, .. } => {
                self.agent_busy = true;
                self.conversation.append_delta(&text);
                self.mark_dirty();
                if self.sticky_bottom { self.rebuild_cache(); self.scroll_offset = self.max_scroll(); }
            }
            Event::AgentThoughtChunk { text, .. } => {
                self.agent_busy = true;
                self.conversation.append_thinking(&text);
                self.mark_dirty();
                self.auto_scroll();
            }
            Event::AgentTextDone { .. } => {
                self.conversation.finish_thinking();
                self.mark_dirty();
            }
            Event::UserMessage { text, .. } => {
                self.conversation.add_user_message(&text);
                self.mark_dirty();
                self.sticky_bottom = true;
                self.auto_scroll();
            }
            Event::ToolCallUpdate { tool, status, .. } => {
                self.conversation.add_tool_call(&tool, &status);
                self.mark_dirty();
                self.auto_scroll();
            }
            Event::ToolResult { tool, result, .. } => {
                self.conversation.complete_tool_call(&tool, &result);
                self.mark_dirty();
                self.auto_scroll();
            }
            Event::UsageUpdate { ctx_pct, ctx_total, cost } => {
                self.conversation.info.ctx_pct = ctx_pct;
                self.conversation.info.ctx_total = ctx_total;
                self.conversation.info.cost = cost;
            }
            Event::ConfigUpdate { model, provider } => {
                if let Some(m) = model { self.conversation.info.model = m; }
                if let Some(p) = provider { self.conversation.info.provider = p; }
            }
            Event::SessionCreated { .. } => {}
            Event::Error(msg) => { eprintln!("ACP error: {msg}"); }
        }
        self.clamp_offset();
    }

    fn did_scroll_up(&mut self) { self.sticky_bottom = false; }
    fn check_sticky(&mut self) { if self.is_at_bottom() { self.sticky_bottom = true; } }
}

pub async fn run(event_rx: mpsc::UnboundedReceiver<Event>, cwd: String) -> Result<()> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;

    let mut app = App::new(cwd);
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
            evt = event_rx.recv() => {
                match evt { Some(evt) => app.handle_event(evt), None => break }
            }
            _ = &mut timeout => {
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
    match evt {
        TermEvent::Key(key) if key.kind == KeyEventKind::Press => match key.code {
            KeyCode::Char('q') => return true,
            KeyCode::Enter => {
                let text = app.input.trim().to_string();
                if !text.is_empty() {
                    app.conversation.add_user_message(&text);
                    app.mark_dirty();
                    app.input.clear();
                    app.sticky_bottom = true;
                    app.rebuild_cache();
                    app.scroll_offset = app.max_scroll();
                }
                false
            }
            KeyCode::Backspace => { app.input.pop(); false }
            KeyCode::Tab => {
                // Expand/collapse the last tool output
                if let Some(msg) = app.conversation.messages.iter_mut().rev().find(|m| !m.tool_calls.is_empty()) {
                    let idx = msg.tool_calls.len() - 1;
                    msg.tool_calls[idx].collapsed = !msg.tool_calls[idx].collapsed;
                    app.mark_dirty();
                    app.rebuild_cache();
                }
                false
            }
            KeyCode::Up => { if app.scroll_offset > 0 { app.scroll_offset -= 1; app.did_scroll_up(); } false }
            KeyCode::Down => { let max = app.max_scroll(); if app.scroll_offset < max { app.scroll_offset += 1; app.check_sticky(); } false }
            KeyCode::PageUp => { let vh = app.viewport_height.max(1); app.scroll_offset = app.scroll_offset.saturating_sub(vh); app.did_scroll_up(); false }
            KeyCode::PageDown => { let vh = app.viewport_height.max(1); let max = app.max_scroll(); app.scroll_offset = (app.scroll_offset + vh).min(max); app.check_sticky(); false }
            KeyCode::Home => { app.scroll_offset = 0; app.did_scroll_up(); false }
            KeyCode::End => { app.scroll_offset = app.max_scroll(); app.sticky_bottom = true; false }
            KeyCode::Char(ch) => { app.input.push(ch); false }
            _ => false,
        },
        TermEvent::Mouse(mev) => match mev.kind {
            MouseEventKind::ScrollDown => { let max = app.max_scroll(); app.scroll_offset = (app.scroll_offset + 3).min(max); app.check_sticky(); false }
            MouseEventKind::ScrollUp => { app.scroll_offset = app.scroll_offset.saturating_sub(3); app.did_scroll_up(); false }
            _ => false,
        },
        TermEvent::Resize(_, h) => {
            let total = h as usize;
            let convo_h = total.saturating_sub(STATUS_HEIGHT + 3);
            app.viewport_height = convo_h;
            app.clamp_offset();
            if app.sticky_bottom { app.scroll_offset = app.max_scroll(); }
            false
        }
        _ => false,
    }
}

fn render(f: &mut Frame<'_>, app: &mut App) {
    let area = f.area();
    if area.width == 0 || area.height == 0 { return; }

    let total_h = area.height as usize;
    let convo_h = total_h.saturating_sub(STATUS_HEIGHT + 3);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(convo_h as u16),
            Constraint::Length(3),
            Constraint::Length(STATUS_HEIGHT as u16),
        ])
        .split(area);

    let convo_area = chunks[0];
    let input_area = chunks[1];
    let status_area = chunks[2];

    app.viewport_height = convo_area.height as usize;
    app.ensure_cache();
    app.clamp_offset();

    render_conversation(f, convo_area, app);
    render_input(f, input_area, app);
    render_status(f, status_area, app);
}

fn render_conversation(f: &mut Frame<'_>, area: Rect, app: &App) {
    let total = app.cached_lines.len();
    let offset = app.scroll_offset.min(total.saturating_sub(1));
    let vh = area.height as usize;

    let start = offset;
    let end = (start + vh).min(total);
    let mut lines: Vec<Line<'static>> = if start < total {
        app.cached_lines[start..end].to_vec()
    } else {
        vec![]
    };

    if app.agent_busy && !app.conversation.messages.back().is_some_and(|m| m.streaming) {
        lines.push(Line::styled(" ● thinking…", Style::default().fg(Color::Yellow)));
    }

    f.render_widget(
        Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
        area,
    );

    let scrollbar = Scrollbar::default()
        .orientation(ScrollbarOrientation::VerticalRight)
        .begin_symbol(Some("↑"))
        .end_symbol(Some("↓"));
    let mut state = ScrollbarState::default().position(offset).content_length(total);
    f.render_stateful_widget(scrollbar, area, &mut state);
}

fn render_input(f: &mut Frame<'_>, area: Rect, app: &App) {
    let block = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::TOP)
        .title(" Prompt (Enter to send, Tab expand tool, q quit)");
    let text = if app.input.is_empty() {
        Text::from(Line::from(Span::styled("Type your message…", Style::default().fg(Color::DarkGray))))
    } else {
        Text::from(Line::from(Span::raw(&app.input)))
    };
    f.render_widget(Paragraph::new(text).block(block), area);
}

fn render_status(f: &mut Frame<'_>, area: Rect, app: &App) {
    let info = &app.conversation.info;
    let ctx = if info.ctx_total > 0 {
        format!("ctx {:.0}%", info.ctx_pct)
    } else {
        String::new()
    };
    let cost = if info.cost > 0.0 {
        format!("${:.4}", info.cost)
    } else {
        String::new()
    };

    let model_label = if info.provider != "—" {
        format!("{}/{}", info.provider, info.model)
    } else {
        info.model.clone()
    };

    // Truncate cwd to fit
    let cwd_w = (area.width as usize).saturating_sub(model_label.len() + ctx.len() + cost.len() + 6);
    let cwd = if app.cwd.len() > cwd_w && cwd_w > 5 {
        format!("…{}", &app.cwd[app.cwd.len().saturating_sub(cwd_w - 1)..])
    } else {
        app.cwd.clone()
    };

    let mut parts = vec![model_label, cwd];
    if !ctx.is_empty() { parts.push(ctx); }
    if !cost.is_empty() { parts.push(cost); }

    let text = parts.join("  ·  ");
    let block = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::TOP)
        .border_style(Style::default().fg(Color::DarkGray));
    f.render_widget(
        Paragraph::new(Text::from(Line::from(Span::styled(text, Style::default().fg(Color::DarkGray)))))
            .block(block)
            .style(Style::default().bg(Color::Rgb(20, 20, 28))),
        area,
    );
}
