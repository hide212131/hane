//! CommonMark parsing with source-byte ranges.

// Parsed syntax values are routinely inspected without retaining their result
// in streaming and background-index call sites.
#![allow(
    clippy::must_use_candidate,
    reason = "syntax query APIs are intentionally discardable during incremental parsing"
)]
#![allow(
    clippy::doc_markdown,
    reason = "crate documentation uses established Markdown terminology as prose"
)]
#![allow(
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation,
    reason = "block-store indices are bounded by the in-memory source representation"
)]
#![allow(
    clippy::format_push_string,
    reason = "test fixture construction intentionally appends formatted fragments"
)]
#![allow(
    clippy::items_after_statements,
    reason = "a local parser helper stays beside its only call site"
)]
#![allow(
    clippy::too_many_lines,
    reason = "the formal event-to-block translation is deliberately kept as one audited table"
)]

mod block_index;
mod block_store;

pub use block_index::{
    BlockId, BlockIndex, BlockIndexState, BlockIndexUpdate, Confidence, IndexSource, IndexedBlock,
    PublishOutcome,
};

use hane_document::{LineId, Revision, RopeBuffer, SourceOffset, SourceRange, TextBuffer};
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag};
use std::sync::Arc;

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
    /// `start` is `Some(n)` for an ordered list starting at `n` — `0`,
    /// non-1 starts, and values written with leading zeros all parse
    /// losslessly to their numeric value — and `None` for a bullet list.
    /// The exact marker bytes, leading zeros included, live in
    /// [`MarkdownParse::list_item_markers`]'s source ranges, not here.
    List {
        start: Option<u64>,
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
    /// A CommonMark soft line break: an ordinary source line ending inside a
    /// paragraph's inline content. Purely a source-level join point, not
    /// markup and not the viewport's own wrap concept (`LineWrap::Soft` in
    /// `hane_presentation`, which is display-width wrapping and unrelated).
    SoftBreak,
    /// A CommonMark hard line break: two or more trailing spaces, or a
    /// trailing backslash, before a line ending inside a paragraph's inline
    /// content. Unlike [`Self::SoftBreak`], the syntax bytes preceding the
    /// line ending are markup and can be hidden/disclosed like other
    /// delimiters.
    HardBreak,
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
                | Self::SoftBreak
                | Self::HardBreak
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

    /// For a `ListItem`, its owning `List` node and its zero-based position
    /// among that list's direct item children (`0` for the first item). `None`
    /// when `id` is not a `ListItem` — every `ListItem` is a direct child of
    /// exactly one `List`, so callers needing a display ordinal derive it from
    /// this position and the owner's `NodeKind::List::start` rather than
    /// scanning source lines or a viewport.
    pub fn list_item_ordinal(&self, id: NodeId) -> Option<(NodeId, usize)> {
        let node = self.node(id)?;
        if !matches!(node.kind, NodeKind::ListItem { .. }) {
            return None;
        }
        let owner = node.parent?;
        let ordinal = self.children(owner).iter().position(|child| *child == id)?;
        Some((owner, ordinal))
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
    /// Fence delimiter ranges kept separately from the merged marker plan so a
    /// formal list projection can preserve them while filtering unrelated
    /// inline markers from literal code content.
    pub fence_markers: Vec<SourceRange>,
    /// Quote prefixes paired with their owning quote node. Continuation-line
    /// prefixes cannot be associated with an owner by comparing source starts.
    /// Kept before range merging so nested, adjacent prefixes retain ownership.
    pub quote_markers: Vec<(SourceRange, NodeId)>,
    /// List item bullet/number markers paired with their owning list item
    /// node. An indented item's own source range starts before its marker
    /// (at the indentation, not the bullet), so ownership cannot be recovered
    /// by comparing source starts either; kept explicit for the same reason
    /// as `quote_markers`.
    pub list_item_markers: Vec<(SourceRange, NodeId)>,
    /// A list item's own structural indentation on every physical line its
    /// content occupies past the marker's own opening line, paired with the
    /// owning item and the column width (not necessarily its byte length, as
    /// a tab can consume fewer bytes than columns) an inactive presentation
    /// should account for in layout. Separate from `list_item_markers`
    /// (which covers only the opening line's own bullet/number) and from
    /// `quote_markers` (a different container's own per-line prefix). A
    /// physical line several nested items all continue onto gets one range
    /// per owner, each covering only that item's own share of the
    /// indentation, the same way nested quote prefixes are split.
    pub list_structural_prefixes: Vec<(SourceRange, NodeId, usize)>,
    /// Padding removed from inline code after container prefixes and line
    /// endings are interpreted. Derived against the parser's code content.
    pub code_padding: Vec<SourceRange>,
    /// Spaces/tabs a soft or hard line break makes insignificant: the single
    /// trailing space CommonMark folds into a soft break, and the leading
    /// indentation of the physical line either break kind continues onto.
    /// Not markup — a soft break in particular carries none, see
    /// [`NodeKind::SoftBreak`] — so kept separate from `markers` the same way
    /// `code_padding` is, while still hidden from rendered presentation.
    pub line_break_padding: Vec<SourceRange>,
}

/// Document-wide list information retained by a formal [`BlockIndex`] so a
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
    rows: Vec<ListProjectionRow>,
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
    item_index: usize,
    is_code_block: bool,
}

impl ListProjection {
    pub(crate) fn new(
        items: Vec<ListProjectionItem>,
        prefixes: Vec<ListProjectionPrefix>,
        lists: Vec<ListProjectionList>,
        rows: Vec<ListProjectionRow>,
    ) -> Self {
        Self {
            items,
            prefixes,
            lists,
            rows,
        }
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
        self.items.get(row.item_index)
    }

    /// Reports whether the formal parse presents the physical list row as code.
    /// A bounded viewport parse can mistake a four-column list continuation for
    /// an indented code block; callers use this bit only to restore the formal
    /// display kind while preserving genuine code nested in the list.
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

/// A single physical line's fence delimiter shape: which byte repeats
/// (`` ` `` or `~`) and how many times. Exposed so a caller presenting one
/// physical line at a time (`hane_presentation`) can classify a candidate
/// fence line without re-parsing the block it belongs to; only whether *this*
/// line closes a *specific* opening (same [`Self::marker`], [`Self::len`] at
/// least as long, per [`fence_closes`]) is Markdown-semantic, and that stays
/// the caller's own decision since only the caller knows which line is the
/// block's actual opening.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FenceDelimiter {
    pub marker: u8,
    pub len: usize,
}

/// Opening or closing fence of a fenced code block, if this line is one. Used by
/// marker derivation to bound the fence delimiters it hides.
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

/// Returns a delimiter only when the remainder of the physical line is valid
/// closing-fence whitespace. Opening fences intentionally use
/// [`fence_delimiter`] because their info string may follow the marker run;
/// closing fences may contain spaces or tabs, but no info string or other
/// content.
pub fn fence_closing_delimiter(source: &str) -> Option<FenceDelimiter> {
    let delimiter = fence_delimiter(source)?;
    let trimmed = source.trim_start_matches(' ');
    trimmed[delimiter.len..]
        .bytes()
        .all(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
        .then_some(delimiter)
}

/// Whether a line shaped like `candidate` legally closes a fence that opened
/// with `opening`: CommonMark requires the same marker character and a run at
/// least as long as the opening's. A closing-shaped line with the wrong
/// character or a shorter run is not a fence at all — it is literal code
/// content, and must stay visible rather than being hidden as markup.
pub const fn fence_closes(opening: FenceDelimiter, candidate: FenceDelimiter) -> bool {
    opening.marker == candidate.marker && candidate.len >= opening.len
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

/// Lines scanned before the viewport when recovering block boundaries without a
/// published [`BlockIndex`]. Bounds the fallback so visible parsing never depends
/// on total document size.
const LOCAL_BLOCK_LOOKBACK: usize = 2_048;

/// Block boundaries for one viewport, parsed from a bounded window.
///
/// Used only while no document-wide [`BlockIndex`] is published — during the
/// first frames after a document is opened, and after an edit history gap drops
/// the published index. The window starts a fixed number of lines above the
/// viewport, so a construct that opens further above (a very long fenced block)
/// is not seen; that is the approximation the bound buys, and it is why every
/// block reported here is [`Confidence::Provisional`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalBlockIndex {
    revision: Revision,
    window: SourceRange,
    /// Block kinds with their absolute source ranges and line counts, tiling
    /// `window`.
    blocks: Vec<(NodeKind, SourceRange, usize)>,
}

impl LocalBlockIndex {
    fn indexed(&self, ordinal: usize) -> IndexedBlock {
        let (kind, source_range, line_count) = self.blocks[ordinal];
        IndexedBlock {
            ordinal,
            // Keyed by start offset rather than a counter: the window is
            // re-parsed on every scroll, and an offset keeps a block's cache
            // entry addressable across those re-parses.
            id: BlockId(source_range.start.0 as u64),
            kind,
            source_range,
            revision: self.revision,
            confidence: Confidence::Provisional,
            line_count,
            leading_content_lines: 0,
        }
    }

    /// Block owning `offset`, or `None` when the offset lies outside the window.
    pub fn block_at(&self, offset: SourceOffset) -> Option<IndexedBlock> {
        let ordinal = self
            .blocks
            .partition_point(|(_, range, _)| range.end.0 <= offset.0)
            .min(self.blocks.len().checked_sub(1)?);
        let block = self.indexed(ordinal);
        (block.source_range.start <= offset && offset <= block.source_range.end).then_some(block)
    }

    /// Blocks overlapping `range`, in document order.
    pub fn blocks_in(&self, range: SourceRange) -> impl Iterator<Item = IndexedBlock> + '_ {
        let first = self
            .blocks
            .partition_point(|(_, span, _)| span.end.0 <= range.start.0);
        (first..self.blocks.len())
            .take_while(move |ordinal| {
                *ordinal == first || self.blocks[*ordinal].1.start.0 < range.end.0
            })
            .map(|ordinal| self.indexed(ordinal))
    }
}

/// Single bounded synchronous parse used only while no [`BlockIndex`] is
/// published. Reads a window that starts `LOCAL_BLOCK_LOOKBACK` lines above
/// `visible` and ends one line below it, and tiles that window into blocks the
/// same way the formal index tiles the document. Never reads the whole document,
/// so it is safe on the visible-render path.
pub fn local_block_index(buffer: &RopeBuffer, visible: std::ops::Range<usize>) -> LocalBlockIndex {
    let line_count = buffer.line_count();
    let revision = buffer.revision();
    let empty = |window| LocalBlockIndex {
        revision,
        window,
        blocks: Vec::new(),
    };
    if line_count == 0 {
        return empty(SourceRange::empty(0));
    }
    let first_line = visible.start.saturating_sub(LOCAL_BLOCK_LOOKBACK);
    // One line past the viewport, so a construct whose closing line sits just
    // below the last visible line is still parsed as part of the same block.
    let last_line = visible.end.min(line_count - 1);
    let (Ok(start), Ok(end)) = (
        buffer.line_range(LineId(first_line)),
        buffer.line_range(LineId(last_line)),
    ) else {
        return empty(SourceRange::empty(0));
    };
    let window = SourceRange::new(start.start.0, end.end.0);
    let Ok(text) = buffer.text(window) else {
        return empty(window);
    };
    let parsed = parse_document(revision, window, &text);
    let mut offset = window.start.0;
    let blocks = block_index::tiled_blocks(&parsed.tree, window, &text)
        .into_iter()
        .map(|(kind, length, lines, _)| {
            let range = SourceRange::new(offset, offset + length);
            offset += length;
            (kind, range, lines)
        })
        .collect();
    LocalBlockIndex {
        revision,
        window,
        blocks,
    }
}

fn absolute_range(base: usize, range: std::ops::Range<usize>) -> SourceRange {
    SourceRange::new(base + range.start, base + range.end)
}

/// Inline kinds whose open/close delimiters are collapsible markup. Kept in one
/// place because marker derivation and presentation must agree on exactly which
/// nodes carry delimiters; `CodeBlock` is included because presentation styles it
/// as an inline run even though its fence markers are derived block-side.
pub const fn has_delimiter_markers(kind: NodeKind) -> bool {
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

/// A cursor in a physical line's container prefix. Tabs can be partially
/// consumed by CommonMark containers, so byte offsets and columns differ.
#[derive(Clone, Copy, Default)]
struct PrefixCursor {
    byte: usize,
    column: usize,
    pending_spaces: usize,
}

impl PrefixCursor {
    fn space(&mut self, line: &[u8]) -> bool {
        if self.pending_spaces > 0 {
            self.pending_spaces -= 1;
        } else {
            match line.get(self.byte) {
                Some(b' ') => {}
                Some(b'\t') => self.pending_spaces = 3 - self.column % 4,
                _ => return false,
            }
            self.byte += 1;
        }
        self.column += 1;
        true
    }

    fn indent(&mut self, line: &[u8], limit: usize) -> usize {
        let start = self.column;
        while self.column - start < limit && self.space(line) {}
        self.column - start
    }

    fn quote(&mut self, line: &[u8]) -> Option<std::ops::Range<usize>> {
        self.indent(line, 3);
        if self.pending_spaces != 0 || line.get(self.byte) != Some(&b'>') {
            return None;
        }
        let start = self.byte;
        self.byte += 1;
        self.column += 1;
        self.space(line);
        Some(start..self.byte)
    }

    /// `line`-relative byte range of the marker symbol (bullet char, or
    /// digits plus their `.`/`)` terminator) parsed by [`Self::list_item`],
    /// together with the column width of the whole prefix a continuation
    /// line on the same item must match.
    fn list_item(&mut self, line: &[u8]) -> Option<ListItemPrefix> {
        let start_column = self.column;
        self.indent(line, 3);
        if self.pending_spaces != 0 {
            return None;
        }
        let marker = self.byte;
        match line.get(self.byte) {
            Some(b'-' | b'+' | b'*') => self.byte += 1,
            Some(b'0'..=b'9') => {
                while self.byte - marker < 9 && line.get(self.byte).is_some_and(u8::is_ascii_digit)
                {
                    self.byte += 1;
                }
                if !matches!(line.get(self.byte), Some(b'.' | b')')) {
                    return None;
                }
                self.byte += 1;
            }
            _ => return None,
        }
        self.column += self.byte - marker;
        let symbol = marker..self.byte;
        // An empty opening line has one implicit padding column, including
        // when the marker touches EOL or has several trailing spaces/tabs.
        // This matches the parser's empty-list-item continuation indentation.
        if line[self.byte..]
            .iter()
            .all(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
        {
            return Some(ListItemPrefix {
                symbol,
                width: self.column - start_column + 1,
            });
        }
        let after_marker = *self;
        let padding = self.indent(line, 5);
        if padding == 0 {
            return None;
        }
        // Five or more spaces mean one padding column followed by content
        // indentation, rather than a wider list-item container.
        if padding > 4 {
            *self = after_marker;
            self.space(line);
        }
        Some(ListItemPrefix {
            symbol,
            width: self.column - start_column,
        })
    }
}

struct ListItemPrefix {
    symbol: std::ops::Range<usize>,
    width: usize,
}

#[derive(Clone, Copy)]
enum PrefixContainer {
    Quote,
    ListItem { start: usize, indent: usize },
}

fn consume_containers(
    containers: &[PrefixContainer],
    line_start: usize,
    line: &[u8],
) -> Option<PrefixCursor> {
    let mut cursor = PrefixCursor::default();
    for container in containers {
        match *container {
            PrefixContainer::Quote => {
                cursor.quote(line)?;
            }
            PrefixContainer::ListItem { start, indent } => {
                if (line_start..line_start + line.len()).contains(&start) {
                    cursor.list_item(line)?;
                } else if cursor.indent(line, indent) != indent {
                    return None;
                }
            }
        }
    }
    Some(cursor)
}

/// CommonMark treats CR, LF and CRLF as line endings, independently of the
/// editor's LF-based RopeBuffer lines. Keep the original bytes for source maps.
fn markdown_line_start(source: &str, mut at: usize) -> usize {
    if source.as_bytes().get(at) == Some(&b'\n') && at > 0 && source.as_bytes()[at - 1] == b'\r' {
        at -= 1;
    }
    source[..at].rfind(['\r', '\n']).map_or(0, |at| at + 1)
}

pub(crate) fn markdown_lines(mut source: &str) -> impl Iterator<Item = &str> {
    std::iter::from_fn(move || {
        if source.is_empty() {
            return None;
        }
        let end = source.find(['\r', '\n']).map_or(source.len(), |at| {
            at + if source.as_bytes()[at] == b'\r' && source.as_bytes().get(at + 1) == Some(&b'\n')
            {
                2
            } else {
                1
            }
        });
        let (line, rest) = source.split_at(end);
        source = rest;
        Some(line)
    })
}

/// Recover a quote's markers from its actual ancestor containers, not a fixed
/// byte width per depth. Lazy continuation lines fail the prefix scan and have
/// no marker. List-item padding is measured on that item's opening line.
fn ancestor_containers(
    tree: &MarkdownTree,
    id: NodeId,
    range: SourceRange,
    source: &str,
) -> Option<Vec<PrefixContainer>> {
    let ancestors: Vec<_> = tree.ancestors(id).skip(1).collect();
    let mut containers = Vec::new();
    for ancestor in ancestors.into_iter().rev() {
        let node = tree.node(ancestor).expect("ancestor exists");
        match node.kind {
            NodeKind::Quote => containers.push(PrefixContainer::Quote),
            NodeKind::ListItem { .. } => {
                let start = node.source_range.start.0 - range.start.0;
                let line_start = markdown_line_start(source, start);
                let line = markdown_lines(&source[line_start..]).next().unwrap_or("");
                let mut cursor = consume_containers(&containers, line_start, line.as_bytes())?;
                let indent = cursor.list_item(line.as_bytes())?.width;
                containers.push(PrefixContainer::ListItem { start, indent });
            }
            _ => {}
        }
    }
    Some(containers)
}

fn quote_markers(
    tree: &MarkdownTree,
    id: NodeId,
    range: SourceRange,
    source: &str,
) -> Vec<SourceRange> {
    let Some(containers) = ancestor_containers(tree, id, range, source) else {
        return Vec::new();
    };
    let node = tree.node(id).expect("quote exists");
    let start = node.source_range.start.0 - range.start.0;
    let end = node.source_range.end.0 - range.start.0;
    let mut line_start = markdown_line_start(source, start);
    let mut markers = Vec::new();
    for line in markdown_lines(&source[line_start..end]) {
        if let Some(mut cursor) = consume_containers(&containers, line_start, line.as_bytes())
            && let Some(marker) = cursor.quote(line.as_bytes())
        {
            markers.push(absolute_range(range.start.0 + line_start, marker));
        }
        line_start += line.len();
    }
    markers
}

struct ParsedCodeSpan {
    node: NodeId,
    content: String,
}

/// Project code content back to source before choosing padding. Source spans
/// include container prefixes; those bytes are not part of Event::Code's text.
fn code_padding(
    tree: &MarkdownTree,
    codes: &[ParsedCodeSpan],
    range: SourceRange,
    source: &str,
) -> Vec<SourceRange> {
    let mut padding = Vec::new();
    for code in codes {
        let node = tree.node(code.node).expect("code node exists");
        let start = node.source_range.start.0 - range.start.0;
        let end = node.source_range.end.0 - range.start.0;
        let delimiter = source[start..end]
            .bytes()
            .take_while(|b| *b == b'`')
            .count();
        let Some(containers) = ancestor_containers(tree, code.node, range, source) else {
            continue;
        };
        let mut cursor = start + delimiter;
        let end = end - delimiter;
        let mut normalized = String::new();
        let mut first = None;
        let mut last = None;
        while cursor < end {
            let from = cursor;
            let byte = source.as_bytes()[cursor];
            let is_break = matches!(byte, b'\r' | b'\n');
            let character = if is_break {
                cursor += 1;
                if byte == b'\r' && source.as_bytes().get(cursor) == Some(&b'\n') {
                    cursor += 1;
                }
                ' '
            } else {
                let character = source[cursor..].chars().next().expect("source character");
                cursor += character.len_utf8();
                character
            };
            let unit = absolute_range(range.start.0, from..cursor);
            first.get_or_insert(unit);
            last = Some(unit);
            normalized.push(character);
            if is_break {
                // Lazy continuation may carry only some ancestor prefixes.
                let line = &source.as_bytes()[cursor..end];
                let mut prefix = PrefixCursor::default();
                for container in &containers {
                    let before = prefix;
                    let matched = match container {
                        PrefixContainer::Quote => prefix.quote(line).is_some(),
                        PrefixContainer::ListItem { indent, .. } => {
                            prefix.indent(line, *indent) == *indent
                        }
                    };
                    if !matched {
                        prefix = before;
                        break;
                    }
                }
                cursor += prefix.byte;
            }
        }
        // The parser is authoritative: only emit padding when removing one
        // semantic space from each end reproduces its actual code content.
        if normalized.starts_with(' ')
            && normalized.ends_with(' ')
            && normalized.bytes().any(|byte| byte != b' ')
            && normalized.get(1..normalized.len() - 1) == Some(code.content.as_str())
        {
            padding.extend([first.expect("nonempty code"), last.expect("nonempty code")]);
        }
    }
    padding
}

/// Bytes a soft or hard line break makes insignificant, so presentation can
/// hide them like other derived padding while keeping their source bytes
/// addressable. Two distinct sources, per the CommonMark line-break rules:
///
/// - The single trailing U+0020 space CommonMark folds into a soft break (two
///   or more would have made it a hard break instead; CommonMark §6.8 defines
///   `space` as U+0020 specifically, so a trailing tab/VT/FF is not part of
///   this rule and stays visible/addressable textual content instead).
///   pulldown-cmark's own `Text` item ends before this byte and its
///   `SoftBreak` item starts at the line ending itself, so — unlike a hard
///   break's own syntax, which is a real `HardBreak` item covering it — this
///   one byte is not part of any parsed item's range at all; it is recovered
///   here directly from `source`.
/// - The leading indentation of the physical line either break kind
///   continues onto: CommonMark drops any amount of it when forming a
///   paragraph's inline content, independent of the break that precedes it.
fn line_break_padding(tree: &MarkdownTree, range: SourceRange, source: &str) -> Vec<SourceRange> {
    let mut padding = Vec::new();
    for (id, span) in tree
        .iter()
        .filter(|(_, node)| matches!(node.kind, NodeKind::SoftBreak | NodeKind::HardBreak))
    {
        let end = span.source_range.end.0.saturating_sub(range.start.0);
        if end > source.len() {
            continue;
        }
        if span.kind == NodeKind::SoftBreak {
            let break_start = span.source_range.start.0;
            if break_start > range.start.0
                && let Some(byte) = source
                    .as_bytes()
                    .get(break_start - range.start.0 - 1)
                    .copied()
                && byte == b' '
            {
                padding.push(SourceRange::new(break_start - 1, break_start));
            }
        }
        let Some(containers) = ancestor_containers(tree, id, range, source) else {
            continue;
        };
        // `end` is the byte right after the line ending this break owns (see
        // the `HardBreak` marker derivation above), i.e. the start of the
        // physical line it continues onto.
        let line_start = end;
        let line = source
            .get(line_start..)
            .and_then(|rest| markdown_lines(rest).next())
            .unwrap_or("");
        // Lazy continuation may carry only some ancestor prefixes, or none:
        // stop at the first container this line does not actually have, the
        // same way `code_padding` does for code spans. The parser already
        // decided this line is part of the same inline content (that is why
        // it produced this SoftBreak/HardBreak at all), so whatever leading
        // whitespace remains past the prefixes this line really has is still
        // insignificant, regardless of how many ancestor levels went missing.
        let mut cursor = PrefixCursor::default();
        for container in &containers {
            let before = cursor;
            let matched = match *container {
                PrefixContainer::Quote => cursor.quote(line.as_bytes()).is_some(),
                PrefixContainer::ListItem { start, indent } => {
                    if (line_start..line_start + line.len()).contains(&start) {
                        cursor.list_item(line.as_bytes()).is_some()
                    } else {
                        cursor.indent(line.as_bytes(), indent) == indent
                    }
                }
            };
            if !matched {
                cursor = before;
                break;
            }
        }
        let before = cursor.byte;
        cursor.indent(line.as_bytes(), line.len());
        if cursor.byte > before {
            padding.push(absolute_range(
                range.start.0 + line_start,
                before..cursor.byte,
            ));
        }
    }
    padding
}

struct DerivedMarkers {
    markers: Vec<SourceRange>,
    fence_markers: Vec<SourceRange>,
    quote_markers: Vec<(SourceRange, NodeId)>,
    list_item_markers: Vec<(SourceRange, NodeId)>,
}

/// Derives marker source ranges by lexing only inside the source ranges that
/// pulldown-cmark already attributed to each node. The event ranges stay
/// authoritative; this only recovers open/close delimiter positions that the
/// event stream does not expose. Returned ranges are sorted and merged.
fn derive_markers(tree: &MarkdownTree, range: SourceRange, source: &str) -> DerivedMarkers {
    let mut markers = Vec::new();
    let mut fence_markers = Vec::new();
    let mut quote_owners = Vec::new();
    let mut list_item_owners = Vec::new();
    for (id, block) in tree.blocks() {
        let relative = block.source_range.start.0.saturating_sub(range.start.0);
        let tail = source.get(relative..).unwrap_or_default();
        match block.kind {
            NodeKind::Heading(_) => {
                // Only the parser decides whether this is ATX (Setext starts
                // with content). Recover delimiters inside its authoritative
                // heading range, leaving escapes and inline delimiters intact.
                let body = &tail[..block.source_range.end.0 - block.source_range.start.0];
                let hashes = body.bytes().take_while(|byte| *byte == b'#').count();
                if (1..=6).contains(&hashes)
                    && body
                        .as_bytes()
                        .get(hashes)
                        .is_none_or(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
                {
                    let opening_end = hashes
                        + body[hashes..]
                            .bytes()
                            .take_while(|byte| matches!(byte, b' ' | b'\t'))
                            .count();
                    let start = block.source_range.start.0;
                    let end = start + body.trim_end_matches(['\r', '\n']).len();
                    markers.push(SourceRange::new(start, start + opening_end));
                    // The parser has already excluded the optional closer and
                    // trailing whitespace from its last direct child. Do not
                    // re-scan hashes: escaped/literal hashes belong to content.
                    let content_end = block
                        .children
                        .last()
                        .and_then(|id| tree.node(*id))
                        .map_or(start + opening_end, |node| node.source_range.end.0);
                    if content_end < end {
                        markers.push(SourceRange::new(content_end, end));
                    }
                }
            }
            NodeKind::Quote => {
                for marker in quote_markers(tree, id, range, source) {
                    markers.push(marker);
                    quote_owners.push((marker, id));
                }
            }
            NodeKind::ListItem { .. } => {
                // The marker (bullet, or digits plus `.`/`)`) appears only on
                // the item's own opening physical line; unlike a quote's
                // per-line prefix, no ancestor container prefix precedes it
                // here, since the item's own source range already starts at
                // that line's indentation.
                let line = markdown_lines(tail).next().unwrap_or("");
                if let Some(prefix) = PrefixCursor::default().list_item(line.as_bytes()) {
                    // Exactly one separator byte (space or tab) is markup,
                    // matching the one CommonMark requires; an item whose
                    // marker touches the line ending has none. Any further
                    // padding is ordinary indentation, not marker syntax.
                    let mut end = prefix.symbol.end;
                    if matches!(line.as_bytes().get(end), Some(b' ' | b'\t')) {
                        end += 1;
                    }
                    let marker = SourceRange::new(
                        block.source_range.start.0 + prefix.symbol.start,
                        block.source_range.start.0 + end,
                    );
                    markers.push(marker);
                    list_item_owners.push((marker, id));
                }
            }
            NodeKind::CodeBlock => {
                // Only the fence delimiter lines are markup; the code between
                // them is literal content and must stay visible.
                let node_start = block.source_range.start.0 - range.start.0;
                let node_end = block.source_range.end.0 - range.start.0;
                let opening_line = markdown_lines(&source[node_start..node_end])
                    .next()
                    .unwrap_or("");
                if let Some(opening_fence) = fence_delimiter(opening_line) {
                    let opening_content_len = opening_line.trim_end_matches(['\r', '\n']).len();
                    // Only the delimiter run itself (leading indentation plus
                    // the repeated `` ` `` or `~` bytes) is markup. An info
                    // string after it — a language identifier such as `rust`
                    // — is not fence syntax and must stay a visible, editable
                    // part of the line rather than disappearing into the same
                    // hidden marker.
                    let indent = opening_content_len
                        - opening_line[..opening_content_len]
                            .trim_start_matches(' ')
                            .len();
                    let opening = indent + opening_fence.len;
                    if opening > 0 {
                        let marker = SourceRange::new(
                            block.source_range.start.0,
                            block.source_range.start.0 + opening,
                        );
                        markers.push(marker);
                        fence_markers.push(marker);
                    }
                    // A quote or list item nested fence still carries its
                    // container's own prefix on every continuation line,
                    // inside this block's own raw source range (the parser
                    // reports the node's range in the original, un-stripped
                    // source, the same reason `quote_markers`/`code_padding`
                    // exist). Skip exactly that prefix on each physical line
                    // so the block's real last logical line — not a
                    // prefix-contaminated lookalike such as `"> ```"` — is
                    // what gets checked against the opening fence.
                    let containers = ancestor_containers(tree, id, range, source).unwrap_or_default();
                    let mut line_start = node_start + opening_line.len();
                    let mut last_logical: Option<(usize, &str)> = None;
                    for line in markdown_lines(&source[line_start..node_end]) {
                        let this_line_start = line_start;
                        line_start += line.len();
                        let logical_start =
                            consume_containers(&containers, this_line_start, line.as_bytes())
                                .map_or(this_line_start, |cursor| this_line_start + cursor.byte);
                        last_logical =
                            Some((logical_start, &source[logical_start..this_line_start + line.len()]));
                    }
                    // A closing-shaped last line only really closes the fence
                    // when it repeats the opening's own marker character at
                    // least as many times (CommonMark's closing-fence rule);
                    // otherwise it is an unterminated fence and this line is
                    // literal code content, not markup, however much it may
                    // look like a shorter or differently-charactered fence.
                    if let Some((closing_start, closing_line)) = last_logical
                        && let Some(closing_fence) = fence_closing_delimiter(closing_line)
                        && fence_closes(opening_fence, closing_fence)
                    {
                        let closing_len = closing_line.trim_end_matches(['\r', '\n']).len();
                        let marker = SourceRange::new(
                            range.start.0 + closing_start,
                            range.start.0 + closing_start + closing_len,
                        );
                        markers.push(marker);
                        fence_markers.push(marker);
                    }
                }
            }
            _ => {}
        }
    }
    for (_, span) in tree
        .iter()
        .filter(|(_, node)| has_delimiter_markers(node.kind))
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
    for (_, span) in tree
        .iter()
        .filter(|(_, node)| node.kind == NodeKind::HardBreak)
    {
        let start = span.source_range.start.0;
        let end = span.source_range.end.0;
        if start < range.start.0 || end > range.end.0 || start >= end {
            continue;
        }
        // pulldown-cmark's hard-break range covers the break syntax (trailing
        // spaces or a backslash) plus the physical line ending it precedes.
        // Only the syntax is markup; the line-ending bytes stay ordinary
        // source so they remain addressable/preserved rather than hidden.
        let text = &source[start - range.start.0..end - range.start.0];
        let marker_len = text.trim_end_matches(['\r', '\n']).len();
        if marker_len > 0 {
            markers.push(SourceRange::new(start, start + marker_len));
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
    fence_markers.sort_by_key(|marker| (marker.start, marker.end));
    fence_markers.dedup();
    DerivedMarkers {
        markers: merged,
        fence_markers,
        quote_markers: quote_owners,
        list_item_markers: list_item_owners,
    }
}

/// Recovers a list item's own structural indentation on every physical line
/// its content occupies, including parser-accepted padding between an opening
/// marker and its body: source-addressable so an inactive presentation can
/// hide exactly the bytes the parser required for the item to keep owning that
/// line, and disclose the original bytes (spaces, tabs) once the item itself
/// is active.
///
/// Mirrors [`quote_markers`]'s per-owner tiling for nested containers: each
/// item is scanned independently, consuming its own ancestors first via
/// [`ancestor_containers`]/[`consume_containers`] and attributing only the
/// remainder — its own registered indent width — to itself, so a physical
/// line several nested items all continue onto ends up with one prefix range
/// per owner rather than one range that conflates them. A line whose
/// required indentation is not fully present (a lazy paragraph continuation,
/// or a blank line) is left alone: [`line_break_padding`] already hides that
/// shortfall — and any genuinely superfluous indentation past what every
/// level requires, since its own consumption only starts after every
/// container it can match has already consumed its own required width — as
/// one undivided run, because CommonMark discards it without regard to which
/// container needed which part of it.
fn list_structural_prefixes(
    tree: &MarkdownTree,
    range: SourceRange,
    source: &str,
) -> Vec<(SourceRange, NodeId, usize)> {
    let mut prefixes = Vec::new();
    for (id, block) in tree.blocks() {
        if !matches!(block.kind, NodeKind::ListItem { .. }) {
            continue;
        }
        let Some(containers) = ancestor_containers(tree, id, range, source) else {
            continue;
        };
        let start = block.source_range.start.0 - range.start.0;
        let end = block.source_range.end.0 - range.start.0;
        let opening_line_start = markdown_line_start(source, start);
        let opening_line = markdown_lines(&source[opening_line_start..])
            .next()
            .unwrap_or("");
        let Some(mut opening_cursor) =
            consume_containers(&containers, opening_line_start, opening_line.as_bytes())
        else {
            continue;
        };
        let before_own_prefix = opening_cursor.byte;
        let mut indent_cursor = opening_cursor;
        let own_prefix_columns = indent_cursor.indent(opening_line.as_bytes(), 3);
        let marker_start_cursor = opening_cursor;
        let Some(prefix) = opening_cursor.list_item(opening_line.as_bytes()) else {
            continue;
        };
        if own_prefix_columns > 0 {
            prefixes.push((
                absolute_range(
                    range.start.0 + opening_line_start,
                    before_own_prefix..prefix.symbol.start,
                ),
                id,
                own_prefix_columns,
            ));
        }
        // The marker projection hides exactly one separator byte so that it
        // can preserve the source mapping of the written marker. CommonMark
        // still treats the remaining 1-3 columns of padding as structural
        // when there are at most four columns after the marker. Claim those
        // bytes separately so an inactive row renders one synthesized label
        // followed immediately by its body. For the 5+ case `list_item`
        // resets to one separator column, intentionally leaving the excess
        // spaces visible as content indentation.
        let marker_end = prefix.symbol.end
            + usize::from(matches!(
                opening_line.as_bytes().get(prefix.symbol.end),
                Some(b' ' | b'\t')
            ));
        if opening_cursor.byte > marker_end {
            let mut marker_end_cursor = marker_start_cursor;
            marker_end_cursor.indent(opening_line.as_bytes(), 3);
            marker_end_cursor.byte += prefix.symbol.len();
            marker_end_cursor.column += prefix.symbol.len();
            if marker_end_cursor.space(opening_line.as_bytes()) {
                while marker_end_cursor.pending_spaces > 0 {
                    marker_end_cursor.space(opening_line.as_bytes());
                }
            }
            let columns = opening_cursor
                .column
                .saturating_sub(marker_end_cursor.column);
            if columns > 0 {
                prefixes.push((
                    absolute_range(
                        range.start.0 + opening_line_start,
                        marker_end..opening_cursor.byte,
                    ),
                    id,
                    columns,
                ));
            }
        }
        let indent = prefix.width;

        let mut line_start = opening_line_start + opening_line.len();
        for line in markdown_lines(&source[line_start..end]) {
            if let Some(mut cursor) = consume_containers(&containers, line_start, line.as_bytes()) {
                let before = cursor.byte;
                if cursor.indent(line.as_bytes(), indent) == indent && cursor.byte > before {
                    prefixes.push((
                        absolute_range(range.start.0 + line_start, before..cursor.byte),
                        id,
                        indent,
                    ));
                }
            }
            line_start += line.len();
        }
    }
    prefixes
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
fn build_tree(source_range: SourceRange, source: &str) -> (MarkdownTree, Vec<ParsedCodeSpan>) {
    let mut codes = Vec::new();
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
    (MarkdownTree { nodes }, codes)
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
    let (tree, codes) = build_tree(source_range, source);
    let markers = derive_markers(&tree, source_range, source);
    let code_padding = code_padding(&tree, &codes, source_range, source);
    let line_break_padding = line_break_padding(&tree, source_range, source);
    let list_structural_prefixes = list_structural_prefixes(&tree, source_range, source);
    MarkdownParse {
        revision,
        source_range,
        tree,
        markers: markers.markers,
        fence_markers: markers.fence_markers,
        quote_markers: markers.quote_markers,
        list_item_markers: markers.list_item_markers,
        list_structural_prefixes,
        code_padding,
        line_break_padding,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_padding_tracks_semantic_spaces_instead_of_container_bytes() {
        for (source, expected) in [
            ("` code `", vec![(1, 2), (6, 7)]),
            // A CRLF contributes one semantic space but covers two source bytes.
            ("`\r\nx\r\n`", vec![(1, 3), (4, 6)]),
            ("> `\r\n> x\r\n> `", vec![(3, 5), (8, 10)]),
            ("> ` \n>  `", vec![]),
            ("` `", vec![]),
            ("`code `", vec![]),
            ("` \t `", vec![(1, 2), (3, 4)]),
        ] {
            let base = 31;
            let parsed = parse_document(
                Revision(1),
                SourceRange::new(base, base + source.len()),
                source,
            );
            let expected: Vec<_> = expected
                .into_iter()
                .map(|(start, end)| SourceRange::new(base + start, base + end))
                .collect();
            assert_eq!(parsed.code_padding, expected, "{source:?}");
            for padding in &parsed.code_padding {
                assert!(
                    parsed
                        .markers
                        .iter()
                        .all(|marker| !padding.intersects(*marker))
                );
            }
        }
    }

    #[test]
    fn nested_fence_markers_skip_the_quotes_own_prefix_on_every_line() {
        // The parser reports a nested block's own range in the original,
        // un-stripped source, so the raw bytes between a nested fence's
        // opening and closing line still contain the quote's "> " prefix.
        // Marker derivation must skip exactly that prefix per line to find
        // the fence's real closing line, not a prefix-contaminated
        // lookalike such as "> ```".
        let source = "> ```rust\n> code\n> ```\n";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        let covered = parsed
            .markers
            .iter()
            .map(|marker| &source[marker.start.0..marker.end.0])
            .collect::<Vec<_>>();
        assert_eq!(covered, vec!["> ", "```", "> ", "> ", "```"]);
    }

    #[test]
    fn an_unterminated_fences_closing_lookalike_last_line_stays_unhidden() {
        for source in [
            // The closing run is shorter than the opening's: CommonMark
            // leaves the fence open, so the whole document is one code
            // block and its last physical line — even though it is shaped
            // like a fence — is literal content, not markup.
            "````rust\ncode\n```\n",
            // The closing line's marker character differs from the
            // opening's: same rule, different reason.
            "```rust\ncode\n~~~\n",
            // A closing fence cannot carry an info string or other content;
            // the marker run is literal when non-whitespace follows it.
            "```rust\ncode\n```oops\n",
        ] {
            let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
            let code_block = parsed
                .tree
                .blocks()
                .find(|(_, node)| node.kind == NodeKind::CodeBlock)
                .unwrap_or_else(|| panic!("expected a code block in {source:?}"));
            assert_eq!(
                code_block.1.source_range,
                SourceRange::new(0, source.len()),
                "the unterminated fence should swallow the rest of the document: {source:?}"
            );
            let trimmed = source.trim_end_matches('\n');
            let last_line_start = trimmed.rfind('\n').map_or(0, |at| at + 1);
            assert!(
                parsed
                    .markers
                    .iter()
                    .all(|marker| marker.start.0 < last_line_start),
                "the closing-lookalike last line must not be derived as markup for {source:?}: {:?}",
                parsed.markers
            );
            assert!(
                parsed.markers.iter().any(|marker| marker.start.0 == 0),
                "the real opening fence should still be derived as markup for {source:?}"
            );
        }
    }

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
    fn local_index_sees_a_fence_that_opens_inside_the_lookback_window() {
        let mut source = String::from("intro\n```rust\n");
        source.push_str(&"inside\n".repeat(2_000));
        source.push_str("```\nafter\n");
        let buffer = RopeBuffer::from_text(&source);
        let local = local_block_index(&buffer, 1_900..1_910);
        let line = buffer.line_range(LineId(1_905)).unwrap();
        assert_eq!(
            local.block_at(line.start).map(|block| block.kind),
            Some(NodeKind::CodeBlock),
            "a line inside the fence resolves to the code block"
        );
        assert!(
            local
                .block_at(line.start)
                .is_some_and(|block| block.confidence == Confidence::Provisional),
            "every locally parsed block is provisional"
        );
    }

    #[test]
    fn local_index_resolves_gfm_pipe_tables() {
        let buffer = RopeBuffer::from_text(
            "before\n| Name | 値 |\n|:---|---:|\n| 羽 | 3 |\n| 鳥 | 4 |\n\nafter\n",
        );
        let local = local_block_index(&buffer, 0..buffer.line_count());
        let kind = |line: usize| {
            local
                .block_at(buffer.line_range(LineId(line)).unwrap().start)
                .map(|block| block.kind)
        };
        assert_eq!(kind(0), Some(NodeKind::Paragraph));
        for line in 1..=4 {
            assert_eq!(kind(line), Some(NodeKind::Table), "line {line} is table");
        }
        // The blank line closing the table belongs to the table block, the way
        // tiling assigns every blank run to the block above it.
        assert_eq!(kind(5), Some(NodeKind::Table));
        assert_eq!(kind(6), Some(NodeKind::Paragraph));
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
    fn quote_markers_follow_each_lines_actual_container_prefixes() {
        for (source, expected) in [
            (
                "> > first\n> > second\n",
                vec![(0, 2), (2, 4), (10, 12), (12, 14)],
            ),
            (">>first\n>>second\n", vec![(0, 1), (1, 2), (8, 9), (9, 10)]),
            (
                "  > > first\n >  >second\n",
                vec![(2, 4), (4, 6), (13, 15), (16, 17)],
            ),
            (
                ">\t>first\n>\t>second\n",
                vec![(0, 2), (2, 3), (9, 11), (11, 12)],
            ),
            (
                "> > first\n> lazy\nlazy too\n> > last\n",
                vec![(0, 2), (2, 4), (10, 12), (26, 28), (28, 30)],
            ),
            (
                "> > first\r\n> > second\r\n",
                vec![(0, 2), (2, 4), (11, 13), (13, 15)],
            ),
            (
                "> > 日本\n> > 語\n",
                vec![(0, 2), (2, 4), (11, 13), (13, 15)],
            ),
        ] {
            // A nonzero slice origin catches accidental mixing of local and
            // absolute positions, including on the first physical line.
            let base = 37;
            let parsed = parse_document(
                Revision(1),
                SourceRange::new(base, base + source.len()),
                source,
            );
            let expected: Vec<_> = expected
                .into_iter()
                .map(|(start, end)| SourceRange::new(base + start, base + end))
                .collect();
            assert_eq!(parsed.markers, expected, "source: {source:?}");
        }
    }

    #[test]
    fn quote_markers_follow_commonmark_line_endings_with_raw_offsets() {
        for newline in ["\r", "\n", "\r\n"] {
            for template in [
                "> 日本\n> 語\n",
                "> > first\n> > second\n",
                "intro\n\n- > first\n  > second\n",
                "intro\n\n> -\n>   > first\n>   > second\n",
            ] {
                let source = template.replace('\n', newline);
                let base = 37;
                let parsed = parse_document(
                    Revision(1),
                    SourceRange::new(base, base + source.len()),
                    &source,
                );
                let mut actual: Vec<_> = parsed.quote_markers.iter().map(|(r, _)| *r).collect();
                actual.sort_by_key(|r| r.start);
                let expected: Vec<_> = source
                    .match_indices('>')
                    .map(|(at, _)| SourceRange::new(base + at, base + at + 2))
                    .collect();
                assert_eq!(actual, expected, "source: {source:?}");
            }
        }
        let source = "> first\r> second\r\n> third\n> fourth";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        let expected: Vec<_> = source
            .match_indices('>')
            .map(|(at, _)| SourceRange::new(at, at + 2))
            .collect();
        assert_eq!(parsed.markers, expected);
    }

    #[test]
    fn markdown_line_scan_preserves_bytes_and_does_not_split_crlf() {
        let source = "日\r本\r\n語\nlast";
        assert_eq!(
            markdown_lines(source).collect::<Vec<_>>(),
            ["日\r", "本\r\n", "語\n", "last"]
        );
        assert_eq!(markdown_line_start(source, 4), 4);
        assert_eq!(markdown_line_start(source, 8), 4); // Inside CRLF.
        assert_eq!(markdown_line_start(source, 9), 9);
    }

    #[test]
    fn quote_markers_follow_list_ancestors_without_hiding_literal_greater_than() {
        for (source, expected) in [
            ("- > first\n  > second\n", vec![(2, 4), (12, 14)]),
            (
                "> - > first\n>   > second\n",
                vec![(0, 2), (4, 6), (12, 14), (16, 18)],
            ),
            ("1) > first\n   > second\n", vec![(3, 5), (14, 16)]),
            (
                "- > first\n  lazy > literal\n  > last\n",
                vec![(2, 4), (29, 31)],
            ),
            (
                "> ```\n> > literal\n> ```\n",
                vec![(0, 2), (6, 8), (18, 20)],
            ),
        ] {
            let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
            let actual: Vec<_> = parsed
                .tree
                .blocks()
                .filter(|(_, node)| node.kind == NodeKind::Quote)
                .flat_map(|(id, _)| quote_markers(&parsed.tree, id, parsed.source_range, source))
                .collect();
            let mut actual = actual;
            actual.sort_by_key(|marker| marker.start);
            let expected: Vec<_> = expected
                .into_iter()
                .map(|(start, end)| SourceRange::new(start, end))
                .collect();
            assert_eq!(actual, expected, "source: {source:?}");
        }
    }

    #[test]
    fn quote_markers_follow_list_items_with_an_empty_opening_line() {
        for marker in ["-", "1.", "1)"] {
            for padding in ["", " ", "   ", "\t"] {
                for newline in ["\n", "\r", "\r\n"] {
                    let indent = " ".repeat(marker.len() + 1);
                    let source = format!(
                        "{marker}{padding}{newline}{indent}> first{newline}{indent}> second"
                    );
                    let base = 37;
                    let parsed = parse_document(
                        Revision(1),
                        SourceRange::new(base, base + source.len()),
                        &source,
                    );
                    let (quote_id, quote) = parsed
                        .tree
                        .blocks()
                        .find(|(_, node)| node.kind == NodeKind::Quote)
                        .expect("the parser recognizes the nested quote");
                    let item_id = quote.parent.unwrap();
                    assert!(matches!(
                        parsed.tree.node(item_id).unwrap().kind,
                        NodeKind::ListItem { .. }
                    ));
                    // The item's own marker is unaffected by its opening line
                    // being empty: exactly one separator byte is markup when
                    // one exists in the source, none when the marker touches
                    // the line ending directly.
                    let separator = usize::from(!padding.is_empty());
                    let expected_item_marker =
                        SourceRange::new(base, base + marker.len() + separator);
                    assert_eq!(
                        parsed.list_item_markers,
                        vec![(expected_item_marker, item_id)],
                        "source: {source:?}"
                    );
                    assert!(parsed.markers.contains(&expected_item_marker));
                    let expected: Vec<_> = source
                        .match_indices('>')
                        .map(|(at, _)| SourceRange::new(base + at, base + at + 2))
                        .collect();
                    assert_eq!(
                        quote_markers(&parsed.tree, quote_id, parsed.source_range, &source),
                        expected,
                        "source: {source:?}"
                    );
                    for range in expected {
                        assert!(
                            parsed.markers.contains(&range),
                            "missing {range:?} in {source:?}"
                        );
                        assert!(parsed.quote_markers.contains(&(range, quote_id)));
                    }
                }
            }
        }
    }

    #[test]
    fn ordered_list_start_value_is_preserved_losslessly() {
        for (source, expected) in [
            ("- item\n", None),
            ("* item\n", None),
            ("+ item\n", None),
            ("1. item\n", Some(1)),
            ("0. item\n", Some(0)),
            ("5) item\n", Some(5)),
            // Leading zeros round-trip to their numeric value here; the
            // original digits are preserved separately in the marker's own
            // source range, not in this parsed value.
            ("007. item\n", Some(7)),
            ("123456789. item\n", Some(123_456_789)),
        ] {
            let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
            let (_, list) = parsed
                .tree
                .blocks()
                .find(|(_, node)| matches!(node.kind, NodeKind::List { .. }))
                .unwrap_or_else(|| panic!("no list parsed for {source:?}"));
            assert_eq!(
                list.kind,
                NodeKind::List { start: expected },
                "source: {source:?}"
            );
        }
    }

    #[test]
    fn ten_digit_ordered_marker_exceeds_commonmarks_limit_and_is_not_a_list() {
        let source = "1234567890. item\n";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        assert!(
            !parsed
                .tree
                .blocks()
                .any(|(_, node)| matches!(node.kind, NodeKind::List { .. })),
            "a 10-digit start exceeds CommonMark's ordered-list marker limit"
        );
    }

    #[test]
    fn four_space_indent_is_an_indented_code_block_not_a_list_item() {
        let source = "    - item\n";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        assert!(
            parsed
                .tree
                .blocks()
                .any(|(_, node)| node.kind == NodeKind::CodeBlock)
        );
        assert!(
            !parsed
                .tree
                .blocks()
                .any(|(_, node)| matches!(node.kind, NodeKind::ListItem { .. }))
        );
    }

    #[test]
    fn list_item_markers_cover_bullet_and_ordered_delimiters_exactly() {
        for (source, expected) in [
            ("- item\n", (0, 2)),
            ("* item\n", (0, 2)),
            ("+ item\n", (0, 2)),
            // A tab is as valid a separator as a single space.
            ("-\titem\n", (0, 2)),
            // An empty item whose marker touches the line ending has no
            // separator byte to include.
            ("-\n", (0, 1)),
            ("-\r\n", (0, 1)),
            // Trailing whitespace on an otherwise-empty opening line still
            // contributes exactly one separator byte, the same as content.
            ("-   \n", (0, 2)),
            ("1. item\n", (0, 3)),
            ("1) item\n", (0, 3)),
            ("0. item\n", (0, 3)),
            // Leading zeros are part of the marker's source bytes, not
            // reconstructed from the parsed start value.
            ("003) item\n", (0, 5)),
            // 0-3 leading spaces are part of the marker range at this
            // nesting level.
            ("  - item\n", (2, 4)),
            ("   - item\n", (3, 5)),
            // 5+ spaces after the marker still contribute only one
            // separator byte; the rest is the item's own indentation.
            ("-     item\n", (0, 2)),
            // Nine digits is CommonMark's maximum ordered-marker width.
            ("123456789. item\n", (0, 11)),
        ] {
            let base = 37;
            let parsed = parse_document(
                Revision(1),
                SourceRange::new(base, base + source.len()),
                source,
            );
            let (item_id, _) = parsed
                .tree
                .blocks()
                .find(|(_, node)| matches!(node.kind, NodeKind::ListItem { .. }))
                .unwrap_or_else(|| panic!("no list item parsed for {source:?}"));
            let expected = SourceRange::new(base + expected.0, base + expected.1);
            assert_eq!(
                parsed.list_item_markers,
                vec![(expected, item_id)],
                "source: {source:?}"
            );
            assert!(parsed.markers.contains(&expected), "source: {source:?}");
        }
    }

    #[test]
    fn list_item_ordinal_reports_owner_and_sibling_position() {
        let source = "- a\n- b\n  - nested a\n  - nested b\n- c\n";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        let (outer_list, _) = parsed
            .tree
            .blocks()
            .find(|(_, node)| matches!(node.kind, NodeKind::List { .. }))
            .expect("outer list");
        let outer_items = parsed.tree.children(outer_list).to_vec();
        assert_eq!(outer_items.len(), 3);
        for (position, item) in outer_items.iter().enumerate() {
            assert_eq!(
                parsed.tree.list_item_ordinal(*item),
                Some((outer_list, position))
            );
        }
        // The nested list's items are siblings of each other, not of the
        // outer list's items, even though they share a document.
        let nested_list = parsed
            .tree
            .children(outer_items[1])
            .iter()
            .copied()
            .find(|id| matches!(parsed.tree.node(*id).unwrap().kind, NodeKind::List { .. }))
            .expect("nested list under the second outer item");
        let nested_items = parsed.tree.children(nested_list).to_vec();
        assert_eq!(nested_items.len(), 2);
        for (position, item) in nested_items.iter().enumerate() {
            assert_eq!(
                parsed.tree.list_item_ordinal(*item),
                Some((nested_list, position))
            );
        }
        assert_eq!(parsed.tree.list_item_ordinal(outer_list), None);
    }

    #[test]
    fn local_index_agrees_with_the_formal_index_on_the_visible_window() {
        let buffer = RopeBuffer::from_text(
            "intro\n```rust\ncode\n```\n| Name | 値 |\n|:---|---:|\n| 羽 | 3 |\ntail\n",
        );
        let formal = BlockIndex::from_buffer(&buffer);
        let local = local_block_index(&buffer, 0..buffer.line_count());
        for line in 0..buffer.line_count() {
            let at = buffer.line_range(LineId(line)).unwrap().start;
            assert_eq!(
                local.block_at(at).map(|block| block.kind),
                formal.block_at(at).map(|block| block.kind),
                "line {line} resolves to the same block kind"
            );
        }
    }

    /// Every derived structural-prefix range must slice to whitespace only:
    /// it is markup-like padding, never a byte of actual content.
    fn assert_structural_prefixes_are_whitespace(
        source: &str,
        prefixes: &[(SourceRange, NodeId, usize)],
    ) {
        for (range, _, _) in prefixes {
            let slice = &source[range.start.0..range.end.0];
            assert!(
                slice.bytes().all(|byte| matches!(byte, b' ' | b'\t')),
                "prefix {range:?} is not whitespace in {source:?}: {slice:?}"
            );
        }
    }

    #[test]
    fn list_structural_prefixes_are_empty_for_a_lazy_paragraph_continuation() {
        // A paragraph continuation line may drop container indentation
        // entirely (CommonMark's lazy continuation); `line_break_padding`
        // already hides that shortfall as one undivided run, so this owned,
        // per-item mechanism must not also claim part of it.
        let source = "- foo\n bar\n";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        assert_eq!(parsed.list_structural_prefixes, Vec::new());
    }

    #[test]
    fn list_structural_prefixes_are_empty_for_an_empty_item() {
        let source = "-\n\n- next\n";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        assert_eq!(parsed.list_structural_prefixes, Vec::new());
    }

    #[test]
    fn list_structural_prefixes_cover_a_loose_items_second_and_third_paragraph() {
        let source = "- first\n\n  second\n\n  third\n";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        let (item, _) = parsed
            .tree
            .blocks()
            .find(|(_, node)| matches!(node.kind, NodeKind::ListItem { .. }))
            .expect("a list item");
        assert_eq!(
            parsed.list_structural_prefixes,
            vec![
                (SourceRange::new(9, 11), item, 2),
                (SourceRange::new(19, 21), item, 2),
            ]
        );
        assert_structural_prefixes_are_whitespace(source, &parsed.list_structural_prefixes);
    }

    #[test]
    fn list_structural_prefixes_split_ownership_for_a_nested_child_then_parent_paragraph() {
        // A nested/mixed fixture (#126): an outer item whose own directly-owned
        // content is interrupted by a nested child list with its own second
        // paragraph, followed by the outer item's own second paragraph.
        let source = "- outer\n\n  - inner\n\n    second\n\n  outer second\n\n- sibling";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        let lists = parsed
            .tree
            .blocks()
            .filter(|(_, node)| matches!(node.kind, NodeKind::List { .. }))
            .map(|(id, _)| id)
            .collect::<Vec<_>>();
        let outer_list = lists[0];
        let outer = parsed.tree.children(outer_list)[0];
        let nested_list = *parsed
            .tree
            .children(outer)
            .iter()
            .find(|id| matches!(parsed.tree.node(**id).unwrap().kind, NodeKind::List { .. }))
            .expect("nested list under the outer item");
        let inner = parsed.tree.children(nested_list)[0];

        // The outer item owns its own share of every line its content
        // continues onto, including the nested list's own opening line and
        // its own later paragraph, but not the nested item's own inner share
        // of "second"'s indentation.
        assert!(
            parsed
                .list_structural_prefixes
                .contains(&(SourceRange::new(9, 11), outer, 2))
        );
        assert!(
            parsed
                .list_structural_prefixes
                .contains(&(SourceRange::new(20, 22), outer, 2))
        );
        assert!(
            parsed
                .list_structural_prefixes
                .contains(&(SourceRange::new(32, 34), outer, 2))
        );
        // The nested item owns only the remainder of "second"'s indentation,
        // past the outer item's own share of the same physical line.
        assert!(
            parsed
                .list_structural_prefixes
                .contains(&(SourceRange::new(22, 24), inner, 2))
        );
        assert_structural_prefixes_are_whitespace(source, &parsed.list_structural_prefixes);
    }

    #[test]
    fn list_structural_prefixes_account_for_leading_spaces_and_marker_padding() {
        for (source, expected_width, continuation) in [
            // 0-3 leading spaces before the opening marker are owned by the
            // item too; the continuation width remains the full parsed prefix.
            ("  - item\n    continued\n", 4, (9, 13)),
            // 5+ spaces after the marker are one separator column, not a
            // wider required continuation indent.
            ("-     item\n  more\n", 2, (11, 13)),
        ] {
            let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
            let (item, _) = parsed
                .tree
                .blocks()
                .find(|(_, node)| matches!(node.kind, NodeKind::ListItem { .. }))
                .expect("a list item");
            let expected = if source.starts_with("  -") {
                vec![
                    (SourceRange::new(0, 2), item, 2),
                    (
                        SourceRange::new(continuation.0, continuation.1),
                        item,
                        expected_width,
                    ),
                ]
            } else {
                vec![(
                    SourceRange::new(continuation.0, continuation.1),
                    item,
                    expected_width,
                )]
            };
            assert_eq!(
                parsed.list_structural_prefixes, expected,
                "source: {source:?}"
            );
            assert_structural_prefixes_are_whitespace(source, &parsed.list_structural_prefixes);
        }
    }

    #[test]
    fn list_structural_prefixes_cover_opening_marker_padding_up_to_the_body() {
        for (source, expected_range, expected_columns) in [
            ("-   item\n", SourceRange::new(2, 4), 2),
            ("1.    item\n", SourceRange::new(3, 6), 3),
        ] {
            let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
            let (item, _) = parsed
                .tree
                .blocks()
                .find(|(_, node)| matches!(node.kind, NodeKind::ListItem { .. }))
                .expect("a list item");
            assert_eq!(
                parsed.list_structural_prefixes,
                vec![(expected_range, item, expected_columns)],
                "source: {source:?}"
            );
            assert_structural_prefixes_are_whitespace(source, &parsed.list_structural_prefixes);
        }

        // Five or more spaces are parser content indentation after the one
        // separator column, so the excess must remain visible.
        let source = "-     item\n";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        assert!(parsed.list_structural_prefixes.is_empty());
    }

    #[test]
    fn list_structural_prefixes_use_column_width_across_a_tab_and_a_two_digit_marker() {
        // A two-digit ordered marker ("10. ") is four columns wide; a single
        // leading tab on the continuation line reaches that width in one
        // byte, so the derived prefix's byte length and its column width
        // (what an inactive presentation synthesizes) legitimately differ.
        let source = "10. item\n\tcontinued\n";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        let (item, _) = parsed
            .tree
            .blocks()
            .find(|(_, node)| matches!(node.kind, NodeKind::ListItem { .. }))
            .expect("a list item");
        assert_eq!(
            parsed.list_structural_prefixes,
            vec![(SourceRange::new(9, 10), item, 4)]
        );
    }

    #[test]
    fn list_structural_prefixes_are_owned_separately_from_the_quote_prefix_sharing_a_line() {
        let source = "> - item\n>   continued\n";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        let (item, _) = parsed
            .tree
            .blocks()
            .find(|(_, node)| matches!(node.kind, NodeKind::ListItem { .. }))
            .expect("a list item");
        assert_eq!(
            parsed.list_structural_prefixes,
            vec![(SourceRange::new(11, 13), item, 2)]
        );
        // Immediately preceded by, and never overlapping, the quote's own
        // per-line prefix on the same continuation line.
        assert!(
            parsed
                .quote_markers
                .iter()
                .any(|(range, _)| *range == SourceRange::new(9, 11))
        );
    }

    #[test]
    fn list_structural_prefixes_cover_every_line_of_an_items_heading_and_fenced_code() {
        // Only the container indentation is markup; the fence delimiters and
        // code content stay literal (recovered separately by `derive_markers`
        // and left untouched here).
        let source = "- # Heading\n\n  ```\n  code\n  ```\n";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        let (item, _) = parsed
            .tree
            .blocks()
            .find(|(_, node)| matches!(node.kind, NodeKind::ListItem { .. }))
            .expect("a list item");
        assert_eq!(
            parsed.list_structural_prefixes,
            vec![
                (SourceRange::new(13, 15), item, 2),
                (SourceRange::new(19, 21), item, 2),
                (SourceRange::new(26, 28), item, 2),
            ]
        );
        assert_structural_prefixes_are_whitespace(source, &parsed.list_structural_prefixes);
    }

    #[test]
    fn list_structural_prefixes_follow_commonmark_line_endings_with_raw_offsets() {
        for newline in ["\n", "\r\n", "\r"] {
            let template = "- first\n\n  second\n";
            let source = template.replace('\n', newline);
            let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), &source);
            let (item, _) = parsed
                .tree
                .blocks()
                .find(|(_, node)| matches!(node.kind, NodeKind::ListItem { .. }))
                .expect("a list item");
            let prefix_start = source.find("  second").expect("second paragraph");
            assert_eq!(
                parsed.list_structural_prefixes,
                vec![(SourceRange::new(prefix_start, prefix_start + 2), item, 2)],
                "newline: {newline:?}"
            );
        }
    }

    #[test]
    fn local_index_covers_only_the_scanned_window() {
        let mut source = String::from("head\n");
        source.push_str(&"body\n".repeat(4_000));
        let buffer = RopeBuffer::from_text(&source);
        let local = local_block_index(&buffer, 3_500..3_510);
        let visible = buffer.line_range(LineId(3_505)).unwrap();
        assert_eq!(
            local.block_at(visible.start).map(|block| block.kind),
            Some(NodeKind::Paragraph)
        );
        // Far above the lookback window is not parsed at all.
        assert!(local.window.start.0 > 0);
        assert_eq!(local.block_at(SourceOffset(0)), None);
    }
}
