//! Visual blocks and lines, source mapping, and variable-height virtualization.

// Presentation query values are frequently inspected only while composing a frame.
#![allow(
    clippy::must_use_candidate,
    reason = "frame-composition query APIs are intentionally discardable"
)]
#![allow(
    clippy::missing_panics_doc,
    reason = "layout invariant panics are documented by their enforcing assertion"
)]
#![allow(
    clippy::doc_markdown,
    reason = "rendering documentation uses established Markdown terminology as prose"
)]
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "layout coordinates intentionally convert bounded counts between pixel and index representations"
)]
#![allow(
    clippy::float_cmp,
    reason = "layout tests and exact zero-width checks require deterministic float equality"
)]
#![allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "the trait implementation signature follows its related non-Copy API"
)]
#![allow(
    clippy::needless_pass_by_value,
    reason = "the layout API owns its input to keep call sites uniform"
)]
#![allow(
    clippy::struct_excessive_bools,
    reason = "the display policy mirrors independent Markdown presentation flags"
)]

mod layout;
pub mod testing;

pub use hane_markdown::TableAlignment;

pub use layout::{
    BlockLayout, LIST_DEPTH_INDENT, LayoutLine, LayoutPoint, LineShaper, LineWrap,
    QUOTE_BAR_GAP, QUOTE_BAR_WIDTH, QUOTE_DEPTH_INDENT, VerticalMove, layout_block,
    line_visual_start, TableCellLayout,
};

use hane_document::{
    Bias, Revision, RevisionDelta, RopeBuffer, SourceOffset, SourceRange, TextBuffer,
};
use hane_markdown::{
    BlockId, BlockIndex, Confidence, FenceDelimiter, FenceMarkerEdge, IndexedBlock, ListProjection,
    ListProjectionItem, ListProjectionPrefix, MarkdownNode, MarkdownParse, MarkdownTree, NodeId,
    NodeKind, TableProjection, fence_closes, fence_closing_delimiter, fence_delimiter,
    has_delimiter_markers, is_table_delimiter,
    parse_document,
};
use std::ops::Range;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub struct VisualOffset(pub usize);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub struct VisualRange {
    pub start: VisualOffset,
    pub end: VisualOffset,
}

impl VisualRange {
    pub const fn new(start: usize, end: usize) -> Self {
        Self {
            start: VisualOffset(start),
            end: VisualOffset(end),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Visibility {
    Visible,
    HiddenMarkup,
    Synthesized,
    ExpandedMarkup,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BoundarySide {
    Leading,
    Trailing,
}

/// Which end of a delimited construct (e.g. `**bold**`, `` `code` ``) a
/// [`Visibility::HiddenMarkup`] segment's marker sits at. A collapsed
/// zero-visual-width marker shares its visual position with the visible
/// content on one side and unrelated content on the other; only the parse
/// tree that produced the marker knows which side is which, so this is
/// resolved once here rather than guessed from position at click time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkerEdge {
    Opening,
    Closing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MappingSegment {
    pub source_range: SourceRange,
    pub visual_range: VisualRange,
    pub visibility: Visibility,
    /// `Some` only for a [`Visibility::HiddenMarkup`] segment whose marker
    /// opens or closes a delimited construct (bold, italic, inline code,
    /// links); `None` for plain visible text and for markup with no
    /// meaningful side (e.g. a quote/list prefix, a table pipe).
    pub marker_edge: Option<MarkerEdge>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PositionCandidate {
    pub source_offset: SourceOffset,
    pub visual_offset: VisualOffset,
    pub affinity: Bias,
    pub side: BoundarySide,
    pub visibility: Visibility,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SourceMap {
    pub segments: Vec<MappingSegment>,
}

impl SourceMap {
    pub fn visual_to_source(
        &self,
        visual: VisualOffset,
        affinity: Bias,
    ) -> Option<PositionCandidate> {
        let candidates = self.segments.iter().filter_map(|segment| {
            if visual.0 < segment.visual_range.start.0 || visual.0 > segment.visual_range.end.0 {
                return None;
            }
            let source = match segment.visibility {
                Visibility::Visible | Visibility::ExpandedMarkup => {
                    let delta = visual.0.saturating_sub(segment.visual_range.start.0);
                    SourceOffset(
                        (segment.source_range.start.0 + delta).min(segment.source_range.end.0),
                    )
                }
                Visibility::HiddenMarkup => match affinity {
                    Bias::Before => segment.source_range.start,
                    Bias::After => segment.source_range.end,
                },
                Visibility::Synthesized => segment.source_range.start,
            };
            Some(PositionCandidate {
                source_offset: source,
                visual_offset: visual,
                affinity,
                side: if affinity == Bias::Before {
                    BoundarySide::Leading
                } else {
                    BoundarySide::Trailing
                },
                visibility: segment.visibility,
            })
        });
        let mut candidates = candidates.collect::<Vec<_>>();
        candidates.sort_by_key(|candidate| candidate.source_offset);
        let visible = candidates
            .iter()
            .filter(|candidate| {
                matches!(
                    candidate.visibility,
                    Visibility::Visible | Visibility::ExpandedMarkup
                )
            })
            .copied()
            .collect::<Vec<_>>();
        match affinity {
            Bias::Before => visible
                .first()
                .copied()
                .or_else(|| candidates.first().copied()),
            Bias::After => visible
                .last()
                .copied()
                .or_else(|| candidates.last().copied()),
        }
    }

    pub fn source_to_visual(
        &self,
        source: SourceOffset,
        affinity: Bias,
    ) -> Option<PositionCandidate> {
        let mut candidates = self
            .segments
            .iter()
            .filter_map(|segment| {
                if source.0 < segment.source_range.start.0 || source.0 > segment.source_range.end.0
                {
                    return None;
                }
                let visual = match segment.visibility {
                    Visibility::Visible | Visibility::ExpandedMarkup => VisualOffset(
                        segment.visual_range.start.0
                            + source.0.saturating_sub(segment.source_range.start.0),
                    ),
                    Visibility::HiddenMarkup => match affinity {
                        Bias::Before => segment.visual_range.start,
                        Bias::After => segment.visual_range.end,
                    },
                    Visibility::Synthesized => segment.visual_range.start,
                };
                Some(PositionCandidate {
                    source_offset: source,
                    visual_offset: visual,
                    affinity,
                    side: if affinity == Bias::Before {
                        BoundarySide::Leading
                    } else {
                        BoundarySide::Trailing
                    },
                    visibility: segment.visibility,
                })
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|candidate| candidate.visual_offset);
        match affinity {
            Bias::Before => candidates.first().copied(),
            Bias::After => candidates.last().copied(),
        }
    }

    /// Hidden and synthesized segments can sit back-to-back with no visible
    /// content between them (e.g. a list marker's synthesized bullet
    /// immediately followed by a nested ATX heading's hidden marker). A
    /// single source→visual→source round trip only walks to the next such
    /// boundary rather than settling on an editable position (ADR-0004), so
    /// `next` can differ from `current` and itself still not be a fixed
    /// point. Repeating the round trip walks the whole chain; each affinity
    /// only ever advances toward candidates sorted later (`After`) or
    /// earlier (`Before`) for that same affinity, so the source offset it
    /// produces is monotonic and bounded, and it stabilizes in at most
    /// `segments.len()` steps.
    pub fn normalize_source(&self, source: SourceOffset, affinity: Bias) -> Option<SourceOffset> {
        let mut current = source;
        for _ in 0..=self.segments.len() {
            let visual = self.source_to_visual(current, affinity)?.visual_offset;
            let next = self.visual_to_source(visual, affinity)?.source_offset;
            if next == current {
                return Some(current);
            }
            current = next;
        }
        Some(current)
    }

    /// Mirrors [`Self::normalize_source`]'s chained-boundary walk in the
    /// visual direction.
    pub fn normalize_visual(&self, visual: VisualOffset, affinity: Bias) -> Option<VisualOffset> {
        let mut current = visual;
        for _ in 0..=self.segments.len() {
            let source = self.visual_to_source(current, affinity)?.source_offset;
            let next = self.source_to_visual(source, affinity)?.visual_offset;
            if next == current {
                return Some(current);
            }
            current = next;
        }
        Some(current)
    }
}

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
/// supplies parsing context for a joined block (see [`BlockWindow::lines`])
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

/// The horizontal column where a list-aware newline places the next caret.
/// This is an editing hint only; it does not add source bytes or visual text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ListCaretOrigin {
    /// Start at the semantic marker column so the next source bytes can form a
    /// sibling or child marker.
    Marker,
    /// Start at the list item's body column for an explicit body continuation.
    Body,
}

/// Transient presentation context for the empty line created by a list-aware
/// newline. The document remains the source of truth: this context only
/// carries the existing semantic owner into the empty line's layout.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListEditingContext {
    pub offset: SourceOffset,
    pub owner: ListOwnerMetadata,
    pub caret_origin: ListCaretOrigin,
    /// Bytes owned by the physical line that is empty except for an existing
    /// line ending. A newline inserted before that ending creates a new line
    /// whose source range is non-empty even though its editable content is
    /// empty. Zero means the new line is the document's zero-length final
    /// line.
    pub line_ending_len: usize,
    /// Source indentation before the current item's marker. It is inserted
    /// ahead of the first text typed on the new line, so Enter itself does not
    /// commit a Markdown structure before the user chooses a marker or text.
    pub indentation: String,
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
    /// Set only on the empty line immediately created by a list-aware newline.
    /// It changes caret geometry, never the source map or visual text.
    pub empty_caret_origin: Option<ListCaretOrigin>,
    /// Source range containing the current item's leading container prefix.
    /// It is used by the editor to carry exact spaces/tabs into the next line.
    pub source_prefix: Option<SourceRange>,
}

/// Formal quote context for one physical line. The depth comes from the
/// parser tree or the document-wide projection, never from counting `>` in
/// the source at render time. The UI uses it only for the quote inset/bar;
/// quote marker bytes remain ordinary source-map segments so disclosure and
/// editing can reveal them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QuoteRowMetadata {
    pub depth: usize,
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
        if let Some(source_prefix) = &mut self.source_prefix {
            for delta in deltas {
                let Some(range) = delta.transform_range(*source_prefix) else {
                    return false;
                };
                *source_prefix = range;
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
            Self::Image => BlockDisplay {
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

/// Applies a list-aware caret position to the empty source line created by an
/// edit. The line keeps its original source map and empty visual text; only
/// the semantic owner and layout hint are carried across the newline.
pub fn apply_list_editing_context(block: &mut VisualBlock, context: &ListEditingContext) -> bool {
    let empty_line_range = SourceRange::new(
        context.offset.0,
        context.offset.0.saturating_add(context.line_ending_len),
    );
    let Some(line) = block
        .lines
        .iter_mut()
        .find(|line| line.source_range == empty_line_range)
    else {
        return false;
    };
    line.list = Some(ListRowMetadata {
        owner: context.owner.clone(),
        role: ListRowRole::Continuation,
        marker: None,
        structural_prefixes: Vec::new(),
        body_visual_start: VisualOffset(0),
        empty_caret_origin: Some(context.caret_origin),
        source_prefix: None,
    });
    true
}

/// Physical source lines a block covers.
///
/// A block ends where the next one begins, so its last byte is the newline of
/// its last line — except for the block that owns the document end, which also
/// owns the empty final line a trailing newline creates. That line holds no
/// bytes but can hold the caret, so it has to be drawn.
pub fn block_line_span(document: &RopeBuffer, block: &IndexedBlock) -> Option<Range<usize>> {
    let first = document.line_for_offset(block.source_range.start).ok()?;
    let last =
        if block.source_range.is_empty() || block.source_range.end.0 >= document.len_bytes().0 {
            document.line_for_offset(block.source_range.end).ok()?
        } else {
            document
                .line_for_offset(SourceOffset(block.source_range.end.0 - 1))
                .ok()?
        };
    Some(first.0..last.0 + 1)
}

/// Blank lines closing a block. Tiling folds the blank run between two blocks
/// into the block above, and those lines are not part of the construct. Walks
/// back from the block's end, so it reads the blank run and one line more.
pub fn trailing_blank_lines(document: &RopeBuffer, span: &Range<usize>) -> usize {
    span.clone()
        .rev()
        .take_while(|line| {
            document
                .line_range(hane_document::LineId(*line))
                .ok()
                .and_then(|range| document.text(range).ok())
                .is_none_or(|text| text.trim().is_empty())
        })
        .count()
}

/// Initial height of every block in the index, from the line height alone.
///
/// Seeds the [`HeightIndex`] at block granularity, and is re-run whenever the
/// block count changes, so it must not touch the rope: the index already counted
/// each block's lines while it tiled them, and this is arithmetic over those
/// counts. Measured heights replace these as blocks are drawn.
pub fn block_heights(document: &RopeBuffer, index: &BlockIndex, line_height: f32) -> Vec<f32> {
    block_heights_with_disclosure(document, index, line_height, None)
}

/// Initial block heights while respecting the editor's active disclosure.
///
/// The height projection is block-relative and cheap to query, so startup and
/// height-index rebuilds can keep the caret/selection/IME-owned fence row at
/// normal height without presenting the block first. This prevents a run of
/// otherwise zero-height fence blocks from disappearing from the initial
/// virtualization window before the editable row has a chance to render.
pub fn block_heights_with_disclosure(
    document: &RopeBuffer,
    index: &BlockIndex,
    line_height: f32,
    disclosure: Option<SourceRange>,
) -> Vec<f32> {
    let mut counted = 0;
    let mut heights = index
        .blocks()
        .map(|block| {
            counted += block.line_count;
            let block_is_final = block.ordinal + 1 == index.len();
            let collapsed = index
                .fence_height_projection(&block)
                .map_or(0, |projection| {
                    projection.inactive_rows_in(
                        block.source_range,
                        block.source_range,
                        disclosure,
                        block_is_final,
                    )
                });
            line_height * block.line_count.saturating_sub(collapsed) as f32
        })
        .collect::<Vec<_>>();
    // A document ending in a newline has one physical line more than its blocks
    // account for — the empty last line, which the block above owns because that
    // is where the caret goes.
    if let Some(last) = heights.last_mut() {
        let extra = document.line_count().saturating_sub(counted);
        *last += line_height * extra as f32;
    }
    heights
}

/// One physical source line handed to [`present_block`].
#[derive(Clone, Copy, Debug)]
pub struct BlockLine<'a> {
    /// Document line number. Becomes the presented line's [`VisualLine::line_id`].
    pub line: usize,
    pub range: SourceRange,
    pub text: &'a str,
    /// Source range whose Markdown markers this line currently discloses,
    /// resolved by the caller from caret, selection and IME state.
    pub disclosure: Option<SourceRange>,
}

/// Which lines of a block to present, and where they sit inside it.
#[derive(Clone, Debug)]
pub struct BlockWindow<'a> {
    /// Document lines the whole block covers.
    pub span: Range<usize>,
    /// Blank lines closing the block. Tiling folds the blank run between two
    /// blocks into the block above, and those lines are not part of the
    /// construct — a blank line after a closing fence is not code.
    pub trailing_blank_lines: usize,
    /// Lines available to build the [`VisualBlock`] from. For a joinable
    /// paragraph (see [`present_block`]) this may reach beyond `render` on
    /// either side, because CommonMark resolves `**`/`_`/`` ` `` across the
    /// whole paragraph, not just the lines that happen to be drawn; a marker
    /// whose other half sits outside `lines` still cannot be recognized, so
    /// the caller should give enough of the paragraph to close every marker
    /// that opens inside `render` — unless `joined` already supplies that
    /// context, in which case `lines` only needs to cover `render`.
    pub lines: &'a [BlockLine<'a>],
    /// Fence delimiter lines outside `lines` that still affect the block's
    /// inactive vertical footprint. They are height-accounting context only:
    /// never joined into paragraph/list parsing and never rendered directly.
    pub clipped_fence_lines: &'a [BlockLine<'a>],
    /// Inactive nested fence rows clipped above/below the render window.
    /// These are pre-counted from the formal projection so presentation does
    /// not materialize off-screen source rows merely to account for height.
    pub zero_height_fence_rows_before: usize,
    pub zero_height_fence_rows_after: usize,
    /// The subset of `lines` (by document line number) to actually turn into
    /// presented [`VisualLine`]s. Lines in `lines` outside this range are
    /// parsing context only and are not drawn.
    pub render: Range<usize>,
    /// A joinable block's whole-span parse, already computed. When present,
    /// a joined run is presented against this instead of rejoining and
    /// reparsing `lines`, so a caller that keeps this cached across
    /// presentation calls (see [`parse_joined_span`]) can hand `render`'s own
    /// lines alone as `lines` — a viewport miss on an already-parsed block
    /// then costs proportionally to what is drawn, not to the whole block,
    /// while still resolving a marker pair arbitrarily far apart the way a
    /// single whole-paragraph parse would, regardless of where the viewport
    /// happens to sit.
    pub joined: Option<&'a JoinedParse>,
    /// Caret, selection or IME range touching this block's whole span,
    /// computed by the caller directly from editor state rather than
    /// reconstructed from `lines`. A joined run's own [`merged_disclosure`]
    /// only sees the physical lines `lines` actually carries, which for a
    /// block presented from `render` alone (see [`JoinedParse`]) excludes any
    /// off-screen line of the same shared construct; this field is what lets
    /// a caret sitting on one of those off-screen lines still disclose the
    /// construct's markers on the lines that are drawn.
    pub block_disclosure: Option<SourceRange>,
}

/// Display kind for a whole block. The syntax-display table distinguishes a
/// container node (a table, a list) that decides its block appearance from a
/// node that contributes a display kind while presenting one physical line.
fn block_display_kind(kind: NodeKind) -> BlockKind {
    syntax_display(kind).indexed_block
}

/// Presents `window.render`'s lines of one indexed Markdown block.
///
/// Every line is presented in the context its block kind implies, except the
/// blank run closing the block. The trailing newline each line's visual text
/// keeps for source-map fidelity is trimmed here, because the renderer draws one
/// element per line.
pub fn present_block(
    block: &IndexedBlock,
    revision: Revision,
    window: &BlockWindow<'_>,
    line_height: f32,
) -> VisualBlock {
    present_block_with_list_projection(block, revision, window, line_height, None)
}

/// Presents a block with document-wide list context from a formal block index.
///
/// A block larger than the synchronous join budget is intentionally parsed only
/// for the visible lines. The compact projection lets those lines retain the
/// formal list's numbering, depth, and marker-column width until a whole-block
/// [`JoinedParse`] becomes available.
pub fn present_block_with_list_projection(
    block: &IndexedBlock,
    revision: Revision,
    window: &BlockWindow<'_>,
    line_height: f32,
    list_projection: Option<&ListProjection>,
) -> VisualBlock {
    present_block_with_table_projection(
        block,
        revision,
        window,
        line_height,
        list_projection,
        None,
    )
}

/// Presents a block with document-wide list and table metadata. The compact
/// table projection is optional so provisional/local callers can safely use
/// the bounded delimiter fallback.
pub fn present_block_with_table_projection(
    block: &IndexedBlock,
    revision: Revision,
    window: &BlockWindow<'_>,
    line_height: f32,
    list_projection: Option<&ListProjection>,
    table_projection: Option<&TableProjection>,
) -> VisualBlock {
    let context = block_line_context(block.kind);
    let content_end = window.span.end.saturating_sub(window.trailing_blank_lines);
    let local_table_delimiter_line = (context == LineContext::Table)
        .then(|| {
            window
                .lines
                .iter()
                .find(|line| is_table_delimiter(line.text))
                .map(|line| line.line)
        })
        .flatten();
    let table_delimiter_line = table_projection
        .and_then(|projection| projection.delimiter_range)
        .and_then(|delimiter| {
            window
                .lines
                .iter()
                .find(|line| line.range == delimiter)
                .map(|line| line.line)
        })
        .or(local_table_delimiter_line);
    let table_alignments = table_projection.map_or_else(
        || {
            table_delimiter_line
                .and_then(|delimiter| window.lines.iter().find(|line| line.line == delimiter))
                .map_or_else(Vec::new, |line| table_alignments(line.text))
        },
        |projection| projection.alignments.to_vec(),
    );
    // The opening fence's own marker shape, read from whichever of `window.lines`
    // carries the block's true first physical line — present regardless of
    // `window.render` (see `BlockWindow::lines`'s own "parsing context only"
    // contract) so a closing-fence candidate deep in a large block can be
    // validated without the caller ever materializing lines between the two.
    // The lowest-numbered line in `window.lines` is not always that line: block
    // ordinal 0's tiled span absorbs any blank run before the document's first
    // block, so a blank line can sit in `window.lines` ahead of the real
    // opening fence. Skipping blank lines here protects the opening metadata
    // from that prefix without re-reading the document. `None` for an
    // indented code block (no fence to derive at all) or when the caller could
    // not supply that line.
    let fence_opening = (context == LineContext::FencedCode)
        .then(|| {
            window
                .lines
                .iter()
                .filter(|line| !line.text.trim().is_empty())
                .min_by_key(|line| line.line)
        })
        .flatten()
        .and_then(|line| fence_delimiter(line.text).map(|delimiter| (line.line, delimiter)));
    let mut lines = Vec::with_capacity(window.render.len().min(window.lines.len()));
    for run in disclosure_runs(block.kind, window) {
        if run_uses_shared_parse(run.len(), window.joined) {
            present_joined_run_with_list_projection(
                &window.lines[run.clone()],
                revision,
                line_height,
                &window.render,
                window.joined,
                window.block_disclosure,
                list_projection,
                &mut lines,
            );
            continue;
        }
        let line = window.lines[run.start];
        if !window.render.contains(&line.line) {
            continue;
        }
        let line_context = if line.line < content_end {
            context
        } else {
            LineContext::Normal
        };
        let fence_role = (line_context == LineContext::FencedCode)
            .then(|| fence_line_role(line.line, content_end, line.text, fence_opening))
            .flatten();
        let table_header = table_projection
            .and_then(|projection| projection.delimiter_range)
            .is_some_and(|delimiter| line.range.end <= delimiter.start)
            || table_delimiter_line.is_some_and(|delimiter| line.line < delimiter);
        let mut presented = present_polished_line_with_fence(
            line.line as u64,
            revision,
            line.range,
            line.text,
            line_height,
            line.disclosure,
            line_context,
            list_projection,
            fence_role,
            table_header,
            &table_alignments,
        );
        while presented.visual_text.ends_with(['\r', '\n']) {
            presented.visual_text.pop();
        }
        lines.push(presented);
    }
    let lines_before = lines.first().map_or(0, |line| {
        (line.line_id as usize).saturating_sub(window.span.start)
    });
    let lines_after = window.span.len().saturating_sub(lines_before + lines.len());
    let first_presented_line = window.span.start.saturating_add(lines_before);
    let presented_end = first_presented_line.saturating_add(lines.len());
    let mut zero_height_lines_before = window.zero_height_fence_rows_before;
    let mut zero_height_lines_after = window.zero_height_fence_rows_after;
    let mut record_collapsed = |line: usize, collapsed: bool| {
        if !collapsed {
            return;
        }
        if line < first_presented_line {
            zero_height_lines_before += 1;
        } else if line >= presented_end {
            zero_height_lines_after += 1;
        }
    };
    if context == LineContext::FencedCode {
        for line in window.lines {
            let Some(fence_role) =
                fence_line_role(line.line, content_end, line.text, fence_opening)
            else {
                continue;
            };
            let collapsed = present_fenced_code_line(
                line.line as u64,
                revision,
                line.range,
                line.text,
                line_height,
                line.disclosure,
                Some(fence_role),
            )
            .height()
                == 0.0;
            record_collapsed(line.line, collapsed);
        }
    }
    for line in window.clipped_fence_lines {
        let collapsed = if context == LineContext::FencedCode {
            fence_line_role(line.line, content_end, line.text, fence_opening)
                .is_some_and(|fence_role| {
                    present_fenced_code_line(
                        line.line as u64,
                        revision,
                        line.range,
                        line.text,
                        line_height,
                        line.disclosure,
                        Some(fence_role),
                    )
                    .height()
                        == 0.0
                })
        } else {
            false
        };
        record_collapsed(line.line, collapsed);
    }
    VisualBlock {
        id: block.id,
        kind: block_display_kind(block.kind),
        source_range: block.source_range,
        revision,
        confidence: block.confidence,
        span: window.span.clone(),
        lines,
        table_projection: table_projection.cloned(),
        lines_before,
        lines_after,
        zero_height_lines_before,
        zero_height_lines_after,
        line_height,
    }
}

fn present_plain(line_id: u64, revision: Revision, range: SourceRange, source: &str) -> VisualLine {
    VisualLine {
        line_id,
        source_range: range,
        revision,
        visual_text: source.to_owned(),
        style_runs: Vec::new(),
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
    }
}

/// Maps a parser syntax kind to the display kind for a block. Returning `None`
/// means "this node does not decide how the block looks" — either it is a
/// structural container (list, table) or a construct with no presenter yet, in
/// which case the raw-source fallback applies. This function is the single seam
/// between the parser vocabulary and the display vocabulary.
#[derive(Clone, Copy)]
struct SyntaxDisplay {
    /// The type a top-level indexed block displays as. Containers may choose a
    /// display even when they do not represent a presentable tree node.
    indexed_block: BlockKind,
    /// The type a presentable tree node contributes to a line.
    node_block: Option<BlockKind>,
    inline_style: Option<StyleKind>,
    line_context: LineContext,
}

const fn syntax_display(kind: NodeKind) -> SyntaxDisplay {
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

fn estimated_height(kind: BlockKind, line_height: f32) -> f32 {
    const BASE_LINE_HEIGHT: f32 = 26.0;
    const IMAGE_ROW_HEIGHT_AT_BASE_ZOOM: f32 = 190.0;

    match kind {
        BlockKind::Heading(1) => line_height * 1.65,
        BlockKind::Heading(2) => line_height * 1.45,
        BlockKind::Heading(3) => line_height * 1.25,
        BlockKind::Heading(_) => line_height * 1.1,
        BlockKind::CodeBlock => line_height,
        BlockKind::Image => IMAGE_ROW_HEIGHT_AT_BASE_ZOOM * line_height / BASE_LINE_HEIGHT,
        BlockKind::TableDelimiter => 8.0,
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
        let owns_marker = if has_delimiter_markers(span.kind)
            && !matches!(span.kind, NodeKind::CodeBlock)
        {
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
        .filter(|marker| {
            marker.quote_owner.is_some() || marker.formal_quote_owner.is_some()
        })
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
                && shared
                    .list_projection
                    .is_some_and(|projection| {
                        marker.fence_edge.is_none()
                            || projection.fence_marker_edge(marker.range).is_some()
                    }))
                || marker.formal_container
        });
    }
    if kind == BlockKind::CodeBlock {
        let opening_fences = projected_markers
            .iter()
            .filter(|marker| {
                marker.fence && marker.fence_edge == Some(MarkerEdge::Opening)
            })
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
    let quote_depth = nodes
        .iter()
        .filter_map(|id| parsed.tree.node(**id))
        .filter(|node| node.kind == NodeKind::Quote)
        .count()
        .max(
            shared
                .list_projection
                .into_iter()
                .flat_map(|projection| projection.quotes_in(range))
                .map(|quote| quote.depth)
                .max()
                .unwrap_or(0),
        )
        .max(
            projected_markers
                .iter()
                .filter_map(|marker| marker.formal_quote_depth)
                .max()
                .unwrap_or(0),
        );
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
    let quote = (quote_depth > 0).then_some(QuoteRowMetadata {
        depth: quote_depth,
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
    for planned in markers_on_line {
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
        && markers_on_line.iter().filter(|marker| marker.fence).any(|marker| {
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
    let depth = projection
        .quotes_in(range)
        .map(|quote| quote.depth)
        .max()
        .unwrap_or(0);
    (depth > 0).then_some(QuoteRowMetadata {
        depth,
        disclosed_depth: 0,
    })
}

/// Whether a [`disclosure_runs`] run should be presented and disclosed with
/// shared-parse semantics — one whole-paragraph parse and one run-wide merged
/// disclosure (see [`merged_disclosure`]) — rather than as a single
/// self-contained line.
///
/// True whenever [`disclosure_runs`] itself joined two or more physical
/// lines, but also whenever a caller already holds a valid whole-block
/// [`JoinedParse`]: `window.lines` can be trimmed down to just `window.render`
/// once one exists (see `presented_block`'s `JOIN_SYNC_LINE_BUDGET` seam in
/// `hane-ui`), which collapses the run to one physical line even though the
/// block's own construct still spans more than the one line being drawn. A
/// one-line run must then still resolve markers against the cached
/// whole-block parse instead of a lone-line reparse that never saw the rest
/// of the construct — the same Markdown must not change marker visibility or
/// style depending on how many physical lines happen to be in the render
/// window.
fn run_uses_shared_parse(run_len: usize, joined: Option<&JoinedParse>) -> bool {
    run_len > 1 || joined.is_some()
}

/// Groups `window.lines` into the runs [`present_block`] presents from: a run
/// of two or more contiguous lines in a joinable block's normal flow, sharing
/// one whole-paragraph parse and one run-wide merged disclosure (see
/// [`merged_disclosure`]), or a single line presented and disclosed on its
/// own. Returns index ranges into `window.lines`, in order. Needs no parse,
/// so [`expected_disclosures`] can reuse the exact same grouping
/// [`present_block`] itself derives without doing the whole-paragraph parse
/// that grouping feeds into.
///
/// A standalone image line stays inside its run rather than splitting it: a
/// delimiter pair CommonMark resolves across the whole paragraph (see
/// [`present_joined_run`]) must still close correctly when an image line sits
/// between the two halves, even though that image line itself renders through
/// [`present_image`] rather than the shared parse (see
/// [`inactive_standalone_image`]).
///
/// Whether a returned run is actually presented with shared-parse semantics
/// is [`run_uses_shared_parse`]'s call, not this function's: a run can come
/// back one line long here and still need the shared parse, when
/// `window.joined` is already available.
fn disclosure_runs(block_kind: NodeKind, window: &BlockWindow<'_>) -> Vec<Range<usize>> {
    let context = block_line_context(block_kind);
    let content_end = window.span.end.saturating_sub(window.trailing_blank_lines);
    let joinable = block_is_joinable(block_kind);
    let mut runs = Vec::new();
    let mut index = 0;
    while index < window.lines.len() {
        let line = window.lines[index];
        let line_context = if line.line < content_end {
            context
        } else {
            LineContext::Normal
        };
        let mut run_end = index + 1;
        if joinable && line_context == LineContext::Normal {
            while run_end < window.lines.len() {
                let next = window.lines[run_end];
                let next_context = if next.line < content_end {
                    context
                } else {
                    LineContext::Normal
                };
                if next_context != LineContext::Normal {
                    break;
                }
                run_end += 1;
            }
        }
        runs.push(index..run_end);
        index = run_end;
    }
    runs
}

/// Disclosure [`present_block`] would currently assign to each of
/// `window.render`'s lines, paired with that line's document line number: the
/// run-wide merged disclosure (see [`merged_disclosure`]) for a non-empty line
/// inside a run [`run_uses_shared_parse`] says presents with shared-parse
/// semantics — even one whose own [`BlockLine::disclosure`] is `None`, because
/// the caret, selection or IME sits on a different physical line of the same
/// shared construct, or because that other line is off-screen entirely and
/// only reachable through `window.joined` — or the line's own disclosure
/// otherwise.
/// An empty line (the blank run trailing the document's last block; see
/// [`block_line_span`]) never gets the merged value even when grouped into the
/// run, matching `present_markdown_from_parse`'s own early return for empty
/// source, which presents it via [`present_plain`] with no disclosure at all.
/// Neither does an inactive standalone image line grouped into the run — see
/// [`inactive_standalone_image`] — since [`present_joined_run`] renders it
/// through [`present_image`], which always reports no disclosure of its own.
///
/// Computed without parsing, from the same grouping [`present_block`] derives
/// (see [`disclosure_runs`]), so a caller checking whether a cached
/// [`VisualBlock`]'s disclosures are still current (see
/// `EditorView::disclosures_are_current`) gets exactly the value a fresh
/// [`present_block`] call would assign instead of risking the two definitions
/// of "current" falling out of sync — which otherwise invalidates and rebuilds
/// the cache every frame the caret sits inside an active multi-line paragraph,
/// since a per-line-only recomputation never agrees with the run-wide value a
/// joined run's other lines were actually presented with.
pub fn expected_disclosures(
    block_kind: NodeKind,
    window: &BlockWindow<'_>,
) -> Vec<(usize, Option<SourceRange>)> {
    let mut out = Vec::new();
    for run in disclosure_runs(block_kind, window) {
        let lines = &window.lines[run];
        let shared_parse = run_uses_shared_parse(lines.len(), window.joined);
        let merged = shared_parse
            .then(|| joined_run_disclosure(lines, window.block_disclosure))
            .flatten();
        for line in lines {
            if window.render.contains(&line.line) {
                let disclosure = if shared_parse
                    && !line.text.is_empty()
                    && inactive_standalone_image(line.text, line.range, line.disclosure).is_none()
                {
                    merged
                } else if shared_parse {
                    None
                } else {
                    line.disclosure
                };
                out.push((line.line, disclosure));
            }
        }
    }
    out
}

/// Union of every line's own disclosure in a joined run, so a shared construct
/// that spans more than one physical line discloses consistently on every line
/// that carries one of its markers, not only the line the caret/selection/IME
/// happens to sit on. Reconstructs the original caret point or selection/IME
/// extent: each line's own disclosure is already that extent clipped to the
/// line, so the union of all of them is the extent itself.
fn merged_disclosure(lines: &[BlockLine<'_>]) -> Option<SourceRange> {
    lines
        .iter()
        .filter_map(|line| line.disclosure)
        .reduce(union_disclosure)
}

fn union_disclosure(a: SourceRange, b: SourceRange) -> SourceRange {
    SourceRange {
        start: a.start.min(b.start),
        end: a.end.max(b.end),
    }
}

/// The disclosure a joined run of `lines` presents against: [`merged_disclosure`]'s
/// reconstruction from `lines`' own per-line disclosures, unioned with
/// `block_disclosure`. `lines` may be only the block's visible slice (see
/// [`BlockWindow::block_disclosure`]), so a caret/selection/IME range that
/// falls on an off-screen physical line of the same shared construct would
/// otherwise never surface here.
fn joined_run_disclosure(
    lines: &[BlockLine<'_>],
    block_disclosure: Option<SourceRange>,
) -> Option<SourceRange> {
    [merged_disclosure(lines), block_disclosure]
        .into_iter()
        .flatten()
        .reduce(union_disclosure)
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

fn list_alignments(tree: &MarkdownTree) -> Vec<Option<ListAlignment>> {
    let mut alignments = vec![None; tree.len()];
    for (id, node) in tree.iter() {
        let NodeKind::List { start } = node.kind else {
            continue;
        };
        let marker_labels = tree
            .children(id)
            .iter()
            .enumerate()
            .map(|(ordinal, _)| list_label(start, ordinal))
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
        let list_alignments = list_alignments(&parsed.tree);
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
        }
    }
}

fn list_marker_metadata(
    planned: &ProjectedMarker,
    parsed: &MarkdownParse,
    nodes: &SourceIndex<NodeId>,
    visual: &str,
    source_map: &SourceMap,
    disclosure: Option<SourceRange>,
    list_projection: Option<&ListProjection>,
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
    Some(ListMarkerMetadata {
        source_range: planned.range,
        anchor: planned.range.start,
        visual_range,
        label,
        synthesized: !expanded,
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
        list_marker_metadata(
            planned,
            parsed,
            &projection.nodes,
            visual,
            source_map,
            disclosure,
            list_projection,
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
    let source_prefix = opening
        .map(|planned| SourceRange::new(range.start.0, planned.range.start.0))
        .filter(|range| !range.is_empty())
        .or_else(|| {
            projected.map(|projected| {
                SourceRange::new(projected.item_range.start.0, projected.marker_range.start.0)
            })
        })
        .filter(|range| !range.is_empty());
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
        empty_caret_origin: None,
        source_prefix,
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

/// Presents a run of a standalone paragraph's contiguous physical lines from
/// one shared parse of their joined source, so emphasis, strong emphasis and
/// code spans that cross a physical line boundary resolve the way a single
/// whole-paragraph parse would. See [`present_markdown_from_parse`] for why a
/// per-line parse cannot see these on its own.
///
/// `joined`, when given, is a whole-block parse computed elsewhere (see
/// [`JoinedParse`]) and is used as the shared parse directly instead of
/// rejoining and reparsing `lines`; only `lines` themselves still have to be
/// this run's own, since each is presented against its own physical range
/// regardless of which parse supplied it.
#[cfg(test)]
fn present_joined_run(
    lines: &[BlockLine<'_>],
    revision: Revision,
    line_height: f32,
    render: &Range<usize>,
    joined: Option<&JoinedParse>,
    block_disclosure: Option<SourceRange>,
    out: &mut Vec<VisualLine>,
) {
    present_joined_run_with_list_projection(
        lines,
        revision,
        line_height,
        render,
        joined,
        block_disclosure,
        None,
        out,
    );
}

#[allow(
    clippy::too_many_arguments,
    reason = "joined presentation keeps the render window and parse context together"
)]
fn present_joined_run_with_list_projection(
    lines: &[BlockLine<'_>],
    revision: Revision,
    line_height: f32,
    render: &Range<usize>,
    joined: Option<&JoinedParse>,
    block_disclosure: Option<SourceRange>,
    list_projection: Option<&ListProjection>,
    out: &mut Vec<VisualLine>,
) {
    let computed;
    let joined = match joined {
        Some(joined) => joined,
        None => {
            computed = parse_joined_block(lines, revision);
            &computed
        }
    };
    let shared = SharedParse {
        parsed: &joined.parsed,
        projection: &joined.projection,
        list_projection,
    };
    // A single active disclosure (caret, selection or IME) may touch a shared
    // construct whose markers live on different physical lines; `marker_is_disclosed`
    // only expands a marker whose enclosing span reaches the disclosure it is
    // given, so every line of the run is offered the same, run-wide disclosure
    // rather than only the one line that literally owns the caret.
    let disclosure = joined_run_disclosure(lines, block_disclosure);
    for line in lines {
        if !render.contains(&line.line) {
            continue;
        }
        // An inactive standalone image line keeps rendering through its own
        // dedicated path even while joined into the run for parse purposes —
        // only the surrounding text lines' delimiters need the shared parse;
        // the image line's own disclosure (not the run-wide merged one)
        // decides whether it is presently being edited as raw markup instead.
        let mut presented = match inactive_standalone_image(line.text, line.range, line.disclosure)
        {
            Some(image) => present_image(
                line.line as u64,
                revision,
                line.range,
                line.text,
                line_height,
                image,
                list_projection.and_then(|projection| formal_quote_metadata(line.range, projection)),
            ),
            None => present_markdown_from_parse(
                line.line as u64,
                revision,
                line.range,
                line.text,
                line_height,
                disclosure,
                &shared,
            ),
        };
        presented.context = LineContext::Normal;
        while presented.visual_text.ends_with(['\r', '\n']) {
            presented.visual_text.pop();
        }
        out.push(presented);
    }
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
        present_fenced_code_line(line_id, revision, range, source, line_height, disclosure, fence_role)
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
        && disclosure.is_none_or(|active| !range_touches(range, active))
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
        Some(FenceLine::Opening) => {
            present_fenced_code_opening_line(line_id, revision, range, source, line_height, disclosure)
        }
        Some(FenceLine::Closing) => {
            present_fenced_code_closing_line(line_id, revision, range, source, line_height, disclosure)
        }
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
    let expanded = disclosure.is_some_and(|active| {
        disclosure_owns_physical_line(range, source, active)
    });
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
    let line_estimated_height =
        fenced_line_height(
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
    let visual_text = if expanded { source.to_owned() } else { String::new() };
    let styled_len = visual_text.trim_end_matches(['\r', '\n']).len();
    let style_runs = if styled_len > 0 {
        vec![StyleRun {
            visual_range: VisualRange::new(0, styled_len),
            kind: StyleKind::CodeBlock,
        }]
    } else {
        Vec::new()
    };
    let line_estimated_height =
        fenced_line_height(
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

fn table_row_from_projection(
    line: &VisualLine,
    projection: Option<&TableProjection>,
    range: SourceRange,
    header: bool,
    alignments: &[TableAlignment],
) -> Option<TableRowDisplay> {
    let projected = projection.and_then(|projection| {
        projection
            .rows
            .iter()
            .find(|row| {
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
    if line.context != LineContext::Table || is_table_delimiter(&line.visual_text) {
        return None;
    }
    let content_end = line.visual_text.trim_end_matches(['\r', '\n']).len();
    let pipes = unescaped_table_pipes(&line.visual_text, content_end);
    let leading_indent = pipes
        .first()
        .copied()
        .filter(|index| {
            *index <= 3 && line.visual_text[..*index].bytes().all(|byte| byte == b' ')
        })
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
    let mut previous_pipe = None;
    for pipe in pipes {
        let pipe_source = source_for_pipe(pipe)?;
        if cursor < pipe {
            let source_start = previous_pipe
                .map_or(SourceOffset(line.source_range.start.0 + leading_indent), |source| {
                    SourceOffset(source.0.saturating_add(1))
                });
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
                    previous_pipe
                        .map_or(line.source_range.start.0 + leading_indent, |source| {
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
        let source_start = previous_pipe
            .map_or(SourceOffset(line.source_range.start.0 + leading_indent), |source| {
                SourceOffset(source.0.saturating_add(1))
            });
        let source_end = line
            .source_map
            .segments
            .iter()
            .filter(|segment| {
                !segment.source_range.is_empty()
                    && segment.visual_range.start.0 == content_end
                    && matches!(segment.visibility, Visibility::Visible | Visibility::ExpandedMarkup)
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

fn present_table_line(
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
        };
    }
    let content_end = source.trim_end_matches(['\r', '\n']).len();
    let pipes = unescaped_table_pipes(source, content_end);
    let leading_indent = pipes
        .first()
        .copied()
        .filter(|index| {
            *index <= 3 && source[..*index].bytes().all(|byte| byte == b' ')
        })
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

/// Heights per leaf chunk. Structural edits rewrite at most the two boundary
/// chunks plus their replacement instead of moving every following block.
const HEIGHT_CHUNK_TARGET: usize = 128;

#[derive(Clone, Debug)]
struct HeightChunk {
    heights: Vec<f32>,
    total: f32,
}

impl HeightChunk {
    fn new(heights: &[f32]) -> Self {
        Self {
            heights: heights.to_vec(),
            total: heights.iter().sum(),
        }
    }
}

/// Two-level Fenwick tree over non-negative block heights. Per-block values
/// live in bounded chunks; the trees index only chunk totals and item counts.
#[derive(Clone, Debug)]
pub struct HeightIndex {
    chunks: Vec<HeightChunk>,
    sums: Vec<f32>,
    counts: Vec<usize>,
    len: usize,
}

impl HeightIndex {
    pub fn new(heights: impl IntoIterator<Item = f32>) -> Self {
        let heights: Vec<_> = heights.into_iter().map(|h| h.max(0.0)).collect();
        let chunks = heights
            .chunks(HEIGHT_CHUNK_TARGET)
            .map(HeightChunk::new)
            .collect();
        let mut this = Self {
            chunks,
            sums: Vec::new(),
            counts: Vec::new(),
            len: heights.len(),
        };
        this.retree();
        this
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn retree(&mut self) {
        self.sums.clear();
        self.counts.clear();
        self.sums.push(0.0);
        self.counts.push(0);
        self.sums
            .extend(self.chunks.iter().map(|chunk| chunk.total));
        self.counts
            .extend(self.chunks.iter().map(|chunk| chunk.heights.len()));
        for index in 1..self.sums.len() {
            let parent = index + (index & index.wrapping_neg());
            if parent < self.sums.len() {
                self.sums[parent] += self.sums[index];
                self.counts[parent] += self.counts[index];
            }
        }
    }

    fn tree_prefix<T>(tree: &[T], exclusive_end: usize) -> T
    where
        T: Copy + Default + std::ops::AddAssign,
    {
        let mut index = exclusive_end.min(tree.len().saturating_sub(1));
        let mut sum = T::default();
        while index > 0 {
            sum += tree[index];
            index &= index - 1;
        }
        sum
    }

    fn search_counts(&self, ordinal: usize) -> (usize, usize) {
        let mut index = 0;
        let mut count = 0;
        let mut step = 1;
        while step << 1 < self.counts.len() {
            step <<= 1;
        }
        while step > 0 {
            let next = index + step;
            if next < self.counts.len() && count + self.counts[next] <= ordinal {
                index = next;
                count += self.counts[next];
            }
            step >>= 1;
        }
        (index, count)
    }

    fn locate(&self, ordinal: usize) -> Option<(usize, usize)> {
        if ordinal >= self.len {
            return None;
        }
        let (chunk, before) = self.search_counts(ordinal);
        Some((chunk, ordinal - before))
    }

    fn locate_insert(&self, ordinal: usize) -> (usize, usize) {
        self.locate(ordinal).unwrap_or_else(|| {
            self.chunks
                .last()
                .map_or((0, 0), |chunk| (self.chunks.len() - 1, chunk.heights.len()))
        })
    }

    fn add_sum(&mut self, chunk: usize, delta: f32) {
        let mut node = chunk + 1;
        while node < self.sums.len() {
            self.sums[node] += delta;
            node += node & node.wrapping_neg();
        }
    }

    pub fn update(&mut self, index: usize, height: f32) {
        let next = height.max(0.0);
        let (chunk, slot) = self.locate(index).expect("height index out of bounds");
        let delta = next - self.chunks[chunk].heights[slot];
        self.chunks[chunk].heights[slot] = next;
        self.chunks[chunk].total += delta;
        self.add_sum(chunk, delta);
    }
    pub fn height(&self, index: usize) -> Option<f32> {
        let (chunk, slot) = self.locate(index)?;
        Some(self.chunks[chunk].heights[slot])
    }
    /// Replaces one ordinal range while preserving the measured heights outside
    /// it. Only boundary chunks are copied; the Fenwick trees are rebuilt over
    /// chunks, not over every block.
    pub fn splice(&mut self, range: Range<usize>, heights: impl IntoIterator<Item = f32>) {
        assert!(range.start <= range.end && range.end <= self.len());
        let inserted = heights
            .into_iter()
            .map(|height| height.max(0.0))
            .collect::<Vec<_>>();
        let (first_chunk, first_slot) = self.locate_insert(range.start);
        let (last_chunk, last_slot) = self.locate_insert(range.end);
        let mut merged = Vec::with_capacity(inserted.len() + 2 * HEIGHT_CHUNK_TARGET);
        if let Some(chunk) = self.chunks.get(first_chunk) {
            merged.extend_from_slice(&chunk.heights[..first_slot.min(chunk.heights.len())]);
        }
        merged.extend_from_slice(&inserted);
        if let Some(chunk) = self.chunks.get(last_chunk) {
            merged.extend_from_slice(&chunk.heights[last_slot.min(chunk.heights.len())..]);
        }
        let replacement = merged
            .chunks(HEIGHT_CHUNK_TARGET)
            .map(HeightChunk::new)
            .collect::<Vec<_>>();
        let end_chunk = (last_chunk + 1).min(self.chunks.len());
        self.chunks
            .splice(first_chunk.min(end_chunk)..end_chunk, replacement);
        self.len = self.len - range.len() + inserted.len();
        self.retree();
    }
    pub fn prefix_sum(&self, exclusive_end: usize) -> f32 {
        let end = exclusive_end.min(self.len);
        let Some((chunk, slot)) = self.locate(end) else {
            return Self::tree_prefix(&self.sums, self.chunks.len());
        };
        Self::tree_prefix(&self.sums, chunk)
            + self.chunks[chunk].heights[..slot].iter().sum::<f32>()
    }
    pub fn total_height(&self) -> f32 {
        self.prefix_sum(self.len())
    }
    pub fn block_at_y(&self, y: f32) -> usize {
        if self.is_empty() {
            return 0;
        }
        let target = y.clamp(0.0, self.total_height());
        let mut chunk = 0usize;
        let mut sum = 0.0;
        let mut bit = 1usize;
        while bit << 1 < self.sums.len() {
            bit <<= 1;
        }
        while bit > 0 {
            let next = chunk + bit;
            if next < self.sums.len() && sum + self.sums[next] <= target {
                chunk = next;
                sum += self.sums[next];
            }
            bit >>= 1;
        }
        // A `target` equal to the whole height — where a viewport at the end of
        // the document lands, since `y` is clamped to it — lets the descent walk
        // past the last chunk. Step back onto that chunk and take its total back
        // out of `sum`, or the scan below starts from a running height that
        // already covers the chunk and returns its first entry.
        if chunk >= self.chunks.len() {
            chunk = self.chunks.len() - 1;
            sum -= self.chunks[chunk].total;
        }
        let ordinal = Self::tree_prefix(&self.counts, chunk);
        for (slot, height) in self.chunks[chunk].heights.iter().enumerate() {
            if target < sum + height {
                return ordinal + slot;
            }
            sum += height;
        }
        (ordinal + self.chunks[chunk].heights.len().saturating_sub(1)).min(self.len - 1)
    }
    pub fn visible_range(&self, scroll_y: f32, viewport: f32, overscan: f32) -> Range<usize> {
        if self.is_empty() {
            return 0..0;
        }
        let start = self.block_at_y((scroll_y - overscan).max(0.0));
        let end = (self.block_at_y(scroll_y + viewport + overscan) + 1).min(self.len());
        start..end
    }
}

/// A scroll position expressed against a block rather than a pixel offset, so it
/// survives a rebuild of the height index.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollAnchor {
    pub block: BlockId,
    pub intra_block_y: f32,
    pub visual_position_hint: Option<VisualOffset>,
}

pub fn anchored_scroll_y(
    anchor: ScrollAnchor,
    blocks: &[VisualBlock],
    heights: &HeightIndex,
) -> Option<f32> {
    let index = blocks.iter().position(|block| block.id == anchor.block)?;
    Some(heights.prefix_sum(index) + anchor.intra_block_y.clamp(0.0, blocks[index].height()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_block_heights_keep_the_caret_owned_fence_row_visible() {
        let document = RopeBuffer::from_text("```\n```");
        let index = BlockIndex::from_buffer(&document);
        let inactive = block_heights(&document, &index, 26.0);
        assert_eq!(inactive, vec![0.0]);

        let editing = block_heights_with_disclosure(
            &document,
            &index,
            26.0,
            Some(SourceRange::empty(1)),
        );
        assert_eq!(
            editing,
            vec![26.0],
            "the opening fence owned by the caret must survive initial virtualization"
        );
    }

    #[test]
    fn selection_outside_a_quote_fence_block_does_not_disclose_its_rows() {
        let source = "before\n\n> ```\n> code\n> ```";
        let document = RopeBuffer::from_text(source);
        let index = BlockIndex::from_buffer(&document);
        let quote_block = index
            .blocks()
            .find(|block| block.kind == NodeKind::Quote)
            .expect("quote block");
        assert!(
            index.fence_height_projection(&quote_block).is_some(),
            "the fixture must exercise the quote fence projection"
        );
        let inactive = block_heights(&document, &index, 26.0);
        let selection_before_block = SourceRange::new(0, quote_block.source_range.start.0);
        let disclosed = block_heights_with_disclosure(
            &document,
            &index,
            26.0,
            Some(selection_before_block),
        );

        assert_eq!(
            disclosed, inactive,
            "a selection ending at the block boundary must not activate its fence rows"
        );
    }

    #[test]
    fn caret_at_the_next_block_start_does_not_disclose_the_previous_quote() {
        let source = "> ```\n> code\n> ```\n\nplain";
        let document = RopeBuffer::from_text(source);
        let index = BlockIndex::from_buffer(&document);
        let quote_block = index
            .blocks()
            .find(|block| block.kind == NodeKind::Quote)
            .expect("quoted block");
        let inactive = block_heights(&document, &index, 26.0);
        let disclosed = block_heights_with_disclosure(
            &document,
            &index,
            26.0,
            Some(SourceRange::empty(quote_block.source_range.end.0)),
        );

        assert_eq!(
            disclosed, inactive,
            "the caret at the following block start must not make the preceding quote visible"
        );
    }

    #[test]
    fn caret_at_a_quote_owner_end_discloses_fence_rows_before_following_list_text() {
        let source = "- > ```\n  > code\n  > ```\n  after";
        let document = RopeBuffer::from_text(source);
        let index = BlockIndex::from_buffer(&document);
        let after = source.rfind('\n').expect("following list row") + 1;
        let inactive = block_heights(&document, &index, 26.0);
        let disclosed = block_heights_with_disclosure(
            &document,
            &index,
            26.0,
            Some(SourceRange::empty(after)),
        );

        assert_eq!(inactive.len(), 1, "the fixture must stay in one list block");
        assert_eq!(disclosed, vec![26.0 * 4.0]);
        assert_eq!(inactive, vec![26.0 * 3.0]);
    }

    #[test]
    fn quote_depth_and_inline_style_come_from_the_shared_parse() {
        let source = "> outer\n> > **inner**\n";
        let mut offset = 0;
        let lines = source
            .split_inclusive('\n')
            .enumerate()
            .map(|(line, text)| {
                let range = SourceRange::new(offset, offset + text.len());
                offset = range.end.0;
                BlockLine {
                    line,
                    range,
                    text,
                    disclosure: None,
                }
            })
            .collect::<Vec<_>>();
        let block = IndexedBlock {
            ordinal: 0,
            id: BlockId(0),
            kind: NodeKind::Quote,
            source_range: SourceRange::new(0, source.len()),
            revision: Revision(1),
            confidence: Confidence::Formal,
            line_count: lines.len(),
            leading_content_lines: 0,
        };
        let joined = parse_joined_block(&lines, Revision(1));
        let visual = present_block(
            &block,
            Revision(1),
            &BlockWindow {
                span: 0..lines.len(),
                trailing_blank_lines: 0,
                lines: &lines,
                clipped_fence_lines: &[],
                zero_height_fence_rows_before: 0,
                zero_height_fence_rows_after: 0,
                render: 0..lines.len(),
                joined: Some(&joined),
                block_disclosure: None,
            },
            26.0,
        );

        assert_eq!(visual.lines[0].visual_text, "outer");
        assert_eq!(visual.lines[1].visual_text, "inner");
        assert_eq!(
            visual.lines[0].quote,
            Some(QuoteRowMetadata {
                depth: 1,
                disclosed_depth: 0,
            })
        );
        assert_eq!(
            visual.lines[1].quote,
            Some(QuoteRowMetadata {
                depth: 2,
                disclosed_depth: 0,
            })
        );
        assert!(visual.lines[1]
            .style_runs
            .iter()
            .any(|run| run.kind == StyleKind::Bold));
        assert!(visual.lines[1]
            .source_map
            .segments
            .iter()
            .any(|segment| segment.visibility == Visibility::HiddenMarkup));
        let layout = layout_block(&visual, 400.0, &testing::FixedAdvanceShaper::default());
        assert_eq!(layout.lines[0].text_x_origin, QUOTE_DEPTH_INDENT);
        assert_eq!(layout.lines[1].text_x_origin, QUOTE_DEPTH_INDENT * 2.0);
        assert!(layout.lines[1].quote_bar_x_origin.is_some());
    }

    #[test]
    fn formal_quote_projection_carries_lazy_continuation_depth() {
        let source = "> outer\ncontinuation\n";
        let index = BlockIndex::build(Revision(1), source);
        let block = index.block(0).expect("quote block");
        let projection = index
            .list_projection(&block)
            .expect("formal quote projection");
        let line_start = "> outer\n".len();
        let line_range = SourceRange::new(line_start, source.len());
        let lines = [BlockLine {
            line: 1,
            range: line_range,
            text: &source[line_start..],
            disclosure: None,
        }];
        let visual = present_block_with_list_projection(
            &block,
            Revision(1),
            &BlockWindow {
                span: 0..2,
                trailing_blank_lines: 0,
                lines: &lines,
                clipped_fence_lines: &[],
                zero_height_fence_rows_before: 0,
                zero_height_fence_rows_after: 0,
                render: 1..2,
                joined: None,
                block_disclosure: None,
            },
            26.0,
            Some(projection),
        );

        assert_eq!(visual.lines[0].visual_text, "continuation");
        assert_eq!(visual.lines[0].kind, BlockKind::Quote);
        assert_eq!(
            visual.lines[0].quote,
            Some(QuoteRowMetadata {
                depth: 1,
                disclosed_depth: 0,
            })
        );
    }

    #[test]
    fn thematic_break_uses_parser_kind_and_discloses_its_source() {
        let source = "---\n";
        let range = SourceRange::new(0, source.len());
        let inactive = present_polished_line(
            0,
            Revision(1),
            range,
            source,
            26.0,
            None,
            LineContext::Normal,
        );
        assert_eq!(inactive.kind, BlockKind::Rule);
        assert!(inactive.visual_text.is_empty());
        assert!(inactive.rule_body_is_collapsed());
        assert_eq!(inactive.source_map.segments[0].source_range, range);
        assert_eq!(
            inactive.source_map.segments[0].visibility,
            Visibility::HiddenMarkup
        );

        let active = present_polished_line(
            0,
            Revision(1),
            range,
            source,
            26.0,
            Some(SourceRange::empty(1)),
            LineContext::Normal,
        );
        assert_eq!(active.kind, BlockKind::Rule);
        assert_eq!(active.visual_text, source);
        assert!(!active.rule_body_is_collapsed());
        assert_eq!(
            active.source_map.segments[0].visibility,
            Visibility::ExpandedMarkup
        );

        for (source, is_rule, has_list_item) in [
            ("---\n", true, false),
            ("title\n---\n", false, false),
            ("- item\n", false, true),
            // The leading bullet is not a list marker here: the complete line
            // is a thematic break, so the parser emits a top-level Rule.
            ("- ---\n", true, false),
            ("--- text\n", false, false),
        ] {
            let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
            assert_eq!(
                parsed.tree.iter().any(|(_, node)| node.kind == NodeKind::Rule),
                is_rule,
                "Rule must follow parser context for {source:?}"
            );
            assert_eq!(
                parsed
                    .tree
                    .iter()
                    .any(|(_, node)| matches!(node.kind, NodeKind::ListItem { .. })),
                has_list_item,
                "list-item ancestry must follow parser context for {source:?}"
            );
        }
    }

    #[test]
    fn thematic_break_parser_tree_preserves_list_and_quote_ancestors() {
        for (source, quoted) in [
            ("- item\n\n  ---\n", false),
            ("> - item\n>\n>   ---\n", true),
        ] {
            let range = SourceRange::new(0, source.len());
            let parsed = parse_document(Revision(1), range, source);
            let rule_id = parsed
                .tree
                .iter()
                .find_map(|(id, node)| (node.kind == NodeKind::Rule).then_some(id))
                .expect("parser-confirmed thematic break");
            let ancestors = parsed
                .tree
                .ancestors(rule_id)
                .skip(1)
                .filter_map(|id| parsed.tree.node(id).map(|node| node.kind))
                .collect::<Vec<_>>();
            assert!(
                ancestors
                    .iter()
                    .any(|kind| matches!(kind, NodeKind::List { .. })),
                "Rule must be nested in a List: {source:?}"
            );
            assert!(
                ancestors
                    .iter()
                    .any(|kind| matches!(kind, NodeKind::ListItem { .. })),
                "Rule must be nested in a ListItem: {source:?}"
            );
            assert_eq!(
                ancestors.contains(&NodeKind::Quote),
                quoted,
                "quote ancestry must follow parser context for {source:?}"
            );
        }
    }

    #[test]
    fn nested_rule_projects_container_prefixes_into_source_map_and_layout_metadata() {
        for source in ["- item\n\n  ---\n", "> - item\n>\n>   ---\n"] {
            let rule_start = source.find("---").expect("rule source");
            let line_start = source[..rule_start]
                .rfind('\n')
                .map_or(0, |newline| newline + 1);
            let rule_line = |active: bool| {
                let mut offset = 0;
                let lines = source
                    .split_inclusive('\n')
                    .enumerate()
                    .map(|(line, text)| {
                        let line_range = SourceRange::new(offset, offset + text.len());
                        offset = line_range.end.0;
                        BlockLine {
                            line,
                            range: line_range,
                            text,
                            disclosure: active
                                .then_some(SourceRange::empty(rule_start + 1))
                                .filter(|disclosure| range_touches(line_range, *disclosure)),
                        }
                    })
                    .collect::<Vec<_>>();
                let joined = parse_joined_block(&lines, Revision(1));
                let mut presented = Vec::new();
                present_joined_run(
                    &lines,
                    Revision(1),
                    26.0,
                    &(0..lines.len()),
                    Some(&joined),
                    None,
                    &mut presented,
                );
                presented
                    .into_iter()
                    .find(|line| line.source_range.start.0 == line_start)
                    .expect("the rule line is presented")
            };

            let inactive = rule_line(false);
            assert_eq!(inactive.kind, BlockKind::Rule, "source: {source:?}");
            assert!(inactive.visual_text.is_empty(), "source: {source:?}");
            let list = inactive.list.as_ref().expect("nested rule keeps list metadata");
            assert_eq!(list.body_visual_start, VisualOffset(0));
            let hidden_prefix_bytes = inactive
                .source_map
                .segments
                .iter()
                .filter(|segment| {
                    segment.source_range.start.0 >= line_start
                        && segment.source_range.end.0 <= rule_start
                        && segment.visibility == Visibility::HiddenMarkup
                })
                .map(|segment| segment.source_range.len_bytes())
                .sum::<usize>();
            assert_eq!(hidden_prefix_bytes, rule_start - line_start);
            assert_eq!(inactive.quote.is_some(), source.starts_with('>'));

            let active = rule_line(true);
            assert_eq!(active.kind, BlockKind::Rule, "source: {source:?}");
            assert_eq!(
                active.visual_text,
                source[line_start..]
                    .trim_end_matches(['\r', '\n']),
                "source: {source:?}"
            );
            let active_list = active.list.as_ref().expect("active rule keeps list metadata");
            assert_eq!(
                active_list.body_visual_start,
                VisualOffset(rule_start - line_start)
            );
            let expanded_prefix_bytes = active
                .source_map
                .segments
                .iter()
                .filter(|segment| {
                    segment.source_range.start.0 >= line_start
                        && segment.source_range.end.0 <= rule_start
                        && segment.visibility == Visibility::ExpandedMarkup
                })
                .map(|segment| segment.source_range.len_bytes())
                .sum::<usize>();
            assert_eq!(expanded_prefix_bytes, rule_start - line_start);
            assert!(active
                .source_map
                .segments
                .iter()
                .all(|segment| segment.source_range.is_empty()
                    || segment.visibility == Visibility::ExpandedMarkup));
        }
    }

    #[test]
    fn source_index_skips_offscreen_spans_under_a_long_enclosing_construct() {
        // A prefix-max-only index would scan all preceding spans because the
        // enclosing construct reaches the end. Subtree maxima must prune them.
        let mut ranges = vec![(SourceRange::new(0, 1_000_000), 0)];
        ranges.extend((1..100_000).map(|i| (SourceRange::new(i * 10, i * 10 + 5), i)));
        let index = SourceIndex::new(ranges);
        let mut hits = Vec::new();
        let mut visited = 0;
        index.query(
            0,
            index.entries.len(),
            SourceRange::new(999_980, 999_985),
            &mut hits,
            &mut visited,
        );
        hits.sort();
        assert_eq!(hits, vec![&0, &99_998]);
        assert!(
            visited < 100,
            "visited {visited} nodes for two intersecting spans"
        );
    }

    #[test]
    fn cached_hundred_thousand_line_quote_projects_only_viewport_markers() {
        let source = format!("> **opening\n{}> closing**\n", "> plain\n".repeat(99_998));
        let mut start = 40;
        let lines = source
            .split_inclusive('\n')
            .enumerate()
            .map(|(line, text)| {
                let range = SourceRange::new(start, start + text.len());
                start = range.end.0;
                BlockLine {
                    line,
                    range,
                    text,
                    disclosure: None,
                }
            })
            .collect::<Vec<_>>();
        let joined = parse_joined_block(&lines, Revision(7));
        assert_eq!(joined.projection.markers.len(), 100_002);
        let shared = SharedParse {
            parsed: &joined.parsed,
            projection: &joined.projection,
            list_projection: None,
        };
        // The caret is off-screen inside the strong span. Prefix ownership and
        // both distant delimiters still use the same complete snapshot.
        let disclosure = Some(SourceRange::empty(lines[1].range.start.0 + 3));
        for line in &lines[99_950..] {
            let presented = present_markdown_from_parse(
                line.line as u64,
                Revision(7),
                line.range,
                line.text,
                26.0,
                disclosure,
                &shared,
            );
            assert_eq!(presented.visual_text, line.text);
            assert!(
                presented
                    .style_runs
                    .iter()
                    .any(|run| run.kind == StyleKind::Bold)
            );
            assert!(segments_tile_range(
                line.range,
                &presented.source_map.segments
            ));
            assert!(
                presented.source_map.segments.capacity() <= 5,
                "mapping storage must depend on this line's markers, not all 100,002"
            );
            let hidden = present_markdown_from_parse(
                line.line as u64,
                Revision(7),
                line.range,
                line.text,
                26.0,
                None,
                &shared,
            );
            assert_eq!(
                hidden.visual_text,
                if line.line == 99_999 {
                    "closing\n"
                } else {
                    "plain\n"
                }
            );
        }
    }

    #[test]
    fn plain_presentation_preserves_markdown_source_bytes() {
        let text = "**日本語**";
        let block = present_plain(3, Revision(2), SourceRange::new(10, 10 + text.len()), text);
        assert_eq!(block.visual_text, text);
        for relative in [0, 2, 5, 8, text.len()] {
            assert_eq!(
                block
                    .source_map
                    .visual_to_source(VisualOffset(relative), Bias::After)
                    .unwrap()
                    .source_offset,
                SourceOffset(10 + relative)
            );
        }
    }
    #[test]
    fn fenwick_updates_and_finds_visible_blocks() {
        let mut h = HeightIndex::new([10.0, 20.0, 30.0]);
        assert_eq!(h.total_height(), 60.0);
        assert_eq!(h.block_at_y(10.0), 1);
        assert_eq!(h.visible_range(12.0, 10.0, 0.0), 1..2);
        h.update(0, 20.0);
        assert_eq!(h.total_height(), 70.0);
        assert_eq!(h.block_at_y(15.0), 0);
    }
    #[test]
    fn the_last_block_stays_visible_when_the_viewport_reaches_the_end() {
        // Measured heights of the README's blocks: the viewport plus overscan
        // reaches past the total height well before the scroll limit, so the
        // whole tail of the document depends on this.
        let heights = HeightIndex::new([
            68.9, 216.0, 104.0, 63.7, 208.0, 63.7, 78.0, 52.0, 63.7, 52.0, 235.3, 52.0, 115.7,
            63.7, 52.0, 314.8, 52.0, 63.7, 52.0, 63.7, 52.0, 52.0, 145.6, 130.0,
        ]);
        let total = heights.total_height();
        assert_eq!(heights.block_at_y(total), 23);
        assert_eq!(heights.block_at_y(total + 500.0), 23);
        let visible = heights.visible_range(1480.6, 693.0, 260.0);
        assert_eq!(visible.end, 24);
        assert!(visible.start < visible.end);
    }

    #[test]
    fn the_last_block_stays_visible_across_a_chunk_boundary() {
        // More entries than one chunk holds, so the descent ends on a chunk the
        // running height has already been carried past.
        let heights = HeightIndex::new(vec![10.0; 300]);
        assert_eq!(heights.block_at_y(3000.0), 299);
        assert_eq!(heights.visible_range(2900.0, 100.0, 0.0), 290..300);
    }

    #[test]
    fn height_splice_preserves_measurements_outside_the_changed_blocks() {
        let mut h = HeightIndex::new([11.0, 22.0, 33.0, 44.0]);
        h.splice(1..3, [7.0, 8.0, 9.0]);
        assert_eq!(h.len(), 5);
        assert_eq!(h.height(0), Some(11.0));
        assert_eq!(h.height(1), Some(7.0));
        assert_eq!(h.height(3), Some(9.0));
        assert_eq!(h.height(4), Some(44.0));
        assert_eq!(h.total_height(), 79.0);
        assert_eq!(h.block_at_y(26.0), 3);
    }
    #[test]
    fn chunked_height_index_matches_a_flat_model_across_boundaries() {
        let mut flat = (0..300)
            .map(|index| (index % 9 + 1) as f32)
            .collect::<Vec<_>>();
        let mut heights = HeightIndex::new(flat.iter().copied());
        for (range, inserted) in [
            (127..130, vec![41.0, 42.0, 43.0, 44.0]),
            (0..1, vec![]),
            (299..299, vec![51.0, 52.0]),
            (100..250, vec![61.0, 62.0, 63.0]),
        ] {
            flat.splice(range.clone(), inserted.iter().copied());
            heights.splice(range, inserted);
            assert_eq!(heights.len(), flat.len());
            for index in 0..flat.len() {
                assert_eq!(heights.height(index), Some(flat[index]));
                assert_eq!(heights.prefix_sum(index), flat[..index].iter().sum());
            }
            assert_eq!(heights.total_height(), flat.iter().sum());
        }
        heights.update(128, 75.0);
        flat[128] = 75.0;
        let mut top = 0.0;
        for (index, height) in flat.iter().copied().enumerate() {
            assert_eq!(heights.block_at_y(top), index);
            top += height;
        }
        assert_eq!(heights.total_height(), top);
    }
    #[test]
    fn stale_non_overlapping_block_rebases() {
        let mut b = present_plain(0, Revision(0), SourceRange::new(4, 7), "two");
        let d = RevisionDelta {
            from_revision: Revision(0),
            to_revision: Revision(1),
            edited_source_range_before: SourceRange::empty(0),
            edited_source_range_after: SourceRange::new(0, 4),
            byte_delta: 4,
        };
        assert!(b.rebase(&[d], Revision(1)));
        assert_eq!(b.source_range, SourceRange::new(8, 11));
    }

    #[test]
    fn phase3_presentation_hides_markers_without_changing_source() {
        let source = "## Hello **太字** and _italic_ with `code`";
        let block = present_markdown(
            5,
            Revision(3),
            SourceRange::new(40, 40 + source.len()),
            source,
            26.0,
        );
        assert_eq!(block.visual_text, "Hello 太字 and italic with code");
        assert_eq!(block.kind, BlockKind::Heading(2));
        for kind in [StyleKind::Bold, StyleKind::Italic, StyleKind::InlineCode] {
            assert!(block.style_runs.iter().any(|run| run.kind == kind));
        }
        assert_eq!(
            block
                .source_map
                .visual_to_source(VisualOffset(0), Bias::After)
                .unwrap()
                .source_offset,
            SourceOffset(43)
        );
        assert!(
            block
                .source_map
                .segments
                .iter()
                .any(|segment| segment.visibility == Visibility::HiddenMarkup)
        );
        assert!(block.height() > 26.0);
    }

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

    /// Whether some style run of `kind` fully covers `range`, the same rule
    /// `hane_ui::inline_display_for` applies when combining runs for one
    /// stretch of visual text.
    fn style_run_covers(style_runs: &[StyleRun], kind: StyleKind, range: &Range<usize>) -> bool {
        style_runs.iter().any(|run| {
            run.kind == kind
                && range.start >= run.visual_range.start.0
                && range.end <= run.visual_range.end.0
        })
    }

    #[test]
    fn italic_and_bold_style_runs_fully_cover_cjk_text() {
        // Both `*text*`/`_text_` delimiters and the `***text***` combination,
        // matching the ASCII coverage already asserted for Bold in
        // `phase3_presentation_hides_markers_without_changing_source`, but
        // with CJK-only emphasis content: the render policy must not depend
        // on the emphasized text being ASCII.
        let source = "*漢字ひらがな* and _漢字ひらがな_ and ***漢字ひらがな***";
        let block = present_markdown(
            0,
            Revision(0),
            SourceRange::new(0, source.len()),
            source,
            16.0,
        );
        assert_eq!(
            block.visual_text,
            "漢字ひらがな and 漢字ひらがな and 漢字ひらがな"
        );

        let cjk = "漢字ひらがな";
        let mut occurrences = block.visual_text.match_indices(cjk).map(|(i, _)| i);

        let star = occurrences.next().unwrap();
        let star_range = star..star + cjk.len();
        assert!(style_run_covers(
            &block.style_runs,
            StyleKind::Italic,
            &star_range
        ));
        assert!(!style_run_covers(
            &block.style_runs,
            StyleKind::Bold,
            &star_range
        ));

        let underscore = occurrences.next().unwrap();
        let underscore_range = underscore..underscore + cjk.len();
        assert!(style_run_covers(
            &block.style_runs,
            StyleKind::Italic,
            &underscore_range
        ));

        let triple = occurrences.next().unwrap();
        let triple_range = triple..triple + cjk.len();
        assert!(style_run_covers(
            &block.style_runs,
            StyleKind::Italic,
            &triple_range
        ));
        assert!(style_run_covers(
            &block.style_runs,
            StyleKind::Bold,
            &triple_range
        ));
    }

    #[test]
    fn disclosure_expands_only_the_active_inline_construct() {
        let source = "**one** and _two_";
        let range = SourceRange::new(20, 20 + source.len());
        let block = present_markdown_with_disclosure(
            1,
            Revision(4),
            range,
            source,
            26.0,
            Some(SourceRange::empty(23)),
        );
        assert_eq!(block.visual_text, "**one** and two");
        assert!(block.source_map.segments.iter().any(|segment| {
            segment.visibility == Visibility::ExpandedMarkup
                && segment.source_range == SourceRange::new(20, 22)
        }));
        assert!(block.source_map.segments.iter().any(|segment| {
            segment.visibility == Visibility::HiddenMarkup
                && segment.source_range == SourceRange::new(32, 33)
        }));
    }

    #[test]
    fn a_bare_cr_hard_break_hides_its_syntax_without_touching_the_line_ending() {
        // `RopeBuffer` and `BlockIndex` now split physical lines on a bare CR
        // exactly like `\n`/`\r\n`, so `line.rs` never hands a range spanning
        // two physical lines to the per-line presenter: a hard break's syntax
        // and its terminator both sit at the very end of their own line's
        // range, same as an LF-based hard break's. A hard break only forms
        // with a following line to break to (see
        // `hard_break_never_forms_inside_code_spans_html_tags_or_at_block_end`),
        // so this goes through the real two-line joined pipeline.
        for (first_line, second_line) in [("a  \r", "b"), ("a\\\r", "b")] {
            let base = 5;
            let first_range = SourceRange::new(base, base + first_line.len());
            let second_range =
                SourceRange::new(first_range.end.0, first_range.end.0 + second_line.len());
            let lines = [
                BlockLine {
                    line: 0,
                    range: first_range,
                    text: first_line,
                    disclosure: None,
                },
                BlockLine {
                    line: 1,
                    range: second_range,
                    text: second_line,
                    disclosure: None,
                },
            ];
            let joined = parse_joined_block(&lines, Revision(1));
            assert!(
                joined
                    .parsed
                    .tree
                    .iter()
                    .any(|(_, node)| node.kind == NodeKind::HardBreak),
                "a following line makes this a hard break: {first_line:?}"
            );
            let mut out = Vec::new();
            present_joined_run(
                &lines,
                Revision(1),
                26.0,
                &(0..2),
                Some(&joined),
                None,
                &mut out,
            );
            // The break syntax ("  " or "\") hides as markup; the terminator
            // is trimmed like any other trailing line ending, not turned into
            // a literal control character or a synthesized space.
            assert_eq!(out[0].visual_text, "a", "source: {first_line:?}");
            assert_eq!(out[1].visual_text, "b");
        }
    }

    #[test]
    fn an_ordinary_bare_cr_soft_break_keeps_each_physical_line_its_own_visible_text() {
        let base = 5;
        let first_range = SourceRange::new(base, base + 2);
        let second_range = SourceRange::new(first_range.end.0, first_range.end.0 + 1);
        let lines = [
            BlockLine {
                line: 0,
                range: first_range,
                text: "a\r",
                disclosure: None,
            },
            BlockLine {
                line: 1,
                range: second_range,
                text: "b",
                disclosure: None,
            },
        ];
        let joined = parse_joined_block(&lines, Revision(1));
        assert!(
            joined
                .parsed
                .tree
                .iter()
                .any(|(_, node)| node.kind == NodeKind::SoftBreak),
            "an ordinary bare-CR line ending is a soft break"
        );
        let mut out = Vec::new();
        present_joined_run(
            &lines,
            Revision(1),
            26.0,
            &(0..2),
            Some(&joined),
            None,
            &mut out,
        );
        assert_eq!(out[0].visual_text, "a");
        assert_eq!(out[1].visual_text, "b");
        assert!(
            out[0]
                .source_map
                .segments
                .iter()
                .all(|segment| segment.visibility == Visibility::Visible),
            "an ordinary line ending has no markup to hide"
        );
    }

    #[test]
    fn line_break_padding_hides_insignificant_spaces_from_rendered_presentation() {
        // CommonMark drops leading indentation of the line a break continues
        // onto, and the single trailing space/tab a soft break folds away
        // (spec §6.6/§6.7); neither should remain as extra visible whitespace,
        // but both stay addressable source bytes disclosed like other markup.
        for (first_line, second_line, expected_first, expected_second, hidden) in [
            ("foo  \n", "     bar", "foo", "bar", &[(6, 11)][..]),
            ("foo\\\n", "     bar", "foo", "bar", &[(5, 10)][..]),
            ("foo \n", " baz", "foo", "baz", &[(3, 4), (5, 6)][..]),
        ] {
            let base = 12;
            let first_range = SourceRange::new(base, base + first_line.len());
            let second_range =
                SourceRange::new(first_range.end.0, first_range.end.0 + second_line.len());
            let lines = [
                BlockLine {
                    line: 0,
                    range: first_range,
                    text: first_line,
                    disclosure: None,
                },
                BlockLine {
                    line: 1,
                    range: second_range,
                    text: second_line,
                    disclosure: None,
                },
            ];
            let joined = parse_joined_block(&lines, Revision(1));
            let mut out = Vec::new();
            present_joined_run(
                &lines,
                Revision(1),
                26.0,
                &(0..2),
                Some(&joined),
                None,
                &mut out,
            );
            assert_eq!(
                out[0].visual_text, expected_first,
                "source: {first_line:?}{second_line:?}"
            );
            assert_eq!(
                out[1].visual_text, expected_second,
                "source: {first_line:?}{second_line:?}"
            );
            for &(start, end) in hidden {
                let hidden_range = SourceRange::new(base + start, base + end);
                assert!(
                    out.iter().any(|line| {
                        line.source_map.segments.iter().any(|segment| {
                            segment.source_range == hidden_range
                                && segment.visibility == Visibility::HiddenMarkup
                        })
                    }),
                    "expected {hidden_range:?} hidden but addressable for {first_line:?}{second_line:?}"
                );
            }
        }
    }

    #[test]
    fn line_break_padding_hides_lazy_continuation_indentation_from_rendered_presentation() {
        // Lazy continuation lines drop some (quote) or all (list, under-indented)
        // ancestor container prefixes, but pulldown-cmark still folds them into
        // the same paragraph as an ordinary soft break, so their leading
        // whitespace must disappear from inactive visual text the same way — all
        // while every source byte, including the hidden padding, stays mapped.
        // The list fixture's opening line carries the synthesized bullet
        // marker even while inactive (#126 contract); the quote fixture has
        // no such marker since `>` markup is hidden rather than synthesized.
        for (first_line, second_line, hidden_marker, hidden_padding, expected_first) in [
            ("> foo\n", "  bar", (0, 2), (6, 8), "foo"),
            ("- foo\n", " bar", (0, 2), (6, 7), "• foo"),
        ] {
            let base = 12;
            let first_range = SourceRange::new(base, base + first_line.len());
            let second_range =
                SourceRange::new(first_range.end.0, first_range.end.0 + second_line.len());
            let lines = [
                BlockLine {
                    line: 0,
                    range: first_range,
                    text: first_line,
                    disclosure: None,
                },
                BlockLine {
                    line: 1,
                    range: second_range,
                    text: second_line,
                    disclosure: None,
                },
            ];
            let joined = parse_joined_block(&lines, Revision(1));
            let mut out = Vec::new();
            present_joined_run(
                &lines,
                Revision(1),
                26.0,
                &(0..2),
                Some(&joined),
                None,
                &mut out,
            );
            assert_eq!(
                out[0].visual_text, expected_first,
                "source: {first_line:?}{second_line:?}"
            );
            assert_eq!(
                out[1].visual_text, "bar",
                "source: {first_line:?}{second_line:?}"
            );
            for (line, range) in [(&lines[0], first_range), (&lines[1], second_range)] {
                assert!(
                    segments_tile_range(range, &out[line.line].source_map.segments),
                    "source bytes must stay fully mapped for line {}: {first_line:?}{second_line:?}",
                    line.line
                );
            }
            for &(start, end) in &[hidden_marker, hidden_padding] {
                let hidden_range = SourceRange::new(base + start, base + end);
                assert!(
                    out.iter().any(|line| {
                        line.source_map.segments.iter().any(|segment| {
                            segment.source_range == hidden_range
                                && segment.visibility == Visibility::HiddenMarkup
                        })
                    }),
                    "expected {hidden_range:?} hidden but addressable for {first_line:?}{second_line:?}"
                );
            }
        }
    }

    #[test]
    fn shared_heading_kinds_follow_each_physical_lines_source_range() {
        for (source, expected) in [
            (
                "> ## first\n> plain\n> ### second\n\n",
                &[
                    BlockKind::Heading(2),
                    BlockKind::Quote,
                    BlockKind::Heading(3),
                    BlockKind::Paragraph,
                ][..],
            ),
            (
                "- ## first\n  plain\n  ### second\n",
                &[
                    BlockKind::Heading(2),
                    BlockKind::ListItem,
                    BlockKind::Heading(3),
                ][..],
            ),
        ] {
            let mut offset = 50;
            let lines = source
                .split_inclusive('\n')
                .enumerate()
                .map(|(line, text)| {
                    let range = SourceRange::new(offset, offset + text.len());
                    offset = range.end.0;
                    BlockLine {
                        line,
                        range,
                        text,
                        disclosure: None,
                    }
                })
                .collect::<Vec<_>>();
            let joined = parse_joined_block(&lines, Revision(1));
            let mut wide = Vec::new();
            present_joined_run(
                &lines,
                Revision(1),
                26.0,
                &(0..lines.len()),
                None,
                None,
                &mut wide,
            );
            for (index, &expected_kind) in expected.iter().enumerate() {
                assert_eq!(
                    wide[index].kind, expected_kind,
                    "line {index} in {source:?}"
                );
                let mut narrow = Vec::new();
                present_joined_run(
                    &lines[index..index + 1],
                    Revision(1),
                    26.0,
                    &(index..index + 1),
                    Some(&joined),
                    None,
                    &mut narrow,
                );
                assert_eq!(narrow[0].kind, expected_kind);
                assert_eq!(narrow[0].estimated_height, wide[index].estimated_height);
                assert_eq!(narrow[0].visual_text, wide[index].visual_text);
                assert_eq!(
                    narrow[0].source_map.segments,
                    wide[index].source_map.segments
                );
            }
        }
    }

    #[test]
    fn code_padding_inside_containers_preserves_shared_projection_and_source() {
        for (source, expected) in [
            ("> `\n> x\n> `", vec!["", "x", ""]),
            ("> `\r\n> x\r\n> `", vec!["", "x", ""]),
            ("> > `\n> > x\n> > `", vec!["", "x", ""]),
            ("> ` \n>  `", vec![" ", " "]),
            ("> ` x\n> y `", vec!["x", "y"]),
            // The list item's opening line carries the synthesized bullet
            // marker even while inactive (#126 contract), unlike the quote
            // fixtures above whose `>` markup is real source text instead.
            ("- `\n  x\n  `", vec!["• ", "x", ""]),
        ] {
            let mut offset = 50;
            let lines: Vec<_> = source
                .split_inclusive('\n')
                .enumerate()
                .map(|(line, text)| {
                    let start = offset;
                    offset += text.len();
                    BlockLine {
                        line,
                        range: SourceRange::new(start, offset),
                        text,
                        disclosure: None,
                    }
                })
                .collect();
            let block = IndexedBlock {
                ordinal: 0,
                id: BlockId(0),
                kind: if source.starts_with('>') {
                    NodeKind::Quote
                } else {
                    NodeKind::List { start: None }
                },
                source_range: SourceRange::new(50, offset),
                revision: Revision(1),
                confidence: Confidence::Formal,
                line_count: lines.len(),
                leading_content_lines: 0,
            };
            let joined = parse_joined_block(&lines, Revision(1));
            let window = BlockWindow {
                span: 0..lines.len(),
                trailing_blank_lines: 0,
                lines: &lines,
                clipped_fence_lines: &[],
                zero_height_fence_rows_before: 0,
                zero_height_fence_rows_after: 0,
                render: 0..lines.len(),
                joined: Some(&joined),
                block_disclosure: None,
            };
            let wide = present_block(&block, Revision(1), &window, 26.0);
            for (index, line) in wide.lines.iter().enumerate() {
                assert_eq!(
                    line.visual_text, expected[index],
                    "{source:?}, line {index}"
                );
                assert_ne!(line.kind, BlockKind::Unsupported);
                assert!(segments_tile_range(
                    lines[index].range,
                    &line.source_map.segments
                ));
                let narrow_window = BlockWindow {
                    lines: &lines[index..index + 1],
                    render: index..index + 1,
                    ..window.clone()
                };
                let narrow = present_block(&block, Revision(1), &narrow_window, 26.0);
                assert_eq!(
                    narrow.lines[0].source_map.segments,
                    line.source_map.segments
                );
            }
            let active_window = BlockWindow {
                block_disclosure: Some(SourceRange::empty(50 + source.find('`').unwrap() + 1)),
                ..window
            };
            let active = present_block(&block, Revision(1), &active_window, 26.0);
            for (index, line) in active.lines.iter().enumerate() {
                assert_eq!(
                    line.visual_text,
                    lines[index].text.trim_end_matches(['\r', '\n'])
                );
            }
        }
    }

    #[test]
    fn average_line_height_ignores_collapsed_fence_rows() {
        let opening = present_fenced_code_opening_line(
            0,
            Revision(1),
            SourceRange::new(0, 4),
            "```\n",
            26.0,
            None,
        );
        let body = present_fenced_code_content_line(
            1,
            Revision(1),
            SourceRange::new(4, 9),
            "code\n",
            26.0,
        );
        let closing = present_fenced_code_closing_line(
            2,
            Revision(1),
            SourceRange::new(9, 13),
            "```\n",
            26.0,
            None,
        );
        assert_eq!(opening.height(), 0.0);
        assert!(body.height() > 0.0);
        assert_eq!(closing.height(), 0.0);

        let block = VisualBlock {
            id: BlockId(0),
            kind: BlockKind::CodeBlock,
            source_range: SourceRange::new(0, 13),
            revision: Revision(1),
            confidence: Confidence::Formal,
            span: 0..3,
            lines: vec![opening, body, closing],
            table_projection: None,
            lines_before: 0,
            lines_after: 0,
            zero_height_lines_before: 0,
            zero_height_lines_after: 0,
            line_height: 26.0,
        };
        let layout = layout_block(&block, 400.0, &testing::FixedAdvanceShaper::default());

        assert_eq!(
            layout.average_line_height(),
            Some(code_line_height(26.0)),
            "collapsed fence rows must not lower the visual-row estimate"
        );
    }

    #[test]
    fn quote_disclosure_does_not_expand_inactive_nested_quote_or_inline_markers() {
        let source = "> outer\n> > **nested**";
        let start = 40;
        let visual = present_markdown_with_disclosure(
            0,
            Revision(1),
            SourceRange::new(start, start + source.len()),
            source,
            26.0,
            Some(SourceRange::empty(start + 4)),
        );
        assert_eq!(visual.visual_text, "> outer\n> nested");
    }

    #[test]
    fn list_marker_discloses_for_every_ancestor_whose_source_range_the_caret_enters() {
        // A nested/mixed fixture (#126): an outer item whose own directly-owned
        // content is interrupted by a nested child list with its own second
        // paragraph, followed by the outer item's own second paragraph, then
        // an unrelated top-level sibling.
        let source = "- outer\n\n  - inner\n\n    second\n\n  outer second\n\n- sibling";
        let start = 50;
        let range = SourceRange::new(start, start + source.len());
        let outer_line = 0;
        let inner_line = 2;
        let sibling_line = 8;
        let line = |visual_text: &str, index: usize| -> String {
            visual_text.split('\n').nth(index).unwrap().to_string()
        };
        let present = |disclosure: Option<SourceRange>| {
            present_markdown_with_disclosure(0, Revision(1), range, source, 26.0, disclosure)
        };

        // Nothing disclosed: both markers collapse to their synthesized bullet.
        let collapsed = present(None);
        assert_eq!(line(&collapsed.visual_text, outer_line), "\u{2022} outer");
        assert_eq!(line(&collapsed.visual_text, inner_line), "\u{2022} inner");
        assert_eq!(
            line(&collapsed.visual_text, sibling_line),
            "\u{2022} sibling"
        );

        // A caret inside the nested child's own first line discloses both the
        // nested item's own marker and its ancestor's, since the caret's
        // source range intersects both owners' source ranges.
        let at_inner = start + source.find("inner").unwrap();
        let inner_caret = present(Some(SourceRange::empty(at_inner)));
        assert_eq!(line(&inner_caret.visual_text, outer_line), "- outer");
        assert_eq!(line(&inner_caret.visual_text, inner_line), "  - inner");
        assert_eq!(
            line(&inner_caret.visual_text, sibling_line),
            "\u{2022} sibling"
        );

        // A selection inside the nested child's own second paragraph — not on
        // the marker's own line — still discloses the nested item's marker and
        // every ancestor whose source range contains that paragraph.
        let second_start = start + source.find("second").unwrap();
        let second_selection = present(Some(SourceRange::new(
            second_start,
            second_start + "second".len(),
        )));
        assert_eq!(line(&second_selection.visual_text, outer_line), "- outer");
        assert_eq!(line(&second_selection.visual_text, inner_line), "  - inner");
        assert_eq!(
            line(&second_selection.visual_text, sibling_line),
            "\u{2022} sibling"
        );

        // A caret back in the outer item's own paragraph, past the nested
        // child, discloses only the outer marker: the caret's source range no
        // longer intersects the nested item's own source range.
        let at_outer_second = start + source.find("outer second").unwrap();
        let outer_second_caret = present(Some(SourceRange::empty(at_outer_second)));
        assert_eq!(line(&outer_second_caret.visual_text, outer_line), "- outer");
        assert_eq!(
            line(&outer_second_caret.visual_text, inner_line),
            "  \u{2022} inner"
        );
        assert_eq!(
            line(&outer_second_caret.visual_text, sibling_line),
            "\u{2022} sibling"
        );

        // A caret in the unrelated sibling item discloses only its own
        // marker, never the outer or nested ancestors of a different subtree.
        let at_sibling = start + source.find("sibling").unwrap();
        let sibling_caret = present(Some(SourceRange::empty(at_sibling)));
        assert_eq!(
            line(&sibling_caret.visual_text, outer_line),
            "\u{2022} outer"
        );
        assert_eq!(
            line(&sibling_caret.visual_text, inner_line),
            "\u{2022} inner"
        );
        assert_eq!(line(&sibling_caret.visual_text, sibling_line), "- sibling");
    }

    #[test]
    fn list_item_boundary_discloses_only_the_item_that_starts_there() {
        let source = "- first\n- second";
        let range = SourceRange::new(0, source.len());
        let second_start = source.find("- second").expect("second list item");
        let presented = present_markdown_with_disclosure(
            0,
            Revision(1),
            range,
            source,
            26.0,
            Some(SourceRange::empty(second_start)),
        );
        let lines = presented.visual_text.split('\n').collect::<Vec<_>>();

        assert_eq!(lines, &["• first", "- second"]);
    }

    #[test]
    fn list_structural_prefix_synthesizes_columns_and_discloses_the_owning_items_raw_bytes() {
        // A two-digit ordered marker ("10. ") is four columns wide; the
        // continuation line reaches that width with a single leading tab, so
        // the inactive synthesized replacement (four plain spaces) and the
        // disclosed original byte (one tab character) are visibly different,
        // unlike the common all-spaces case.
        let source = "10. item\n\tcontinued\n";
        let start = 40;
        let range = SourceRange::new(start, start + source.len());
        let line = |visual_text: &str, index: usize| -> String {
            visual_text.split('\n').nth(index).unwrap().to_string()
        };
        let present = |disclosure: Option<SourceRange>| {
            present_markdown_with_disclosure(0, Revision(1), range, source, 26.0, disclosure)
        };

        let collapsed = present(None);
        assert_eq!(line(&collapsed.visual_text, 1), "continued");

        let at_continued = start + source.find("continued").unwrap();
        let disclosed = present(Some(SourceRange::empty(at_continued)));
        assert_eq!(line(&disclosed.visual_text, 1), "\tcontinued");
    }

    #[test]
    fn opening_list_structural_prefix_uses_source_mapping_without_fake_text() {
        let source = "  - item\n";
        let start = 40;
        let range = SourceRange::new(start, start + source.len());
        let collapsed = present_markdown_with_disclosure(0, Revision(1), range, source, 26.0, None);
        assert_eq!(collapsed.visual_text, "• item\n");
        let prefix = collapsed
            .list
            .as_ref()
            .and_then(|list| list.structural_prefixes.first())
            .expect("opening indentation is owned by the item");
        assert_eq!(prefix.source_range, SourceRange::new(start, start + 2));
        assert_eq!(prefix.columns, 2);

        let item = start + source.find("item").expect("item in source");
        let disclosed = present_markdown_with_disclosure(
            0,
            Revision(1),
            range,
            source,
            26.0,
            Some(SourceRange::empty(item)),
        );
        assert_eq!(disclosed.visual_text, source);
    }

    #[test]
    fn opening_list_marker_padding_is_hidden_but_disclosed_with_the_item() {
        for (source, expected) in [("-   item\n", "• item\n"), ("1.    item\n", "1. item\n")] {
            let start = 40;
            let range = SourceRange::new(start, start + source.len());
            let collapsed =
                present_markdown_with_disclosure(0, Revision(1), range, source, 26.0, None);
            assert_eq!(collapsed.visual_text, expected, "source: {source:?}");

            let item = start + source.find("item").expect("item in source");
            let disclosed = present_markdown_with_disclosure(
                0,
                Revision(1),
                range,
                source,
                26.0,
                Some(SourceRange::empty(item)),
            );
            assert_eq!(disclosed.visual_text, source, "source: {source:?}");
        }
    }

    #[test]
    fn ordered_list_labels_use_owner_start_plus_sibling_position() {
        for (source, expected_labels) in [
            // Loose/irregular source digits never leak into the synthesized
            // label; only the first item's marker sets the owner's `start`,
            // and later items number sequentially from it regardless of
            // their own written digits.
            ("3. a\n1. b\n1. c", vec!["3. a", "4. b", "5. c"]),
            // `0` is a valid ordered-list start.
            ("0. a\n0. b", vec!["0. a", "1. b"]),
        ] {
            let start = 40;
            let range = SourceRange::new(start, start + source.len());
            let presented =
                present_markdown_with_disclosure(0, Revision(1), range, source, 26.0, None);
            let lines: Vec<_> = presented.visual_text.split('\n').collect();
            assert_eq!(lines, expected_labels, "source: {source:?}");
        }
    }

    #[test]
    fn nested_lists_number_independently_of_their_ancestors_and_siblings() {
        // A mixed fixture: an ordered outer list whose first item contains a
        // nested bullet list, followed by the outer list's second item. The
        // nested items are siblings of each other only, and the outer
        // second item's ordinal must not count the nested items at all.
        let source = "3. outer-a\n   - nested-a\n   - nested-b\n1. outer-b";
        let start = 40;
        let range = SourceRange::new(start, start + source.len());
        let presented = present_markdown_with_disclosure(0, Revision(1), range, source, 26.0, None);
        let lines: Vec<_> = presented.visual_text.split('\n').collect();
        assert_eq!(
            lines,
            vec![
                "3. outer-a",
                "\u{2022} nested-a",
                "\u{2022} nested-b",
                "4. outer-b",
            ]
        );
    }

    #[test]
    fn list_rows_expose_semantic_geometry_policy_without_reparsing_in_layout() {
        let source = "- parent\n\n  3. child\n\n     - grandchild\n  8. child2\n\n  parent second\n- sibling";
        let mut offset = 0;
        let lines = source
            .split_inclusive('\n')
            .enumerate()
            .map(|(line, text)| {
                let range = SourceRange::new(offset, offset + text.len());
                offset = range.end.0;
                BlockLine {
                    line,
                    range,
                    text,
                    disclosure: None,
                }
            })
            .collect::<Vec<_>>();
        let joined = parse_joined_block(&lines, Revision(1));
        let mut presented = Vec::new();
        present_joined_run(
            &lines,
            Revision(1),
            26.0,
            &(0..lines.len()),
            Some(&joined),
            None,
            &mut presented,
        );

        let rows = presented
            .iter()
            .filter_map(|line| line.list.as_ref().map(|list| (line, list)))
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 9);

        let (parent, parent_meta) = rows[0];
        assert_eq!(parent.visual_text, "\u{2022} parent");
        assert_eq!(parent_meta.owner.depth, 1);
        assert_eq!(parent_meta.owner.marker_kind, ListMarkerKind::Bullet);
        assert_eq!(parent_meta.owner.ordinal, 0);
        assert_eq!(parent_meta.owner.alignment.max_marker_label, "\u{2022} ");
        assert_eq!(parent_meta.owner.alignment.max_marker_columns, 2);
        assert_eq!(parent_meta.role, ListRowRole::Opening);
        let parent_marker = parent_meta.marker.as_ref().expect("parent marker");
        assert!(parent_marker.synthesized);
        assert_eq!(parent_marker.label, "\u{2022} ");
        assert_eq!(parent_marker.anchor, parent_marker.source_range.start);
        assert_eq!(
            parent_meta.body_visual_start,
            parent_marker.visual_range.end
        );

        let (_, child_meta) = rows[2];
        assert_eq!(child_meta.owner.depth, 2);
        assert_eq!(child_meta.owner.marker_kind, ListMarkerKind::Ordered);
        assert_eq!(child_meta.owner.start, Some(3));
        assert_eq!(child_meta.owner.ordinal, 0);
        assert_eq!(child_meta.owner.alignment.max_marker_label, "3. ");
        assert_eq!(child_meta.owner.alignment.max_marker_columns, 3);
        assert_eq!(child_meta.role, ListRowRole::Opening);

        let (_, grandchild_meta) = rows[4];
        assert_eq!(grandchild_meta.owner.depth, 3);
        assert_eq!(grandchild_meta.owner.marker_kind, ListMarkerKind::Bullet);
        assert_eq!(grandchild_meta.role, ListRowRole::Opening);
        assert_eq!(grandchild_meta.structural_prefixes.len(), 2);

        let (_, second_child_meta) = rows[5];
        assert_eq!(second_child_meta.owner.list_id, child_meta.owner.list_id);
        assert_eq!(second_child_meta.owner.ordinal, 1);
        assert_eq!(second_child_meta.marker.as_ref().unwrap().label, "4. ");

        let (_, parent_second_meta) = rows[7];
        assert_eq!(parent_second_meta.owner.item_id, parent_meta.owner.item_id);
        assert_eq!(
            parent_second_meta.role,
            ListRowRole::ParentParagraphAfterNestedList { ordinal: 1 }
        );
        assert_eq!(parent_second_meta.owner.depth, 1);

        let (_, sibling_meta) = rows[8];
        assert_eq!(sibling_meta.owner.list_id, parent_meta.owner.list_id);
        assert_eq!(sibling_meta.owner.ordinal, 1);
        assert_eq!(sibling_meta.role, ListRowRole::Opening);
    }

    fn empty_list_editing_block(
        owner: ListOwnerMetadata,
        offset: usize,
    ) -> (VisualBlock, SourceMap) {
        let opening = present_plain(0, Revision(1), SourceRange::empty(0), "");
        let empty = present_plain(1, Revision(1), SourceRange::empty(offset), "");
        let source_map = empty.source_map.clone();
        let mut block = VisualBlock {
            id: BlockId(0),
            kind: BlockKind::Paragraph,
            source_range: SourceRange::new(0, offset),
            revision: Revision(1),
            confidence: Confidence::Formal,
            span: 0..2,
            lines: vec![opening, empty],
            table_projection: None,
            lines_before: 0,
            lines_after: 0,
            zero_height_lines_before: 0,
            zero_height_lines_after: 0,
            line_height: 26.0,
        };
        let context = ListEditingContext {
            offset: SourceOffset(offset),
            owner,
            caret_origin: ListCaretOrigin::Marker,
            line_ending_len: 0,
            indentation: String::new(),
        };
        assert!(apply_list_editing_context(&mut block, &context));
        (block, source_map)
    }

    #[test]
    fn list_editing_context_preserves_source_mapping_and_places_marker_caret() {
        let owner = ListOwnerMetadata {
            list_id: ListId(1),
            item_id: ListItemId(1),
            marker_kind: ListMarkerKind::Bullet,
            start: None,
            ordinal: 0,
            depth: 2,
            alignment: ListAlignment {
                max_marker_label: "• ".to_owned(),
                max_marker_columns: 2,
                marker_labels: vec!["• ".to_owned()].into(),
            },
        };
        let (mut block, original_source_map) = empty_list_editing_block(owner.clone(), 6);
        let line = &block.lines[1];
        assert!(line.visual_text.is_empty());
        assert_eq!(line.source_map, original_source_map);
        assert_eq!(
            line.source_map
                .source_to_visual(SourceOffset(6), Bias::After)
                .map(|candidate| candidate.visual_offset),
            Some(VisualOffset(0))
        );
        assert_eq!(line.list.as_ref().unwrap().owner, owner);

        let marker_layout = layout_block(&block, 400.0, &testing::FixedAdvanceShaper::default());
        let marker_point = marker_layout
            .point_for_source(
                &block,
                SourceOffset(6),
                &testing::FixedAdvanceShaper::default(),
            )
            .expect("empty list line owns the caret");
        assert_eq!(marker_point.x, LIST_DEPTH_INDENT);
        assert_eq!(
            marker_layout.source_for_point(
                &block,
                marker_point.x,
                marker_point.y,
                &testing::FixedAdvanceShaper::default()
            ),
            Some(SourceOffset(6))
        );

        block.lines[1].list.as_mut().unwrap().empty_caret_origin = Some(ListCaretOrigin::Body);
        let body_layout = layout_block(&block, 400.0, &testing::FixedAdvanceShaper::default());
        let body_point = body_layout
            .point_for_source(
                &block,
                SourceOffset(6),
                &testing::FixedAdvanceShaper::default(),
            )
            .expect("body list line owns the caret");
        assert_eq!(body_point.x, LIST_DEPTH_INDENT + 16.0);
        assert_eq!(
            body_layout.source_for_point(
                &block,
                body_point.x,
                body_point.y,
                &testing::FixedAdvanceShaper::default()
            ),
            Some(SourceOffset(6))
        );
    }

    #[test]
    fn list_editing_context_owns_existing_line_ending_without_losing_source_bytes() {
        let owner = ListOwnerMetadata {
            list_id: ListId(1),
            item_id: ListItemId(1),
            marker_kind: ListMarkerKind::Bullet,
            start: None,
            ordinal: 0,
            depth: 2,
            alignment: ListAlignment {
                max_marker_label: "• ".to_owned(),
                max_marker_columns: 2,
                marker_labels: vec!["• ".to_owned()].into(),
            },
        };

        for ending in ["\n", "\r\n", "\r"] {
            let offset = 6;
            let mut empty = present_plain(
                1,
                Revision(1),
                SourceRange::new(offset, offset + ending.len()),
                ending,
            );
            let original_source_map = empty.source_map.clone();
            // `presented_block` trims line-ending bytes from visual text while
            // retaining them in the source map. Mirror that render contract
            // in this focused range/geometry test.
            empty.visual_text.clear();
            let mut block = VisualBlock {
                id: BlockId(0),
                kind: BlockKind::Paragraph,
                source_range: SourceRange::new(0, offset + ending.len()),
                revision: Revision(1),
                confidence: Confidence::Formal,
                span: 0..2,
                lines: vec![
                    present_plain(0, Revision(1), SourceRange::empty(0), ""),
                    empty,
                ],
                table_projection: None,
                lines_before: 0,
                lines_after: 0,
                zero_height_lines_before: 0,
                zero_height_lines_after: 0,
                line_height: 26.0,
            };
            let context = ListEditingContext {
                offset: SourceOffset(offset),
                owner: owner.clone(),
                caret_origin: ListCaretOrigin::Marker,
                line_ending_len: ending.len(),
                indentation: String::new(),
            };

            assert!(apply_list_editing_context(&mut block, &context));
            let line = &block.lines[1];
            assert_eq!(
                line.source_range,
                SourceRange::new(offset, offset + ending.len())
            );
            assert_eq!(line.source_map, original_source_map);
            assert_eq!(
                line.source_map
                    .source_to_visual(SourceOffset(offset), Bias::After)
                    .map(|candidate| candidate.visual_offset),
                Some(VisualOffset(0))
            );

            for (origin, expected_x) in [
                (ListCaretOrigin::Marker, LIST_DEPTH_INDENT),
                (ListCaretOrigin::Body, LIST_DEPTH_INDENT + 16.0),
            ] {
                block.lines[1].list.as_mut().unwrap().empty_caret_origin = Some(origin);
                let layout = layout_block(&block, 400.0, &testing::FixedAdvanceShaper::default());
                let point = layout
                    .point_for_source(
                        &block,
                        SourceOffset(offset),
                        &testing::FixedAdvanceShaper::default(),
                    )
                    .expect("line ending-only row owns the caret");
                assert_eq!(point.x, expected_x, "line ending bytes: {ending:?}");
                assert_eq!(
                    layout.source_for_point(
                        &block,
                        point.x,
                        point.y,
                        &testing::FixedAdvanceShaper::default(),
                    ),
                    Some(SourceOffset(offset)),
                    "line ending bytes: {ending:?}"
                );
            }
        }
    }

    #[test]
    fn list_editing_context_uses_actual_multi_digit_marker_width() {
        let owner = ListOwnerMetadata {
            list_id: ListId(10),
            item_id: ListItemId(10),
            marker_kind: ListMarkerKind::Ordered,
            start: Some(10),
            ordinal: 0,
            depth: 1,
            alignment: ListAlignment {
                max_marker_label: "10. ".to_owned(),
                max_marker_columns: 4,
                marker_labels: vec!["10. ".to_owned()].into(),
            },
        };
        let (mut block, _) = empty_list_editing_block(owner, 5);
        block.lines[1].list.as_mut().unwrap().empty_caret_origin = Some(ListCaretOrigin::Body);
        let layout = layout_block(&block, 400.0, &testing::FixedAdvanceShaper::default());
        let point = layout
            .point_for_source(
                &block,
                SourceOffset(5),
                &testing::FixedAdvanceShaper::default(),
            )
            .expect("ordered empty line owns the caret");
        assert_eq!(point.x, 32.0);
    }

    #[test]
    fn list_row_metadata_distinguishes_continuation_and_loose_paragraphs() {
        let source = "- first\ncontinued\n\n  second\n\n  third\n- next";
        let mut offset = 0;
        let lines = source
            .split_inclusive('\n')
            .enumerate()
            .map(|(line, text)| {
                let range = SourceRange::new(offset, offset + text.len());
                offset = range.end.0;
                BlockLine {
                    line,
                    range,
                    text,
                    disclosure: None,
                }
            })
            .collect::<Vec<_>>();
        let joined = parse_joined_block(&lines, Revision(1));
        let mut presented = Vec::new();
        present_joined_run(
            &lines,
            Revision(1),
            26.0,
            &(0..lines.len()),
            Some(&joined),
            None,
            &mut presented,
        );

        let metadata = |line: usize| presented[line].list.as_ref().expect("list row");
        assert_eq!(metadata(0).role, ListRowRole::Opening);
        assert_eq!(metadata(1).role, ListRowRole::Continuation);
        assert!(metadata(1).structural_prefixes.is_empty());
        assert_eq!(metadata(3).role, ListRowRole::LooseParagraph { ordinal: 1 });
        assert_eq!(metadata(5).role, ListRowRole::LooseParagraph { ordinal: 2 });
        assert_eq!(metadata(6).role, ListRowRole::Opening);
        assert_eq!(metadata(6).owner.ordinal, 1);
        assert_eq!(metadata(0).owner.item_id, metadata(1).owner.item_id);
        assert_eq!(metadata(3).owner.item_id, metadata(0).owner.item_id);
    }

    #[test]
    fn ordered_row_metadata_uses_inactive_labels_for_shared_alignment() {
        let source = "9. first\n1. second";
        let mut offset = 0;
        let lines = source
            .split_inclusive('\n')
            .enumerate()
            .map(|(line, text)| {
                let range = SourceRange::new(offset, offset + text.len());
                offset = range.end.0;
                BlockLine {
                    line,
                    range,
                    text,
                    disclosure: None,
                }
            })
            .collect::<Vec<_>>();
        let joined = parse_joined_block(&lines, Revision(1));
        let mut presented = Vec::new();
        present_joined_run(
            &lines,
            Revision(1),
            26.0,
            &(0..lines.len()),
            Some(&joined),
            None,
            &mut presented,
        );

        let first = presented[0].list.as_ref().expect("first list row");
        let second = presented[1].list.as_ref().expect("second list row");
        assert_eq!(first.marker.as_ref().unwrap().label, "9. ");
        assert_eq!(second.marker.as_ref().unwrap().label, "10. ");
        assert_eq!(first.owner.list_id, second.owner.list_id);
        assert_eq!(first.owner.alignment.max_marker_label, "10. ");
        assert_eq!(first.owner.alignment.max_marker_columns, 4);
        assert_eq!(second.owner.alignment, first.owner.alignment);
    }

    #[test]
    fn disclosed_ordered_marker_metadata_keeps_leading_zero_and_delimiter() {
        let source = "003) first\n9) second";
        let mut offset = 0;
        let lines = source
            .split_inclusive('\n')
            .enumerate()
            .map(|(line, text)| {
                let range = SourceRange::new(offset, offset + text.len());
                offset = range.end.0;
                BlockLine {
                    line,
                    range,
                    text,
                    disclosure: (line == 0).then_some(SourceRange::empty(0)),
                }
            })
            .collect::<Vec<_>>();
        let joined = parse_joined_block(&lines, Revision(1));
        let mut presented = Vec::new();
        present_joined_run(
            &lines,
            Revision(1),
            26.0,
            &(0..lines.len()),
            Some(&joined),
            None,
            &mut presented,
        );

        let disclosed = presented[0].list.as_ref().expect("disclosed list row");
        assert_eq!(disclosed.marker.as_ref().unwrap().label, "003) ");
        assert!(!disclosed.marker.as_ref().unwrap().synthesized);
        assert_eq!(
            presented[1]
                .list
                .as_ref()
                .unwrap()
                .marker
                .as_ref()
                .unwrap()
                .label,
            "4. "
        );
        assert_eq!(disclosed.owner.start, Some(3));
        assert_eq!(disclosed.owner.alignment.max_marker_label, "3. ");
    }

    #[test]
    fn list_item_label_returns_none_when_the_ordinal_table_omits_the_item() {
        // `list_item_label` must read only the precomputed ordinal table, not
        // fall back to `MarkdownTree::list_item_ordinal`'s preceding-sibling
        // scan when an entry is missing — a fallback here would mask a
        // construction gap and silently reintroduce a per-line sibling walk.
        let source = "1. a\n2. b\n3. c\n";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        let (item, _) = parsed
            .tree
            .blocks()
            .find(|(_, node)| matches!(node.kind, NodeKind::ListItem { .. }))
            .expect("a list item");
        let populated = list_item_ordinals(&parsed.tree);
        assert_eq!(
            list_item_label(&parsed, &populated, item),
            Some("1. ".to_owned())
        );
        let empty = vec![None; parsed.tree.len()];
        assert_eq!(list_item_label(&parsed, &empty, item), None);
    }

    #[test]
    fn large_ordered_list_viewport_labels_reuse_one_shared_parse_at_middle_and_tail() {
        // 100,000 flat items in one ordered list. If a visible item's label
        // still re-derived its ordinal via `MarkdownTree::list_item_ordinal`
        // per line (a scan of every preceding sibling), presenting only the
        // handful of lines drawn from the middle and the tail below would
        // still be correct but would cost proportional to each item's
        // position rather than to the lines actually presented; this test
        // fixes correctness at that position, and
        // `list_item_label_returns_none_when_the_ordinal_table_omits_the_item`
        // fixes structurally that no such fallback scan exists at all.
        const ITEMS: usize = 100_000;
        let start_value = 3u64;
        let source = (0..ITEMS)
            .map(|i| format!("{start_value}. item {i}\n"))
            .collect::<String>();
        let mut cursor = 40;
        let lines = source
            .split_inclusive('\n')
            .enumerate()
            .map(|(line, text)| {
                let range = SourceRange::new(cursor, cursor + text.len());
                cursor = range.end.0;
                BlockLine {
                    line,
                    range,
                    text,
                    disclosure: None,
                }
            })
            .collect::<Vec<_>>();
        let joined = parse_joined_block(&lines, Revision(9));
        let shared = SharedParse {
            parsed: &joined.parsed,
            projection: &joined.projection,
            list_projection: None,
        };
        let middle = ITEMS / 2;
        for &index in &[
            middle - 2,
            middle - 1,
            middle,
            ITEMS - 3,
            ITEMS - 2,
            ITEMS - 1,
        ] {
            let line = &lines[index];
            let presented = present_markdown_from_parse(
                line.line as u64,
                Revision(9),
                line.range,
                line.text,
                26.0,
                None,
                &shared,
            );
            assert_eq!(
                presented.visual_text,
                format!("{}. item {index}\n", start_value + index as u64),
                "item {index}"
            );
            assert!(segments_tile_range(
                line.range,
                &presented.source_map.segments
            ));
            assert!(
                presented.source_map.segments.capacity() <= 5,
                "mapping storage must depend on this line's own markers, not item {index}'s position"
            );
        }
    }

    #[test]
    fn quote_disclosure_uses_prefix_ownership_across_viewports() {
        let texts = ["> **first**\n", "> second"];
        let start = 50;
        let split = start + texts[0].len();
        let lines = [
            BlockLine {
                line: 0,
                range: SourceRange::new(start, split),
                text: texts[0],
                disclosure: None,
            },
            BlockLine {
                line: 1,
                range: SourceRange::new(split, split + texts[1].len()),
                text: texts[1],
                disclosure: None,
            },
        ];
        let block = IndexedBlock {
            ordinal: 0,
            id: BlockId(0),
            kind: NodeKind::Quote,
            source_range: SourceRange::new(start, lines[1].range.end.0),
            revision: Revision(1),
            confidence: Confidence::Formal,
            line_count: 2,
            leading_content_lines: 0,
        };
        let joined = parse_joined_block(&lines, Revision(1));
        // Empty ranges represent carets; non-empty ranges also cover selection
        // and IME disclosure. Neither should disclose the inactive strong span.
        for disclosure in [
            SourceRange::empty(split + 4),
            SourceRange::new(split + 3, split + 6),
        ] {
            let wide_window = BlockWindow {
                span: 0..2,
                trailing_blank_lines: 0,
                lines: &lines,
                clipped_fence_lines: &[],
                zero_height_fence_rows_before: 0,
                zero_height_fence_rows_after: 0,
                render: 0..2,
                joined: Some(&joined),
                block_disclosure: Some(disclosure),
            };
            let wide = present_block(&block, Revision(1), &wide_window, 26.0);
            assert_eq!(wide.lines[0].visual_text, "> first");
            assert_eq!(wide.lines[1].visual_text, "> second");
            for index in 0..2 {
                let narrow_window = BlockWindow {
                    lines: &lines[index..index + 1],
                    render: index..index + 1,
                    ..wide_window.clone()
                };
                let narrow = present_block(&block, Revision(1), &narrow_window, 26.0);
                assert_eq!(narrow.lines[0].visual_text, wide.lines[index].visual_text);
                assert_eq!(
                    narrow.lines[0].source_map.segments,
                    wide.lines[index].source_map.segments
                );
            }
        }
    }

    #[test]
    fn code_fence_disclosure_only_expands_the_touched_fence_marker() {
        let source = "- item\n  ```rust\n  code\n  ```";
        let base = 0;
        let mut line_start = base;
        let lines = source
            .split_inclusive('\n')
            .enumerate()
            .map(|(line, text)| {
                let start = line_start;
                line_start += text.len();
                BlockLine {
                    line,
                    range: SourceRange::new(start, start + text.len()),
                    text,
                    disclosure: None,
                }
            })
            .collect::<Vec<_>>();
        let index = BlockIndex::build(Revision(1), source);
        let block = index.block(0).expect("list block");
        let projection = index.list_projection(&block).expect("list projection");
        let joined = parse_joined_block(&lines, Revision(1));
        let code_start = base + source.find("code").expect("code content");
        let mut presented = Vec::new();

        present_joined_run_with_list_projection(
            &lines,
            Revision(1),
            26.0,
            &(0..lines.len()),
            Some(&joined),
            Some(SourceRange::new(code_start, code_start + 4)),
            Some(projection),
            &mut presented,
        );

        let opening = presented.get(1).expect("opening fence");
        assert!(!opening.visual_text.contains("```"));
        assert!(opening.source_map.segments.iter().any(|segment| {
            segment.visibility == Visibility::HiddenMarkup
                && segment.marker_edge == Some(MarkerEdge::Opening)
        }));
        let fence_start = source.find("```").expect("opening fence") + base;
        let fence = opening
            .source_map
            .segments
            .iter()
            .find(|segment| {
                segment.source_range == SourceRange::new(fence_start, fence_start + 3)
            })
            .expect("fence mapping");
        assert_eq!(fence.visibility, Visibility::HiddenMarkup);
        assert_ne!(fence.visibility, Visibility::ExpandedMarkup);
    }

    #[test]
    fn disclosure_on_one_line_of_a_shared_construct_reaches_every_line_that_carries_it() {
        // A caret on the line that owns a shared Strong's opening `**` must also
        // expand the closing `**` on the other physical line: the two markers
        // belong to one active construct, so a partial disclosure would show one
        // marker and hide the other for the same bold run.
        let line0 = "This is **bold\n";
        let line1 = "across lines** ok";
        let start = 50;
        let range0 = SourceRange::new(start, start + line0.len());
        let range1 = SourceRange::new(range0.end.0, range0.end.0 + line1.len());
        let caret = range0.start.0 + line0.find("bold").unwrap();
        let lines = [
            BlockLine {
                line: 0,
                range: range0,
                text: line0,
                disclosure: Some(SourceRange::empty(caret)),
            },
            BlockLine {
                line: 1,
                range: range1,
                text: line1,
                disclosure: None,
            },
        ];
        let block = IndexedBlock {
            ordinal: 0,
            id: BlockId(0),
            kind: NodeKind::Paragraph,
            source_range: SourceRange::new(range0.start.0, range1.end.0),
            revision: Revision(1),
            confidence: Confidence::Formal,
            line_count: 2,
            leading_content_lines: 0,
        };
        let window = BlockWindow {
            span: 0..2,
            trailing_blank_lines: 0,
            lines: &lines,
            clipped_fence_lines: &[],
            zero_height_fence_rows_before: 0,
            zero_height_fence_rows_after: 0,
            render: 0..2,
            joined: None,
            block_disclosure: None,
        };
        let visual = present_block(&block, Revision(1), &window, 26.0);
        let closing_marker = range1.start.0 + line1.find("**").unwrap();
        assert!(
            visual.lines[1].source_map.segments.iter().any(|segment| {
                segment.visibility == Visibility::ExpandedMarkup
                    && segment.source_range.start.0 == closing_marker
            }),
            "the closing marker on the line without its own disclosure must still expand"
        );

        // `expected_disclosures` must agree with what `present_block` itself
        // just assigned to every line of the run, including the one without
        // its own `BlockLine::disclosure` — a caller comparing a cached
        // presentation's disclosures against this instead of each line's own,
        // unmerged disclosure is what lets it recognize the cache as current
        // while the caret sits inside a joined multi-line run (see
        // `EditorView::disclosures_are_current`).
        let expected = expected_disclosures(NodeKind::Paragraph, &window);
        assert_eq!(
            expected,
            visual
                .lines
                .iter()
                .map(|line| (line.line_id as usize, line.disclosure))
                .collect::<Vec<_>>(),
        );
    }

    #[test]
    fn standalone_image_line_inside_a_run_does_not_split_the_shared_parse() {
        // A `**`/`_`/`` ` `` pair distant enough to sit on either side of a
        // standalone image line's own physical line must still resolve as one
        // shared construct: the image line is its own presentation (rendered
        // through `present_image`, not the joined parse), but it must not act
        // as a run boundary that forces the paragraph's two halves into
        // separate, self-contained parses.
        let line0 = "This is **bold\n";
        let image_line = "![alt](dest)\n";
        let line2 = "across lines** ok";
        let start = 50;
        let range0 = SourceRange::new(start, start + line0.len());
        let range1 = SourceRange::new(range0.end.0, range0.end.0 + image_line.len());
        let range2 = SourceRange::new(range1.end.0, range1.end.0 + line2.len());
        let caret = range0.start.0 + line0.find("bold").unwrap();
        let lines = [
            BlockLine {
                line: 0,
                range: range0,
                text: line0,
                disclosure: Some(SourceRange::empty(caret)),
            },
            BlockLine {
                line: 1,
                range: range1,
                text: image_line,
                disclosure: None,
            },
            BlockLine {
                line: 2,
                range: range2,
                text: line2,
                disclosure: None,
            },
        ];
        let block = IndexedBlock {
            ordinal: 0,
            id: BlockId(0),
            kind: NodeKind::Paragraph,
            source_range: SourceRange::new(range0.start.0, range2.end.0),
            revision: Revision(1),
            confidence: Confidence::Formal,
            line_count: 3,
            leading_content_lines: 0,
        };
        let window = BlockWindow {
            span: 0..3,
            trailing_blank_lines: 0,
            lines: &lines,
            clipped_fence_lines: &[],
            zero_height_fence_rows_before: 0,
            zero_height_fence_rows_after: 0,
            render: 0..3,
            joined: None,
            block_disclosure: None,
        };
        let visual = present_block(&block, Revision(1), &window, 26.0);
        assert_eq!(visual.lines.len(), 3);

        // The closing `**` on the far side of the image line must still be
        // recognized as the same Strong construct's marker and expand,
        // exactly as if the image line were not there.
        let closing_marker = range2.start.0 + line2.find("**").unwrap();
        assert!(
            visual.lines[2].source_map.segments.iter().any(|segment| {
                segment.visibility == Visibility::ExpandedMarkup
                    && segment.source_range.start.0 == closing_marker
            }),
            "the closing marker across the image line must still expand"
        );

        // The image line itself must still go through Hane's dedicated image
        // presentation path, not the shared markdown parse.
        assert_eq!(visual.lines[1].kind, BlockKind::Image);
        assert_eq!(
            visual.lines[1].image,
            Some(ImagePresentation {
                alt: "alt".to_owned(),
                destination: "dest".to_owned(),
            })
        );
        assert_eq!(visual.lines[1].disclosure, None);

        // `expected_disclosures` must still agree with what `present_block`
        // actually assigned, including the image line's own hardcoded `None`
        // even though the run's merged disclosure (from the caret on line 0)
        // is `Some`.
        let expected = expected_disclosures(NodeKind::Paragraph, &window);
        assert_eq!(
            expected,
            visual
                .lines
                .iter()
                .map(|line| (line.line_id as usize, line.disclosure))
                .collect::<Vec<_>>(),
        );
    }

    #[test]
    fn one_physical_line_render_window_with_cached_joined_parse_matches_a_wider_window() {
        // `presented_block` (in `hane-ui`) trims `window.lines` down to just
        // `window.render` once a cached `JoinedParse` is available (see
        // `JOIN_SYNC_LINE_BUDGET`), which can leave `disclosure_runs` a run
        // only one physical line long even though the block's own construct —
        // a `**` pair here — spans a second, off-screen line. `present_block`
        // must still resolve it against the cached whole-block parse instead
        // of falling back to a lone-line, self-contained reparse that never
        // sees the closing marker: the rendered line's marker visibility,
        // style runs and source map must come out identical whether the
        // render window is one line or both.
        let line0 = "This is **bold\n";
        let line1 = "across lines** ok";
        let start = 50;
        let range0 = SourceRange::new(start, start + line0.len());
        let range1 = SourceRange::new(range0.end.0, range0.end.0 + line1.len());
        let lines = [
            BlockLine {
                line: 0,
                range: range0,
                text: line0,
                disclosure: None,
            },
            BlockLine {
                line: 1,
                range: range1,
                text: line1,
                disclosure: None,
            },
        ];
        let block = IndexedBlock {
            ordinal: 0,
            id: BlockId(0),
            kind: NodeKind::Paragraph,
            source_range: SourceRange::new(range0.start.0, range1.end.0),
            revision: Revision(1),
            confidence: Confidence::Formal,
            line_count: 2,
            leading_content_lines: 0,
        };
        let joined = parse_joined_block(&lines, Revision(1));

        let wide_window = BlockWindow {
            span: 0..2,
            trailing_blank_lines: 0,
            lines: &lines,
            clipped_fence_lines: &[],
            zero_height_fence_rows_before: 0,
            zero_height_fence_rows_after: 0,
            render: 0..2,
            joined: Some(&joined),
            block_disclosure: None,
        };
        let wide = present_block(&block, Revision(1), &wide_window, 26.0);

        let narrow_lines = &lines[0..1];
        let narrow_window = BlockWindow {
            span: 0..2,
            trailing_blank_lines: 0,
            lines: narrow_lines,
            clipped_fence_lines: &[],
            zero_height_fence_rows_before: 0,
            zero_height_fence_rows_after: 0,
            render: 0..1,
            joined: Some(&joined),
            block_disclosure: None,
        };
        let narrow = present_block(&block, Revision(1), &narrow_window, 26.0);

        assert_eq!(narrow.lines.len(), 1);
        assert_eq!(narrow.lines[0].visual_text, wide.lines[0].visual_text);
        assert_eq!(narrow.lines[0].style_runs, wide.lines[0].style_runs);
        assert_eq!(
            narrow.lines[0].source_map.segments,
            wide.lines[0].source_map.segments
        );
        // Without the shared parse, line 0's lone `**` would be unmatched and
        // stay literal instead of hiding as an opening Strong marker.
        assert!(
            narrow.lines[0]
                .source_map
                .segments
                .iter()
                .any(|segment| segment.visibility == Visibility::HiddenMarkup),
            "the opening marker must resolve against the cached whole-block parse, not a lone-line reparse"
        );

        // `expected_disclosures` must agree with what `present_block` itself
        // just assigned, for the same trimmed window.
        let expected = expected_disclosures(NodeKind::Paragraph, &narrow_window);
        assert_eq!(
            expected,
            narrow
                .lines
                .iter()
                .map(|line| (line.line_id as usize, line.disclosure))
                .collect::<Vec<_>>(),
        );
    }

    #[test]
    fn standalone_image_line_does_not_split_a_run_with_distant_delimiters() {
        // Same guarantee as `standalone_image_line_inside_a_run_does_not_split_the_shared_parse`,
        // but with several plain lines separating the image line from each
        // delimiter instead of the image sitting immediately next to both —
        // the run's shared parse must not narrow to a self-contained window
        // around the image line.
        let lines_text = [
            "This text has **bold that continues\n",
            "through several\n",
            "![alt](dest)\n",
            "more plain lines\n",
            "until it finally ends** here",
        ];
        let mut ranges = Vec::new();
        let mut cursor = 50;
        for text in &lines_text {
            let range = SourceRange::new(cursor, cursor + text.len());
            cursor = range.end.0;
            ranges.push(range);
        }
        let caret = ranges[0].start.0 + lines_text[0].find("bold").unwrap();
        let lines: Vec<BlockLine<'_>> = lines_text
            .iter()
            .zip(ranges.iter())
            .enumerate()
            .map(|(index, (text, range))| BlockLine {
                line: index,
                range: *range,
                text,
                disclosure: (index == 0).then(|| SourceRange::empty(caret)),
            })
            .collect();
        let block = IndexedBlock {
            ordinal: 0,
            id: BlockId(0),
            kind: NodeKind::Paragraph,
            source_range: SourceRange::new(ranges[0].start.0, ranges[4].end.0),
            revision: Revision(1),
            confidence: Confidence::Formal,
            line_count: lines.len(),
            leading_content_lines: 0,
        };
        let window = BlockWindow {
            span: 0..lines.len(),
            trailing_blank_lines: 0,
            lines: &lines,
            clipped_fence_lines: &[],
            zero_height_fence_rows_before: 0,
            zero_height_fence_rows_after: 0,
            render: 0..lines.len(),
            joined: None,
            block_disclosure: None,
        };
        let visual = present_block(&block, Revision(1), &window, 26.0);
        assert_eq!(visual.lines.len(), lines.len());

        // The opening `**` (line 0) and the closing `**` (line 4) are five
        // physical lines apart with an image line between them; both must
        // still resolve as the same Strong construct's markers.
        let opening_marker = ranges[0].start.0 + lines_text[0].find("**").unwrap();
        assert!(
            visual.lines[0].source_map.segments.iter().any(|segment| {
                segment.visibility == Visibility::ExpandedMarkup
                    && segment.source_range.start.0 == opening_marker
            }),
            "the opening marker before the image line must still expand"
        );
        let closing_marker = ranges[4].start.0 + lines_text[4].find("**").unwrap();
        assert!(
            visual.lines[4].source_map.segments.iter().any(|segment| {
                segment.visibility == Visibility::ExpandedMarkup
                    && segment.source_range.start.0 == closing_marker
            }),
            "the closing marker after the image line must still expand"
        );

        // The image line itself still renders through the dedicated image
        // path rather than the shared markdown parse.
        assert_eq!(visual.lines[2].kind, BlockKind::Image);
        assert_eq!(
            visual.lines[2].image,
            Some(ImagePresentation {
                alt: "alt".to_owned(),
                destination: "dest".to_owned(),
            })
        );
    }

    #[test]
    fn hidden_unicode_boundaries_normalize_with_affinity() {
        let source = "**日本🙂**";
        let block = present_markdown(
            0,
            Revision(1),
            SourceRange::new(100, 100 + source.len()),
            source,
            26.0,
        );
        assert_eq!(block.visual_text, "日本🙂");
        assert_eq!(
            block
                .source_map
                .normalize_source(SourceOffset(101), Bias::After),
            Some(SourceOffset(102))
        );
        assert_eq!(
            block
                .source_map
                .normalize_visual(VisualOffset("日本🙂".len()), Bias::Before),
            Some(VisualOffset("日本🙂".len()))
        );
        for segment in &block.source_map.segments {
            assert!(source.is_char_boundary(segment.source_range.start.0 - 100));
            assert!(source.is_char_boundary(segment.source_range.end.0 - 100));
        }
    }

    #[test]
    fn nested_delimiter_runs_do_not_duplicate_visual_or_source_segments() {
        let source = "***nested***";
        let block = present_markdown(
            0,
            Revision(1),
            SourceRange::new(0, source.len()),
            source,
            26.0,
        );
        assert_eq!(block.visual_text, "nested");
        assert_eq!(
            block.source_map.segments[0].source_range,
            SourceRange::new(0, 3)
        );
        assert_eq!(
            block.source_map.segments.last().unwrap().source_range,
            SourceRange::new(9, 12)
        );
    }

    #[test]
    fn phase4_image_keeps_source_and_exposes_lazy_destination() {
        let source = "![羽のロゴ](assets/phase4-feather.svg)\n";
        let range = SourceRange::new(100, 100 + source.len());
        let block = present_polished_line(
            7,
            Revision(2),
            range,
            source,
            26.0,
            None,
            LineContext::Normal,
        );
        assert_eq!(block.kind, BlockKind::Image);
        assert_eq!(block.visual_text, "羽のロゴ");
        assert_eq!(
            block.image,
            Some(ImagePresentation {
                alt: "羽のロゴ".to_owned(),
                destination: "assets/phase4-feather.svg".to_owned(),
            })
        );
        assert!(block.source_map.segments.iter().any(|segment| {
            segment.visibility == Visibility::HiddenMarkup
                && segment.source_range.start == range.start
        }));
        let active = present_polished_line(
            7,
            Revision(2),
            range,
            source,
            26.0,
            Some(SourceRange::empty(105)),
            LineContext::Normal,
        );
        assert_eq!(active.kind, BlockKind::Paragraph);
        assert!(active.image.is_none());
        assert!(active.visual_text.contains("assets/phase4-feather.svg"));
    }

    #[test]
    fn image_estimated_height_scales_with_the_zoomed_line_height() {
        let source = "![alt](dest)";
        let range = SourceRange::new(0, source.len());
        let normal = present_polished_line(
            0,
            Revision(1),
            range,
            source,
            26.0,
            None,
            LineContext::Normal,
        );
        let zoomed = present_polished_line(
            0,
            Revision(1),
            range,
            source,
            52.0,
            None,
            LineContext::Normal,
        );

        assert_eq!(normal.estimated_height, 190.0);
        assert_eq!(zoomed.estimated_height, 380.0);
    }

    #[test]
    fn table_rows_expose_cells_without_fake_separator_offsets() {
        let source = "| 名前 | 値 |\n";
        let range = SourceRange::new(50, 50 + source.len());
        let block = present_polished_line(
            1,
            Revision(3),
            range,
            source,
            26.0,
            None,
            LineContext::Table,
        );
        assert_eq!(block.kind, BlockKind::TableRow);
        assert_eq!(block.visual_text, " 名前  値 \n");
        let table = block.table_row.as_ref().expect("table metadata");
        assert_eq!(table.cells.len(), 2);
        assert_eq!(table.cells[0].source_range, SourceRange::new(51, 59));
        assert_eq!(table.cells[1].source_range, SourceRange::new(60, 65));
        assert!(!block
            .source_map
            .segments
            .iter()
            .any(|segment| segment.visibility == Visibility::Synthesized));
        let empty = present_polished_line(
            1,
            Revision(3),
            SourceRange::new(0, "| x || z |\n".len()),
            "| x || z |\n",
            26.0,
            None,
            LineContext::Table,
        );
        let empty_cells = empty.table_row.as_ref().expect("empty table row").cells.as_slice();
        assert_eq!(
            empty_cells.iter().map(|cell| cell.column).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert!(empty_cells[1].visual_range.start == empty_cells[1].visual_range.end);
        let indented = present_table_line(
            1,
            Revision(3),
            SourceRange::new(0, "  | a | b |\n".len()),
            "  | a | b |\n",
            26.0,
            false,
            &[TableAlignment::Left, TableAlignment::Right],
        );
        let indented_cells = indented
            .table_row
            .as_ref()
            .expect("indented table row")
            .cells
            .as_slice();
        assert_eq!(indented_cells.len(), 2);
        assert_eq!(indented_cells[0].source_range.start, SourceOffset(3));
        let one_column_delimiter = present_polished_line(
            1,
            Revision(3),
            SourceRange::new(0, "| --- |\n".len()),
            "| --- |\n",
            26.0,
            None,
            LineContext::Table,
        );
        assert_eq!(one_column_delimiter.kind, BlockKind::TableDelimiter);
        let sparse = present_table_line(
            1,
            Revision(3),
            SourceRange::new(0, "| only |\n".len()),
            "| only |\n",
            26.0,
            false,
            &[TableAlignment::Default, TableAlignment::Right],
        );
        assert_eq!(
            sparse
                .table_row
                .as_ref()
                .expect("sparse table row")
                .column_count,
            2
        );
        let sparse_cells = sparse
            .table_row
            .as_ref()
            .expect("sparse table row cells")
            .cells
            .as_slice();
        assert_eq!(
            sparse_cells.iter().map(|cell| cell.column).collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert!(sparse_cells[1].visual_range.start == sparse_cells[1].visual_range.end);
        assert_eq!(
            sparse_cells[1].visual_range.start.0,
            sparse.visual_text.trim_end_matches(['\r', '\n']).len()
        );
        let excess = present_table_line(
            1,
            Revision(3),
            SourceRange::new(0, "| a | 2 |\n".len()),
            "| a | 2 |\n",
            26.0,
            false,
            &[TableAlignment::Default],
        );
        let excess_table = excess.table_row.as_ref().expect("excess table row");
        assert_eq!(excess_table.column_count, 1);
        assert_eq!(excess_table.cells.len(), 1);
        assert_eq!(
            &excess.visual_text[excess_table.cells[0].visual_range.start.0
                ..excess_table.cells[0].visual_range.end.0],
            " a "
        );
        let active = present_polished_line(
            1,
            Revision(3),
            range,
            source,
            26.0,
            Some(SourceRange::empty(54)),
            LineContext::Table,
        );
        assert_eq!(active.visual_text, source);
        assert_eq!(active.kind, BlockKind::Paragraph);
    }

    #[test]
    fn raw_source_fallback_preserves_every_byte_as_plain_text() {
        let source = "<div class=\"note\">未対応 &amp; raw</div>";
        let range = SourceRange::new(12, 12 + source.len());
        let block = present_raw_source(9, Revision(3), range, source, 26.0);
        assert_eq!(block.kind, BlockKind::Unsupported);
        assert_eq!(block.visual_text, source);
        assert!(block.style_runs.is_empty());
        assert_eq!(block.source_map.segments.len(), 1);
        assert_eq!(block.source_map.segments[0].visibility, Visibility::Visible);
        for relative in 0..=source.len() {
            if !source.is_char_boundary(relative) {
                continue;
            }
            let offset = SourceOffset(12 + relative);
            let visual = block
                .source_map
                .source_to_visual(offset, Bias::After)
                .unwrap()
                .visual_offset;
            assert_eq!(
                block
                    .source_map
                    .visual_to_source(visual, Bias::After)
                    .unwrap()
                    .source_offset,
                offset
            );
        }
    }

    #[test]
    fn fenced_code_context_styles_the_line_as_code_without_hiding_markup() {
        let source = "let answer = **42**;\n";
        let range = SourceRange::new(30, 30 + source.len());
        let block = present_polished_line(
            2,
            Revision(1),
            range,
            source,
            26.0,
            None,
            LineContext::FencedCode,
        );
        assert_eq!(block.kind, BlockKind::CodeBlock);
        // Source is literal inside a fence: emphasis markers stay visible.
        assert_eq!(block.visual_text, source);
        assert_eq!(block.style_runs.len(), 1);
        let run = block.style_runs[0];
        assert_eq!(run.kind, StyleKind::CodeBlock);
        // The style run covers the content but stops before the trailing newline.
        assert_eq!(
            run.visual_range,
            VisualRange::new(0, source.trim_end_matches('\n').len())
        );
        assert_eq!(block.height(), 26.0);
        assert!(
            block
                .source_map
                .segments
                .iter()
                .all(|segment| segment.visibility == Visibility::Visible)
        );
    }

    fn fence_block_lines(source: &'static str, base: usize) -> (IndexedBlock, Vec<BlockLine<'static>>) {
        let mut offset = base;
        let lines = source
            .split_inclusive('\n')
            .enumerate()
            .map(|(line, text)| {
                let range = SourceRange::new(offset, offset + text.len());
                offset = range.end.0;
                BlockLine {
                    line,
                    range,
                    text,
                    disclosure: None,
                }
            })
            .collect::<Vec<_>>();
        let block = IndexedBlock {
            ordinal: 0,
            id: BlockId(0),
            kind: NodeKind::CodeBlock,
            source_range: SourceRange::new(base, base + source.len()),
            revision: Revision(1),
            confidence: Confidence::Formal,
            line_count: lines.len(),
            leading_content_lines: 0,
        };
        (block, lines)
    }

    #[test]
    fn present_block_hides_fence_delimiters_and_info_string_when_inactive() {
        let (block, lines) = fence_block_lines("```rust\nlet answer = 42;\n```", 40);
        let window = BlockWindow {
            trailing_blank_lines: 0,
            span: 0..lines.len(),
            render: 0..lines.len(),
            lines: &lines,
            clipped_fence_lines: &[],
            zero_height_fence_rows_before: 0,
            zero_height_fence_rows_after: 0,
            joined: None,
            block_disclosure: None,
        };
        let visual = present_block(&block, Revision(1), &window, 26.0);
        assert_eq!(visual.lines.len(), 3);

        let opening = &visual.lines[0];
        assert_eq!(opening.kind, BlockKind::CodeBlock);
        assert_eq!(opening.visual_text, "");
        let hidden = opening
            .source_map
            .segments
            .iter()
            .find(|segment| segment.visibility == Visibility::HiddenMarkup)
            .expect("opening fence hides its own delimiter run");
        assert_eq!(hidden.source_range, SourceRange::new(40, 43));
        assert_eq!(hidden.marker_edge, Some(MarkerEdge::Opening));
        let hidden_info = opening
            .source_map
            .segments
            .iter()
            .find(|segment| segment.source_range.start == SourceOffset(43))
            .expect("the info string and trailing newline have their own mapping segment");
        assert_eq!(hidden_info.visibility, Visibility::HiddenMarkup);
        assert_eq!(hidden_info.marker_edge, None);
        assert_eq!(hidden_info.source_range, SourceRange::new(43, 48));
        assert_eq!(opening.height(), 0.0);

        let mut disclosed_lines = lines.clone();
        disclosed_lines[0].disclosure = Some(SourceRange::empty(44));
        let disclosed = present_block(
            &block,
            Revision(1),
            &BlockWindow {
                lines: &disclosed_lines,
                block_disclosure: Some(SourceRange::empty(44)),
                ..window.clone()
            },
            26.0,
        );
        assert_eq!(disclosed.lines[0].visual_text, "```rust");
        assert_eq!(disclosed.lines[0].height(), 26.0);
        assert!(disclosed.lines[0].source_map.segments.iter().all(|segment| {
            segment.visibility == Visibility::ExpandedMarkup
        }));

        let content = &visual.lines[1];
        assert_eq!(content.visual_text, "let answer = 42;");
        assert!(
            content
                .source_map
                .segments
                .iter()
                .all(|segment| segment.visibility == Visibility::Visible),
            "code content is never treated as markup"
        );

        let closing = &visual.lines[2];
        assert_eq!(closing.kind, BlockKind::CodeBlock);
        assert_eq!(closing.visual_text, "");
        assert_eq!(closing.source_map.segments.len(), 1);
        let closing_segment = closing.source_map.segments[0];
        assert_eq!(closing_segment.visibility, Visibility::HiddenMarkup);
        assert_eq!(closing_segment.marker_edge, Some(MarkerEdge::Closing));
        assert_eq!(closing_segment.source_range, lines[2].range);
    }

    #[test]
    fn present_block_keeps_an_unterminated_fences_closing_lookalike_line_literal() {
        // A 3-backtick line cannot close a 4-backtick fence (CommonMark:
        // matching character, run at least as long). The fence never closes,
        // so its closing-shaped last line must stay literal code content.
        let (block, lines) = fence_block_lines("````rust\ncode\n```", 0);
        let window = BlockWindow {
            trailing_blank_lines: 0,
            span: 0..lines.len(),
            render: 0..lines.len(),
            lines: &lines,
            clipped_fence_lines: &[],
            zero_height_fence_rows_before: 0,
            zero_height_fence_rows_after: 0,
            joined: None,
            block_disclosure: None,
        };
        let visual = present_block(&block, Revision(1), &window, 26.0);
        assert_eq!(visual.lines[0].visual_text, "", "the real opening collapses");
        assert_eq!(visual.lines[1].visual_text, "code");
        let last = &visual.lines[2];
        assert_eq!(last.visual_text, "```");
        assert!(
            last.source_map
                .segments
                .iter()
                .all(|segment| segment.visibility == Visibility::Visible),
            "an unterminated fence's closing-lookalike last line is not markup"
        );
    }

    #[test]
    fn nested_code_block_kind_wins_over_its_enclosing_quote() {
        // A heading nested in a quote keeps its own heading level rather than
        // showing as quoted plain text (see `shared_heading_kinds_follow_...`);
        // a nested code block must carry the same priority, or a quoted fence
        // would lose its code background, monospace and fence hiding.
        let source = "> ```rust\n> code\n> ```\n";
        let mut offset = 0;
        let lines = source
            .split_inclusive('\n')
            .enumerate()
            .map(|(line, text)| {
                let range = SourceRange::new(offset, offset + text.len());
                offset = range.end.0;
                BlockLine {
                    line,
                    range,
                    text,
                    disclosure: None,
                }
            })
            .collect::<Vec<_>>();
        let joined = parse_joined_block(&lines, Revision(1));
        let mut presented = Vec::new();
        present_joined_run(
            &lines,
            Revision(1),
            26.0,
            &(0..lines.len()),
            Some(&joined),
            None,
            &mut presented,
        );
        for line in &presented {
            assert_eq!(line.kind, BlockKind::CodeBlock, "{:?}", line.visual_text);
        }
        assert_eq!(presented[0].visual_text, "");
        assert_eq!(presented[1].visual_text, "code");
        assert_eq!(
            presented[2].visual_text, "",
            "the closing fence collapses inside a quote the same way it does at the top level"
        );
    }
}
