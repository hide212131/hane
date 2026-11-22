//! Single-line HTML block elements (`p`, `h1`–`h6`, `hr`; Issue #757, phase 2).
//!
//! The block index publishes [`NodeKind::HtmlBlockElement`] for a top-level HTML
//! block that is one complete element on one physical line. This module turns
//! that line into the equivalent Markdown row. The content is never re-parsed
//! as Markdown, only the phase-1 tags inside it are applied. While the line is
//! disclosed (caret, selection, composition or find match) nothing is produced
//! and the caller shows the line as ordinary source.

use super::*;
use hane_markdown::{HtmlBlockKind, recognize_html_block_line};

/// The display kind a recognized block element shows as while it is collapsed.
fn collapsed_kind(kind: HtmlBlockKind) -> BlockKind {
    match kind {
        HtmlBlockKind::Paragraph => BlockKind::Paragraph,
        HtmlBlockKind::Heading(level) => BlockKind::Heading(level),
        HtmlBlockKind::Rule => BlockKind::Rule,
    }
}

/// Whether `disclosure` opens the one source line `range` (`source`).
pub(super) fn html_line_is_disclosed(
    range: SourceRange,
    source: &str,
    disclosure: Option<SourceRange>,
) -> bool {
    disclosure.is_some_and(|active| disclosure_owns_physical_line(range, source, active))
}

/// Presents one physical line of a single-line HTML block element, or `None`
/// when it is disclosed or is not such an element after all.
pub(super) fn present_html_block_line(
    line_id: u64,
    revision: Revision,
    range: SourceRange,
    source: &str,
    line_height: f32,
    disclosure: Option<SourceRange>,
) -> Option<VisualLine> {
    if html_line_is_disclosed(range, source, disclosure) {
        return None;
    }
    let recognized = recognize_html_block_line(source)?;
    let absolute = |relative: &Range<usize>| {
        SourceRange::new(range.start.0 + relative.start, range.start.0 + relative.end)
    };
    let kind = collapsed_kind(recognized.kind);
    let mut visual = String::new();
    let mut segments = Vec::new();
    let mut style_runs = Vec::new();
    let mut links = Vec::new();
    if recognized.kind == HtmlBlockKind::Rule {
        // Like a Markdown rule: an empty source-mapped row the UI paints as a
        // separator, with every source byte hidden.
        segments.push(MappingSegment {
            source_range: range,
            visual_range: VisualRange::new(0, 0),
            visibility: Visibility::HiddenMarkup,
            marker_edge: None,
        });
    } else {
        let mut hidden = vec![(absolute(&recognized.open), MarkerEdge::Opening)];
        for element in &recognized.inline {
            hidden.push((absolute(&element.open_tag), MarkerEdge::Opening));
            hidden.push((absolute(&element.close_tag), MarkerEdge::Closing));
        }
        hidden.push((absolute(&recognized.close), MarkerEdge::Closing));
        hidden.sort_by_key(|(marker, _)| (marker.start, marker.end));
        let mut cursor = range.start.0;
        for (marker, edge) in &hidden {
            if marker.start.0 < cursor || marker.end.0 > range.end.0 {
                return None;
            }
            if cursor < marker.start.0 {
                append_segment(
                    &mut visual,
                    &mut segments,
                    source,
                    range,
                    SourceRange::new(cursor, marker.start.0),
                    Visibility::Visible,
                    None,
                );
            }
            if marker.start.0 < marker.end.0 {
                append_segment(
                    &mut visual,
                    &mut segments,
                    source,
                    range,
                    *marker,
                    Visibility::HiddenMarkup,
                    Some(*edge),
                );
            }
            cursor = marker.end.0;
        }
        if cursor < range.end.0 {
            append_segment(
                &mut visual,
                &mut segments,
                source,
                range,
                SourceRange::new(cursor, range.end.0),
                Visibility::Visible,
                None,
            );
        }
        if !segments_tile_range(range, &segments) {
            return None;
        }
    }
    let source_map = SourceMap { segments };
    for element in &recognized.inline {
        let element_range = absolute(&element.range);
        let (Some(start), Some(end)) = (
            source_map.source_to_visual(element_range.start, Bias::After),
            source_map.source_to_visual(element_range.end, Bias::Before),
        ) else {
            continue;
        };
        if start.visual_offset.0 >= end.visual_offset.0 {
            continue;
        }
        let style = match &element.destination {
            Some(destination) => {
                let text_range = absolute(&element.content_range);
                links.push(LinkTarget {
                    source_range: text_range,
                    link_text_range: text_range,
                    destination: destination.clone(),
                    uses_reference: false,
                });
                Some(StyleKind::Link)
            }
            None => html_style(element.kind),
        };
        if let Some(style) = style {
            style_runs.push(StyleRun {
                visual_range: VisualRange {
                    start: start.visual_offset,
                    end: end.visual_offset,
                },
                kind: style,
            });
        }
    }
    style_runs.sort_by_key(|run| (run.visual_range.start.0, run.visual_range.end.0));
    Some(VisualLine {
        line_id,
        source_range: range,
        revision,
        visual_text: visual,
        style_runs,
        links,
        kind,
        source_map,
        estimated_height: estimated_height(kind, line_height),
        measured_height: None,
        invalid: false,
        context: LineContext::Normal,
        disclosure,
        image: None,
        list: None,
        quote: None,
        quote_marker_visual_ranges: Vec::new(),
        quote_marker_source_ranges: Vec::new(),
        table_row: None,
        table_header: false,
    })
}

/// The display kind of an indexed block for height and block-level purposes:
/// a single-line HTML block element is raw source while `disclosure` opens its
/// content line, and its Markdown equivalent otherwise.
pub(super) fn block_kind_with_disclosure(
    document: &RopeBuffer,
    block: &IndexedBlock,
    disclosure: Option<SourceRange>,
) -> BlockKind {
    let kind = crate::display::block_display_kind(block.kind);
    if !matches!(block.kind, NodeKind::HtmlBlockElement(_)) || disclosure.is_none() {
        return kind;
    }
    let Some(span) = block_line_span(document, block) else {
        return kind;
    };
    let line = hane_document::LineId(span.start + block.leading_content_lines);
    let (Ok(range), true) = (document.line_range(line), span.contains(&line.0)) else {
        return kind;
    };
    let Ok(text) = document.text(range) else {
        return kind;
    };
    if html_line_is_disclosed(range, &text, disclosure) {
        BlockKind::Unsupported
    } else {
        kind
    }
}
