//! CommonMark parsing with source-byte ranges.

use hane_document::{LineId, Revision, RopeBuffer, SourceRange, TextBuffer};
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag};

/// Markdown *syntax* kind, as written in the source.
///
/// This type is the parser's vocabulary only. It says nothing about how a
/// construct is displayed: `hane_presentation` owns the display kind, and the UI
/// crate never sees a `NodeKind` at all. Constructs Hane does not model yet map
/// to [`NodeKind::Unsupported`] instead of being dropped, so the tree always
/// covers the whole event stream and no source range goes unaccounted for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeKind {
    /// Synthetic root spanning the whole parsed source range.
    Document,
    Paragraph,
    Heading(u8),
    CodeBlock,
    Quote,
    List {
        ordered: bool,
    },
    /// `task` is `Some(checked)` for a GFM task-list item and `None` otherwise.
    ListItem {
        task: Option<bool>,
    },
    Table,
    TableHead,
    TableRow,
    TableCell,
    Rule,
    HtmlBlock,
    Html,
    FootnoteDefinition,
    Text,
    Strong,
    Emphasis,
    Strikethrough,
    InlineCode,
    Link,
    Image,
    InlineHtml,
    FootnoteReference,
    TaskMarker(bool),
    Break,
    /// A construct with no modeled kind. Retains its source range so callers can
    /// still account for the bytes.
    Unsupported,
}

impl NodeKind {
    pub const fn is_block(self) -> bool {
        matches!(
            self,
            Self::Document
                | Self::Paragraph
                | Self::Heading(_)
                | Self::CodeBlock
                | Self::Quote
                | Self::List { .. }
                | Self::ListItem { .. }
                | Self::Table
                | Self::TableHead
                | Self::TableRow
                | Self::TableCell
                | Self::Rule
                | Self::HtmlBlock
                | Self::Html
                | Self::FootnoteDefinition
        )
    }

    pub const fn is_inline(self) -> bool {
        matches!(
            self,
            Self::Text
                | Self::Strong
                | Self::Emphasis
                | Self::Strikethrough
                | Self::InlineCode
                | Self::Link
                | Self::Image
                | Self::InlineHtml
                | Self::FootnoteReference
                | Self::TaskMarker(_)
                | Self::Break
        )
    }
}

/// Identifies a node inside a [`MarkdownTree`]. Ids are storage indices assigned
/// in document order, so a smaller id never starts after a larger one, and
/// [`MarkdownTree::ROOT`] is always `NodeId(0)`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NodeId(pub usize);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownNode {
    pub kind: NodeKind,
    pub source_range: SourceRange,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    /// Distance from the document root, which sits at depth 0.
    pub depth: usize,
}

/// Block/inline node tree for one parsed source slice: parent/child structure,
/// document order, and a source range on every node.
///
/// This replaces the previous flat block and span lists. Nested constructs
/// (list → item → paragraph, quote → paragraph, table → row → cell) are
/// expressible without a new side table per feature, which is what keeps a new
/// Markdown construct from growing parallel vectors here and matching branches
/// downstream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownTree {
    nodes: Vec<MarkdownNode>,
}

impl MarkdownTree {
    pub const ROOT: NodeId = NodeId(0);

    pub fn root(&self) -> &MarkdownNode {
        &self.nodes[Self::ROOT.0]
    }

    pub fn node(&self, id: NodeId) -> Option<&MarkdownNode> {
        self.nodes.get(id.0)
    }

    pub fn children(&self, id: NodeId) -> &[NodeId] {
        self.node(id).map_or(&[], |node| node.children.as_slice())
    }

    /// Node count including the synthetic root.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// True when the slice parsed to nothing but the synthetic root.
    pub fn is_empty(&self) -> bool {
        self.nodes.len() <= 1
    }

    /// Every node except the synthetic root, in document order.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = (NodeId, &MarkdownNode)> {
        self.nodes
            .iter()
            .enumerate()
            .skip(1)
            .map(|(index, node)| (NodeId(index), node))
    }

    pub fn blocks(&self) -> impl Iterator<Item = (NodeId, &MarkdownNode)> {
        self.iter().filter(|(_, node)| node.kind.is_block())
    }

    pub fn inlines(&self) -> impl Iterator<Item = (NodeId, &MarkdownNode)> {
        self.iter().filter(|(_, node)| node.kind.is_inline())
    }

    /// `id` followed by each ancestor up to and including the root.
    pub fn ancestors(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        std::iter::successors(Some(id), move |current| {
            self.node(*current).and_then(|node| node.parent)
        })
    }

    /// How many enclosing lists a node sits in. `1` for a top-level list item,
    /// `2` for an item of a list nested inside another item, and so on.
    pub fn list_depth(&self, id: NodeId) -> usize {
        self.ancestors(id)
            .filter(|ancestor| {
                self.node(*ancestor)
                    .is_some_and(|node| matches!(node.kind, NodeKind::List { .. }))
            })
            .count()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownParse {
    pub revision: Revision,
    pub source_range: SourceRange,
    pub tree: MarkdownTree,
    /// Sorted, non-overlapping source ranges of the syntactic markers (heading
    /// hashes, quote/list prefixes, fence delimiters, emphasis/code delimiters,
    /// link brackets). Derived here so presentation and UI never re-lex markup.
    pub markers: Vec<SourceRange>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FenceDelimiter {
    pub marker: u8,
    pub len: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockContextIndex {
    pub revision: Revision,
    fenced_lines: Vec<bool>,
    table_lines: Vec<bool>,
}

impl BlockContextIndex {
    pub fn line_is_fenced(&self, line: usize) -> Option<bool> {
        self.fenced_lines.get(line).copied()
    }

    pub fn line_count(&self) -> usize {
        self.fenced_lines.len()
    }

    pub fn line_is_table(&self, line: usize) -> Option<bool> {
        self.table_lines.get(line).copied()
    }
}

pub fn is_pipe_row(source: &str) -> bool {
    let content = source.trim_end_matches(['\r', '\n']);
    content.matches('|').count() >= 2
        && (content.trim_start().starts_with('|') || content.trim_end().ends_with('|'))
}

pub fn is_table_delimiter(source: &str) -> bool {
    let content = source.trim_end_matches(['\r', '\n']).trim();
    let cells = content.trim_matches('|').split('|').collect::<Vec<_>>();
    cells.len() >= 2
        && cells.iter().all(|cell| {
            let trimmed = cell.trim().trim_matches(':');
            trimmed.len() >= 3 && trimmed.bytes().all(|byte| byte == b'-')
        })
}

pub fn fence_delimiter(source: &str) -> Option<FenceDelimiter> {
    let trimmed = source.trim_start_matches(' ');
    if source.len() - trimmed.len() > 3 {
        return None;
    }
    let marker = *trimmed.as_bytes().first()?;
    if !matches!(marker, b'`' | b'~') {
        return None;
    }
    let len = trimmed
        .as_bytes()
        .iter()
        .take_while(|byte| **byte == marker)
        .count();
    (len >= 3).then_some(FenceDelimiter { marker, len })
}

/// Fenced-code and table classification for a contiguous window of source lines.
/// The window is assumed to start outside any open fence; callers that need this
/// to hold exactly (the whole-document background job) start at line 0, while the
/// bounded fallback accepts the approximation inherent to a lookback window.
fn scan_block_context(lines: &[String]) -> (Vec<bool>, Vec<bool>) {
    let mut fenced_lines = Vec::with_capacity(lines.len());
    let mut pipe_rows = Vec::with_capacity(lines.len());
    let mut table_delimiters = Vec::with_capacity(lines.len());
    let mut fence: Option<FenceDelimiter> = None;
    for source in lines {
        let delimiter = fence_delimiter(source);
        pipe_rows.push(is_pipe_row(source));
        table_delimiters.push(is_table_delimiter(source));
        fenced_lines.push(fence.is_some() || delimiter.is_some());
        if let Some(delimiter) = delimiter {
            fence = match fence {
                Some(open) if open.marker == delimiter.marker && delimiter.len >= open.len => None,
                None => Some(delimiter),
                current => current,
            };
        }
    }
    let mut table_lines = vec![false; pipe_rows.len()];
    for delimiter in 1..pipe_rows.len() {
        if !table_delimiters[delimiter] || !pipe_rows[delimiter - 1] {
            continue;
        }
        table_lines[delimiter - 1] = true;
        table_lines[delimiter] = true;
        let mut row = delimiter + 1;
        while row < pipe_rows.len() && pipe_rows[row] && !table_delimiters[row] {
            table_lines[row] = true;
            row += 1;
        }
    }
    (fenced_lines, table_lines)
}

fn line_source(buffer: &RopeBuffer, line: usize) -> String {
    buffer
        .line_range(LineId(line))
        .ok()
        .and_then(|range| buffer.text(range).ok())
        .unwrap_or_default()
}

/// Builds document-wide fenced-block context from a shared Rope snapshot.
/// This is intended for a single coalesced background job, never an input path.
pub fn parse_block_context(buffer: &RopeBuffer) -> BlockContextIndex {
    let lines = (0..buffer.line_count())
        .map(|line| line_source(buffer, line))
        .collect::<Vec<_>>();
    let (fenced_lines, table_lines) = scan_block_context(&lines);
    BlockContextIndex {
        revision: buffer.revision(),
        fenced_lines,
        table_lines,
    }
}

/// Lines scanned before the viewport when recovering fence/table state without
/// the background index. Bounds the fallback so visible parsing never depends on
/// total document size.
pub const LOCAL_CONTEXT_LOOKBACK: usize = 2_048;

/// Fenced-code and table context for a viewport, computed from a bounded window.
/// Indexed by absolute document line; lines outside the scanned window return
/// `None`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalBlockContext {
    base_line: usize,
    fenced_lines: Vec<bool>,
    table_lines: Vec<bool>,
}

impl LocalBlockContext {
    pub fn line_is_fenced(&self, line: usize) -> Option<bool> {
        line.checked_sub(self.base_line)
            .and_then(|index| self.fenced_lines.get(index).copied())
    }

    pub fn line_is_table(&self, line: usize) -> Option<bool> {
        line.checked_sub(self.base_line)
            .and_then(|index| self.table_lines.get(index).copied())
    }
}

/// Single bounded synchronous fallback used only while the background
/// [`BlockContextIndex`] is stale. Scans a lookback window before `visible.start`
/// to recover fence and table state, then classifies the visible lines. Never
/// scans the whole document, so it is safe on the visible-parse path.
pub fn local_block_context(
    buffer: &RopeBuffer,
    visible: std::ops::Range<usize>,
) -> LocalBlockContext {
    let line_count = buffer.line_count();
    let base_line = visible.start.saturating_sub(LOCAL_CONTEXT_LOOKBACK);
    // Include one line past the viewport so a header row whose delimiter sits
    // just below the last visible line is still recognized as a table.
    let scan_end = visible.end.saturating_add(1).min(line_count);
    let lines = (base_line..scan_end)
        .map(|line| line_source(buffer, line))
        .collect::<Vec<_>>();
    let (fenced_lines, table_lines) = scan_block_context(&lines);
    LocalBlockContext {
        base_line,
        fenced_lines,
        table_lines,
    }
}

fn absolute_range(base: usize, range: std::ops::Range<usize>) -> SourceRange {
    SourceRange::new(base + range.start, base + range.end)
}

/// Inline kinds whose open/close delimiters are collapsible markup. Kept in one
/// place because marker derivation and presentation must agree on exactly which
/// nodes carry delimiters; `CodeBlock` is included because presentation styles it
/// as an inline run even though its fence markers are derived block-side.
pub const fn is_delimited_inline(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Strong
            | NodeKind::Emphasis
            | NodeKind::Strikethrough
            | NodeKind::InlineCode
            | NodeKind::Link
            | NodeKind::CodeBlock
    )
}

/// Derives marker source ranges by lexing only inside the source ranges that
/// pulldown-cmark already attributed to each node. The event ranges stay
/// authoritative; this only recovers open/close delimiter positions that the
/// event stream does not expose. Returned ranges are sorted and merged.
fn derive_markers(tree: &MarkdownTree, range: SourceRange, source: &str) -> Vec<SourceRange> {
    let mut markers = Vec::new();
    for (_, block) in tree.blocks() {
        let relative = block.source_range.start.0.saturating_sub(range.start.0);
        let tail = source.get(relative..).unwrap_or_default();
        match block.kind {
            NodeKind::Heading(_) => {
                let hashes = tail
                    .as_bytes()
                    .iter()
                    .take_while(|byte| **byte == b'#')
                    .count();
                if hashes > 0 {
                    let suffix = usize::from(tail.as_bytes().get(hashes) == Some(&b' '));
                    markers.push(SourceRange::new(
                        block.source_range.start.0,
                        block.source_range.start.0 + hashes + suffix,
                    ));
                }
            }
            NodeKind::Quote => {
                if tail.starts_with("> ") {
                    markers.push(SourceRange::new(
                        block.source_range.start.0,
                        block.source_range.start.0 + 2,
                    ));
                }
            }
            NodeKind::ListItem { .. } => {
                let prefix = tail
                    .find(|character: char| !character.is_ascii_whitespace())
                    .unwrap_or(0);
                let item = &tail[prefix..];
                let marker_len =
                    if item.starts_with("- ") || item.starts_with("* ") || item.starts_with("+ ") {
                        2
                    } else {
                        item.find(". ").map_or(0, |end| end + 2)
                    };
                if marker_len > 0 {
                    markers.push(SourceRange::new(
                        block.source_range.start.0 + prefix,
                        block.source_range.start.0 + prefix + marker_len,
                    ));
                }
            }
            NodeKind::CodeBlock => {
                // Only the fence delimiter lines are markup; the code between
                // them is literal content and must stay visible. `tail` runs to
                // the end of the parsed slice, so clip it to the block first.
                let body = tail
                    .get(..block.source_range.end.0 - block.source_range.start.0)
                    .unwrap_or(tail);
                if fence_delimiter(body).is_some() {
                    let opening_end = body.find('\n').unwrap_or(body.len());
                    let opening = body[..opening_end].trim_end_matches(['\r', '\n']).len();
                    if opening > 0 {
                        markers.push(SourceRange::new(
                            block.source_range.start.0,
                            block.source_range.start.0 + opening,
                        ));
                    }
                    let closed = body.trim_end_matches(['\r', '\n']);
                    if let Some(closing_start) = closed.rfind('\n').map(|line_end| line_end + 1)
                        && closing_start > opening_end
                        && fence_delimiter(&closed[closing_start..]).is_some()
                    {
                        markers.push(SourceRange::new(
                            block.source_range.start.0 + closing_start,
                            block.source_range.start.0 + closed.len(),
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    for (_, span) in tree
        .iter()
        .filter(|(_, node)| is_delimited_inline(node.kind))
    {
        let start = span.source_range.start.0;
        let end = span.source_range.end.0;
        if start < range.start.0 || end > range.end.0 || start >= end {
            continue;
        }
        let text = &source[start - range.start.0..end - range.start.0];
        let marker_len = match span.kind {
            NodeKind::Strong | NodeKind::Emphasis => text
                .as_bytes()
                .first()
                .filter(|marker| matches!(marker, b'*' | b'_'))
                .map_or(0, |marker| {
                    text.as_bytes()
                        .iter()
                        .take_while(|byte| *byte == marker)
                        .count()
                }),
            NodeKind::Strikethrough => 2,
            NodeKind::InlineCode => text.bytes().take_while(|byte| *byte == b'`').count(),
            NodeKind::Link => {
                if let (Some(open), Some(close)) = (text.find('['), text.find("](")) {
                    markers.push(SourceRange::new(start + open, start + open + 1));
                    markers.push(SourceRange::new(start + close, end));
                }
                0
            }
            _ => 0,
        };
        if marker_len > 0 && marker_len * 2 <= text.len() {
            markers.push(SourceRange::new(start, start + marker_len));
            markers.push(SourceRange::new(end - marker_len, end));
        }
    }
    markers.sort_by_key(|marker| (marker.start, marker.end));
    let mut merged: Vec<SourceRange> = Vec::with_capacity(markers.len());
    for marker in markers {
        if let Some(previous) = merged.last_mut()
            && marker.start < previous.end
        {
            previous.end = previous.end.max(marker.end);
        } else {
            merged.push(marker);
        }
    }
    merged
}

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
        Tag::List(start) => NodeKind::List {
            ordered: start.is_some(),
        },
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
fn build_tree(source_range: SourceRange, source: &str) -> MarkdownTree {
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
            Event::Code(_) => {
                push(&mut nodes, &open, NodeKind::InlineCode, range);
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
            Event::SoftBreak | Event::HardBreak => {
                push(&mut nodes, &open, NodeKind::Break, range);
            }
            Event::Rule => {
                push(&mut nodes, &open, NodeKind::Rule, range);
            }
            _ => {
                push(&mut nodes, &open, NodeKind::Unsupported, range);
            }
        }
    }
    MarkdownTree { nodes }
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
    let tree = build_tree(source_range, source);
    let markers = derive_markers(&tree, source_range, source);
    MarkdownParse {
        revision,
        source_range,
        tree,
        markers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commonmark_ranges_remain_absolute_for_unicode_and_nested_styles() {
        let source = "## 日本語 **太字と _斜体_** `code` ~~del~~";
        let parsed = parse_document(
            Revision(7),
            SourceRange::new(100, 100 + source.len()),
            source,
        );
        assert_eq!(parsed.revision, Revision(7));
        assert!(parsed.tree.blocks().any(|(_, block)| {
            block.kind == NodeKind::Heading(2)
                && block.source_range == SourceRange::new(100, 100 + source.len())
        }));
        for kind in [
            NodeKind::Strong,
            NodeKind::Emphasis,
            NodeKind::InlineCode,
            NodeKind::Strikethrough,
        ] {
            assert!(parsed.tree.iter().any(|(_, node)| node.kind == kind));
        }
        assert!(parsed.tree.iter().all(|(_, node)| {
            node.source_range.start.0 >= 100 && node.source_range.end.0 <= 100 + source.len()
        }));
    }

    #[test]
    fn parses_fenced_code_as_a_code_block() {
        let source = "```rust\nlet answer = 42;\n```\n";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        assert!(
            parsed
                .tree
                .blocks()
                .any(|(_, block)| block.kind == NodeKind::CodeBlock)
        );
    }

    #[test]
    fn tree_nests_children_inside_their_parent_source_ranges() {
        let source = "- outer\n  - inner **bold**\n";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        for (id, node) in parsed.tree.iter() {
            let parent = parsed
                .tree
                .node(node.parent.expect("non-root node"))
                .unwrap();
            assert!(
                parent.source_range.start <= node.source_range.start
                    && node.source_range.end <= parent.source_range.end,
                "{id:?} {:?} escapes its parent {:?}",
                node.kind,
                parent.kind
            );
            assert_eq!(node.depth, parent.depth + 1);
        }
        let inner = parsed
            .tree
            .iter()
            .filter(|(_, node)| matches!(node.kind, NodeKind::ListItem { .. }))
            .map(|(id, _)| parsed.tree.list_depth(id))
            .collect::<Vec<_>>();
        assert_eq!(inner, vec![1, 2]);
    }

    #[test]
    fn task_list_items_carry_their_checkbox_state() {
        let source = "- [ ] todo\n- [x] done\n";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        let tasks = parsed
            .tree
            .iter()
            .filter_map(|(_, node)| match node.kind {
                NodeKind::ListItem { task } => Some(task),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(tasks, vec![Some(false), Some(true)]);
    }

    #[test]
    fn tables_nest_rows_and_cells_under_the_table() {
        let source = "| a | b |\n|---|---|\n| 1 | 2 |\n";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        let (table, _) = parsed
            .tree
            .iter()
            .find(|(_, node)| node.kind == NodeKind::Table)
            .expect("pipe table must parse as a table");
        let rows = parsed.tree.children(table);
        assert_eq!(rows.len(), 2);
        for row in rows {
            assert_eq!(
                parsed.tree.children(*row).len(),
                2,
                "each row has two cells"
            );
        }
    }

    #[test]
    fn background_context_tracks_fences_longer_than_local_overscan() {
        let mut source = String::from("```rust\n");
        source.push_str(&"inside\n".repeat(2_100));
        source.push_str("```\nafter\n");
        let buffer = RopeBuffer::from_text(&source);
        let index = parse_block_context(&buffer);
        assert_eq!(index.revision, Revision(0));
        assert_eq!(index.line_count(), buffer.line_count());
        assert_eq!(index.line_is_fenced(2_050), Some(true));
        assert_eq!(index.line_is_fenced(2_102), Some(false));
    }

    #[test]
    fn background_context_tracks_gfm_pipe_tables() {
        let buffer = RopeBuffer::from_text(
            "before\n| Name | 値 |\n|:---|---:|\n| 羽 | 3 |\n| 鳥 | 4 |\nafter\n",
        );
        let index = parse_block_context(&buffer);
        assert_eq!(index.line_is_table(0), Some(false));
        for line in 1..=4 {
            assert_eq!(index.line_is_table(line), Some(true));
        }
        assert_eq!(index.line_is_table(5), Some(false));
    }

    #[test]
    fn markers_cover_inline_open_and_close_delimiters() {
        let source = "**b** _i_ `c` ~~s~~ [t](u)";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        for expected in [
            (0, 2),
            (3, 5),
            (6, 7),
            (8, 9),
            (10, 11),
            (12, 13),
            (14, 16),
            (17, 19),
            (20, 21),
            (22, 26),
        ] {
            let range = SourceRange::new(expected.0, expected.1);
            assert!(parsed.markers.contains(&range), "missing marker {range:?}");
        }
        // Merged and sorted: strictly increasing, non-overlapping.
        assert!(
            parsed
                .markers
                .windows(2)
                .all(|pair| pair[0].end <= pair[1].start)
        );
    }

    #[test]
    fn markers_cover_block_prefixes_for_heading_quote_and_list() {
        let heading = parse_document(Revision(1), SourceRange::new(0, 8), "## Head\n");
        assert_eq!(
            heading.markers.first().copied(),
            Some(SourceRange::new(0, 3))
        );

        let quote = parse_document(Revision(1), SourceRange::new(0, 8), "> quote\n");
        assert_eq!(quote.markers.first().copied(), Some(SourceRange::new(0, 2)));

        let bullet = parse_document(Revision(1), SourceRange::new(0, 7), "- item\n");
        assert_eq!(
            bullet.markers.first().copied(),
            Some(SourceRange::new(0, 2))
        );

        let ordered = parse_document(Revision(1), SourceRange::new(0, 8), "1. item\n");
        assert_eq!(
            ordered.markers.first().copied(),
            Some(SourceRange::new(0, 3))
        );
    }

    #[test]
    fn local_fallback_matches_background_index_on_the_visible_window() {
        let buffer = RopeBuffer::from_text(
            "intro\n```rust\ncode\n```\n| Name | 値 |\n|:---|---:|\n| 羽 | 3 |\ntail\n",
        );
        let background = parse_block_context(&buffer);
        let local = local_block_context(&buffer, 0..buffer.line_count());
        for line in 0..buffer.line_count() {
            assert_eq!(local.line_is_fenced(line), background.line_is_fenced(line));
            assert_eq!(local.line_is_table(line), background.line_is_table(line));
        }
    }

    #[test]
    fn local_fallback_returns_none_outside_the_scanned_window() {
        let mut source = String::from("head\n");
        source.push_str(&"body\n".repeat(4_000));
        let buffer = RopeBuffer::from_text(&source);
        let local = local_block_context(&buffer, 3_500..3_510);
        assert_eq!(local.line_is_fenced(3_505), Some(false));
        // Far above the lookback window is not scanned.
        assert_eq!(local.line_is_fenced(0), None);
    }
}
