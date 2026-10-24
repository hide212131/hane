//! Mermaid diagrams for fenced code blocks (Issue #587).
//!
//! This module holds everything about a diagram that does not need the editor
//! view: which blocks are drawn at all, the input/output limits, the Merman +
//! GPUI rendering pipeline that runs off the UI thread, and the store that keeps
//! recognition results and rendered pictures per block. `view/mermaid_diagrams.rs`
//! connects it to scheduling, presentation and input.
//!
//! The Markdown source is never changed here. A diagram is only an alternative
//! presentation of a block; every failure leaves the source on screen.

use gpui::{DevicePixels, RenderImage, SvgRenderer, SvgSize};
use hane_document::{Revision, SourceRange};
use hane_markdown::{BlockId, MermaidFence};
#[cfg(test)]
use hane_markdown::mermaid_fence;
use merman::svg::{RenderResourcePolicy, SvgPipeline};
use merman::{
    OperationControl, ParseOptions, RenderOutput, RenderRequest, Renderer, SvgEnvironment,
    SvgRequest,
};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Largest diagram body that is rendered.
pub(crate) const MAX_INPUT_BYTES: usize = 64 * 1024;
/// Most physical lines a diagram body may have.
pub(crate) const MAX_INPUT_LINES: usize = 512;
/// Room for the fence lines and surrounding blank lines when deciding whether a
/// block is small enough to be read at all. A block bigger than this is never
/// copied; only its first lines are looked at.
pub(crate) const BLOCK_READ_SLACK_BYTES: usize = 4 * 1024;
pub(crate) const BLOCK_READ_SLACK_LINES: usize = 8;
/// Diagrams rendered at the same time. A cancelled job still holds its slot
/// until it really stops.
pub(crate) const MAX_CONCURRENT_JOBS: usize = 2;
/// Diagrams (rendered, rendering or failed) kept at once.
pub(crate) const MAX_RETAINED_ENTRIES: usize = 64;
/// Largest SVG text one job parses. With the concurrency limit this bounds the
/// SVG held at any moment to 16 MiB; an SVG is dropped as soon as it has been
/// turned into a picture.
pub(crate) const MAX_SVG_BYTES: usize = 8 * 1024 * 1024;
/// Largest natural width or height an SVG may declare.
const MAX_NATURAL_SIDE: f32 = 16_384.0;
/// Longest side of one picture, in pixels.
pub(crate) const MAX_IMAGE_SIDE: u32 = 2_048;
/// Most pixels one picture may have.
pub(crate) const MAX_IMAGE_PIXELS: u64 = 4_194_304;
/// Pixel memory (width × height × 4) of every picture that is waiting,
/// displayed, or kept for reuse, including pictures still being produced.
pub(crate) const MAX_RETAINED_PIXEL_BYTES: usize = 32 * 1024 * 1024;
/// Pictures are produced at up to twice their natural size so they stay sharp on
/// dense displays, within the limits above.
const RASTER_OVERSAMPLE: f32 = 2.0;
/// Bytes per pixel of the pictures GPUI keeps.
const BYTES_PER_PIXEL: usize = 4;
/// Font stack handed to the diagram. Japanese-capable system fonts come first;
/// the final choice is made by the SVG renderer's font database.
const FONT_FAMILY: &str = "Hiragino Sans, Yu Gothic UI, Meiryo, Noto Sans CJK JP, sans-serif";

/// Why a Mermaid block is shown as source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MermaidFailure {
    /// The closing fence is missing.
    Unclosed,
    /// The diagram type is not one of the five that are drawn.
    Unsupported,
    /// Merman could not parse the description.
    Syntax,
    /// The body is over the input limits.
    TooLarge,
    /// The SVG carries an embedded image, which is never decoded.
    EmbeddedImage,
    /// The SVG has no usable natural size.
    InvalidSize,
    /// Merman stopped at its own resource limits.
    ResourceLimit,
    /// Rendering or rasterizing failed for another reason.
    RenderFailed,
    /// The picture would not fit the pixel memory budget.
    OverBudget,
    /// The job was stopped because its result is no longer wanted. Never shown.
    Cancelled,
}

impl MermaidFailure {
    /// Short reason shown next to the source.
    pub(crate) const fn reason(self) -> &'static str {
        match self {
            Self::Unclosed => "囲みが閉じていないため原文を表示しています",
            Self::Unsupported => "未対応の図種のため原文を表示しています",
            Self::Syntax => "図の記述を解釈できないため原文を表示しています",
            Self::TooLarge => "入力が上限を超えているため原文を表示しています",
            Self::EmbeddedImage => "埋め込み画像を含むため原文を表示しています",
            Self::InvalidSize => "図の大きさを確定できないため原文を表示しています",
            Self::ResourceLimit => "描画の上限を超えたため原文を表示しています",
            Self::RenderFailed => "図を描画できないため原文を表示しています",
            Self::OverBudget => "表示用メモリの上限を超えるため原文を表示しています",
            Self::Cancelled => "",
        }
    }
}

/// Colors and light/dark choice a diagram is drawn with. Part of a picture's
/// identity: a different palette is a different picture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Palette {
    pub(crate) dark: bool,
    pub(crate) background: u32,
    pub(crate) foreground: u32,
    pub(crate) surface: u32,
    pub(crate) border: u32,
    pub(crate) line: u32,
}

impl Palette {
    pub(crate) fn from_theme(theme: &crate::theme::Theme, dark: bool) -> Self {
        Self {
            dark,
            background: theme.media_background,
            foreground: theme.foreground,
            surface: theme.table_header_background,
            border: theme.table_border,
            line: theme.quote_foreground,
        }
    }
}

fn hex(color: u32) -> String {
    format!("#{:06x}", color & 0x00ff_ffff)
}

/// The `%%{init: …}%%` directive that carries the palette and font into the
/// diagram before it is parsed, so colors never have to be patched into the SVG
/// afterwards.
pub(crate) fn init_directive(palette: &Palette) -> String {
    let config = serde_json::json!({
        "theme": "base",
        "securityLevel": "strict",
        "htmlLabels": false,
        "flowchart": { "htmlLabels": false },
        "themeVariables": {
            "darkMode": palette.dark,
            "background": hex(palette.background),
            "fontFamily": FONT_FAMILY,
            "primaryColor": hex(palette.surface),
            "primaryTextColor": hex(palette.foreground),
            "primaryBorderColor": hex(palette.border),
            "secondaryColor": hex(palette.surface),
            "tertiaryColor": hex(palette.background),
            "lineColor": hex(palette.line),
            "textColor": hex(palette.foreground),
            "noteBkgColor": hex(palette.surface),
            "noteTextColor": hex(palette.foreground),
            "noteBorderColor": hex(palette.border),
            "actorBkg": hex(palette.surface),
            "actorBorder": hex(palette.border),
            "actorTextColor": hex(palette.foreground),
            "actorLineColor": hex(palette.line),
            "signalColor": hex(palette.line),
            "signalTextColor": hex(palette.foreground),
            "labelBoxBkgColor": hex(palette.surface),
            "labelBoxBorderColor": hex(palette.border),
            "labelTextColor": hex(palette.foreground),
            "loopTextColor": hex(palette.foreground),
        }
    });
    format!("%%{{init: {config}}}%%\n")
}

/// `code` with the palette directive in front of the diagram. A leading YAML
/// front matter has to stay first, so the directive goes after it.
pub(crate) fn themed_source(code: &str, palette: &Palette) -> String {
    let directive = init_directive(palette);
    let mut offset = 0;
    let mut opened = false;
    for line in code.split_inclusive('\n') {
        offset += line.len();
        let trimmed = line.trim_end();
        if !opened {
            if trimmed.is_empty() {
                continue;
            }
            if trimmed == "---" {
                opened = true;
                continue;
            }
            break;
        }
        if trimmed == "---" {
            return format!("{}{directive}{}", &code[..offset], &code[offset..]);
        }
    }
    if opened {
        // An unterminated front matter is the diagram's own problem.
        return code.to_owned();
    }
    format!("{directive}{code}")
}

/// First word of the diagram description, after blank lines, a leading front
/// matter and `%%` directives or comments.
pub(crate) fn diagram_header(code: &str) -> Option<&str> {
    let mut lines = code.lines().peekable();
    while lines.peek().is_some_and(|line| line.trim().is_empty()) {
        lines.next();
    }
    if lines.peek().is_some_and(|line| line.trim_end() == "---") {
        lines.next();
        loop {
            if lines.next()?.trim_end() == "---" {
                break;
            }
        }
    }
    lines
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("%%"))
        .and_then(|line| line.split_whitespace().next())
}

/// The five diagram types that are drawn.
pub(crate) fn is_supported_diagram(header: &str) -> bool {
    matches!(
        header,
        "flowchart" | "graph" | "sequenceDiagram" | "classDiagram" | "stateDiagram-v2" | "erDiagram"
    )
}

fn parse_length(value: &str) -> Option<f32> {
    let value = value.trim();
    let number = value.strip_suffix("px").unwrap_or(value).trim();
    let parsed: f32 = number.parse().ok()?;
    (parsed.is_finite() && parsed > 0.0).then_some(parsed)
}

fn parse_view_box(value: &str) -> Option<(f32, f32)> {
    let mut parts = value
        .split(|character: char| character.is_whitespace() || character == ',')
        .filter(|part| !part.is_empty())
        .map(str::parse::<f32>);
    let (_, _, width, height) = (
        parts.next()?.ok()?,
        parts.next()?.ok()?,
        parts.next()?.ok()?,
        parts.next()?.ok()?,
    );
    if parts.next().is_some() {
        return None;
    }
    (width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0)
        .then_some((width, height))
}

/// Reads the SVG as XML and accepts it only if it is safe and sized.
///
/// The root must be an `svg` with a finite, positive natural size (absolute
/// `width`/`height`, else the `viewBox`), and no `image` or `feImage` element may
/// exist anywhere: GPUI's SVG loader would otherwise read such a reference from
/// the local file system, and an embedded picture is never decoded. The text is
/// not changed.
pub(crate) fn inspect_svg(svg: &str) -> Result<(f32, f32), MermaidFailure> {
    if svg.len() > MAX_SVG_BYTES {
        return Err(MermaidFailure::TooLarge);
    }
    let document = roxmltree::Document::parse(svg).map_err(|_| MermaidFailure::RenderFailed)?;
    let root = document.root_element();
    if root.tag_name().name() != "svg" {
        return Err(MermaidFailure::InvalidSize);
    }
    let has_image = document.descendants().any(|node| {
        node.is_element() && {
            let name = node.tag_name().name();
            name.eq_ignore_ascii_case("image") || name.eq_ignore_ascii_case("feImage")
        }
    });
    if has_image {
        return Err(MermaidFailure::EmbeddedImage);
    }
    let absolute = root
        .attribute("width")
        .and_then(parse_length)
        .zip(root.attribute("height").and_then(parse_length));
    let (width, height) = absolute
        .or_else(|| root.attribute("viewBox").and_then(parse_view_box))
        .ok_or(MermaidFailure::InvalidSize)?;
    if !(width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0) {
        return Err(MermaidFailure::InvalidSize);
    }
    if width > MAX_NATURAL_SIDE || height > MAX_NATURAL_SIDE {
        return Err(MermaidFailure::TooLarge);
    }
    Ok((width, height))
}

/// Pixel size of the picture for a natural size: up to twice the natural size,
/// scaled down to stay inside the side and pixel limits, aspect ratio kept.
pub(crate) fn raster_size(natural: (f32, f32)) -> Option<(u32, u32)> {
    let (width, height) = natural;
    if !(width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0) {
        return None;
    }
    let scale = RASTER_OVERSAMPLE
        .min(MAX_IMAGE_SIDE as f32 / width.max(height))
        .min((MAX_IMAGE_PIXELS as f32 / (width * height)).sqrt());
    let pixel_width = ((width * scale).floor().max(1.0) as u32).min(MAX_IMAGE_SIDE);
    let pixel_height = ((height * scale).floor().max(1.0) as u32).min(MAX_IMAGE_SIDE);
    (u64::from(pixel_width) * u64::from(pixel_height) <= MAX_IMAGE_PIXELS)
        .then_some((pixel_width, pixel_height))
}

fn pixel_bytes(width: u32, height: u32) -> usize {
    (width as usize)
        .saturating_mul(height as usize)
        .saturating_mul(BYTES_PER_PIXEL)
}

/// Shared pixel-memory account. A reservation is taken before a picture is
/// produced and lives as long as the picture does, so waiting, displayed and
/// reusable pictures, and the ones still being drawn in the background, all
/// count against one limit.
#[derive(Clone, Debug)]
pub(crate) struct PixelBudget {
    used: Arc<AtomicUsize>,
    limit: usize,
}

impl PixelBudget {
    pub(crate) fn new() -> Self {
        Self::with_limit(MAX_RETAINED_PIXEL_BYTES)
    }

    pub(crate) fn with_limit(limit: usize) -> Self {
        Self {
            used: Arc::new(AtomicUsize::new(0)),
            limit,
        }
    }

    #[cfg(test)]
    pub(crate) fn used(&self) -> usize {
        self.used.load(Ordering::Acquire)
    }

    /// Takes `bytes` of the budget, or `None` when they do not fit.
    pub(crate) fn try_reserve(&self, bytes: usize) -> Option<PixelReservation> {
        let mut current = self.used.load(Ordering::Acquire);
        loop {
            let next = current
                .checked_add(bytes)
                .filter(|total| *total <= self.limit)?;
            match self.used.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(PixelReservation {
                        used: Arc::clone(&self.used),
                        bytes,
                    });
                }
                Err(actual) => current = actual,
            }
        }
    }
}

/// A taken share of a [`PixelBudget`], returned when dropped.
#[derive(Debug)]
pub(crate) struct PixelReservation {
    used: Arc<AtomicUsize>,
    bytes: usize,
}

impl Drop for PixelReservation {
    fn drop(&mut self) {
        self.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

/// Stops a job. Merman is asked through its own [`OperationControl`]; the flag
/// also lets the stages around Merman notice a stop request.
#[derive(Clone)]
pub(crate) struct JobControl {
    operation: OperationControl,
    cancelled: Arc<AtomicBool>,
}

impl JobControl {
    pub(crate) fn new() -> Self {
        Self {
            operation: OperationControl::new(),
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.operation.cancel();
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn operation(&self) -> OperationControl {
        self.operation.clone()
    }
}

/// Maps a Merman render error to a failure. The error type is non-exhaustive;
/// only a stop request and the resource limit are told apart, every other error
/// is a description Merman could not turn into a picture.
fn classify_error(error: &str) -> MermaidFailure {
    if error.starts_with("Cancelled") {
        MermaidFailure::Cancelled
    } else if error.starts_with("ResourceLimitExceeded") {
        MermaidFailure::ResourceLimit
    } else {
        MermaidFailure::Syntax
    }
}

/// Merman, strict parsing, the interactive resource limits and the `resvg_safe`
/// SVG pipeline. The returned text is exactly what Merman produced.
fn render_svg(
    code: &str,
    palette: &Palette,
    control: &JobControl,
) -> Result<String, MermaidFailure> {
    let source = themed_source(code, palette);
    let renderer = Renderer::new().with_parse_options(ParseOptions::strict());
    let request = SvgRequest {
        environment: SvgEnvironment::deterministic()
            .with_resource_policy(RenderResourcePolicy::interactive()),
        pipeline: Some(SvgPipeline::resvg_safe()),
        ..SvgRequest::default()
    };
    match renderer.render(RenderRequest::svg(
        source.as_str(),
        control.operation(),
        request,
    )) {
        Ok(RenderOutput::Svg(Some(svg))) => {
            let text = svg.into_parts().0;
            let text: &str = text.as_ref();
            Ok(text.to_owned())
        }
        Ok(_) => Err(MermaidFailure::RenderFailed),
        Err(error) => Err(classify_error(&format!("{error:?}"))),
    }
}

/// Everything a background job needs. Cheap to move to another thread.
pub(crate) struct RenderJob {
    pub(crate) code: Arc<str>,
    pub(crate) palette: Palette,
    pub(crate) control: JobControl,
    pub(crate) budget: PixelBudget,
    pub(crate) svg_renderer: SvgRenderer,
}

/// A finished picture and the budget it holds.
pub(crate) struct RenderedDiagram {
    pub(crate) image: Arc<RenderImage>,
    pub(crate) natural: (f32, f32),
    pub(crate) reservation: PixelReservation,
}

/// Renders one diagram off the UI thread: Merman → SVG text → XML check → GPUI's
/// SVG renderer. The SVG is never edited between Merman and GPUI.
pub(crate) fn render_diagram(job: &RenderJob) -> Result<RenderedDiagram, MermaidFailure> {
    let cancelled = || job.control.is_cancelled();
    if cancelled() {
        return Err(MermaidFailure::Cancelled);
    }
    let svg = render_svg(&job.code, &job.palette, &job.control)?;
    if cancelled() {
        return Err(MermaidFailure::Cancelled);
    }
    let natural = inspect_svg(&svg)?;
    let (pixel_width, pixel_height) = raster_size(natural).ok_or(MermaidFailure::TooLarge)?;
    let reservation = job
        .budget
        .try_reserve(pixel_bytes(pixel_width, pixel_height))
        .ok_or(MermaidFailure::OverBudget)?;
    let parsed = job
        .svg_renderer
        .parse_svg(svg.as_bytes())
        .map_err(|_| MermaidFailure::RenderFailed)?;
    drop(svg);
    if cancelled() {
        return Err(MermaidFailure::Cancelled);
    }
    let exact = gpui::size(
        DevicePixels(i32::try_from(pixel_width).map_err(|_| MermaidFailure::TooLarge)?),
        DevicePixels(i32::try_from(pixel_height).map_err(|_| MermaidFailure::TooLarge)?),
    );
    let image = job
        .svg_renderer
        .render_parsed(&parsed, SvgSize::ExactSize(exact))
        .map_err(|_| MermaidFailure::RenderFailed)?;
    if cancelled() {
        return Err(MermaidFailure::Cancelled);
    }
    Ok(RenderedDiagram {
        image,
        natural,
        reservation,
    })
}

/// What reading one indexed block said about Mermaid.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Recognition {
    /// Not a Mermaid fence, or not a block this feature looks at.
    Plain,
    /// A Mermaid fence that stays source, and why.
    Rejected(MermaidFailure),
    /// A closed fence with a drawable diagram type and an input within limits.
    Diagram(Arc<str>),
}

/// Decides what the parser's reading of a block means for drawing.
pub(crate) fn classify_fence(fence: Option<&MermaidFence>) -> Recognition {
    let Some(fence) = fence else {
        return Recognition::Plain;
    };
    if !fence.closed {
        return Recognition::Rejected(MermaidFailure::Unclosed);
    }
    if fence.code.len() > MAX_INPUT_BYTES || fence.code.lines().count() > MAX_INPUT_LINES {
        return Recognition::Rejected(MermaidFailure::TooLarge);
    }
    match diagram_header(&fence.code) {
        Some(header) if is_supported_diagram(header) => {
            Recognition::Diagram(Arc::from(fence.code.as_str()))
        }
        _ => Recognition::Rejected(MermaidFailure::Unsupported),
    }
}

/// Recognizes a block from its full source text.
#[cfg(test)]
pub(crate) fn recognize(source_range: SourceRange, text: &str) -> Recognition {
    classify_fence(mermaid_fence(source_range, text).as_ref())
}

/// Whether the first non-blank line of `head` could be a Mermaid opening fence.
/// Only a cheap filter in front of the parser, which makes the real decision.
pub(crate) fn head_mentions_mermaid(head: &str) -> bool {
    head.lines()
        .find(|line| !line.trim().is_empty())
        .is_some_and(|line| line.to_ascii_lowercase().contains("mermaid"))
}

/// Recognition of one block, valid for the block's revision and range.
#[derive(Clone, Debug)]
pub(crate) struct Recognized {
    pub(crate) revision: Revision,
    pub(crate) source_range: SourceRange,
    /// Absolute document lines of the fenced construct.
    pub(crate) lines: Range<usize>,
    pub(crate) recognition: Recognition,
}

/// Identity of a picture besides its block: the exact diagram text and the
/// palette. Anything else that changes the picture is a new job.
#[derive(Clone, Debug)]
struct EntryKey {
    code: Arc<str>,
    dark: bool,
}

impl EntryKey {
    fn matches(&self, code: &Arc<str>, dark: bool) -> bool {
        self.dark == dark && (Arc::ptr_eq(&self.code, code) || *self.code == **code)
    }
}

/// A finished picture kept for a block.
pub(crate) struct ReadyDiagram {
    image: Arc<RenderImage>,
    natural: (f32, f32),
    reservation: PixelReservation,
}

impl ReadyDiagram {
    pub(crate) fn natural(&self) -> (f32, f32) {
        self.natural
    }
}

enum EntryState {
    Rendering { token: u64, control: JobControl },
    Ready(ReadyDiagram),
    Failed(MermaidFailure),
}

struct Entry {
    key: EntryKey,
    state: EntryState,
    last_used: u64,
}

/// A picture the window must forget before its memory is returned. The budget
/// share stays taken until the picture has been handed back.
pub(crate) struct Retired {
    image: Arc<RenderImage>,
    reservation: PixelReservation,
}

impl Retired {
    pub(crate) fn into_parts(self) -> (Arc<RenderImage>, PixelReservation) {
        (self.image, self.reservation)
    }
}

/// What the painter needs for one diagram row.
#[derive(Clone)]
pub(crate) struct DiagramPaint {
    pub(crate) image: Arc<RenderImage>,
    pub(crate) natural: (f32, f32),
}

/// How a finished job changed the store.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Finished {
    /// The job was no longer wanted; its result was dropped.
    Discarded,
    /// The block's entry changed.
    Updated,
}

/// Recognition results and pictures of one view.
pub(crate) struct MermaidStore {
    recognized: HashMap<BlockId, Recognized>,
    entries: HashMap<BlockId, Entry>,
    running: usize,
    budget: PixelBudget,
    frame: u64,
    next_token: u64,
    retired: Vec<Retired>,
}

impl MermaidStore {
    pub(crate) fn new() -> Self {
        Self::with_budget(PixelBudget::new())
    }

    pub(crate) fn with_budget(budget: PixelBudget) -> Self {
        Self {
            recognized: HashMap::new(),
            entries: HashMap::new(),
            running: 0,
            budget,
            frame: 0,
            next_token: 0,
            retired: Vec::new(),
        }
    }

    pub(crate) fn budget(&self) -> PixelBudget {
        self.budget.clone()
    }

    #[cfg(test)]
    pub(crate) fn running_jobs(&self) -> usize {
        self.running
    }

    #[cfg(test)]
    pub(crate) fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// How many jobs have ever been registered; tests use it to prove that an
    /// unchanged diagram or a repeated failure is not rendered again.
    #[cfg(test)]
    pub(crate) fn jobs_started(&self) -> u64 {
        self.next_token
    }

    /// Starts a frame and hands over the pictures the window has to forget.
    pub(crate) fn begin_frame(&mut self) -> Vec<Retired> {
        self.frame += 1;
        std::mem::take(&mut self.retired)
    }

    /// The recognition of a block, only while it describes the block as it is.
    pub(crate) fn current(
        &self,
        id: BlockId,
        revision: Revision,
        source_range: SourceRange,
    ) -> Option<&Recognized> {
        self.recognized
            .get(&id)
            .filter(|known| known.revision == revision && known.source_range == source_range)
    }

    pub(crate) fn remember(&mut self, id: BlockId, recognized: Recognized) {
        self.recognized.insert(id, recognized);
    }

    /// The finished picture for exactly this diagram text and palette.
    pub(crate) fn ready(&self, id: BlockId, code: &Arc<str>, dark: bool) -> Option<&ReadyDiagram> {
        self.entries
            .get(&id)
            .filter(|entry| entry.key.matches(code, dark))
            .and_then(|entry| match &entry.state {
                EntryState::Ready(ready) => Some(ready),
                _ => None,
            })
    }

    /// Whether the block already has a job, a picture or a failure for exactly
    /// this diagram text and palette.
    pub(crate) fn has_entry(&self, id: BlockId, code: &Arc<str>, dark: bool) -> bool {
        self.entries
            .get(&id)
            .is_some_and(|entry| entry.key.matches(code, dark))
    }

    /// Moves a finished entry made for exactly this diagram text and palette
    /// from another block id to `id`. Block ids are positions in a freshly built
    /// index, so an edit that adds a block above shifts them; the picture or the
    /// recorded failure is still the right one for the same text. Ids in
    /// `claimed` belong to blocks that use their own entry this frame and are
    /// never taken from.
    pub(crate) fn adopt(
        &mut self,
        id: BlockId,
        code: &Arc<str>,
        dark: bool,
        claimed: &HashSet<BlockId>,
    ) -> bool {
        if self.has_entry(id, code, dark) {
            return false;
        }
        let donor = self
            .entries
            .iter()
            .find(|(other, entry)| {
                **other != id
                    && !claimed.contains(*other)
                    && entry.key.matches(code, dark)
                    && !matches!(entry.state, EntryState::Rendering { .. })
            })
            .map(|(other, _)| *other);
        let Some(entry) = donor.and_then(|donor| self.entries.remove(&donor)) else {
            return false;
        };
        self.remove_entry(id);
        let last_used = self.frame;
        self.entries.insert(id, Entry { last_used, ..entry });
        true
    }

    pub(crate) fn touch(&mut self, id: BlockId) {
        let frame = self.frame;
        if let Some(entry) = self.entries.get_mut(&id) {
            entry.last_used = frame;
        }
    }

    /// The short reason a block is shown as source, if it has one. Only a
    /// recognition made for this very range and a failure recorded for exactly
    /// this text and palette speak: a block that changed since has no reason
    /// until it has been read again.
    pub(crate) fn note(
        &self,
        id: BlockId,
        source_range: SourceRange,
        dark: bool,
    ) -> Option<&'static str> {
        let known = self
            .recognized
            .get(&id)
            .filter(|known| known.source_range == source_range)?;
        match &known.recognition {
            Recognition::Plain => None,
            Recognition::Rejected(failure) => Some(failure.reason()),
            Recognition::Diagram(code) => {
                let entry = self
                    .entries
                    .get(&id)
                    .filter(|entry| entry.key.matches(code, dark))?;
                match entry.state {
                    EntryState::Failed(failure) => Some(failure.reason()),
                    _ => None,
                }
            }
        }
    }

    /// The picture to paint on the owning row of a block's diagram.
    pub(crate) fn paint(&self, id: BlockId) -> Option<DiagramPaint> {
        match &self.entries.get(&id)?.state {
            EntryState::Ready(ready) => Some(DiagramPaint {
                image: Arc::clone(&ready.image),
                natural: ready.natural,
            }),
            _ => None,
        }
    }

    /// Registers a job for `id` if a slot is free, stopping whatever the block
    /// had before. Returns the token the job must present when it finishes.
    pub(crate) fn begin(
        &mut self,
        id: BlockId,
        code: Arc<str>,
        dark: bool,
        control: JobControl,
    ) -> Option<u64> {
        if self.running >= MAX_CONCURRENT_JOBS {
            return None;
        }
        if !self.entries.contains_key(&id)
            && self.entries.len() >= MAX_RETAINED_ENTRIES
            && !self.retire_least_recently_used()
        {
            return None;
        }
        self.remove_entry(id);
        let token = self.next_token;
        self.next_token += 1;
        self.entries.insert(
            id,
            Entry {
                key: EntryKey { code, dark },
                state: EntryState::Rendering { token, control },
                last_used: self.frame,
            },
        );
        self.running += 1;
        Some(token)
    }

    /// A job's thread has stopped, whatever became of its result.
    pub(crate) fn job_ended(&mut self) {
        self.running = self.running.saturating_sub(1);
    }

    /// Applies a finished job unless a newer job or a removal replaced it.
    pub(crate) fn finish(
        &mut self,
        id: BlockId,
        token: u64,
        result: Result<RenderedDiagram, MermaidFailure>,
    ) -> Finished {
        let current = self.entries.get(&id).is_some_and(|entry| {
            matches!(&entry.state, EntryState::Rendering { token: held, .. } if *held == token)
        });
        if !current {
            return Finished::Discarded;
        }
        match result {
            Ok(rendered) => {
                self.set_state(
                    id,
                    EntryState::Ready(ReadyDiagram {
                        image: rendered.image,
                        natural: rendered.natural,
                        reservation: rendered.reservation,
                    }),
                );
                Finished::Updated
            }
            Err(MermaidFailure::Cancelled) => {
                self.entries.remove(&id);
                Finished::Discarded
            }
            Err(MermaidFailure::OverBudget) => {
                if self.retire_unused_ready() {
                    // Room was made; the block asks again on the next frame.
                    self.entries.remove(&id);
                } else {
                    self.set_state(id, EntryState::Failed(MermaidFailure::OverBudget));
                }
                Finished::Updated
            }
            Err(failure) => {
                self.set_state(id, EntryState::Failed(failure));
                Finished::Updated
            }
        }
    }

    fn set_state(&mut self, id: BlockId, state: EntryState) {
        if let Some(entry) = self.entries.get_mut(&id) {
            entry.state = state;
        }
    }

    /// Stops and forgets every entry whose block is not in `keep`.
    pub(crate) fn retain_blocks(&mut self, keep: &HashSet<BlockId>) {
        let gone = self
            .entries
            .keys()
            .copied()
            .filter(|id| !keep.contains(id))
            .collect::<Vec<_>>();
        for id in gone {
            self.remove_entry(id);
        }
        self.recognized.retain(|id, _| keep.contains(id));
    }

    /// Forgets everything: another document replaced this one.
    pub(crate) fn reset(&mut self) {
        let all = self.entries.keys().copied().collect::<Vec<_>>();
        for id in all {
            self.remove_entry(id);
        }
        self.recognized.clear();
    }

    /// Everything the window still holds, for a view that is going away.
    pub(crate) fn release_all(&mut self) -> Vec<Retired> {
        self.reset();
        std::mem::take(&mut self.retired)
    }

    fn remove_entry(&mut self, id: BlockId) {
        let Some(entry) = self.entries.remove(&id) else {
            return;
        };
        match entry.state {
            EntryState::Rendering { control, .. } => control.cancel(),
            EntryState::Ready(ready) => self.retired.push(Retired {
                image: ready.image,
                reservation: ready.reservation,
            }),
            EntryState::Failed(_) => {}
        }
    }

    /// Drops the entry used least recently, if any was not used this frame.
    fn retire_least_recently_used(&mut self) -> bool {
        let frame = self.frame;
        let oldest = self
            .entries
            .iter()
            .filter(|(_, entry)| entry.last_used < frame)
            .min_by_key(|(_, entry)| entry.last_used)
            .map(|(id, _)| *id);
        match oldest {
            Some(id) => {
                self.remove_entry(id);
                true
            }
            None => false,
        }
    }

    /// Drops every finished picture that was not used this frame.
    fn retire_unused_ready(&mut self) -> bool {
        let frame = self.frame;
        let unused = self
            .entries
            .iter()
            .filter(|(_, entry)| {
                entry.last_used < frame && matches!(entry.state, EntryState::Ready(_))
            })
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        let any = !unused.is_empty();
        for id in unused {
            self.remove_entry(id);
        }
        any
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hane_document::SourceRange;

    fn palette(dark: bool) -> Palette {
        Palette {
            dark,
            background: 0x292a2d,
            foreground: 0xe8e5df,
            surface: 0x34363a,
            border: 0x55585f,
            line: 0xaaa39a,
        }
    }

    fn fence(code: &str) -> Recognition {
        let text = format!("```mermaid\n{code}\n```\n");
        recognize(SourceRange::new(0, text.len()), &text)
    }

    #[test]
    fn the_five_diagram_types_are_drawn_and_others_stay_source() {
        for code in [
            "flowchart TD\n  A[開始] --> B[終了]",
            "graph LR\n  A --> B",
            "sequenceDiagram\n  太郎->>花子: こんにちは",
            "classDiagram\n  class 動物",
            "stateDiagram-v2\n  [*] --> 待機",
            "erDiagram\n  客 ||--o{ 注文 : 発注",
            "%% コメント\nflowchart TD\n  A --> B",
            "---\ntitle: 図\n---\nflowchart TD\n  A --> B",
        ] {
            assert!(matches!(fence(code), Recognition::Diagram(_)), "{code:?}");
        }
        for code in [
            "pie\n  \"a\": 1",
            "gantt\n  title x",
            "stateDiagram\n  [*] --> A",
            "mindmap\n  root",
            "",
            "   \n",
        ] {
            assert_eq!(
                fence(code),
                Recognition::Rejected(MermaidFailure::Unsupported),
                "{code:?}"
            );
        }
    }

    #[test]
    fn input_limits_and_unclosed_fences_stay_source() {
        let long_line = format!("flowchart TD\n  A[{}] --> B", "x".repeat(MAX_INPUT_BYTES));
        assert_eq!(
            fence(&long_line),
            Recognition::Rejected(MermaidFailure::TooLarge)
        );
        let many_lines = format!("flowchart TD\n{}", "  A --> B\n".repeat(MAX_INPUT_LINES));
        assert_eq!(
            fence(&many_lines),
            Recognition::Rejected(MermaidFailure::TooLarge)
        );
        let at_limit = format!("flowchart TD\n{}", "  A --> B\n".repeat(MAX_INPUT_LINES - 2));
        assert!(matches!(fence(at_limit.trim_end()), Recognition::Diagram(_)));

        let unclosed = "```mermaid\nflowchart TD\n  A --> B\n";
        assert_eq!(
            recognize(SourceRange::new(0, unclosed.len()), unclosed),
            Recognition::Rejected(MermaidFailure::Unclosed)
        );
        let other = "```rust\nfn main() {}\n```\n";
        assert_eq!(
            recognize(SourceRange::new(0, other.len()), other),
            Recognition::Plain
        );
    }

    #[test]
    fn the_head_filter_only_looks_at_the_first_non_blank_line() {
        assert!(head_mentions_mermaid("\n\n```Mermaid\nx"));
        assert!(head_mentions_mermaid("~~~ mermaid"));
        assert!(!head_mentions_mermaid("```rust\n// mermaid\n"));
        assert!(!head_mentions_mermaid(""));
    }

    #[test]
    fn the_palette_directive_is_valid_json_in_front_of_the_diagram() {
        let source = themed_source("flowchart TD\n  A --> B", &palette(true));
        assert!(source.starts_with("%%{init: {"));
        let body = source
            .strip_suffix("}%%\nflowchart TD\n  A --> B")
            .expect("the diagram follows the directive unchanged");
        let json = body.strip_prefix("%%{init: ").expect("directive start");
        let value: serde_json::Value = serde_json::from_str(json).expect("json");
        assert_eq!(value["themeVariables"]["darkMode"], serde_json::json!(true));
        assert_eq!(value["themeVariables"]["background"], "#292a2d");
        assert_eq!(value["themeVariables"]["primaryTextColor"], "#e8e5df");
        let light = themed_source("flowchart TD\n  A --> B", &palette(false));
        assert_ne!(source, light);
        assert!(light.contains("\"darkMode\":false"));
    }

    #[test]
    fn a_front_matter_stays_first() {
        let code = "---\ntitle: 図\n---\nflowchart TD\n  A --> B";
        let source = themed_source(code, &palette(false));
        assert!(source.starts_with("---\ntitle: 図\n---\n%%{init: "));
        assert!(source.ends_with("}%%\nflowchart TD\n  A --> B"));
        assert_eq!(
            themed_source("---\ntitle: x\nflowchart TD", &palette(false)),
            "---\ntitle: x\nflowchart TD"
        );
    }

    #[test]
    fn svg_size_comes_from_absolute_dimensions_or_the_view_box() {
        let percent = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100%" viewBox="0 0 320 180"></svg>"#;
        assert_eq!(inspect_svg(percent), Ok((320.0, 180.0)));
        let absolute = r#"<svg xmlns="http://www.w3.org/2000/svg" width="200px" height="100" viewBox="0 0 10 10"></svg>"#;
        assert_eq!(inspect_svg(absolute), Ok((200.0, 100.0)));
        for bad in [
            r#"<svg xmlns="http://www.w3.org/2000/svg"></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 0 10"></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 NaN 10"></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 inf 10"></svg>"#,
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10"></svg>"#,
            r#"<div></div>"#,
        ] {
            assert_eq!(inspect_svg(bad), Err(MermaidFailure::InvalidSize), "{bad}");
        }
        let huge = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 99999 10"></svg>"#;
        assert_eq!(inspect_svg(huge), Err(MermaidFailure::TooLarge));
        assert_eq!(
            inspect_svg("<svg"),
            Err(MermaidFailure::RenderFailed),
            "malformed XML"
        );
    }

    #[test]
    fn an_image_or_fe_image_element_anywhere_refuses_the_svg() {
        for element in [
            r#"<image href="data:image/png;base64,AAAA" width="4" height="4"/>"#,
            r#"<g><g><IMAGE xlink:href="file:///etc/passwd"/></g></g>"#,
            r##"<filter id="f"><feImage href="x.png"/></filter>"##,
        ] {
            let svg = format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 10 10">{element}</svg>"#
            );
            assert_eq!(
                inspect_svg(&svg),
                Err(MermaidFailure::EmbeddedImage),
                "{element}"
            );
        }
        let plain = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><text>image</text></svg>"#;
        assert_eq!(inspect_svg(plain), Ok((10.0, 10.0)));
    }

    #[test]
    fn raster_size_keeps_the_ratio_and_stays_inside_the_limits() {
        assert_eq!(raster_size((300.0, 100.0)), Some((600, 200)));
        let wide = raster_size((10_000.0, 100.0)).expect("wide");
        assert_eq!(wide.0, MAX_IMAGE_SIDE);
        assert!(wide.1 >= 1);
        let square = raster_size((8_000.0, 8_000.0)).expect("square");
        assert!(square.0 <= MAX_IMAGE_SIDE && square.1 <= MAX_IMAGE_SIDE);
        assert!(u64::from(square.0) * u64::from(square.1) <= MAX_IMAGE_PIXELS);
        let tiny = raster_size((0.1, 0.1)).expect("tiny");
        assert!(tiny.0 >= 1 && tiny.1 >= 1);
        for bad in [(0.0, 1.0), (1.0, f32::NAN), (f32::INFINITY, 1.0), (-1.0, 1.0)] {
            assert_eq!(raster_size(bad), None);
        }
    }

    #[test]
    fn the_pixel_budget_counts_reservations_until_they_are_dropped() {
        let budget = PixelBudget::with_limit(100);
        let first = budget.try_reserve(60).expect("fits");
        assert_eq!(budget.used(), 60);
        assert!(budget.try_reserve(41).is_none());
        let second = budget.try_reserve(40).expect("exactly fits");
        assert_eq!(budget.used(), 100);
        drop(first);
        assert_eq!(budget.used(), 40);
        assert!(budget.try_reserve(60).is_some());
        drop(second);
        assert_eq!(budget.used(), 0);
        assert!(budget.try_reserve(usize::MAX).is_none());
    }

    #[test]
    fn a_stale_or_cancelled_job_never_changes_the_store() {
        let mut store = MermaidStore::new();
        let code: Arc<str> = Arc::from("flowchart TD\n  A --> B");
        let id = BlockId(1);
        let first_control = JobControl::new();
        let first = store
            .begin(id, code.clone(), false, first_control.clone())
            .expect("slot");
        assert_eq!(store.running_jobs(), 1);

        // A newer request for the same block stops the older job.
        let second = store
            .begin(id, code.clone(), true, JobControl::new())
            .expect("slot");
        assert!(first_control.is_cancelled());
        assert_ne!(first, second);
        assert_eq!(
            store.finish(id, first, Err(MermaidFailure::Syntax)),
            Finished::Discarded
        );
        assert!(store.has_entry(id, &code, true));
        assert!(!store.has_entry(id, &code, false));
        assert_eq!(store.note(id, SourceRange::new(0, 1), true), None);

        assert_eq!(
            store.finish(id, second, Err(MermaidFailure::Cancelled)),
            Finished::Discarded
        );
        assert_eq!(store.entry_count(), 0);
        // A late result for a block that no longer exists is dropped as well.
        assert_eq!(
            store.finish(id, second, Err(MermaidFailure::Syntax)),
            Finished::Discarded
        );
    }

    #[test]
    fn failures_are_kept_per_exact_text_and_shown_as_a_reason() {
        let mut store = MermaidStore::new();
        let code: Arc<str> = Arc::from("flowchart TD\n  A -->");
        let id = BlockId(7);
        let range = SourceRange::new(10, 50);
        store.remember(
            id,
            Recognized {
                revision: Revision(3),
                source_range: range,
                lines: 2..6,
                recognition: Recognition::Diagram(code.clone()),
            },
        );
        let token = store
            .begin(id, code.clone(), false, JobControl::new())
            .expect("slot");
        assert_eq!(store.note(id, range, false), None);
        store.job_ended();
        assert_eq!(
            store.finish(id, token, Err(MermaidFailure::Syntax)),
            Finished::Updated
        );
        assert_eq!(
            store.note(id, range, false),
            Some(MermaidFailure::Syntax.reason())
        );
        // The same failure is not asked again while the text and palette match…
        assert!(store.has_entry(id, &code, false));
        // …and a different palette, text or range does not inherit it.
        assert_eq!(store.note(id, range, true), None);
        assert!(!store.has_entry(id, &Arc::from("flowchart TD\n  A --> B"), false));
        assert_eq!(store.note(id, SourceRange::new(11, 51), false), None);
        assert!(store.current(id, Revision(3), range).is_some());
        assert!(store.current(id, Revision(4), range).is_none());
        assert_eq!(store.running_jobs(), 0);
    }

    #[test]
    fn concurrency_and_entry_limits_are_enforced() {
        let mut store = MermaidStore::new();
        let code: Arc<str> = Arc::from("flowchart TD");
        for index in 0..MAX_CONCURRENT_JOBS {
            assert!(
                store
                    .begin(BlockId(index as u64), code.clone(), false, JobControl::new())
                    .is_some()
            );
        }
        assert!(
            store
                .begin(BlockId(99), code.clone(), false, JobControl::new())
                .is_none(),
            "a third job waits for a free slot"
        );
        for _ in 0..MAX_CONCURRENT_JOBS {
            store.job_ended();
        }

        let mut store = MermaidStore::new();
        for index in 0..MAX_RETAINED_ENTRIES {
            let id = BlockId(index as u64);
            let token = store
                .begin(id, code.clone(), false, JobControl::new())
                .expect("slot");
            store.job_ended();
            store.finish(id, token, Err(MermaidFailure::Syntax));
        }
        assert_eq!(store.entry_count(), MAX_RETAINED_ENTRIES);
        // Everything was used this frame, so there is nothing to give up.
        assert!(
            store
                .begin(BlockId(1_000), code.clone(), false, JobControl::new())
                .is_none()
        );
        store.begin_frame();
        assert!(
            store
                .begin(BlockId(1_000), code.clone(), false, JobControl::new())
                .is_some(),
            "an entry unused this frame is given up"
        );
        assert_eq!(store.entry_count(), MAX_RETAINED_ENTRIES);
    }

    #[test]
    fn an_entry_follows_its_text_when_block_ids_shift() {
        let mut store = MermaidStore::new();
        let code: Arc<str> = Arc::from("flowchart TD\n  A -->");
        let other: Arc<str> = Arc::from("flowchart TD\n  B --> C");
        let token = store
            .begin(BlockId(1), code.clone(), false, JobControl::new())
            .expect("slot");
        store.job_ended();
        store.finish(BlockId(1), token, Err(MermaidFailure::Syntax));
        let claimed = HashSet::new();

        // Another text, another palette, or an entry still being rendered is
        // not taken over.
        assert!(!store.adopt(BlockId(2), &other, false, &claimed));
        assert!(!store.adopt(BlockId(2), &code, true, &claimed));
        let rendering = store
            .begin(BlockId(3), other.clone(), false, JobControl::new())
            .expect("slot");
        assert!(!store.adopt(BlockId(4), &other, false, &claimed));
        store.job_ended();
        store.finish(BlockId(3), rendering, Err(MermaidFailure::Cancelled));

        // A block that uses its own entry is never robbed.
        assert!(!store.adopt(BlockId(2), &code, false, &HashSet::from([BlockId(1)])));
        assert!(store.adopt(BlockId(2), &code, false, &claimed));
        assert!(store.has_entry(BlockId(2), &code, false));
        assert!(!store.has_entry(BlockId(1), &code, false));
        // Nothing is taken when the block already has its own entry.
        assert!(!store.adopt(BlockId(2), &code, false, &claimed));
    }

    #[test]
    fn leaving_the_retained_window_or_the_document_stops_the_jobs() {
        let mut store = MermaidStore::new();
        let code: Arc<str> = Arc::from("flowchart TD");
        let keep_control = JobControl::new();
        let drop_control = JobControl::new();
        store.begin(BlockId(1), code.clone(), false, keep_control.clone());
        store.begin(BlockId(2), code.clone(), false, drop_control.clone());
        store.retain_blocks(&HashSet::from([BlockId(1)]));
        assert!(!keep_control.is_cancelled());
        assert!(drop_control.is_cancelled());
        assert_eq!(store.entry_count(), 1);
        store.reset();
        assert!(keep_control.is_cancelled());
        assert_eq!(store.entry_count(), 0);
    }

    #[test]
    fn merman_draws_each_supported_type_with_japanese_labels() {
        for (name, code) in [
            ("flowchart", "flowchart TD\n  A[開始] --> B{判断}\n  B -->|はい| C[終了]"),
            ("graph", "graph LR\n  A[日本語] --> B[図]"),
            (
                "sequence",
                "sequenceDiagram\n  participant 太郎\n  participant 花子\n  太郎->>花子: こんにちは",
            ),
            (
                "class",
                "classDiagram\n  class 動物 {\n    +名前\n    +鳴く()\n  }\n  動物 <|-- 犬",
            ),
            ("state", "stateDiagram-v2\n  [*] --> 待機\n  待機 --> 実行 : 開始"),
            ("er", "erDiagram\n  客 ||--o{ 注文 : 発注"),
        ] {
            for dark in [false, true] {
                let svg = render_svg(code, &palette(dark), &JobControl::new())
                    .unwrap_or_else(|failure| panic!("{name} ({dark}): {failure:?}"));
                let (width, height) = inspect_svg(&svg)
                    .unwrap_or_else(|failure| panic!("{name} ({dark}): {failure:?}"));
                assert!(width > 0.0 && height > 0.0, "{name}");
                assert!(raster_size((width, height)).is_some(), "{name}");
            }
        }
    }

    #[test]
    fn merman_refuses_a_broken_description() {
        let broken = render_svg(
            "flowchart TD\n  A --> [[[",
            &palette(false),
            &JobControl::new(),
        );
        assert!(broken.is_err());
    }

    #[test]
    fn a_job_control_remembers_that_it_was_stopped() {
        let control = JobControl::new();
        assert!(!control.is_cancelled());
        let copy = control.clone();
        control.cancel();
        assert!(copy.is_cancelled());
    }
}
