use crate::theme::Theme;
use gpui::{Div, IntoElement, ParentElement, Styled, div, prelude::FluentBuilder, px, rgb};
use hane_document::{Bias, LineId, SourceOffset, SourceRange, TextBuffer};
use hane_editor::Editor;
use hane_presentation::{VisualBlock, VisualOffset, present_plain};
use std::ops::Range;

fn line_owns_cursor(range: SourceRange, cursor: SourceOffset, is_final_line: bool) -> bool {
    range.start <= cursor && (cursor < range.end || (is_final_line && cursor == range.end))
}

pub(crate) fn presented_line(editor: &Editor, line: usize) -> Option<VisualBlock> {
    let Ok(range) = editor.document().line_range(LineId(line)) else {
        return None;
    };
    let source = editor.document().text(range).unwrap_or_default();
    let mut block = present_plain(line as u64, editor.document().revision(), range, &source);
    while block.visual_text.ends_with(['\r', '\n']) {
        block.visual_text.pop();
    }
    Some(block)
}

pub(crate) fn line_element_from_block(
    editor: &Editor,
    line: usize,
    block: &VisualBlock,
    theme: Theme,
) -> Div {
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
                    .child(block.visual_text[segment.visual_range.clone()].to_owned())
                    .into_any_element(),
            );
        }
    }
    if visual_cursor == Some(VisualOffset(block.visual_text.len())) {
        elements.push(cursor_overlay(theme).into_any_element());
    }

    div()
        .h(px(theme.line_height))
        .w_full()
        .flex()
        .items_center()
        .px(px(theme.line_horizontal_padding))
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
}

fn line_segments(
    text_len: usize,
    cursor: Option<usize>,
    selected: Option<Range<usize>>,
    marked: Option<Range<usize>>,
) -> Vec<LineSegment> {
    let mut boundaries = vec![0, text_len];
    boundaries.extend(cursor.map(|offset| offset.min(text_len)));
    for range in [selected.as_ref(), marked.as_ref()].into_iter().flatten() {
        boundaries.push(range.start.min(text_len));
        boundaries.push(range.end.min(text_len));
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
                visual_range: range,
            }
        })
        .collect()
}

fn cursor_overlay(theme: Theme) -> Div {
    div()
        .relative()
        .flex_none()
        .w(px(0.))
        .h(px(theme.line_height))
        .child(
            div()
                .absolute()
                .top(px(3.))
                .left(px(0.))
                .w(px(1.))
                .h(px(theme.line_height - 6.))
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
            line_segments(12, Some(3), Some(3..9), Some(6..12)),
            vec![
                LineSegment {
                    visual_range: 0..3,
                    selected: false,
                    marked: false,
                    cursor_before: false
                },
                LineSegment {
                    visual_range: 3..6,
                    selected: true,
                    marked: false,
                    cursor_before: true
                },
                LineSegment {
                    visual_range: 6..9,
                    selected: true,
                    marked: true,
                    cursor_before: false
                },
                LineSegment {
                    visual_range: 9..12,
                    selected: false,
                    marked: true,
                    cursor_before: false
                },
            ]
        );
    }

    #[test]
    fn phase1_line_keeps_markdown_markers_visible() {
        let editor = Editor::new("**bold**");
        let block = presented_line(&editor, 0).unwrap();
        assert_eq!(block.visual_text, "**bold**");
    }
}
