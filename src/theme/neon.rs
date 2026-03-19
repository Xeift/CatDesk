use ratatui::style::Color;
use ratatui::widgets::BorderType;

use super::{Palette, ThemeDef};

pub const THEME: ThemeDef = ThemeDef {
    id: "neon",
    label: "neon",
    description: "Cyberpunk pink accents with neon highlights.",
    palette: Palette {
        header_fg: Color::LightMagenta,
        border_fg: Color::Magenta,
        border_type: BorderType::Rounded,
        title_fg: Color::LightMagenta,
        key_fg: Color::Magenta,
        primary_fg: Color::White,
        secondary_fg: Color::LightCyan,
        muted_fg: Color::DarkGray,
        success_fg: Color::LightMagenta,
        warning_fg: Color::LightCyan,
        danger_fg: Color::LightRed,
        info_fg: Color::LightMagenta,
        selection_bg: Color::Magenta,
        selection_fg: Color::White,
        toast_bg: Color::LightMagenta,
        toast_fg: Color::Black,
    },
};
