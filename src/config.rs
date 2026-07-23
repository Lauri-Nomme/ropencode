use ratatui::style::Color;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub theme: Theme,
    #[serde(default)]
    pub defaults: Defaults,
}

#[derive(Debug, Clone)]
pub struct Theme {
    pub status_bar_bg: Color,
    pub user_color: Color,
    pub assistant_color: Color,
    pub error_color: Color,
    pub thinking_color: Color,
    pub accent_color: Color,
    pub selection_bg: Color,
    pub heading_fg: Color,
    pub link_fg: Color,
    pub blockquote_fg: Color,
    pub inline_code_fg: Color,
    pub inline_code_bg: Color,
    pub code_bg: Color,
}

#[derive(Debug, Default, Deserialize)]
pub struct Defaults {
    pub model: Option<String>,
    pub cwd: Option<String>,
}

impl<'de> Deserialize<'de> for Theme {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawTheme {
            #[serde(default = "default_hex")]
            status_bar_bg: String,
            #[serde(default = "default_hex")]
            user_color: String,
            #[serde(default = "default_hex")]
            assistant_color: String,
            #[serde(default = "default_hex")]
            error_color: String,
            #[serde(default = "default_hex")]
            thinking_color: String,
            #[serde(default = "default_hex")]
            accent_color: String,
            #[serde(default = "default_hex")]
            heading_fg: String,
            #[serde(default = "default_hex")]
            link_fg: String,
            #[serde(default = "default_hex")]
            blockquote_fg: String,
            #[serde(default = "default_hex")]
            inline_code_fg: String,
            #[serde(default = "default_hex")]
            inline_code_bg: String,
            #[serde(default = "default_hex")]
            code_bg: String,
        }
        let raw = RawTheme::deserialize(deserializer)?;
        Ok(Theme {
            status_bar_bg: parse_hex(&raw.status_bar_bg),
            user_color: parse_hex(&raw.user_color),
            assistant_color: parse_hex(&raw.assistant_color),
            error_color: parse_hex(&raw.error_color),
            thinking_color: parse_hex(&raw.thinking_color),
            accent_color: parse_hex(&raw.accent_color),
            selection_bg: Color::Rgb(40, 40, 60),
            heading_fg: parse_hex(&raw.heading_fg),
            link_fg: parse_hex(&raw.link_fg),
            blockquote_fg: parse_hex(&raw.blockquote_fg),
            inline_code_fg: parse_hex(&raw.inline_code_fg),
            inline_code_bg: parse_hex(&raw.inline_code_bg),
            code_bg: parse_hex(&raw.code_bg),
        })
    }
}

impl Default for Theme {
    fn default() -> Self {
        Theme {
            status_bar_bg: Color::Rgb(20, 20, 28),
            user_color: Color::Magenta,
            assistant_color: Color::White,
            error_color: Color::Red,
            thinking_color: Color::DarkGray,
            accent_color: Color::Cyan,
            selection_bg: Color::Rgb(40, 40, 60),
            heading_fg: Color::Cyan,
            link_fg: Color::Rgb(80, 160, 255),
            blockquote_fg: Color::Green,
            inline_code_fg: Color::White,
            inline_code_bg: Color::Rgb(40, 40, 52),
            code_bg: Color::Rgb(25, 25, 35),
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let path = config_path();
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return Config::default(),
        };
        toml::from_str(&content).unwrap_or_else(|e| {
            eprintln!("ropencode: failed to parse config at {}: {e}", path.display());
            Config::default()
        })
    }
}

impl Default for Config {
    fn default() -> Self {
        Self { theme: Theme::default(), defaults: Defaults::default() }
    }
}

fn config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let mut p = PathBuf::from(home);
    p.push(".config");
    p.push("ropencode");
    p.push("config.toml");
    p
}

fn default_hex() -> String { String::new() }

pub fn parse_hex(s: &str) -> Color {
    let s = s.trim_start_matches('#');
    if let Ok(v) = u32::from_str_radix(s, 16) {
        let r = ((v >> 16) & 0xFF) as u8;
        let g = ((v >> 8) & 0xFF) as u8;
        let b = (v & 0xFF) as u8;
        Color::Rgb(r, g, b)
    } else {
        Color::White
    }
}
