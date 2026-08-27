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
    InlineDisplay, LayoutLine, LineWrap, VisualBlock, VisualLine, VisualOffset, block_line_span,
    present_block, trailing_blank_lines,
};
use hane_session::ResourceResolver;
use std::ops::Range;

fn line_owns_cursor(range: SourceRange, cursor: SourceOffset, is_final_line: bool) -> bool {
    range.start <= cursor && (cursor < range.end || (is_final_line && cursor == range.end))
}

/// Presents the lines of one indexed Markdown block that reach `visible`.
///
/// A block is not bounded — a document without a blank line in it is a single
/// paragraph — so only the lines inside the visible window are built; the rest
/// are counted and stand in as space. Which lines are literal code or table
/// syntax is decided in presentation, from the block kind; this crate never
/// inspects the source for fences or pipes.
pub(crate) fn presented_block(
    editor: &Editor,
    block: &IndexedBlock,
    visible: &Range<usize>,
) -> Option<VisualBlock> {
    let document = editor.document();
    let span = block_line_span(document, block)?;
    let window = span.start.max(visible.start)..span.end.min(visible.end).max(span.start);
    let ranges = window
        .clone()
        .map(|line| document.line_range(LineId(line)).ok())
        .collect::<Option<Vec<_>>>()?;
    let texts = ranges
        .iter()
        .map(|range| document.text(*range).unwrap_or_default())
        .collect::<Vec<_>>();
    let lines = window
        .zip(&ranges)
        .zip(&texts)
        .map(|((line, range), text)| BlockLine {
            line,
            range: *range,
            text,
            disclosure: disclosure_for_line(editor, line, *range),
        })
        .collect::<Vec<_>>();
    Some(present_block(
        block,
        document.revision(),
        &BlockWindow {
            trailing_blank_lines: trailing_blank_lines(document, &span),
            span,
            lines: &lines,
        },
        DEFAULT_LINE_HEIGHT,
    ))
}

/// Source range whose Markdown markers this line discloses: the caret's own
/// position, the part of the selection that falls on the line, or the IME's
/// marked range, which wins because composing text must stay visible.
pub(crate) fn disclosure_for_line(
    editor: &Editor,
    line: usize,
    range: SourceRange,
) -> Option<SourceRange> {
    let selection = editor.selection().range();
    let disclosure = if selection.is_empty() {
        line_owns_cursor(
            range,
            selection.start,
            line + 1 == editor.document().line_count(),
        )
        .then_some(selection)
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
                .max_w(px(640.))
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
                    .when(segment.display.code_background, |element| {
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
    let clamp = |offset: usize| offset.clamp(bounds.start, bounds.end);
    let mut boundaries = vec![bounds.start, bounds.end];
    boundaries.extend(cursor.map(clamp));
    for range in [selected.as_ref(), marked.as_ref()].into_iter().flatten() {
        boundaries.push(clamp(range.start));
        boundaries.push(clamp(range.end));
    }
    for run in style_runs {
        boundaries.push(clamp(run.visual_range.start.0));
        boundaries.push(clamp(run.visual_range.end.0));
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    boundaries
        .windows(2)
        .map(|pair| {
            let range = pair[0]..pair[1];
            LineSegment {
                selected: selected.as_ref().is_some_and(|selected| {
                    range.start >= selected.start && range.end <= selected.end
                }),
                marked: marked
                    .as_ref()
                    .is_some_and(|marked| range.start >= marked.start && range.end <= marked.end),
                cursor_before: cursor == Some(range.start),
                display: inline_display_for(&range, style_runs),
                visual_range: range.clone(),
            }
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

    /// Presents a document the way the renderer does: index first, then one
    /// `present_block` call per block.
    fn presented_lines(editor: &Editor) -> Vec<VisualLine> {
        let index = BlockIndex::from_buffer(editor.document());
        index
            .blocks()
            .flat_map(|block| {
                presented_block(editor, &block, &(0..usize::MAX))
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
}
