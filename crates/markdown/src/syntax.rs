use crate::FenceMarkerEdge;
use hane_document::{Revision, SourceRange};
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

/// Alignment declared by a GFM pipe-table delimiter row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TableAlignment {
    Default,
    Left,
    Center,
    Right,
}

/// Table-level parser metadata kept beside the syntax tree so the alignment
/// vector from pulldown-cmark is not lost when `Tag::Table` becomes `NodeKind`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableParse {
    pub node: NodeId,
    pub alignments: Arc<[TableAlignment]>,
}

/// An openable Markdown link: an inline or reference link whose destination is
/// an absolute HTTP(S) URL. Only these are kept, so a consumer never has to
/// re-judge the scheme; images, relative paths, `file:` and other schemes never
/// appear here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LinkParse {
    pub node: NodeId,
    /// Source range of the link text (first to last child), excluding the
    /// brackets and the destination part.
    pub text_range: SourceRange,
    pub destination: Arc<str>,
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
    pub(super) nodes: Vec<MarkdownNode>,
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

/// The contiguous run of `links` whose text range intersects `range`.
///
/// `links` is in document order and link text never nests or overlaps, so
/// both starts and ends are sorted; two binary searches bound the run without
/// visiting links outside it. A link ending exactly at `range.start` or
/// starting exactly at `range.end` does not intersect.
pub fn links_intersecting(links: &[LinkParse], range: SourceRange) -> &[LinkParse] {
    let first = links.partition_point(|link| link.text_range.end <= range.start);
    let last = links.partition_point(|link| link.text_range.start < range.end);
    if first < last {
        &links[first..last]
    } else {
        &[]
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownParse {
    pub revision: Revision,
    pub source_range: SourceRange,
    pub tree: MarkdownTree,
    /// Alignment metadata for each parsed table node, keyed by its tree node.
    pub table_parses: Vec<TableParse>,
    /// Openable external links in document order, keyed by their tree node.
    pub links: Vec<LinkParse>,
    /// Sorted, non-overlapping source ranges of the syntactic markers (heading
    /// hashes, quote/list prefixes, fence delimiters, emphasis/code delimiters,
    /// link brackets). Derived here so presentation and UI never re-lex markup.
    pub markers: Vec<SourceRange>,
    /// Fence delimiter ranges kept separately from the merged marker plan so a
    /// formal list projection can preserve them while filtering unrelated
    /// inline markers from literal code content.
    pub fence_markers: Vec<SourceRange>,
    /// The formal opening/closing role for each fence marker. Kept separate
    /// from `fence_markers` so range-only consumers remain unchanged.
    pub fence_marker_edges: Vec<(SourceRange, FenceMarkerEdge)>,
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
