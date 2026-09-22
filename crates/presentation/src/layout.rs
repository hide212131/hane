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

use crate::{BlockKind, LineContext, ListId, VisualBlock, VisualLine, VisualOffset, VisualRange};
use hane_document::{Bias, Revision, RevisionDelta, SourceOffset, SourceRange};
use hane_markdown::{BlockId, TableAlignment, TableProjection, is_table_delimiter};
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
/// Horizontal padding painted inside every inactive table cell.
///
/// Keep this in the layout model so the intrinsic column measurements, cell
/// fragments, and geometry handed to the UI describe the same box.
const TABLE_CELL_PADDING: f32 = 8.0;
const TABLE_CELL_HORIZONTAL_PADDING: f32 = TABLE_CELL_PADDING * 2.0;

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

/// One visual line of a table cell. The range is always a UTF-8 boundary and
/// fragments tile their owning cell's visible range in order. `text_x` is in
/// the table row's coordinate space, while `y` is relative to that row.
#[derive(Clone, Debug, PartialEq)]
pub struct TableCellFragment {
    pub visual_range: Range<usize>,
    pub text_x: f32,
    pub y: f32,
    pub height: f32,
}

/// Geometry for one table cell on a laid-out row. The presentation model owns
/// the source/visual ranges; layout adds the font-dependent x coordinates and
/// the cell's visual fragments.
#[derive(Clone, Debug, PartialEq)]
pub struct TableCellLayout {
    pub column: usize,
    pub visual_range: Range<usize>,
    pub source_range: SourceRange,
    pub alignment: TableAlignment,
    pub x: f32,
    pub width: f32,
    pub text_x: f32,
    pub fragments: Vec<TableCellFragment>,
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
    /// Geometry-only gap between an inactive displayed marker and the aligned
    /// body column. It is zero for continuation rows and disclosed source
    /// markers.
    pub marker_body_gap: f32,
    /// Total geometry-only gap between the rendered prefix and the body. This
    /// includes the quote inset that follows a disclosed outer quote prefix,
    /// plus any inactive marker alignment gap.
    pub body_gap: f32,
    /// x of the quote bar in the block's text column, when this row is quoted.
    pub quote_bar_x_origin: Option<f32>,
    /// Cell geometry for a structured table row. Empty for ordinary rows.
    pub table_cells: Vec<TableCellLayout>,
}

impl LayoutLine {
    pub fn bottom(&self) -> f32 {
        self.y + self.height
    }

    fn table_cell_for_visual(&self, visual: usize) -> Option<&TableCellLayout> {
        self.table_cells
            .iter()
            .enumerate()
            .find(|(index, cell)| {
                cell.visual_range.start <= visual
                    && (visual < cell.visual_range.end
                        || (*index + 1 == self.table_cells.len()
                            && visual == cell.visual_range.end))
            })
            .map(|(_, cell)| cell)
    }

    fn table_cell_for_x(&self, x: f32) -> Option<&TableCellLayout> {
        self.table_cells
            .iter()
            .enumerate()
            .find(|(index, cell)| {
                x >= cell.x
                    && (x < cell.x + cell.width
                        || (*index + 1 == self.table_cells.len() && x <= cell.x + cell.width))
            })
            .map(|(_, cell)| cell)
    }

    fn table_fragment_for_local_y<'a>(
        &self,
        cell: &'a TableCellLayout,
        local_y: f32,
    ) -> Option<&'a TableCellFragment> {
        let local_y = local_y.clamp(0.0, self.height.max(0.0));
        cell.fragments
            .iter()
            .find(|fragment| local_y < fragment.y + fragment.height)
            .or_else(|| cell.fragments.last())
    }

    fn table_fragment_for_visual<'a>(
        &self,
        cell: &'a TableCellLayout,
        visual: usize,
    ) -> Option<&'a TableCellFragment> {
        cell.fragments
            .iter()
            .enumerate()
            .find_map(|(index, fragment)| {
                (visual < fragment.visual_range.end
                    || (index + 1 == cell.fragments.len() && visual == fragment.visual_range.end))
                    .then_some(fragment)
            })
    }

    fn table_fragment_index_for_visual(&self, visual: usize) -> usize {
        self.table_cell_for_visual(visual)
            .and_then(|cell| {
                cell.fragments
                    .iter()
                    .enumerate()
                    .find_map(|(index, fragment)| {
                        (visual < fragment.visual_range.end
                            || (index + 1 == cell.fragments.len()
                                && visual == fragment.visual_range.end))
                            .then_some(index)
                    })
            })
            .unwrap_or(0)
    }

    fn table_fragment_count_for_visual(&self, visual: usize) -> usize {
        self.table_cell_for_visual(visual)
            .map(|cell| cell.fragments.len())
            .unwrap_or(1)
            .max(1)
    }

    fn table_edge_y_for_x(&self, x: f32, down: bool) -> f32 {
        if down {
            return 0.0;
        }
        self.table_cell_for_x(x)
            .and_then(|cell| cell.fragments.last())
            .map_or(0.0, |fragment| fragment.y)
    }

    fn fragment_y_for_visual(&self, visual: usize) -> (f32, f32) {
        let Some(cell) = self.table_cell_for_visual(visual) else {
            return (0.0, self.height);
        };
        self.table_fragment_for_visual(cell, visual)
            .map_or((0.0, self.height), |fragment| (fragment.y, fragment.height))
    }

    fn x_for_visual(&self, line: &VisualLine, visual: usize, shaper: &dyn LineShaper) -> f32 {
        if !self.table_cells.is_empty()
            && let Some(cell) = self.table_cell_for_visual(visual)
        {
            let visual = visual.clamp(cell.visual_range.start, cell.visual_range.end);
            if let Some(fragment) =
                cell.fragments
                    .iter()
                    .enumerate()
                    .find_map(|(index, fragment)| {
                        (visual < fragment.visual_range.end
                            || (index + 1 == cell.fragments.len()
                                && visual == fragment.visual_range.end))
                            .then_some(fragment)
                    })
            {
                return fragment.text_x
                    + shaper.x_for_offset(
                        line,
                        fragment.visual_range.clone(),
                        visual.clamp(fragment.visual_range.start, fragment.visual_range.end),
                    );
            }
            return cell.text_x;
        }
        if let Some(first) = self.table_cells.first()
            && visual < first.visual_range.start
        {
            return first.x;
        }
        if let Some(next) = self
            .table_cells
            .windows(2)
            .find(|cells| visual < cells[1].visual_range.start)
            .map(|cells| &cells[1])
        {
            return next.x;
        }
        if let Some(last) = self.table_cells.last()
            && visual >= last.visual_range.end
        {
            return last.x + last.width;
        }
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

    fn visual_for_xy(
        &self,
        line: &VisualLine,
        x: f32,
        local_y: f32,
        shaper: &dyn LineShaper,
    ) -> usize {
        if let Some(cell) = self.table_cell_for_x(x) {
            let fragment = self.table_fragment_for_local_y(cell, local_y);
            return fragment.map_or(cell.visual_range.start, |fragment| {
                shaper
                    .offset_for_x(
                        line,
                        fragment.visual_range.clone(),
                        (x - fragment.text_x).max(0.0),
                    )
                    .clamp(fragment.visual_range.start, fragment.visual_range.end)
            });
        }
        if let Some(body) = self.body_visual_start
            && body <= self.line_visual_range.end
        {
            let body_fragment_start = body.max(self.line_visual_range.start);
            if self.body_gap > 0.0
                && body_fragment_start == body
                && body > self.line_visual_range.start
            {
                let rendered_prefix_end_x = self.text_x_origin
                    + shaper.x_for_offset(
                        line,
                        self.line_visual_range.start..body_fragment_start,
                        body_fragment_start,
                    );
                if (rendered_prefix_end_x..self.body_x_origin).contains(&x) {
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

    fn visual_for_x(&self, line: &VisualLine, x: f32, shaper: &dyn LineShaper) -> usize {
        self.visual_for_xy(line, x, 0.0, shaper)
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
        let (fragment_y, fragment_height) = row.fragment_y_for_visual(visual);
        Some(LayoutPoint {
            row: row_index,
            x: row.x_for_visual(line, visual, shaper),
            y: self.leading_space + row.y + fragment_y,
            height: fragment_height,
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
        let row_index = self.row_at_y(y)?;
        let row = self.lines.get(row_index)?;
        let local_y = y - self.leading_space - row.y;
        self.source_at_xy(block, row_index, x, local_y, shaper)
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

    /// The line-local visual offset under `(x, y)` on one physical row. Table
    /// rows contain several visual fragments, so the y coordinate selects the
    /// fragment before x is measured against its own aligned text origin.
    pub fn visual_at_xy(
        &self,
        block: &VisualBlock,
        row_index: usize,
        x: f32,
        local_y: f32,
        shaper: &dyn LineShaper,
    ) -> Option<VisualOffset> {
        let row = self.lines.get(row_index)?;
        let line = block.lines.get(row.line)?;
        Some(VisualOffset(row.visual_for_xy(line, x, local_y, shaper)))
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

    /// The source offset at `(x, y)` on one physical row, using the default
    /// boundary affinity. `local_y` is relative to the top of the row.
    pub fn source_at_xy(
        &self,
        block: &VisualBlock,
        row_index: usize,
        x: f32,
        local_y: f32,
        shaper: &dyn LineShaper,
    ) -> Option<SourceOffset> {
        self.source_at_xy_with_bias(block, row_index, x, local_y, shaper, Bias::After)
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

    /// The source offset at `(x, y)`, using the caller's boundary affinity.
    pub fn source_at_xy_with_bias(
        &self,
        block: &VisualBlock,
        row_index: usize,
        x: f32,
        local_y: f32,
        shaper: &dyn LineShaper,
        affinity: Bias,
    ) -> Option<SourceOffset> {
        let row = self.lines.get(row_index)?;
        let line = block.lines.get(row.line)?;
        let visual = self.visual_at_xy(block, row_index, x, local_y, shaper)?.0;
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
        let line = block.lines.get(self.lines[row].line);
        let current_visual = line.and_then(|line| {
            line.source_map
                .source_to_visual(offset, Bias::After)
                .map(|candidate| candidate.visual_offset.0)
        });
        let current_fragment = current_visual
            .map(|visual| self.lines[row].table_fragment_index_for_visual(visual))
            .unwrap_or(0);
        let fragment_count = current_visual.map_or(1, |visual| {
            self.lines[row].table_fragment_count_for_visual(visual)
        });
        if (down && current_fragment + 1 < fragment_count) || (!down && current_fragment > 0) {
            let target_fragment = if down {
                current_fragment + 1
            } else {
                current_fragment - 1
            };
            let local_y = current_visual
                .and_then(|visual| self.lines[row].table_cell_for_visual(visual))
                .and_then(|cell| cell.fragments.get(target_fragment))
                .map_or_else(
                    || {
                        let fragment_height = self.lines[row].height / fragment_count as f32;
                        target_fragment as f32 * fragment_height
                    },
                    |fragment| fragment.y,
                );
            return self
                .source_at_xy(block, row, x, local_y, shaper)
                .map_or(VerticalMove::Unknown, VerticalMove::To);
        }
        let target = if down { row + 1 } else { row.wrapping_sub(1) };
        if down && target >= self.lines.len() || !down && row == 0 {
            return VerticalMove::PastEdge;
        }
        let target_y = self.lines[target].table_edge_y_for_x(x, down);
        self.source_at_xy(block, target, x, target_y, shaper)
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
    if block.kind == BlockKind::TableRow || block.lines.iter().any(|line| line.table_row.is_some())
    {
        return layout_table_block(block, width, shaper);
    }
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
                    line_geometry.text_x_origin
                } else {
                    line_geometry.body_x_origin
                },
                body_x_origin: line_geometry.body_x_origin,
                effective_width: line_geometry.effective_width,
                marker_x_origin: line_geometry.marker_x_origin,
                body_visual_start: line_geometry.body_visual_start,
                marker_visual_range: line_geometry.marker_visual_range.clone(),
                marker_body_gap: line_geometry.marker_body_gap,
                body_gap: line_geometry.body_gap,
                quote_bar_x_origin: line_geometry.quote_bar_x_origin,
                table_cells: Vec::new(),
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

/// Lays out presented table rows on shared intrinsic-width columns.
///
/// Each column is measured across the currently presented header/body cells,
/// including a raw row that is being edited. The formal rows remain
/// authoritative for the table's column count when both forms are present.
/// The preferred width is the widest complete cell, while the minimum width is
/// the widest unbreakable segment in that column. If all preferred widths fit,
/// the unused space remains outside the grid. Otherwise widths are allocated
/// deterministically between minimum and preferred widths, with a final
/// proportional compression when even the minimums do not fit. The latter is
/// deliberately bounded by `width`: cell fragments consume the same safe
/// geometry without making the table widen the editor viewport.
fn layout_table_block(block: &VisualBlock, width: f32, shaper: &dyn LineShaper) -> BlockLayout {
    let formal_columns = block
        .table_projection
        .as_ref()
        .map(|projection| projection.alignments.len())
        .filter(|columns| *columns > 0)
        .or_else(|| {
            block
                .lines
                .iter()
                .filter_map(|line| line.table_row.as_ref().map(|row| row.column_count))
                .max()
        });
    let columns = formal_columns.unwrap_or_else(|| {
        block
            .lines
            .iter()
            .filter_map(|line| {
                editing_table_cells(line, block.table_projection.as_ref()).map(|cells| {
                    cells
                        .iter()
                        .map(|cell| cell.column.saturating_add(1))
                        .max()
                        .unwrap_or(0)
                })
            })
            .max()
            .unwrap_or(0)
    });
    let column_widths = table_column_widths(block, columns, width, shaper);
    let mut column_offsets = Vec::with_capacity(column_widths.len() + 1);
    column_offsets.push(0.0);
    for column_width in &column_widths {
        column_offsets.push(column_offsets.last().copied().unwrap_or(0.0) + *column_width);
    }
    let marker_widths = list_marker_widths(block, shaper);
    let mut lines = Vec::with_capacity(block.lines.len());
    let mut y = 0.0;
    for (index, line) in block.lines.iter().enumerate() {
        if line.table_row.is_none() {
            if let Some(cells) = editing_table_cells(line, block.table_projection.as_ref()) {
                let block_start = line_visual_start(block, index);
                let fragment_height = line.height();
                let cells = table_cell_layouts(
                    line,
                    cells,
                    &column_offsets,
                    &column_widths,
                    fragment_height,
                    shaper,
                );
                let height = table_row_height(&cells, fragment_height);
                lines.push(LayoutLine {
                    line: index,
                    line_id: line.line_id,
                    fragment: 0,
                    wrap: LineWrap::Hard,
                    line_visual_range: 0..line.visual_text.len(),
                    visual_range: VisualRange::new(
                        block_start,
                        block_start + line.visual_text.len(),
                    ),
                    source_range: line.source_range,
                    y,
                    height,
                    text_x_origin: 0.0,
                    body_x_origin: 0.0,
                    effective_width: width.max(MIN_EFFECTIVE_WRAP_WIDTH),
                    marker_x_origin: None,
                    body_visual_start: None,
                    marker_visual_range: None,
                    marker_body_gap: 0.0,
                    body_gap: 0.0,
                    quote_bar_x_origin: None,
                    table_cells: cells,
                });
                y += height;
                continue;
            }
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
                        line_geometry.text_x_origin
                    } else {
                        line_geometry.body_x_origin
                    },
                    body_x_origin: line_geometry.body_x_origin,
                    effective_width: line_geometry.effective_width,
                    marker_x_origin: line_geometry.marker_x_origin,
                    body_visual_start: line_geometry.body_visual_start,
                    marker_visual_range: line_geometry.marker_visual_range.clone(),
                    marker_body_gap: line_geometry.marker_body_gap,
                    body_gap: line_geometry.body_gap,
                    quote_bar_x_origin: line_geometry.quote_bar_x_origin,
                    table_cells: Vec::new(),
                });
                y += height;
            }
            continue;
        }
        let fragment_height = line.height();
        let cells = line.table_row.as_ref().map_or_else(Vec::new, |row| {
            table_cell_layouts(
                line,
                row.cells.clone(),
                &column_offsets,
                &column_widths,
                fragment_height,
                shaper,
            )
        });
        let block_start = line_visual_start(block, index);
        let height = table_row_height(&cells, fragment_height);
        lines.push(LayoutLine {
            line: index,
            line_id: line.line_id,
            fragment: 0,
            wrap: LineWrap::Hard,
            line_visual_range: 0..line.visual_text.len(),
            visual_range: VisualRange::new(block_start, block_start + line.visual_text.len()),
            source_range: line.source_range,
            y,
            height,
            text_x_origin: 0.0,
            body_x_origin: 0.0,
            effective_width: width.max(MIN_EFFECTIVE_WRAP_WIDTH),
            marker_x_origin: None,
            body_visual_start: None,
            marker_visual_range: None,
            marker_body_gap: 0.0,
            body_gap: 0.0,
            quote_bar_x_origin: None,
            table_cells: cells,
        });
        y += height;
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

fn table_cell_layouts(
    line: &VisualLine,
    cells: Vec<crate::TableCellDisplay>,
    column_offsets: &[f32],
    column_widths: &[f32],
    fragment_height: f32,
    shaper: &dyn LineShaper,
) -> Vec<TableCellLayout> {
    cells
        .into_iter()
        .map(|cell| {
            let visual_range = cell.visual_range.start.0..cell.visual_range.end.0;
            let x = column_offsets.get(cell.column).copied().unwrap_or(0.0);
            let column_width = column_widths.get(cell.column).copied().unwrap_or(0.0);
            let inner_width = (column_width - TABLE_CELL_HORIZONTAL_PADDING).max(0.0);
            let fragments = table_cell_fragments(
                line,
                visual_range.clone(),
                x,
                inner_width,
                cell.alignment,
                fragment_height,
                shaper,
            );
            // A wrapped aligned cell can have a short first fragment (for
            // example, a leading space) whose origin is farther right than a
            // later, wider fragment. Keep the cell-level origin at the
            // leftmost fragment origin so source→visual→x coordinates never
            // fall to the left of the cell geometry they belong to.
            let text_x = fragments
                .iter()
                .fold(x + TABLE_CELL_PADDING, |origin, fragment| {
                    origin.min(fragment.text_x)
                });
            TableCellLayout {
                column: cell.column,
                visual_range,
                source_range: cell.source_range,
                alignment: cell.alignment,
                x,
                width: column_width,
                text_x,
                fragments,
            }
        })
        .collect()
}

fn table_row_height(cells: &[TableCellLayout], fragment_height: f32) -> f32 {
    let fragment_count = cells
        .iter()
        .map(|cell| cell.fragments.len())
        .max()
        .unwrap_or(0)
        .max(1);
    fragment_height * fragment_count as f32
}

/// Splits one visible cell into the same font-shaped boundaries used by the
/// ordinary line wrapper. A cell's column width is its outer box, so padding is
/// removed before asking the shaper for breaks. The shaper returns byte offsets;
/// the defensive boundary checks keep a faulty or platform-specific result from
/// ever slicing through a UTF-8 code point.
fn table_cell_fragments(
    line: &VisualLine,
    visual_range: Range<usize>,
    x: f32,
    inner_width: f32,
    alignment: TableAlignment,
    fragment_height: f32,
    shaper: &dyn LineShaper,
) -> Vec<TableCellFragment> {
    if visual_range.is_empty() {
        return Vec::new();
    }

    let mut boundaries = shaper
        .wrap_boundaries(line, visual_range.clone(), inner_width)
        .into_iter()
        .filter(|offset| {
            *offset > visual_range.start
                && *offset < visual_range.end
                && line.visual_text.is_char_boundary(*offset)
        })
        .collect::<Vec<_>>();
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut ranges = Vec::with_capacity(boundaries.len() + 1);
    let mut start = visual_range.start;
    for boundary in boundaries {
        ranges.push(start..boundary);
        start = boundary;
    }
    ranges.push(start..visual_range.end);

    ranges
        .into_iter()
        .enumerate()
        .map(|(index, visual_range)| {
            let text_width = shaper.x_for_offset(line, visual_range.clone(), visual_range.end);
            let text_x = x
                + TABLE_CELL_PADDING
                + match alignment {
                    TableAlignment::Center => ((inner_width - text_width).max(0.0)) / 2.0,
                    TableAlignment::Right => (inner_width - text_width).max(0.0),
                    TableAlignment::Default | TableAlignment::Left => 0.0,
                };
            TableCellFragment {
                visual_range,
                text_x,
                y: index as f32 * fragment_height,
                height: fragment_height,
            }
        })
        .collect()
}

/// Measures the intrinsic widths of one table block and clamps their allocation
/// to the available text column. A formal projection contributes rows outside
/// the current presentation window as well.
fn table_column_widths(
    block: &VisualBlock,
    columns: usize,
    width: f32,
    shaper: &dyn LineShaper,
) -> Vec<f32> {
    if columns == 0 {
        return Vec::new();
    }

    let mut preferred = vec![TABLE_CELL_HORIZONTAL_PADDING; columns];
    let mut minimum = vec![TABLE_CELL_HORIZONTAL_PADDING; columns];
    for line in &block.lines {
        let cells = line
            .table_row
            .as_ref()
            .map(|table| table.cells.clone())
            .or_else(|| editing_table_cells(line, block.table_projection.as_ref()));
        let Some(cells) = cells else {
            continue;
        };
        for cell in cells {
            let column = cell.column;
            let visual_range = cell.visual_range.start.0..cell.visual_range.end.0;
            let Some((preferred_text, minimum_text)) =
                table_cell_intrinsic_widths(line, visual_range, shaper)
            else {
                continue;
            };
            let Some(preferred_column) = preferred.get_mut(column) else {
                continue;
            };
            *preferred_column =
                (*preferred_column).max(TABLE_CELL_HORIZONTAL_PADDING + preferred_text.max(0.0));
            if let Some(minimum_column) = minimum.get_mut(column) {
                *minimum_column =
                    (*minimum_column).max(TABLE_CELL_HORIZONTAL_PADDING + minimum_text.max(0.0));
            }
        }
    }

    // A formal table projection owns every row, including rows clipped out of
    // the current presentation window. Measure those rows through the same
    // inactive table presenter used for visible rows, so source ranges,
    // alignment and the raw inline text all follow one geometry contract.
    if let Some(projection) = &block.table_projection {
        for row in projection.rows.iter() {
            let metric = crate::present_table_line(
                0,
                block.revision,
                row.source_range,
                row.source.as_ref(),
                block.line_height,
                row.header,
                &projection.alignments,
            );
            let Some(table) = metric.table_row.as_ref() else {
                continue;
            };
            for cell in &table.cells {
                let visual_range = cell.visual_range.start.0..cell.visual_range.end.0;
                let Some((preferred_text, minimum_text)) =
                    table_cell_intrinsic_widths(&metric, visual_range, shaper)
                else {
                    continue;
                };
                let Some(preferred_column) = preferred.get_mut(cell.column) else {
                    continue;
                };
                *preferred_column = (*preferred_column)
                    .max(TABLE_CELL_HORIZONTAL_PADDING + preferred_text.max(0.0));
                if let Some(minimum_column) = minimum.get_mut(cell.column) {
                    *minimum_column = (*minimum_column)
                        .max(TABLE_CELL_HORIZONTAL_PADDING + minimum_text.max(0.0));
                }
            }
        }
    }

    let available = if width.is_finite() {
        width.max(0.0)
    } else {
        0.0
    };
    let preferred_total = preferred.iter().sum::<f32>();
    if preferred_total <= available {
        return preferred;
    }

    let minimum_total = minimum.iter().sum::<f32>();
    if minimum_total > available {
        if minimum_total <= 0.0 {
            return vec![0.0; columns];
        }
        return bound_table_column_widths(
            minimum
                .into_iter()
                .map(|column| available * column / minimum_total)
                .collect(),
            available,
        );
    }

    let remaining = available - minimum_total;
    let capacity_total = preferred
        .iter()
        .zip(&minimum)
        .map(|(preferred, minimum)| (preferred - minimum).max(0.0))
        .sum::<f32>();
    if capacity_total <= 0.0 {
        return minimum;
    }
    bound_table_column_widths(
        minimum
            .into_iter()
            .zip(preferred)
            .map(|(minimum, preferred)| {
                minimum + remaining * (preferred - minimum).max(0.0) / capacity_total
            })
            .collect(),
        available,
    )
}

/// Returns cell ranges for a table row that is currently disclosed for
/// editing. Formal rows use the parser-owned source ranges; provisional rows
/// use the existing source↔visual map. In both cases inline marker bytes stay
/// in the source range while the displayed cell range follows the shortened
/// visual text.
fn editing_table_cells(
    line: &VisualLine,
    projection: Option<&TableProjection>,
) -> Option<Vec<crate::TableCellDisplay>> {
    if line.context != LineContext::Table
        || line.kind == BlockKind::TableDelimiter
        || is_table_delimiter(&line.visual_text)
    {
        return None;
    }
    let alignments = projection.map_or(&[][..], |projection| projection.alignments.as_ref());
    crate::table_row_from_projection(line, projection, line.source_range, false, alignments)
        .map(|table| table.cells)
}

/// Keeps the floating-point allocation inside the main panel even when the
/// final proportional sum differs from the target by a rounding unit.
fn bound_table_column_widths(mut widths: Vec<f32>, available: f32) -> Vec<f32> {
    let mut used = 0.0;
    for column in &mut widths {
        let remaining = (available - used).max(0.0);
        *column = (*column).max(0.0).min(remaining);
        used += *column;
    }
    widths
}

/// Returns `(preferred, minimum)` text widths for one presented cell.
///
/// The preferred width measures the complete cell. The minimum width measures
/// the widest non-whitespace run, which is the amount ordinary whitespace
/// wrapping cannot split. Both measurements use the line's shaper, so header
/// weight and inline font changes are respected.
fn table_cell_intrinsic_widths(
    line: &VisualLine,
    visual_range: Range<usize>,
    shaper: &dyn LineShaper,
) -> Option<(f32, f32)> {
    if visual_range.start > visual_range.end
        || visual_range.end > line.visual_text.len()
        || !line.visual_text.is_char_boundary(visual_range.start)
        || !line.visual_text.is_char_boundary(visual_range.end)
    {
        return None;
    }
    let preferred = shaper.x_for_offset(line, visual_range.clone(), visual_range.end);
    let text = &line.visual_text[visual_range.clone()];
    let mut minimum: f32 = 0.0;
    let mut run_start = None;
    for (relative, character) in text.char_indices() {
        let offset = visual_range.start + relative;
        if character.is_whitespace() {
            if let Some(start) = run_start.take() {
                minimum = minimum.max(shaper.x_for_offset(line, start..offset, offset));
            }
        } else if run_start.is_none() {
            run_start = Some(offset);
        }
    }
    if let Some(start) = run_start {
        minimum = minimum.max(shaper.x_for_offset(line, start..visual_range.end, visual_range.end));
    }
    Some((preferred, minimum))
}

#[derive(Clone, Debug)]
struct LineGeometry {
    text_x_origin: f32,
    marker_x_origin: Option<f32>,
    body_x_origin: f32,
    first_row_width: f32,
    effective_width: f32,
    body_visual_start: Option<usize>,
    marker_visual_range: Option<Range<usize>>,
    marker_body_gap: f32,
    body_gap: f32,
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
        let quote_x = line.quote.map_or(0.0, |quote| {
            quote.depth.saturating_sub(quote.disclosed_depth) as f32 * QUOTE_DEPTH_INDENT
        });
        let (quote_prefix_end, quote_prefix_width) = disclosed_quote_prefix(line, shaper);
        let quote_prefix_after_outer = quote_x > 0.0 && quote_prefix_end.is_some();
        let body_x = if quote_prefix_after_outer {
            quote_x + quote_prefix_width
        } else {
            quote_x
        };
        let text_x_origin = if quote_prefix_after_outer {
            0.0
        } else {
            quote_x
        };
        return LineGeometry {
            text_x_origin,
            marker_x_origin: None,
            body_x_origin: body_x,
            first_row_width: if quote_prefix_after_outer {
                width - body_x + quote_prefix_width
            } else {
                width - quote_x
            },
            effective_width: (width - body_x).max(MIN_EFFECTIVE_WRAP_WIDTH),
            body_visual_start: quote_prefix_end.filter(|_| quote_prefix_after_outer),
            marker_visual_range: None,
            marker_body_gap: 0.0,
            body_gap: quote_prefix_end
                .filter(|body| *body > 0)
                .map_or(0.0, |body| {
                    (body_x - text_x_origin - shaper.x_for_offset(line, 0..body, body)).max(0.0)
                }),
            quote_bar_x_origin: (quote_x > 0.0)
                .then_some(quote_prefix_width + quote_x - QUOTE_BAR_GAP - QUOTE_BAR_WIDTH),
        };
    };
    let quote_x = line.quote.map_or(0.0, |quote| {
        quote.depth.saturating_sub(quote.disclosed_depth) as f32 * QUOTE_DEPTH_INDENT
    });
    let (quote_prefix_end, quote_prefix_width) = disclosed_quote_prefix(line, shaper);
    let quote_prefix_after_outer = quote_x > 0.0 && quote_prefix_end.is_some();
    let quote_source_start = line
        .quote_marker_source_ranges
        .first()
        .map(|range| range.start.0);
    let list_prefix_end = list
        .marker
        .as_ref()
        .map(|marker| marker.source_range.end.0)
        .into_iter()
        .chain(
            list.structural_prefixes
                .iter()
                .map(|prefix| prefix.source_range.end.0),
        )
        .max();
    let quote_after_list = quote_source_start.is_some_and(|quote_start| {
        list_prefix_end.is_some_and(|prefix_end| prefix_end <= quote_start)
    });
    let quote_prefix_before_list = quote_prefix_after_outer
        && list.marker.as_ref().is_some_and(|marker| {
            line.quote_marker_visual_ranges
                .first()
                .is_some_and(|quote| quote.start < marker.visual_range.start)
        });
    let marker_x = list_depth_x(list.owner.depth)
        + if quote_prefix_after_outer {
            if quote_prefix_before_list {
                quote_x + quote_prefix_width
            } else {
                0.0
            }
        } else if quote_after_list {
            0.0
        } else {
            quote_x
        };
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
    let mut body_visual_start = Some(list.body_visual_start.0);
    if quote_prefix_after_outer {
        body_visual_start = match (body_visual_start, quote_prefix_end) {
            (Some(body), Some(prefix_end)) => Some(body.max(prefix_end)),
            (body, _) => body,
        };
    }
    let body_visual_start_offset = body_visual_start.unwrap_or(list.body_visual_start.0);
    let marker_visual_range = list.marker.as_ref().map(|marker| marker.visual_range);
    let expanded_prefix_width = line
        .source_map
        .segments
        .iter()
        .filter(|segment| {
            segment.visibility == crate::Visibility::ExpandedMarkup
                && (!quote_prefix_after_outer
                    || !line
                        .quote_marker_visual_ranges
                        .iter()
                        .take(line.quote.map_or(0, |quote| quote.disclosed_depth))
                        .any(|range| *range == segment.visual_range))
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
    // coordinate truth: retaining the aggregate column here would leave a
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
    let marker_body_gap = list
        .marker
        .as_ref()
        .filter(|marker| marker.synthesized)
        .map_or(0.0, |_| {
            (aggregate_marker_width.max(disclosed_marker_width) - disclosed_marker_width).max(0.0)
        });
    let opening_marker = list.marker.is_some();
    let body_x = if quote_prefix_after_outer {
        let body = body_visual_start.unwrap_or(body_visual_start_offset);
        shaper.x_for_offset(line, 0..body, body)
            + quote_x
            + list_depth_x(list.owner.depth)
            + marker_body_gap
    } else {
        marker_x
            + expanded_prefix_width
            + marker_column_width
            + if quote_after_list { quote_x } else { 0.0 }
    };
    let text_x_origin = if quote_prefix_after_outer {
        0.0
    } else if opening_marker {
        marker_x
    } else {
        body_x - expanded_prefix_width
    };
    let first_row_width = if quote_prefix_after_outer {
        body_visual_start.map_or(width - text_x_origin, |body| {
            shaper.x_for_offset(line, 0..body, body) + width - body_x + marker_body_gap
        })
    } else {
        width - text_x_origin
    };
    let body_gap = body_visual_start
        .filter(|body| *body > 0)
        .map_or(marker_body_gap, |body| {
            (body_x - text_x_origin - shaper.x_for_offset(line, 0..body, body)).max(0.0)
        });
    let quote_bar_x_origin = if quote_x > 0.0 {
        let quote_bar_x = if quote_prefix_after_outer && quote_prefix_before_list {
            quote_prefix_width + quote_x - QUOTE_BAR_GAP - QUOTE_BAR_WIDTH
        } else if quote_after_list {
            body_x - QUOTE_BAR_GAP - QUOTE_BAR_WIDTH
        } else {
            quote_x - QUOTE_BAR_GAP - QUOTE_BAR_WIDTH
        };
        Some(quote_bar_x)
    } else {
        None
    };
    LineGeometry {
        text_x_origin,
        marker_x_origin: opening_marker.then_some(marker_x),
        body_x_origin: body_x,
        first_row_width,
        effective_width: (width - body_x).max(MIN_EFFECTIVE_WRAP_WIDTH),
        body_visual_start,
        marker_visual_range: list
            .marker
            .as_ref()
            .map(|marker| marker.visual_range.start.0..marker.visual_range.end.0),
        marker_body_gap,
        body_gap,
        quote_bar_x_origin,
    }
}

fn disclosed_quote_prefix(line: &VisualLine, shaper: &dyn LineShaper) -> (Option<usize>, f32) {
    let Some(quote) = line.quote else {
        return (None, 0.0);
    };
    if quote.disclosed_depth == 0 {
        return (None, 0.0);
    }

    let mut disclosed = 0;
    for marker in &line.quote_marker_visual_ranges {
        disclosed += 1;
        if disclosed == quote.disclosed_depth {
            let visual_end = marker.end.0;
            return (
                Some(visual_end),
                shaper.x_for_offset(line, 0..visual_end, visual_end),
            );
        }
    }
    (None, 0.0)
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
    let first_width =
        (geometry.first_row_width - geometry.marker_body_gap).max(MIN_EFFECTIVE_WRAP_WIDTH);
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
