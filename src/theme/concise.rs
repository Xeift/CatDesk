use ratatui::style::Color;

use super::{Palette, ThemeDef};

pub const THEME: ThemeDef = ThemeDef {
    id: "concise",
    label: "concise",
    description: "Black/gray/white minimal UI with low color usage.",
    palette: Palette {
        header_fg: Color::White,
        border_fg: Color::DarkGray,
        title_fg: Color::Gray,
        key_fg: Color::Gray,
        primary_fg: Color::White,
        secondary_fg: Color::Gray,
        muted_fg: Color::DarkGray,
        success_fg: Color::White,
        warning_fg: Color::Gray,
        danger_fg: Color::Gray,
        info_fg: Color::White,
        selection_bg: Color::DarkGray,
        selection_fg: Color::White,
        toast_bg: Color::White,
        toast_fg: Color::Black,
    },
};
