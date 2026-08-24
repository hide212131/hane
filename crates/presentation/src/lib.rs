//! Local visual blocks, source mapping, and variable-height virtualization.

use hane_document::{
    Bias, Revision, RevisionDelta, RopeBuffer, SourceOffset, SourceRange, TextBuffer,
};
use hane_markdown::parse_bold;
use std::ops::Range;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub struct VisualOffset(pub usize);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub struct VisualRange {
    pub start: VisualOffset,
    pub end: VisualOffset,
}

impl VisualRange {
    pub const fn new(start: usize, end: usize) -> Self {
        Self {
            start: VisualOffset(start),
            end: VisualOffset(end),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Visibility {
    Visible,
    HiddenMarkup,
    Synthesized,
    ExpandedMarkup,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BoundarySide {
    Leading,
    Trailing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MappingSegment {
    pub source_range: SourceRange,
    pub visual_range: VisualRange,
    pub visibility: Visibility,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PositionCandidate {
    pub source_offset: SourceOffset,
    pub visual_offset: VisualOffset,
    pub affinity: Bias,
    pub side: BoundarySide,
    pub visibility: Visibility,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SourceMap {
    pub segments: Vec<MappingSegment>,
}

impl SourceMap {
    pub fn visual_to_source(
        &self,
        visual: VisualOffset,
        affinity: Bias,
    ) -> Option<PositionCandidate> {
        let mut candidates = self.segments.iter().filter_map(|segment| {
            if visual.0 < segment.visual_range.start.0 || visual.0 > segment.visual_range.end.0 {
                return None;
            }
            let source = match segment.visibility {
                Visibility::Visible | Visibility::ExpandedMarkup => {
                    let delta = visual.0.saturating_sub(segment.visual_range.start.0);
                    SourceOffset(
                        (segment.source_range.start.0 + delta).min(segment.source_range.end.0),
                    )
                }
                Visibility::HiddenMarkup => match affinity {
                    Bias::Before => segment.source_range.start,
                    Bias::After => segment.source_range.end,
                },
                Visibility::Synthesized => segment.source_range.start,
            };
            Some(PositionCandidate {
                source_offset: source,
                visual_offset: visual,
                affinity,
                side: if affinity == Bias::Before {
                    BoundarySide::Leading
                } else {
                    BoundarySide::Trailing
                },
                visibility: segment.visibility,
            })
        });
        candidates
            .find(|c| c.visibility == Visibility::Visible)
            .or_else(|| candidates.next())
    }

    pub fn source_to_visual(
        &self,
        source: SourceOffset,
        affinity: Bias,
    ) -> Option<PositionCandidate> {
        self.segments.iter().find_map(|segment| {
            if source.0 < segment.source_range.start.0 || source.0 > segment.source_range.end.0 {
                return None;
            }
            let visual = match segment.visibility {
                Visibility::Visible | Visibility::ExpandedMarkup => VisualOffset(
                    segment.visual_range.start.0
                        + source.0.saturating_sub(segment.source_range.start.0),
                ),
                Visibility::HiddenMarkup => match affinity {
                    Bias::Before => segment.visual_range.start,
                    Bias::After => segment.visual_range.end,
                },
                Visibility::Synthesized => segment.visual_range.start,
            };
            Some(PositionCandidate {
                source_offset: source,
                visual_offset: visual,
                affinity,
                side: if affinity == Bias::Before {
                    BoundarySide::Leading
                } else {
                    BoundarySide::Trailing
                },
                visibility: segment.visibility,
            })
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StyleKind {
    Bold,
    MarkedText,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StyleRun {
    pub visual_range: VisualRange,
    pub kind: StyleKind,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VisualBlock {
    pub block_id: u64,
    pub source_range: SourceRange,
    pub revision: Revision,
    pub visual_text: String,
    pub style_runs: Vec<StyleRun>,
    pub source_map: SourceMap,
    pub estimated_height: f32,
    pub measured_height: Option<f32>,
    pub invalid: bool,
}

impl VisualBlock {
    pub fn height(&self) -> f32 {
        self.measured_height.unwrap_or(self.estimated_height)
    }

    pub fn rebase(&mut self, deltas: &[RevisionDelta], current: Revision) -> bool {
        let mut range = self.source_range;
        for delta in deltas {
            let Some(next) = delta.transform_range(range) else {
                return false;
            };
            range = next;
            for segment in &mut self.source_map.segments {
                let Some(rebased) = delta.transform_range(segment.source_range) else {
                    return false;
                };
                segment.source_range = rebased;
            }
        }
        self.source_range = range;
        self.revision = current;
        true
    }
}

pub fn present_bold(
    block_id: u64,
    revision: Revision,
    range: SourceRange,
    source: &str,
) -> VisualBlock {
    let parsed = parse_bold(revision, range, source);
    let mut visual = String::with_capacity(source.len());
    let mut map = SourceMap::default();
    let mut styles = Vec::new();
    let mut source_cursor = 0;
    for span in parsed.spans {
        let open = span.open_marker.start.0 - range.start.0;
        let content_start = span.content_range.start.0 - range.start.0;
        let content_end = span.content_range.end.0 - range.start.0;
        let close_end = span.close_marker.end.0 - range.start.0;
        if source_cursor < open {
            let visual_start = visual.len();
            visual.push_str(&source[source_cursor..open]);
            map.segments.push(MappingSegment {
                source_range: SourceRange::new(range.start.0 + source_cursor, range.start.0 + open),
                visual_range: VisualRange::new(visual_start, visual.len()),
                visibility: Visibility::Visible,
            });
        }
        let visual_at_open = visual.len();
        map.segments.push(MappingSegment {
            source_range: span.open_marker,
            visual_range: VisualRange::new(visual_at_open, visual_at_open),
            visibility: Visibility::HiddenMarkup,
        });
        let style_start = visual.len();
        visual.push_str(&source[content_start..content_end]);
        map.segments.push(MappingSegment {
            source_range: span.content_range,
            visual_range: VisualRange::new(style_start, visual.len()),
            visibility: Visibility::Visible,
        });
        styles.push(StyleRun {
            visual_range: VisualRange::new(style_start, visual.len()),
            kind: StyleKind::Bold,
        });
        map.segments.push(MappingSegment {
            source_range: span.close_marker,
            visual_range: VisualRange::new(visual.len(), visual.len()),
            visibility: Visibility::HiddenMarkup,
        });
        source_cursor = close_end;
    }
    if source_cursor < source.len() {
        let visual_start = visual.len();
        visual.push_str(&source[source_cursor..]);
        map.segments.push(MappingSegment {
            source_range: SourceRange::new(range.start.0 + source_cursor, range.end.0),
            visual_range: VisualRange::new(visual_start, visual.len()),
            visibility: Visibility::Visible,
        });
    }
    VisualBlock {
        block_id,
        source_range: range,
        revision,
        visual_text: visual,
        style_runs: styles,
        source_map: map,
        estimated_height: 24.0,
        measured_height: None,
        invalid: false,
    }
}

pub fn paragraph_blocks(buffer: &RopeBuffer, line_height: f32) -> Vec<VisualBlock> {
    let mut blocks = Vec::with_capacity(buffer.line_count());
    let mut start = 0;
    for line in 0..buffer.line_count() {
        let end = if line + 1 < buffer.line_count() {
            buffer
                .offset_for_line_col(hane_document::LineId(line + 1), hane_document::LineCol(0))
                .unwrap()
                .0
        } else {
            buffer.len_bytes().0
        };
        let range = SourceRange::new(start, end);
        let text = buffer.text(range).unwrap_or_default();
        let mut block = present_bold(line as u64, buffer.revision(), range, &text);
        block.estimated_height = line_height;
        blocks.push(block);
        start = end;
    }
    blocks
}

/// Fenwick tree over non-negative block heights.
#[derive(Clone, Debug)]
pub struct HeightIndex {
    heights: Vec<f32>,
    tree: Vec<f32>,
}

impl HeightIndex {
    pub fn new(heights: impl IntoIterator<Item = f32>) -> Self {
        let heights: Vec<_> = heights.into_iter().map(|h| h.max(0.0)).collect();
        let mut this = Self {
            tree: vec![0.0; heights.len() + 1],
            heights,
        };
        for ix in 0..this.heights.len() {
            this.add(ix, this.heights[ix]);
        }
        this
    }
    pub fn len(&self) -> usize {
        self.heights.len()
    }
    pub fn is_empty(&self) -> bool {
        self.heights.is_empty()
    }
    fn add(&mut self, index: usize, delta: f32) {
        let mut i = index + 1;
        while i < self.tree.len() {
            self.tree[i] += delta;
            i += i & i.wrapping_neg();
        }
    }
    pub fn update(&mut self, index: usize, height: f32) {
        let next = height.max(0.0);
        let delta = next - self.heights[index];
        self.heights[index] = next;
        self.add(index, delta);
    }
    pub fn prefix_sum(&self, exclusive_end: usize) -> f32 {
        let mut i = exclusive_end.min(self.len());
        let mut sum = 0.0;
        while i > 0 {
            sum += self.tree[i];
            i &= i - 1;
        }
        sum
    }
    pub fn total_height(&self) -> f32 {
        self.prefix_sum(self.len())
    }
    pub fn block_at_y(&self, y: f32) -> usize {
        if self.is_empty() {
            return 0;
        }
        let target = y.clamp(0.0, self.total_height());
        let mut index = 0usize;
        let mut sum = 0.0;
        let mut bit = 1usize;
        while bit << 1 < self.tree.len() {
            bit <<= 1;
        }
        while bit > 0 {
            let next = index + bit;
            if next < self.tree.len() && sum + self.tree[next] <= target {
                index = next;
                sum += self.tree[next];
            }
            bit >>= 1;
        }
        index.min(self.len() - 1)
    }
    pub fn visible_range(&self, scroll_y: f32, viewport: f32, overscan: f32) -> Range<usize> {
        if self.is_empty() {
            return 0..0;
        }
        let start = self.block_at_y((scroll_y - overscan).max(0.0));
        let end = (self.block_at_y(scroll_y + viewport + overscan) + 1).min(self.len());
        start..end
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollAnchor {
    pub block_id: u64,
    pub intra_block_y: f32,
    pub visual_position_hint: Option<VisualOffset>,
}

pub fn anchored_scroll_y(
    anchor: ScrollAnchor,
    blocks: &[VisualBlock],
    heights: &HeightIndex,
) -> Option<f32> {
    let index = blocks.iter().position(|b| b.block_id == anchor.block_id)?;
    Some(heights.prefix_sum(index) + anchor.intra_block_y.clamp(0.0, blocks[index].height()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bold_hides_markers_and_maps_unicode() {
        let b = present_bold(0, Revision(1), SourceRange::new(0, 18), "これは**重要**");
        assert_eq!(b.visual_text, "これは重要");
        assert_eq!(b.style_runs.len(), 1);
        assert_eq!(
            b.source_map
                .visual_to_source(VisualOffset(12), Bias::After)
                .unwrap()
                .source_offset,
            SourceOffset(14)
        );
    }
    #[test]
    fn fenwick_updates_and_finds_visible_blocks() {
        let mut h = HeightIndex::new([10.0, 20.0, 30.0]);
        assert_eq!(h.total_height(), 60.0);
        assert_eq!(h.block_at_y(10.0), 1);
        assert_eq!(h.visible_range(12.0, 10.0, 0.0), 1..2);
        h.update(0, 20.0);
        assert_eq!(h.total_height(), 70.0);
        assert_eq!(h.block_at_y(15.0), 0);
    }
    #[test]
    fn stale_non_overlapping_block_rebases() {
        let mut b = present_bold(0, Revision(0), SourceRange::new(4, 7), "two");
        let d = RevisionDelta {
            from_revision: Revision(0),
            to_revision: Revision(1),
            edited_source_range_before: SourceRange::empty(0),
            edited_source_range_after: SourceRange::new(0, 4),
            byte_delta: 4,
        };
        assert!(b.rebase(&[d], Revision(1)));
        assert_eq!(b.source_range, SourceRange::new(8, 11));
    }
}
