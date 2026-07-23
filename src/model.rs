use crate::config::Theme;
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use std::collections::VecDeque;
use tui_markdown::StyleSheet;
use tui_markdown::{from_str_with_options, Options as MdOptions};

const COLLAPSED_MAX_LINES: usize = 5;

#[derive(Clone)]
pub struct ThemeStyleSheet {
    heading_fg: Color,
    link_fg: Color,
    blockquote_fg: Color,
    inline_code_fg: Color,
    inline_code_bg: Color,
    pub code_bg: Color,
}

impl ThemeStyleSheet {
    pub fn from_theme(theme: &Theme) -> Self {
        Self {
            heading_fg: theme.heading_fg,
            link_fg: theme.link_fg,
            blockquote_fg: theme.blockquote_fg,
            inline_code_fg: theme.inline_code_fg,
            inline_code_bg: theme.inline_code_bg,
            code_bg: theme.code_bg,
        }
    }
}

impl StyleSheet for ThemeStyleSheet {
    fn heading(&self, level: u8) -> Style {
        match level {
            1 => Style::new().fg(self.heading_fg).bold().underlined(),
            2 => Style::new().fg(self.heading_fg).bold(),
            3 => Style::new().fg(self.heading_fg).bold().italic(),
            4..=6 => Style::new().fg(self.heading_fg).italic(),
            _ => Style::new().fg(self.heading_fg).italic(),
        }
    }
    fn code(&self) -> Style {
        Style::new().fg(self.inline_code_fg).bg(self.inline_code_bg)
    }
    fn link(&self) -> Style {
        Style::new().fg(self.link_fg).underlined()
    }
    fn blockquote(&self) -> Style {
        Style::new().fg(self.blockquote_fg)
    }
    fn heading_meta(&self) -> Style {
        Style::new().dim()
    }
    fn metadata_block(&self) -> Style {
        Style::new().light_yellow()
    }
}

impl Conversation {
    pub fn set_theme(&mut self, theme: &Theme) {
        self.stylesheet = ThemeStyleSheet::from_theme(theme);
    }
}

#[derive(Debug, Clone)]
pub struct ToolCall {
    pub tool: String,
    pub tool_call_id: String,
    pub status: String,
    pub result: Option<String>,
    pub collapsed: bool,
}

pub struct Message {
    pub role: Role,
    pub text: String,
    pub thinking_text: String,
    pub streaming: bool,
    pub tool_calls: Vec<ToolCall>,
    rendered: Vec<Line<'static>>,
    thinking_rendered: Vec<Line<'static>>,
    rendered_tools: Vec<Line<'static>>,
    pub is_thinking: bool,
    pub time: String,
}

fn now_str() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

fn ts_or_now(iso: Option<&str>) -> String {
    match iso {
        Some(s) => chrono::DateTime::parse_from_rfc3339(s)
            .or_else(|_| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.fZ")
                .map(|d| d.and_utc().fixed_offset()))
            .map(|dt| dt.format("%H:%M:%S").to_string())
            .unwrap_or_else(|_| now_str()),
        None => now_str(),
    }
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
    thinking_msg_idx: Option<usize>,
    stylesheet: ThemeStyleSheet,
}

pub fn global_line_to_tool_call<'a>(messages: &'a VecDeque<Message>, line_idx: usize) -> Option<(usize, usize)> {
    let mut cur = 0usize;
    for (msg_idx, msg) in messages.iter().enumerate() {
        let end = cur + msg_line_count(msg);
        if line_idx < end {
            let in_msg = line_idx - cur;
            let header = if msg.role == Role::User { 2 } else { 1 };
            let tools_start = header + msg.thinking_rendered.len() + msg.rendered.len();
            if in_msg >= tools_start {
                let tool_offset = in_msg - tools_start;
                let mut running = 0;
                for (tidx, tc) in msg.tool_calls.iter().enumerate() {
                    let block = tool_block_line_count(tc);
                    if tool_offset < running + block {
                        return Some((msg_idx, tidx));
                    }
                    running += block;
                }
            }
            return None;
        }
        cur = end;
    }
    None
}

impl Conversation {
    pub fn new() -> Self {
        Self {
            messages: VecDeque::new(), total_lines: 0, info: SessionInfo::default(), error: None, thinking_msg_idx: None,
            stylesheet: ThemeStyleSheet::from_theme(&Theme::default()),
        }
    }

    pub fn add_user_message(&mut self, text: &str, created_at: Option<&str>) {
        let rendered = render_text_lines(text, &self.stylesheet);
        let lines = rendered.len() + 2;
        self.total_lines += lines;
        let time = ts_or_now(created_at);
        self.messages.push_back(Message {
            role: Role::User,
            text: text.to_string(),
            thinking_text: String::new(),
            streaming: false,
            tool_calls: vec![],
            rendered,
            thinking_rendered: vec![],
            rendered_tools: vec![],
            is_thinking: false,
            time,
        });
    }

    pub fn start_assistant_message(&mut self, created_at: Option<&str>) {
        let time = ts_or_now(created_at);
        self.messages.push_back(Message {
            role: Role::Assistant,
            text: String::new(),
            thinking_text: String::new(),
            streaming: true,
            tool_calls: vec![],
            rendered: vec![],
            thinking_rendered: vec![],
            rendered_tools: vec![],
            is_thinking: false,
            time,
        });
    }

    pub fn append_delta(&mut self, delta: &str, created_at: Option<&str>) {
        // If the last message is thinking, finish it and start a fresh response message
        if self.messages.back().is_some_and(|m| m.is_thinking) {
            self.finish_thinking();
        }
        if self.messages.is_empty() || self.messages.back().is_some_and(|m| m.role != Role::Assistant) {
            self.start_assistant_message(created_at);
        }
        let last = self.messages.len() - 1;
        let old_lines = self.messages[last].rendered.len();
        self.messages[last].text.push_str(delta);
        let full = self.messages[last].text.clone();
        self.messages[last].rendered = render_text_lines(&full, &self.stylesheet);
        let new_lines = self.messages[last].rendered.len();
        if new_lines > old_lines {
            self.total_lines += new_lines - old_lines;
        }
    }

    pub fn append_thinking(&mut self, delta: &str, created_at: Option<&str>) {
        if self.thinking_msg_idx.is_none() {
            self.start_assistant_message(created_at);
            self.thinking_msg_idx = Some(self.messages.len() - 1);
        }
        let last = self.thinking_msg_idx.unwrap();
        self.messages[last].is_thinking = true;
        self.messages[last].thinking_text.push_str(delta);
        let rendered = render_text_lines(delta, &self.stylesheet);
        self.total_lines += rendered.len();
        self.messages[last].thinking_rendered.extend(rendered);
    }

    pub fn finish_thinking(&mut self) {
        if let Some(idx) = self.thinking_msg_idx.take() {
            if let Some(msg) = self.messages.get_mut(idx) {
                msg.is_thinking = false;
            }
        }
    }

    pub fn add_tool_call(&mut self, tool: &str, tool_call_id: &str, status: &str) {
        self.assure_assistant();
        let last = self.messages.len() - 1;
        self.messages[last].tool_calls.push(ToolCall {
            tool: tool.to_string(),
            tool_call_id: tool_call_id.to_string(),
            status: status.to_string(),
            result: None,
            collapsed: true,
        });
        self.total_lines += 1;
        self.rebuild_tool_lines(last);
    }

    pub fn complete_tool_call(&mut self, tool_call_id: &str, result: &str) {
        let last = self.messages.len().saturating_sub(1);
        if let Some(tc) = self.messages[last].tool_calls.iter_mut().rev().find(|tc| tc.tool_call_id == tool_call_id) {
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
                let has_thinking = !msg.thinking_rendered.is_empty();
                if has_thinking {
                    for l in &msg.thinking_rendered {
                        out.push(Line::styled(l.to_string(), Style::default().fg(Color::DarkGray)));
                    }
                }
                for l in &msg.rendered {
                    out.push(l.clone());
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
            self.start_assistant_message(None);
        }
    }

    fn rebuild_tool_lines(&mut self, msg_idx: usize) {
        if let Some(msg) = self.messages.get_mut(msg_idx) {
            msg.rendered_tools = render_tool_lines(&msg.tool_calls, &msg.text);
        }
    }
}

fn render_text_lines(text: &str, ss: &ThemeStyleSheet) -> Vec<Line<'static>> {
    let md = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        from_str_with_options(text, &MdOptions::new(ss.clone()))
    }));
    let Ok(md_text) = md else {
        return vec![Line::from(ratatui::text::Span::raw(text.to_string()))];
    };
    let mut out = Vec::new();
    let mut in_code = false;
    for line in md_text.lines.iter() {
        let text_content: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        if text_content.starts_with("```") {
            in_code = !in_code;
        }
        let owned: Vec<_> = line.spans.iter().map(|s| {
            let content: String = s.content.chars().collect();
            ratatui::text::Span::styled(content, s.style)
        }).collect();
        let mut line = Line::from(owned);
        if in_code || text_content.starts_with("```") {
            line = line.patch_style(Style::default().bg(ss.code_bg));
        }
        out.push(line);
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

fn tool_block_line_count(tc: &ToolCall) -> usize {
    let mut n = 1;
    if let Some(result) = &tc.result {
        let lines = result.lines().count();
        if tc.collapsed {
            n += COLLAPSED_MAX_LINES.min(lines);
            if lines > COLLAPSED_MAX_LINES { n += 1; }
        } else {
            n += lines;
        }
    }
    n + 1
}

pub fn msg_line_count(msg: &Message) -> usize {
    let header = if msg.role == Role::User { 2 } else { 1 };
    header + msg.thinking_rendered.len() + msg.rendered.len() + msg.rendered_tools.len()
}
