use crate::view::EditorView;
use gpui::{Bounds, Context, EntityInputHandler, Pixels, Size, UTF16Selection, Window, point, px};
use hane_document::SourceRange;
use std::ops::Range;

impl EntityInputHandler for EditorView {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let (text, actual) = self.editor().text_for_utf16_range(range_utf16).ok()?;
        actual_range.replace(actual);
        Some(text)
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self
                .editor()
                .source_range_to_utf16(self.editor().selection().range())
                .ok()?,
            reversed: self.editor().selection().is_reversed(),
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.editor()
            .ime()
            .and_then(|ime| self.editor().source_range_to_utf16(ime.current_range).ok())
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.editor_mut().commit_composition();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = if range_utf16.is_none() && self.editor().ime().is_none() {
            self.editor_mut().insert_text(new_text).map(|_| ())
        } else {
            self.editor_mut()
                .commit_text(range_utf16, new_text)
                .map(|_| ())
        };
        if let Err(error) = result {
            self.report_error("text input", error);
        }
        self.after_input(cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Err(error) =
            self.editor_mut()
                .replace_and_mark_text(range_utf16, new_text, new_selected_range_utf16)
        {
            self.report_error("IME update", error);
        }
        self.after_input(cx);
    }

    /// Where the IME should put its candidate window: the caret rectangle the
    /// last frame drew, in window coordinates. `bounds` is the top-left of the
    /// text area, and the layout answers the rest.
    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let Some(caret) = self.caret_geometry() else {
            return Some(bounds);
        };
        Some(Bounds {
            origin: bounds.origin + point(px(caret.x), px(caret.y)),
            size: Size {
                width: px(1.0),
                height: px(caret.height),
            },
        })
    }

    fn character_index_for_point(
        &mut self,
        _: gpui::Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        self.editor()
            .source_range_to_utf16(SourceRange::empty(self.editor().selection().active.0))
            .ok()
            .map(|range| range.start)
    }
}
