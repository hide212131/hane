use crate::display::{
    BlockKind, ImagePresentation, LineContext, LinkTarget, ListAlignment, ListId, ListItemId, ListMarkerKind,
    ListMarkerMetadata, ListOwnerMetadata, ListRowMetadata, ListRowRole,
    ListStructuralPrefixMetadata, ListTaskMetadata, QuoteRowMetadata, StyleKind, StyleRun, TableCellDisplay,
    TableRowDisplay, VisualBlock, VisualLine, code_line_height, estimated_height, syntax_display,
};
#[cfg(test)]
use crate::layout::{QUOTE_DEPTH_INDENT, layout_block};
use crate::source_map::{
    MappingSegment, MarkerEdge, SourceMap, Visibility, VisualOffset, VisualRange,
};
#[cfg(test)]
use crate::testing;
use hane_document::{Bias, Revision, RopeBuffer, SourceOffset, SourceRange, TextBuffer};
#[cfg(test)]
use hane_document::RevisionDelta;
use hane_markdown::{
    BlockIndex, FenceDelimiter, FenceMarkerEdge, IndexedBlock, ListProjection, ListProjectionItem,
    ListProjectionPrefix, MarkdownNode, MarkdownParse, MarkdownTree, NodeId, NodeKind,
    TableAlignment, TableProjection, fence_closes, fence_closing_delimiter, fence_delimiter,
    has_delimiter_markers, is_table_delimiter, links_intersecting, parse_document, task_glyph,
};
#[cfg(test)]
use hane_markdown::{BlockId, Confidence};
use std::ops::Range;

mod block;
pub use block::{
    BlockLine, BlockWindow, block_height_with_disclosure, block_heights,
    block_heights_with_disclosure, block_line_span, expected_disclosures, present_block,
    present_block_with_list_projection, present_block_with_table_projection, trailing_blank_lines,
};

fn present_plain(line_id: u64, revision: Revision, range: SourceRange, source: &str) -> VisualLine {
    VisualLine {
        line_id,
        source_range: range,
        revision,
        visual_text: source.to_owned(),
        style_runs: Vec::new(),
        links: Vec::new(),
        kind: BlockKind::Paragraph,
        source_map: SourceMap {
            segments: vec![MappingSegment {
                source_range: range,
                visual_range: VisualRange::new(0, source.len()),
                visibility: Visibility::Visible,
                marker_edge: None,
            }],
        },
        estimated_height: 24.0,
        measured_height: None,
        invalid: false,
        context: LineContext::Normal,
        disclosure: None,
        image: None,
        list: None,
        quote: None,
        quote_marker_visual_ranges: Vec::new(),
        quote_marker_source_ranges: Vec::new(),
        table_row: None,
        table_header: false,
    }
}

/// Raw-source fallback presentation. Shows the block source verbatim through a
/// single visible segment so no source byte is hidden or dropped, and marks the
/// block [`BlockKind::Unsupported`] so the UI renders it as plain text without
/// inferring structure. This is the formal display contract for unimplemented
/// syntax: a construct with no specialized presenter, or one whose marker
/// derivation fails to tile the source range, still round-trips its source.
fn present_raw_source(
    line_id: u64,
    revision: Revision,
    range: SourceRange,
    source: &str,
    line_height: f32,
) -> VisualLine {
    let mut block = present_plain(line_id, revision, range, source);
    block.kind = BlockKind::Unsupported;
    block.estimated_height = estimated_height(BlockKind::Unsupported, line_height);
    block
}

/// Presents a parser-confirmed thematic break. Inactive rows become an empty
/// source-mapped row for the UI to paint as a separator; touching the rule with
/// a caret, selection or IME expands the exact source bytes for direct editing.
#[allow(
    clippy::too_many_arguments,
    reason = "rule presentation keeps the shared parse and projected markers with row inputs"
)]
fn present_rule_line(
    line_id: u64,
    revision: Revision,
    range: SourceRange,
    source: &str,
    line_height: f32,
    disclosure: Option<SourceRange>,
    quote: Option<QuoteRowMetadata>,
    shared: &SharedParse<'_>,
    markers_on_line: &[ProjectedMarker],
) -> VisualLine {
    let expanded =
        disclosure.is_some_and(|active| disclosure_owns_physical_line(range, source, active));
    let visibility = if expanded {
        Visibility::ExpandedMarkup
    } else {
        Visibility::HiddenMarkup
    };
    let mut visual = String::new();
    let mut segments = Vec::with_capacity(markers_on_line.len() * 2 + 1);
    let mut source_cursor = range.start.0;
    for planned in markers_on_line {
        let marker = planned.range;
        if marker.end > range.end {
            continue;
        }
        let marker_expanded = expanded
            || marker_is_disclosed(
                planned,
                shared.parsed,
                &shared.projection.nodes,
                disclosure,
                shared.list_projection,
            );
        let marker_visibility = if marker_expanded {
            Visibility::ExpandedMarkup
        } else {
            Visibility::HiddenMarkup
        };
        if source_cursor < marker.start.0 {
            append_segment(
                &mut visual,
                &mut segments,
                source,
                range,
                SourceRange::new(source_cursor, marker.start.0),
                visibility,
                None,
            );
        }
        append_segment(
            &mut visual,
            &mut segments,
            source,
            range,
            marker,
            marker_visibility,
            marker_edge(planned, shared.parsed, &shared.projection.nodes),
        );
        // Keep the ordinary list projection contract even for a Rule. A
        // parser-confirmed Rule can be a continuation row inside a list, and
        // an opening marker (if one ever shares the row) still gets the same
        // synthesized inactive label as every other list row.
        let label = if let Some(owner) = planned.list_owner {
            list_item_label_with_projection(
                shared.parsed,
                &shared.projection.list_item_ordinals,
                owner,
                shared.list_projection,
                planned.range,
            )
        } else {
            planned
                .global_list_item
                .map(|item| list_label(item.start, item.ordinal))
        };
        if !marker_expanded && let Some(label) = label {
            let visual_start = visual.len();
            visual.push_str(&label);
            segments.push(MappingSegment {
                source_range: SourceRange::empty(marker.start.0),
                visual_range: VisualRange::new(visual_start, visual.len()),
                visibility: Visibility::Synthesized,
                marker_edge: None,
            });
        }
        source_cursor = marker.end.0;
    }
    if source_cursor < range.end.0 {
        append_segment(
            &mut visual,
            &mut segments,
            source,
            range,
            SourceRange::new(source_cursor, range.end.0),
            visibility,
            None,
        );
    }
    if segments.is_empty() {
        segments.push(MappingSegment {
            source_range: range,
            visual_range: VisualRange::new(0, visual.len()),
            visibility,
            marker_edge: None,
        });
    }
    let (quote_marker_visual_ranges, quote_marker_source_ranges) =
        quote_marker_ranges(markers_on_line, &segments);
    VisualLine {
        line_id,
        source_range: range,
        revision,
        visual_text: visual,
        style_runs: Vec::new(),
        links: Vec::new(),
        kind: BlockKind::Rule,
        source_map: SourceMap { segments },
        estimated_height: estimated_height(BlockKind::Rule, line_height),
        measured_height: None,
        invalid: false,
        context: LineContext::Normal,
        disclosure,
        image: None,
        list: None,
        quote,
        quote_marker_visual_ranges,
        quote_marker_source_ranges,
        table_row: None,
        table_header: false,
    }
}

/// A fence-only source row is structural markup, not an empty code row. Once
/// its delimiter is collapsed and no visible label/content remains, it must
/// consume no vertical space. As soon as the delimiter is disclosed for
/// editing, the ordinary code-row height comes back.
fn fenced_line_height(
    visual_text: &str,
    has_hidden_fence: bool,
    is_editing: bool,
    line_height: f32,
) -> f32 {
    if has_hidden_fence && !is_editing && visual_text.trim().is_empty() {
        0.0
    } else {
        code_line_height(line_height)
    }
}

fn range_touches(range: SourceRange, disclosure: SourceRange) -> bool {
    if disclosure.is_empty() {
        range.start <= disclosure.start && disclosure.start <= range.end
    } else {
        range.intersects(disclosure)
    }
}

fn table_delimiter_is_collapsed_range(
    range: SourceRange,
    delimiter_has_line_ending: bool,
    disclosure: Option<SourceRange>,
) -> bool {
    disclosure.is_none_or(|active| {
        !table_delimiter_is_disclosed(range, delimiter_has_line_ending, active)
    })
}

/// A caret at the first byte of the line after a table delimiter belongs to
/// that body line, not to the delimiter. The range includes its line ending,
/// so its end is excluded unless the delimiter is the unterminated final line
/// and the caret is at EOF. Non-empty selections retain intersection ownership.
fn table_delimiter_is_disclosed(
    range: SourceRange,
    delimiter_has_line_ending: bool,
    disclosure: SourceRange,
) -> bool {
    if disclosure.is_empty() {
        range.start <= disclosure.start
            && (disclosure.start < range.end
                || (!delimiter_has_line_ending && disclosure.start == range.end))
    } else {
        range.intersects(disclosure)
    }
}

/// Whether a formal table's delimiter is hidden from the inactive visual
/// projection. The source range remains available for disclosure and editing.
pub fn table_delimiter_is_collapsed(
    projection: &TableProjection,
    disclosure: Option<SourceRange>,
) -> bool {
    projection.delimiter_range.is_some_and(|range| {
        table_delimiter_is_collapsed_range(range, projection.delimiter_has_line_ending, disclosure)
    })
}

fn disclosure_owns_physical_line(
    range: SourceRange,
    source: &str,
    disclosure: SourceRange,
) -> bool {
    if disclosure.is_empty() {
        range.start <= disclosure.start
            && (disclosure.start < range.end
                || (!source.ends_with(['\n', '\r']) && disclosure.start == range.end))
    } else {
        range.intersects(disclosure)
    }
}

/// Resolves an empty disclosure at a list-item boundary to the item that
/// starts there. Markdown list-item ranges are allowed to meet exactly at the
/// following item's start, so the generic inclusive caret check would
/// disclose both sibling markers and make the preceding row change width too.
/// Keep the inclusive end for an item's ordinary terminal caret unless that
/// same offset is also the start of a sibling.
fn list_item_range_touches(
    tree: &MarkdownTree,
    item_id: NodeId,
    item: &MarkdownNode,
    disclosure: SourceRange,
) -> bool {
    if !range_touches(item.source_range, disclosure) {
        return false;
    }
    if !disclosure.is_empty() || disclosure.start != item.source_range.end {
        return true;
    }
    let Some(list_id) = item.parent else {
        return true;
    };
    !tree.children(list_id).iter().any(|sibling_id| {
        *sibling_id != item_id
            && tree.node(*sibling_id).is_some_and(|sibling| {
                matches!(sibling.kind, NodeKind::ListItem { .. })
                    && sibling.source_range.start == disclosure.start
            })
    })
}

fn projected_list_item_range_touches(
    projection: &ListProjection,
    item: &ListProjectionItem,
    disclosure: SourceRange,
) -> bool {
    if !range_touches(item.item_range, disclosure) {
        return false;
    }
    if !disclosure.is_empty() || disclosure.start != item.item_range.end {
        return true;
    }
    let sibling = projection
        .items
        .partition_point(|candidate| candidate.item_range.start < disclosure.start);
    !projection.items[sibling..]
        .iter()
        .take_while(|candidate| candidate.item_range.start == disclosure.start)
        .any(|candidate| candidate.list_range == item.list_range)
}

fn marker_is_disclosed(
    marker: &ProjectedMarker,
    parsed: &MarkdownParse,
    nodes: &SourceIndex<NodeId>,
    disclosure: Option<SourceRange>,
    list_projection: Option<&ListProjection>,
) -> bool {
    let Some(disclosure) = disclosure else {
        return false;
    };
    // Quote prefixes disclose only their actual owner, not nested constructs.
    if let Some(owner) = marker.quote_owner {
        return parsed
            .tree
            .node(owner)
            .is_some_and(|quote| range_touches(quote.source_range, disclosure));
    }
    if let Some(owner_range) = marker.formal_quote_owner {
        return range_touches(owner_range, disclosure);
    }
    if let Some(item) = marker.global_list_item {
        return list_projection.is_some_and(|projection| {
            projected_list_item_range_touches(projection, &item, disclosure)
        });
    }
    if let Some(item) = marker.formal_list_item {
        return list_projection.is_some_and(|projection| {
            projected_list_item_range_touches(projection, &item, disclosure)
        });
    }
    // A list item's own bullet/number marker discloses whenever the caret,
    // selection or IME touches any of the item's own source range — its
    // opening line, its own paragraphs, and any nested child list or item it
    // contains. A nested item's `source_range` sits inside every enclosing
    // ancestor's own `source_range`, so entering nested content discloses the
    // whole ancestor chain down to the item actually touched, while a sibling
    // item — whose `source_range` never overlaps — stays hidden.
    if let Some(owner) = marker.list_owner {
        return parsed
            .tree
            .node(owner)
            .is_some_and(|item| list_item_range_touches(&parsed.tree, owner, item, disclosure));
    }
    // A list item's own structural continuation indentation follows the same
    // ownership rule as its opening marker above: it discloses whenever the
    // caret, selection or IME touches any of the owning item's own source
    // range, not just the physical line the indentation itself sits on.
    if let Some(prefix) = marker.global_list_prefix {
        let Some(projection) = list_projection else {
            return false;
        };
        return projection
            .item_for_source_range(prefix.item_range)
            .is_some_and(|item| projected_list_item_range_touches(projection, item, disclosure));
    }
    if let Some((owner, _)) = marker.list_prefix {
        return parsed
            .tree
            .node(owner)
            .is_some_and(|item| list_item_range_touches(&parsed.tree, owner, item, disclosure));
    }
    let marker = marker.range;
    if range_touches(marker, disclosure) {
        return true;
    }
    nodes.intersecting(marker).into_iter().any(|id| {
        let span = parsed.tree.node(*id).expect("indexed node");
        let owns_marker =
            if has_delimiter_markers(span.kind) && !matches!(span.kind, NodeKind::CodeBlock) {
                span.source_range.start <= marker.start && marker.end <= span.source_range.end
            } else if matches!(span.kind, NodeKind::Heading(_)) {
                (marker.start == span.source_range.start
                    || (matches!(span.kind, NodeKind::Heading(_))
                        && span
                            .children
                            .last()
                            .and_then(|id| parsed.tree.node(*id))
                            .is_none_or(|child| child.source_range.end <= marker.start)))
                    && marker.end <= span.source_range.end
            } else {
                false
            };
        owns_marker && range_touches(span.source_range, disclosure)
    })
}

/// Resolves whether `marker` is the opening or closing delimiter of the
/// smallest enclosing construct that owns it, by checking whether the
/// marker's own range starts or ends exactly where that construct's node
/// does. Returns `None` when the marker does not align with either edge of
/// any delimiter-owning node (not expected for markers this crate derives,
/// but not a display-breaking condition either).
///
/// A quote or list item's own prefix marker is always an opening edge — the
/// content it introduces sits to its right — even though its owning node's
/// `source_range` starts before the marker at the container's indentation,
/// which would otherwise make the `start == marker.start` check below miss
/// it entirely for an indented item.
///
/// An ATX heading is not in [`has_delimiter_markers`] (its markers are
/// derived block-side, not from a delimiter pair the inline scanner walks),
/// so it is classified separately here rather than being folded into that
/// set.
fn marker_edge(
    planned: &ProjectedMarker,
    parsed: &MarkdownParse,
    nodes: &SourceIndex<NodeId>,
) -> Option<MarkerEdge> {
    if let Some(edge) = planned.fence_edge {
        return Some(edge);
    }
    if planned.quote_owner.is_some()
        || planned.formal_quote_owner.is_some()
        || planned.list_owner.is_some()
        || planned.list_prefix.is_some()
        || planned.global_list_prefix.is_some()
        || planned.global_list_item.is_some()
    {
        return Some(MarkerEdge::Opening);
    }
    let marker = planned.range;
    nodes.intersecting(marker).into_iter().find_map(|id| {
        let span = parsed.tree.node(*id)?;
        let is_heading = matches!(span.kind, NodeKind::Heading(_));
        if !has_delimiter_markers(span.kind) && !is_heading {
            return None;
        }
        if span.source_range.start == marker.start {
            Some(MarkerEdge::Opening)
        } else if is_heading {
            // A heading's own source range can include the block's trailing
            // newline, which the derived closing marker's end excludes, so the
            // end-alignment check below never matches; a marker positioned
            // after all of the heading's content is its closing sequence.
            span.children
                .last()
                .and_then(|child_id| parsed.tree.node(*child_id))
                .is_none_or(|child| child.source_range.end <= marker.start)
                .then_some(MarkerEdge::Closing)
        } else if span.source_range.end == marker.end {
            Some(MarkerEdge::Closing)
        } else {
            None
        }
    })
}

/// The synthesized replacement text for an inactive list item's own hidden
/// bullet/number marker: `"• "` for an unordered item, or `"{n}. "` for an
/// ordered item, where `n` is the owning list's `start` plus the item's
/// zero-based sibling position — never the item's own source digits, which
/// [`NodeKind::List`] documents as possibly `0`, non-sequential, or
/// leading-zero-padded. The synthesized delimiter is always `.`, independent
/// of the source's own `.`/`)`; only disclosing the marker (see
/// [`marker_is_disclosed`]) shows those source bytes, unchanged, as editable
/// [`Visibility::ExpandedMarkup`].
///
/// `ordinals` is [`ProjectionIndex::list_item_ordinals`], the whole tree's
/// owner/position table built once per parse: looking `item` up here is
/// `O(1)`, unlike `MarkdownTree::list_item_ordinal`'s preceding-sibling scan,
/// which this function must not call per visible line — a large list's
/// viewport projects one line at a time (see
/// [`present_markdown_from_parse`]), and a scan there would cost proportional
/// to each visible item's position rather than the visible range.
#[cfg(test)]
fn list_item_label(
    parsed: &MarkdownParse,
    ordinals: &[Option<(NodeId, usize)>],
    item: NodeId,
) -> Option<String> {
    let (owner, ordinal) = (*ordinals.get(item.0)?)?;
    match parsed.tree.node(owner)?.kind {
        NodeKind::List { start } => Some(list_label(start, ordinal)),
        _ => None,
    }
}

fn list_item_label_with_projection(
    parsed: &MarkdownParse,
    ordinals: &[Option<(NodeId, usize)>],
    item: NodeId,
    list_projection: Option<&ListProjection>,
    marker_range: SourceRange,
) -> Option<String> {
    if let Some(projected) = projected_item_for_marker(list_projection, marker_range) {
        return Some(list_label(projected.start, projected.ordinal));
    }
    let (owner, ordinal) = (*ordinals.get(item.0)?)?;
    match parsed.tree.node(owner)?.kind {
        NodeKind::List { start } => Some(list_label(start, ordinal)),
        _ => None,
    }
}

/// Returns true when `segments` tile `range` contiguously, so every source byte
/// of the block belongs to exactly one mapping segment (empty synthesized
/// segments are ignored). Enforces the "source is never lost" display contract:
/// a false result means marker derivation left a gap or overlap and the caller
/// must fall back to raw-source presentation.
fn segments_tile_range(range: SourceRange, segments: &[MappingSegment]) -> bool {
    let mut cursor = range.start.0;
    for segment in segments {
        if segment.source_range.is_empty() {
            continue;
        }
        if segment.source_range.start.0 != cursor {
            return false;
        }
        cursor = segment.source_range.end.0;
    }
    cursor == range.end.0
}

fn append_segment(
    visual: &mut String,
    segments: &mut Vec<MappingSegment>,
    source: &str,
    block_range: SourceRange,
    source_range: SourceRange,
    visibility: Visibility,
    marker_edge: Option<MarkerEdge>,
) {
    let visual_start = visual.len();
    if visibility != Visibility::HiddenMarkup {
        visual.push_str(
            &source[source_range.start.0 - block_range.start.0
                ..source_range.end.0 - block_range.start.0],
        );
    }
    segments.push(MappingSegment {
        source_range,
        visual_range: VisualRange::new(visual_start, visual.len()),
        visibility,
        marker_edge,
    });
}

fn quote_marker_ranges(
    markers_on_line: &[ProjectedMarker],
    segments: &[MappingSegment],
) -> (Vec<VisualRange>, Vec<SourceRange>) {
    markers_on_line
        .iter()
        .filter(|marker| marker.quote_owner.is_some() || marker.formal_quote_owner.is_some())
        .filter_map(|marker| {
            segments
                .iter()
                .find(|segment| {
                    segment.source_range == marker.range
                        && segment.marker_edge == Some(MarkerEdge::Opening)
                })
                .map(|segment| (segment.visual_range, segment.source_range))
        })
        .unzip()
}

/// Builds a native Markdown block with progressive disclosure. Markdown source
/// remains authoritative; only marker ranges outside `disclosure` collapse.
pub fn present_markdown_with_disclosure(
    line_id: u64,
    revision: Revision,
    range: SourceRange,
    source: &str,
    line_height: f32,
    disclosure: Option<SourceRange>,
) -> VisualLine {
    present_markdown_with_list_projection(
        line_id,
        revision,
        range,
        source,
        line_height,
        disclosure,
        None,
    )
}

fn present_markdown_with_list_projection(
    line_id: u64,
    revision: Revision,
    range: SourceRange,
    source: &str,
    line_height: f32,
    disclosure: Option<SourceRange>,
    list_projection: Option<&ListProjection>,
) -> VisualLine {
    if source.is_empty() {
        let mut block = present_plain(line_id, revision, range, source);
        block.estimated_height = line_height;
        return block;
    }
    let parsed = parse_document(revision, range, source);
    let projection = ProjectionIndex::new(&parsed);
    present_markdown_from_parse(
        line_id,
        revision,
        range,
        source,
        line_height,
        disclosure,
        &SharedParse {
            parsed: &parsed,
            projection: &projection,
            list_projection,
        },
    )
}

/// A parse [`present_markdown_from_parse`] projects one physical line from.
/// It may describe delimiters, padding and spans on other physical lines.
struct SharedParse<'a> {
    parsed: &'a MarkdownParse,
    projection: &'a ProjectionIndex,
    list_projection: Option<&'a ListProjection>,
}

/// Presents one physical line from an already-parsed tree instead of parsing
/// `source` alone.
///
/// CommonMark treats a soft line break inside a paragraph as whitespace within
/// one continuous run of inline content, not as a boundary: `**bold` opened on
/// one physical line can close with `**` on the next, and the same is true of
/// `_..._`  and a backtick-delimited code span. Presenting each physical line as
/// its own self-contained parse — which [`present_markdown_with_disclosure`]
/// does, because caret, selection and IME still address physical lines — cannot
/// see that, since the delimiter that closes the construct sits outside the
/// slice being parsed. [`present_block`] instead parses every line of a
/// multi-line paragraph together and calls this once per line with the shared
/// result, so `shared.parsed`'s markers and tree may describe delimiters and
/// spans that live partly or wholly on a different physical line; this clips
/// them to `range` exactly as a self-contained parse already clips a node that
/// escapes it.
fn present_markdown_from_parse(
    line_id: u64,
    revision: Revision,
    range: SourceRange,
    source: &str,
    line_height: f32,
    disclosure: Option<SourceRange>,
    shared: &SharedParse<'_>,
) -> VisualLine {
    if source.is_empty() {
        let mut block = present_plain(line_id, revision, range, source);
        block.estimated_height = line_height;
        return block;
    }
    let parsed = shared.parsed;
    let formal_code_block = shared
        .list_projection
        .and_then(|projection| projection.is_code_block_for_range(range));
    // An ATX heading or a fenced/indented code block nested in an existing
    // quote/list still carries its own display kind rather than the
    // container's — a quoted or listed line inside a code block must still
    // render as code (monospace, literal, its own fence hiding), the same
    // way a nested heading keeps its level. Only nodes intersecting this
    // physical line contribute: the shared tree also contains other
    // headings and trailing blank lines. Container layout (quote tint,
    // list marker) remains owned by the indexed block for every other kind,
    // where the outermost intersecting container's own display wins.
    let mut nodes = shared.projection.nodes.intersecting(range);
    // Keep the tree's original order when multiple enclosing blocks apply.
    nodes.sort_unstable();
    let blocks = || {
        nodes
            .iter()
            .filter_map(|id| parsed.tree.node(**id))
            .filter(|node| node.kind.is_block())
    };
    let mut kind = blocks()
        .find_map(|block| match block.kind {
            NodeKind::Heading(level) => Some(BlockKind::Heading(level)),
            NodeKind::CodeBlock => Some(BlockKind::CodeBlock),
            NodeKind::Rule => Some(BlockKind::Rule),
            _ => None,
        })
        .or_else(|| blocks().find_map(|block| syntax_display(block.kind).node_block))
        .unwrap_or_default();
    match formal_code_block {
        Some(false) if kind == BlockKind::CodeBlock => kind = BlockKind::ListItem,
        Some(true) => kind = BlockKind::CodeBlock,
        _ => {}
    }
    let mut visual = String::with_capacity(source.len());
    // The ordered plan belongs to the semantic snapshot. Binary search avoids
    // copying or walking off-screen markers for each physical line.
    let plan = &shared.projection.markers;
    let start = plan.partition_point(|marker| marker.range.start < range.start);
    let end = plan.partition_point(|marker| marker.range.start < range.end);
    let mut projected_markers = plan[start..end].to_vec();
    if let Some(global) = shared.list_projection {
        for prefix in global.prefixes_in(range) {
            if let Some(marker) = projected_markers
                .iter_mut()
                .find(|marker| marker.range == prefix.source_range)
            {
                marker.global_list_prefix = Some(*prefix);
            } else {
                projected_markers.push(ProjectedMarker {
                    range: prefix.source_range,
                    fence: false,
                    fence_edge: None,
                    formal_container: true,
                    global_list_item: None,
                    formal_list_item: None,
                    formal_quote_owner: None,
                    formal_quote_depth: None,
                    quote_owner: None,
                    list_owner: None,
                    list_prefix: None,
                    global_list_prefix: Some(*prefix),
                });
            }
        }
        projected_markers.sort_by_key(|marker| (marker.range.start, marker.range.end));
    }
    if formal_code_block == Some(true) {
        if let Some(projection) = shared.list_projection {
            let formal_item = projection.item_for_range(range).copied();
            for (fence_range, edge) in projection.fence_markers_in(range) {
                let edge = match edge {
                    FenceMarkerEdge::Opening => MarkerEdge::Opening,
                    FenceMarkerEdge::Closing => MarkerEdge::Closing,
                };
                if let Some(marker) = projected_markers
                    .iter_mut()
                    .find(|marker| marker.range == fence_range)
                {
                    marker.fence = true;
                    marker.fence_edge = Some(edge);
                } else {
                    projected_markers.push(ProjectedMarker {
                        range: fence_range,
                        fence: true,
                        fence_edge: Some(edge),
                        formal_container: false,
                        global_list_item: None,
                        formal_list_item: None,
                        formal_quote_owner: None,
                        formal_quote_depth: None,
                        quote_owner: None,
                        list_owner: None,
                        list_prefix: None,
                        global_list_prefix: None,
                    });
                }
                if edge == MarkerEdge::Opening && fence_range.end < range.end {
                    // A fenced code opening's info string and trailing
                    // whitespace are source-addressable markup too. Keep it
                    // as a separate marker so the delimiter retains its
                    // opening edge in the SourceMap while the whole physical
                    // line can be disclosed for editing.
                    projected_markers.push(ProjectedMarker {
                        range: SourceRange::new(fence_range.end.0, range.end.0),
                        fence: true,
                        fence_edge: None,
                        formal_container: false,
                        global_list_item: None,
                        formal_list_item: None,
                        formal_quote_owner: None,
                        formal_quote_depth: None,
                        quote_owner: None,
                        list_owner: None,
                        list_prefix: None,
                        global_list_prefix: None,
                    });
                }
            }
            for (container_range, quote_owner, quote_depth) in
                projection.container_markers_in(range)
            {
                let global_list_item = projection
                    .item_for_marker(container_range)
                    .copied()
                    .filter(|item| formal_item == Some(*item));
                let formal_list_item = projection.item_for_marker(container_range).copied();
                if let Some(marker) = projected_markers
                    .iter_mut()
                    .find(|marker| marker.range == container_range)
                {
                    marker.formal_container = true;
                    marker.formal_quote_owner = quote_owner;
                    marker.formal_quote_depth = (quote_owner.is_some()).then_some(quote_depth);
                    marker.global_list_item = global_list_item;
                    marker.formal_list_item = formal_list_item;
                } else {
                    projected_markers.push(ProjectedMarker {
                        range: container_range,
                        fence: false,
                        fence_edge: None,
                        formal_container: true,
                        global_list_item,
                        formal_list_item,
                        formal_quote_owner: quote_owner,
                        formal_quote_depth: (quote_owner.is_some()).then_some(quote_depth),
                        quote_owner: None,
                        list_owner: None,
                        list_prefix: None,
                        global_list_prefix: None,
                    });
                }
            }
            projected_markers.sort_by_key(|marker| (marker.range.start, marker.range.end));
        }
        projected_markers.retain(|marker| {
            (marker.fence
                && shared.list_projection.is_some_and(|projection| {
                    marker.fence_edge.is_none()
                        || projection.fence_marker_edge(marker.range).is_some()
                }))
                || marker.formal_container
        });
    }
    if kind == BlockKind::CodeBlock {
        let opening_fences = projected_markers
            .iter()
            .filter(|marker| marker.fence && marker.fence_edge == Some(MarkerEdge::Opening))
            .map(|marker| marker.range)
            .collect::<Vec<_>>();
        for fence_range in opening_fences {
            if fence_range.end < range.end
                && !projected_markers.iter().any(|marker| {
                    marker.fence
                        && marker.fence_edge.is_none()
                        && marker.range.start == fence_range.end
                        && marker.range.end == range.end
                })
            {
                // Keep the info string separate from the delimiter even when
                // this line came from the shared parse rather than the formal
                // list projection. Both markers disclose with the physical
                // opening-line policy below.
                projected_markers.push(ProjectedMarker {
                    range: SourceRange::new(fence_range.end.0, range.end.0),
                    fence: true,
                    fence_edge: None,
                    formal_container: false,
                    global_list_item: None,
                    formal_list_item: None,
                    formal_quote_owner: None,
                    formal_quote_depth: None,
                    quote_owner: None,
                    list_owner: None,
                    list_prefix: None,
                    global_list_prefix: None,
                });
            }
        }
        projected_markers.sort_by_key(|marker| (marker.range.start, marker.range.end));
    }
    let markers_on_line = projected_markers.as_slice();
    let mut quote_owner = None;
    let mut quote_depth = 0;
    for id in &nodes {
        let Some(node) = parsed.tree.node(**id) else {
            continue;
        };
        if node.kind != NodeKind::Quote {
            continue;
        }
        let depth = parsed
            .tree
            .ancestors(**id)
            .filter(|ancestor| {
                parsed
                    .tree
                    .node(*ancestor)
                    .is_some_and(|node| node.kind == NodeKind::Quote)
            })
            .count();
        if depth > quote_depth {
            quote_depth = depth;
            quote_owner = Some(node.source_range);
        }
    }
    if let Some(projection) = shared.list_projection {
        for quote in projection.quotes_in(range) {
            // A bounded viewport parse may already have selected a different
            // owner at this depth. The formal projection is document-wide,
            // so its owner is canonical even when the depth ties.
            if quote.depth >= quote_depth {
                quote_depth = quote.depth;
                quote_owner = Some(quote.source_range);
            }
        }
    }
    for marker in &projected_markers {
        let Some(owner) = marker.formal_quote_owner else {
            continue;
        };
        let Some(depth) = marker.formal_quote_depth else {
            continue;
        };
        // Formal marker metadata has the same canonical-owner priority as the
        // document-wide quote projection above.
        if depth >= quote_depth {
            quote_depth = depth;
            quote_owner = Some(owner);
        }
    }
    let disclosed_quote_depth = markers_on_line
        .iter()
        .filter(|planned| {
            (planned.quote_owner.is_some() || planned.formal_quote_owner.is_some())
                && marker_is_disclosed(
                    planned,
                    parsed,
                    &shared.projection.nodes,
                    disclosure,
                    shared.list_projection,
                )
        })
        .count()
        .min(quote_depth);
    let quote = quote_owner.map(|owner| QuoteRowMetadata {
        depth: quote_depth,
        owner,
        disclosed_depth: disclosed_quote_depth,
    });
    if quote_depth > 0 && kind == BlockKind::Paragraph {
        kind = BlockKind::Quote;
    }
    if kind == BlockKind::Rule {
        let mut line = present_rule_line(
            line_id,
            revision,
            range,
            source,
            line_height,
            disclosure,
            quote,
            shared,
            markers_on_line,
        );
        // A rule still owns the surrounding list row. Keep the specialized
        // separator/source disclosure presentation, but do not skip the
        // semantic list projection that supplies list ownership/marker
        // semantics, prefixes and body geometry to layout and editing.
        line.list = list_row_metadata(
            parsed,
            shared.projection,
            range,
            markers_on_line,
            &line.visual_text,
            &line.source_map,
            disclosure,
            shared.list_projection,
        );
        return line;
    }
    let mut segments = Vec::with_capacity(markers_on_line.len() * 2 + 1);
    let mut source_cursor = range.start.0;
    for (position, planned) in markers_on_line.iter().enumerate() {
        let marker = planned.range;
        if marker.end > range.end {
            continue;
        }
        if source_cursor < marker.start.0 {
            append_segment(
                &mut visual,
                &mut segments,
                source,
                range,
                SourceRange::new(source_cursor, marker.start.0),
                Visibility::Visible,
                None,
            );
        }
        let expanded = if planned.fence {
            // Fenced-code opening rows are edited as one physical source line:
            // a caret/selection/IME anywhere in the delimiter, info string or
            // trailing whitespace reveals the complete raw line. The formal
            // projection uses a second fence marker with no edge for the info
            // string, while the delimiter keeps MarkerEdge::Opening.
            disclosure.is_some_and(|active| {
                disclosure_owns_physical_line(range, source, active)
                    && (planned.fence_edge == Some(MarkerEdge::Opening)
                        || planned.fence_edge.is_none())
            })
        } else {
            marker_is_disclosed(
                planned,
                parsed,
                &shared.projection.nodes,
                disclosure,
                shared.list_projection,
            )
        };
        append_segment(
            &mut visual,
            &mut segments,
            source,
            range,
            marker,
            if expanded {
                Visibility::ExpandedMarkup
            } else {
                Visibility::HiddenMarkup
            },
            marker_edge(planned, parsed, &shared.projection.nodes),
        );
        // An inactive list item marker is replaced by one synthesized
        // bullet/number, anchored at the hidden marker's own start rather
        // than carrying an independent fake source position (see
        // `list_item_label`). Disclosing the marker (`expanded` above) shows
        // its real source bytes directly instead, so the two never render
        // together.
        let label = if let Some(owner) = planned.list_owner {
            list_item_label_with_projection(
                parsed,
                &shared.projection.list_item_ordinals,
                owner,
                shared.list_projection,
                planned.range,
            )
        } else {
            planned
                .global_list_item
                .map(|item| list_label(item.start, item.ordinal))
        };
        // A confirmed task marker directly after the list marker is part of
        // the same opening prefix: the synthesized label also carries the
        // checkbox, while the real `[ ]` bytes stay mapped (hidden, or
        // expanded as ordinary source once the item is disclosed).
        let task = label.as_ref().and_then(|_| {
            task_after_marker(
                planned_task(planned, shared.projection),
                marker,
                range,
                markers_on_line
                    .get(position + 1)
                    .map(|next| next.range.start),
            )
        });
        let label = match (&task, label) {
            (Some(task), Some(label)) if !expanded => {
                Some(label + &task_label_suffix(task.checked))
            }
            (_, label) => label,
        };
        if !expanded && let Some(label) = label {
            let visual_start = visual.len();
            visual.push_str(&label);
            segments.push(MappingSegment {
                source_range: SourceRange::empty(marker.start.0),
                visual_range: VisualRange::new(visual_start, visual.len()),
                visibility: Visibility::Synthesized,
                marker_edge: None,
            });
        }
        if let Some(task) = task {
            append_segment(
                &mut visual,
                &mut segments,
                source,
                range,
                task.hidden_range,
                if expanded {
                    Visibility::ExpandedMarkup
                } else {
                    Visibility::HiddenMarkup
                },
                Some(MarkerEdge::Opening),
            );
            source_cursor = task.hidden_range.end.0;
            continue;
        }
        // An inactive list item's structural continuation indentation has no
        // visual representation. Layout owns the semantic body column; adding
        // spaces here would make source indentation part of the text being
        // shaped and would make caret/hit-test geometry disagree with it.
        // Disclosing the item still shows the original source spaces or tabs
        // through this marker's ExpandedMarkup segment.
        source_cursor = marker.end.0;
    }
    if source_cursor < range.end.0 {
        append_segment(
            &mut visual,
            &mut segments,
            source,
            range,
            SourceRange::new(source_cursor, range.end.0),
            Visibility::Visible,
            None,
        );
    }
    if segments.is_empty() {
        segments.push(MappingSegment {
            source_range: range,
            visual_range: VisualRange::new(0, visual.len()),
            visibility: Visibility::Visible,
            marker_edge: None,
        });
    }
    // Contract guard: if marker derivation for an unsupported construct left the
    // source range only partially covered, degrade to raw source rather than
    // hiding or dropping the uncovered bytes.
    if !segments_tile_range(range, &segments) {
        return present_raw_source(line_id, revision, range, source, line_height);
    }
    let source_map = SourceMap { segments };
    // A style run's node may also span lines that are not this one; clipping to
    // `range` (as already done here) and letting `source_to_visual` fail outside
    // it is what discards the part that belongs elsewhere.
    let mut style_runs = nodes
        .iter()
        .filter_map(|id| parsed.tree.node(**id))
        .filter(|node| {
            formal_code_block != Some(true)
                && !(formal_code_block == Some(false) && matches!(node.kind, NodeKind::CodeBlock))
        })
        .filter(|node| has_delimiter_markers(node.kind))
        .filter_map(|span| {
            let style = syntax_display(span.kind).inline_style?;
            let clipped = SourceRange {
                start: span.source_range.start.max(range.start),
                end: span.source_range.end.min(range.end),
            };
            let start = source_map
                .source_to_visual(clipped.start, Bias::After)?
                .visual_offset;
            let end = source_map
                .source_to_visual(clipped.end, Bias::Before)?
                .visual_offset;
            (start.0 < end.0).then_some(StyleRun {
                visual_range: VisualRange { start, end },
                kind: style,
            })
        })
        .collect::<Vec<_>>();
    if formal_code_block == Some(true)
        && !style_runs
            .iter()
            .any(|run| run.kind == StyleKind::CodeBlock)
    {
        let content_end = visual.trim_end_matches(['\r', '\n']).len();
        if content_end > 0 {
            style_runs.push(StyleRun {
                visual_range: VisualRange::new(0, content_end),
                kind: StyleKind::CodeBlock,
            });
        }
    }
    style_runs.sort_by_key(|run| (run.visual_range.start.0, run.visual_range.end.0));
    // Openable links carry the parser's destination with the part of their text
    // on this physical line; the UI matches a click's source offset against it.
    let mut links = if formal_code_block == Some(true) {
        Vec::new()
    } else {
        links_intersecting(&parsed.links, range)
            .iter()
            .filter(|link| nodes.iter().any(|id| **id == link.node))
            .filter_map(|link| {
                let clipped = SourceRange {
                    start: link.text_range.start.max(range.start),
                    end: link.text_range.end.min(range.end),
                };
                (clipped.start < clipped.end).then(|| LinkTarget {
                    source_range: clipped,
                    destination: link.destination.clone(),
                    uses_reference: link.uses_reference,
                })
            })
            .collect::<Vec<_>>()
    };
    // A line parsed on its own cannot see a reference definition on another
    // line. The formal projection resolved every link against the whole
    // document, so restore the ones the local parse could not.
    if formal_code_block != Some(true)
        && let Some(projection) = shared.list_projection
    {
        for link in projection.links_in(range) {
            let clipped = SourceRange {
                start: link.text_range.start.max(range.start),
                end: link.text_range.end.min(range.end),
            };
            if clipped.start < clipped.end
                && !links
                    .iter()
                    .any(|existing| existing.source_range.intersects(clipped))
            {
                links.push(LinkTarget {
                    source_range: clipped,
                    destination: link.destination.clone(),
                    uses_reference: link.uses_reference,
                });
            }
        }
        links.sort_by_key(|link| (link.source_range.start, link.source_range.end));
    }
    let (quote_marker_visual_ranges, quote_marker_source_ranges) =
        quote_marker_ranges(markers_on_line, &source_map.segments);
    let list = list_row_metadata(
        parsed,
        shared.projection,
        range,
        markers_on_line,
        &visual,
        &source_map,
        disclosure,
        shared.list_projection,
    );
    let has_hidden_fence = kind == BlockKind::CodeBlock
        && markers_on_line
            .iter()
            .filter(|marker| marker.fence)
            .any(|marker| {
                source_map.segments.iter().any(|segment| {
                    segment.source_range == marker.range
                        && segment.visibility == Visibility::HiddenMarkup
                })
            });
    let line_estimated_height = if kind == BlockKind::CodeBlock {
        fenced_line_height(
            &visual,
            has_hidden_fence,
            disclosure.is_some_and(|active| disclosure_owns_physical_line(range, source, active)),
            line_height,
        )
    } else {
        estimated_height(kind, line_height)
    };
    VisualLine {
        line_id,
        source_range: range,
        revision,
        visual_text: visual,
        style_runs,
        links,
        source_map,
        estimated_height: line_estimated_height,
        measured_height: None,
        invalid: false,
        kind,
        context: LineContext::Normal,
        disclosure,
        image: None,
        list,
        quote,
        quote_marker_visual_ranges,
        quote_marker_source_ranges,
        table_row: None,
        table_header: false,
    }
}

/// The standalone image `source` presents as, when nothing currently discloses
/// it. Shared by [`present_polished_line`]'s own single-line dispatch and
/// [`present_joined_run`]'s per-line dispatch inside a joined multi-line run,
/// so both agree on exactly which physical lines render through
/// [`present_image`] rather than the shared/self-contained markdown parse —
/// an image line's disclosure state is its own, not the run-wide merged one a
/// joined run otherwise shares (see [`merged_disclosure`]), since only a
/// disclosure that actually touches this line's own range should reveal its
/// raw markup.
fn inactive_standalone_image<'a>(
    source: &'a str,
    range: SourceRange,
    disclosure: Option<SourceRange>,
) -> Option<StandaloneImage<'a>> {
    parse_standalone_image(source)
        .filter(|_| disclosure.is_none_or(|active| !range_touches(range, active)))
}

fn formal_quote_metadata(
    range: SourceRange,
    projection: &ListProjection,
) -> Option<QuoteRowMetadata> {
    let quote = projection
        .quotes_in(range)
        .max_by_key(|quote| quote.depth)?;
    Some(QuoteRowMetadata {
        depth: quote.depth,
        owner: quote.source_range,
        disclosed_depth: 0,
    })
}

/// Balanced interval index. Each subtree stores its greatest source end, so
/// a long enclosing construct does not force a scan of unrelated short spans.
#[derive(Clone, Debug)]
struct SourceIndex<T> {
    entries: Vec<(SourceRange, T)>,
    max_ends: Vec<SourceOffset>,
}

impl<T> SourceIndex<T> {
    fn new(mut entries: Vec<(SourceRange, T)>) -> Self {
        entries.sort_by_key(|(range, _)| (range.start, range.end));
        let mut index = Self {
            max_ends: vec![SourceOffset(0); entries.len()],
            entries,
        };
        index.build(0, index.entries.len());
        index
    }

    fn build(&mut self, start: usize, end: usize) -> SourceOffset {
        if start == end {
            return SourceOffset(0);
        }
        let mid = start + (end - start) / 2;
        let max = self.entries[mid]
            .0
            .end
            .max(self.build(start, mid))
            .max(self.build(mid + 1, end));
        self.max_ends[mid] = max;
        max
    }

    fn intersecting(&self, range: SourceRange) -> Vec<&T> {
        let mut result = Vec::new();
        self.query(0, self.entries.len(), range, &mut result, &mut 0);
        result
    }

    fn query<'a>(
        &'a self,
        start: usize,
        end: usize,
        range: SourceRange,
        result: &mut Vec<&'a T>,
        visited: &mut usize,
    ) {
        if start == end {
            return;
        }
        let mid = start + (end - start) / 2;
        *visited += 1;
        if self.max_ends[mid] <= range.start || self.entries[start].0.start >= range.end {
            return;
        }
        self.query(start, mid, range, result, visited);
        let (source, value) = &self.entries[mid];
        if source.intersects(range) {
            result.push(value);
        }
        self.query(mid + 1, end, range, result, visited);
    }
}

#[derive(Clone, Debug)]
struct ProjectedMarker {
    range: SourceRange,
    fence: bool,
    fence_edge: Option<MarkerEdge>,
    formal_container: bool,
    global_list_item: Option<ListProjectionItem>,
    /// Formal list-item owner used only for disclosure. This intentionally
    /// keeps every ancestor marker's owner, while `global_list_item` below is
    /// restricted to the deepest item for one row's list metadata and label.
    formal_list_item: Option<ListProjectionItem>,
    /// Source-range owner for a quote marker recovered from the formal parse.
    /// Unlike `quote_owner`, this remains valid when the viewport parse has a
    /// different `NodeId` allocation.
    formal_quote_owner: Option<SourceRange>,
    formal_quote_depth: Option<usize>,
    quote_owner: Option<NodeId>,
    list_owner: Option<NodeId>,
    /// `Some((owner, columns))` for a list item's own structural continuation
    /// indentation (`MarkdownParse::list_structural_prefixes`): the owning
    /// item and the semantic column width the layout uses for its body.
    /// Distinct from `list_owner`, which is the item's own opening
    /// bullet/number and gets a synthesized label.
    list_prefix: Option<(NodeId, usize)>,
    /// A structural prefix recovered from the formal block projection when
    /// the viewport parse has no local `ListItem` owner for this line.
    global_list_prefix: Option<ListProjectionPrefix>,
}

#[derive(Clone, Copy, Debug)]
struct ListParagraphRegion {
    range: SourceRange,
    ordinal: usize,
    after_nested_list: bool,
}

#[derive(Clone, Debug)]
struct ListItemProjection {
    owner: ListOwnerMetadata,
    paragraphs: Vec<ListParagraphRegion>,
    nested_lists: Vec<SourceRange>,
}

fn list_marker_kind(start: Option<u64>) -> ListMarkerKind {
    start.map_or(ListMarkerKind::Bullet, |_| ListMarkerKind::Ordered)
}

fn list_label(start: Option<u64>, ordinal: usize) -> String {
    match start {
        None => "\u{2022} ".to_owned(),
        Some(start) => format!("{}. ", start.saturating_add(ordinal as u64)),
    }
}

/// The confirmed task marker of one list item, by source range so it is the
/// same value whether it came from the local parse or the document-wide list
/// projection.
#[derive(Clone, Copy, Debug)]
struct PlannedTask {
    marker_range: SourceRange,
    hidden_range: SourceRange,
    checked: bool,
}

/// The checkbox text appended to an inactive item's bullet/number label. It
/// is the same text `hane_markdown` measures for the list-wide alignment.
fn task_label_suffix(checked: bool) -> String {
    format!("{} ", task_glyph(checked))
}

/// The task marker of the item whose opening list marker is `planned`. The
/// local parse's item wins over the formal projection's, matching how the
/// bullet label itself is chosen.
fn planned_task(planned: &ProjectedMarker, projection: &ProjectionIndex) -> Option<PlannedTask> {
    if let Some(owner) = planned.list_owner {
        return projection
            .list_item_tasks
            .get(owner.0)
            .and_then(|task| *task);
    }
    let task = planned.global_list_item?.task?;
    Some(PlannedTask {
        marker_range: task.marker_range,
        hidden_range: task.hidden_range,
        checked: task.checked,
    })
}

/// The task marker to present directly after `marker` on the physical line
/// `range`. It must start exactly where the list marker ends and fit on the
/// line without touching the next planned marker, so the hidden bytes tile
/// the source with no gap or overlap.
fn task_after_marker(
    task: Option<PlannedTask>,
    marker: SourceRange,
    range: SourceRange,
    next_marker_start: Option<SourceOffset>,
) -> Option<PlannedTask> {
    task.filter(|task| {
        task.hidden_range.start == marker.end
            && task.hidden_range.end <= range.end
            && next_marker_start.is_none_or(|next| next >= task.hidden_range.end)
    })
}

fn projected_item_for_marker(
    projection: Option<&ListProjection>,
    marker_range: SourceRange,
) -> Option<&ListProjectionItem> {
    projection?.item_for_marker(marker_range)
}

fn projected_item_for_range(
    projection: Option<&ListProjection>,
    range: SourceRange,
) -> Option<&ListProjectionItem> {
    projection?.item_for_range(range)
}

fn projected_list_alignment(
    projection: &ListProjection,
    item: &ListProjectionItem,
) -> Option<ListAlignment> {
    let list = projection.list(item.list_range)?;
    Some(ListAlignment {
        max_marker_label: list.max_marker_label.clone(),
        max_marker_columns: list.max_marker_columns,
        marker_labels: list.marker_labels.clone(),
    })
}

fn list_alignments(
    tree: &MarkdownTree,
    tasks: &[Option<PlannedTask>],
) -> Vec<Option<ListAlignment>> {
    let mut alignments = vec![None; tree.len()];
    for (id, node) in tree.iter() {
        let NodeKind::List { start } = node.kind else {
            continue;
        };
        let marker_labels = tree
            .children(id)
            .iter()
            .enumerate()
            .map(|(ordinal, child)| {
                let mut label = list_label(start, ordinal);
                if let Some(task) = tasks.get(child.0).and_then(|task| *task) {
                    label.push_str(&task_label_suffix(task.checked));
                }
                label
            })
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
        alignments[id.0] = Some(ListAlignment {
            max_marker_label,
            max_marker_columns,
            marker_labels: marker_labels.into(),
        });
    }
    alignments
}

/// Groups the direct content of an item into semantic paragraph regions. Tight
/// list items have inline children directly under the item, while loose items
/// have explicit Paragraph nodes; treating adjacent inline children as one
/// region keeps both forms on the same presentation contract.
fn list_paragraph_regions(tree: &MarkdownTree, item: NodeId) -> Vec<ListParagraphRegion> {
    let mut regions = Vec::new();
    let mut paragraph_ordinal = 0;
    let mut after_nested_list = false;
    let mut inline_region: Option<(SourceOffset, SourceOffset, bool)> = None;

    let flush_inline = |regions: &mut Vec<ListParagraphRegion>,
                        inline_region: &mut Option<(SourceOffset, SourceOffset, bool)>,
                        ordinal: &mut usize| {
        if let Some((start, end, after_nested_list)) = inline_region.take() {
            regions.push(ListParagraphRegion {
                range: SourceRange { start, end },
                ordinal: *ordinal,
                after_nested_list,
            });
            *ordinal += 1;
        }
    };

    for child_id in tree.children(item) {
        let Some(child) = tree.node(*child_id) else {
            continue;
        };
        if matches!(child.kind, NodeKind::List { .. }) {
            flush_inline(&mut regions, &mut inline_region, &mut paragraph_ordinal);
            after_nested_list = true;
            continue;
        }
        if child.kind.is_block() {
            flush_inline(&mut regions, &mut inline_region, &mut paragraph_ordinal);
            regions.push(ListParagraphRegion {
                range: child.source_range,
                ordinal: paragraph_ordinal,
                after_nested_list,
            });
            paragraph_ordinal += 1;
        } else {
            let entry = inline_region.get_or_insert((
                child.source_range.start,
                child.source_range.end,
                after_nested_list,
            ));
            entry.0 = entry.0.min(child.source_range.start);
            entry.1 = entry.1.max(child.source_range.end);
        }
    }
    flush_inline(&mut regions, &mut inline_region, &mut paragraph_ordinal);
    regions
}

fn list_nested_ranges(tree: &MarkdownTree, item: NodeId) -> Vec<SourceRange> {
    tree.children(item)
        .iter()
        .filter_map(|child_id| {
            let child = tree.node(*child_id)?;
            matches!(child.kind, NodeKind::List { .. }).then_some(child.source_range)
        })
        .collect()
}

fn list_item_projections(
    tree: &MarkdownTree,
    ordinals: &[Option<(NodeId, usize)>],
    alignments: &[Option<ListAlignment>],
) -> Vec<Option<ListItemProjection>> {
    let mut items = vec![None; tree.len()];
    for (item_id, node) in tree.iter() {
        if !matches!(node.kind, NodeKind::ListItem { .. }) {
            continue;
        }
        let Some((list_id, ordinal)) = ordinals.get(item_id.0).and_then(|entry| *entry) else {
            continue;
        };
        let Some(NodeKind::List { start }) = tree.node(list_id).map(|node| node.kind) else {
            continue;
        };
        let Some(alignment) = alignments.get(list_id.0).and_then(Clone::clone) else {
            continue;
        };
        items[item_id.0] = Some(ListItemProjection {
            owner: ListOwnerMetadata {
                list_id: ListId(list_id.0 as u64),
                item_id: ListItemId(item_id.0 as u64),
                marker_kind: list_marker_kind(start),
                start,
                ordinal,
                depth: tree.list_depth(item_id),
                alignment,
            },
            paragraphs: list_paragraph_regions(tree, item_id),
            nested_lists: list_nested_ranges(tree, item_id),
        });
    }
    items
}

#[derive(Clone, Debug)]
struct ProjectionIndex {
    markers: Vec<ProjectedMarker>,
    nodes: SourceIndex<NodeId>,
    list_items: SourceIndex<NodeId>,
    /// Owner list and zero-based sibling position for every list item in
    /// the parse, indexed by [`NodeId::0`]. Built once here — a single pass
    /// over each list's own children — so [`list_item_label`] looks a
    /// visible item's ordinal up in `O(1)` instead of every visible line
    /// re-deriving it with [`MarkdownTree::list_item_ordinal`]'s
    /// preceding-sibling scan.
    list_item_ordinals: Vec<Option<(NodeId, usize)>>,
    list_item_projections: Vec<Option<ListItemProjection>>,
    /// Confirmed task marker per list item, indexed by [`NodeId::0`]. Only the
    /// parser's direct-child `TaskMarker` of each item appears here.
    list_item_tasks: Vec<Option<PlannedTask>>,
}

/// [`ProjectionIndex::list_item_ordinals`]'s builder: every `List` node's
/// children, in the document order [`MarkdownTree::children`] already keeps
/// them in, get positions `0, 1, 2, ...`. Each list item is visited exactly
/// once across the whole tree, so this is `O(node count)` total regardless
/// of how many lines a later viewport projects from it.
fn list_item_ordinals(tree: &MarkdownTree) -> Vec<Option<(NodeId, usize)>> {
    let mut ordinals = vec![None; tree.len()];
    for (id, node) in tree.iter() {
        if matches!(node.kind, NodeKind::List { .. }) {
            for (position, child) in tree.children(id).iter().enumerate() {
                ordinals[child.0] = Some((id, position));
            }
        }
    }
    ordinals
}

impl ProjectionIndex {
    fn new(parsed: &MarkdownParse) -> Self {
        let owners = parsed
            .quote_markers
            .iter()
            .map(|(range, owner)| ((range.start, range.end), *owner))
            .collect::<std::collections::BTreeMap<_, _>>();
        let list_owners = parsed
            .list_item_markers
            .iter()
            .map(|(range, owner)| ((range.start, range.end), *owner))
            .collect::<std::collections::BTreeMap<_, _>>();
        let list_prefixes = parsed
            .list_structural_prefixes
            .iter()
            .map(|(range, owner, columns)| ((range.start, range.end), (*owner, *columns)))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut markers = parsed
            .markers
            .iter()
            .chain(&parsed.code_padding)
            .chain(&parsed.line_break_padding)
            .chain(
                parsed
                    .list_structural_prefixes
                    .iter()
                    .map(|(range, _, _)| range),
            )
            .map(|range| ProjectedMarker {
                range: *range,
                fence: parsed
                    .fence_markers
                    .binary_search_by_key(&(range.start, range.end), |marker| {
                        (marker.start, marker.end)
                    })
                    .is_ok(),
                fence_edge: parsed
                    .fence_marker_edges
                    .binary_search_by_key(&(range.start, range.end), |(marker, _)| {
                        (marker.start, marker.end)
                    })
                    .ok()
                    .map(|index| match parsed.fence_marker_edges[index].1 {
                        FenceMarkerEdge::Opening => MarkerEdge::Opening,
                        FenceMarkerEdge::Closing => MarkerEdge::Closing,
                    }),
                formal_container: false,
                global_list_item: None,
                formal_list_item: None,
                formal_quote_owner: None,
                formal_quote_depth: None,
                quote_owner: owners.get(&(range.start, range.end)).copied(),
                list_owner: list_owners.get(&(range.start, range.end)).copied(),
                list_prefix: list_prefixes.get(&(range.start, range.end)).copied(),
                global_list_prefix: None,
            })
            .collect::<Vec<_>>();
        markers.sort_by_key(|marker| (marker.range.start, marker.range.end));
        let nodes = SourceIndex::new(
            parsed
                .tree
                .iter()
                .filter(|(_, node)| node.kind.is_block() || has_delimiter_markers(node.kind))
                .map(|(id, node)| (node.source_range, id))
                .collect(),
        );
        let list_item_ordinals = list_item_ordinals(&parsed.tree);
        let mut list_item_tasks = vec![None; parsed.tree.len()];
        for task in &parsed.task_markers {
            if let Some(slot) = list_item_tasks.get_mut(task.item.0) {
                *slot = Some(PlannedTask {
                    marker_range: task.marker_range,
                    hidden_range: task.hidden_range,
                    checked: task.checked,
                });
            }
        }
        let list_alignments = list_alignments(&parsed.tree, &list_item_tasks);
        let list_item_projections =
            list_item_projections(&parsed.tree, &list_item_ordinals, &list_alignments);
        let list_items = SourceIndex::new(
            parsed
                .tree
                .iter()
                .filter(|(_, node)| matches!(node.kind, NodeKind::ListItem { .. }))
                .map(|(id, node)| (node.source_range, id))
                .collect(),
        );
        Self {
            markers,
            nodes,
            list_items,
            list_item_ordinals,
            list_item_projections,
            list_item_tasks,
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "list marker projection keeps source, visual, disclosure and task inputs together"
)]
fn list_marker_metadata(
    planned: &ProjectedMarker,
    parsed: &MarkdownParse,
    nodes: &SourceIndex<NodeId>,
    visual: &str,
    source_map: &SourceMap,
    disclosure: Option<SourceRange>,
    list_projection: Option<&ListProjection>,
    task: Option<PlannedTask>,
) -> Option<ListMarkerMetadata> {
    let expanded = marker_is_disclosed(planned, parsed, nodes, disclosure, list_projection);
    let visual_range = if expanded {
        source_map
            .segments
            .iter()
            .find(|segment| {
                segment.source_range == planned.range
                    && segment.visibility == Visibility::ExpandedMarkup
            })
            .map(|segment| segment.visual_range)
    } else {
        source_map
            .segments
            .iter()
            .find(|segment| {
                segment.visibility == Visibility::Synthesized
                    && segment.source_range.is_empty()
                    && segment.source_range.start == planned.range.start
                    && segment.visual_range.start.0 < segment.visual_range.end.0
            })
            .map(|segment| segment.visual_range)
    }?;
    let label = visual
        .get(visual_range.start.0..visual_range.end.0)?
        .to_owned();
    // Report the task only when this very row mapped its bytes (hidden behind
    // the checkbox, or expanded as source) — never a control the row did not
    // actually present.
    let task = task.and_then(|task| {
        let mapped = source_map.segments.iter().any(|segment| {
            segment.source_range == task.hidden_range
                && segment.visibility
                    == if expanded {
                        Visibility::ExpandedMarkup
                    } else {
                        Visibility::HiddenMarkup
                    }
        });
        if !mapped {
            return None;
        }
        let checkbox_visual_range = if expanded {
            None
        } else {
            let suffix = task_label_suffix(task.checked);
            let glyph_len = suffix.len() - 1;
            if !label.ends_with(&suffix) {
                return None;
            }
            Some(VisualRange::new(
                visual_range.end.0 - suffix.len(),
                visual_range.end.0 - suffix.len() + glyph_len,
            ))
        };
        Some(ListTaskMetadata {
            marker_range: task.marker_range,
            checked: task.checked,
            checkbox_visual_range,
        })
    });
    Some(ListMarkerMetadata {
        source_range: planned.range,
        anchor: planned.range.start,
        visual_range,
        label,
        synthesized: !expanded,
        task,
    })
}

fn list_row_paragraph(
    projection: &ListItemProjection,
    range: SourceRange,
) -> Option<&ListParagraphRegion> {
    projection
        .paragraphs
        .iter()
        .find(|region| region.range.intersects(range))
        .or_else(|| {
            let nested_before = projection
                .nested_lists
                .iter()
                .filter(|nested| nested.end <= range.start)
                .max_by_key(|nested| nested.end);
            projection.paragraphs.iter().find(|region| {
                if region.range.start < range.end {
                    return false;
                }
                // A blank row before a nested list belongs to the item but not
                // to the paragraph after that list. Once the nested range is
                // behind the row, the following parent paragraph is the
                // correct hanging-indent owner.
                !region.after_nested_list || nested_before.is_some()
            })
        })
        .or_else(|| {
            let nested_before = projection
                .nested_lists
                .iter()
                .any(|nested| nested.end <= range.start);
            nested_before
                .then(|| {
                    projection
                        .paragraphs
                        .iter()
                        .rev()
                        .find(|region| region.after_nested_list)
                })
                .flatten()
        })
}

#[allow(
    clippy::too_many_arguments,
    reason = "list row projection keeps source, visual and disclosure inputs together"
)]
fn list_row_metadata(
    parsed: &MarkdownParse,
    projection: &ProjectionIndex,
    range: SourceRange,
    markers_on_line: &[ProjectedMarker],
    visual: &str,
    source_map: &SourceMap,
    disclosure: Option<SourceRange>,
    list_projection: Option<&ListProjection>,
) -> Option<ListRowMetadata> {
    let item = projection
        .list_items
        .intersecting(range)
        .into_iter()
        .copied()
        .max_by_key(|id| parsed.tree.list_depth(*id));
    let item_projection = item.and_then(|item| {
        projection
            .list_item_projections
            .get(item.0)
            .and_then(Option::as_ref)
    });
    let projected = projected_item_for_range(list_projection, range);
    if item_projection.is_none() && projected.is_none() {
        return None;
    }
    let opening = markers_on_line.iter().find(|planned| {
        (item.is_some_and(|item| planned.list_owner == Some(item))
            || projected.is_some_and(|projected| planned.global_list_item == Some(*projected)))
            && planned.range.start >= range.start
            && planned.range.start < range.end
    });
    let role = if opening.is_some() {
        ListRowRole::Opening
    } else if let Some(item_projection) = item_projection
        && let Some(region) = list_row_paragraph(item_projection, range)
    {
        if region.after_nested_list {
            ListRowRole::ParentParagraphAfterNestedList {
                ordinal: region.ordinal,
            }
        } else if region.ordinal > 0 {
            ListRowRole::LooseParagraph {
                ordinal: region.ordinal,
            }
        } else {
            ListRowRole::Continuation
        }
    } else {
        ListRowRole::Continuation
    };
    let marker = opening.and_then(|planned| {
        let next_marker_start = markers_on_line
            .iter()
            .find(|next| {
                (next.range.start, next.range.end) > (planned.range.start, planned.range.end)
            })
            .map(|next| next.range.start);
        list_marker_metadata(
            planned,
            parsed,
            &projection.nodes,
            visual,
            source_map,
            disclosure,
            list_projection,
            task_after_marker(
                planned_task(planned, projection),
                planned.range,
                range,
                next_marker_start,
            ),
        )
    });
    let structural_prefixes = markers_on_line
        .iter()
        .filter_map(|planned| {
            if let Some((owner, columns)) = planned.list_prefix {
                return Some(ListStructuralPrefixMetadata {
                    item_id: ListItemId(owner.0 as u64),
                    source_range: planned.range,
                    columns,
                });
            }
            let prefix = planned.global_list_prefix?;
            Some(ListStructuralPrefixMetadata {
                item_id: ListItemId(prefix.item_range.start.0 as u64),
                source_range: prefix.source_range,
                columns: prefix.columns,
            })
        })
        .collect::<Vec<_>>();
    let body_visual_start = markers_on_line
        .iter()
        .filter(|planned| {
            planned.list_owner.is_some()
                || planned.list_prefix.is_some()
                || planned.global_list_prefix.is_some()
                || planned.global_list_item.is_some()
                || planned.formal_container
        })
        .flat_map(|planned| {
            source_map.segments.iter().filter_map(|segment| {
                let same_source = segment.source_range == planned.range;
                let synthesized = segment.visibility == Visibility::Synthesized
                    && segment.source_range.is_empty()
                    && segment.source_range.start == planned.range.start;
                (same_source || synthesized).then_some(segment.visual_range.end)
            })
        })
        .max()
        .unwrap_or(VisualOffset(0));
    let owner = if let Some(projected) = projected {
        let list_projection = list_projection?;
        let alignment = projected_list_alignment(list_projection, projected)?;
        let mut owner = item_projection.map_or_else(
            || ListOwnerMetadata {
                list_id: ListId(projected.list_range.start.0 as u64),
                item_id: ListItemId(projected.item_range.start.0 as u64),
                marker_kind: list_marker_kind(projected.start),
                start: projected.start,
                ordinal: projected.ordinal,
                depth: projected.depth,
                alignment: alignment.clone(),
            },
            |item_projection| item_projection.owner.clone(),
        );
        owner.list_id = ListId(projected.list_range.start.0 as u64);
        owner.item_id = ListItemId(projected.item_range.start.0 as u64);
        owner.start = projected.start;
        owner.ordinal = projected.ordinal;
        owner.depth = projected.depth;
        owner.alignment = alignment;
        owner
    } else {
        item_projection?.owner.clone()
    };
    Some(ListRowMetadata {
        owner,
        role,
        marker,
        structural_prefixes,
        body_visual_start,
    })
}

/// One joinable block's whole-span parse, kept independent of which lines a
/// particular presentation call is drawing.
///
/// CommonMark resolves `**`/`_`/`` ` `` across a whole paragraph, so
/// presenting it correctly needs a parse of its entire joined source — but a
/// block has no size limit (see [`VisualBlock`]), and only the lines that
/// reach the viewport need to be turned into [`VisualLine`]s. Splitting the
/// two lets a caller compute this once, off the render path for a block too
/// large to reparse on every viewport miss, and reuse it for every window
/// drawn from the block afterward (see [`BlockWindow::joined`]) — so the
/// result of a marker pair arbitrarily far apart does not depend on where the
/// viewport happens to sit, the way a fixed context window around it would.
#[derive(Clone, Debug)]
pub struct JoinedParse {
    /// Byte range the joined source was read from; a stale cache (an edit
    /// touched the block, or the index re-tiled it) is one whose block no
    /// longer reports this same range.
    pub source_range: SourceRange,
    pub joined_source: String,
    pub parsed: MarkdownParse,
    projection: ProjectionIndex,
}

/// Parses `lines` joined into one source, from texts the caller already has
/// in hand. Used both as [`present_joined_run`]'s fallback when no cached
/// [`JoinedParse`] is available, and to build one from a bounded run of
/// [`BlockLine`]s.
pub fn parse_joined_block(lines: &[BlockLine<'_>], revision: Revision) -> JoinedParse {
    let joined_range = SourceRange::new(lines[0].range.start.0, lines[lines.len() - 1].range.end.0);
    let joined_source = lines.iter().map(|line| line.text).collect::<String>();
    let parsed = parse_document(revision, joined_range, &joined_source);
    let projection = ProjectionIndex::new(&parsed);
    JoinedParse {
        source_range: joined_range,
        joined_source,
        parsed,
        projection,
    }
}

/// Reads and parses one joinable block's whole content span directly from
/// `document`, independent of any particular viewport. Meant to run off the
/// render path (see [`BlockWindow::joined`]): a single read and parse whose
/// cost scales with the block's size, which for a joinable block has no
/// upper bound. `content` excludes the block's trailing blank run, matching
/// what [`present_block`] itself treats as the construct's own lines.
///
/// The read is clipped to `block_source_range` as a defensive bound: `content`
/// names whole `RopeBuffer` lines, which tile the document the same way
/// blocks do, so in the ordinary case the clip changes nothing.
pub fn parse_joined_span(
    document: &RopeBuffer,
    content: Range<usize>,
    block_source_range: SourceRange,
    revision: Revision,
) -> Option<JoinedParse> {
    if content.is_empty() {
        return None;
    }
    let start = document
        .line_range(hane_document::LineId(content.start))
        .ok()?
        .start;
    let end = document
        .line_range(hane_document::LineId(content.end - 1))
        .ok()?
        .end;
    let joined_range = SourceRange::new(
        start.0.max(block_source_range.start.0),
        end.0.min(block_source_range.end.0),
    );
    if joined_range.is_empty() {
        return None;
    }
    let joined_source = document.text(joined_range).ok()?;
    let parsed = parse_document(revision, joined_range, &joined_source);
    let projection = ProjectionIndex::new(&parsed);
    Some(JoinedParse {
        source_range: joined_range,
        joined_source,
        parsed,
        projection,
    })
}

/// Presents one physical source line with Phase 4 block polish. The block-level
/// [`LineContext`] is supplied by the caller from the document context index;
/// standalone image recognition is local to this source slice. Presentation owns
/// every display-kind and style-run decision here so the UI renders purely by
/// [`BlockKind`]/[`StyleKind`] without re-inspecting the source.
pub fn present_polished_line(
    line_id: u64,
    revision: Revision,
    range: SourceRange,
    source: &str,
    line_height: f32,
    disclosure: Option<SourceRange>,
    context: LineContext,
) -> VisualLine {
    present_polished_line_with_fence(
        line_id,
        revision,
        range,
        source,
        line_height,
        disclosure,
        context,
        None,
        None,
        false,
        &[],
    )
}

/// Which fence delimiter, if either, a physical line inside a fenced code
/// block is. `None` (handled by the caller before this type is ever reached)
/// covers both an indented code block and an ordinary content line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FenceLine {
    /// This physical line is the block's own first line and reads as a fence
    /// delimiter — the run of `` ` `` or `~` plus up to 3 leading spaces —
    /// independent of whether a valid closing fence exists anywhere below it.
    Opening,
    /// This physical line is the block's last content line, and it legally
    /// closes the opening fence (matching character, run at least as long;
    /// see [`fence_closes`]).
    Closing,
}

/// Classifies one physical line of a fenced code block against the block's
/// own opening delimiter. `fence_opening` is `(opening line number, opening
/// delimiter)`, resolved once per block by the caller from whichever of
/// [`BlockWindow::lines`] carries the true first line — not necessarily
/// `line`, and not necessarily inside the render window — so a line deep
/// inside a large block can still be validated as a genuine closing fence
/// without this function, or its caller, ever reading the lines between.
fn fence_line_role(
    line: usize,
    content_end: usize,
    source: &str,
    fence_opening: Option<(usize, FenceDelimiter)>,
) -> Option<FenceLine> {
    let (opening_line, opening_delimiter) = fence_opening?;
    if line == opening_line {
        return Some(FenceLine::Opening);
    }
    // Only the block's own last content line can be a closing fence: a
    // fence-shaped interior line (e.g. a nested code sample pasted into a
    // fenced block) is literal content, matching what the parser itself
    // already decided when it kept the block open past that line.
    if line + 1 == content_end
        && fence_closing_delimiter(source)
            .is_some_and(|candidate| fence_closes(opening_delimiter, candidate))
    {
        return Some(FenceLine::Closing);
    }
    None
}

#[allow(
    clippy::too_many_arguments,
    reason = "line presentation keeps the existing public dispatch inputs plus list context and fence role"
)]
fn present_polished_line_with_fence(
    line_id: u64,
    revision: Revision,
    range: SourceRange,
    source: &str,
    line_height: f32,
    disclosure: Option<SourceRange>,
    context: LineContext,
    list_projection: Option<&ListProjection>,
    fence_role: Option<FenceLine>,
    table_header: bool,
    table_alignments: &[TableAlignment],
) -> VisualLine {
    // A fenced-code line has no disclosable inline markup; its content is literal,
    // so it stays a code block regardless of cursor position and wins over image
    // and table recognition that would otherwise mis-read the literal text.
    let mut block = if context == LineContext::FencedCode {
        present_fenced_code_line(
            line_id,
            revision,
            range,
            source,
            line_height,
            disclosure,
            fence_role,
        )
    } else if let Some(image) = inactive_standalone_image(source, range, disclosure) {
        present_image(
            line_id,
            revision,
            range,
            source,
            line_height,
            image,
            list_projection.and_then(|projection| formal_quote_metadata(range, projection)),
        )
    } else if context == LineContext::Table
        && disclosure.is_none_or(|active| !disclosure_owns_physical_line(range, source, active))
    {
        present_table_line(
            line_id,
            revision,
            range,
            source,
            line_height,
            table_header,
            table_alignments,
        )
    } else {
        present_markdown_with_list_projection(
            line_id,
            revision,
            range,
            source,
            line_height,
            disclosure,
            list_projection,
        )
    };
    block.context = context;
    block.table_header = table_header;
    block
}

/// Presents one line that the context index reports is inside a fenced code
/// block. Code content is shown verbatim (no marker hiding, no inline Markdown
/// interpretation) and styled as code. An opening or closing fence delimiter
/// line (`fence_role`) hides its own delimiter bytes instead, unless `disclosure`
/// touches them, in which case the raw source is shown for direct editing.
fn present_fenced_code_line(
    line_id: u64,
    revision: Revision,
    range: SourceRange,
    source: &str,
    line_height: f32,
    disclosure: Option<SourceRange>,
    fence_role: Option<FenceLine>,
) -> VisualLine {
    match fence_role {
        Some(FenceLine::Opening) => present_fenced_code_opening_line(
            line_id,
            revision,
            range,
            source,
            line_height,
            disclosure,
        ),
        Some(FenceLine::Closing) => present_fenced_code_closing_line(
            line_id,
            revision,
            range,
            source,
            line_height,
            disclosure,
        ),
        None => present_fenced_code_content_line(line_id, revision, range, source, line_height),
    }
}

/// Presents a fenced code block's literal content line: the same raw,
/// unhidden presentation every fenced-code line used before fence hiding
/// existed, still used for indented code blocks (no fence to hide at all)
/// and for interior lines between the fences.
fn present_fenced_code_content_line(
    line_id: u64,
    revision: Revision,
    range: SourceRange,
    source: &str,
    line_height: f32,
) -> VisualLine {
    let mut block = present_plain(line_id, revision, range, source);
    block.kind = BlockKind::CodeBlock;
    block.estimated_height = estimated_height(BlockKind::CodeBlock, line_height);
    // Style the code content but not the trailing newline the visual text keeps
    // for source-map fidelity; callers trim the newline before painting.
    let content_end = source.trim_end_matches(['\r', '\n']).len();
    if content_end > 0 {
        block.style_runs = vec![StyleRun {
            visual_range: VisualRange::new(0, content_end),
            kind: StyleKind::CodeBlock,
        }];
    }
    block
}

/// Presents a fenced code block's opening delimiter line. Inactive rows hide
/// the delimiter, info string and trailing whitespace as separate source-map
/// segments, leaving the whole code-block background with no text in it.
/// Disclosing any part of the physical line (caret, selection or IME) shows
/// the complete raw source so the fence and info string can be edited directly.
fn present_fenced_code_opening_line(
    line_id: u64,
    revision: Revision,
    range: SourceRange,
    source: &str,
    line_height: f32,
    disclosure: Option<SourceRange>,
) -> VisualLine {
    let Some(delimiter) = fence_delimiter(source) else {
        return present_fenced_code_content_line(line_id, revision, range, source, line_height);
    };
    let content_end = source.trim_end_matches(['\r', '\n']).len();
    let indent = source.len() - source.trim_start_matches(' ').len();
    let delimiter_end = (indent + delimiter.len).min(content_end);
    let base = range.start.0;
    let delimiter_range = SourceRange::new(base, base + delimiter_end);
    let expanded =
        disclosure.is_some_and(|active| disclosure_owns_physical_line(range, source, active));
    let mut visual = String::new();
    let mut segments = Vec::new();
    append_segment(
        &mut visual,
        &mut segments,
        source,
        range,
        delimiter_range,
        if expanded {
            Visibility::ExpandedMarkup
        } else {
            Visibility::HiddenMarkup
        },
        Some(MarkerEdge::Opening),
    );
    if delimiter_end < source.len() {
        append_segment(
            &mut visual,
            &mut segments,
            source,
            range,
            SourceRange::new(base + delimiter_end, range.end.0),
            if expanded {
                Visibility::ExpandedMarkup
            } else {
                Visibility::HiddenMarkup
            },
            None,
        );
    }
    let styled_len = visual.trim_end_matches(['\r', '\n']).len();
    let style_runs = if styled_len > 0 {
        vec![StyleRun {
            visual_range: VisualRange::new(0, styled_len),
            kind: StyleKind::CodeBlock,
        }]
    } else {
        Vec::new()
    };
    let line_estimated_height = fenced_line_height(
        &visual,
        !expanded,
        disclosure.is_some_and(|active| range_touches(range, active)),
        line_height,
    );
    VisualLine {
        line_id,
        source_range: range,
        revision,
        visual_text: visual,
        style_runs,
        links: Vec::new(),
        kind: BlockKind::CodeBlock,
        source_map: SourceMap { segments },
        estimated_height: line_estimated_height,
        measured_height: None,
        invalid: false,
        context: LineContext::FencedCode,
        disclosure: None,
        image: None,
        list: None,
        quote: None,
        quote_marker_visual_ranges: Vec::new(),
        quote_marker_source_ranges: Vec::new(),
        table_row: None,
        table_header: false,
    }
}

/// Presents a fenced code block's closing delimiter line: hidden in full,
/// the same way an inactive table's delimiter row hides (see
/// [`present_table_line`]), since a valid closing fence line carries no
/// other content per CommonMark. Disclosing it shows the raw fence bytes.
fn present_fenced_code_closing_line(
    line_id: u64,
    revision: Revision,
    range: SourceRange,
    source: &str,
    line_height: f32,
    disclosure: Option<SourceRange>,
) -> VisualLine {
    let expanded = disclosure.is_some_and(|active| range_touches(range, active));
    let visibility = if expanded {
        Visibility::ExpandedMarkup
    } else {
        Visibility::HiddenMarkup
    };
    let visual_text = if expanded {
        source.to_owned()
    } else {
        String::new()
    };
    let styled_len = visual_text.trim_end_matches(['\r', '\n']).len();
    let style_runs = if styled_len > 0 {
        vec![StyleRun {
            visual_range: VisualRange::new(0, styled_len),
            kind: StyleKind::CodeBlock,
        }]
    } else {
        Vec::new()
    };
    let line_estimated_height = fenced_line_height(
        &visual_text,
        !expanded,
        disclosure.is_some_and(|active| range_touches(range, active)),
        line_height,
    );
    VisualLine {
        line_id,
        source_range: range,
        revision,
        visual_text,
        style_runs,
        links: Vec::new(),
        kind: BlockKind::CodeBlock,
        source_map: SourceMap {
            segments: vec![MappingSegment {
                source_range: range,
                visual_range: VisualRange::new(0, if expanded { source.len() } else { 0 }),
                visibility,
                marker_edge: Some(MarkerEdge::Closing),
            }],
        },
        estimated_height: line_estimated_height,
        measured_height: None,
        invalid: false,
        context: LineContext::FencedCode,
        disclosure: None,
        image: None,
        list: None,
        quote: None,
        quote_marker_visual_ranges: Vec::new(),
        quote_marker_source_ranges: Vec::new(),
        table_row: None,
        table_header: false,
    }
}

struct StandaloneImage<'a> {
    alt: &'a str,
    destination: &'a str,
    prefix_end: usize,
    alt_start: usize,
    alt_end: usize,
    suffix_start: usize,
    suffix_end: usize,
}

fn parse_standalone_image(source: &str) -> Option<StandaloneImage<'_>> {
    let content_end = source.trim_end_matches(['\r', '\n']).len();
    let content = &source[..content_end];
    let leading = content.len() - content.trim_start().len();
    let image = &content[leading..];
    let alt_end_relative = image.find("](")?;
    if !image.starts_with("![") || !image.ends_with(')') {
        return None;
    }
    let alt_start = leading + 2;
    let alt_end = leading + alt_end_relative;
    let destination_start = alt_end + 2;
    let suffix_end = content.len();
    (destination_start <= suffix_end.saturating_sub(1)).then_some(StandaloneImage {
        alt: &source[alt_start..alt_end],
        destination: &source[destination_start..suffix_end - 1],
        prefix_end: alt_start,
        alt_start,
        alt_end,
        suffix_start: alt_end,
        suffix_end,
    })
}

fn present_image(
    line_id: u64,
    revision: Revision,
    range: SourceRange,
    source: &str,
    line_height: f32,
    image: StandaloneImage<'_>,
    quote: Option<QuoteRowMetadata>,
) -> VisualLine {
    let mut segments = Vec::new();
    let base = range.start.0;
    if image.prefix_end > 2 {
        segments.push(MappingSegment {
            source_range: SourceRange::new(base, base + image.prefix_end - 2),
            visual_range: VisualRange::new(0, image.prefix_end - 2),
            visibility: Visibility::Visible,
            marker_edge: None,
        });
    }
    let visual_prefix = image.prefix_end.saturating_sub(2);
    segments.push(MappingSegment {
        source_range: SourceRange::new(base + visual_prefix, base + image.prefix_end),
        visual_range: VisualRange::new(visual_prefix, visual_prefix),
        visibility: Visibility::HiddenMarkup,
        marker_edge: Some(MarkerEdge::Opening),
    });
    segments.push(MappingSegment {
        source_range: SourceRange::new(base + image.alt_start, base + image.alt_end),
        visual_range: VisualRange::new(visual_prefix, visual_prefix + image.alt.len()),
        visibility: Visibility::Visible,
        marker_edge: None,
    });
    let visual_end = visual_prefix + image.alt.len();
    segments.push(MappingSegment {
        source_range: SourceRange::new(base + image.suffix_start, base + image.suffix_end),
        visual_range: VisualRange::new(visual_end, visual_end),
        visibility: Visibility::HiddenMarkup,
        marker_edge: Some(MarkerEdge::Closing),
    });
    if image.suffix_end < source.len() {
        segments.push(MappingSegment {
            source_range: SourceRange::new(base + image.suffix_end, range.end.0),
            visual_range: VisualRange::new(
                visual_end,
                visual_end + source.len() - image.suffix_end,
            ),
            visibility: Visibility::Visible,
            marker_edge: None,
        });
    }
    VisualLine {
        line_id,
        source_range: range,
        revision,
        visual_text: format!("{}{}", &source[..visual_prefix], image.alt),
        style_runs: vec![StyleRun {
            visual_range: VisualRange::new(visual_prefix, visual_end),
            kind: StyleKind::Image,
        }],
        links: Vec::new(),
        kind: BlockKind::Image,
        source_map: SourceMap { segments },
        estimated_height: estimated_height(BlockKind::Image, line_height),
        measured_height: None,
        invalid: false,
        context: LineContext::Normal,
        disclosure: None,
        image: Some(ImagePresentation {
            alt: image.alt.to_owned(),
            destination: image.destination.to_owned(),
        }),
        list: None,
        quote,
        quote_marker_visual_ranges: Vec::new(),
        quote_marker_source_ranges: Vec::new(),
        table_row: None,
        table_header: false,
    }
}

fn table_alignments(source: &str) -> Vec<TableAlignment> {
    let content_end = source.trim_end_matches(['\r', '\n']).len();
    let trimmed = source[..content_end].trim();
    let trimmed = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let trimmed = trimmed.strip_suffix('|').unwrap_or(trimmed);
    trimmed
        .split('|')
        .map(|cell| {
            let cell = cell.trim();
            match (cell.starts_with(':'), cell.ends_with(':')) {
                (true, true) => TableAlignment::Center,
                (true, false) => TableAlignment::Left,
                (false, true) => TableAlignment::Right,
                (false, false) => TableAlignment::Default,
            }
        })
        .collect()
}

pub(crate) fn table_row_from_projection(
    line: &VisualLine,
    projection: Option<&TableProjection>,
    range: SourceRange,
    header: bool,
    alignments: &[TableAlignment],
) -> Option<TableRowDisplay> {
    let projected = projection.and_then(|projection| {
        projection.rows.iter().find(|row| {
            row.source_range == range
                || (row.source_range.start < range.end && range.start < row.source_range.end)
        })
    });
    let projected_header = projected.map(|row| row.header).unwrap_or(header);
    let cells = if let Some(projected) = projected {
        let formal_cells = projected
            .cells
            .iter()
            .map(|cell| {
                let start = line
                    .source_map
                    .source_to_visual(cell.source_range.start, Bias::After)?
                    .visual_offset
                    .0;
                let end = line
                    .source_map
                    .source_to_visual(cell.source_range.end, Bias::Before)?
                    .visual_offset
                    .0;
                let start = start.min(line.visual_text.len());
                let end = end.min(line.visual_text.len()).max(start);
                Some(TableCellDisplay {
                    column: cell.column,
                    source_range: cell.source_range,
                    visual_range: VisualRange::new(start, end),
                    alignment: alignments
                        .get(cell.column)
                        .copied()
                        .unwrap_or(TableAlignment::Default),
                })
            })
            .collect::<Option<Vec<_>>>();
        match formal_cells {
            Some(cells) if cells.len() == projected.cells.len() => cells,
            _ => table_cells_from_visual_mapping(line, alignments)?,
        }
    } else {
        table_cells_from_visual_mapping(line, alignments)?
    };
    let column_count = if alignments.is_empty() {
        cells
            .iter()
            .map(|cell| cell.column.saturating_add(1))
            .max()
            .unwrap_or(0)
    } else {
        alignments.len()
    };
    Some(TableRowDisplay {
        header: projected_header,
        column_count,
        cells,
    })
}

/// Reconstructs table cells from the line's existing source↔visual map when a
/// provisional/local presentation has no formal row projection. Pipes are
/// already visible in a disclosed raw row; unlike reparsing `visual_text`,
/// mapping their positions back to source keeps hidden inline markers inside
/// the real source cell range.
fn table_cells_from_visual_mapping(
    line: &VisualLine,
    alignments: &[TableAlignment],
) -> Option<Vec<TableCellDisplay>> {
    if line.context != LineContext::Table
        || line.kind == BlockKind::TableDelimiter
        || is_table_delimiter(&line.visual_text)
    {
        return None;
    }
    let content_end = line.visual_text.trim_end_matches(['\r', '\n']).len();
    let pipes = unescaped_table_pipes(&line.visual_text, content_end);
    let leading_indent = pipes
        .first()
        .copied()
        .filter(|index| *index <= 3 && line.visual_text[..*index].bytes().all(|byte| byte == b' '))
        .unwrap_or(0);
    let opening_pipe = pipes.first().copied().unwrap_or(leading_indent);
    let source_for_pipe = |visual: usize| {
        line.source_map
            .visual_to_source(VisualOffset(visual), Bias::Before)
            .map(|candidate| candidate.source_offset)
    };
    let mut cells = Vec::new();
    let mut cursor = leading_indent;
    let mut column = 0;
    let mut previous_pipe: Option<SourceOffset> = None;
    for pipe in pipes {
        let pipe_source = source_for_pipe(pipe)?;
        if cursor < pipe {
            let source_start = previous_pipe.map_or(
                SourceOffset(line.source_range.start.0 + leading_indent),
                |source| SourceOffset(source.0.saturating_add(1)),
            );
            if alignments.is_empty() || column < alignments.len() {
                cells.push(TableCellDisplay {
                    column,
                    source_range: SourceRange::new(source_start.0, pipe_source.0),
                    visual_range: VisualRange::new(cursor, pipe),
                    alignment: alignments
                        .get(column)
                        .copied()
                        .unwrap_or(TableAlignment::Default),
                });
            }
            column += 1;
        } else if pipe != opening_pipe {
            if alignments.is_empty() || column < alignments.len() {
                let source = SourceOffset(
                    previous_pipe.map_or(line.source_range.start.0 + leading_indent, |source| {
                        source.0.saturating_add(1)
                    }),
                );
                cells.push(TableCellDisplay {
                    column,
                    source_range: SourceRange::empty(source.0),
                    visual_range: VisualRange::new(cursor, cursor),
                    alignment: alignments
                        .get(column)
                        .copied()
                        .unwrap_or(TableAlignment::Default),
                });
            }
            column += 1;
        }
        cursor = pipe + 1;
        previous_pipe = Some(pipe_source);
    }
    if cursor < content_end {
        let source_start = previous_pipe.map_or(
            SourceOffset(line.source_range.start.0 + leading_indent),
            |source| SourceOffset(source.0.saturating_add(1)),
        );
        let source_end = line
            .source_map
            .segments
            .iter()
            .filter(|segment| {
                !segment.source_range.is_empty()
                    && segment.visual_range.start.0 == content_end
                    && matches!(
                        segment.visibility,
                        Visibility::Visible | Visibility::ExpandedMarkup
                    )
            })
            .map(|segment| segment.source_range.start)
            .next()
            .unwrap_or(line.source_range.end);
        if alignments.is_empty() || column < alignments.len() {
            cells.push(TableCellDisplay {
                column,
                source_range: SourceRange::new(source_start.0, source_end.0),
                visual_range: VisualRange::new(cursor, content_end),
                alignment: alignments
                    .get(column)
                    .copied()
                    .unwrap_or(TableAlignment::Default),
            });
        }
    }
    let column_count = if alignments.is_empty() {
        column + usize::from(cursor < content_end)
    } else {
        alignments.len()
    };
    while cells.len() < column_count {
        let column = cells.len();
        cells.push(TableCellDisplay {
            column,
            source_range: SourceRange::empty(line.source_range.end.0),
            visual_range: VisualRange::new(content_end, content_end),
            alignment: alignments
                .get(column)
                .copied()
                .unwrap_or(TableAlignment::Default),
        });
    }
    Some(cells)
}

fn unescaped_table_pipes(source: &str, content_end: usize) -> Vec<usize> {
    let bytes = source.as_bytes();
    let mut pipes = Vec::new();
    for index in 0..content_end {
        if bytes[index] != b'|' {
            continue;
        }
        let mut backslashes = 0;
        let mut cursor = index;
        while cursor > 0 && bytes[cursor - 1] == b'\\' {
            backslashes += 1;
            cursor -= 1;
        }
        if backslashes % 2 == 0 {
            pipes.push(index);
        }
    }
    pipes
}

pub(crate) fn present_table_line(
    line_id: u64,
    revision: Revision,
    range: SourceRange,
    source: &str,
    line_height: f32,
    header: bool,
    alignments: &[TableAlignment],
) -> VisualLine {
    if is_table_delimiter(source) {
        return VisualLine {
            line_id,
            source_range: range,
            revision,
            visual_text: String::new(),
            style_runs: Vec::new(),
            links: Vec::new(),
            kind: BlockKind::TableDelimiter,
            source_map: SourceMap {
                segments: vec![MappingSegment {
                    source_range: range,
                    visual_range: VisualRange::new(0, 0),
                    visibility: Visibility::HiddenMarkup,
                    marker_edge: None,
                }],
            },
            estimated_height: estimated_height(BlockKind::TableDelimiter, line_height),
            measured_height: None,
            invalid: false,
            context: LineContext::Normal,
            disclosure: None,
            image: None,
            list: None,
            quote: None,
            quote_marker_visual_ranges: Vec::new(),
            quote_marker_source_ranges: Vec::new(),
            table_row: None,
            table_header: false,
        };
    }
    let content_end = source.trim_end_matches(['\r', '\n']).len();
    let pipes = unescaped_table_pipes(source, content_end);
    let leading_indent = pipes
        .first()
        .copied()
        .filter(|index| *index <= 3 && source[..*index].bytes().all(|byte| byte == b' '))
        .unwrap_or(0);
    let opening_pipe = pipes.first().copied().unwrap_or(leading_indent);
    let mut visual = String::new();
    let mut segments = Vec::new();
    let mut cells = Vec::new();
    let base = range.start.0;
    if leading_indent > 0 {
        segments.push(MappingSegment {
            source_range: SourceRange::new(base, base + leading_indent),
            visual_range: VisualRange::new(0, 0),
            visibility: Visibility::HiddenMarkup,
            marker_edge: None,
        });
    }
    let mut cursor = leading_indent;
    let mut column = 0;
    for index in pipes {
        if cursor < index {
            let visual_start = visual.len();
            append_segment(
                &mut visual,
                &mut segments,
                source,
                range,
                SourceRange::new(base + cursor, base + index),
                Visibility::Visible,
                None,
            );
            if alignments.is_empty() || column < alignments.len() {
                cells.push(TableCellDisplay {
                    column,
                    source_range: SourceRange::new(base + cursor, base + index),
                    visual_range: VisualRange::new(visual_start, visual.len()),
                    alignment: alignments
                        .get(column)
                        .copied()
                        .unwrap_or(TableAlignment::Default),
                });
            }
            column += 1;
        } else if index != opening_pipe {
            if alignments.is_empty() || column < alignments.len() {
                cells.push(TableCellDisplay {
                    column,
                    source_range: SourceRange::empty(base + cursor),
                    visual_range: VisualRange::new(visual.len(), visual.len()),
                    alignment: alignments
                        .get(column)
                        .copied()
                        .unwrap_or(TableAlignment::Default),
                });
            }
            column += 1;
        }
        let at = visual.len();
        segments.push(MappingSegment {
            source_range: SourceRange::new(base + index, base + index + 1),
            visual_range: VisualRange::new(at, at),
            visibility: Visibility::HiddenMarkup,
            marker_edge: None,
        });
        cursor = index + 1;
    }
    if cursor < content_end {
        let visual_start = visual.len();
        append_segment(
            &mut visual,
            &mut segments,
            source,
            range,
            SourceRange::new(base + cursor, base + content_end),
            Visibility::Visible,
            None,
        );
        if alignments.is_empty() || column < alignments.len() {
            cells.push(TableCellDisplay {
                column,
                source_range: SourceRange::new(base + cursor, base + content_end),
                visual_range: VisualRange::new(visual_start, visual.len()),
                alignment: alignments
                    .get(column)
                    .copied()
                    .unwrap_or(TableAlignment::Default),
            });
        }
        column += 1;
    }
    let table_content_visual_end = visual.len();
    if content_end < source.len() {
        append_segment(
            &mut visual,
            &mut segments,
            source,
            range,
            SourceRange::new(base + content_end, range.end.0),
            Visibility::Visible,
            None,
        );
    }
    let column_count = if alignments.is_empty() {
        column
    } else {
        alignments.len()
    };
    while cells.len() < column_count {
        let column = cells.len();
        cells.push(TableCellDisplay {
            column,
            source_range: SourceRange::empty(base + content_end),
            visual_range: VisualRange::new(table_content_visual_end, table_content_visual_end),
            alignment: alignments
                .get(column)
                .copied()
                .unwrap_or(TableAlignment::Default),
        });
    }
    let visual_len = visual.len();
    VisualLine {
        line_id,
        source_range: range,
        revision,
        visual_text: visual,
        style_runs: vec![StyleRun {
            visual_range: VisualRange::new(0, visual_len),
            kind: StyleKind::Table,
        }],
        links: Vec::new(),
        kind: BlockKind::TableRow,
        source_map: SourceMap { segments },
        estimated_height: line_height * 1.2,
        measured_height: None,
        invalid: false,
        context: LineContext::Normal,
        disclosure: None,
        image: None,
        list: None,
        quote: None,
        quote_marker_visual_ranges: Vec::new(),
        quote_marker_source_ranges: Vec::new(),
        table_row: Some(TableRowDisplay {
            header,
            column_count,
            cells,
        }),
        table_header: header,
    }
}

pub fn present_markdown(
    line_id: u64,
    revision: Revision,
    range: SourceRange,
    source: &str,
    line_height: f32,
) -> VisualLine {
    present_markdown_with_disclosure(line_id, revision, range, source, line_height, None)
}

#[cfg(test)]
mod tests;
