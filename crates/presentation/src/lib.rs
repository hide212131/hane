//! Local visual blocks, source mapping, and variable-height virtualization.

use hane_document::{
    Bias, Revision, RevisionDelta, RopeBuffer, SourceOffset, SourceRange, TextBuffer,
};
use hane_markdown::{
    BlockKind as MarkdownBlockKind, InlineKind as MarkdownInlineKind, parse_bold, parse_document,
};
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
    Italic,
    Strikethrough,
    InlineCode,
    CodeBlock,
    Link,
    MarkedText,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StyleRun {
    pub visual_range: VisualRange,
    pub kind: StyleKind,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BlockKind {
    #[default]
    Paragraph,
    Heading(u8),
    CodeBlock,
    Quote,
    ListItem,
    Rule,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VisualBlock {
    pub block_id: u64,
    pub source_range: SourceRange,
    pub revision: Revision,
    pub visual_text: String,
    pub style_runs: Vec<StyleRun>,
    pub kind: BlockKind,
    pub source_map: SourceMap,
    pub estimated_height: f32,
    pub measured_height: Option<f32>,
    pub invalid: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LineSpan {
    pub visual_range: Range<usize>,
    pub bold: bool,
}

/// Splits a visual block at style and cursor boundaries.
///
/// The returned cursor index is an insertion point in the span vector, so the
/// UI can insert its caret element without repeating presentation logic.
pub fn line_spans(
    block: &VisualBlock,
    cursor: Option<VisualOffset>,
) -> (Vec<LineSpan>, Option<usize>) {
    let cursor = cursor.map(|offset| offset.0.min(block.visual_text.len()));
    let mut boundaries = vec![0, block.visual_text.len()];
    boundaries.extend(cursor);
    for run in &block.style_runs {
        boundaries.push(run.visual_range.start.0);
        boundaries.push(run.visual_range.end.0);
    }
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut spans = Vec::with_capacity(boundaries.len().saturating_sub(1));
    let mut cursor_span = None;
    let mut style_index = 0;
    for pair in boundaries.windows(2) {
        let range = pair[0]..pair[1];
        if cursor == Some(range.start) {
            cursor_span = Some(spans.len());
        }
        if range.is_empty()
            || !block.visual_text.is_char_boundary(range.start)
            || !block.visual_text.is_char_boundary(range.end)
        {
            continue;
        }
        while style_index < block.style_runs.len()
            && block.style_runs[style_index].visual_range.end.0 <= range.start
        {
            style_index += 1;
        }
        let bold = block.style_runs[style_index..]
            .iter()
            .take_while(|run| run.visual_range.start.0 < range.end)
            .any(|run| {
                run.kind == StyleKind::Bold
                    && range.start >= run.visual_range.start.0
                    && range.end <= run.visual_range.end.0
            });
        spans.push(LineSpan {
            visual_range: range,
            bold,
        });
    }
    if cursor == Some(block.visual_text.len()) {
        cursor_span = Some(spans.len());
    }
    (spans, cursor_span)
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
        kind: BlockKind::Paragraph,
        source_map: map,
        estimated_height: 24.0,
        measured_height: None,
        invalid: false,
    }
}

pub fn present_plain(
    block_id: u64,
    revision: Revision,
    range: SourceRange,
    source: &str,
) -> VisualBlock {
    VisualBlock {
        block_id,
        source_range: range,
        revision,
        visual_text: source.to_owned(),
        style_runs: Vec::new(),
        kind: BlockKind::Paragraph,
        source_map: SourceMap {
            segments: vec![MappingSegment {
                source_range: range,
                visual_range: VisualRange::new(0, source.len()),
                visibility: Visibility::Visible,
            }],
        },
        estimated_height: 24.0,
        measured_height: None,
        invalid: false,
    }
}

fn presentation_block_kind(kind: MarkdownBlockKind) -> BlockKind {
    match kind {
        MarkdownBlockKind::Paragraph => BlockKind::Paragraph,
        MarkdownBlockKind::Heading(level) => BlockKind::Heading(level),
        MarkdownBlockKind::CodeBlock => BlockKind::CodeBlock,
        MarkdownBlockKind::Quote => BlockKind::Quote,
        MarkdownBlockKind::ListItem => BlockKind::ListItem,
        MarkdownBlockKind::Rule => BlockKind::Rule,
    }
}

fn presentation_style_kind(kind: MarkdownInlineKind) -> StyleKind {
    match kind {
        MarkdownInlineKind::Bold => StyleKind::Bold,
        MarkdownInlineKind::Italic => StyleKind::Italic,
        MarkdownInlineKind::Strikethrough => StyleKind::Strikethrough,
        MarkdownInlineKind::InlineCode => StyleKind::InlineCode,
        MarkdownInlineKind::Link => StyleKind::Link,
        MarkdownInlineKind::CodeBlock => StyleKind::CodeBlock,
    }
}

fn estimated_height(kind: BlockKind, line_height: f32) -> f32 {
    match kind {
        BlockKind::Heading(1) => line_height * 1.65,
        BlockKind::Heading(2) => line_height * 1.45,
        BlockKind::Heading(3) => line_height * 1.25,
        BlockKind::Heading(_) => line_height * 1.1,
        BlockKind::CodeBlock => line_height * 1.15,
        _ => line_height,
    }
}

/// Builds a Phase 2 visual block. Markdown bytes remain visible, so the source
/// map is deliberately an identity map until progressive disclosure in Phase 3.
pub fn present_markdown(
    block_id: u64,
    revision: Revision,
    range: SourceRange,
    source: &str,
    line_height: f32,
) -> VisualBlock {
    if source.is_empty() {
        let mut block = present_plain(block_id, revision, range, source);
        block.estimated_height = line_height;
        return block;
    }
    let parsed = parse_document(revision, range, source);
    let kind = parsed
        .blocks
        .first()
        .map(|block| presentation_block_kind(block.kind))
        .unwrap_or_default();
    let mut style_runs = parsed
        .spans
        .into_iter()
        .filter_map(|span| {
            let clipped = SourceRange {
                start: span.source_range.start.max(range.start),
                end: span.source_range.end.min(range.end),
            };
            (!clipped.is_empty()).then_some(StyleRun {
                visual_range: VisualRange::new(
                    clipped.start.0 - range.start.0,
                    clipped.end.0 - range.start.0,
                ),
                kind: presentation_style_kind(span.kind),
            })
        })
        .collect::<Vec<_>>();
    style_runs.sort_by_key(|run| (run.visual_range.start.0, run.visual_range.end.0));
    VisualBlock {
        block_id,
        source_range: range,
        revision,
        visual_text: source.to_owned(),
        style_runs,
        source_map: SourceMap {
            segments: vec![MappingSegment {
                source_range: range,
                visual_range: VisualRange::new(0, source.len()),
                visibility: Visibility::Visible,
            }],
        },
        estimated_height: estimated_height(kind, line_height),
        measured_height: None,
        invalid: false,
        kind,
    }
}

pub fn paragraph_blocks(buffer: &RopeBuffer, line_height: f32) -> Vec<VisualBlock> {
    let mut blocks = Vec::with_capacity(buffer.line_count());
    for line in 0..buffer.line_count() {
        let Ok(range) = buffer.line_range(hane_document::LineId(line)) else {
            continue;
        };
        let text = buffer.text(range).unwrap_or_default();
        let block = present_markdown(line as u64, buffer.revision(), range, &text, line_height);
        blocks.push(block);
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
    fn plain_presentation_preserves_markdown_source_bytes() {
        let text = "**日本語**";
        let block = present_plain(3, Revision(2), SourceRange::new(10, 10 + text.len()), text);
        assert_eq!(block.visual_text, text);
        for relative in [0, 2, 5, 8, text.len()] {
            assert_eq!(
                block
                    .source_map
                    .visual_to_source(VisualOffset(relative), Bias::After)
                    .unwrap()
                    .source_offset,
                SourceOffset(10 + relative)
            );
        }
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

    #[test]
    fn line_spans_split_bold_text_at_the_cursor() {
        let source = "a **日本語** z";
        let block = present_bold(1, Revision(0), SourceRange::new(0, source.len()), source);
        let cursor = VisualOffset("a 日".len());
        let (spans, cursor_span) = line_spans(&block, Some(cursor));
        assert_eq!(cursor_span, Some(2));
        assert_eq!(
            spans,
            vec![
                LineSpan {
                    visual_range: 0..2,
                    bold: false
                },
                LineSpan {
                    visual_range: 2..5,
                    bold: true
                },
                LineSpan {
                    visual_range: 5..11,
                    bold: true
                },
                LineSpan {
                    visual_range: 11..13,
                    bold: false
                },
            ]
        );
    }

    #[test]
    fn phase2_presentation_styles_markdown_without_changing_source_identity() {
        let source = "## Hello **太字** and _italic_ with `code`";
        let block = present_markdown(
            5,
            Revision(3),
            SourceRange::new(40, 40 + source.len()),
            source,
            26.0,
        );
        assert_eq!(block.visual_text, source);
        assert_eq!(block.kind, BlockKind::Heading(2));
        for kind in [StyleKind::Bold, StyleKind::Italic, StyleKind::InlineCode] {
            assert!(block.style_runs.iter().any(|run| run.kind == kind));
        }
        for relative in [0, 3, source.len()] {
            assert_eq!(
                block
                    .source_map
                    .visual_to_source(VisualOffset(relative), Bias::After)
                    .unwrap()
                    .source_offset,
                SourceOffset(40 + relative)
            );
        }
        assert!(block.height() > 26.0);
    }
}
