use gpui::WindowAppearance;
use hane_session::ThemePreference;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Theme {
    pub line_height: f32,
    pub line_horizontal_padding: f32,
    pub header_height: f32,
    pub overscan: f32,
    pub editor_background: u32,
    pub foreground: u32,
    pub selection_background: u32,
    pub header_background: u32,
    pub header_foreground: u32,
    pub code_background: u32,
    pub code_block_background: u32,
    pub link_foreground: u32,
    pub quote_foreground: u32,
    pub media_background: u32,
    pub table_background: u32,
}

pub(crate) const DEFAULT_THEME: Theme = Theme {
    line_height: 26.0,
    line_horizontal_padding: 12.0,
    header_height: 68.0,
    overscan: 260.0,
    editor_background: 0xfaf9f7,
    foreground: 0x262626,
    selection_background: 0xe8eefc,
    header_background: 0x242424,
    header_foreground: 0xf5f5f5,
    code_background: 0xeeeae4,
    code_block_background: 0xf1eee9,
    link_foreground: 0x2867a9,
    quote_foreground: 0x6b6259,
    media_background: 0xf3f0eb,
    table_background: 0xf5f2ed,
};

pub(crate) const DARK_THEME: Theme = Theme {
    line_height: 26.0,
    line_horizontal_padding: 12.0,
    header_height: 68.0,
    overscan: 260.0,
    editor_background: 0x1f2022,
    foreground: 0xe8e5df,
    selection_background: 0x34435f,
    header_background: 0x151618,
    header_foreground: 0xf5f5f5,
    code_background: 0x333438,
    code_block_background: 0x292a2e,
    link_foreground: 0x79b8ff,
    quote_foreground: 0xaaa39a,
    media_background: 0x292a2d,
    table_background: 0x27282b,
};

pub(crate) fn resolve_theme(preference: ThemePreference, appearance: WindowAppearance) -> Theme {
    match preference {
        ThemePreference::Light => DEFAULT_THEME,
        ThemePreference::Dark => DARK_THEME,
        ThemePreference::System => match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => DARK_THEME,
            WindowAppearance::Light | WindowAppearance::VibrantLight => DEFAULT_THEME,
        },
    }
}
