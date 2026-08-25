use crate::theme::Theme;
use gpui::{Div, IntoElement, ParentElement, Styled, div, prelude::FluentBuilder, px, rgb};
use hane_document::{Bias, LineId, SourceOffset, SourceRange, TextBuffer};
use hane_editor::Editor;
use hane_presentation::{VisualOffset, line_spans, present_bold};

fn line_owns_cursor(range: SourceRange, cursor: SourceOffset, is_final_line: bool) -> bool {
    range.start <= cursor && (cursor < range.end || (is_final_line && cursor == range.end))
}

pub(crate) fn line_element(editor: &Editor, line: usize, theme: Theme) -> Div {
    let Ok(range) = editor.document().line_range(LineId(line)) else {
        return div().h(px(theme.line_height));
    };
    let source = editor.document().text(range).unwrap_or_default();
    let mut block = present_bold(line as u64, editor.document().revision(), range, &source);
    while block.visual_text.ends_with(['\r', '\n']) {
        block.visual_text.pop();
    }

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
    let (line_spans, cursor_span) = line_spans(&block, visual_cursor);
    let mut elements = Vec::with_capacity(line_spans.len() + usize::from(cursor_span.is_some()));
    for (index, span) in line_spans.iter().enumerate() {
        if cursor_span == Some(index) {
            elements.push(cursor_overlay(theme).into_any_element());
        }
        elements.push(
            div()
                .when(span.bold, |element| {
                    element.font_weight(gpui::FontWeight::BOLD)
                })
                .child(block.visual_text[span.visual_range.clone()].to_owned())
                .into_any_element(),
        );
    }
    if cursor_span == Some(line_spans.len()) {
        elements.push(cursor_overlay(theme).into_any_element());
    }

    let selected = editor.selection().range().intersects(range);
    div()
        .h(px(theme.line_height))
        .w_full()
        .flex()
        .items_center()
        .px_3()
        .when(selected, |element| {
            element.bg(rgb(theme.selection_background))
        })
        .children(elements)
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
}
