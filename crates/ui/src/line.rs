use crate::theme::Theme;
use gpui::{
    Div, FontWeight, IntoElement, ObjectFit, ParentElement, Styled, StyledImage, div, img,
    prelude::FluentBuilder, px, rgb,
};
use hane_document::{Bias, LineId, SourceOffset, SourceRange, TextBuffer};
use hane_editor::Editor;
use hane_presentation::{
    BlockDisplay, BlockSurface, BlockTint, BlockWeight, InlineDisplay, LineContext, VisualBlock,
    VisualOffset, present_polished_line,
};
use std::ops::Range;
use std::path::Path;

fn line_owns_cursor(range: SourceRange, cursor: SourceOffset, is_final_line: bool) -> bool {
    range.start <= cursor && (cursor < range.end || (is_final_line && cursor == range.end))
}

pub(crate) fn presented_line(
    editor: &Editor,
    line: usize,
    context: LineContext,
) -> Option<VisualBlock> {
    let Ok(range) = editor.document().line_range(LineId(line)) else {
        return None;
    };
    let source = editor.document().text(range).unwrap_or_default();
    let disclosure = disclosure_for_line(editor, line, range);
    let mut block = present_polished_line(
        line as u64,
        editor.document().revision(),
        range,
        &source,
        DEFAULT_LINE_HEIGHT,
        disclosure,
        context,
    );
    while block.visual_text.ends_with(['\r', '\n']) {
        block.visual_text.pop();
    }
    Some(block)
}

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

pub(crate) fn block_font_size(block: &VisualBlock) -> f32 {
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

pub(crate) fn line_element_from_block(
    editor: &Editor,
    line: usize,
    block: &VisualBlock,
    theme: Theme,
    document_directory: Option<&Path>,
) -> Div {
    let display = block.display();
    if let Some(image) = &block.image {
        let destination = Path::new(&image.destination);
        let resolved = if destination.is_absolute() {
            destination.to_path_buf()
        } else {
            document_directory
                .unwrap_or_else(|| Path::new("."))
                .join(destination)
        };
        return styled_block(
            div()
                .h(px(block.height()))
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
                .h(px((block.height() - 32.0).max(1.0)))
                .object_fit(ObjectFit::Contain),
        )
        .child(
            div()
                .text_size(px(12.0))
                .text_color(rgb(theme.quote_foreground))
                .child(image.alt.clone()),
        );
    }
    let range = block.source_range;

    let cursor = editor.selection().active;
    let visual_cursor =
        if line_owns_cursor(range, cursor, line + 1 == editor.document().line_count()) {
            block
                .source_map
                .source_to_visual(cursor, Bias::After)
                .map(|candidate| candidate.visual_offset)
                .or_else(|| (cursor == range.start).then_some(VisualOffset(0)))
        } else {
            None
        };
    let selection = editor.selection().range();
    let selected_visual = clipped_visual_range(block, selection);
    let marked_visual = editor
        .ime()
        .and_then(|ime| clipped_visual_range(block, ime.current_range));
    let segments = line_segments(
        block.visual_text.len(),
        visual_cursor.map(|offset| offset.0),
        selected_visual,
        marked_visual,
        &block.style_runs,
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
                    .child(block.visual_text[segment.visual_range.clone()].to_owned())
                    .into_any_element(),
            );
        }
    }
    if visual_cursor == Some(VisualOffset(block.visual_text.len())) {
        elements.push(cursor_overlay(theme).into_any_element());
    }

    styled_block(
        div()
            .h(px(block.height()))
            .w_full()
            .flex()
            .items_center()
            .px(px(theme.line_horizontal_padding)),
        display,
        theme,
    )
    .children(elements)
}

fn clipped_visual_range(block: &VisualBlock, source: SourceRange) -> Option<Range<usize>> {
    let clipped = SourceRange {
        start: source.start.max(block.source_range.start),
        end: source.end.min(block.source_range.end),
    };
    if clipped.is_empty() {
        return None;
    }
    let start = block
        .source_map
        .source_to_visual(clipped.start, Bias::After)?
        .visual_offset
        .0
        .min(block.visual_text.len());
    let end = block
        .source_map
        .source_to_visual(clipped.end, Bias::Before)?
        .visual_offset
        .0
        .min(block.visual_text.len());
    (start < end).then_some(start..end)
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

fn line_segments(
    text_len: usize,
    cursor: Option<usize>,
    selected: Option<Range<usize>>,
    marked: Option<Range<usize>>,
    style_runs: &[hane_presentation::StyleRun],
) -> Vec<LineSegment> {
    let mut boundaries = vec![0, text_len];
    boundaries.extend(cursor.map(|offset| offset.min(text_len)));
    for range in [selected.as_ref(), marked.as_ref()].into_iter().flatten() {
        boundaries.push(range.start.min(text_len));
        boundaries.push(range.end.min(text_len));
    }
    for run in style_runs {
        boundaries.push(run.visual_range.start.0.min(text_len));
        boundaries.push(run.visual_range.end.0.min(text_len));
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

    #[test]
    fn selection_and_ime_boundaries_split_only_the_affected_text() {
        assert_eq!(
            line_segments(12, Some(3), Some(3..9), Some(6..12), &[]),
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
    fn phase3_line_discloses_only_the_active_construct() {
        let editor = Editor::new("# **bold** and _italic_");
        let block = presented_line(&editor, 0, LineContext::Normal).unwrap();
        assert_eq!(block.visual_text, "# bold and italic");
        assert_eq!(block.disclosure, Some(SourceRange::empty(0)));
        assert_eq!(block.display().weight, BlockWeight::Semibold);
        assert_eq!(block_font_size(&block), 24.0);
        // Asserted through the render policy, not the Markdown style kind: the UI
        // only ever sees `InlineDisplay`.
        let bold = block.visual_text.find("bold").unwrap();
        assert!(inline_display_for(&(bold..bold + "bold".len()), &block.style_runs).bold);
        let italic = block.visual_text.find("italic").unwrap();
        assert!(inline_display_for(&(italic..italic + "italic".len()), &block.style_runs).italic);
    }
}
