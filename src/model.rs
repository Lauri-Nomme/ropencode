use ratatui::style::{Color, Style};
use ratatui::text::Line;
use std::collections::VecDeque;

const COLLAPSED_MAX_LINES: usize = 5;

#[derive(Debug, Clone)]
pub struct ToolCall {
    pub tool: String,
    pub status: String,
    pub result: Option<String>,
    pub collapsed: bool,
}

pub struct Message {
    pub role: Role,
    pub text: String,
    pub streaming: bool,
    pub tool_calls: Vec<ToolCall>,
    rendered: Vec<Line<'static>>,
    rendered_tools: Vec<Line<'static>>,
    pub is_thinking: bool,
    pub time: String,
}

fn now_str() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

#[derive(Debug, Clone, PartialEq)]
pub enum Role {
    User,
    Assistant,
}

pub struct SessionInfo {
    pub model: String,
    pub provider: String,
    pub cwd: String,
    pub ctx_pct: f64,
    pub ctx_total: u64,
    pub cost: f64,
}

impl Default for SessionInfo {
    fn default() -> Self {
        Self { model: "—".into(), provider: "—".into(), cwd: String::new(), ctx_pct: 0.0, ctx_total: 0, cost: 0.0 }
    }
}

pub struct Conversation {
    pub messages: VecDeque<Message>,
    pub total_lines: usize,
    pub info: SessionInfo,
    pub error: Option<String>,
}

impl Conversation {
    pub fn new() -> Self {
        Self { messages: VecDeque::new(), total_lines: 0, info: SessionInfo::default(), error: None }
    }

    pub fn add_user_message(&mut self, text: &str) {
        let rendered = render_text_lines(text);
        let lines = rendered.len() + 2;
        self.total_lines += lines;
        self.messages.push_back(Message {
            role: Role::User,
            text: text.to_string(),
            streaming: false,
            tool_calls: vec![],
            rendered,
            rendered_tools: vec![],
            is_thinking: false,
            time: now_str(),
        });
    }

    pub fn start_assistant_message(&mut self) {
        self.messages.push_back(Message {
            role: Role::Assistant,
            text: String::new(),
            streaming: true,
            tool_calls: vec![],
            rendered: vec![],
            rendered_tools: vec![],
            is_thinking: false,
            time: now_str(),
        });
    }

    pub fn append_delta(&mut self, delta: &str) {
        // If the last message is thinking, finish it and start a fresh response message
        if self.messages.back().is_some_and(|m| m.is_thinking) {
            self.finish_thinking();
        }
        if self.messages.is_empty() || self.messages.back().is_some_and(|m| m.role != Role::Assistant) {
            self.start_assistant_message();
        }
        let last = self.messages.len() - 1;
        let old_lines = self.messages[last].rendered.len();
        self.messages[last].text.push_str(delta);
        let full = self.messages[last].text.clone();
        self.messages[last].rendered = render_text_lines(&full);
        let new_lines = self.messages[last].rendered.len();
        if new_lines > old_lines {
            self.total_lines += new_lines - old_lines;
        }
    }

    pub fn append_thinking(&mut self, delta: &str) {
        if self.messages.is_empty() || self.messages.back().is_some_and(|m| m.role != Role::Assistant && !m.is_thinking) {
            self.start_assistant_message();
        }
        let last = self.messages.len() - 1;
        let old_lines = self.messages[last].rendered.len();
        // Don't push_str for thinking — it's not part of the visible response text
        // self.messages[last].text.push_str(delta);
        self.messages[last].is_thinking = true;
        let rendered = render_text_lines(delta);
        self.messages[last].rendered.extend(rendered.clone());
        self.total_lines += rendered.len();
    }

    pub fn finish_thinking(&mut self) {
        if let Some(msg) = self.messages.iter_mut().rev().find(|m| m.is_thinking) {
            msg.is_thinking = false;
        }
    }

    pub fn add_tool_call(&mut self, tool: &str, status: &str) {
        self.assure_assistant();
        let last = self.messages.len() - 1;
        self.messages[last].tool_calls.push(ToolCall {
            tool: tool.to_string(),
            status: status.to_string(),
            result: None,
            collapsed: true,
        });
        self.total_lines += 1;
        self.rebuild_tool_lines(last);
    }

    pub fn complete_tool_call(&mut self, tool: &str, result: &str) {
        let last = self.messages.len().saturating_sub(1);
        if let Some(tc) = self.messages[last].tool_calls.iter_mut().rev().find(|tc| tc.tool == tool) {
            let old_lines = count_lines(&tc.result);
            tc.status = "completed".into();
            tc.result = Some(result.to_string());
            let new_lines = count_lines(&Some(result.to_string()));
            self.total_lines = self.total_lines.saturating_sub(old_lines).saturating_add(
                if tc.collapsed { new_lines.min(COLLAPSED_MAX_LINES) } else { new_lines }
            );
        }
        self.rebuild_tool_lines(last);
    }

    pub fn toggle_tool_expand(&mut self, msg_idx: usize, tool_idx: usize) {
        if let Some(msg) = self.messages.get_mut(msg_idx) {
            if let Some(tc) = msg.tool_calls.get_mut(tool_idx) {
                let was_collapsed = tc.collapsed;
                tc.collapsed = !tc.collapsed;
                let result_lines = count_lines(&tc.result);
                let prev = if was_collapsed { COLLAPSED_MAX_LINES.min(result_lines) } else { result_lines };
                let now = if tc.collapsed { COLLAPSED_MAX_LINES.min(result_lines) } else { result_lines };
                self.total_lines = self.total_lines.saturating_sub(prev).saturating_add(now);
                self.rebuild_tool_lines(msg_idx);
            }
        }
    }

    pub fn rendered_lines(&self) -> Vec<Line<'static>> {
        let mut out = Vec::with_capacity(self.total_lines);
        for msg in &self.messages {
            let style = if msg.is_thinking {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default()
            };
            if msg.role == Role::User {
                out.push(Line::from(""));
                out.push(Line::styled(format!(" ┃ [{}]", msg.time), Style::default().fg(Color::Magenta)));
                for l in &msg.rendered {
                    let mut spans = vec![ratatui::text::Span::styled(" ┃ ", Style::default().fg(Color::Magenta))];
                    spans.extend(l.spans.iter().map(|s| {
                        ratatui::text::Span::styled(s.content.clone(), Style::default().fg(Color::Magenta))
                    }));
                    out.push(Line::from(spans));
                }
            } else {
                let label = if msg.streaming { format!(" Assistant (streaming…) [{}]", msg.time) } else { format!(" Assistant [{}]", msg.time) };
                out.push(Line::styled(label, style));
                for l in &msg.rendered {
                    let line = if msg.is_thinking {
                        Line::styled(l.to_string(), Style::default().fg(Color::DarkGray))
                    } else {
                        l.clone()
                    };
                    out.push(line);
                }
            }
            for l in &msg.rendered_tools {
                out.push(l.clone());
            }
        }
        if let Some(err) = &self.error {
            out.push(Line::styled(format!("  ✕ {err}"), Style::default().fg(Color::Red)));
            out.push(Line::styled("", Style::default().fg(Color::Red)));
        }
        out
    }

    fn assure_assistant(&mut self) {
        if self.messages.is_empty() || self.messages.back().is_some_and(|m| m.role != Role::Assistant) {
            self.start_assistant_message();
        }
    }

    fn rebuild_tool_lines(&mut self, msg_idx: usize) {
        if let Some(msg) = self.messages.get_mut(msg_idx) {
            msg.rendered_tools = render_tool_lines(&msg.tool_calls, &msg.text);
        }
    }
}

fn render_text_lines(text: &str) -> Vec<Line<'static>> {
    let text = tui_markdown::from_str(text);
    let mut out = Vec::new();
    for line in text.lines.iter() {
        let owned: Vec<_> = line.spans.iter().map(|s| {
            let content: String = s.content.chars().collect();
            ratatui::text::Span::styled(content, s.style)
        }).collect();
        out.push(Line::from(owned));
    }
    out
}

fn render_tool_lines(tools: &[ToolCall], _msg_text: &str) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let bg = Color::Rgb(30, 30, 40);
    for tc in tools {
        let status_color = match tc.status.as_str() {
            "completed" => Color::Green,
            "running" | "in_progress" => Color::Yellow,
            "error" => Color::Red,
            _ => Color::White,
        };
        let collapse_hint = if tc.collapsed { " [+]" } else { " [-]" };
        out.push(Line::styled(
            format!("  🛠 {}{}{}", tc.tool, collapse_hint, if tc.status == "completed" { " ✓" } else { "" }),
            Style::default().fg(status_color).bg(bg),
        ));
        if let Some(result) = &tc.result {
            let lines: Vec<&str> = result.lines().collect();
            let show = if tc.collapsed { &lines[..lines.len().min(COLLAPSED_MAX_LINES)] } else { &lines[..] };
            for &r_line in show {
                out.push(Line::styled(format!("    {r_line}"), Style::default().bg(bg)));
            }
            if tc.collapsed && lines.len() > COLLAPSED_MAX_LINES {
                out.push(Line::styled(
                    format!("    … {} more lines (click/hotkey to expand)", lines.len() - COLLAPSED_MAX_LINES),
                    Style::default().fg(Color::DarkGray).bg(bg),
                ));
            }
        }
        out.push(Line::styled("", Style::default().bg(bg)));
    }
    out
}

fn count_lines(s: &Option<String>) -> usize {
    s.as_ref().map_or(0, |s| s.lines().count())
}
