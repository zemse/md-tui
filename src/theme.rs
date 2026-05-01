use ratatui::style::{Color, Modifier, Style};

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct Theme {
    pub name: String,
    pub fg: Color,
    pub bg: Option<Color>,
    pub muted: Color,
    pub heading: [Color; 6],
    pub heading_modifier: Modifier,
    pub emphasis: Modifier,
    pub strong: Modifier,
    pub code_fg: Color,
    pub code_bg: Option<Color>,
    pub link: Color,
    pub link_focused: Color,
    pub link_modifier: Modifier,
    pub quote: Color,
    pub list_marker: Color,
    pub rule: Color,
    pub strikethrough: Modifier,
    pub status_fg: Color,
    pub status_bg: Color,
    pub syntect_theme: &'static str,
}

#[allow(dead_code)]
impl Theme {
    pub fn dark() -> Self {
        Theme {
            name: "dark".into(),
            fg: Color::Reset,
            bg: None,
            muted: Color::DarkGray,
            heading: [
                Color::LightMagenta,
                Color::LightCyan,
                Color::LightBlue,
                Color::LightYellow,
                Color::LightGreen,
                Color::Gray,
            ],
            heading_modifier: Modifier::BOLD,
            emphasis: Modifier::ITALIC,
            strong: Modifier::BOLD,
            code_fg: Color::Rgb(0xe6, 0xdb, 0x74),
            code_bg: Some(Color::Rgb(0x26, 0x26, 0x26)),
            link: Color::Cyan,
            link_focused: Color::LightYellow,
            link_modifier: Modifier::UNDERLINED,
            quote: Color::Gray,
            list_marker: Color::LightBlue,
            rule: Color::DarkGray,
            strikethrough: Modifier::CROSSED_OUT,
            status_fg: Color::Black,
            status_bg: Color::LightBlue,
            syntect_theme: "base16-ocean.dark",
        }
    }

    pub fn light() -> Self {
        Theme {
            name: "light".into(),
            fg: Color::Reset,
            bg: None,
            muted: Color::Gray,
            heading: [
                Color::Magenta,
                Color::Blue,
                Color::Cyan,
                Color::Rgb(0x98, 0x71, 0x00),
                Color::Green,
                Color::DarkGray,
            ],
            heading_modifier: Modifier::BOLD,
            emphasis: Modifier::ITALIC,
            strong: Modifier::BOLD,
            code_fg: Color::Rgb(0x88, 0x55, 0x00),
            code_bg: Some(Color::Rgb(0xee, 0xee, 0xee)),
            link: Color::Blue,
            link_focused: Color::Rgb(0xc8, 0x66, 0x00),
            link_modifier: Modifier::UNDERLINED,
            quote: Color::DarkGray,
            list_marker: Color::Blue,
            rule: Color::Gray,
            strikethrough: Modifier::CROSSED_OUT,
            status_fg: Color::White,
            status_bg: Color::Blue,
            syntect_theme: "InspiredGitHub",
        }
    }

    pub fn style_text(&self) -> Style {
        Style::default().fg(self.fg)
    }
}

pub fn resolve(name: &str, cfg: &crate::config::Config) -> Theme {
    let n = if name == "auto" { cfg.theme.as_deref().unwrap_or("auto") } else { name };
    match n {
        "light" => Theme::light(),
        "dark" => Theme::dark(),
        _ => detect_terminal_theme(),
    }
}

fn detect_terminal_theme() -> Theme {
    if let Ok(v) = std::env::var("COLORFGBG") {
        // Convention: "<fg>;<bg>" — bg 7-15 light, 0-6 dark
        if let Some(bg) = v.split(';').last().and_then(|s| s.parse::<u8>().ok()) {
            if bg >= 7 { return Theme::light(); }
        }
    }
    Theme::dark()
}
