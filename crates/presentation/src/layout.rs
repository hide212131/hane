//! Block → LayoutLine → Run: the visual coordinate system.
//!
//! R4A made the Markdown block the unit of virtualization while caret, selection
//! and IME still addressed physical source lines. That works only while one
//! physical line is one row on screen. A wrapped paragraph is not: it occupies
//! several rows, and "the row below" is not "the source line below".
//!
//! This module introduces the row — a [`LayoutLine`] — between the block and the
//! runs the renderer paints. A row is a whole physical line, or one fragment of a
//! soft-wrapped one, and it carries where it sits (`y`, `height`), what source it
//! covers, and its stretch of visual text in both line-local and block-local
//! coordinates. Every caret geometry question is answered here: source offset →
//! (row, x), (x, y) → source offset, and the row above or below a caret.
//!
//! Measuring text needs a font, which is GPUI's business, so layout takes a
//! [`LineShaper`]. The UI backs it with the window's text system; tests back it
//! with a fixed advance width, which is what makes the coordinate contract
//! verifiable without a window.

use crate::{VisualBlock, VisualLine, VisualOffset, VisualRange};
use hane_document::{Bias, Revision, RevisionDelta, SourceOffset, SourceRange};
use hane_markdown::BlockId;
use std::ops::Range;

/// How a row ends.
///
/// The distinction matters to the caret: a hard break is a newline in the source
/// and separates two source lines, while a soft break is a layout decision that
/// no source byte stands for. Only hard breaks own the trailing caret position.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LineWrap {
    /// The row ends where its physical source line ends.
    Hard,
    /// The row ends because the text did not fit, and continues on the next row.
    Soft,
}

/// One row of a block: a whole physical line, or one fragment of a wrapped one.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutLine {
    /// Index into [`VisualBlock::lines`] of the presented line this row is part
    /// of. The row's text, style runs and source map all come from there.
    pub line: usize,
    /// Document line number, for callers that still address physical lines.
    pub line_id: u64,
    /// Which fragment of that line this is; 0 for the first row of the line.
    pub fragment: usize,
    pub wrap: LineWrap,
    /// The row's stretch of its line's visual text.
    pub line_visual_range: Range<usize>,
    /// The same stretch in block-local visual coordinates, where each line is
    /// followed by one position standing for its break. Block-local offsets are
    /// unique and ordered across the whole block, which is what lets a cache
    /// entry (R4C) describe a position without naming a physical line.
    pub visual_range: VisualRange,
    /// Source bytes this row covers. Rows tile their line's source range, and
    /// lines tile the block, so every source byte belongs to exactly one row.
    pub source_range: SourceRange,
    /// Top of the row, relative to the first presented row of the block.
    pub y: f32,
    pub height: f32,
}

impl LayoutLine {
    pub fn bottom(&self) -> f32 {
        self.y + self.height
    }

    /// True when `offset` is inside this row, or at its end and the row is the
    /// last of its line. A soft break's boundary offset belongs to the row that
    /// starts there, so a caret at a wrap point renders at the start of the next
    /// row rather than past the right edge of the previous one.
    pub fn owns_source(&self, offset: SourceOffset, is_last_row: bool) -> bool {
        if offset < self.source_range.start {
            return false;
        }
        if offset < self.source_range.end {
            return true;
        }
        offset == self.source_range.end && self.wrap == LineWrap::Hard && is_last_row
    }
}

/// The text measurement layout needs, expressed without a font or a window.
///
/// Fragments are half-open byte ranges of the line's visual text. Offsets are
/// byte offsets into that same text, never into the fragment, so callers never
/// have to rebase them.
pub trait LineShaper {
    /// Byte offsets of `line.visual_text` where it has to break to fit `width`,
    /// ascending, excluding 0 and the text length.
    fn wrap_boundaries(&self, line: &VisualLine, width: f32) -> Vec<usize>;
    /// x of `offset`, measured from the left edge of `fragment`.
    fn x_for_offset(&self, line: &VisualLine, fragment: Range<usize>, offset: usize) -> f32;
    /// The offset in `fragment` closest to `x`, measured from its left edge.
    fn offset_for_x(&self, line: &VisualLine, fragment: Range<usize>, x: f32) -> usize;
}

/// Where a source offset sits inside a laid-out block.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutPoint {
    pub row: usize,
    /// Distance from the left edge of the text column.
    pub x: f32,
    /// Top of the row, relative to the top of the block.
    pub y: f32,
    pub height: f32,
}

/// The result of asking a block for the row above or below a caret.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VerticalMove {
    /// A caret target inside this block.
    To(SourceOffset),
    /// The caret is already on the first or last row of the block; the caller
    /// continues in the block above or below.
    PastEdge,
    /// The offset is not in this block's laid-out rows.
    Unknown,
}

/// The rows of one presented block, for one width.
///
/// Holds the width and revision it was built for so a cache can tell when it
/// still applies — the R4C invalidation keys.
#[derive(Clone, Debug, PartialEq)]
pub struct BlockLayout {
    pub block: BlockId,
    pub revision: Revision,
    /// Text column width the rows were wrapped to.
    pub width: f32,
    pub lines: Vec<LayoutLine>,
    /// Space standing in for the block's lines clipped above the presented run.
    pub leading_space: f32,
    /// The same below.
    pub trailing_space: f32,
}

impl BlockLayout {
    /// Height of the whole block: the clipped space plus every row.
    pub fn height(&self) -> f32 {
        self.leading_space + self.trailing_space + self.lines.last().map_or(0.0, LayoutLine::bottom)
    }

    /// Height of the rows one presented line occupies. A wrapped line is taller
    /// than one row, which is what the line-granularity height index needs.
    pub fn line_height_of(&self, line: usize) -> f32 {
        self.lines
            .iter()
            .filter(|row| row.line == line)
            .map(|row| row.height)
            .sum()
    }

    /// Mean height of a presented line, used to estimate how far into a block a
    /// scroll position falls before that part of the block has been laid out.
    pub fn average_line_height(&self) -> Option<f32> {
        let lines = self.lines.last().map(|row| row.line + 1)?;
        (lines > 0).then(|| self.lines.iter().map(|row| row.height).sum::<f32>() / lines as f32)
    }

    /// The row a source offset renders on.
    pub fn row_for_source(&self, offset: SourceOffset) -> Option<usize> {
        let last = self.lines.len().saturating_sub(1);
        self.lines
            .iter()
            .position(|row| row.owns_source(offset, false))
            .or_else(|| {
                self.lines
                    .get(last)
                    .filter(|row| row.owns_source(offset, true))
                    .map(|_| last)
            })
    }

    /// The row at `y`, measured from the top of the block. Positions inside the
    /// clipped space at either end resolve to the nearest presented row.
    pub fn row_at_y(&self, y: f32) -> Option<usize> {
        let local = y - self.leading_space;
        if self.lines.is_empty() {
            return None;
        }
        Some(
            self.lines
                .iter()
                .position(|row| local < row.bottom())
                .unwrap_or(self.lines.len() - 1),
        )
    }

    /// Vertical extent of the row a source offset renders on, relative to the
    /// top of the block. Answering this needs no font, so scrolling the caret
    /// into view does not have to shape anything.
    pub fn row_bounds_for_source(&self, offset: SourceOffset) -> Option<(f32, f32)> {
        let row = &self.lines[self.row_for_source(offset)?];
        Some((self.leading_space + row.y, row.height))
    }

    /// Where a source offset sits, for drawing the caret and for placing the IME
    /// candidate window.
    pub fn point_for_source(
        &self,
        block: &VisualBlock,
        offset: SourceOffset,
        shaper: &dyn LineShaper,
    ) -> Option<LayoutPoint> {
        let row_index = self.row_for_source(offset)?;
        let row = &self.lines[row_index];
        let line = block.lines.get(row.line)?;
        let visual = line
            .source_map
            .source_to_visual(offset, Bias::After)
            .map_or(row.line_visual_range.start, |candidate| {
                candidate.visual_offset.0
            })
            .clamp(row.line_visual_range.start, row.line_visual_range.end);
        Some(LayoutPoint {
            row: row_index,
            x: shaper.x_for_offset(line, row.line_visual_range.clone(), visual),
            y: self.leading_space + row.y,
            height: row.height,
        })
    }

    /// The source offset under a point in the block, for clicks and drags.
    pub fn source_for_point(
        &self,
        block: &VisualBlock,
        x: f32,
        y: f32,
        shaper: &dyn LineShaper,
    ) -> Option<SourceOffset> {
        self.source_at_x(block, self.row_at_y(y)?, x, shaper)
    }

    /// The source offset at `x` on one row. Vertical movement is this applied to
    /// the row above or below, which is why it is separate from the point form.
    pub fn source_at_x(
        &self,
        block: &VisualBlock,
        row_index: usize,
        x: f32,
        shaper: &dyn LineShaper,
    ) -> Option<SourceOffset> {
        let row = self.lines.get(row_index)?;
        let line = block.lines.get(row.line)?;
        let visual = shaper
            .offset_for_x(line, row.line_visual_range.clone(), x)
            .clamp(row.line_visual_range.start, row.line_visual_range.end);
        Some(
            line.source_map
                .visual_to_source(VisualOffset(visual), Bias::After)
                .map_or(row.source_range.start, |candidate| candidate.source_offset),
        )
    }

    /// The caret target one row above or below `offset`, aiming at `x`.
    ///
    /// This is what replaces the grapheme column: a column is a property of a
    /// source line, and a wrapped row has no source line of its own.
    pub fn vertical_target(
        &self,
        block: &VisualBlock,
        offset: SourceOffset,
        down: bool,
        x: f32,
        shaper: &dyn LineShaper,
    ) -> VerticalMove {
        let Some(row) = self.row_for_source(offset) else {
            return VerticalMove::Unknown;
        };
        let target = if down { row + 1 } else { row.wrapping_sub(1) };
        if down && target >= self.lines.len() || !down && row == 0 {
            return VerticalMove::PastEdge;
        }
        self.source_at_x(block, target, x, shaper)
            .map_or(VerticalMove::Unknown, VerticalMove::To)
    }

    /// Moves every row onto `current`, for an edit that did not touch this
    /// block. Rows describe the same text at shifted offsets, so the layout
    /// survives typing elsewhere. Returns false when a delta cannot be
    /// transformed, which is the caller's signal to lay the block out again.
    pub fn rebase(&mut self, deltas: &[RevisionDelta], current: Revision) -> bool {
        for row in &mut self.lines {
            let mut range = row.source_range;
            for delta in deltas {
                let Some(next) = delta.transform_range(range) else {
                    return false;
                };
                range = next;
            }
            row.source_range = range;
        }
        self.revision = current;
        true
    }

    /// The part of a source range that falls on one row, in that row's visual
    /// text offsets. Selection and IME underlines are painted per row, so each
    /// row asks only for its own share.
    pub fn visual_range_on_row(
        &self,
        block: &VisualBlock,
        row_index: usize,
        source: SourceRange,
    ) -> Option<Range<usize>> {
        let row = self.lines.get(row_index)?;
        let line = block.lines.get(row.line)?;
        let clipped = SourceRange {
            start: source.start.max(row.source_range.start),
            end: source.end.min(row.source_range.end),
        };
        if clipped.is_empty() {
            return None;
        }
        let start = line
            .source_map
            .source_to_visual(clipped.start, Bias::After)?
            .visual_offset
            .0
            .clamp(row.line_visual_range.start, row.line_visual_range.end);
        let end = line
            .source_map
            .source_to_visual(clipped.end, Bias::Before)?
            .visual_offset
            .0
            .clamp(row.line_visual_range.start, row.line_visual_range.end);
        (start < end).then_some(start..end)
    }
}

/// Block-local visual offset the presented line at `index` starts at.
///
/// Each line contributes its visual text plus one position for the break that
/// follows it, so a block-local offset names a position in the block without
/// naming a line, and offsets stay ordered across lines.
pub fn line_visual_start(block: &VisualBlock, index: usize) -> usize {
    block.lines[..index.min(block.lines.len())]
        .iter()
        .map(|line| line.visual_text.len() + 1)
        .sum()
}

/// Lays out the presented lines of a block into rows for one text column width.
///
/// Wrapping is asked of the shaper per line; everything else — where the source
/// break falls, how tall a row is, where a row sits — is decided here so it is
/// the same with any font.
pub fn layout_block(block: &VisualBlock, width: f32, shaper: &dyn LineShaper) -> BlockLayout {
    let mut lines = Vec::with_capacity(block.lines.len());
    let mut y = 0.0;
    for (index, line) in block.lines.iter().enumerate() {
        let block_start = line_visual_start(block, index);
        let height = line.height();
        let boundaries = fragment_boundaries(line, width, shaper);
        for (fragment, pair) in boundaries.windows(2).enumerate() {
            let (start, end) = (pair[0], pair[1]);
            let last = end == line.visual_text.len();
            let source_start = if fragment == 0 {
                line.source_range.start
            } else {
                source_at_visual(line, start)
            };
            let source_end = if last {
                line.source_range.end
            } else {
                source_at_visual(line, end)
            };
            lines.push(LayoutLine {
                line: index,
                line_id: line.line_id,
                fragment,
                wrap: if last { LineWrap::Hard } else { LineWrap::Soft },
                line_visual_range: start..end,
                visual_range: VisualRange::new(block_start + start, block_start + end),
                source_range: SourceRange {
                    start: source_start,
                    end: source_end.max(source_start),
                },
                y,
                height,
            });
            y += height;
        }
    }
    BlockLayout {
        block: block.id,
        revision: block.revision,
        width,
        lines,
        leading_space: block.leading_space(),
        trailing_space: block.trailing_space(),
    }
}

/// Fragment boundaries of one line, including 0 and the text length, so
/// `windows(2)` yields the fragments. A line with nothing to wrap is one
/// fragment, which is the case for every line until it outgrows the column.
fn fragment_boundaries(line: &VisualLine, width: f32, shaper: &dyn LineShaper) -> Vec<usize> {
    let len = line.visual_text.len();
    // An image row is drawn as a picture, not as text: it has one row whatever
    // its alt text measures.
    if width <= 0.0 || line.image.is_some() {
        return vec![0, len];
    }
    let mut boundaries = Vec::with_capacity(4);
    boundaries.push(0);
    boundaries.extend(
        shaper
            .wrap_boundaries(line, width)
            .into_iter()
            .filter(|offset| {
                *offset > 0 && *offset < len && line.visual_text.is_char_boundary(*offset)
            }),
    );
    // Rows are built from consecutive pairs, so out-of-order or repeated
    // boundaries from a shaper would slice text backwards rather than fail a
    // check somewhere later.
    boundaries.sort_unstable();
    boundaries.dedup();
    boundaries.push(len);
    boundaries
}

/// Source offset a visual offset inside a line stands for. Used only for wrap
/// boundaries, where the visual position is real text rather than a hidden
/// marker, so both affinities agree.
fn source_at_visual(line: &VisualLine, visual: usize) -> SourceOffset {
    line.source_map
        .visual_to_source(VisualOffset(visual), Bias::After)
        .map_or(line.source_range.start, |candidate| candidate.source_offset)
}
