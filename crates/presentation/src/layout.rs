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

use crate::{ListCaretOrigin, ListId, VisualBlock, VisualLine, VisualOffset, VisualRange};
use hane_document::{Bias, Revision, RevisionDelta, SourceOffset, SourceRange};
use hane_markdown::BlockId;
use std::collections::{HashMap, HashSet};
use std::ops::Range;

/// Horizontal distance between semantic list depths. This is presentation
/// geometry, not a claim about how many source spaces a Markdown parser
/// consumed.
pub const LIST_DEPTH_INDENT: f32 = 24.0;
/// Horizontal inset between nested quote bodies. This is semantic display
/// geometry; it is independent of how many source spaces follow `>`.
pub const QUOTE_DEPTH_INDENT: f32 = 24.0;
pub const QUOTE_BAR_WIDTH: f32 = 2.0;
pub const QUOTE_BAR_GAP: f32 = 8.0;
const MIN_EFFECTIVE_WRAP_WIDTH: f32 = 1.0;

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
    /// x of the text drawn at the start of this fragment, in the block's text
    /// column. The first fragment of an opening list row starts at its marker;
    /// every hanging fragment starts at the item's body column.
    pub text_x_origin: f32,
    /// x at which the owning list item's body starts, in the block's text
    /// column. Inactive markers may be narrower than this column because the
    /// whole list aligns to its aggregate synthesized label width.
    pub body_x_origin: f32,
    /// Width passed to the shaper for body-column fragments. The first
    /// fragment of an opening list row receives the wider marker-inclusive
    /// budget; this value remains the positive body budget used by later
    /// fragments, even when a deeply nested list leaves no usable room.
    pub effective_width: f32,
    /// Semantic marker position, when this is a list row. Kept separately from
    /// `text_x_origin` so painting can place a marker and body without making
    /// source text carry structural indentation.
    pub marker_x_origin: Option<f32>,
    /// The visual offset at which the item body starts on this line. Hidden
    /// source prefixes have zero visual width, so this may be zero.
    pub body_visual_start: Option<usize>,
    /// The visual range of the marker displayed on this line, if any.
    pub marker_visual_range: Option<Range<usize>>,
    /// Geometry-only gap between the displayed marker and the aligned body
    /// column. It is zero for continuation rows and for a marker already as
    /// wide as the list's aggregate label.
    pub marker_body_gap: f32,
    /// x of the quote bar in the block's text column, when this row is quoted.
    pub quote_bar_x_origin: Option<f32>,
}

impl LayoutLine {
    pub fn bottom(&self) -> f32 {
        self.y + self.height
    }

    fn x_for_visual(&self, line: &VisualLine, visual: usize, shaper: &dyn LineShaper) -> f32 {
        let visual = visual.clamp(self.line_visual_range.start, self.line_visual_range.end);
        if let Some(body) = self.body_visual_start
            && body <= self.line_visual_range.end
            && visual >= body
        {
            let body_fragment_start = body.max(self.line_visual_range.start);
            return self.body_x_origin
                + shaper.x_for_offset(
                    line,
                    body_fragment_start..self.line_visual_range.end,
                    visual,
                );
        }
        self.text_x_origin + shaper.x_for_offset(line, self.line_visual_range.clone(), visual)
    }

    fn visual_for_x(&self, line: &VisualLine, x: f32, shaper: &dyn LineShaper) -> usize {
        if let Some(body) = self.body_visual_start
            && body <= self.line_visual_range.end
        {
            let body_fragment_start = body.max(self.line_visual_range.start);
            if self.marker_body_gap > 0.0
                && body_fragment_start == body
                && let Some(marker) = &self.marker_visual_range
                && marker.start >= self.line_visual_range.start
                && marker.end <= self.line_visual_range.end
            {
                let marker_end_x = self.text_x_origin
                    + shaper.x_for_offset(
                        line,
                        self.line_visual_range.start..marker.end,
                        marker.end,
                    );
                if (marker_end_x..self.body_x_origin).contains(&x) {
                    return body_fragment_start;
                }
            }
            if body_fragment_start < self.line_visual_range.end && x >= self.body_x_origin {
                return shaper
                    .offset_for_x(
                        line,
                        body_fragment_start..self.line_visual_range.end,
                        x - self.body_x_origin,
                    )
                    .clamp(body_fragment_start, self.line_visual_range.end);
            }
            if body_fragment_start > self.line_visual_range.start && x >= self.body_x_origin {
                return body_fragment_start;
            }
        }
        shaper
            .offset_for_x(line, self.line_visual_range.clone(), x - self.text_x_origin)
            .clamp(self.line_visual_range.start, self.line_visual_range.end)
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
    /// Byte offsets of `line.visual_text[fragment]` where it has to break to
    /// fit `width`, returned as absolute offsets into `line.visual_text` and
    /// excluding the fragment's start and end.
    fn wrap_boundaries(&self, line: &VisualLine, fragment: Range<usize>, width: f32) -> Vec<usize>;
    /// x of `offset`, measured from the left edge of `fragment`.
    fn x_for_offset(&self, line: &VisualLine, fragment: Range<usize>, offset: usize) -> f32;
    /// The offset in `fragment` closest to `x`, measured from its left edge.
    fn offset_for_x(&self, line: &VisualLine, fragment: Range<usize>, x: f32) -> usize;
    /// Width of arbitrary presentation text in the line's block font. Layout
    /// uses this for an inactive marker label when the widest item is outside
    /// the currently presented viewport.
    fn width_for_text(&self, line: &VisualLine, text: &str) -> f32;
}

/// Where a source offset sits inside a laid-out block.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutPoint {
    pub row: usize,
    /// Absolute x inside the block's text column, including semantic list
    /// marker/body indentation.
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

    /// Mean height of a visible presented line, used to estimate how far into a
    /// block a scroll position falls before that part of the block has been laid
    /// out. A zero-height line is a collapsed structural row (for example an
    /// inactive fence delimiter), not a visual row; including it in the
    /// denominator would make a visual y position look farther into the block
    /// than it really is before the fence projection is inverted.
    pub fn average_line_height(&self) -> Option<f32> {
        let mut total = 0.0;
        let mut count = 0;
        let mut current_line = None;
        let mut current_height = 0.0;
        for row in &self.lines {
            if current_line != Some(row.line) {
                if current_line.is_some() && current_height > 0.0 {
                    total += current_height;
                    count += 1;
                }
                current_line = Some(row.line);
                current_height = 0.0;
            }
            current_height += row.height;
        }
        if current_line.is_some() && current_height > 0.0 {
            total += current_height;
            count += 1;
        }
        (count > 0).then(|| total / count as f32)
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
            x: row.x_for_visual(line, visual, shaper),
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

    /// The line-local visual offset under `x` on one row.
    pub fn visual_at_x(
        &self,
        block: &VisualBlock,
        row_index: usize,
        x: f32,
        shaper: &dyn LineShaper,
    ) -> Option<VisualOffset> {
        let row = self.lines.get(row_index)?;
        let line = block.lines.get(row.line)?;
        Some(VisualOffset(row.visual_for_x(line, x, shaper)))
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
        self.source_at_x_with_bias(block, row_index, x, shaper, Bias::After)
    }

    /// The source offset at `x`, using the caller's boundary affinity.
    ///
    /// Most geometry queries use [`Bias::After`]. Mouse clicks are different:
    /// a collapsed inline marker can share one visual point with content on
    /// either side, so the UI can preserve the affinity of the row edge that
    /// was actually clicked.
    pub fn source_at_x_with_bias(
        &self,
        block: &VisualBlock,
        row_index: usize,
        x: f32,
        shaper: &dyn LineShaper,
        affinity: Bias,
    ) -> Option<SourceOffset> {
        let row = self.lines.get(row_index)?;
        let line = block.lines.get(row.line)?;
        let visual = self.visual_at_x(block, row_index, x, shaper)?.0;
        Some(
            line.source_map
                .visual_to_source(VisualOffset(visual), affinity)
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
    let marker_widths = list_marker_widths(block, shaper);
    let mut lines = Vec::with_capacity(block.lines.len());
    let mut y = 0.0;
    for (index, line) in block.lines.iter().enumerate() {
        let block_start = line_visual_start(block, index);
        let height = line.height();
        let line_geometry = line_geometry(line, width, shaper, &marker_widths);
        let boundaries = if width <= 0.0 {
            vec![0, line.visual_text.len()]
        } else {
            fragment_boundaries(line, &line_geometry, width, shaper)
        };
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
                text_x_origin: if fragment == 0 {
                    line_geometry.marker_x_origin.unwrap_or(
                        line_geometry.body_x_origin - line_geometry.expanded_prefix_width,
                    )
                } else {
                    line_geometry.body_x_origin
                },
                body_x_origin: line_geometry.body_x_origin,
                effective_width: line_geometry.effective_width,
                marker_x_origin: line_geometry.marker_x_origin,
                body_visual_start: line_geometry.body_visual_start,
                marker_visual_range: line_geometry.marker_visual_range.clone(),
                marker_body_gap: line_geometry.marker_body_gap,
                quote_bar_x_origin: line_geometry.quote_bar_x_origin,
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

#[derive(Clone, Debug)]
struct LineGeometry {
    marker_x_origin: Option<f32>,
    body_x_origin: f32,
    expanded_prefix_width: f32,
    effective_width: f32,
    body_visual_start: Option<usize>,
    marker_visual_range: Option<Range<usize>>,
    marker_body_gap: f32,
    quote_bar_x_origin: Option<f32>,
}

fn list_marker_widths(block: &VisualBlock, shaper: &dyn LineShaper) -> HashMap<ListId, f32> {
    let mut widths: HashMap<ListId, f32> = HashMap::new();
    let mut measured_scales: HashSet<(ListId, u32)> = HashSet::new();
    for line in &block.lines {
        let Some(list) = &line.list else {
            continue;
        };
        let list_id = list.owner.list_id;
        let scale = line.display().font_scale.to_bits();
        if !measured_scales.insert((list_id, scale)) {
            continue;
        }
        let width = list
            .owner
            .alignment
            .marker_labels
            .iter()
            .map(|label| shaper.width_for_text(line, label))
            .fold(0.0, f32::max);
        let width = if width > 0.0 {
            width
        } else {
            shaper.width_for_text(line, &list.owner.alignment.max_marker_label)
        };
        widths
            .entry(list_id)
            .and_modify(|current| *current = current.max(width))
            .or_insert(width);
    }
    widths
}

fn list_depth_x(depth: usize) -> f32 {
    depth.saturating_sub(1) as f32 * LIST_DEPTH_INDENT
}

fn line_geometry(
    line: &VisualLine,
    width: f32,
    shaper: &dyn LineShaper,
    marker_widths: &HashMap<ListId, f32>,
) -> LineGeometry {
    let Some(list) = &line.list else {
        let quote_x = line
            .quote
            .map_or(0.0, |quote| {
                quote.depth.saturating_sub(quote.disclosed_depth) as f32 * QUOTE_DEPTH_INDENT
            });
        return LineGeometry {
            marker_x_origin: None,
            body_x_origin: quote_x,
            expanded_prefix_width: 0.0,
            effective_width: width.max(0.0),
            body_visual_start: None,
            marker_visual_range: None,
            marker_body_gap: 0.0,
            quote_bar_x_origin: (quote_x > 0.0).then_some(
                quote_x - QUOTE_BAR_GAP - QUOTE_BAR_WIDTH,
            ),
        };
    };
    let quote_x = line
        .quote
        .map_or(0.0, |quote| {
            quote.depth.saturating_sub(quote.disclosed_depth) as f32 * QUOTE_DEPTH_INDENT
        });
    let marker_x = quote_x + list_depth_x(list.owner.depth);
    let aggregate_marker_width = marker_widths
        .get(&list.owner.list_id)
        .copied()
        .unwrap_or_else(|| shaper.width_for_text(line, &list.owner.alignment.max_marker_label));
    let disclosed_marker_width = list.marker.as_ref().map_or(0.0, |marker| {
        shaper.x_for_offset(
            line,
            marker.visual_range.start.0..marker.visual_range.end.0,
            marker.visual_range.end.0,
        )
    });
    let body_visual_start = match list.empty_caret_origin {
        Some(ListCaretOrigin::Marker) => None,
        Some(ListCaretOrigin::Body) => Some(0),
        None => Some(list.body_visual_start.0),
    };
    let body_visual_start_offset = list.body_visual_start.0;
    let marker_visual_range = list.marker.as_ref().map(|marker| marker.visual_range);
    let expanded_prefix_width = line
        .source_map
        .segments
        .iter()
        .filter(|segment| {
            segment.visibility == crate::Visibility::ExpandedMarkup
                && segment.visual_range.start.0 < body_visual_start_offset
                && segment.visual_range.end.0 <= body_visual_start_offset
                && marker_visual_range.as_ref() != Some(&segment.visual_range)
        })
        .map(|segment| {
            shaper.x_for_offset(
                line,
                segment.visual_range.start.0..segment.visual_range.end.0,
                segment.visual_range.end.0,
            )
        })
        .sum::<f32>();
    // A disclosed continuation has no marker of its own. Its expanded
    // structural prefix already paints the indentation that leads to the
    // item's body, so adding the aggregate marker width would reserve that
    // column a second time. When the prefix is hidden, keep the aggregate
    // marker column so inactive continuation rows still hang under the body.
    // Synthesized markers participate in the list-wide inactive alignment
    // column. Once a marker is disclosed, the source-visible marker is the
    // coordinate truth: retaining the aggregate column would leave a
    // geometry-only gap between the raw marker and its body.
    let marker_column_width = if list
        .marker
        .as_ref()
        .is_some_and(|marker| !marker.synthesized)
    {
        disclosed_marker_width
    } else if list.marker.is_none() && expanded_prefix_width > 0.0 {
        0.0
    } else {
        aggregate_marker_width.max(disclosed_marker_width)
    };
    let body_x = marker_x + expanded_prefix_width + marker_column_width;
    LineGeometry {
        marker_x_origin: if list.marker.is_some()
            || list.empty_caret_origin == Some(ListCaretOrigin::Marker)
        {
            Some(marker_x)
        } else {
            None
        },
        body_x_origin: body_x,
        expanded_prefix_width,
        effective_width: (width - body_x).max(MIN_EFFECTIVE_WRAP_WIDTH),
        body_visual_start,
        marker_visual_range: list
            .marker
            .as_ref()
            .map(|marker| marker.visual_range.start.0..marker.visual_range.end.0),
        marker_body_gap: list
            .marker
            .as_ref()
            .filter(|marker| marker.synthesized)
            .map_or(0.0, |_| {
                (aggregate_marker_width.max(disclosed_marker_width) - disclosed_marker_width)
                    .max(0.0)
            }),
        quote_bar_x_origin: (quote_x > 0.0).then_some(
            quote_x - QUOTE_BAR_GAP - QUOTE_BAR_WIDTH,
        ),
    }
}

/// Fragment boundaries of one line, including 0 and the text length, so
/// `windows(2)` yields the fragments. A line with nothing to wrap is one
/// fragment, which is the case for every line until it outgrows the column.
fn fragment_boundaries(
    line: &VisualLine,
    geometry: &LineGeometry,
    width: f32,
    shaper: &dyn LineShaper,
) -> Vec<usize> {
    let len = line.visual_text.len();
    // An image row is drawn as a picture, not as text: it has one row whatever
    // its alt text measures.
    if width <= 0.0 || line.image.is_some() {
        return vec![0, len];
    }
    let first_x_origin = geometry
        .marker_x_origin
        .unwrap_or(geometry.body_x_origin - geometry.expanded_prefix_width);
    let first_width =
        (width - first_x_origin - geometry.marker_body_gap).max(MIN_EFFECTIVE_WRAP_WIDTH);
    let mut boundaries = Vec::with_capacity(4);
    boundaries.push(0);
    let valid_boundaries = |start: usize, row_width: f32| {
        let mut candidates = shaper
            .wrap_boundaries(line, start..len, row_width)
            .into_iter()
            .filter(|offset| {
                *offset > start && *offset < len && line.visual_text.is_char_boundary(*offset)
            })
            .collect::<Vec<_>>();
        candidates.sort_unstable();
        candidates.dedup();
        candidates
    };
    // A shaper returns every boundary for the requested stretch, so reuse that
    // result instead of shaping the progressively shorter suffix once per row.
    // Opening list rows are the one exception: the first row has a marker
    // budget, while rows starting at the body use the hanging budget. At most
    // one shaping pass is needed for each of those two width regions.
    let body_boundary = geometry.body_visual_start.filter(|body| *body > 0);
    if let Some(body) = body_boundary {
        let body = body.min(len);
        if body < len {
            let marker_overflows_first_row =
                geometry.marker_visual_range.as_ref().is_some_and(|_| {
                    let opening_width = shaper.x_for_offset(line, 0..body, body);
                    opening_width > first_width
                });
            if marker_overflows_first_row {
                // The disclosed prefix and marker are one indivisible
                // synthesized/source projection. If their aligned column cannot
                // fit in the opening-row budget, let it overflow to the body
                // boundary instead of wrapping either part into fragments that
                // would be painted at the body origin.
                boundaries.push(body);
                boundaries.extend(valid_boundaries(body, geometry.effective_width));
            } else {
                let first_boundaries = valid_boundaries(0, first_width);
                if let Some(split) = first_boundaries.iter().find(|offset| **offset >= body) {
                    boundaries.push(*split);
                    boundaries.extend(valid_boundaries(*split, geometry.effective_width));
                } else {
                    boundaries.extend(first_boundaries);
                }
            }
        }
        // An empty item still owns one opening row. There is no body row to
        // hang from, so the final `len` boundary below must be the only
        // boundary after zero; otherwise it would create an empty fragment.
    } else {
        let row_width = geometry
            .body_visual_start
            .filter(|body| *body == 0)
            .map_or(first_width, |_| geometry.effective_width);
        boundaries.extend(valid_boundaries(0, row_width));
    }
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
