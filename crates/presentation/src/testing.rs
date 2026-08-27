//! A shaper with no font behind it, so the layout contract can be tested — and
//! benchmarked — without a window.
//!
//! Every character advances the same distance, which is wrong for real text and
//! exactly right for asserting geometry: a test can say "the caret is four
//! columns in" and mean it.

use crate::VisualLine;
use crate::layout::LineShaper;
use std::ops::Range;

/// Measures text as a fixed advance per character, scaled by the line's own
/// font scale so headings still measure wider than body text.
#[derive(Clone, Copy, Debug)]
pub struct FixedAdvanceShaper {
    advance: f32,
}

impl FixedAdvanceShaper {
    pub const fn new(advance: f32) -> Self {
        Self { advance }
    }

    fn advance_for(&self, line: &VisualLine) -> f32 {
        self.advance * line.display().font_scale
    }
}

impl Default for FixedAdvanceShaper {
    fn default() -> Self {
        Self::new(8.0)
    }
}

impl LineShaper for FixedAdvanceShaper {
    fn wrap_boundaries(&self, line: &VisualLine, width: f32) -> Vec<usize> {
        let advance = self.advance_for(line);
        if advance <= 0.0 || width < advance {
            return Vec::new();
        }
        let columns = (width / advance).floor().max(1.0) as usize;
        let mut boundaries = Vec::new();
        let mut start = 0;
        let mut taken = 0;
        // The last place a break could go without splitting a word, if there was
        // one since the current row started.
        let mut breakable = None;
        for (offset, character) in line.visual_text.char_indices() {
            if taken == columns {
                let at = match breakable {
                    Some(at) if at > start => at,
                    _ => offset,
                };
                boundaries.push(at);
                start = at;
                breakable = None;
                taken = line.visual_text[at..offset].chars().count();
            }
            if character.is_whitespace() {
                breakable = Some(offset + character.len_utf8());
            }
            taken += 1;
        }
        boundaries
    }

    fn x_for_offset(&self, line: &VisualLine, fragment: Range<usize>, offset: usize) -> f32 {
        let offset = offset.clamp(fragment.start, fragment.end);
        line.visual_text[fragment.start..offset].chars().count() as f32 * self.advance_for(line)
    }

    fn offset_for_x(&self, line: &VisualLine, fragment: Range<usize>, x: f32) -> usize {
        let advance = self.advance_for(line);
        let text = &line.visual_text[fragment.clone()];
        let column = if advance <= 0.0 {
            0
        } else {
            (x / advance).round().max(0.0) as usize
        };
        fragment.start
            + text
                .char_indices()
                .nth(column)
                .map_or(text.len(), |(offset, _)| offset)
    }
}
