//! The window's text system behind the layout's [`LineShaper`].
//!
//! Layout decides where rows begin and end; only the font can say how wide a
//! stretch of text is. This is the whole of that dependency: presentation asks
//! three questions, this answers them with the same runs the painted elements
//! use, so hit testing, the caret and the painted text cannot drift apart.

use crate::line::{block_font_size, inline_display_for};
use crate::ranges::partition;
use gpui::{FontStyle, FontWeight, TextRun, TextStyle, Window, WindowTextSystem, px};
use hane_presentation::{BlockWeight, LineShaper, VisualLine};
use std::hash::{DefaultHasher, Hash, Hasher};
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

    /// Stable key for every font property that can change wrapping or glyph x
    /// positions. Color and other paint-only text style fields are deliberately
    /// excluded, so a palette change does not discard valid geometry.
    pub(crate) fn font_revision(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.style.font().hash(&mut hasher);
        hasher.finish()
    }

    /// Font runs for one stretch of a line's visual text, split where the inline
    /// display policy changes. Lengths are relative to the stretch, which is what
    /// the text system expects.
    fn runs(&self, line: &VisualLine, fragment: &Range<usize>) -> Vec<TextRun> {
        let mut boundaries = Vec::new();
        for run in &line.style_runs {
            for at in [run.visual_range.start.0, run.visual_range.end.0] {
                if fragment.start < at && at < fragment.end {
                    boundaries.push(at);
                }
            }
        }
        let semibold = line.display().weight == BlockWeight::Semibold;
        partition(fragment.clone(), boundaries)
            .into_iter()
            .map(|range| {
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
                TextRun {
                    len: range.len(),
                    font,
                    color: self.style.color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::line::presented_block;
    use crate::view::EditorView;
    use hane_editor::Editor;
    use hane_markdown::BlockIndex;

    /// Issue #101 / PR #120 review: the earlier regression test asserted only
    /// `InlineDisplay::italic`, which is set one layer above the boundary this
    /// file owns. It never drove `WindowShaper::runs`, so a gap between "the
    /// render policy says italic" and "the `TextRun` GPUI/CoreText receives
    /// says italic" could pass unnoticed. This calls `runs` directly — the
    /// same private method `shape_fragment` and `wrap_boundaries` both use,
    /// and what `line.rs`'s painted `element.italic()`/`font_weight` mirrors
    /// for the non-shaped paint path — and checks a CJK run asks GPUI for the
    /// identical `FontStyle`/`FontWeight` as the equivalent ASCII run.
    #[gpui::test]
    fn cjk_and_ascii_emphasis_produce_the_same_font_style_at_the_shape_rs_boundary(
        cx: &mut gpui::TestAppContext,
    ) {
        let (_, cx) = cx.add_window_view(|_, cx| EditorView::new("", "Untitled", cx));

        cx.update(|window, _| {
            let shaper = WindowShaper::new(window);
            let editor = Editor::new("*漢字ひらがな* *ascii* **太字** **bold**");
            let index = BlockIndex::from_buffer(editor.document());
            let block = index.blocks().next().expect("one paragraph block");
            let visual =
                presented_block(&editor, &block, &(0..usize::MAX), None).expect("block presents");
            let line = &visual.lines[0];

            let range_of = |needle: &str| {
                let start = line.visual_text.find(needle).expect("needle present");
                start..start + needle.len()
            };

            let cjk_italic = shaper.runs(line, &range_of("漢字ひらがな"));
            let ascii_italic = shaper.runs(line, &range_of("ascii"));
            assert!(
                cjk_italic
                    .iter()
                    .all(|run| run.font.style == FontStyle::Italic),
                "the CJK run must reach GPUI as FontStyle::Italic, not just InlineDisplay::italic"
            );
            assert_eq!(
                cjk_italic
                    .iter()
                    .map(|run| run.font.style)
                    .collect::<Vec<_>>(),
                ascii_italic
                    .iter()
                    .map(|run| run.font.style)
                    .collect::<Vec<_>>(),
                "CJK and ASCII italic must request the same FontStyle"
            );

            let cjk_bold = shaper.runs(line, &range_of("太字"));
            let ascii_bold = shaper.runs(line, &range_of("bold"));
            assert!(
                cjk_bold
                    .iter()
                    .all(|run| run.font.weight == FontWeight::BOLD),
                "the CJK run must reach GPUI as FontWeight::BOLD, not just InlineDisplay::bold"
            );
            assert_eq!(
                cjk_bold
                    .iter()
                    .map(|run| run.font.weight)
                    .collect::<Vec<_>>(),
                ascii_bold
                    .iter()
                    .map(|run| run.font.weight)
                    .collect::<Vec<_>>(),
                "CJK and ASCII bold must request the same FontWeight"
            );
        });
    }
}
