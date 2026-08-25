use crate::theme::Theme;
use gpui::{
    Div, FontWeight, IntoElement, ObjectFit, ParentElement, Styled, StyledImage, div, img,
    prelude::FluentBuilder, px, rgb,
};
use hane_document::{Bias, LineId, SourceOffset, SourceRange, TextBuffer};
use hane_editor::Editor;
use hane_presentation::{BlockKind, StyleKind, VisualBlock, VisualOffset, present_polished_line};
use std::ops::Range;
use std::path::Path;

fn line_owns_cursor(range: SourceRange, cursor: SourceOffset, is_final_line: bool) -> bool {
    range.start <= cursor && (cursor < range.end || (is_final_line && cursor == range.end))
}

pub(crate) fn presented_line(
    editor: &Editor,
    line: usize,
    fenced_code_context: bool,
    table_context: bool,
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
        table_context,
    );
    while block.visual_text.ends_with(['\r', '\n']) {
        block.visual_text.pop();
    }
    if fenced_code_context {
        block.kind = BlockKind::CodeBlock;
        block.estimated_height = DEFAULT_LINE_HEIGHT * 1.15;
        if !block.visual_text.is_empty() {
            block.style_runs.push(hane_presentation::StyleRun {
                visual_range: hane_presentation::VisualRange::new(0, block.visual_text.len()),
                kind: StyleKind::CodeBlock,
            });
        }
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

pub(crate) fn block_font_size(block: &VisualBlock) -> f32 {
    match block.kind {
        BlockKind::Heading(1) => 24.0,
        BlockKind::Heading(2) => 21.0,
        BlockKind::Heading(3) => 18.0,
        BlockKind::Heading(_) => 16.0,
        _ => 14.0,
    }
}

pub(crate) fn line_element_from_block(
    editor: &Editor,
    line: usize,
    block: &VisualBlock,
    theme: Theme,
    document_directory: Option<&Path>,
) -> Div {
    if let Some(image) = &block.image {
        let destination = Path::new(&image.destination);
        let resolved = if destination.is_absolute() {
            destination.to_path_buf()
        } else {
            document_directory
                .unwrap_or_else(|| Path::new("."))
                .join(destination)
        };
        return div()
            .h(px(block.height()))
            .w_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .px(px(theme.line_horizontal_padding))
            .bg(rgb(theme.media_background))
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
                    .when(segment.marked, |element| element.underline())
                    .when(segment.bold, |element| {
                        element.font_weight(FontWeight::BOLD)
                    })
                    .when(segment.italic, |element| element.italic())
                    .when(segment.strikethrough, |element| element.line_through())
                    .when(segment.code, |element| {
                        element
                            .font_family("ui-monospace")
                            .text_bg(rgb(theme.code_background))
                    })
                    .when(segment.link, |element| {
                        element.underline().text_color(rgb(theme.link_foreground))
                    })
                    .child(block.visual_text[segment.visual_range.clone()].to_owned())
                    .into_any_element(),
            );
        }
    }
    if visual_cursor == Some(VisualOffset(block.visual_text.len())) {
        elements.push(cursor_overlay(theme).into_any_element());
    }

    div()
        .h(px(block.height()))
        .w_full()
        .flex()
        .items_center()
        .px(px(theme.line_horizontal_padding))
        .text_size(px(block_font_size(block)))
        .when(matches!(block.kind, BlockKind::Heading(_)), |element| {
            element.font_weight(FontWeight::SEMIBOLD)
        })
        .when(block.kind == BlockKind::CodeBlock, |element| {
            element.bg(rgb(theme.code_block_background))
        })
        .when(block.kind == BlockKind::Quote, |element| {
            element.text_color(rgb(theme.quote_foreground))
        })
        .when(block.kind == BlockKind::TableRow, |element| {
            element
                .font_family("ui-monospace")
                .bg(rgb(theme.table_background))
        })
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
    bold: bool,
    italic: bool,
    strikethrough: bool,
    code: bool,
    link: bool,
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
            let has_style = |kind| {
                style_runs.iter().any(|run| {
                    run.kind == kind
                        && range.start >= run.visual_range.start.0
                        && range.end <= run.visual_range.end.0
                })
            };
            LineSegment {
                selected: selected.as_ref().is_some_and(|selected| {
                    range.start >= selected.start && range.end <= selected.end
                }),
                marked: marked
                    .as_ref()
                    .is_some_and(|marked| range.start >= marked.start && range.end <= marked.end),
                cursor_before: cursor == Some(range.start),
                visual_range: range.clone(),
                bold: has_style(StyleKind::Bold),
                italic: has_style(StyleKind::Italic),
                strikethrough: has_style(StyleKind::Strikethrough),
                code: has_style(StyleKind::InlineCode) || has_style(StyleKind::CodeBlock),
                link: has_style(StyleKind::Link),
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
                    bold: false,
                    italic: false,
                    strikethrough: false,
                    code: false,
                    link: false
                },
                LineSegment {
                    visual_range: 3..6,
                    selected: true,
                    marked: false,
                    cursor_before: true,
                    bold: false,
                    italic: false,
                    strikethrough: false,
                    code: false,
                    link: false
                },
                LineSegment {
                    visual_range: 6..9,
                    selected: true,
                    marked: true,
                    cursor_before: false,
                    bold: false,
                    italic: false,
                    strikethrough: false,
                    code: false,
                    link: false
                },
                LineSegment {
                    visual_range: 9..12,
                    selected: false,
                    marked: true,
                    cursor_before: false,
                    bold: false,
                    italic: false,
                    strikethrough: false,
                    code: false,
                    link: false
                },
            ]
        );
    }

    #[test]
    fn phase3_line_discloses_only_the_active_construct() {
        let editor = Editor::new("# **bold** and _italic_");
        let block = presented_line(&editor, 0, false, false).unwrap();
        assert_eq!(block.visual_text, "# bold and italic");
        assert_eq!(block.disclosure, Some(SourceRange::empty(0)));
        assert_eq!(block.kind, BlockKind::Heading(1));
        assert!(
            block
                .style_runs
                .iter()
                .any(|run| run.kind == StyleKind::Bold)
        );
        assert!(
            block
                .style_runs
                .iter()
                .any(|run| run.kind == StyleKind::Italic)
        );
    }
}
