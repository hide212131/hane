//! Revision-tracked Markdown block boundaries.
//!
//! [`BlockIndex`] answers the two questions the editor asks constantly — which
//! block owns a byte offset, and which source range a block ordinal covers — and
//! it survives edits without re-parsing the document: an edit is absorbed by the
//! block that contains it, only the affected window is re-parsed, and the result
//! is spliced back once the parse has re-synchronized with the untouched blocks
//! that follow it.
//!
//! Invariants:
//!
//! - Blocks *tile* the document. Block `i` runs from its own parsed start to the
//!   next block's start, so the blank lines between two blocks belong to the
//!   block above, block 0 starts at offset 0, and the last block ends at the
//!   document end. Every byte therefore belongs to exactly one block.
//! - The index holds *top-level* blocks only (the children of the parse tree
//!   root). Nesting stays in [`crate::MarkdownTree`].
//! - The index is empty exactly when the document contains no Markdown block at
//!   all, which for CommonMark means the document is blank.
//! - Block byte *lengths*, not absolute offsets, are what the index stores (see
//!   [`crate::block_store`]). An edit inside one block updates one length, and
//!   every later block moves implicitly, so typing never writes to blocks it did
//!   not touch.

use crate::block_store::BlockStore;
use crate::{
    FenceHeightProjection, ListEditProjection, ListProjection, ListProjectionItem,
    ListProjectionList, ListProjectionPrefix, ListProjectionRow, MarkdownParse, MarkdownTree,
    NodeKind, QuoteProjection, TableAlignment, build_list_edit_projection, is_table_delimiter,
    markdown_lines, parse_document,
};
use hane_document::{Revision, RevisionDelta, RopeBuffer, SourceOffset, SourceRange, TextBuffer};
use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Bytes a single incremental update may re-parse while hunting for a
/// re-synchronization boundary before it gives up and invalidates the tail.
/// Bounds the input path: an edit that cannot re-synchronize costs a fixed
/// amount of work rather than an amount proportional to the document.
const RESYNC_BYTE_BUDGET: usize = 256 * 1024;

/// Blocks a single incremental update may pull into the re-parse window. Guards
/// documents made of very many tiny blocks, where the byte budget alone would
/// still allow a long walk.
const RESYNC_BLOCK_BUDGET: usize = 512;

/// Identifies a block across edits. A block keeps its id while its kind survives
/// a re-parse of the window it sits in, so caches keyed by block id stay warm
/// through ordinary typing.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BlockId(pub u64);

/// How much the index knows about a block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Confidence {
    /// Parsed, and re-synchronized with the blocks around it. Its kind and range
    /// are what a full parse of the document would produce.
    Formal,
    /// Its range was rebased through the edits, but the parse that produced its
    /// kind could not be re-synchronized. Usable for display, and replaced as
    /// soon as a formal parse arrives.
    Provisional,
}

/// One block as seen by a caller. Ranges are computed on lookup, so this is a
/// value rather than a borrow into the index.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IndexedBlock {
    pub ordinal: usize,
    pub id: BlockId,
    pub kind: NodeKind,
    pub source_range: SourceRange,
    /// Document revision this block's kind was parsed at. Older than the index
    /// revision means the block's text has not changed since.
    pub revision: Revision,
    pub confidence: Confidence,
    /// Physical source lines the block covers.
    ///
    /// Counted as the CommonMark 0.31.2 source line endings (`\n`, `\r\n` or a
    /// bare `\r`) in the block's bytes, plus one when the block does not end in
    /// one. Defined that way the counts are additive under concatenation, so
    /// merging blocks needs no re-count — and the empty last line of a document
    /// that ends in a line ending belongs to no block, which is what
    /// `hane_presentation::block_heights` accounts for.
    pub line_count: usize,
    /// Number of leading blank source lines included in the tiled span before
    /// the block's first content line. Block zero may own this prefix because
    /// tiling starts at byte zero; keeping the offset in the index lets the UI
    /// locate a fenced block's opening line without rescanning those blanks.
    pub leading_content_lines: usize,
}

/// One cell in the formal source projection of a table row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableProjectionCell {
    pub column: usize,
    pub source_range: SourceRange,
}

/// One physical table row retained by the formal source projection.
///
/// The source text is the exact row slice from the parsed revision. Keeping it
/// here lets presentation measure a row that is outside the current viewport
/// without reparsing a shortened visual line or depending on which rows happen
/// to be materialized by virtualization.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableProjectionRow {
    pub source_range: SourceRange,
    pub source: Arc<str>,
    pub header: bool,
    pub cells: Arc<[TableProjectionCell]>,
}

/// Formal table metadata used by presentation for rows outside the current
/// viewport. The row source is tied to the same formal parse revision as the
/// block, so it is discarded with the projection when an incremental update
/// makes the index provisional.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableProjection {
    pub source_range: SourceRange,
    pub delimiter_range: Option<SourceRange>,
    /// Whether the delimiter's physical source line ends with a line ending.
    /// A caret at `delimiter_range.end` belongs to the delimiter only when
    /// this is false and that offset is the document EOF.
    pub delimiter_has_line_ending: bool,
    pub alignments: Arc<[TableAlignment]>,
    pub rows: Arc<[TableProjectionRow]>,
}

impl IndexedBlock {
    /// Builds the provisional paragraph used when a bounded viewport parse sees
    /// no Markdown block. This keeps UI fallback construction out of the UI
    /// crate, which must not depend on parser syntax vocabulary.
    #[must_use]
    pub fn provisional_paragraph(
        revision: Revision,
        source_range: SourceRange,
        line_count: usize,
    ) -> Self {
        Self {
            ordinal: 0,
            id: BlockId(source_range.start.0 as u64),
            kind: NodeKind::Paragraph,
            source_range,
            revision,
            confidence: Confidence::Provisional,
            line_count,
            leading_content_lines: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Entry {
    id: BlockId,
    kind: NodeKind,
    revision: Revision,
    lines: usize,
    leading_content_lines: usize,
}

/// What one incremental update did. Reported so the caller can measure update
/// time, re-parsed bytes, and invalidated blocks without instrumenting the
/// index itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlockIndexUpdate {
    pub revision: Revision,
    pub reparsed_bytes: usize,
    /// First ordinal removed from the index by the window re-parse.
    pub first_replaced_block: usize,
    /// Blocks removed from the index and replaced by the window re-parse.
    pub replaced_blocks: usize,
    /// Blocks inserted at [`Self::first_replaced_block`].
    pub inserted_blocks: usize,
    /// Blocks after the window that were conservatively marked
    /// [`Confidence::Provisional`] because the parse could not re-synchronize.
    pub invalidated_blocks: usize,
    pub resynchronized: bool,
    pub elapsed: Duration,
}

/// One tiled block: its kind, byte length, physical line count, and the number
/// of leading blank lines in its tiled span.
pub(crate) type TiledBlock = (NodeKind, usize, usize, usize);

/// Counts CommonMark 0.31.2 source line endings in `slice`: `\n`, `\r\n` and a
/// bare `\r` each count once. A block boundary always falls at the start of a
/// line, so a slice that ends in a bare `\r` can never have that `\r`'s
/// partner `\n` sitting in the next block's slice — treating `slice` as
/// self-contained is safe.
fn count_line_endings(slice: &str) -> usize {
    let bytes = slice.as_bytes();
    let mut count = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                count += 1;
                i += 1;
            }
            b'\r' => {
                count += 1;
                i += if bytes.get(i + 1) == Some(&b'\n') {
                    2
                } else {
                    1
                };
            }
            _ => i += 1,
        }
    }
    count
}

fn list_projection_label(start: Option<u64>, ordinal: usize) -> String {
    match start {
        None => "\u{2022} ".to_owned(),
        Some(start) => format!("{}. ", start.saturating_add(ordinal as u64)),
    }
}

fn source_line_ranges(range: SourceRange, source: &str) -> Vec<SourceRange> {
    let mut ranges = Vec::new();
    let mut start = range.start.0;
    while start < range.end.0 {
        let tail = &source[start..range.end.0];
        let end = tail.find(['\r', '\n']).map_or(tail.len(), |offset| {
            offset
                + if tail.as_bytes()[offset] == b'\r'
                    && tail.as_bytes().get(offset + 1) == Some(&b'\n')
                {
                    2
                } else {
                    1
                }
        });
        let next = (start + end).min(range.end.0);
        if next <= start {
            break;
        }
        ranges.push(SourceRange::new(start, next));
        start = next;
    }
    ranges
}

fn source_line_ranges_in(
    parse_range: SourceRange,
    range: SourceRange,
    source: &str,
) -> Vec<SourceRange> {
    let mut ranges = Vec::new();
    let mut start = range.start.0;
    while start < range.end.0 {
        let relative = start.saturating_sub(parse_range.start.0);
        let relative_end = range.end.0.saturating_sub(parse_range.start.0);
        let Some(tail) = source.get(relative..relative_end) else {
            break;
        };
        let end = tail.find(['\r', '\n']).map_or(tail.len(), |offset| {
            offset
                + if tail.as_bytes()[offset] == b'\r'
                    && tail.as_bytes().get(offset + 1) == Some(&b'\n')
                {
                    2
                } else {
                    1
                }
        });
        let next = (start + end).min(range.end.0);
        if next <= start {
            break;
        }
        ranges.push(SourceRange::new(start, next));
        start = next;
    }
    ranges
}

fn build_fence_height_projection(
    block_range: SourceRange,
    parse_range: SourceRange,
    source: &str,
    fence_markers: &[(SourceRange, crate::FenceMarkerEdge)],
    list_item_markers: &[SourceRange],
    quote_markers: &[(SourceRange, SourceRange)],
) -> Option<FenceHeightProjection> {
    let line_ranges = source_line_ranges_in(parse_range, block_range, source);
    let mut rows = Vec::new();
    for (marker, edge) in fence_markers {
        let line_index = line_ranges.partition_point(|line| line.end <= marker.start);
        let line = line_ranges.get(line_index);
        let Some(line) = line else {
            continue;
        };
        if !line.intersects(*marker) {
            continue;
        }
        let item_marker = list_item_markers
            .get(list_item_markers.partition_point(|candidate| candidate.end <= line.start))
            .is_some_and(|candidate| candidate.start < line.end);
        if item_marker {
            continue;
        }
        let remainder_start = marker.end.0.saturating_sub(parse_range.start.0);
        let line_end = line.end.0.saturating_sub(parse_range.start.0);
        let line_start = line.start.0.saturating_sub(parse_range.start.0);
        let Some(remainder) = source.get(remainder_start..line_end) else {
            continue;
        };
        let Some(line_source) = source.get(line_start..line_end) else {
            continue;
        };
        let collapses_when_inactive = match edge {
            // The opening fence's info string is part of the inactive
            // structural row now, so its presence must not reserve a line.
            crate::FenceMarkerEdge::Opening => true,
            // A closing fence is valid only when the bytes after its
            // delimiter are whitespace; keep the existing guard for that
            // edge so a literal closing-lookalike never collapses.
            crate::FenceMarkerEdge::Closing => remainder.trim().is_empty(),
        };
        if collapses_when_inactive {
            let quote_start = quote_markers.partition_point(|(marker, _)| marker.end <= line.start);
            let quote_end = quote_markers.partition_point(|(marker, _)| marker.start < line.end);
            let quote_owners = quote_markers[quote_start..quote_end]
                .iter()
                .map(|(_, owner)| *owner)
                .collect();
            rows.push((
                *line,
                !line_source.ends_with(['\n', '\r']),
                quote_owners,
                line_index,
            ));
        }
    }
    let projection = FenceHeightProjection::from_absolute_rows(block_range, rows);
    (!projection.is_empty()).then_some(projection)
}

fn build_fence_height_projections(
    parsed: &MarkdownParse,
    blocks: &[TiledBlock],
    range: SourceRange,
    source: &str,
) -> Vec<Option<FenceHeightProjection>> {
    let block_ranges = blocks
        .iter()
        .scan(range.start.0, |start, (_, length, _, _)| {
            let block = SourceRange::new(*start, *start + *length);
            *start = block.end.0;
            Some(block)
        })
        .collect::<Vec<_>>();
    let mut list_item_markers = parsed
        .list_item_markers
        .iter()
        .map(|(marker, _)| *marker)
        .collect::<Vec<_>>();
    list_item_markers.sort_by_key(|marker| (marker.start, marker.end));
    let mut quote_markers = parsed
        .quote_markers
        .iter()
        .filter_map(|(marker, owner)| {
            parsed
                .tree
                .node(*owner)
                .map(|quote| (*marker, quote.source_range))
        })
        .collect::<Vec<_>>();
    quote_markers.sort_by_key(|(marker, owner)| (marker.start, marker.end, owner.start));
    block_ranges
        .iter()
        .map(|block_range| {
            let start = parsed
                .fence_marker_edges
                .partition_point(|(marker, _)| marker.end <= block_range.start);
            let end = parsed
                .fence_marker_edges
                .partition_point(|(marker, _)| marker.start < block_range.end);
            build_fence_height_projection(
                *block_range,
                range,
                source,
                &parsed.fence_marker_edges[start..end],
                &list_item_markers,
                &quote_markers,
            )
        })
        .collect()
}
fn build_list_rows(
    block_range: SourceRange,
    source: &str,
    items: &[ListProjectionItem],
    code_blocks: &[SourceRange],
) -> Vec<ListProjectionRow> {
    let mut rows = Vec::new();
    let mut active = Vec::new();
    let mut next_item = 0;
    for source_range in source_line_ranges(block_range, source) {
        while next_item < items.len() && items[next_item].item_range.start < source_range.end {
            active.push(next_item);
            next_item += 1;
        }
        active.retain(|index| items[*index].item_range.end > source_range.start);
        let item_index = active
            .iter()
            .copied()
            .max_by_key(|index| items[*index].depth);
        let code_block = code_blocks.partition_point(|code| code.end <= source_range.start);
        let is_code_block = code_blocks
            .get(code_block)
            .is_some_and(|code| code.intersects(source_range));
        if item_index.is_some() || is_code_block {
            rows.push(ListProjectionRow {
                source_range,
                item_index,
                is_code_block,
            });
        }
    }
    rows
}

fn build_list_projections(
    parsed: &MarkdownParse,
    blocks: &[TiledBlock],
    range: SourceRange,
    source: &str,
) -> Vec<Option<ListProjection>> {
    let block_ranges = blocks
        .iter()
        .scan(range.start.0, |start, (_, length, _, _)| {
            let block = SourceRange::new(*start, *start + *length);
            *start = block.end.0;
            Some(block)
        })
        .collect::<Vec<_>>();
    let mut ordinals = vec![None; parsed.tree.len()];
    let mut lists = Vec::new();
    let mut code_blocks = parsed
        .tree
        .iter()
        .filter_map(|(id, node)| {
            let nested_in_container = parsed.tree.ancestors(id).any(|ancestor| {
                parsed.tree.node(ancestor).is_some_and(|node| {
                    matches!(node.kind, NodeKind::ListItem { .. } | NodeKind::Quote)
                })
            });
            (matches!(node.kind, NodeKind::CodeBlock) && nested_in_container)
                .then_some(node.source_range)
        })
        .collect::<Vec<_>>();
    code_blocks.sort_by_key(|range| (range.start, range.end));
    for (list_id, node) in parsed.tree.iter() {
        let NodeKind::List { start } = node.kind else {
            continue;
        };
        let marker_labels = parsed
            .tree
            .children(list_id)
            .iter()
            .enumerate()
            .map(|(ordinal, _)| list_projection_label(start, ordinal))
            .collect::<Vec<_>>();
        let mut max_marker_label = String::new();
        let mut max_marker_columns = 0;
        for label in &marker_labels {
            let columns = label.chars().count();
            if columns > max_marker_columns {
                max_marker_columns = columns;
                max_marker_label.clone_from(label);
            }
        }
        lists.push(ListProjectionList {
            source_range: node.source_range,
            start,
            item_count: marker_labels.len(),
            max_marker_label,
            max_marker_columns,
            marker_labels: marker_labels.into(),
        });
        for (ordinal, child) in parsed.tree.children(list_id).iter().enumerate() {
            ordinals[child.0] = Some((list_id, ordinal));
        }
    }
    let mut items = vec![Vec::new(); blocks.len()];
    for (marker_range, item_id) in &parsed.list_item_markers {
        let Some(item) = parsed.tree.node(*item_id) else {
            continue;
        };
        let Some((list_id, ordinal)) = ordinals.get(item_id.0).and_then(|entry| *entry) else {
            continue;
        };
        let Some(NodeKind::List { start }) = parsed.tree.node(list_id).map(|node| node.kind) else {
            continue;
        };
        let Some(list) = parsed.tree.node(list_id) else {
            continue;
        };
        let Some(block) = block_ranges.iter().position(|block| {
            block.start <= item.source_range.start && item.source_range.start < block.end
        }) else {
            continue;
        };
        items[block].push(ListProjectionItem {
            item_range: item.source_range,
            marker_range: *marker_range,
            list_range: list.source_range,
            start,
            ordinal,
            depth: parsed.tree.list_depth(*item_id),
            item_count: parsed.tree.children(list_id).len(),
        });
    }
    let mut prefixes = vec![Vec::new(); blocks.len()];
    for (source_range, item_id, columns) in &parsed.list_structural_prefixes {
        let Some(item) = parsed.tree.node(*item_id) else {
            continue;
        };
        let Some(block) = block_ranges
            .iter()
            .position(|block| block.start <= source_range.start && source_range.start < block.end)
        else {
            continue;
        };
        prefixes[block].push(ListProjectionPrefix {
            source_range: *source_range,
            item_range: item.source_range,
            columns: *columns,
        });
    }
    for block_items in &mut items {
        block_items.sort_by_key(|item| (item.item_range.start, item.item_range.end));
    }
    for block_prefixes in &mut prefixes {
        block_prefixes.sort_by_key(|prefix| (prefix.source_range.start, prefix.source_range.end));
    }
    let mut container_markers = parsed
        .quote_markers
        .iter()
        .map(|(marker, owner)| {
            (
                *marker,
                parsed.tree.node(*owner).map(|quote| quote.source_range),
                parsed
                    .tree
                    .ancestors(*owner)
                    .filter(|ancestor| {
                        parsed
                            .tree
                            .node(*ancestor)
                            .is_some_and(|node| node.kind == NodeKind::Quote)
                    })
                    .count(),
            )
        })
        .chain(
            parsed
                .list_item_markers
                .iter()
                .map(|(marker, _)| (*marker, None, 0)),
        )
        .chain(
            parsed
                .list_structural_prefixes
                .iter()
                .map(|(marker, _, _)| (*marker, None, 0)),
        )
        .collect::<Vec<_>>();
    container_markers.sort_by_key(|(marker, _, _)| (marker.start, marker.end));
    container_markers.dedup_by_key(|(marker, _, _)| (marker.start, marker.end));
    let mut quotes = parsed
        .tree
        .iter()
        .filter(|(_, node)| node.kind == NodeKind::Quote)
        .map(|(id, node)| QuoteProjection {
            source_range: node.source_range,
            depth: parsed
                .tree
                .ancestors(id)
                .filter(|ancestor| {
                    parsed
                        .tree
                        .node(*ancestor)
                        .is_some_and(|node| node.kind == NodeKind::Quote)
                })
                .count(),
        })
        .collect::<Vec<_>>();
    quotes.sort_by_key(|quote| (quote.source_range.start, quote.source_range.end));
    let mut quotes_by_block = vec![Vec::new(); block_ranges.len()];
    for quote in quotes {
        let block = block_ranges.partition_point(|range| range.end <= quote.source_range.start);
        if block_ranges
            .get(block)
            .is_some_and(|range| range.start <= quote.source_range.start)
        {
            quotes_by_block[block].push(quote);
        }
    }
    blocks
        .iter()
        .enumerate()
        .map(|(block, _)| {
            let block_range = block_ranges[block];
            let block_items = std::mem::take(&mut items[block]);
            let block_prefixes = std::mem::take(&mut prefixes[block]);
            let block_lists = lists
                .iter()
                .filter(|list| {
                    block_items
                        .iter()
                        .any(|item| item.list_range == list.source_range)
                })
                .cloned()
                .collect();
            let rows = build_list_rows(block_range, source, &block_items, &code_blocks);
            let fence_start = parsed
                .fence_marker_edges
                .partition_point(|(marker, _)| marker.end <= block_range.start);
            let fence_end = parsed
                .fence_marker_edges
                .partition_point(|(marker, _)| marker.start < block_range.end);
            let block_fence_markers = parsed.fence_marker_edges[fence_start..fence_end].to_vec();
            let container_start =
                container_markers.partition_point(|(marker, _, _)| marker.end <= block_range.start);
            let container_end =
                container_markers.partition_point(|(marker, _, _)| marker.start < block_range.end);
            let block_container_markers =
                container_markers[container_start..container_end].to_vec();
            let block_quotes = std::mem::take(&mut quotes_by_block[block]);
            if block_items.is_empty() && rows.is_empty() && block_quotes.is_empty() {
                None
            } else {
                Some(ListProjection::new(
                    block_items,
                    block_prefixes,
                    block_lists,
                    block_quotes,
                    rows,
                    block_fence_markers,
                    block_container_markers,
                ))
            }
        })
        .collect()
}

fn build_table_projections(
    parsed: &MarkdownParse,
    blocks: &[TiledBlock],
    range: SourceRange,
    source: &str,
) -> HashMap<BlockId, TableProjection> {
    let block_ranges = blocks
        .iter()
        .scan(range.start.0, |start, (_, length, _, _)| {
            let block = SourceRange::new(*start, *start + *length);
            *start = block.end.0;
            Some(block)
        })
        .collect::<Vec<_>>();
    parsed
        .table_parses
        .iter()
        .filter_map(|table| {
            let node = parsed.tree.node(table.node)?;
            let ordinal = block_ranges.iter().position(|block| {
                block.start <= node.source_range.start && node.source_range.start < block.end
            })?;
            let block_range = block_ranges[ordinal];
            let delimiter_range = source_line_ranges_in(range, block_range, source)
                .into_iter()
                .find(|line| {
                    let relative = line.start.0.saturating_sub(range.start.0);
                    let end = line.end.0.saturating_sub(range.start.0);
                    source.get(relative..end).is_some_and(is_table_delimiter)
                });
            let delimiter_has_line_ending = delimiter_range.is_some_and(|delimiter| {
                let relative = delimiter.start.0.saturating_sub(range.start.0);
                let end = delimiter.end.0.saturating_sub(range.start.0);
                source
                    .get(relative..end)
                    .is_some_and(|text| text.ends_with(['\n', '\r']))
            });
            let rows = parsed
                .tree
                .children(table.node)
                .iter()
                .filter_map(|row_id| {
                    let row = parsed.tree.node(*row_id)?;
                    let header = row.kind == NodeKind::TableHead;
                    if !header && row.kind != NodeKind::TableRow {
                        return None;
                    }
                    let source_start = row.source_range.start.0.checked_sub(range.start.0)?;
                    let source_end = row.source_range.end.0.checked_sub(range.start.0)?;
                    let source = source.get(source_start..source_end)?.to_owned().into();
                    let cells = parsed
                        .tree
                        .children(*row_id)
                        .iter()
                        .enumerate()
                        .filter_map(|(column, cell_id)| {
                            let cell = parsed.tree.node(*cell_id)?;
                            (cell.kind == NodeKind::TableCell
                                && (table.alignments.is_empty() || column < table.alignments.len()))
                            .then_some(TableProjectionCell {
                                column,
                                source_range: cell.source_range,
                            })
                        })
                        .collect::<Vec<_>>()
                        .into();
                    Some(TableProjectionRow {
                        source_range: row.source_range,
                        source,
                        header,
                        cells,
                    })
                })
                .collect::<Vec<_>>()
                .into();
            Some((
                BlockId(ordinal as u64),
                TableProjection {
                    source_range: node.source_range,
                    delimiter_range,
                    delimiter_has_line_ending,
                    alignments: table.alignments.clone(),
                    rows,
                },
            ))
        })
        .collect()
}

/// Tiles one parsed slice into block spans covering `range` exactly: each
/// top-level block runs from its own start to the next block's start, the first
/// starts at `range.start`, and the last ends at `range.end`. Returns no block
/// when the slice parses to nothing, which is the caller's signal that the slice
/// is blank.
///
/// Line counts are taken here because this is the one place that already holds
/// the block's bytes; resolving them from the rope later costs a traversal per
/// block, which the input path cannot afford.
pub(crate) fn tiled_blocks(
    tree: &MarkdownTree,
    range: SourceRange,
    source: &str,
) -> Vec<TiledBlock> {
    let starts = tree
        .children(MarkdownTree::ROOT)
        .iter()
        .filter_map(|id| tree.node(*id))
        .map(|node| (node.kind, node.source_range.start.0))
        .collect::<Vec<_>>();
    starts
        .iter()
        .enumerate()
        .map(|(index, (kind, block_start))| {
            // Leading bytes before the first block belong to block 0.
            let start = if index == 0 {
                range.start.0
            } else {
                *block_start
            };
            let end = starts
                .get(index + 1)
                .map_or(range.end.0, |(_, next)| *next)
                .max(start);
            let slice = &source[start - range.start.0..end - range.start.0];
            let endings = count_line_endings(slice);
            let ends_with_line_ending = slice.ends_with(['\n', '\r']);
            let lines = endings + usize::from(!ends_with_line_ending);
            let leading_content_lines = markdown_lines(slice)
                .take_while(|line| line.trim().is_empty())
                .count();
            (*kind, end - start, lines, leading_content_lines)
        })
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockIndex {
    revision: Revision,
    store: BlockStore<Entry>,
    next_id: u64,
    /// Compact list context from the last formal full parse. Incremental
    /// updates clear it because a changed list can alter every later ordinal.
    list_projections: Vec<Option<ListProjection>>,
    /// Lightweight block-local fence geometry, rebuilt by both formal and
    /// incremental parses so editing never has to wait for formal list semantics
    /// merely to keep virtualized heights stable.
    fence_height_projections: HashMap<BlockId, FenceHeightProjection>,
    /// Formal table metadata; cleared by incremental edits and repopulated by
    /// the next formal parse so stale alignment cannot reach the renderer.
    table_projections: HashMap<BlockId, TableProjection>,
    /// First ordinal of the conservatively invalidated tail, if any. Invalidation
    /// always covers a suffix, so one ordinal answers "is this block provisional"
    /// in constant time instead of writing a flag into every affected block.
    provisional_from: Option<usize>,
}

impl BlockIndex {
    /// Full parse. This is the formal path and is meant for a background job:
    /// cost is proportional to the document.
    pub fn build(revision: Revision, source: &str) -> Self {
        let range = SourceRange::new(0, source.len());
        let parsed = parse_document(revision, range, source);
        let blocks = tiled_blocks(&parsed.tree, range, source);
        let list_projections = build_list_projections(&parsed, &blocks, range, source);
        let table_projections = build_table_projections(&parsed, &blocks, range, source);
        let fence_height_by_ordinal =
            build_fence_height_projections(&parsed, &blocks, range, source);
        let fence_height_projections = fence_height_by_ordinal
            .into_iter()
            .enumerate()
            .filter_map(|(ordinal, projection)| {
                projection.map(|projection| (BlockId(ordinal as u64), projection))
            })
            .collect::<HashMap<_, _>>();
        let next_id = blocks.len() as u64;
        let store = BlockStore::new(blocks.into_iter().enumerate().map(
            |(index, (kind, length, lines, leading_content_lines))| {
                (
                    Entry {
                        id: BlockId(index as u64),
                        kind,
                        revision,
                        lines,
                        leading_content_lines,
                    },
                    length,
                )
            },
        ));
        Self {
            revision,
            store,
            next_id,
            list_projections,
            fence_height_projections,
            table_projections,
            provisional_from: None,
        }
    }

    pub fn from_buffer(buffer: &RopeBuffer) -> Self {
        Self::build(buffer.revision(), &buffer.full_text())
    }

    /// Document revision the block spans are expressed in.
    pub fn revision(&self) -> Revision {
        self.revision
    }

    pub fn len(&self) -> usize {
        self.store.len()
    }

    pub fn is_empty(&self) -> bool {
        self.store.is_empty()
    }

    /// Total bytes owned by blocks. Equals the document length whenever the
    /// document holds at least one block.
    pub fn covered_bytes(&self) -> usize {
        self.store.total_bytes()
    }

    /// True when some block's kind could not be re-synchronized and only a full
    /// parse can restore it.
    pub fn has_provisional_blocks(&self) -> bool {
        self.provisional_from.is_some()
    }

    fn confidence(&self, ordinal: usize) -> Confidence {
        match self.provisional_from {
            Some(from) if ordinal >= from => Confidence::Provisional,
            _ => Confidence::Formal,
        }
    }

    fn span(&self, ordinal: usize) -> SourceRange {
        let start = self.store.start(ordinal);
        SourceRange::new(start, start + self.store.length(ordinal))
    }

    /// Source range of a block ordinal.
    pub fn block(&self, ordinal: usize) -> Option<IndexedBlock> {
        let (entry, length) = self.store.get(ordinal)?;
        let start = self.store.start(ordinal);
        Some(self.indexed(ordinal, entry, SourceRange::new(start, start + length)))
    }

    fn indexed(&self, ordinal: usize, entry: Entry, source_range: SourceRange) -> IndexedBlock {
        IndexedBlock {
            ordinal,
            id: entry.id,
            kind: entry.kind,
            source_range,
            revision: entry.revision,
            confidence: self.confidence(ordinal),
            line_count: entry.lines,
            leading_content_lines: entry.leading_content_lines,
        }
    }

    /// Ordinal of the block owning `offset`: a Fenwick search plus a scan
    /// bounded by the chunk size. The document end belongs to the last block.
    pub fn ordinal_at(&self, offset: SourceOffset) -> Option<usize> {
        if offset.0 > self.covered_bytes() {
            return None;
        }
        self.store.ordinal_at(offset.0)
    }

    /// Block owning `offset`. See [`BlockIndex::ordinal_at`] for the cost.
    pub fn block_at(&self, offset: SourceOffset) -> Option<IndexedBlock> {
        self.block(self.ordinal_at(offset)?)
    }

    /// Document-wide list context for a block from the last formal parse.
    /// Provisional or stale block values never receive a projection.
    pub fn list_projection(&self, block: &IndexedBlock) -> Option<&ListProjection> {
        let current = self.block(block.ordinal)?;
        (current.id == block.id
            && current.source_range == block.source_range
            && current.confidence == Confidence::Formal)
            .then(|| {
                self.list_projections
                    .get(block.ordinal)
                    .and_then(Option::as_ref)
            })
            .flatten()
    }

    /// Builds the exact-current-revision list editing projection for the block
    /// containing `offset`. This is intentionally synchronous and local to the
    /// caret's block: the input path must be able to apply Enter and then Tab
    /// without waiting for the background formal projection.
    pub fn list_edit_projection_at(
        &self,
        buffer: &RopeBuffer,
        offset: SourceOffset,
    ) -> Option<ListEditProjection> {
        if self.revision != buffer.revision() {
            return None;
        }
        let block = self.block_at(offset)?;
        let source = buffer.text(block.source_range).ok()?;
        let parsed = parse_document(buffer.revision(), block.source_range, &source);
        Some(build_list_edit_projection(&parsed, &source))
    }

    /// Height-only fenced-code projection. Unlike list semantics this remains
    /// available after an incremental parse, as long as the block itself is not
    /// in the conservatively invalidated tail.
    pub fn fence_height_projection(&self, block: &IndexedBlock) -> Option<&FenceHeightProjection> {
        let current = self.block(block.ordinal)?;
        (current.id == block.id
            && current.source_range == block.source_range
            && current.confidence == Confidence::Formal)
            .then(|| self.fence_height_projections.get(&block.id))
            .flatten()
    }

    /// Formal table metadata for a current block. Provisional/stale values are
    /// deliberately withheld; presentation can safely fall back to raw source.
    pub fn table_projection(&self, block: &IndexedBlock) -> Option<&TableProjection> {
        let current = self.block(block.ordinal)?;
        (current.id == block.id
            && current.source_range == block.source_range
            && current.confidence == Confidence::Formal)
            .then(|| self.table_projections.get(&block.id))
            .flatten()
    }

    /// Every block in document order.
    pub fn blocks(&self) -> impl Iterator<Item = IndexedBlock> + '_ {
        self.blocks_from(0)
    }

    fn blocks_from(&self, ordinal: usize) -> impl Iterator<Item = IndexedBlock> + '_ {
        self.store
            .iter_from(ordinal)
            .map(move |(ordinal, entry, start, length)| {
                self.indexed(ordinal, entry, SourceRange::new(start, start + length))
            })
    }

    /// Blocks overlapping `range`, after a seek to the first one. An empty range
    /// yields the single block that owns the offset.
    pub fn blocks_in(&self, range: SourceRange) -> impl Iterator<Item = IndexedBlock> + '_ {
        let first = self.ordinal_at(range.start).unwrap_or(0);
        self.blocks_from(first).take_while(move |block| {
            block.ordinal == first || block.source_range.start.0 < range.end.0
        })
    }

    /// Rebases the index onto `buffer` and re-parses only what the edits could
    /// have changed. `deltas` must chain from this index's revision to the
    /// buffer's, as returned by [`RopeBuffer::deltas_since`].
    ///
    /// Non-intersecting blocks are rebased implicitly: an edit changes the byte
    /// length of the blocks it touches, and every later block's start moves with
    /// it. Only blocks the edits intersect are replaced.
    pub fn update(&mut self, buffer: &RopeBuffer, deltas: &[RevisionDelta]) -> BlockIndexUpdate {
        let started = Instant::now();
        let revision = buffer.revision();
        let finish = |index: &mut Self,
                      reparsed_bytes,
                      first_replaced_block,
                      replaced_blocks,
                      inserted_blocks,
                      invalidated_blocks,
                      resynchronized| {
            index.revision = revision;
            BlockIndexUpdate {
                revision,
                reparsed_bytes,
                first_replaced_block,
                replaced_blocks,
                inserted_blocks,
                invalidated_blocks,
                resynchronized,
                elapsed: started.elapsed(),
            }
        };
        if deltas.is_empty() {
            return finish(self, 0, 0, 0, 0, 0, true);
        }
        self.list_projections.clear();
        self.table_projections.clear();
        // Only a document with no block at all indexes to nothing, so this
        // rebuild parses a blank (hence tiny) document.
        if self.is_empty() {
            let rebuilt = Self::build(revision, &buffer.full_text());
            let bytes = rebuilt.covered_bytes();
            let blocks = rebuilt.len();
            *self = rebuilt;
            return finish(self, bytes, 0, 0, blocks, 0, true);
        }

        let Some((dirty_first, dirty_last)) = self.absorb_edits(deltas, revision) else {
            let replaced = self.len();
            let rebuilt = Self::build(revision, &buffer.full_text());
            let bytes = rebuilt.covered_bytes();
            let inserted = rebuilt.len();
            *self = rebuilt;
            return finish(self, bytes, 0, replaced, inserted, 0, true);
        };

        // The block before the dirty run joins the window: an edit can merge its
        // block with the one above (a deleted blank line, a new setext
        // underline), and that merge reaches at most one block back.
        let window_first = dirty_first.saturating_sub(1);
        let mut window_last = (dirty_last + 1).min(self.len() - 1);
        let mut reparsed_bytes = 0;
        loop {
            let window = SourceRange::new(
                self.span(window_first).start.0,
                self.span(window_last).end.0,
            );
            let Ok(text) = buffer.text(window) else {
                // The window is inside the buffer by construction; if reading it
                // ever fails, say so rather than leaving stale kinds marked
                // formal.
                self.provisional_from = Some(
                    self.provisional_from
                        .map_or(window_first, |at| at.min(window_first)),
                );
                let invalidated = self.len() - window_first;
                return finish(self, reparsed_bytes, window_first, 0, 0, invalidated, false);
            };
            reparsed_bytes += window.len_bytes();
            let parsed = parse_document(revision, window, &text);
            let blocks = tiled_blocks(&parsed.tree, window, &text);
            let fence_height_projections =
                build_fence_height_projections(&parsed, &blocks, window, &text);
            // Re-synchronized when the window's last parsed block lands exactly
            // on the boundary and kind the index already has for the untouched
            // block that closes the window. Everything after that boundary is
            // then still valid, because its text did not change. A window that
            // reaches the document end has nothing after it to disagree with.
            let tail_start = self.span(window_last).start.0;
            let tail_kind = self.store.get(window_last).map(|(entry, _)| entry.kind);
            let resynchronized = window.end.0 == buffer.len_bytes().0
                || blocks.last().is_some_and(|(kind, length, _, _)| {
                    Some(*kind) == tail_kind && window.end.0 - *length == tail_start
                });
            let can_grow = window_last + 1 < self.len()
                && reparsed_bytes < RESYNC_BYTE_BUDGET
                && window_last - window_first < RESYNC_BLOCK_BUDGET;
            if !resynchronized && can_grow {
                window_last += 1;
                continue;
            }
            let replaced = window_last + 1 - window_first;
            let inserted = self.splice_window(
                window_first..window_last + 1,
                &blocks,
                &fence_height_projections,
                revision,
            );
            let invalidated = if resynchronized {
                0
            } else {
                // The parse could not prove where the window ends, so every
                // block after it may have moved or changed kind. Keep their
                // rebased spans (offsets stay usable) but stop claiming their
                // kinds are current.
                let from = window_first + inserted;
                self.provisional_from = Some(self.provisional_from.map_or(from, |at| at.min(from)));
                self.len() - from
            };
            return finish(
                self,
                reparsed_bytes,
                window_first,
                replaced,
                inserted,
                invalidated,
                resynchronized,
            );
        }
    }

    /// Applies each edit's byte delta to the blocks it intersects and returns the
    /// ordinal range that needs re-parsing. Returns `None` when an edit falls
    /// outside the indexed bytes, which means the index no longer describes this
    /// document and the caller must rebuild.
    fn absorb_edits(
        &mut self,
        deltas: &[RevisionDelta],
        revision: Revision,
    ) -> Option<(usize, usize)> {
        let mut dirty: Option<(usize, usize)> = None;
        for delta in deltas {
            let edit = delta.edited_source_range_before;
            if edit.end.0 > self.covered_bytes() {
                return None;
            }
            let first = self.store.ordinal_at(edit.start.0)?;
            let last = self.store.ordinal_at(edit.end.0)?;
            let start = self.span(first).start.0;
            let end = self.span(last).end.0;
            let combined = (end - start).checked_add_signed(delta.byte_delta)?;
            self.merge_run(first, last, combined, revision);
            // Merging collapsed `first..=last` into `first`, so ordinals recorded
            // by earlier deltas move down by the blocks that disappeared.
            let remap = |ordinal: usize| {
                if ordinal <= first {
                    ordinal
                } else if ordinal <= last {
                    first
                } else {
                    ordinal - (last - first)
                }
            };
            dirty = Some(match dirty {
                None => (first, first),
                Some((low, high)) => (remap(low).min(first), remap(high).max(first)),
            });
        }
        dirty
    }

    /// Collapses `first..=last` into one entry of `length` bytes. The common case
    /// (an edit inside a single block) touches one length and nothing else.
    fn merge_run(&mut self, first: usize, last: usize, length: usize, revision: Revision) {
        let Some((mut entry, _)) = self.store.get(first) else {
            return;
        };
        entry.revision = revision;
        if first == last {
            self.store.set_payload(first, entry);
            self.store.set_length(first, length);
            return;
        }
        // Line counts are additive under concatenation, so a merged run carries
        // the sum. The edit itself may have added or removed newlines; the window
        // re-parse that follows in the same update replaces the count exactly.
        entry.lines = (first..=last)
            .filter_map(|ordinal| self.store.get(ordinal))
            .map(|(entry, _)| entry.lines)
            .sum();
        // Only the first block id survives the merge. Height projections are
        // keyed by block id, so drop every projection owned by an entry that
        // disappears here instead of leaving unreachable row vectors behind.
        for ordinal in first + 1..=last {
            if let Some((removed, _)) = self.store.get(ordinal) {
                self.fence_height_projections.remove(&removed.id);
            }
        }
        self.store.set_payload(first, entry);
        self.store.splice(first..last + 1, &[(entry, length)]);
        if let Some(from) = self.provisional_from {
            self.provisional_from = Some(if from <= first {
                from
            } else if from <= last {
                first
            } else {
                from - (last - first)
            });
        }
    }

    /// Replaces the entries covering `window` with freshly parsed blocks and
    /// returns how many were inserted. Ids are carried over from the front and
    /// the back while kinds still line up, so an edit inside a block leaves that
    /// block's identity — and any cache keyed by it — intact.
    fn splice_window(
        &mut self,
        window: Range<usize>,
        blocks: &[TiledBlock],
        fence_height_projections: &[Option<FenceHeightProjection>],
        revision: Revision,
    ) -> usize {
        let window_start = self.store.start(window.start);
        let existing = self
            .store
            .iter_from(window.start)
            .take(window.len())
            .collect::<Vec<_>>();
        let previous = existing
            .iter()
            .map(|(_, entry, _, _)| *entry)
            .collect::<Vec<_>>();
        // Old and new blocks tile the same bytes, so their offsets within the
        // window are directly comparable. Match from both ends while a block
        // still starts (or ends) where it did and kept its kind: inserting a
        // block in the middle then leaves the blocks around it untouched.
        let old_offsets = existing
            .iter()
            .map(|(_, _, start, length)| (start - window_start, start + length - window_start))
            .collect::<Vec<_>>();
        let new_offsets = blocks
            .iter()
            .scan(0, |start, (_, length, _, _)| {
                let span = (*start, *start + length);
                *start = span.1;
                Some(span)
            })
            .collect::<Vec<_>>();
        let mut ids = vec![None; blocks.len()];
        let mut front = 0;
        while front < blocks.len()
            && front < previous.len()
            && blocks[front].0 == previous[front].kind
            && new_offsets[front].0 == old_offsets[front].0
        {
            ids[front] = Some(previous[front].id);
            front += 1;
        }
        let mut back = 0;
        while back + front < blocks.len()
            && back + front < previous.len()
            && blocks[blocks.len() - 1 - back].0 == previous[previous.len() - 1 - back].kind
            && new_offsets[blocks.len() - 1 - back].1 == old_offsets[previous.len() - 1 - back].1
        {
            ids[blocks.len() - 1 - back] = Some(previous[previous.len() - 1 - back].id);
            back += 1;
        }
        let entries = blocks
            .iter()
            .zip(&ids)
            .map(|((kind, _, lines, leading_content_lines), id)| Entry {
                id: id.unwrap_or_else(|| {
                    let id = BlockId(self.next_id);
                    self.next_id += 1;
                    id
                }),
                kind: *kind,
                revision,
                lines: *lines,
                leading_content_lines: *leading_content_lines,
            })
            .collect::<Vec<_>>();
        for previous in &previous {
            self.fence_height_projections.remove(&previous.id);
        }
        for (entry, projection) in entries.iter().zip(fence_height_projections) {
            if let Some(projection) = projection {
                self.fence_height_projections
                    .insert(entry.id, projection.clone());
            }
        }
        if blocks.is_empty() {
            // A window that parses to nothing is blank. Its bytes join the block
            // above, keeping the tiling intact; with no block above, the document
            // holds no block at all.
            let bytes: usize = existing.iter().map(|(_, _, _, length)| length).sum();
            let lines: usize = existing.iter().map(|(_, entry, _, _)| entry.lines).sum();
            self.store.splice(window.clone(), &[]);
            if window.start > 0 {
                let above = window.start - 1;
                self.store
                    .set_length(above, self.store.length(above) + bytes);
                if let Some((mut entry, _)) = self.store.get(above) {
                    entry.lines += lines;
                    self.store.set_payload(above, entry);
                }
            }
        } else if blocks.len() == window.len() {
            // Same block count: rewrite the slots in place, so an edit that does
            // not change the window's structure never re-chunks the store.
            for (offset, (entry, (_, length, _, _))) in entries.iter().zip(blocks).enumerate() {
                self.store.set_payload(window.start + offset, *entry);
                self.store.set_length(window.start + offset, *length);
            }
        } else {
            let items = entries
                .into_iter()
                .zip(blocks.iter().map(|(_, length, _, _)| *length))
                .collect::<Vec<_>>();
            self.store.splice(window.clone(), &items);
        }
        if let Some(from) = self.provisional_from
            && from >= window.start
        {
            self.provisional_from = Some(if from < window.end {
                window.start
            } else {
                from + blocks.len() - window.len()
            });
        }
        blocks.len()
    }
}

/// Where a published [`BlockIndex`] came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexSource {
    /// A full parse of one document revision: authoritative for that revision.
    Formal,
    /// Incremental updates around the edit site, or a formal parse that has been
    /// rebased onto later edits. Correct to display, but a formal parse of the
    /// same revision supersedes it.
    Provisional,
}

/// What [`BlockIndexState::publish`] did with a candidate index.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublishOutcome {
    /// Accepted; the candidate already described the buffer's revision.
    Published,
    /// Accepted after re-parsing the edits made since the candidate started.
    Rebased(BlockIndexUpdate),
    /// Rejected: older than what is already published, so publishing it would
    /// overwrite newer knowledge with a stale parse.
    Stale,
    /// Rejected: same revision, but no more authoritative than what is published.
    NotMoreAuthoritative,
    /// Rejected: the buffer no longer remembers the edits since the candidate's
    /// revision, so it cannot be rebased onto the current document.
    HistoryUnavailable,
}

/// Owns the published [`BlockIndex`] and the rule for which result may replace
/// it. Both the background full parse and the incremental updates on the input
/// path go through here, so "a stale result never overwrites the display" is one
/// rule in one place rather than a revision check at each call site.
///
/// Priority, highest first:
///
/// 1. a newer document revision beats an older one;
/// 2. at the same revision, [`IndexSource::Formal`] beats
///    [`IndexSource::Provisional`], and a provisional result never replaces a
///    formal one;
/// 3. a candidate older than what is published is rejected outright;
/// 4. an accepted candidate is first brought to the buffer's current revision;
///    one that cannot be (its edits are no longer in the buffer's history) is
///    rejected rather than published stale.
#[derive(Clone, Debug, Default)]
pub struct BlockIndexState {
    published: Option<BlockIndex>,
    source: Option<IndexSource>,
}

impl BlockIndexState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn index(&self) -> Option<&BlockIndex> {
        self.published.as_ref()
    }

    pub fn source(&self) -> Option<IndexSource> {
        self.source
    }

    /// True while the published index is missing, behind the buffer, or carries
    /// blocks a full parse still has to confirm.
    pub fn needs_formal_parse(&self, buffer: &RopeBuffer) -> bool {
        self.published.as_ref().is_none_or(|index| {
            index.revision() != buffer.revision()
                || index.has_provisional_blocks()
                || self.source != Some(IndexSource::Formal)
        })
    }

    /// Brings the published index up to the buffer's revision by re-parsing only
    /// the edited window. Intended for the input path. Drops the index if the
    /// buffer no longer remembers the edits, leaving the caller to re-parse.
    pub fn apply_edits(&mut self, buffer: &RopeBuffer) -> Option<BlockIndexUpdate> {
        let index = self.published.as_mut()?;
        if index.revision() == buffer.revision() {
            return None;
        }
        let Ok(deltas) = buffer.deltas_since(index.revision()) else {
            self.published = None;
            self.source = None;
            return None;
        };
        let update = index.update(buffer, &deltas);
        self.source = Some(IndexSource::Provisional);
        Some(update)
    }

    /// Applies the publish priority to a finished parse.
    pub fn publish(
        &mut self,
        mut candidate: BlockIndex,
        source: IndexSource,
        buffer: &RopeBuffer,
    ) -> PublishOutcome {
        if let Some(published) = &self.published {
            if candidate.revision() < published.revision() {
                return PublishOutcome::Stale;
            }
            if candidate.revision() == published.revision()
                && !(source == IndexSource::Formal && self.source == Some(IndexSource::Provisional))
            {
                return PublishOutcome::NotMoreAuthoritative;
            }
        }
        if candidate.revision() == buffer.revision() {
            self.published = Some(candidate);
            self.source = Some(source);
            return PublishOutcome::Published;
        }
        let Ok(deltas) = buffer.deltas_since(candidate.revision()) else {
            return PublishOutcome::HistoryUnavailable;
        };
        let update = candidate.update(buffer, &deltas);
        self.published = Some(candidate);
        // Rebasing re-parses only the edited windows, so the result is no longer
        // a full parse of the current revision.
        self.source = Some(IndexSource::Provisional);
        PublishOutcome::Rebased(update)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hane_document::TextBuffer;

    /// Kind and range of every block, for comparing an incrementally updated
    /// index against a full parse of the same text.
    fn structure(index: &BlockIndex) -> Vec<(NodeKind, Range<usize>)> {
        index
            .blocks()
            .map(|block| (block.kind, block.source_range.as_usize()))
            .collect()
    }

    fn edit(buffer: &mut RopeBuffer, range: SourceRange, replacement: &str) -> Vec<RevisionDelta> {
        let base = buffer.revision();
        buffer.edit(range, replacement).unwrap();
        buffer.deltas_since(base).unwrap()
    }

    fn apply(
        index: &mut BlockIndex,
        buffer: &mut RopeBuffer,
        at: usize,
        replacement: &str,
    ) -> BlockIndexUpdate {
        let deltas = edit(buffer, SourceRange::empty(at), replacement);
        index.update(buffer, &deltas)
    }

    #[test]
    fn blocks_tile_the_document_and_resolve_by_offset() {
        let source = "# head\n\npara one\ncont\n\n\n- a\n- b\n\n```rust\nx\n```\n\n> q\n\ntail";
        let index = BlockIndex::build(Revision(3), source);
        assert_eq!(index.covered_bytes(), source.len());
        let mut expected_start = 0;
        for block in index.blocks() {
            assert_eq!(block.source_range.start.0, expected_start);
            expected_start = block.source_range.end.0;
            assert_eq!(block.revision, Revision(3));
            assert_eq!(block.confidence, Confidence::Formal);
        }
        assert_eq!(expected_start, source.len());
        // The blank lines between two blocks belong to the block above.
        assert_eq!(
            index.block_at(SourceOffset(7)).map(|block| block.kind),
            Some(NodeKind::Heading(1))
        );
        assert_eq!(
            index.block_at(SourceOffset(9)).map(|block| block.kind),
            Some(NodeKind::Paragraph)
        );
        assert_eq!(
            index.block_at(SourceOffset(34)).map(|block| block.kind),
            Some(NodeKind::CodeBlock)
        );
        // The document end belongs to the last block; past it is out of range.
        assert_eq!(
            index
                .block_at(SourceOffset(source.len()))
                .map(|b| b.ordinal),
            Some(index.len() - 1)
        );
        assert_eq!(index.block_at(SourceOffset(source.len() + 1)), None);
        // Ordinal to range agrees with the iteration order.
        for (ordinal, block) in index.blocks().enumerate() {
            assert_eq!(index.block(ordinal), Some(block));
        }
        assert_eq!(index.block(index.len()), None);
    }

    #[test]
    fn the_first_fenced_block_remembers_its_leading_blank_lines() {
        let source = "\n\n```rust\ncode\n```\n";
        let index = BlockIndex::build(Revision(1), source);
        let block = index.blocks().next().expect("fenced block");
        assert_eq!(block.kind, NodeKind::CodeBlock);
        assert_eq!(block.leading_content_lines, 2);
    }

    #[test]
    fn fence_height_projection_counts_only_inactive_visually_empty_rows() {
        let source = "- item\n  ```\n  code\n  ```\n  ```rust\n  code2\n  ```\n";
        let index = BlockIndex::build(Revision(1), source);
        let block = index.blocks().next().expect("list block");
        let projection = index
            .fence_height_projection(&block)
            .expect("fence height projection");

        assert_eq!(
            projection.inactive_rows_in(block.source_range, block.source_range, None, true),
            4,
            "both opening rows and both closing fences collapse while inactive"
        );

        let bare_opening = source.find("```").expect("bare opening");
        assert_eq!(
            projection.inactive_rows_in(
                block.source_range,
                block.source_range,
                Some(SourceRange::empty(bare_opening)),
                true,
            ),
            3,
            "editing a fence restores only that physical row"
        );

        let code_start = source.find("code").expect("code row");
        assert_eq!(
            projection.inactive_rows_in(
                block.source_range,
                block.source_range,
                Some(SourceRange::empty(code_start)),
                true,
            ),
            4,
            "the following row's start does not own the preceding fence row"
        );
    }

    #[test]
    fn disclosed_quote_prefix_keeps_nested_fence_rows_at_normal_height() {
        let source = "> ```\n> code\n> ```";
        let index = BlockIndex::build(Revision(1), source);
        let block = index.blocks().next().expect("quoted block");
        let projection = index
            .fence_height_projection(&block)
            .expect("quoted fence height projection");

        assert_eq!(
            projection.inactive_rows_in(block.source_range, block.source_range, None, true),
            2
        );
        let code = source.find("code").expect("code row");
        assert_eq!(
            projection.inactive_rows_in(
                block.source_range,
                block.source_range,
                Some(SourceRange::empty(code)),
                true,
            ),
            0,
            "the visible quote prefix discloses both fence rows"
        );
    }

    #[test]
    fn document_end_caret_restores_a_final_nested_closing_fence() {
        let source = "- item\n  ```\n  code\n  ```";
        let index = BlockIndex::build(Revision(1), source);
        let block = index.blocks().next().expect("list block");
        let projection = index
            .fence_height_projection(&block)
            .expect("fence height projection");

        assert_eq!(
            projection.inactive_rows_in(block.source_range, block.source_range, None, true),
            2
        );
        assert_eq!(
            projection.inactive_rows_in(
                block.source_range,
                block.source_range,
                Some(SourceRange::empty(source.len())),
                true,
            ),
            1,
            "the final physical row owns a caret at document end"
        );
    }

    #[test]
    fn caret_at_a_quote_owner_end_inside_a_block_restores_fence_rows() {
        let source = "- > ```\n  > code\n  > ```\n  after";
        let index = BlockIndex::build(Revision(1), source);
        let block = index.blocks().next().expect("list block");
        let projection = index
            .fence_height_projection(&block)
            .expect("quoted fence height projection");
        let after = source.rfind('\n').expect("following list row") + 1;

        assert_eq!(
            projection.inactive_rows_in(block.source_range, block.source_range, None, true),
            1
        );
        assert_eq!(
            projection.inactive_rows_in(
                block.source_range,
                block.source_range,
                Some(SourceRange::empty(after)),
                true,
            ),
            0,
            "the quote owner remains disclosed at its interior end boundary"
        );
    }

    #[test]
    fn an_incremental_edit_before_the_last_quote_uses_the_current_document_end() {
        let source = "before\n\nmiddle one\n\nmiddle two\n\n> ```\n> code\n> ```";
        let mut buffer = RopeBuffer::from_text(source);
        let mut index = BlockIndex::from_buffer(&buffer);
        let before = index.blocks().last().expect("final quote block");
        let before_projection = index
            .fence_height_projection(&before)
            .expect("final quote fence projection");
        assert_eq!(
            before_projection.inactive_rows_in(
                before.source_range,
                before.source_range,
                Some(SourceRange::empty(source.len())),
                true,
            ),
            0
        );

        // The bounded incremental window reparses the edited block and its
        // neighbor, not the distant final quote block. Its block-relative
        // fence rows must nevertheless honor the current final-block status.
        apply(&mut index, &mut buffer, 1, "X");
        let after = index.blocks().last().expect("final quote block after edit");
        let after_projection = index
            .fence_height_projection(&after)
            .expect("final quote projection after edit");
        assert_eq!(
            after_projection.inactive_rows_in(
                after.source_range,
                after.source_range,
                Some(SourceRange::empty(buffer.len_bytes().0)),
                true,
            ),
            0,
            "a stale absolute document end must not hide the quote owner after an earlier edit"
        );
    }

    #[test]
    fn incremental_list_edit_keeps_fence_height_projection_current() {
        let source = "- item\n  ```\n  code\n  ```\n";
        let mut buffer = RopeBuffer::from_text(source);
        let mut index = BlockIndex::from_buffer(&buffer);
        let before = index.blocks().next().expect("list block");
        assert!(index.list_projection(&before).is_some());
        assert_eq!(
            index
                .fence_height_projection(&before)
                .expect("initial fence heights")
                .inactive_rows_in(before.source_range, before.source_range, None, true),
            2
        );

        let code = source.find("code").expect("code row") + 2;
        let update = apply(&mut index, &mut buffer, code, "X");
        assert!(update.resynchronized);

        let after = index.blocks().next().expect("updated list block");
        assert_eq!(
            after.id, before.id,
            "ordinary typing keeps the block identity"
        );
        assert!(
            index.list_projection(&after).is_none(),
            "incremental parsing intentionally drops formal list semantics"
        );
        assert_eq!(
            index
                .fence_height_projection(&after)
                .expect("incremental parse rebuilds fence heights")
                .inactive_rows_in(after.source_range, after.source_range, None, true),
            2,
            "nested fence geometry stays stable while formal parsing catches up"
        );
    }
    #[test]
    fn merging_blocks_drops_fence_height_projections_for_removed_ids() {
        let source = "> ```\n> one\n> ```\n\nplain\n\n> ```\n> two\n> ```\n";
        let mut index = BlockIndex::build(Revision(1), source);
        let blocks = index.blocks().collect::<Vec<_>>();
        assert!(
            blocks.len() >= 3,
            "fixture has separate quote/paragraph blocks"
        );
        let removed = blocks.last().expect("last quote block").id;
        assert!(
            index.fence_height_projections.contains_key(&removed),
            "last quoted fence has indexed height geometry"
        );

        let last = blocks.len() - 1;
        index.merge_run(0, last, source.len(), Revision(2));

        assert_eq!(index.len(), 1);
        assert!(
            !index.fence_height_projections.contains_key(&removed),
            "projection for a block id removed by merge_run must be released"
        );
    }
    #[test]
    fn typing_inside_a_block_reparses_only_its_neighborhood() {
        let mut source = String::new();
        for line in 0..20_000 {
            source.push_str(&format!("paragraph {line}\n\n"));
        }
        let mut buffer = RopeBuffer::from_text(&source);
        let mut index = BlockIndex::from_buffer(&buffer);
        assert_eq!(index.len(), 20_000);
        let ids = index.blocks().map(|block| block.id).collect::<Vec<_>>();
        let target = index.block(10_000).unwrap();
        let update = apply(
            &mut index,
            &mut buffer,
            target.source_range.start.0 + 4,
            "!",
        );

        assert!(update.resynchronized);
        assert_eq!(update.invalidated_blocks, 0);
        assert_eq!(update.first_replaced_block, 9_999);
        assert_eq!(update.replaced_blocks, 3);
        assert_eq!(update.inserted_blocks, 3);
        assert!(
            update.reparsed_bytes < 200,
            "one keystroke re-parsed {} bytes",
            update.reparsed_bytes
        );
        assert_eq!(index.len(), 20_000);
        assert_eq!(index.covered_bytes(), buffer.len_bytes().0);
        // Untouched blocks keep both their identity and their parse revision.
        assert_eq!(
            index.blocks().map(|block| block.id).collect::<Vec<_>>(),
            ids
        );
        assert_eq!(index.block(0).unwrap().revision, Revision(0));
        assert_eq!(index.block(10_000).unwrap().revision, buffer.revision());
        assert_eq!(index.revision(), buffer.revision());
        assert_eq!(
            structure(&index),
            structure(&BlockIndex::from_buffer(&buffer))
        );
    }

    #[test]
    fn splitting_and_merging_blocks_keeps_the_index_equal_to_a_full_parse() {
        let mut buffer = RopeBuffer::from_text("alpha\n\nbravo\n\ncharlie\n");
        let mut index = BlockIndex::from_buffer(&buffer);
        let last_id = index.block(2).unwrap().id;

        // Split "bravo" into two paragraphs.
        let update = apply(&mut index, &mut buffer, 12, "\n\nsplit");
        assert!(update.resynchronized);
        assert_eq!(update.first_replaced_block, 0);
        assert_eq!(update.replaced_blocks, 3);
        assert_eq!(update.inserted_blocks, 4);
        assert_eq!(index.len(), 4);
        assert_eq!(
            structure(&index),
            structure(&BlockIndex::from_buffer(&buffer))
        );
        // The block after the split is untouched, so it keeps its id.
        assert_eq!(index.block(3).unwrap().id, last_id);

        // Delete the blank line that separates the first two paragraphs.
        let deltas = edit(&mut buffer, SourceRange::new(5, 7), "\n");
        let update = index.update(&buffer, &deltas);
        assert!(update.resynchronized);
        assert_eq!(index.len(), 3);
        assert_eq!(
            structure(&index),
            structure(&BlockIndex::from_buffer(&buffer))
        );
        assert_eq!(index.block(2).unwrap().id, last_id);
    }

    #[test]
    fn an_unterminated_fence_invalidates_the_tail_conservatively() {
        let mut source = String::from("intro\n\n");
        for line in 0..2_000 {
            source.push_str(&format!("paragraph {line}\n\n"));
        }
        let mut buffer = RopeBuffer::from_text(&source);
        let mut index = BlockIndex::from_buffer(&buffer);
        let blocks_before = index.len();

        // An opening fence with no closing fence swallows everything after it,
        // so the parse can never re-synchronize with the blocks that follow.
        let update = apply(&mut index, &mut buffer, 5, "\n\n```");
        assert!(!update.resynchronized);
        assert!(update.invalidated_blocks > 0);
        assert!(
            update.reparsed_bytes <= RESYNC_BYTE_BUDGET + buffer.len_bytes().0 / 4,
            "gave up after {} bytes",
            update.reparsed_bytes
        );
        assert!(index.has_provisional_blocks());
        // Offsets still resolve, and the tail is honest about being provisional.
        assert_eq!(index.covered_bytes(), buffer.len_bytes().0);
        assert!(index.len() <= blocks_before + 1);
        assert_eq!(
            index.block(index.len() - 1).unwrap().confidence,
            Confidence::Provisional
        );
        assert_eq!(index.block(0).unwrap().confidence, Confidence::Formal);

        // A full parse of the same revision restores formal knowledge.
        let formal = BlockIndex::from_buffer(&buffer);
        assert!(!formal.has_provisional_blocks());
    }

    #[test]
    fn closing_a_fence_resynchronizes_the_following_blocks() {
        let mut buffer = RopeBuffer::from_text("```\ncode\n\nalpha\n\nbravo\n");
        let mut index = BlockIndex::from_buffer(&buffer);
        assert_eq!(index.len(), 1, "the unterminated fence owns the document");

        let update = apply(&mut index, &mut buffer, 9, "```\n");
        assert!(update.resynchronized);
        assert_eq!(
            structure(&index),
            structure(&BlockIndex::from_buffer(&buffer))
        );
        assert!(!index.has_provisional_blocks());
    }

    #[test]
    fn a_blank_document_holds_no_block_and_recovers_when_typed_into() {
        let mut buffer = RopeBuffer::from_text("\n\n");
        let mut index = BlockIndex::from_buffer(&buffer);
        assert!(index.is_empty());
        assert_eq!(index.block_at(SourceOffset(0)), None);

        let update = apply(&mut index, &mut buffer, 0, "# heading\n");
        assert!(update.resynchronized);
        assert_eq!(index.len(), 1);
        assert_eq!(index.covered_bytes(), buffer.len_bytes().0);
        assert_eq!(index.block(0).unwrap().kind, NodeKind::Heading(1));

        // Deleting it all returns the index to empty without losing the tiling.
        let whole = SourceRange::new(0, buffer.len_bytes().0);
        let deltas = edit(&mut buffer, whole, "");
        index.update(&buffer, &deltas);
        assert!(index.is_empty());
        assert_eq!(index.covered_bytes(), 0);
    }

    #[test]
    fn top_level_fenced_code_does_not_build_a_list_projection() {
        let source = "````rust\ncode\n````";
        let index = BlockIndex::build(Revision(1), source);
        let block = index.block(0).expect("top-level code block");
        assert_eq!(block.kind, NodeKind::CodeBlock);
        assert!(index.list_projection(&block).is_none());
    }

    #[test]
    fn formal_projection_keeps_nested_quote_depth_for_viewport_rows() {
        let source = "> outer\n> > inner\n";
        let index = BlockIndex::build(Revision(1), source);
        let block = index.block(0).expect("quote block");
        let projection = index.list_projection(&block).expect("quote projection");
        let outer_line = SourceRange::new(0, "> outer\n".len());
        let inner_start = "> outer\n".len();
        let inner_line = SourceRange::new(inner_start, source.len());
        assert_eq!(
            projection
                .quotes_in(outer_line)
                .map(|quote| quote.depth)
                .collect::<Vec<_>>(),
            vec![1]
        );
        assert_eq!(
            projection
                .quotes_in(inner_line)
                .map(|quote| quote.depth)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[test]
    fn formal_quote_projection_stays_with_its_own_block() {
        let source = "> first\n\n> second\n";
        let index = BlockIndex::build(Revision(1), source);
        let blocks = index.blocks().collect::<Vec<_>>();
        assert_eq!(blocks.len(), 2);

        let first = index
            .list_projection(&blocks[0])
            .expect("first quote projection");
        let second = index
            .list_projection(&blocks[1])
            .expect("second quote projection");
        assert_eq!(first.quotes_in(blocks[0].source_range).count(), 1);
        assert_eq!(second.quotes_in(blocks[1].source_range).count(), 1);
        assert_eq!(
            first.quotes_in(blocks[1].source_range).count(),
            0,
            "the first block must not retain quote projections from later blocks"
        );
        assert_eq!(
            second.quotes_in(blocks[0].source_range).count(),
            0,
            "the second block must not retain quote projections from earlier blocks"
        );
    }

    #[test]
    fn incremental_updates_match_a_full_parse_across_mixed_edits() {
        let source = "# title\n\nintro paragraph\n\n- one\n- two\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n> quote\n\nlast\n";
        let mut buffer = RopeBuffer::from_text(source);
        let mut index = BlockIndex::from_buffer(&buffer);
        let edits: [(usize, usize, &str); 7] = [
            (9, 9, "**bold** "),
            (0, 1, "###"),
            (30, 30, "\n- three"),
            (5, 5, "日本語"),
            (2, 4, ""),
            (60, 60, "\n\nnew paragraph\n"),
            (0, 0, "front matter\n\n"),
        ];
        for (start, end, replacement) in edits {
            let start = start.min(buffer.len_bytes().0);
            let end = end.clamp(start, buffer.len_bytes().0);
            let deltas = edit(&mut buffer, SourceRange::new(start, end), replacement);
            let update = index.update(&buffer, &deltas);
            assert_eq!(index.revision(), buffer.revision());
            assert_eq!(index.covered_bytes(), buffer.len_bytes().0);
            if update.resynchronized && !index.has_provisional_blocks() {
                assert_eq!(
                    structure(&index),
                    structure(&BlockIndex::from_buffer(&buffer)),
                    "diverged after replacing {start}..{end} with {replacement:?}"
                );
            }
        }
    }

    #[test]
    fn batched_edits_are_absorbed_in_one_update() {
        let mut buffer = RopeBuffer::from_text("alpha\n\nbravo\n\ncharlie\n");
        let mut index = BlockIndex::from_buffer(&buffer);
        let base = buffer.revision();
        buffer.edit(SourceRange::empty(5), "!").unwrap();
        buffer.edit(SourceRange::empty(6), "?").unwrap();
        let deltas = buffer.deltas_since(base).unwrap();
        assert_eq!(deltas.len(), 2);
        let update = index.update(&buffer, &deltas);
        assert!(update.resynchronized);
        assert_eq!(index.revision(), buffer.revision());
        assert_eq!(
            structure(&index),
            structure(&BlockIndex::from_buffer(&buffer))
        );
    }

    #[test]
    fn blocks_in_range_returns_only_overlapping_blocks() {
        let source = "alpha\n\nbravo\n\ncharlie\n";
        let index = BlockIndex::build(Revision(0), source);
        let middle = index.block(1).unwrap().source_range;
        let overlapping = index
            .blocks_in(middle)
            .map(|block| block.ordinal)
            .collect::<Vec<_>>();
        assert_eq!(overlapping, vec![1]);
        assert_eq!(
            index.blocks_in(SourceRange::new(0, source.len())).count(),
            index.len()
        );
        assert_eq!(
            index
                .blocks_in(SourceRange::empty(middle.start.0 + 1))
                .map(|block| block.ordinal)
                .collect::<Vec<_>>(),
            vec![1]
        );
    }

    #[test]
    fn publish_prefers_newer_revisions_and_formal_parses() {
        let mut buffer = RopeBuffer::from_text("alpha\n\nbravo\n");
        let mut state = BlockIndexState::new();
        assert!(state.needs_formal_parse(&buffer));

        let formal = BlockIndex::from_buffer(&buffer);
        assert_eq!(
            state.publish(formal.clone(), IndexSource::Formal, &buffer),
            PublishOutcome::Published
        );
        assert!(!state.needs_formal_parse(&buffer));

        // Same revision, no more authoritative: rejected either way round.
        assert_eq!(
            state.publish(formal.clone(), IndexSource::Provisional, &buffer),
            PublishOutcome::NotMoreAuthoritative
        );
        assert_eq!(
            state.publish(formal.clone(), IndexSource::Formal, &buffer),
            PublishOutcome::NotMoreAuthoritative
        );

        // An edit makes the published index provisional, and a formal parse of
        // that same revision may then replace it.
        buffer.edit(SourceRange::empty(0), "# ").unwrap();
        let update = state.apply_edits(&buffer).expect("edits are applied");
        assert!(update.resynchronized);
        assert_eq!(state.source(), Some(IndexSource::Provisional));
        assert!(state.needs_formal_parse(&buffer));
        assert_eq!(
            state.index().unwrap().block(0).unwrap().kind,
            NodeKind::Heading(1)
        );
        assert_eq!(
            state.publish(
                BlockIndex::from_buffer(&buffer),
                IndexSource::Formal,
                &buffer
            ),
            PublishOutcome::Published
        );
        assert_eq!(state.source(), Some(IndexSource::Formal));

        // A parse that started before the last edit is stale and never publishes.
        assert_eq!(
            state.publish(formal, IndexSource::Formal, &buffer),
            PublishOutcome::Stale
        );
    }

    #[test]
    fn a_background_parse_that_finished_late_is_rebased_onto_the_edits() {
        let mut buffer = RopeBuffer::from_text("alpha\n\nbravo\n\ncharlie\n");
        let mut state = BlockIndexState::new();
        // The background job starts here and finishes after two more edits.
        let candidate = BlockIndex::from_buffer(&buffer);
        buffer.edit(SourceRange::empty(0), "# ").unwrap();
        buffer.edit(SourceRange::empty(9), "!").unwrap();

        let outcome = state.publish(candidate, IndexSource::Formal, &buffer);
        assert!(matches!(outcome, PublishOutcome::Rebased(_)));
        let index = state.index().unwrap();
        assert_eq!(index.revision(), buffer.revision());
        assert_eq!(index.covered_bytes(), buffer.len_bytes().0);
        assert_eq!(
            structure(index),
            structure(&BlockIndex::from_buffer(&buffer))
        );
        // Rebasing re-parsed only the edited windows, so it is not a formal
        // parse of the current revision.
        assert_eq!(state.source(), Some(IndexSource::Provisional));
    }
}
