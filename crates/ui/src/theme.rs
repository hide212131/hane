#![allow(
    clippy::unreadable_literal,
    reason = "ARGB literals intentionally retain their contiguous channel representation"
)]

use gpui::{App, Window, WindowAppearance};
use gpui_component::Theme as ComponentTheme;
use gpui_component::ThemeMode;
use hane_session::ThemePreference;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Theme {
    pub line_height: f32,
    pub line_horizontal_padding: f32,
    pub header_height: f32,
    pub footer_height: f32,
    pub overscan: f32,
    pub editor_background: u32,
    pub foreground: u32,
    pub selection_background: u32,
    pub header_background: u32,
    pub header_foreground: u32,
    pub tab_active_background: u32,
    pub tab_active_foreground: u32,
    pub code_background: u32,
    pub code_block_background: u32,
    pub link_foreground: u32,
    pub quote_foreground: u32,
    pub media_background: u32,
    pub table_background: u32,
    pub table_header_background: u32,
    pub table_border: u32,
    pub sidebar_width: f32,
    pub sidebar_background: u32,
    pub sidebar_foreground: u32,
    pub sidebar_active_background: u32,
}

pub(crate) const DEFAULT_THEME: Theme = Theme {
    line_height: 26.0,
    line_horizontal_padding: 12.0,
    header_height: 40.0,
    footer_height: 54.0,
    overscan: 260.0,
    editor_background: 0xfaf9f7,
    foreground: 0x262626,
    selection_background: 0xe8eefc,
    header_background: 0x242424,
    header_foreground: 0xf5f5f5,
    tab_active_background: 0x356d94,
    tab_active_foreground: 0xf7fbff,
    code_background: 0xeeeae4,
    code_block_background: 0xf1eee9,
    link_foreground: 0x2867a9,
    quote_foreground: 0x6b6259,
    media_background: 0xf3f0eb,
    table_background: 0xf5f2ed,
    table_header_background: 0xe9e4dc,
    table_border: 0xd2cbc0,
    sidebar_width: 220.0,
    sidebar_background: 0xf0ede7,
    sidebar_foreground: 0x262626,
    sidebar_active_background: 0xe0dcd3,
};

pub(crate) const DARK_THEME: Theme = Theme {
    line_height: 26.0,
    line_horizontal_padding: 12.0,
    header_height: 40.0,
    footer_height: 54.0,
    overscan: 260.0,
    editor_background: 0x1f2022,
    foreground: 0xe8e5df,
    selection_background: 0x34435f,
    header_background: 0x151618,
    header_foreground: 0xf5f5f5,
    tab_active_background: 0x44779e,
    tab_active_foreground: 0xf7fbff,
    code_background: 0x333438,
    code_block_background: 0x292a2e,
    link_foreground: 0x79b8ff,
    quote_foreground: 0xaaa39a,
    media_background: 0x292a2d,
    table_background: 0x27282b,
    table_header_background: 0x34363a,
    table_border: 0x55585f,
    sidebar_width: 220.0,
    sidebar_background: 0x18191b,
    sidebar_foreground: 0xe8e5df,
    sidebar_active_background: 0x2c2d30,
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

/// Whether `resolve_theme` would pick Hane's dark palette for this
/// preference/appearance pair.
fn resolves_dark(preference: ThemePreference, appearance: WindowAppearance) -> bool {
    match preference {
        ThemePreference::Light => false,
        ThemePreference::Dark => true,
        ThemePreference::System => matches!(
            appearance,
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        ),
    }
}

/// Keeps `gpui_component`'s own Theme global (which colors the internals of
/// components like the selected file tab, independently of Hane's `Theme`)
/// in lockstep with the light/dark/system choice resolved above. Without
/// this, the Component Theme stays on the Light mode `gpui_component::init`
/// starts with even after Hane switches to dark.
pub(crate) fn sync_component_theme(
    preference: ThemePreference,
    appearance: WindowAppearance,
    window: &mut Window,
    cx: &mut App,
) {
    let mode = if resolves_dark(preference, appearance) {
        ThemeMode::Dark
    } else {
        ThemeMode::Light
    };
    ComponentTheme::change(mode, window, cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    const APPEARANCES: [WindowAppearance; 4] = [
        WindowAppearance::Light,
        WindowAppearance::VibrantLight,
        WindowAppearance::Dark,
        WindowAppearance::VibrantDark,
    ];
    const PREFERENCES: [ThemePreference; 3] = [
        ThemePreference::Light,
        ThemePreference::Dark,
        ThemePreference::System,
    ];

    #[test]
    fn resolves_dark_matches_explicit_preferences() {
        assert!(!resolves_dark(ThemePreference::Light, WindowAppearance::Dark));
        assert!(resolves_dark(ThemePreference::Dark, WindowAppearance::Light));
    }

    #[test]
    fn resolves_dark_follows_system_appearance() {
        assert!(resolves_dark(
            ThemePreference::System,
            WindowAppearance::Dark
        ));
        assert!(resolves_dark(
            ThemePreference::System,
            WindowAppearance::VibrantDark
        ));
        assert!(!resolves_dark(
            ThemePreference::System,
            WindowAppearance::Light
        ));
        assert!(!resolves_dark(
            ThemePreference::System,
            WindowAppearance::VibrantLight
        ));
    }

    /// Guards the exact bug this module exists to fix: the GPUI Component
    /// Theme mode fed to `sync_component_theme` must never disagree with the
    /// palette `resolve_theme` picked for Hane's own `Theme`, or the
    /// Component Theme drifts from Hane's theme (e.g. the selected file tab
    /// staying on the Light palette after Hane switches to dark).
    #[test]
    fn resolves_dark_matches_resolve_theme_choice_for_every_combination() {
        for preference in PREFERENCES {
            for appearance in APPEARANCES {
                let expected_dark = resolve_theme(preference, appearance) == DARK_THEME;
                assert_eq!(resolves_dark(preference, appearance), expected_dark);
            }
        }
    }
}
