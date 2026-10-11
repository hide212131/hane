use crate::FenceMarkerEdge;
use hane_document::SourceRange;
use std::sync::Arc;

/// Document-wide list information retained by a formal [`crate::BlockIndex`] so a
/// viewport-only presentation can keep the same numbering and nesting while
/// the block's full Markdown parse is still being prepared in the background.
///
/// This is deliberately a render-neutral projection: it contains source
/// ranges and list positions, not display labels or layout coordinates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListProjection {
    pub items: Vec<ListProjectionItem>,
    pub prefixes: Vec<ListProjectionPrefix>,
    pub lists: Vec<ListProjectionList>,
    /// Formal quote containers intersecting the owning block. This keeps quote
    /// depth available when a bounded viewport parse starts on a continuation
    /// line and cannot recover the quote's ancestors from that line alone.
    quotes: Vec<QuoteProjection>,
    rows: Vec<ListProjectionRow>,
    fence_markers: Vec<(SourceRange, FenceMarkerEdge)>,
    /// Formal quote/list prefix ranges paired with the owning quote's source
    /// range, when the marker belongs to a quote. List marker ownership is
    /// resolved through `items` by range so nested list items retain their own
    /// metadata without leaking a `NodeId` across parse trees.
    container_markers: Vec<(SourceRange, Option<SourceRange>, usize)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ListProjectionItem {
    pub item_range: SourceRange,
    pub marker_range: SourceRange,
    pub list_range: SourceRange,
    pub start: Option<u64>,
    pub ordinal: usize,
    pub depth: usize,
    pub item_count: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ListProjectionPrefix {
    pub source_range: SourceRange,
    pub item_range: SourceRange,
    pub columns: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QuoteProjection {
    pub source_range: SourceRange,
    pub depth: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListProjectionList {
    pub source_range: SourceRange,
    pub start: Option<u64>,
    pub item_count: usize,
    pub max_marker_label: String,
    pub max_marker_columns: usize,
    pub marker_labels: Arc<[String]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ListProjectionRow {
    pub source_range: SourceRange,
    /// The deepest formal list item owning this row, when the row is inside a
    /// list. A row can be present without an item when it is a nested code
    /// line in another container such as a block quote; keeping those rows in
    /// the same projection lets viewport-only presentation recover the formal
    /// code-block kind without duplicating a second document-wide index.
    pub(super) item_index: Option<usize>,
    pub(super) is_code_block: bool,
}

impl ListProjection {
    pub(crate) fn new(
        items: Vec<ListProjectionItem>,
        prefixes: Vec<ListProjectionPrefix>,
        lists: Vec<ListProjectionList>,
        quotes: Vec<QuoteProjection>,
        rows: Vec<ListProjectionRow>,
        fence_markers: Vec<(SourceRange, FenceMarkerEdge)>,
        container_markers: Vec<(SourceRange, Option<SourceRange>, usize)>,
    ) -> Self {
        Self {
            items,
            prefixes,
            lists,
            quotes,
            rows,
            fence_markers,
            container_markers,
        }
    }

    /// The formal opening/closing role for `range`, if it is a fence delimiter.
    /// Viewport-only parses may mistake literal fence-shaped code content for
    /// an opening fence; this lets presentation keep only delimiters confirmed
    /// by the document-wide parse when it projects a list-contained code row.
    pub fn fence_marker_edge(&self, range: SourceRange) -> Option<FenceMarkerEdge> {
        self.fence_markers
            .binary_search_by_key(&(range.start, range.end), |marker| {
                (marker.0.start, marker.0.end)
            })
            .ok()
            .map(|index| self.fence_markers[index].1)
    }

    pub fn fence_markers_in(
        &self,
        range: SourceRange,
    ) -> impl Iterator<Item = (SourceRange, FenceMarkerEdge)> + '_ {
        let start = self
            .fence_markers
            .partition_point(|(marker, _)| marker.end <= range.start);
        let end = self
            .fence_markers
            .partition_point(|(marker, _)| marker.start < range.end);
        self.fence_markers[start..end]
            .iter()
            .copied()
            .filter(move |(marker, _)| marker.intersects(range))
    }

    /// Formal quote/list prefix ranges in `range`. Viewport-only parses can
    /// mistake literal `>` or `-` at the start of a code line for a nested
    /// container; presentation keeps only these document-wide-confirmed
    /// ranges when projecting a code row.
    pub fn container_markers_in(
        &self,
        range: SourceRange,
    ) -> impl Iterator<Item = (SourceRange, Option<SourceRange>, usize)> + '_ {
        let start = self
            .container_markers
            .partition_point(|(marker, _, _)| marker.end <= range.start);
        let end = self
            .container_markers
            .partition_point(|(marker, _, _)| marker.start < range.end);
        self.container_markers[start..end]
            .iter()
            .copied()
            .filter(move |(marker, _, _)| marker.intersects(range))
    }

    /// Formal quote containers intersecting `range`, ordered by source range.
    /// Presentation uses this for rows whose bounded parse sees only a lazy
    /// continuation and therefore has no local quote marker or ancestor.
    pub fn quotes_in(&self, range: SourceRange) -> impl Iterator<Item = QuoteProjection> + '_ {
        self.quotes
            .iter()
            .copied()
            .filter(move |quote| quote.source_range.intersects(range))
    }

    pub fn item_for_marker(&self, marker_range: SourceRange) -> Option<&ListProjectionItem> {
        let index = self
            .items
            .binary_search_by_key(&(marker_range.start, marker_range.end), |item| {
                (item.marker_range.start, item.marker_range.end)
            })
            .ok()?;
        self.items.get(index)
    }

    pub fn item_for_source_range(&self, item_range: SourceRange) -> Option<&ListProjectionItem> {
        let index = self
            .items
            .binary_search_by_key(&(item_range.start, item_range.end), |item| {
                (item.item_range.start, item.item_range.end)
            })
            .ok()?;
        self.items.get(index)
    }

    /// Finds the deepest formal list item owning a physical source line. The
    /// row index is built with the formal parse, so a viewport parse does not
    /// have to scan every item in a large list to recover its owner.
    pub fn item_for_range(&self, range: SourceRange) -> Option<&ListProjectionItem> {
        let row = self.row_for_range(range)?;
        self.items.get(row.item_index?)
    }

    /// Reports whether the formal parse presents the physical row as code.
    /// A bounded viewport parse can mistake a container continuation for an
    /// indented code block, or fail to see code nested in a large quote;
    /// callers use this bit only to restore the formal display kind while
    /// preserving genuine literal code.
    pub fn is_code_block_for_range(&self, range: SourceRange) -> Option<bool> {
        Some(self.row_for_range(range)?.is_code_block)
    }

    pub fn list(&self, source_range: SourceRange) -> Option<&ListProjectionList> {
        let index = self
            .lists
            .binary_search_by_key(&(source_range.start, source_range.end), |list| {
                (list.source_range.start, list.source_range.end)
            })
            .ok()?;
        self.lists.get(index)
    }

    pub fn prefixes_in(&self, range: SourceRange) -> impl Iterator<Item = &ListProjectionPrefix> {
        let start = self
            .prefixes
            .partition_point(|prefix| prefix.source_range.end <= range.start);
        let end = self
            .prefixes
            .partition_point(|prefix| prefix.source_range.start < range.end);
        self.prefixes[start..end]
            .iter()
            .filter(move |prefix| prefix.source_range.intersects(range))
    }

    fn row_for_range(&self, range: SourceRange) -> Option<&ListProjectionRow> {
        let index = self
            .rows
            .partition_point(|row| row.source_range.end <= range.start);
        let row = self.rows.get(index)?;
        row.source_range.intersects(range).then_some(row)
    }
}
