use crate::view::{EditorView, InlineRenameRenderState};
use gpui::{
    App, Bounds, Context, Element, ElementId, ElementInputHandler, Entity, EntityInputHandler,
    GlobalElementId, IntoElement, LayoutId, PaintQuad, Pixels, ShapedLine, Size, Style, TextRun,
    UTF16Selection, UnderlineStyle, Window, fill, point, px, relative, rgba,
};
use hane_document::SourceRange;
use std::ops::Range;

/// The visible, one-line text field used by a sidebar inline rename. Keeping
/// this as an element rather than a second view lets the editor entity remain
/// the single owner of both the rename state and the platform text-input
/// handler.
pub(crate) struct InlineRenameInput {
    pub(crate) input: Entity<EditorView>,
}

impl IntoElement for InlineRenameInput {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

pub struct InlineRenamePrepaintState {
    line: Option<ShapedLine>,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
}

pub(crate) fn shape_inline_rename_line(
    state: &InlineRenameRenderState,
    window: &mut Window,
) -> ShapedLine {
    let style = window.text_style();
    let run = TextRun {
        len: state.text.len(),
        font: style.font(),
        color: style.color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let runs = if let Some(marked_range) = state.marked_range.as_ref() {
        vec![
            TextRun {
                len: marked_range.start,
                ..run.clone()
            },
            TextRun {
                len: marked_range.end.saturating_sub(marked_range.start),
                underline: Some(UnderlineStyle {
                    color: Some(run.color),
                    thickness: px(1.0),
                    wavy: false,
                }),
                ..run.clone()
            },
            TextRun {
                len: state.text.len().saturating_sub(marked_range.end),
                ..run
            },
        ]
        .into_iter()
        .filter(|run| run.len > 0)
        .collect()
    } else {
        vec![run]
    };
    let font_size = style.font_size.to_pixels(window.rem_size());
    window
        .text_system()
        .shape_line(state.text.clone().into(), font_size, &runs, None)
}

impl Element for InlineRenameInput {
    type RequestLayoutState = ();
    type PrepaintState = InlineRenamePrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.input
            .update(cx, |view, _| view.set_text_input_bounds(bounds));
        let input = self.input.read(cx);
        let Some(state) = input.text_input_render_state() else {
            return InlineRenamePrepaintState {
                line: None,
                cursor: None,
                selection: None,
            };
        };
        let line = shape_inline_rename_line(&state, window);
        let selection = if state.selected_range.is_empty() {
            None
        } else {
            Some(fill(
                Bounds::from_corners(
                    point(
                        bounds.left() + line.x_for_index(state.selected_range.start),
                        bounds.top(),
                    ),
                    point(
                        bounds.left() + line.x_for_index(state.selected_range.end),
                        bounds.bottom(),
                    ),
                ),
                rgba(0x3311ff30),
            ))
        };
        let cursor = if state.selected_range.is_empty() {
            Some(fill(
                Bounds::new(
                    point(
                        bounds.left() + line.x_for_index(state.selected_range.end),
                        bounds.top(),
                    ),
                    Size {
                        width: px(2.0),
                        height: bounds.bottom() - bounds.top(),
                    },
                ),
                gpui::blue(),
            ))
        } else {
            None
        };
        InlineRenamePrepaintState {
            line: Some(line),
            cursor,
            selection,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        if let Some(selection) = prepaint.selection.take() {
            window.paint_quad(selection);
        }
        if let Some(line) = prepaint.line.take() {
            line.paint(bounds.origin, window.line_height(), window, cx)
                .unwrap();
        }
        let input_is_focused = {
            let input = self.input.read(cx);
            input.inline_rename_active() || input.sidebar_filter_is_focused()
        };
        if focus_handle.is_focused(window)
            && input_is_focused
            && let Some(cursor) = prepaint.cursor.take()
        {
            window.paint_quad(cursor);
        }
    }
}

impl EntityInputHandler for EditorView {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        if self.inline_rename_active() {
            let (text, actual) = self.inline_rename_text_for_range(range_utf16)?;
            actual_range.replace(actual);
            return Some(text);
        }
        if self.sidebar_filter_is_focused() {
            let (text, actual) = self.sidebar_filter_text_for_range(range_utf16)?;
            actual_range.replace(actual);
            return Some(text);
        }
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
        if let Some((range, reversed)) = self.inline_rename_selection() {
            return Some(UTF16Selection { range, reversed });
        }
        if let Some((range, reversed)) = self.sidebar_filter_selection() {
            return Some(UTF16Selection { range, reversed });
        }
        Some(UTF16Selection {
            range: self
                .editor()
                .source_range_to_utf16(self.editor().selection().range())
                .ok()?,
            reversed: self.editor().selection().is_reversed(),
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        if self.inline_rename_active() {
            return self.inline_rename_marked_range();
        }
        if self.sidebar_filter_is_focused() {
            return self.sidebar_filter_marked_range();
        }
        self.editor()
            .ime()
            .and_then(|ime| self.editor().source_range_to_utf16(ime.marked_range).ok())
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if self.inline_rename_active() {
            self.commit_inline_rename_composition(cx);
        } else if self.sidebar_filter_is_focused() {
            self.commit_sidebar_filter_composition(cx);
        } else {
            self.clear_pending_list_editing();
            self.editor_mut().commit_composition();
        }
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.inline_rename_active() {
            self.replace_inline_rename_text(range_utf16, new_text, cx);
            return;
        }
        if self.sidebar_filter_is_focused() {
            self.replace_sidebar_filter_text(range_utf16, new_text, cx);
            return;
        }
        if range_utf16.is_none() && self.editor().ime().is_none() {
            self.insert_text(new_text, cx);
            return;
        }
        let indentation = self.pending_list_indentation();
        self.clear_pending_list_editing();
        let mut replacement = indentation;
        replacement.push_str(new_text);
        let result = self
            .editor_mut()
            .commit_text(range_utf16, &replacement)
            .map(|_| ());
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
        if self.inline_rename_active() {
            self.replace_and_mark_inline_rename_text(
                range_utf16,
                new_text,
                new_selected_range_utf16,
                cx,
            );
            return;
        }
        if self.sidebar_filter_is_focused() {
            self.replace_and_mark_sidebar_filter_text(
                range_utf16,
                new_text,
                new_selected_range_utf16,
                cx,
            );
            return;
        }
        let indentation = self.pending_list_indentation();
        self.clear_pending_list_editing();
        if let Err(error) = self.editor_mut().replace_and_mark_text_with_prefix(
            range_utf16,
            &indentation,
            new_text,
            new_selected_range_utf16,
        ) {
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
        window: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        if self.inline_rename_active() {
            let state = self.inline_rename_render_state()?;
            let line = shape_inline_rename_line(&state, window);
            let caret = if state.selection_reversed {
                state.selected_range.start
            } else {
                state.selected_range.end
            };
            return Some(Bounds {
                origin: point(bounds.left() + line.x_for_index(caret), bounds.top()),
                size: Size {
                    width: px(1.0),
                    height: bounds.size.height,
                },
            });
        }
        if self.sidebar_filter_is_focused() {
            let state = self.text_input_render_state()?;
            let line = shape_inline_rename_line(&state, window);
            let caret = if state.selection_reversed {
                state.selected_range.start
            } else {
                state.selected_range.end
            };
            return Some(Bounds {
                origin: point(bounds.left() + line.x_for_index(caret), bounds.top()),
                size: Size {
                    width: px(1.0),
                    height: bounds.size.height,
                },
            });
        }
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
        point: gpui::Point<Pixels>,
        window: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        if self.inline_rename_active() {
            return self.inline_rename_character_index_for_point(point, window);
        }
        if self.sidebar_filter_is_focused() {
            return self.sidebar_filter_character_index_for_point(point, window);
        }
        self.editor()
            .source_range_to_utf16(SourceRange::empty(self.editor().selection().active.0))
            .ok()
            .map(|range| range.start)
    }
}
