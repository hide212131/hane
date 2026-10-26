use crate::{SourceMap, Visibility, VisualOffset, VisualRange};
use hane_document::{Revision, RevisionDelta, SourceOffset, SourceRange};
use hane_markdown::{BlockId, Confidence, IndexedBlock, NodeKind, TableAlignment, TableProjection};
use std::ops::Range;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StyleKind {
    Bold,
    Italic,
    Strikethrough,
    InlineCode,
    CodeBlock,
    Link,
    Image,
    Table,
    MarkedText,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StyleRun {
    pub visual_range: VisualRange,
    pub kind: StyleKind,
}

/// Source and visual geometry for one inactive table cell. The visual range is
/// a range into the row's concatenated visible text; pipes and grid decoration
/// never become editable visual offsets.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableCellDisplay {
    pub column: usize,
    pub source_range: SourceRange,
    pub visual_range: VisualRange,
    pub alignment: TableAlignment,
}

/// Presentation metadata for one physical table row. `header` is a display
/// role, not a Markdown parser kind, so the UI can style it without parsing
/// source text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TableRowDisplay {
    pub header: bool,
    pub column_count: usize,
    pub cells: Vec<TableCellDisplay>,
}

/// Render policy for one inline run, the same idea as [`BlockDisplay`] one level
/// down: the UI applies these flags and never matches on [`StyleKind`]. Flags are
/// unioned when several runs cover the same text, so overlapping constructs
/// compose without the UI knowing which ones exist.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct InlineDisplay {
    pub bold: bool,
    pub italic: bool,
    pub strikethrough: bool,
    pub monospace: bool,
    /// Paint the inline-code background behind the text.
    pub code_background: bool,
    pub underline: bool,
    /// Draw the text in the theme's link color.
    pub link_color: bool,
}

impl InlineDisplay {
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self {
            bold: self.bold || other.bold,
            italic: self.italic || other.italic,
            strikethrough: self.strikethrough || other.strikethrough,
            monospace: self.monospace || other.monospace,
            code_background: self.code_background || other.code_background,
            underline: self.underline || other.underline,
            link_color: self.link_color || other.link_color,
        }
    }

    /// Combined policy for every style covering one stretch of visual text.
    pub fn for_styles(styles: impl IntoIterator<Item = StyleKind>) -> Self {
        styles.into_iter().fold(Self::default(), |display, kind| {
            display.union(kind.display())
        })
    }
}

impl StyleKind {
    pub const fn display(self) -> InlineDisplay {
        let base = InlineDisplay {
            bold: false,
            italic: false,
            strikethrough: false,
            monospace: false,
            code_background: false,
            underline: false,
            link_color: false,
        };
        match self {
            Self::Bold => InlineDisplay { bold: true, ..base },
            Self::Italic => InlineDisplay {
                italic: true,
                ..base
            },
            Self::Strikethrough => InlineDisplay {
                strikethrough: true,
                ..base
            },
            Self::InlineCode => InlineDisplay {
                monospace: true,
                code_background: true,
                ..base
            },
            Self::CodeBlock => InlineDisplay {
                monospace: true,
                ..base
            },
            Self::Link => InlineDisplay {
                underline: true,
                link_color: true,
                ..base
            },
            Self::MarkedText => InlineDisplay {
                underline: true,
                ..base
            },
            // Images and tables are carried by the block-level display.
            Self::Image | Self::Table => base,
        }
    }
}

/// Block-level context one physical source line is presented in, derived from
/// the owning block's kind by [`block_line_context`]. Presentation owns the
/// resulting display kind and style runs, so the UI never re-derives fenced-code
/// or table styling from raw source.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LineContext {
    #[default]
    Normal,
    FencedCode,
    Table,
}

/// Display context for every physical line of a block.
///
/// Fenced code wins by construction: a line inside a code block is literal, so
/// its pipes are never re-read as table syntax. This is the single seam where
/// "which lines are literal" and "which lines are table syntax" is decided, and
/// it reads only the block kind the index published.
pub const fn block_line_context(kind: NodeKind) -> LineContext {
    syntax_display(kind).line_context
}

/// True when a top-level block's contiguous plain-text lines are parsed
/// together (`present_joined_run`) instead of one physical line at a time, so
/// CommonMark inline constructs — `**`/`_`/`` ` `` — that cross a soft line
/// break resolve the way a single whole-paragraph parse would.
///
/// A blockquote's or a list item's paragraph content presents
/// `LineContext::Normal` the same as a top-level paragraph, and its inline
/// content joins the same way; marker derivation gives every quoted physical
/// line its own `> ` marker so joining does not lose it, and a list item's own
/// bullet is already scoped to the one line it starts on. The caller that
/// supplies parsing context for a joined block (see [`crate::BlockWindow::lines`])
/// must reach this same set of kinds, or a marker whose match lies outside the
/// render window will not resolve.
pub const fn block_is_joinable(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Paragraph | NodeKind::Quote | NodeKind::List { .. }
    )
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BlockKind {
    #[default]
    Paragraph,
    Heading(u8),
    CodeBlock,
    Quote,
    ListItem,
    Rule,
    Image,
    TableRow,
    TableDelimiter,
    /// The one source line of a fenced code block shown as a rendered diagram
    /// that owns the diagram's height. The block's other construct lines stay
    /// in the block as [`BlockKind::CodeBlock`] rows of zero height; see
    /// [`crate::apply_diagram`].
    Diagram,
    /// A source line whose Markdown construct has no specialized presenter yet, or
    /// whose marker derivation failed to tile the source range. Rendered verbatim
    /// as plain text so the source is never lost through the raw-source fallback.
    Unsupported,
}

/// Opaque presentation identity for a semantic list. The value is local to a
/// parsed snapshot; consumers use it to group rows, never to inspect Markdown
/// syntax.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ListId(pub u64);

/// Opaque presentation identity for a list item. The value is local to a
/// parsed snapshot and is intentionally separate from [`ListId`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ListItemId(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListMarkerKind {
    Bullet,
    Ordered,
}

/// The semantic role of one physical list row. The opening row is the only
/// row that owns the item marker; every other role hangs from the same item's
/// body column. Paragraph ordinals are zero-based within the item's direct
/// paragraph content, so `1` is the second paragraph.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListRowRole {
    Opening,
    Continuation,
    LooseParagraph { ordinal: usize },
    ParentParagraphAfterNestedList { ordinal: usize },
}

/// Aggregate marker information for one semantic list. The width is measured
/// from the inactive display labels (`3. `, `10. `, `• `), never from source
/// digits such as `003)`. `marker_labels` retains every direct-child label so
/// layout can measure actual font widths instead of assuming that equal digit
/// counts have equal advances.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListAlignment {
    pub max_marker_label: String,
    pub max_marker_columns: usize,
    pub marker_labels: Arc<[String]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListOwnerMetadata {
    pub list_id: ListId,
    pub item_id: ListItemId,
    pub marker_kind: ListMarkerKind,
    pub start: Option<u64>,
    pub ordinal: usize,
    pub depth: usize,
    pub alignment: ListAlignment,
}

/// The marker a row currently displays. `visual_range` points at either the
/// synthesized inactive label or the disclosed source marker. In both cases
/// `anchor` is the real source marker start; no synthetic edit position is
/// introduced.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListMarkerMetadata {
    pub source_range: SourceRange,
    pub anchor: SourceOffset,
    pub visual_range: VisualRange,
    pub label: String,
    pub synthesized: bool,
    /// The item's confirmed task marker, when it has one.
    pub task: Option<ListTaskMetadata>,
}

/// The GFM task marker (`[ ]`, `[x]`, `[X]`) of the list item owning a row.
/// `marker_range` is always the real source triple, never a synthetic
/// position. `checkbox_visual_range` is `Some` only while the marker is
/// inactive, where it names the checkbox glyph inside the synthesized label;
/// once the marker is disclosed the source bytes are ordinary editable text
/// and there is no control to press.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ListTaskMetadata {
    pub marker_range: SourceRange,
    pub checked: bool,
    pub checkbox_visual_range: Option<VisualRange>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ListStructuralPrefixMetadata {
    pub item_id: ListItemId,
    pub source_range: SourceRange,
    pub columns: usize,
}

/// Presentation-level list policy for one physical line. This is deliberately
/// a render contract rather than a Markdown AST node, so layout can establish
/// hanging indents and marker columns without parsing source text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListRowMetadata {
    pub owner: ListOwnerMetadata,
    pub role: ListRowRole,
    pub marker: Option<ListMarkerMetadata>,
    pub structural_prefixes: Vec<ListStructuralPrefixMetadata>,
    /// Visual offset at which the item's body begins in the current inactive
    /// presentation. Phase 2 may replace this source-derived visual padding
    /// with absolute layout geometry while retaining the same semantic owner.
    pub body_visual_start: VisualOffset,
}

/// Formal quote context for one physical line. The depth comes from the
/// parser tree or the document-wide projection, never from counting `>` in
/// the source at render time. The UI uses it only for the quote inset/bar;
/// quote marker bytes remain ordinary source-map segments so disclosure and
/// editing can reveal them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QuoteRowMetadata {
    pub depth: usize,
    /// Source range of the deepest formal quote container owning this row.
    /// Unlike a physical line or marker range, this remains stable across
    /// soft-wrapped rows and distinguishes adjacent quote containers at the
    /// same depth.
    pub owner: SourceRange,
    /// Number of this row's formal quote prefixes that are visible as source
    /// bytes. Layout keeps semantic inset only for the remaining hidden quote
    /// depth, so disclosed prefixes do not consume the same space twice.
    pub disclosed_depth: usize,
}

impl ListRowMetadata {
    fn rebase(&mut self, deltas: &[RevisionDelta]) -> bool {
        if let Some(marker) = &mut self.marker {
            for delta in deltas {
                let Some(range) = delta.transform_range(marker.source_range) else {
                    return false;
                };
                marker.source_range = range;
                marker.anchor = range.start;
                if let Some(task) = &mut marker.task {
                    // A toggle-sized edit inside the triple can't be carried
                    // over: the control must be re-resolved from a new parse.
                    let Some(range) = delta.transform_range(task.marker_range) else {
                        return false;
                    };
                    task.marker_range = range;
                }
            }
        }
        for prefix in &mut self.structural_prefixes {
            for delta in deltas {
                let Some(range) = delta.transform_range(prefix.source_range) else {
                    return false;
                };
                prefix.source_range = range;
            }
        }
        true
    }
}

/// Block-level font weight the UI must apply.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BlockWeight {
    #[default]
    Normal,
    Semibold,
}

/// Background fill role for a block. A role, not a color: the UI theme picks the
/// concrete value, so a new construct never forces a new UI branch.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BlockSurface {
    #[default]
    Default,
    Code,
    Table,
    Media,
}

/// Foreground role for a block.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BlockTint {
    #[default]
    Default,
    Muted,
}

/// Everything the UI needs to draw a block, expressed as roles and ratios rather
/// than Markdown kinds or theme colors.
///
/// This is the third type in the R3.25 split: `hane_markdown::NodeKind` is
/// syntax, [`BlockKind`] is the display kind presentation decides, and
/// `BlockDisplay` is the render policy the UI applies verbatim. Adding a Markdown
/// construct means giving it a `BlockDisplay` here; the UI crate does not change.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlockDisplay {
    /// Multiplier on the UI's body text size.
    pub font_scale: f32,
    pub weight: BlockWeight,
    pub surface: BlockSurface,
    pub tint: BlockTint,
    /// Whether the whole block is set in the monospace family.
    pub monospace: bool,
}

impl Default for BlockDisplay {
    fn default() -> Self {
        Self {
            font_scale: 1.0,
            weight: BlockWeight::Normal,
            surface: BlockSurface::Default,
            tint: BlockTint::Default,
            monospace: false,
        }
    }
}

impl BlockKind {
    /// The render policy for this display kind. Heading scales are relative to
    /// body text; they mirror the pixel sizes the UI used before the split.
    pub fn display(self) -> BlockDisplay {
        let heading = |scale| BlockDisplay {
            font_scale: scale,
            weight: BlockWeight::Semibold,
            ..BlockDisplay::default()
        };
        match self {
            Self::Heading(1) => heading(24.0 / 14.0),
            Self::Heading(2) => heading(21.0 / 14.0),
            Self::Heading(3) => heading(18.0 / 14.0),
            Self::Heading(_) => heading(16.0 / 14.0),
            Self::CodeBlock => BlockDisplay {
                surface: BlockSurface::Code,
                monospace: true,
                ..BlockDisplay::default()
            },
            Self::Quote => BlockDisplay {
                tint: BlockTint::Muted,
                ..BlockDisplay::default()
            },
            Self::TableRow => BlockDisplay {
                surface: BlockSurface::Table,
                ..BlockDisplay::default()
            },
            Self::Image | Self::Diagram => BlockDisplay {
                surface: BlockSurface::Media,
                ..BlockDisplay::default()
            },
            Self::Paragraph
            | Self::ListItem
            | Self::Rule
            | Self::TableDelimiter
            | Self::Unsupported => BlockDisplay::default(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImagePresentation {
    pub alt: String,
    pub destination: String,
}

/// An openable external link on one presented line. `source_range` is the part
/// of the link text that lies on this physical line, so a click resolved to a
/// source offset by the existing source map can be matched without re-reading
/// the Markdown.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LinkTarget {
    pub source_range: SourceRange,
    pub destination: Arc<str>,
    /// Whether the destination comes from a reference definition, which an edit
    /// outside this link's block can change.
    pub uses_reference: bool,
}

impl LinkTarget {
    /// Whether a click that resolved to `offset` lands on the link text.
    pub fn contains(&self, offset: SourceOffset) -> bool {
        self.source_range.start <= offset && offset < self.source_range.end
    }
}

/// One physical source line, presented. In R4A this is the compatibility layer
/// inside a [`VisualBlock`]: cursor, selection and IME still address physical
/// lines while virtualization moved to blocks. R4B replaces it with a layout
/// line that can also carry a soft-wrapped fragment.
#[derive(Clone, Debug, PartialEq)]
pub struct VisualLine {
    /// Document line number this was presented from.
    pub line_id: u64,
    pub source_range: SourceRange,
    pub revision: Revision,
    pub visual_text: String,
    pub style_runs: Vec<StyleRun>,
    /// Openable external links whose text is on this line, in source order.
    pub links: Vec<LinkTarget>,
    pub kind: BlockKind,
    pub source_map: SourceMap,
    pub estimated_height: f32,
    pub measured_height: Option<f32>,
    pub invalid: bool,
    /// The block context this presentation was built from. Callers cache blocks
    /// and must rebuild when the document context index changes the answer, so
    /// the input is recorded here instead of being inferred back from `kind`.
    pub context: LineContext,
    /// Source range whose Markdown markers are currently disclosed.
    pub disclosure: Option<SourceRange>,
    /// Present only for an inactive standalone Markdown image. The UI resolves
    /// relative destinations against the document directory and loads only
    /// visible image blocks.
    pub image: Option<ImagePresentation>,
    /// Semantic list geometry policy for this physical line. `None` means the
    /// line is not owned by a list item (or was deliberately rendered through
    /// a literal/raw fallback).
    pub list: Option<ListRowMetadata>,
    /// Semantic quote context used by layout/UI to draw the inset and bar.
    pub quote: Option<QuoteRowMetadata>,
    /// Visual ranges of this line's formal quote markers, in source order.
    /// Layout uses these ranges to distinguish quote prefixes from other
    /// expanded container markers such as list indentation.
    pub quote_marker_visual_ranges: Vec<VisualRange>,
    /// Source ranges paired with [`Self::quote_marker_visual_ranges`].
    pub quote_marker_source_ranges: Vec<SourceRange>,
    /// Cell-level metadata for an inactive structured table row.
    pub table_row: Option<TableRowDisplay>,
    /// Whether this line is a projected table header while its raw Markdown
    /// presentation is disclosed for editing.
    pub table_header: bool,
}

impl VisualLine {
    pub fn height(&self) -> f32 {
        self.measured_height.unwrap_or(self.estimated_height)
    }

    /// Whether a thematic-break body is collapsed to the separator drawn by
    /// the UI. Container prefixes (for example `> `) may be disclosed while
    /// the rule body remains hidden, so `visual_text.is_empty()` is not a
    /// sufficient test for selecting the separator renderer.
    pub fn rule_body_is_collapsed(&self) -> bool {
        self.kind == BlockKind::Rule
            && self
                .source_map
                .segments
                .iter()
                .rev()
                .find(|segment| !segment.source_range.is_empty())
                .is_some_and(|segment| segment.visibility == Visibility::HiddenMarkup)
    }

    /// Render policy for this line. The UI draws from this alone and never
    /// matches on [`BlockKind`].
    pub fn display(&self) -> BlockDisplay {
        self.kind.display()
    }

    pub fn rebase(&mut self, deltas: &[RevisionDelta], current: Revision) -> bool {
        let mut range = self.source_range;
        for delta in deltas {
            let Some(next) = delta.transform_range(range) else {
                return false;
            };
            range = next;
            for segment in &mut self.source_map.segments {
                let Some(rebased) = delta.transform_range(segment.source_range) else {
                    return false;
                };
                segment.source_range = rebased;
            }
            if let Some(disclosure) = self.disclosure {
                let Some(rebased) = delta.transform_range(disclosure) else {
                    return false;
                };
                self.disclosure = Some(rebased);
            }
        }
        for link in &mut self.links {
            for delta in deltas {
                let Some(rebased) = delta.transform_range(link.source_range) else {
                    return false;
                };
                link.source_range = rebased;
            }
        }
        if let Some(list) = &mut self.list
            && !list.rebase(deltas)
        {
            return false;
        }
        if let Some(table) = &mut self.table_row {
            for cell in &mut table.cells {
                for delta in deltas {
                    let Some(rebased) = delta.transform_range(cell.source_range) else {
                        return false;
                    };
                    cell.source_range = rebased;
                }
            }
        }
        self.source_range = range;
        self.revision = current;
        true
    }
}

/// One Markdown block as the renderer sees it, and the unit of virtualization.
///
/// A block spans every physical source line of its construct — a fenced code
/// block including both fences, a table including its delimiter row, a paragraph
/// including its continuation lines — plus the blank run that block tiling folds
/// into it. Its [`VisualLine`]s are the R4A compatibility layer: element
/// generation, height accounting and scrolling are driven by blocks, while
/// caret, selection and IME still address physical lines.
///
/// A block has no size limit — a document with no blank line in it is one
/// paragraph — so only the lines that reach the viewport are presented. The
/// clipped lines are counted, not built, and stand in as plain line-height space
/// above and below.
#[derive(Clone, Debug, PartialEq)]
pub struct VisualBlock {
    /// Stable id from the block index; the cache key that survives typing.
    pub id: BlockId,
    pub kind: BlockKind,
    /// Spans the whole construct, so it usually covers several lines.
    pub source_range: SourceRange,
    pub revision: Revision,
    pub confidence: Confidence,
    /// Document lines the whole block covers, presented or not.
    pub span: Range<usize>,
    /// The presented run of lines, a contiguous slice of `span`.
    pub lines: Vec<VisualLine>,
    /// Formal table source metadata shared by every viewport of this block.
    /// `None` is retained for provisional/local presentations, which fall back
    /// to the rows they have in hand.
    pub table_projection: Option<TableProjection>,
    /// Lines of `span` clipped above and below the presented run.
    pub lines_before: usize,
    pub lines_after: usize,
    /// Clipped structural rows whose inactive presentation has zero height.
    /// These remain part of the physical-line counts above so coverage and
    /// source-line mapping stay exact while their layout footprint disappears.
    pub zero_height_lines_before: usize,
    pub zero_height_lines_after: usize,
    /// Height a non-collapsed clipped line stands in for.
    pub line_height: f32,
}

impl VisualBlock {
    /// Height of the whole block: what was presented, plus the non-collapsed
    /// clipped lines standing in above and below it.
    pub fn height(&self) -> f32 {
        self.leading_space()
            + self.trailing_space()
            + self.lines.iter().map(VisualLine::height).sum::<f32>()
    }

    /// Space to leave above the presented lines, inside the block.
    pub fn leading_space(&self) -> f32 {
        self.lines_before
            .saturating_sub(self.zero_height_lines_before) as f32
            * self.line_height
    }

    /// Space to leave below the presented lines, inside the block.
    pub fn trailing_space(&self) -> f32 {
        self.lines_after
            .saturating_sub(self.zero_height_lines_after) as f32
            * self.line_height
    }

    /// Render policy for the block. As with a line, the UI applies this and never
    /// matches on [`BlockKind`].
    pub fn display(&self) -> BlockDisplay {
        self.kind.display()
    }

    /// True when this presentation still describes what the index now says about
    /// the block: same construct, same span, same confidence. A cached block that
    /// fails this has to be presented again.
    pub fn matches(&self, block: &IndexedBlock) -> bool {
        self.id == block.id
            && self.source_range == block.source_range
            && self.confidence == block.confidence
            && self.kind == block_display_kind(block.kind)
    }

    /// True when the presented run already covers `lines`, so a cached block can
    /// be drawn for that viewport without presenting anything again.
    pub fn covers(&self, lines: &Range<usize>) -> bool {
        let presented = self.span.start + self.lines_before
            ..self.span.start + self.lines_before + self.lines.len();
        let wanted = lines.start.max(self.span.start)..lines.end.min(self.span.end);
        wanted.is_empty() || (presented.start <= wanted.start && wanted.end <= presented.end)
    }

    /// Moves the block and every line in it onto `current`. Returns false when a
    /// delta cannot be transformed, which is the caller's signal to re-present
    /// rather than to display a block whose mapping no longer holds.
    pub fn rebase(&mut self, deltas: &[RevisionDelta], current: Revision) -> bool {
        // Formal table rows retain source text from the parsed revision. Do not
        // carry stale off-screen intrinsic metrics across an edit to the table.
        if self.table_projection.is_some() && !deltas.is_empty() {
            return false;
        }
        let mut range = self.source_range;
        for delta in deltas {
            let Some(next) = delta.transform_range(range) else {
                return false;
            };
            range = next;
        }
        if !self
            .lines
            .iter_mut()
            .all(|line| line.rebase(deltas, current))
        {
            return false;
        }
        self.source_range = range;
        self.revision = current;
        true
    }
}

/// Display kind for a whole block. The syntax-display table distinguishes a
/// container node (a table, a list) that decides its block appearance from a
/// node that contributes a display kind while presenting one physical line.
pub(super) fn block_display_kind(kind: NodeKind) -> BlockKind {
    syntax_display(kind).indexed_block
}

/// Maps a parser syntax kind to the display kind for a block. Returning `None`
/// means "this node does not decide how the block looks" — either it is a
/// structural container (list, table) or a construct with no presenter yet, in
/// which case the raw-source fallback applies. This function is the single seam
/// between the parser vocabulary and the display vocabulary.
#[derive(Clone, Copy)]
pub(super) struct SyntaxDisplay {
    /// The type a top-level indexed block displays as. Containers may choose a
    /// display even when they do not represent a presentable tree node.
    indexed_block: BlockKind,
    /// The type a presentable tree node contributes to a line.
    pub(super) node_block: Option<BlockKind>,
    pub(super) inline_style: Option<StyleKind>,
    line_context: LineContext,
}

pub(super) const fn syntax_display(kind: NodeKind) -> SyntaxDisplay {
    let default = SyntaxDisplay {
        indexed_block: BlockKind::Unsupported,
        node_block: None,
        inline_style: None,
        line_context: LineContext::Normal,
    };
    match kind {
        NodeKind::Paragraph => SyntaxDisplay {
            indexed_block: BlockKind::Paragraph,
            node_block: Some(BlockKind::Paragraph),
            ..default
        },
        NodeKind::Heading(level) => SyntaxDisplay {
            indexed_block: BlockKind::Heading(level),
            node_block: Some(BlockKind::Heading(level)),
            ..default
        },
        NodeKind::CodeBlock => SyntaxDisplay {
            indexed_block: BlockKind::CodeBlock,
            node_block: Some(BlockKind::CodeBlock),
            inline_style: Some(StyleKind::CodeBlock),
            line_context: LineContext::FencedCode,
        },
        NodeKind::Quote => SyntaxDisplay {
            indexed_block: BlockKind::Quote,
            node_block: Some(BlockKind::Quote),
            ..default
        },
        NodeKind::List { .. } => SyntaxDisplay {
            indexed_block: BlockKind::ListItem,
            ..default
        },
        NodeKind::ListItem { .. } => SyntaxDisplay {
            indexed_block: BlockKind::ListItem,
            node_block: Some(BlockKind::ListItem),
            ..default
        },
        NodeKind::Table | NodeKind::TableHead | NodeKind::TableRow => SyntaxDisplay {
            indexed_block: BlockKind::TableRow,
            line_context: LineContext::Table,
            ..default
        },
        NodeKind::Rule => SyntaxDisplay {
            indexed_block: BlockKind::Rule,
            node_block: Some(BlockKind::Rule),
            ..default
        },
        NodeKind::Strong => SyntaxDisplay {
            inline_style: Some(StyleKind::Bold),
            ..default
        },
        NodeKind::Emphasis => SyntaxDisplay {
            inline_style: Some(StyleKind::Italic),
            ..default
        },
        NodeKind::Strikethrough => SyntaxDisplay {
            inline_style: Some(StyleKind::Strikethrough),
            ..default
        },
        NodeKind::InlineCode => SyntaxDisplay {
            inline_style: Some(StyleKind::InlineCode),
            ..default
        },
        NodeKind::Link => SyntaxDisplay {
            inline_style: Some(StyleKind::Link),
            ..default
        },
        _ => default,
    }
}

pub(super) fn estimated_height(kind: BlockKind, line_height: f32) -> f32 {
    const BASE_LINE_HEIGHT: f32 = 26.0;
    const IMAGE_ROW_HEIGHT_AT_BASE_ZOOM: f32 = 190.0;

    match kind {
        BlockKind::Heading(1) => line_height * 1.65,
        BlockKind::Heading(2) => line_height * 1.45,
        BlockKind::Heading(3) => line_height * 1.25,
        BlockKind::Heading(_) => line_height * 1.1,
        BlockKind::CodeBlock => line_height,
        BlockKind::Image => IMAGE_ROW_HEIGHT_AT_BASE_ZOOM * line_height / BASE_LINE_HEIGHT,
        BlockKind::TableDelimiter => 0.0,
        _ => line_height,
    }
}

/// Height of one fenced-code row when its presentation is not collapsed.
/// Height-index adjustments use this same policy to remove a disclosed fence
/// from an already measured block without disturbing the measured content rows
/// that remain visible.
pub fn code_line_height(line_height: f32) -> f32 {
    estimated_height(BlockKind::CodeBlock, line_height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_and_fenced_code_keep_distinct_inline_background_policies() {
        let inline = StyleKind::InlineCode.display();
        assert!(inline.monospace);
        assert!(inline.code_background);

        let fenced = StyleKind::CodeBlock.display();
        assert!(fenced.monospace);
        assert!(!fenced.code_background);
    }

    #[test]
    fn fenced_code_rows_use_the_normal_line_height() {
        assert_eq!(code_line_height(26.0), 26.0);
    }
}
