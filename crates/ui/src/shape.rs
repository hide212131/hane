//! The window's text system behind the layout's [`LineShaper`].
//!
//! Layout decides where rows begin and end; only the font can say how wide a
//! stretch of text is. This is the whole of that dependency: presentation asks
//! three questions, this answers them with the same runs the painted elements
//! use, so hit testing, the caret and the painted text cannot drift apart.

use crate::line::{block_font_size, inline_display_for};
use gpui::{FontStyle, FontWeight, TextRun, TextStyle, Window, WindowTextSystem, px};
use hane_presentation::{BlockWeight, LineShaper, VisualLine};
use std::ops::Range;
use std::sync::Arc;

pub(crate) struct WindowShaper {
    text_system: Arc<WindowTextSystem>,
    style: TextStyle,
}

impl WindowShaper {
    pub(crate) fn new(window: &Window) -> Self {
        Self {
            text_system: window.text_system().clone(),
            style: window.text_style(),
        }
    }

    /// Font runs for one stretch of a line's visual text, split where the inline
    /// display policy changes. Lengths are relative to the stretch, which is what
    /// the text system expects.
    fn runs(&self, line: &VisualLine, fragment: &Range<usize>) -> Vec<TextRun> {
        let mut boundaries = vec![fragment.start, fragment.end];
        for run in &line.style_runs {
            for at in [run.visual_range.start.0, run.visual_range.end.0] {
                if fragment.start < at && at < fragment.end {
                    boundaries.push(at);
                }
            }
        }
        boundaries.sort_unstable();
        boundaries.dedup();
        let semibold = line.display().weight == BlockWeight::Semibold;
        boundaries
            .windows(2)
            .filter_map(|pair| {
                let range = pair[0]..pair[1];
                if range.is_empty() {
                    return None;
                }
                let inline = inline_display_for(&range, &line.style_runs);
                let mut font = self.style.font();
                if semibold || inline.bold {
                    font.weight = if inline.bold {
                        FontWeight::BOLD
                    } else {
                        FontWeight::SEMIBOLD
                    };
                }
                if inline.italic {
                    font.style = FontStyle::Italic;
                }
                if inline.monospace {
                    font.family = "ui-monospace".into();
                }
                Some(TextRun {
                    len: range.len(),
                    font,
                    color: self.style.color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                })
            })
            .collect()
    }

    fn shape_fragment(&self, line: &VisualLine, fragment: &Range<usize>) -> gpui::ShapedLine {
        let runs = self.runs(line, fragment);
        self.text_system.shape_line(
            line.visual_text[fragment.clone()].to_owned().into(),
            px(block_font_size(line)),
            &runs,
            None,
        )
    }
}

impl LineShaper for WindowShaper {
    fn wrap_boundaries(&self, line: &VisualLine, width: f32) -> Vec<usize> {
        let whole = 0..line.visual_text.len();
        if whole.is_empty() {
            return Vec::new();
        }
        let runs = self.runs(line, &whole);
        let Ok(wrapped) = self.text_system.shape_text(
            line.visual_text.clone().into(),
            px(block_font_size(line)),
            &runs,
            Some(px(width)),
            None,
        ) else {
            return Vec::new();
        };
        // The visual text of a presented line never contains a newline, so the
        // text system returns exactly one wrapped line.
        wrapped
            .first()
            .map(|line| {
                line.wrap_boundaries
                    .iter()
                    .filter_map(|boundary| {
                        let run = line.unwrapped_layout.runs.get(boundary.run_ix)?;
                        run.glyphs.get(boundary.glyph_ix).map(|glyph| glyph.index)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn x_for_offset(&self, line: &VisualLine, fragment: Range<usize>, offset: usize) -> f32 {
        if fragment.is_empty() {
            return 0.0;
        }
        let offset = offset.clamp(fragment.start, fragment.end) - fragment.start;
        f32::from(self.shape_fragment(line, &fragment).x_for_index(offset))
    }

    fn offset_for_x(&self, line: &VisualLine, fragment: Range<usize>, x: f32) -> usize {
        if fragment.is_empty() {
            return fragment.start;
        }
        let start = fragment.start;
        start
            + self
                .shape_fragment(line, &fragment)
                .closest_index_for_x(px(x))
    }
}
