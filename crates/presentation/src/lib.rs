//! Local visual blocks, source mapping, and variable-height virtualization.

use hane_document::{
    Bias, Revision, RevisionDelta, RopeBuffer, SourceOffset, SourceRange, TextBuffer,
};
use hane_markdown::{
    BlockKind as MarkdownBlockKind, InlineKind as MarkdownInlineKind, MarkdownParse,
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

/// Block-level context that a single source line sits in, resolved by the caller
/// from the document-wide [`hane_markdown::BlockContextIndex`] (or its bounded
/// fallback). Presentation owns the resulting display kind and style runs so the
/// UI never re-derives fenced-code or table styling from raw source.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LineContext {
    #[default]
    Normal,
    FencedCode,
    Table,
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
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImagePresentation {
    pub alt: String,
    pub destination: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VisualBlock {
    pub block_id: u64,
    pub source_range: SourceRange,
    pub revision: Revision,
    pub visual_text: String,
    pub style_runs: Vec<StyleRun>,
    pub kind: BlockKind,
    pub source_map: SourceMap,
    pub estimated_height: f32,
    pub measured_height: Option<f32>,
    pub invalid: bool,
    /// Source range whose Markdown markers are currently disclosed.
    pub disclosure: Option<SourceRange>,
    /// Present only for an inactive standalone Markdown image. The UI resolves
    /// relative destinations against the document directory and loads only
    /// visible image blocks.
    pub image: Option<ImagePresentation>,
}

impl VisualBlock {
    pub fn height(&self) -> f32 {
        self.measured_height.unwrap_or(self.estimated_height)
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

pub fn present_plain(
    block_id: u64,
    revision: Revision,
    range: SourceRange,
    source: &str,
) -> VisualBlock {
    VisualBlock {
        block_id,
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
        disclosure: None,
        image: None,
    }
}

fn presentation_block_kind(kind: MarkdownBlockKind) -> BlockKind {
    match kind {
        MarkdownBlockKind::Paragraph => BlockKind::Paragraph,
        MarkdownBlockKind::Heading(level) => BlockKind::Heading(level),
        MarkdownBlockKind::CodeBlock => BlockKind::CodeBlock,
        MarkdownBlockKind::Quote => BlockKind::Quote,
        MarkdownBlockKind::ListItem => BlockKind::ListItem,
        MarkdownBlockKind::Rule => BlockKind::Rule,
    }
}

fn presentation_style_kind(kind: MarkdownInlineKind) -> StyleKind {
    match kind {
        MarkdownInlineKind::Bold => StyleKind::Bold,
        MarkdownInlineKind::Italic => StyleKind::Italic,
        MarkdownInlineKind::Strikethrough => StyleKind::Strikethrough,
        MarkdownInlineKind::InlineCode => StyleKind::InlineCode,
        MarkdownInlineKind::Link => StyleKind::Link,
        MarkdownInlineKind::CodeBlock => StyleKind::CodeBlock,
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
        || parsed.spans.iter().any(|span| {
            span.source_range.start <= marker.start
                && marker.end <= span.source_range.end
                && range_touches(span.source_range, disclosure)
        })
        || parsed.blocks.iter().any(|block| {
            marker.start == block.source_range.start
                && marker.end <= block.source_range.end
                && range_touches(block.source_range, disclosure)
        })
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
    block_id: u64,
    revision: Revision,
    range: SourceRange,
    source: &str,
    line_height: f32,
    disclosure: Option<SourceRange>,
) -> VisualBlock {
    if source.is_empty() {
        let mut block = present_plain(block_id, revision, range, source);
        block.estimated_height = line_height;
        return block;
    }
    let parsed = parse_document(revision, range, source);
    let kind = parsed
        .blocks
        .first()
        .map(|block| presentation_block_kind(block.kind))
        .unwrap_or_default();
    let mut visual = String::with_capacity(source.len());
    let mut segments = Vec::with_capacity(parsed.markers.len() * 2 + 1);
    let mut source_cursor = range.start.0;
    for &marker in &parsed.markers {
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
        let expanded = marker_is_disclosed(marker, &parsed, disclosure);
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
    let source_map = SourceMap { segments };
    let mut style_runs = parsed
        .spans
        .iter()
        .filter_map(|span| {
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
                kind: presentation_style_kind(span.kind),
            })
        })
        .collect::<Vec<_>>();
    style_runs.sort_by_key(|run| (run.visual_range.start.0, run.visual_range.end.0));
    VisualBlock {
        block_id,
        source_range: range,
        revision,
        visual_text: visual,
        style_runs,
        source_map,
        estimated_height: estimated_height(kind, line_height),
        measured_height: None,
        invalid: false,
        kind,
        disclosure,
        image: None,
    }
}

/// Presents one physical source line with Phase 4 block polish. The block-level
/// [`LineContext`] is supplied by the caller from the document context index;
/// standalone image recognition is local to this source slice. Presentation owns
/// every display-kind and style-run decision here so the UI renders purely by
/// [`BlockKind`]/[`StyleKind`] without re-inspecting the source.
pub fn present_polished_line(
    block_id: u64,
    revision: Revision,
    range: SourceRange,
    source: &str,
    line_height: f32,
    disclosure: Option<SourceRange>,
    context: LineContext,
) -> VisualBlock {
    // A fenced-code line has no disclosable inline markup; its content is literal,
    // so it stays a code block regardless of cursor position and wins over image
    // and table recognition that would otherwise mis-read the literal text.
    if context == LineContext::FencedCode {
        return present_fenced_code_line(block_id, revision, range, source, line_height);
    }
    if let Some(image) = parse_standalone_image(source)
        && disclosure.is_none_or(|active| !range_touches(range, active))
    {
        return present_image(block_id, revision, range, source, line_height, image);
    }
    if context == LineContext::Table
        && disclosure.is_none_or(|active| !range_touches(range, active))
    {
        return present_table_line(block_id, revision, range, source, line_height);
    }
    present_markdown_with_disclosure(block_id, revision, range, source, line_height, disclosure)
}

/// Presents one line that the context index reports is inside a fenced code
/// block. The source is shown verbatim (no marker hiding) and styled as code.
fn present_fenced_code_line(
    block_id: u64,
    revision: Revision,
    range: SourceRange,
    source: &str,
    line_height: f32,
) -> VisualBlock {
    let mut block = present_plain(block_id, revision, range, source);
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
    block_id: u64,
    revision: Revision,
    range: SourceRange,
    source: &str,
    line_height: f32,
    image: StandaloneImage<'_>,
) -> VisualBlock {
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
    VisualBlock {
        block_id,
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
        disclosure: None,
        image: Some(ImagePresentation {
            alt: image.alt.to_owned(),
            destination: image.destination.to_owned(),
        }),
    }
}

fn present_table_line(
    block_id: u64,
    revision: Revision,
    range: SourceRange,
    source: &str,
    line_height: f32,
) -> VisualBlock {
    if is_table_delimiter(source) {
        return VisualBlock {
            block_id,
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
    VisualBlock {
        block_id,
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
        disclosure: None,
        image: None,
    }
}

pub fn present_markdown(
    block_id: u64,
    revision: Revision,
    range: SourceRange,
    source: &str,
    line_height: f32,
) -> VisualBlock {
    present_markdown_with_disclosure(block_id, revision, range, source, line_height, None)
}

pub fn paragraph_blocks(buffer: &RopeBuffer, line_height: f32) -> Vec<VisualBlock> {
    let mut blocks = Vec::with_capacity(buffer.line_count());
    for line in 0..buffer.line_count() {
        let Ok(range) = buffer.line_range(hane_document::LineId(line)) else {
            continue;
        };
        let text = buffer.text(range).unwrap_or_default();
        let block = present_markdown(line as u64, buffer.revision(), range, &text, line_height);
        blocks.push(block);
    }
    blocks
}

/// Fenwick tree over non-negative block heights.
#[derive(Clone, Debug)]
pub struct HeightIndex {
    heights: Vec<f32>,
    tree: Vec<f32>,
}

impl HeightIndex {
    pub fn new(heights: impl IntoIterator<Item = f32>) -> Self {
        let heights: Vec<_> = heights.into_iter().map(|h| h.max(0.0)).collect();
        let mut this = Self {
            tree: vec![0.0; heights.len() + 1],
            heights,
        };
        for ix in 0..this.heights.len() {
            this.add(ix, this.heights[ix]);
        }
        this
    }
    pub fn len(&self) -> usize {
        self.heights.len()
    }
    pub fn is_empty(&self) -> bool {
        self.heights.is_empty()
    }
    fn add(&mut self, index: usize, delta: f32) {
        let mut i = index + 1;
        while i < self.tree.len() {
            self.tree[i] += delta;
            i += i & i.wrapping_neg();
        }
    }
    pub fn update(&mut self, index: usize, height: f32) {
        let next = height.max(0.0);
        let delta = next - self.heights[index];
        self.heights[index] = next;
        self.add(index, delta);
    }
    pub fn prefix_sum(&self, exclusive_end: usize) -> f32 {
        let mut i = exclusive_end.min(self.len());
        let mut sum = 0.0;
        while i > 0 {
            sum += self.tree[i];
            i &= i - 1;
        }
        sum
    }
    pub fn total_height(&self) -> f32 {
        self.prefix_sum(self.len())
    }
    pub fn block_at_y(&self, y: f32) -> usize {
        if self.is_empty() {
            return 0;
        }
        let target = y.clamp(0.0, self.total_height());
        let mut index = 0usize;
        let mut sum = 0.0;
        let mut bit = 1usize;
        while bit << 1 < self.tree.len() {
            bit <<= 1;
        }
        while bit > 0 {
            let next = index + bit;
            if next < self.tree.len() && sum + self.tree[next] <= target {
                index = next;
                sum += self.tree[next];
            }
            bit >>= 1;
        }
        index.min(self.len() - 1)
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollAnchor {
    pub block_id: u64,
    pub intra_block_y: f32,
    pub visual_position_hint: Option<VisualOffset>,
}

pub fn anchored_scroll_y(
    anchor: ScrollAnchor,
    blocks: &[VisualBlock],
    heights: &HeightIndex,
) -> Option<f32> {
    let index = blocks.iter().position(|b| b.block_id == anchor.block_id)?;
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
