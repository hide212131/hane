#![allow(
    clippy::too_many_lines,
    clippy::similar_names,
    clippy::cast_possible_truncation,
    clippy::redundant_closure_for_method_calls,
    clippy::needless_pass_by_value,
    reason = "line rendering keeps GPUI-facing arguments and run assembly together"
)]

use crate::ranges::partition;
use crate::theme::Theme;
use gpui::{
    Div, FontWeight, IntoElement, ObjectFit, ParentElement, Styled, StyledImage, div, img,
    prelude::FluentBuilder, px, rgb,
};
use hane_document::{Bias, LineId, SourceOffset, SourceRange, TextBuffer};
use hane_editor::Editor;
use hane_markdown::IndexedBlock;
use hane_presentation::{
    BlockDisplay, BlockLayout, BlockLine, BlockSurface, BlockTint, BlockWeight, BlockWindow,
    InlineDisplay, JoinedParse, LayoutLine, LineWrap, VisualBlock, VisualLine, VisualOffset,
    block_is_joinable, block_line_span, expected_disclosures, present_block, trailing_blank_lines,
};
use hane_session::ResourceResolver;
use std::ops::Range;

fn line_owns_cursor(range: SourceRange, cursor: SourceOffset, is_final_line: bool) -> bool {
    range.start <= cursor && (cursor < range.end || (is_final_line && cursor == range.end))
}

/// Disclosure (caret, selection or IME range) that touches `range`, which may
/// be a single physical line's range or a whole block's span. `is_final`
/// mirrors [`line_owns_cursor`]'s own end-of-range tweak: true when `range`
/// reaches the document's end, so a caret resting right after the last
/// character is still owned by it rather than by nothing.
fn range_disclosure(editor: &Editor, range: SourceRange, is_final: bool) -> Option<SourceRange> {
    let selection = editor.selection().range();
    let disclosure = if selection.is_empty() {
        line_owns_cursor(range, selection.start, is_final).then_some(selection)
    } else if selection.intersects(range) {
        Some(SourceRange {
            start: selection.start.max(range.start),
            end: selection.end.min(range.end),
        })
    } else {
        None
    };
    editor
        .ime()
        .and_then(|ime| {
            ime.current_range
                .intersects(range)
                .then_some(ime.current_range)
        })
        .or(disclosure)
}

/// Lines a joinable block's own span may reach before `presented_block` stops
/// reading and reparsing all of it synchronously on every viewport miss.
///
/// A block is not bounded — a document without a blank line in it is a single
/// paragraph (see [`VisualBlock`] in `hane_presentation`) — so nothing here
/// can assume a joinable block's whole span is cheap to read. This line budget
/// is paired with [`JOIN_SYNC_BYTE_BUDGET`]; both must be satisfied before a
/// full-block parse is allowed on the render path.
pub(crate) const JOIN_SYNC_LINE_BUDGET: usize = 4_096;

/// Source bytes a joinable block may contain before its whole-span parse must
/// move off the render path. This starts at the same 256 KiB bound Hane already
/// uses for incremental Markdown re-synchronization work. The concrete value is
/// a tuning parameter; the invariant is that a block must satisfy both this
/// byte budget and [`JOIN_SYNC_LINE_BUDGET`] before synchronous full parsing.
pub(crate) const JOIN_SYNC_BYTE_BUDGET: usize = 256 * 1024;

/// Whether reading, copying, joining and parsing this whole block is bounded
/// enough for the synchronous render path. The same predicate is used by
/// `EditorView::schedule_joined_parse`: failing either limit must both prevent
/// a synchronous whole-block parse and make the block eligible for a cached
/// background [`JoinedParse`], otherwise a byte-large, low-line-count block
/// could fall between the two policies and never receive shared semantics.
pub(crate) fn block_fits_sync_join_budget(block: &IndexedBlock, span: &Range<usize>) -> bool {
    let bytes = block
        .source_range
        .end
        .0
        .saturating_sub(block.source_range.start.0);
    span.len() <= JOIN_SYNC_LINE_BUDGET && bytes <= JOIN_SYNC_BYTE_BUDGET
}

/// Presents the lines of one indexed Markdown block that reach `visible`.
///
/// A block is not bounded — a document without a blank line in it is a single
/// paragraph — so only the lines inside the visible window are drawn; the rest
/// are counted and stand in as space. For a joinable block (see
/// [`block_is_joinable`]) the source fetched for parsing is the block's whole
/// span, not just `visible`, because CommonMark resolves `**`/`_`/`` ` `` across
/// the whole paragraph and a marker whose other half falls outside what was
/// read cannot be recognized — a fixed window around `visible` would still miss
/// a match that falls further away, and the same construct would render
/// differently depending on scroll position. Which lines are literal code or
/// table syntax is decided in presentation, from the block kind; this crate
/// never inspects the source for fences or pipes.
///
/// `joined`, when given, is a cached whole-span parse for this exact block
/// (see [`JoinedParse`]); it is what lets a block that exceeds either sync
/// budget still resolve a marker pair arbitrarily far apart without this
/// function reading the whole span itself on every call.
pub(crate) fn presented_block(
    editor: &Editor,
    block: &IndexedBlock,
    visible: &Range<usize>,
    joined: Option<&JoinedParse>,
) -> Option<VisualBlock> {
    let document = editor.document();
    let span = block_line_span(document, block)?;
    let render = span.start.max(visible.start)..span.end.min(visible.end).max(span.start);
    let ctx = block_context(editor, block, &span, &render, joined)?;
    let lines = block_lines(editor, &ctx);
    Some(present_block(
        block,
        document.revision(),
        &BlockWindow {
            trailing_blank_lines: ctx.trailing_blank_lines,
            span,
            lines: &lines,
            render,
            joined,
            block_disclosure: ctx.block_disclosure,
        },
        DEFAULT_LINE_HEIGHT,
    ))
}

/// Disclosure `presented_block` would currently assign to each line of
/// `render` in this block, without presenting it.
///
/// Cheap enough to call every frame the caret, selection or IME sits inside
/// the block: it groups `render`'s lines the same way `present_block` itself
/// does (see `hane_presentation::disclosure_runs`) and reads no more source
/// than `presented_block` would for the same `render`/`joined` pair, so a
/// cache-currentness check compares like for like instead of drifting from
/// what a fresh presentation would produce for a run joined across physical
/// lines.
pub(crate) fn expected_block_disclosures(
    editor: &Editor,
    block: &IndexedBlock,
    render: &Range<usize>,
    joined: Option<&JoinedParse>,
) -> Option<Vec<(usize, Option<SourceRange>)>> {
    let document = editor.document();
    let span = block_line_span(document, block)?;
    let ctx = block_context(editor, block, &span, render, joined)?;
    let lines = block_lines(editor, &ctx);
    Some(expected_disclosures(
        block.kind,
        &BlockWindow {
            trailing_blank_lines: ctx.trailing_blank_lines,
            span,
            lines: &lines,
            render: render.clone(),
            joined,
            block_disclosure: ctx.block_disclosure,
        },
    ))
}

/// Lines read from `document` to build a [`BlockWindow`] for `block`: the
/// whole block span when it is joinable, satisfies both synchronous budgets,
/// and no cached [`JoinedParse`] already supplies whole-span context — matching
/// exactly when `presented_block` itself reads the whole span rather than just
/// `render` — or `render` alone otherwise.
struct BlockContext {
    trailing_blank_lines: usize,
    context: Range<usize>,
    ranges: Vec<SourceRange>,
    texts: Vec<String>,
    block_disclosure: Option<SourceRange>,
}

fn block_context(
    editor: &Editor,
    block: &IndexedBlock,
    span: &Range<usize>,
    render: &Range<usize>,
    joined: Option<&JoinedParse>,
) -> Option<BlockContext> {
    let document = editor.document();
    let joinable = block_is_joinable(block.kind);
    let context = if joinable && joined.is_none() && block_fits_sync_join_budget(block, span) {
        span.clone()
    } else {
        render.clone()
    };
    let ranges = context
        .clone()
        .map(|line| document.line_range(LineId(line)).ok())
        .collect::<Option<Vec<_>>>()?;
    let texts = ranges
        .iter()
        .map(|range| document.text(*range).unwrap_or_default())
        .collect::<Vec<_>>();
    // Computed from editor state against the block's whole source range, not
    // from `ranges`/`texts` above, so a joinable block whose `context` is
    // `render` alone (a cached `JoinedParse` or either sync budget exceeded;
    // see the branch above) still reports a caret/selection/IME range that
    // sits on one of its own off-screen physical lines instead of losing it
    // to a reconstruction that only ever saw the visible ones.
    let block_disclosure = range_disclosure(
        editor,
        block.source_range,
        span.end == document.line_count(),
    );
    Some(BlockContext {
        trailing_blank_lines: trailing_blank_lines(document, span),
        context,
        ranges,
        texts,
        block_disclosure,
    })
}

fn block_lines<'a>(editor: &Editor, ctx: &'a BlockContext) -> Vec<BlockLine<'a>> {
    ctx.context
        .clone()
        .zip(&ctx.ranges)
        .zip(&ctx.texts)
        .map(|((line, range), text)| BlockLine {
            line,
            range: *range,
            text,
            disclosure: disclosure_for_line(editor, line, *range),
        })
        .collect()
}

/// Source range whose Markdown markers this line discloses: the caret's own
/// position, the part of the selection that falls on the line, or the IME's
/// marked range, which wins because composing text must stay visible.
pub(crate) fn disclosure_for_line(
    editor: &Editor,
    line: usize,
    range: SourceRange,
) -> Option<SourceRange> {
    range_disclosure(editor, range, line + 1 == editor.document().line_count())
}

const DEFAULT_LINE_HEIGHT: f32 = 26.0;

/// Body text size. Every block size is this scaled by the presentation-supplied
/// [`BlockDisplay::font_scale`], so the UI never keys sizing off a Markdown kind.
pub(crate) const BODY_FONT_SIZE: f32 = 14.0;

pub(crate) fn block_font_size(block: &VisualLine) -> f32 {
    BODY_FONT_SIZE * block.display().font_scale
}

/// Resolves a presentation background role against the active theme.
fn surface_color(surface: BlockSurface, theme: Theme) -> Option<u32> {
    match surface {
        BlockSurface::Default => None,
        BlockSurface::Code => Some(theme.code_block_background),
        BlockSurface::Table => Some(theme.table_background),
        BlockSurface::Media => Some(theme.media_background),
    }
}

/// Applies a whole-block render policy. Adding a Markdown construct means giving
/// it a `BlockDisplay` in `hane-presentation`; nothing here changes.
fn styled_block(element: Div, display: BlockDisplay, theme: Theme) -> Div {
    element
        .text_size(px(BODY_FONT_SIZE * display.font_scale))
        .when(display.weight == BlockWeight::Semibold, |element| {
            element.font_weight(FontWeight::SEMIBOLD)
        })
        .when(display.monospace, |element| {
            element.font_family("ui-monospace")
        })
        .when(display.tint == BlockTint::Muted, |element| {
            element.text_color(rgb(theme.quote_foreground))
        })
        .when_some(surface_color(display.surface, theme), |element, color| {
            element.bg(rgb(color))
        })
}

/// Container for one Markdown block. The block is the virtualization unit; its
/// children are rows — a whole physical line, or one fragment of a soft-wrapped
/// one — plus the space standing in for the lines clipped outside the viewport.
pub(crate) fn block_element(layout: &BlockLayout, children: impl IntoIterator<Item = Div>) -> Div {
    div()
        .flex()
        .flex_col()
        .w_full()
        .child(div().h(px(layout.leading_space)))
        .children(children)
        .child(div().h(px(layout.trailing_space)))
}

/// True when a caret at `visual` renders on this row. A soft break's boundary
/// belongs to the row that starts there, so only a row that ends where its
/// source line ends owns the position past its last character.
fn row_owns_visual(row: &LayoutLine, visual: usize) -> bool {
    row.line_visual_range.start <= visual
        && (visual < row.line_visual_range.end || row.wrap == LineWrap::Hard)
}

/// One row of a block: the text that fits on it, with the caret, selection and
/// IME underline that fall inside it.
///
/// Rows, not source lines, are what is painted. Which stretch of the line's
/// visual text this row holds, and which source bytes it stands for, are the
/// layout's answers; this only applies them.
pub(crate) fn row_element(
    editor: &Editor,
    block: &VisualBlock,
    layout: &BlockLayout,
    row_index: usize,
    theme: Theme,
    resolver: &ResourceResolver,
) -> Div {
    let row = &layout.lines[row_index];
    let line = &block.lines[row.line];
    let display = line.display();
    if let Some(image) = &line.image {
        let resolved = resolver.resolve(&image.destination);
        return styled_block(
            div()
                .h(px(row.height))
                .w_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .px(px(theme.line_horizontal_padding)),
            display,
            theme,
        )
        .child(
            img(resolved)
                .max_w(px(layout.width.min(640.0)))
                .h(px((row.height - 32.0).max(1.0)))
                .object_fit(ObjectFit::Contain),
        )
        .child(
            div()
                .text_size(px(12.0))
                .text_color(rgb(theme.quote_foreground))
                .child(image.alt.clone()),
        );
    }

    let cursor = editor.selection().active;
    let is_final_line = row.line_id as usize + 1 == editor.document().line_count();
    let visual_cursor = if line_owns_cursor(line.source_range, cursor, is_final_line) {
        line.source_map
            .source_to_visual(cursor, Bias::After)
            .map(|candidate| candidate.visual_offset)
            .or_else(|| (cursor == line.source_range.start).then_some(VisualOffset(0)))
            .filter(|visual| row_owns_visual(row, visual.0))
    } else {
        None
    };
    let selected_visual = layout.visual_range_on_row(block, row_index, editor.selection().range());
    let marked_visual = editor
        .ime()
        .and_then(|ime| layout.visual_range_on_row(block, row_index, ime.current_range));
    let segments = line_segments(
        row.line_visual_range.clone(),
        visual_cursor.map(|offset| offset.0),
        selected_visual,
        marked_visual,
        &line.style_runs,
    );
    let mut elements = Vec::with_capacity(segments.len() * 2 + 1);
    for segment in &segments {
        if segment.cursor_before {
            elements.push(cursor_overlay(theme).into_any_element());
        }
        if !segment.visual_range.is_empty() {
            elements.push(
                div()
                    .when(segment.selected, |element| {
                        element.bg(rgb(theme.selection_background))
                    })
                    .when(segment.marked || segment.display.underline, |element| {
                        element.underline()
                    })
                    .when(segment.display.bold, |element| {
                        element.font_weight(FontWeight::BOLD)
                    })
                    .when(segment.display.italic, |element| element.italic())
                    .when(segment.display.strikethrough, |element| {
                        element.line_through()
                    })
                    .when(segment.display.monospace, |element| {
                        element.font_family("ui-monospace")
                    })
                    .when(segment_shows_code_background(segment), |element| {
                        element.text_bg(rgb(theme.code_background))
                    })
                    .when(segment.display.link_color, |element| {
                        element.text_color(rgb(theme.link_foreground))
                    })
                    .child(line.visual_text[segment.visual_range.clone()].to_owned())
                    .into_any_element(),
            );
        }
    }
    if visual_cursor == Some(VisualOffset(row.line_visual_range.end)) {
        elements.push(cursor_overlay(theme).into_any_element());
    }

    styled_block(
        div()
            .h(px(row.height))
            .w_full()
            .flex()
            .items_center()
            // The row already holds exactly what fits: any further wrapping here
            // would put text where no layout row accounts for it.
            .whitespace_nowrap()
            .px(px(theme.line_horizontal_padding)),
        display,
        theme,
    )
    .children(elements)
}

#[derive(Debug, Eq, PartialEq)]
struct LineSegment {
    visual_range: Range<usize>,
    selected: bool,
    marked: bool,
    cursor_before: bool,
    /// Inline render policy for this stretch, supplied by presentation.
    display: InlineDisplay,
}

/// Whether a segment's div should carry the code span's `text_bg`.
///
/// GPUI paints a `TextStyleRefinement::background_color` (set by `text_bg`)
/// as part of the glyph run, above the box's own `StyleRefinement::background`
/// (set by `bg`) regardless of which method was chained first or last on the
/// element — the two live in unrelated fields painted by unrelated passes. A
/// segment inside a selected code span sets both, so leaving this unguarded
/// hides the selection highlight entirely underneath the code background.
fn segment_shows_code_background(segment: &LineSegment) -> bool {
    segment.display.code_background && !segment.selected
}

/// Combined inline policy for every style run that fully covers `range`.
pub(crate) fn inline_display_for(
    range: &Range<usize>,
    style_runs: &[hane_presentation::StyleRun],
) -> InlineDisplay {
    InlineDisplay::for_styles(style_runs.iter().filter_map(|run| {
        (range.start >= run.visual_range.start.0 && range.end <= run.visual_range.end.0)
            .then_some(run.kind)
    }))
}

/// Splits one row's stretch of visual text where the caret, the selection, the
/// IME underline or an inline style begins or ends. Everything is clamped into
/// `bounds`, so a construct that spans a soft wrap contributes to both rows.
fn line_segments(
    bounds: Range<usize>,
    cursor: Option<usize>,
    selected: Option<Range<usize>>,
    marked: Option<Range<usize>>,
    style_runs: &[hane_presentation::StyleRun],
) -> Vec<LineSegment> {
    let mut boundaries = Vec::new();
    boundaries.extend(cursor);
    for range in [selected.as_ref(), marked.as_ref()].into_iter().flatten() {
        boundaries.push(range.start);
        boundaries.push(range.end);
    }
    for run in style_runs {
        boundaries.push(run.visual_range.start.0);
        boundaries.push(run.visual_range.end.0);
    }
    partition(bounds, boundaries)
        .into_iter()
        .map(|range| LineSegment {
            selected: selected
                .as_ref()
                .is_some_and(|selected| range.start >= selected.start && range.end <= selected.end),
            marked: marked
                .as_ref()
                .is_some_and(|marked| range.start >= marked.start && range.end <= marked.end),
            cursor_before: cursor == Some(range.start),
            display: inline_display_for(&range, style_runs),
            visual_range: range.clone(),
        })
        .collect()
}

fn cursor_overlay(theme: Theme) -> Div {
    div().relative().flex_none().w(px(0.)).h_full().child(
        div()
            .absolute()
            .top(px(3.))
            .left(px(0.))
            .w(px(1.))
            .bottom(px(3.))
            .bg(rgb(theme.foreground)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use hane_markdown::BlockIndex;

    fn code_segment(selected: bool) -> LineSegment {
        LineSegment {
            visual_range: 0..1,
            selected,
            marked: false,
            cursor_before: false,
            display: InlineDisplay {
                code_background: true,
                ..InlineDisplay::default()
            },
        }
    }

    #[test]
    fn a_selected_code_segment_shows_the_selection_not_the_code_background() {
        // Issue #117: `text_bg` (code background) and `bg` (selection) are
        // unrelated GPUI fields painted by unrelated passes, so setting both on
        // the same div is not "last write wins" — `text_bg` always paints over
        // `bg`. Applying it here would hide the selection highlight entirely.
        assert!(!segment_shows_code_background(&code_segment(true)));
    }

    #[test]
    fn an_unselected_code_segment_keeps_its_code_background() {
        assert!(segment_shows_code_background(&code_segment(false)));
    }

    #[test]
    fn a_selection_crossing_into_a_code_span_marks_the_overlap_as_both() {
        // Proves the precondition the fix above guards against: partitioning a
        // selection that reaches into a code run does produce a segment that is
        // simultaneously `selected` and `code_background`, not two disjoint
        // segments that would have sidestepped the conflict on their own.
        let style_runs = [hane_presentation::StyleRun {
            visual_range: hane_presentation::VisualRange::new(3, 9),
            kind: hane_presentation::StyleKind::InlineCode,
        }];
        let segments = line_segments(0..9, None, Some(0..6), None, &style_runs);
        let overlap = segments
            .iter()
            .find(|segment| segment.visual_range == (3..6))
            .expect("the selected half of the code run is its own segment");
        assert!(overlap.selected);
        assert!(overlap.display.code_background);
        assert!(!segment_shows_code_background(overlap));
    }

    /// Presents a document the way the renderer does: index first, then one
    /// `present_block` call per block.
    fn presented_lines(editor: &Editor) -> Vec<VisualLine> {
        let index = BlockIndex::from_buffer(editor.document());
        index
            .blocks()
            .flat_map(|block| {
                presented_block(editor, &block, &(0..usize::MAX), None)
                    .expect("block presents")
                    .lines
            })
            .collect()
    }

    #[test]
    fn shared_line_boundary_belongs_only_to_the_following_line() {
        let first = SourceRange::new(0, 4);
        let second = SourceRange::new(4, 8);
        let cursor = SourceOffset(4);
        assert!(!line_owns_cursor(first, cursor, false));
        assert!(line_owns_cursor(second, cursor, true));
    }

    #[test]
    fn document_end_belongs_to_the_final_line() {
        assert!(line_owns_cursor(
            SourceRange::new(4, 8),
            SourceOffset(8),
            true
        ));
    }

    #[test]
    fn trailing_empty_line_exclusively_owns_document_end() {
        let content_line = SourceRange::new(0, 4);
        let trailing_empty_line = SourceRange::new(4, 4);
        let cursor = SourceOffset(4);
        assert!(!line_owns_cursor(content_line, cursor, false));
        assert!(line_owns_cursor(trailing_empty_line, cursor, true));
    }

    fn row(range: Range<usize>, wrap: LineWrap) -> LayoutLine {
        LayoutLine {
            line: 0,
            line_id: 0,
            fragment: 0,
            wrap,
            visual_range: hane_presentation::VisualRange::new(range.start, range.end),
            line_visual_range: range,
            source_range: SourceRange::new(0, 0),
            y: 0.0,
            height: 26.0,
        }
    }

    #[test]
    fn only_a_break_the_source_makes_owns_the_caret_past_the_last_character() {
        let soft = row(0..6, LineWrap::Soft);
        let hard = row(6..12, LineWrap::Hard);
        assert!(row_owns_visual(&soft, 0));
        assert!(row_owns_visual(&soft, 5));
        assert!(
            !row_owns_visual(&soft, 6),
            "a wrap point belongs to the row that starts there, not to the one it ends"
        );
        assert!(row_owns_visual(&hard, 6));
        assert!(
            row_owns_visual(&hard, 12),
            "the position after the last character of a source line is on that line"
        );
    }

    #[test]
    fn a_row_paints_only_its_own_stretch_of_the_line() {
        // Selection and IME ranges that reach past the row are clipped to it, so
        // a construct spanning a soft wrap is painted on both rows and neither
        // row draws outside its own text.
        let segments = line_segments(6..12, Some(3), Some(0..9), None, &[]);
        assert_eq!(
            segments.first().map(|segment| segment.visual_range.start),
            Some(6)
        );
        assert_eq!(
            segments.last().map(|segment| segment.visual_range.end),
            Some(12)
        );
        assert!(
            segments.iter().all(|segment| !segment.cursor_before),
            "a caret before the row is not drawn on it"
        );
        assert!(segments.iter().any(|segment| segment.selected));
    }

    #[test]
    fn selection_and_ime_boundaries_split_only_the_affected_text() {
        assert_eq!(
            line_segments(0..12, Some(3), Some(3..9), Some(6..12), &[]),
            vec![
                LineSegment {
                    visual_range: 0..3,
                    selected: false,
                    marked: false,
                    cursor_before: false,
                    display: InlineDisplay::default()
                },
                LineSegment {
                    visual_range: 3..6,
                    selected: true,
                    marked: false,
                    cursor_before: true,
                    display: InlineDisplay::default()
                },
                LineSegment {
                    visual_range: 6..9,
                    selected: true,
                    marked: true,
                    cursor_before: false,
                    display: InlineDisplay::default()
                },
                LineSegment {
                    visual_range: 9..12,
                    selected: false,
                    marked: true,
                    cursor_before: false,
                    display: InlineDisplay::default()
                },
            ]
        );
    }

    #[test]
    fn a_fenced_block_presents_every_line_as_literal_code() {
        let editor = Editor::new("```rust\nlet x = **1**;\n```\n\nafter\n");
        let lines = presented_lines(&editor);
        for (line, visual) in lines.iter().enumerate().take(3) {
            assert_eq!(
                visual.display().surface,
                BlockSurface::Code,
                "line {line} is inside the fence"
            );
        }
        // The literal `**1**` keeps its asterisks: nothing inside a fence is
        // read as inline markup.
        assert_eq!(lines[1].visual_text, "let x = **1**;");
        // The blank line tiling folded into the code block is not code.
        assert_eq!(lines[3].display().surface, BlockSurface::Default);
        assert_eq!(lines[4].visual_text, "after");
    }

    #[test]
    fn phase3_line_discloses_only_the_active_construct() {
        let editor = Editor::new("# **bold** and _italic_");
        let block = &presented_lines(&editor)[0];
        assert_eq!(block.visual_text, "# bold and italic");
        assert_eq!(block.disclosure, Some(SourceRange::empty(0)));
        assert_eq!(block.display().weight, BlockWeight::Semibold);
        assert_eq!(block_font_size(block), 24.0);
        // Asserted through the render policy, not the Markdown style kind: the UI
        // only ever sees `InlineDisplay`.
        let bold = block.visual_text.find("bold").unwrap();
        assert!(inline_display_for(&(bold..bold + "bold".len()), &block.style_runs).bold);
        let italic = block.visual_text.find("italic").unwrap();
        assert!(inline_display_for(&(italic..italic + "italic".len()), &block.style_runs).italic);
    }

    #[test]
    fn cjk_emphasis_is_italic_through_the_same_render_policy_as_ascii() {
        // The render policy the paint layer reads (`InlineDisplay`) must not
        // depend on the emphasized text being ASCII: `*漢字*` marks the same
        // bytes italic as `*bold*` marks bold, through the identical
        // `StyleKind::Italic` -> `InlineDisplay` path.
        //
        // The source deliberately does not start with a marker byte: `Editor::new`
        // always places the caret at offset 0, and the disclosure policy keeps a
        // marker visible when the caret touches it, so a paragraph that opened with
        // `*` would leave that one marker undisclosed regardless of script.
        let editor = Editor::new("emphasis: *漢字ひらがな* and **太字**");
        let block = &presented_lines(&editor)[0];
        assert_eq!(block.visual_text, "emphasis: 漢字ひらがな and 太字");

        let italic = block.visual_text.find("漢字ひらがな").unwrap();
        let italic_range = italic..italic + "漢字ひらがな".len();
        assert!(inline_display_for(&italic_range, &block.style_runs).italic);

        let bold = block.visual_text.find("太字").unwrap();
        let bold_range = bold..bold + "太字".len();
        assert!(inline_display_for(&bold_range, &block.style_runs).bold);
    }

    #[test]
    fn a_marker_pair_far_apart_resolves_regardless_of_the_visible_window() {
        // A blank-line-free paragraph long enough that a fixed-radius join
        // window around the scrolled viewport would miss one side of this
        // Strong construct entirely: the closing marker is written more than
        // 200 lines below the opening one, and neither is inside `visible`.
        let mut source = String::from("**bold\n");
        for line in 0..300 {
            source.push_str(&format!("filler line {line}\n"));
        }
        source.push_str("end**\n");
        let editor = Editor::new(&source);
        let index = BlockIndex::from_buffer(editor.document());
        let block = index.blocks().next().expect("one paragraph block");
        let visible = 150..155;
        let visual = presented_block(&editor, &block, &visible, None).expect("block presents");
        let rendered = visual
            .lines
            .iter()
            .find(|line| line.line_id as usize == 152)
            .expect("line inside the visible window is presented");
        assert!(
            rendered
                .style_runs
                .iter()
                .any(|run| run.kind == hane_presentation::StyleKind::Bold),
            "the Strong construct must resolve even though both its markers sit \
             outside a fixed-radius window around the scrolled viewport"
        );
    }

    #[test]
    fn a_byte_large_low_line_count_paragraph_does_not_sync_parse_the_whole_block() {
        // The line-count budget alone would classify this three-line paragraph
        // as small, but its source exceeds the byte budget. With no cached
        // JoinedParse, presenting only the middle line must therefore stay on
        // the bounded render-window path instead of copying and parsing the
        // complete block synchronously. The opening/closing Strong markers are
        // deliberately outside the visible line, so resolving Bold here would
        // prove that the whole block was still read.
        let half = "x".repeat(JOIN_SYNC_BYTE_BUDGET / 2 + 32);
        let source = format!("**{half}\n{half}\nend**\n");
        let editor = Editor::new(&source);
        let index = BlockIndex::from_buffer(editor.document());
        let block = index.blocks().next().expect("one paragraph block");
        let span = block_line_span(editor.document(), &block).expect("block spans lines");
        assert!(span.len() <= JOIN_SYNC_LINE_BUDGET);
        assert!(!block_fits_sync_join_budget(&block, &span));

        let visible = 1..2;
        let visual = presented_block(&editor, &block, &visible, None).expect("block presents");
        assert_eq!(visual.lines.len(), 1);
        assert_eq!(visual.lines[0].line_id, 1);
        assert!(
            visual.lines[0]
                .style_runs
                .iter()
                .all(|run| run.kind != hane_presentation::StyleKind::Bold),
            "a byte-large block must not be whole-span parsed synchronously just because it has few lines"
        );
    }

    /// A paragraph of 100,000 lines with no blank line in it, so the whole
    /// document is one block (see `VisualBlock`'s doc in `hane_presentation`).
    fn huge_paragraph() -> String {
        let mut source = String::from("**bold\n");
        for line in 0..100_000 {
            source.push_str(&format!("filler line {line}\n"));
        }
        source.push_str("end**\n");
        source
    }

    #[test]
    fn a_huge_joinable_paragraph_presents_only_its_visible_window_without_a_cached_parse() {
        // Without a cached whole-span `JoinedParse`, `presented_block` must not
        // read, join and reparse this whole 100,000-line paragraph on this
        // call: the sync budgets bound it to `visible`'s own lines instead.
        // The Strong construct's markers sit at the very start and very end of
        // the block, nowhere near `visible`, so this also proves the bound is
        // actually in effect: were the whole span still read and parsed here,
        // the construct would resolve regardless of the window, exactly as it
        // does in the smaller `a_marker_pair_far_apart_resolves_...` case above.
        let source = huge_paragraph();
        let editor = Editor::new(&source);
        let index = BlockIndex::from_buffer(editor.document());
        let block = index.blocks().next().expect("one paragraph block");
        let visible = 50_000..50_005;
        let visual = presented_block(&editor, &block, &visible, None).expect("block presents");
        assert!(
            visual
                .lines
                .iter()
                .all(|line| visible.contains(&(line.line_id as usize))),
            "only the visible window's lines are presented, not the whole block"
        );
        assert!(
            visual
                .lines
                .iter()
                .flat_map(|line| &line.style_runs)
                .all(|run| run.kind != hane_presentation::StyleKind::Bold),
            "a marker pair far outside the visible window does not resolve when \
             the block is too large to read and reparse synchronously and no \
             cached whole-span parse was supplied"
        );
    }

    #[test]
    fn a_cached_whole_span_parse_resolves_a_huge_paragraphs_markers_regardless_of_scroll_position()
    {
        // The counterpart to the test above: once a `JoinedParse` covering the
        // whole block is available (as `EditorView::schedule_joined_parse`
        // computes off the render path), the same far-apart Strong construct
        // resolves correctly for a window deep inside the paragraph, with no
        // dependency on where that window happens to sit — resolving the
        // review concern that a fixed context window's result would change
        // with scroll position.
        let source = huge_paragraph();
        let editor = Editor::new(&source);
        let index = BlockIndex::from_buffer(editor.document());
        let block = index.blocks().next().expect("one paragraph block");
        let document = editor.document();
        let span = block_line_span(document, &block).expect("block spans lines");
        let content_end = span.end - hane_presentation::trailing_blank_lines(document, &span);
        let joined = hane_presentation::parse_joined_span(
            document,
            span.start..content_end,
            document.revision(),
        )
        .expect("the whole span parses");
        let visible = 50_000..50_005;
        let visual =
            presented_block(&editor, &block, &visible, Some(&joined)).expect("block presents");
        assert!(
            visual
                .lines
                .iter()
                .flat_map(|line| &line.style_runs)
                .any(|run| run.kind == hane_presentation::StyleKind::Bold),
            "a cached whole-span parse resolves the Strong construct even for a \
             window far from either of its markers"
        );
    }
}
