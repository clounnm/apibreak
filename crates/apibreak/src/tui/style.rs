//! The whole palette lives here: one accent plus status colors.

use ratatui::style::{Color, Modifier, Style};

pub const ACCENT: Color = Color::Cyan;
pub const MUTED: Color = Color::DarkGray;

pub fn accent() -> Style {
    Style::new().fg(ACCENT)
}

pub fn muted() -> Style {
    Style::new().fg(MUTED)
}

pub fn heading() -> Style {
    accent().add_modifier(Modifier::BOLD)
}

pub fn selected() -> Style {
    Style::new().bg(ACCENT).fg(Color::Black)
}

pub fn warning() -> Style {
    Style::new().fg(Color::Yellow)
}

pub fn error() -> Style {
    Style::new().fg(Color::Red)
}

pub fn border(focused: bool) -> Style {
    if focused {
        accent()
    } else {
        muted()
    }
}

pub fn status(code: u16) -> Style {
    let color = match code {
        200..=299 => Color::Green,
        300..=399 => Color::Blue,
        400..=499 => Color::Yellow,
        500..=599 => Color::Red,
        _ => Color::Reset,
    };
    Style::new().fg(color)
}
