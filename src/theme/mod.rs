mod concise;
mod neon;

use ratatui::style::Color;
use ratatui::widgets::BorderType;

#[derive(Clone, Copy)]
pub struct Palette {
    pub header_fg: Color,
    pub border_fg: Color,
    pub border_type: BorderType,
    pub title_fg: Color,
    pub key_fg: Color,
    pub primary_fg: Color,
    pub secondary_fg: Color,
    pub muted_fg: Color,
    pub success_fg: Color,
    pub warning_fg: Color,
    pub danger_fg: Color,
    pub info_fg: Color,
    pub selection_bg: Color,
    pub selection_fg: Color,
    pub toast_bg: Color,
    pub toast_fg: Color,
}

#[derive(Clone, Copy)]
pub struct ThemeDef {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub palette: Palette,
}

pub const DEFAULT_THEME_ID: &str = concise::THEME.id;

const THEMES: [ThemeDef; 2] = [concise::THEME, neon::THEME];

pub fn all() -> &'static [ThemeDef] {
    &THEMES
}

pub fn get(id: &str) -> Option<&'static ThemeDef> {
    THEMES.iter().find(|theme| theme.id == id)
}

pub fn resolve(id: &str) -> &'static ThemeDef {
    get(id).unwrap_or(&THEMES[0])
}
