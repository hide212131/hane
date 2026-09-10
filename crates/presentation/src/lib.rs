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

pub use layout::{
    BlockLayout, LayoutLine, LayoutPoint, LineShaper, LineWrap, VerticalMove, layout_block,
    line_visual_start,
};

use hane_document::{
    Bias, Revision, RevisionDelta, RopeBuffer, SourceOffset, SourceRange, TextBuffer,
};
use hane_markdown::{
    BlockId, BlockIndex, Confidence, IndexedBlock, MarkdownParse, NodeKind, has_delimiter_markers,
    is_table_delimiter, parse_document,
};
use std::ops::Range;

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MappingSegment {
    pub source_range: SourceRange,
    pub visual_range: VisualRange,
    pub visibility: Visibility,
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

    pub fn normalize_source(&self, source: SourceOffset, affinity: Bias) -> Option<SourceOffset> {
        let visual = self.source_to_visual(source, affinity)?.visual_offset;
        self.visual_to_source(visual, affinity)
            .map(|candidate| candidate.source_offset)
    }

    pub fn normalize_visual(&self, visual: VisualOffset, affinity: Bias) -> Option<VisualOffset> {
        let source = self.visual_to_source(visual, affinity)?.source_offset;
        self.source_to_visual(source, affinity)
            .map(|candidate| candidate.visual_offset)
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
            Self::InlineCode | Self::CodeBlock => InlineDisplay {
                monospace: true,
                code_background: true,
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
                monospace: true,
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
}

impl VisualLine {
    pub fn height(&self) -> f32 {
        self.measured_height.unwrap_or(self.estimated_height)
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
    /// Lines of `span` clipped above and below the presented run.
    pub lines_before: usize,
    pub lines_after: usize,
    /// Height a clipped line stands in for.
    pub line_height: f32,
}

impl VisualBlock {
    /// Height of the whole block: what was presented, plus line height for what
    /// was clipped.
    pub fn height(&self) -> f32 {
        let clipped = (self.lines_before + self.lines_after) as f32 * self.line_height;
        clipped + self.lines.iter().map(VisualLine::height).sum::<f32>()
    }

    /// Space to leave above the presented lines, inside the block.
    pub fn leading_space(&self) -> f32 {
        self.lines_before as f32 * self.line_height
    }

    /// Space to leave below the presented lines, inside the block.
    pub fn trailing_space(&self) -> f32 {
        self.lines_after as f32 * self.line_height
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
    let mut counted = 0;
    let mut heights = index
        .blocks()
        .map(|block| {
            counted += block.line_count;
            line_height * block.line_count as f32
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
    let context = block_line_context(block.kind);
    let content_end = window.span.end.saturating_sub(window.trailing_blank_lines);
    let mut lines = Vec::with_capacity(window.render.len().min(window.lines.len()));
    for run in disclosure_runs(block.kind, window) {
        if run.len() > 1 {
            present_joined_run(
                &window.lines[run.clone()],
                revision,
                line_height,
                &window.render,
                window.joined,
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
        let mut presented = present_polished_line(
            line.line as u64,
            revision,
            line.range,
            line.text,
            line_height,
            line.disclosure,
            line_context,
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
    VisualBlock {
        id: block.id,
        kind: block_display_kind(block.kind),
        source_range: block.source_range,
        revision,
        confidence: block.confidence,
        span: window.span.clone(),
        lines,
        lines_before,
        lines_after,
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
            }],
        },
        estimated_height: 24.0,
        measured_height: None,
        invalid: false,
        context: LineContext::Normal,
        disclosure: None,
        image: None,
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
    match kind {
        BlockKind::Heading(1) => line_height * 1.65,
        BlockKind::Heading(2) => line_height * 1.45,
        BlockKind::Heading(3) => line_height * 1.25,
        BlockKind::Heading(_) => line_height * 1.1,
        BlockKind::CodeBlock => line_height * 1.15,
        BlockKind::Image => 190.0,
        BlockKind::TableDelimiter => 8.0,
        _ => line_height,
    }
}

fn range_touches(range: SourceRange, disclosure: SourceRange) -> bool {
    if disclosure.is_empty() {
        range.start <= disclosure.start && disclosure.start <= range.end
    } else {
        range.intersects(disclosure)
    }
}

fn marker_is_disclosed(
    marker: SourceRange,
    parsed: &MarkdownParse,
    disclosure: Option<SourceRange>,
) -> bool {
    let Some(disclosure) = disclosure else {
        return false;
    };
    range_touches(marker, disclosure)
        || parsed
            .tree
            .iter()
            .filter(|(_, node)| has_delimiter_markers(node.kind))
            .any(|(_, span)| {
                span.source_range.start <= marker.start
                    && marker.end <= span.source_range.end
                    && range_touches(span.source_range, disclosure)
            })
        || parsed
            .tree
            .blocks()
            .filter(|(_, node)| syntax_display(node.kind).node_block.is_some())
            .any(|(_, block)| {
                marker.start == block.source_range.start
                    && marker.end <= block.source_range.end
                    && range_touches(block.source_range, disclosure)
            })
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
    });
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
    if source.is_empty() {
        let mut block = present_plain(line_id, revision, range, source);
        block.estimated_height = line_height;
        return block;
    }
    let parsed = parse_document(revision, range, source);
    present_markdown_from_parse(
        line_id,
        revision,
        range,
        source,
        line_height,
        disclosure,
        &SharedParse {
            whole_source: source,
            parsed: &parsed,
        },
    )
}

/// A parse [`present_markdown_from_parse`] presents one physical line of, plus
/// the text it was parsed from. For a self-contained parse `whole_source` is
/// that line's own `source`; for a joined multi-line run (see
/// [`present_joined_run`]) it is every line's text concatenated, since
/// `parsed` may then describe delimiters and spans on a different physical
/// line than the one being presented.
struct SharedParse<'a> {
    whole_source: &'a str,
    parsed: &'a MarkdownParse,
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
    let kind = parsed
        .tree
        .blocks()
        .find_map(|(_, block)| syntax_display(block.kind).node_block)
        .unwrap_or_default();
    let mut visual = String::with_capacity(source.len());
    let mut segments = Vec::with_capacity(parsed.markers.len() * 2 + 1);
    let mut source_cursor = range.start.0;
    // A code span whose content both opens and closes on whitespace hides one
    // leading and one trailing byte of it, the same display-only normalization
    // CommonMark §6.1 applies to a code span's rendered content. Folded into the
    // marker list so it hides, discloses and clips exactly like a real marker.
    let mut markers = parsed.markers.clone();
    markers.extend(code_span_padding(parsed, shared.whole_source));
    markers.sort_by_key(|marker| (marker.start, marker.end));
    // Only markers wholly inside this physical line's range are this line's to
    // show or hide; a shared multi-line parse also carries every other line's
    // markers, which belong to the [`VisualLine`] built for that line instead.
    let markers_on_line = markers
        .iter()
        .copied()
        .filter(|marker| marker.start >= range.start && marker.end <= range.end);
    for marker in markers_on_line {
        if source_cursor < marker.start.0 {
            append_segment(
                &mut visual,
                &mut segments,
                source,
                range,
                SourceRange::new(source_cursor, marker.start.0),
                Visibility::Visible,
            );
        }
        let expanded = marker_is_disclosed(marker, parsed, disclosure);
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
        );
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
        );
    }
    if segments.is_empty() {
        segments.push(MappingSegment {
            source_range: range,
            visual_range: VisualRange::new(0, visual.len()),
            visibility: Visibility::Visible,
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
    let mut style_runs = parsed
        .tree
        .iter()
        .filter(|(_, node)| has_delimiter_markers(node.kind))
        .filter_map(|(_, span)| {
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
    style_runs.sort_by_key(|run| (run.visual_range.start.0, run.visual_range.end.0));
    VisualLine {
        line_id,
        source_range: range,
        revision,
        visual_text: visual,
        style_runs,
        source_map,
        estimated_height: estimated_height(kind, line_height),
        measured_height: None,
        invalid: false,
        kind,
        context: LineContext::Normal,
        disclosure,
        image: None,
    }
}

/// Byte ranges of a code span's leading and trailing padding, for every code
/// span in `parsed` whose content both opens and closes on whitespace (a line
/// ending counts, since CommonMark §6.1 first converts one to a space) without
/// being made of whitespace alone. Each returned range is exactly the one byte
/// that display trims; [`present_markdown_from_parse`] folds them into its
/// marker list so they hide, disclose and clip exactly like a real delimiter,
/// leaving the source itself untouched.
fn code_span_padding(parsed: &MarkdownParse, whole_source: &str) -> Vec<SourceRange> {
    let base = parsed.source_range.start.0;
    let is_pad = |byte: u8| matches!(byte, b' ' | b'\n' | b'\r');
    parsed
        .tree
        .iter()
        .filter(|(_, node)| node.kind == NodeKind::InlineCode)
        .filter_map(|(_, node)| {
            let start = node.source_range.start.0;
            let end = node.source_range.end.0;
            let text = whole_source.get(start - base..end - base)?;
            let marker_len = text.bytes().take_while(|byte| *byte == b'`').count();
            if marker_len == 0 || marker_len * 2 > text.len() {
                return None;
            }
            let content_start = start + marker_len;
            let content_end = end - marker_len;
            let content = whole_source.get(content_start - base..content_end - base)?;
            let bytes = content.as_bytes();
            let padded = bytes.first().copied().is_some_and(is_pad)
                && bytes.last().copied().is_some_and(is_pad)
                && !bytes.iter().copied().all(is_pad);
            padded.then(|| {
                [
                    SourceRange::new(content_start, content_start + 1),
                    SourceRange::new(content_end - 1, content_end),
                ]
            })
        })
        .flatten()
        .collect()
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
/// inside a joined multi-line run — even one whose own [`BlockLine::disclosure`]
/// is `None`, because the caret, selection or IME sits on a different physical
/// line of the same shared construct — or the line's own disclosure otherwise.
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
        let joined = lines.len() > 1;
        let merged = joined.then(|| merged_disclosure(lines)).flatten();
        for line in lines {
            if window.render.contains(&line.line) {
                let disclosure = if joined
                    && !line.text.is_empty()
                    && inactive_standalone_image(line.text, line.range, line.disclosure).is_none()
                {
                    merged
                } else if joined {
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
        .reduce(|a, b| SourceRange {
            start: a.start.min(b.start),
            end: a.end.max(b.end),
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
}

/// Parses `lines` joined into one source, from texts the caller already has
/// in hand. Used both as [`present_joined_run`]'s fallback when no cached
/// [`JoinedParse`] is available, and to build one from a bounded run of
/// [`BlockLine`]s.
pub fn parse_joined_block(lines: &[BlockLine<'_>], revision: Revision) -> JoinedParse {
    let joined_range = SourceRange::new(lines[0].range.start.0, lines[lines.len() - 1].range.end.0);
    let joined_source = lines.iter().map(|line| line.text).collect::<String>();
    let parsed = parse_document(revision, joined_range, &joined_source);
    JoinedParse {
        source_range: joined_range,
        joined_source,
        parsed,
    }
}

/// Reads and parses one joinable block's whole content span directly from
/// `document`, independent of any particular viewport. Meant to run off the
/// render path (see [`BlockWindow::joined`]): a single read and parse whose
/// cost scales with the block's size, which for a joinable block has no
/// upper bound. `content` excludes the block's trailing blank run, matching
/// what [`present_block`] itself treats as the construct's own lines.
pub fn parse_joined_span(
    document: &RopeBuffer,
    content: Range<usize>,
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
    let joined_range = SourceRange::new(start.0, end.0);
    let joined_source = document.text(joined_range).ok()?;
    let parsed = parse_document(revision, joined_range, &joined_source);
    Some(JoinedParse {
        source_range: joined_range,
        joined_source,
        parsed,
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
fn present_joined_run(
    lines: &[BlockLine<'_>],
    revision: Revision,
    line_height: f32,
    render: &Range<usize>,
    joined: Option<&JoinedParse>,
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
        whole_source: &joined.joined_source,
        parsed: &joined.parsed,
    };
    // A single active disclosure (caret, selection or IME) may touch a shared
    // construct whose markers live on different physical lines; `marker_is_disclosed`
    // only expands a marker whose enclosing span reaches the disclosure it is
    // given, so every line of the run is offered the same, run-wide disclosure
    // rather than only the one line that literally owns the caret.
    let disclosure = merged_disclosure(lines);
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
    // A fenced-code line has no disclosable inline markup; its content is literal,
    // so it stays a code block regardless of cursor position and wins over image
    // and table recognition that would otherwise mis-read the literal text.
    let mut block = if context == LineContext::FencedCode {
        present_fenced_code_line(line_id, revision, range, source, line_height)
    } else if let Some(image) = inactive_standalone_image(source, range, disclosure) {
        present_image(line_id, revision, range, source, line_height, image)
    } else if context == LineContext::Table
        && disclosure.is_none_or(|active| !range_touches(range, active))
    {
        present_table_line(line_id, revision, range, source, line_height)
    } else {
        present_markdown_with_disclosure(line_id, revision, range, source, line_height, disclosure)
    };
    block.context = context;
    block
}

/// Presents one line that the context index reports is inside a fenced code
/// block. The source is shown verbatim (no marker hiding) and styled as code.
fn present_fenced_code_line(
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
) -> VisualLine {
    let mut segments = Vec::new();
    let base = range.start.0;
    if image.prefix_end > 2 {
        segments.push(MappingSegment {
            source_range: SourceRange::new(base, base + image.prefix_end - 2),
            visual_range: VisualRange::new(0, image.prefix_end - 2),
            visibility: Visibility::Visible,
        });
    }
    let visual_prefix = image.prefix_end.saturating_sub(2);
    segments.push(MappingSegment {
        source_range: SourceRange::new(base + visual_prefix, base + image.prefix_end),
        visual_range: VisualRange::new(visual_prefix, visual_prefix),
        visibility: Visibility::HiddenMarkup,
    });
    segments.push(MappingSegment {
        source_range: SourceRange::new(base + image.alt_start, base + image.alt_end),
        visual_range: VisualRange::new(visual_prefix, visual_prefix + image.alt.len()),
        visibility: Visibility::Visible,
    });
    let visual_end = visual_prefix + image.alt.len();
    segments.push(MappingSegment {
        source_range: SourceRange::new(base + image.suffix_start, base + image.suffix_end),
        visual_range: VisualRange::new(visual_end, visual_end),
        visibility: Visibility::HiddenMarkup,
    });
    if image.suffix_end < source.len() {
        segments.push(MappingSegment {
            source_range: SourceRange::new(base + image.suffix_end, range.end.0),
            visual_range: VisualRange::new(
                visual_end,
                visual_end + source.len() - image.suffix_end,
            ),
            visibility: Visibility::Visible,
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
    }
}

fn present_table_line(
    line_id: u64,
    revision: Revision,
    range: SourceRange,
    source: &str,
    line_height: f32,
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
                }],
            },
            estimated_height: estimated_height(BlockKind::TableDelimiter, line_height),
            measured_height: None,
            invalid: false,
            context: LineContext::Normal,
            disclosure: None,
            image: None,
        };
    }
    let content_end = source.trim_end_matches(['\r', '\n']).len();
    let mut visual = String::new();
    let mut segments = Vec::new();
    let base = range.start.0;
    let mut cursor = 0;
    for (index, marker) in source[..content_end].match_indices('|') {
        if cursor < index {
            append_segment(
                &mut visual,
                &mut segments,
                source,
                range,
                SourceRange::new(base + cursor, base + index),
                Visibility::Visible,
            );
        }
        let at = visual.len();
        segments.push(MappingSegment {
            source_range: SourceRange::new(base + index, base + index + marker.len()),
            visual_range: VisualRange::new(at, at),
            visibility: Visibility::HiddenMarkup,
        });
        if index > 0 && index + 1 < content_end {
            visual.push('│');
            segments.push(MappingSegment {
                source_range: SourceRange::empty(base + index + 1),
                visual_range: VisualRange::new(at, visual.len()),
                visibility: Visibility::Synthesized,
            });
        }
        cursor = index + marker.len();
    }
    if cursor < source.len() {
        append_segment(
            &mut visual,
            &mut segments,
            source,
            range,
            SourceRange::new(base + cursor, range.end.0),
            Visibility::Visible,
        );
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
        };
        let window = BlockWindow {
            span: 0..2,
            trailing_blank_lines: 0,
            lines: &lines,
            render: 0..2,
            joined: None,
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
        };
        let window = BlockWindow {
            span: 0..3,
            trailing_blank_lines: 0,
            lines: &lines,
            render: 0..3,
            joined: None,
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
    fn phase4_table_uses_synthesized_separators_with_canonical_mapping() {
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
        assert_eq!(block.visual_text, " 名前 │ 値 \n");
        let synthesized = block
            .source_map
            .segments
            .iter()
            .find(|segment| segment.visibility == Visibility::Synthesized)
            .unwrap();
        assert!(synthesized.source_range.is_empty());
        for affinity in [Bias::Before, Bias::After] {
            let visual = synthesized.visual_range.start;
            let normalized = block.source_map.normalize_visual(visual, affinity).unwrap();
            assert_eq!(
                block.source_map.normalize_visual(normalized, affinity),
                Some(normalized)
            );
        }
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
        assert!(block.height() > 26.0);
        assert!(
            block
                .source_map
                .segments
                .iter()
                .all(|segment| segment.visibility == Visibility::Visible)
        );
    }
}
