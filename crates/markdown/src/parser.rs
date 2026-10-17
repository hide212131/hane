use crate::markers::{
    ParsedCodeSpan, absolute_range, code_padding, derive_markers, line_break_padding,
    list_structural_prefixes,
};
use crate::syntax::{
    LinkParse, MarkdownNode, MarkdownParse, MarkdownTree, NodeId, NodeKind, TableAlignment,
    TableParse,
};
use hane_document::{Revision, SourceRange};
use pulldown_cmark::{Event, HeadingLevel, LinkType, Options, Parser, Tag};
use std::sync::Arc;

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// The single parser configuration. Every parse in Hane goes through this so
/// enabling a GFM extension is a one-line change with no second code path.
fn parser_options() -> Options {
    Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS | Options::ENABLE_TABLES
}

fn node_kind_for_tag(tag: &Tag) -> NodeKind {
    match tag {
        Tag::Paragraph => NodeKind::Paragraph,
        Tag::Heading { level, .. } => NodeKind::Heading(heading_level(*level)),
        Tag::CodeBlock(_) => NodeKind::CodeBlock,
        Tag::BlockQuote(_) => NodeKind::Quote,
        Tag::List(start) => NodeKind::List { start: *start },
        Tag::Item => NodeKind::ListItem { task: None },
        Tag::Table(_) => NodeKind::Table,
        Tag::TableHead => NodeKind::TableHead,
        Tag::TableRow => NodeKind::TableRow,
        Tag::TableCell => NodeKind::TableCell,
        Tag::HtmlBlock => NodeKind::HtmlBlock,
        Tag::FootnoteDefinition(_) => NodeKind::FootnoteDefinition,
        Tag::Strong => NodeKind::Strong,
        Tag::Emphasis => NodeKind::Emphasis,
        Tag::Strikethrough => NodeKind::Strikethrough,
        Tag::Link { .. } => NodeKind::Link,
        Tag::Image { .. } => NodeKind::Image,
        _ => NodeKind::Unsupported,
    }
}

/// Builds the node tree from the offset event stream. Container `Start`/`End`
/// pairs push and pop; every other event becomes a leaf under the open
/// container. Unmodeled tags still push a node so the stack stays balanced and
/// their source range remains reachable.
fn build_tree(
    source_range: SourceRange,
    source: &str,
) -> (
    MarkdownTree,
    Vec<ParsedCodeSpan>,
    Vec<TableParse>,
    Vec<LinkParse>,
) {
    let mut codes = Vec::new();
    let mut tables = Vec::new();
    let mut pending_links: Vec<(NodeId, Arc<str>, bool)> = Vec::new();
    let mut nodes = vec![MarkdownNode {
        kind: NodeKind::Document,
        source_range,
        parent: None,
        children: Vec::new(),
        depth: 0,
    }];
    let mut open = vec![MarkdownTree::ROOT];
    fn push(
        nodes: &mut Vec<MarkdownNode>,
        open: &[NodeId],
        kind: NodeKind,
        range: SourceRange,
    ) -> NodeId {
        let parent = open.last().copied().unwrap_or(MarkdownTree::ROOT);
        let id = NodeId(nodes.len());
        let depth = nodes[parent.0].depth + 1;
        nodes.push(MarkdownNode {
            kind,
            source_range: range,
            parent: Some(parent),
            children: Vec::new(),
            depth,
        });
        nodes[parent.0].children.push(id);
        id
    }
    for (event, relative_range) in Parser::new_ext(source, parser_options()).into_offset_iter() {
        let range = absolute_range(source_range.start.0, relative_range);
        match event {
            Event::Start(tag) => {
                let id = push(&mut nodes, &open, node_kind_for_tag(&tag), range);
                if let Tag::Link {
                    link_type,
                    dest_url,
                    ..
                } = &tag
                    && matches!(
                        link_type,
                        LinkType::Inline
                            | LinkType::Reference
                            | LinkType::Collapsed
                            | LinkType::Shortcut
                    )
                    && is_external_http_url(dest_url)
                {
                    pending_links.push((
                        id,
                        Arc::from(&**dest_url),
                        !matches!(link_type, LinkType::Inline),
                    ));
                }
                if let Tag::Table(alignments) = &tag {
                    tables.push(TableParse {
                        node: id,
                        alignments: alignments
                            .iter()
                            .map(|alignment| match alignment {
                                pulldown_cmark::Alignment::None => TableAlignment::Default,
                                pulldown_cmark::Alignment::Left => TableAlignment::Left,
                                pulldown_cmark::Alignment::Center => TableAlignment::Center,
                                pulldown_cmark::Alignment::Right => TableAlignment::Right,
                            })
                            .collect::<Vec<_>>()
                            .into(),
                    });
                }
                open.push(id);
            }
            Event::End(_) => {
                open.pop();
            }
            Event::TaskListMarker(checked) => {
                // pulldown reports the checkbox as a child event, so the item kind
                // is only complete once the marker arrives.
                if let Some(item) = open.last()
                    && let NodeKind::ListItem { task } = &mut nodes[item.0].kind
                {
                    *task = Some(checked);
                }
                push(&mut nodes, &open, NodeKind::TaskMarker(checked), range);
            }
            Event::Text(_) => {
                push(&mut nodes, &open, NodeKind::Text, range);
            }
            Event::Code(content) => {
                let node = push(&mut nodes, &open, NodeKind::InlineCode, range);
                codes.push(ParsedCodeSpan {
                    node,
                    content: content.into_string(),
                });
            }
            Event::Html(_) => {
                push(&mut nodes, &open, NodeKind::Html, range);
            }
            Event::InlineHtml(_) => {
                push(&mut nodes, &open, NodeKind::InlineHtml, range);
            }
            Event::FootnoteReference(_) => {
                push(&mut nodes, &open, NodeKind::FootnoteReference, range);
            }
            Event::SoftBreak => {
                push(&mut nodes, &open, NodeKind::SoftBreak, range);
            }
            Event::HardBreak => {
                push(&mut nodes, &open, NodeKind::HardBreak, range);
            }
            Event::Rule => {
                push(&mut nodes, &open, NodeKind::Rule, range);
            }
            _ => {
                push(&mut nodes, &open, NodeKind::Unsupported, range);
            }
        }
    }
    let links = pending_links
        .into_iter()
        .filter_map(|(node, destination, uses_reference)| {
            let children = &nodes[node.0].children;
            let start = nodes[children.first()?.0].source_range.start;
            let end = nodes[children.last()?.0].source_range.end;
            (start < end).then_some(LinkParse {
                node,
                text_range: SourceRange { start, end },
                destination,
                uses_reference,
            })
        })
        .collect();
    (MarkdownTree { nodes }, codes, tables, links)
}

/// True for an absolute `http://` or `https://` URL with a host and no
/// whitespace or control characters. Everything else (relative paths, `file:`,
/// other schemes) is not opened.
fn is_external_http_url(url: &str) -> bool {
    let Some(scheme_len) = ["https://", "http://"]
        .iter()
        .find(|scheme| {
            url.get(..scheme.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(scheme))
        })
        .map(|scheme| scheme.len())
    else {
        return false;
    };
    if url.chars().any(|ch| ch.is_whitespace() || ch.is_control()) {
        return false;
    }
    let authority = url[scheme_len..]
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    !authority.is_empty() && !authority.starts_with('@') && !authority.starts_with(':')
}

/// Parses a source slice into a node tree and retains the byte range of every
/// node. The returned offsets are absolute within the document, even for a local
/// slice.
pub fn parse_document(
    revision: Revision,
    source_range: SourceRange,
    source: &str,
) -> MarkdownParse {
    debug_assert_eq!(source_range.end.0 - source_range.start.0, source.len());
    let (tree, codes, table_parses, links) = build_tree(source_range, source);
    let markers = derive_markers(&tree, source_range, source);
    let code_padding = code_padding(&tree, &codes, source_range, source);
    let line_break_padding = line_break_padding(&tree, source_range, source);
    let list_structural_prefixes = list_structural_prefixes(&tree, source_range, source);
    MarkdownParse {
        revision,
        source_range,
        tree,
        table_parses,
        links,
        markers: markers.markers,
        fence_markers: markers.fence_markers,
        fence_marker_edges: markers.fence_marker_edges,
        quote_markers: markers.quote_markers,
        list_item_markers: markers.list_item_markers,
        list_structural_prefixes,
        code_padding,
        line_break_padding,
    }
}
