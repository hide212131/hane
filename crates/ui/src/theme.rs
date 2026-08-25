#[derive(Clone, Copy, Debug)]
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
}

pub(crate) const DEFAULT_THEME: Theme = Theme {
    line_height: 26.0,
    line_horizontal_padding: 12.0,
    header_height: 38.0,
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
};
