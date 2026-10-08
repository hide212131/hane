//! Document-find entry bar: open/close and focus isolation only (Issue #413
//! stage 1 of this feature). This module deliberately does not scan the
//! document, highlight matches, count results, or navigate between them —
//! those are later stages. Opening, closing, re-invoking, and editing the
//! query must never touch the active document's text, selection, revision,
//! dirty flag, undo history, or IME composition; this bar only reads the
//! current selection once, to seed its own separate query field.
//!
//! Kept entirely separate from `content_search` (the Work-folder body search
//! living in the sidebar, opened with Cmd/Ctrl+Shift+F): different shortcut
//! (Cmd/Ctrl+F), different field, different focus flag, so the two can be
//! open at the same time without either stealing the other's state.

use super::*;
use gpui::{AppContext, Entity};
use gpui_component::input::{Input, InputEvent, InputState};

const DOCUMENT_FIND_BAR_HEIGHT: f32 = SIDEBAR_FILTER_HEIGHT;

pub(super) struct DocumentFindState {
    open: bool,
    input: Option<Entity<InputState>>,
    input_subscription: Option<Subscription>,
    /// Whether the find bar's own input currently holds keyboard focus, as
    /// opposed to the document body sharing the view's focus handle. This is
    /// the only state other actions (see `actions.rs`) check before routing
    /// document-editing keys, so it must be kept in lockstep with the
    /// input's real `Focus`/`Blur` events rather than inferred.
    input_focused: bool,
}

impl Default for DocumentFindState {
    fn default() -> Self {
        Self {
            open: false,
            input: None,
            input_subscription: None,
            input_focused: false,
        }
    }
}

impl EditorView {
    fn initialize_document_find_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.document_find.input.is_some() {
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("検索"));
        let subscription =
            cx.subscribe(&input, |view, _input, event: &InputEvent, cx| match event {
                InputEvent::Focus => {
                    view.document_find.input_focused = true;
                    view.sidebar_keyboard_focus = false;
                    cx.notify();
                }
                InputEvent::Blur => {
                    view.document_find.input_focused = false;
                    cx.notify();
                }
                InputEvent::Change | InputEvent::PressEnter { .. } => {}
            });
        self.document_find.input = Some(input);
        self.document_find.input_subscription = Some(subscription);
    }

    pub(crate) fn document_find_input_is_focused(&self) -> bool {
        self.document_find.input_focused
    }

    pub(crate) fn document_find_should_leave_on_escape(&self) -> bool {
        self.document_find.open && self.document_find.input_focused
    }

    /// A non-empty, single-line selection in the active document's body, or
    /// `None` when the selection is empty or spans more than one line. Reads
    /// the selection only; never mutates it.
    fn document_find_selection_seed(&self) -> Option<String> {
        let editor = self.sessions.active().editor();
        let range = editor.selection().range();
        if range.start == range.end {
            return None;
        }
        let text = editor.selected_text().ok()?;
        if text.is_empty() || text.contains(['\n', '\r']) {
            return None;
        }
        Some(text)
    }

    /// Opens the bar and focuses its input, seeding the query from the
    /// current selection the first time the bar opens. Invoking this again
    /// while the bar is already open selects the current query text instead
    /// of reseeding it, matching the platform convention for re-pressing
    /// Cmd/Ctrl+F.
    pub(crate) fn open_document_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.inline_rename_active() || self.sidebar_filter_is_focused() {
            return;
        }
        self.initialize_document_find_input(window, cx);
        let already_open = self.document_find.open;
        if !already_open {
            self.document_find.open = true;
            if let Some(seed) = self.document_find_selection_seed()
                && let Some(input) = self.document_find.input.as_ref()
            {
                input.update(cx, |state, cx| state.set_value(seed, window, cx));
            }
        }
        if let Some(input) = self.document_find.input.as_ref() {
            input.update(cx, |state, cx| {
                state.select_all(window, cx);
                state.focus(window, cx);
            });
        }
        // Opening the find bar hides neither the Work-folder content search
        // sidebar nor the document body, but only one field should ever
        // claim `*_input_is_focused()` at a time, or the guards in
        // `actions.rs` could let a document-editing key reach the body while
        // this bar's input visibly holds focus.
        self.blur_content_search_focus(cx);
        self.document_find.input_focused = true;
        self.sidebar_keyboard_focus = false;
        cx.notify();
    }

    /// Hides the bar and returns GPUI focus to the document body. Returns
    /// `false` without changing anything when the bar was not open, so
    /// callers (see the `CancelComposition`/Escape handler) can fall through
    /// to other Escape behavior.
    pub(crate) fn leave_document_find(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.document_find.open {
            return false;
        }
        self.document_find.open = false;
        self.document_find.input_focused = false;
        window.focus(&self.focus_handle, cx);
        cx.notify();
        true
    }

    pub(super) fn document_find_bar(&self, cx: &mut Context<Self>) -> Option<gpui::Stateful<gpui::Div>> {
        if !self.document_find.open {
            return None;
        }
        let input = self
            .document_find
            .input
            .as_ref()
            .expect("the find bar's input is initialized before it is opened");
        let search_input = Input::new(input)
            .id("document-find-input")
            .h(px(DOCUMENT_FIND_BAR_HEIGHT))
            .appearance(false)
            .bordered(false)
            .focus_bordered(false);
        let search_box = div()
            .id("document-find-query")
            .h(px(DOCUMENT_FIND_BAR_HEIGHT))
            .flex_1()
            .min_w(px(0.0))
            .px(px(4.0))
            .rounded_sm()
            .border_1()
            .border_color(rgb(self.theme.sidebar_foreground))
            .bg(rgb(self.theme.code_background))
            .child(search_input);
        let close = div()
            .id("document-find-close")
            .h(px(DOCUMENT_FIND_BAR_HEIGHT))
            .px(px(6.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded_sm()
            .cursor_pointer()
            .bg(rgb(self.theme.code_background))
            .child("閉じる")
            .on_click(cx.listener(|view, _, window, cx| {
                view.leave_document_find(window, cx);
            }));
        Some(
            // Absolutely positioned over the top of the editor viewport
            // (directly under the tab bar) instead of a flex sibling that
            // would push it down: `self.viewport_height` and the
            // mouse-to-row math in `on_editor_mouse_down`/`on_row_mouse_down`
            // are derived from the header/footer heights alone and must not
            // shift just because this bar is open (Issue #413).
            div()
                .id("document-find-bar")
                .debug_selector(|| "document-find-bar".to_owned())
                .absolute()
                .top(px(0.0))
                .left(px(0.0))
                .right(px(0.0))
                .h(px(DOCUMENT_FIND_BAR_HEIGHT + SIDEBAR_FILTER_GAP * 2.0))
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py(px(SIDEBAR_FILTER_GAP))
                .bg(rgb(self.theme.header_background))
                .text_color(rgb(self.theme.header_foreground))
                .child(search_box)
                .child(close),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn secondary_f_opens_the_bar_and_focuses_its_input_without_touching_the_document(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("one\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        // Focus the editor's "HaneEditor" key context the way a real click
        // does, so secondary-f is dispatched the same way a user would hit
        // it, not just called as a direct method.
        let point = cx
            .debug_bounds("row-0-0")
            .expect("first row painted")
            .center();
        cx.simulate_mouse_down(point, MouseButton::Left, gpui::Modifiers::none());
        cx.simulate_mouse_up(point, MouseButton::Left, gpui::Modifiers::none());
        cx.run_until_parked();

        cx.simulate_keystrokes("secondary-f");
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert!(view.document_find.open);
            assert!(view.document_find_input_is_focused());
            // Opening the bar must never touch the document it floats over.
            assert_eq!(view.editor().document().full_text(), "one\n");
        });
    }

    #[gpui::test]
    fn opening_seeds_the_query_from_a_non_empty_single_line_selection(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) =
            cx.add_window_view(|_, cx| EditorView::new("needle and thread\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection {
                    anchor: SourceOffset(0),
                    active: SourceOffset(6),
                })
                .unwrap();
            cx.notify();
        });
        cx.run_until_parked();

        cx.update(|window, app| {
            view.update(app, |view, cx| view.open_document_find(window, cx));
        });
        cx.run_until_parked();

        view.read_with(cx, |view, app| {
            let value = view
                .document_find
                .input
                .as_ref()
                .expect("find input is initialized by open_document_find")
                .read(app)
                .value()
                .to_string();
            assert_eq!(value, "needle");
            // Seeding only reads the selection; it must never change it or
            // the document's text.
            assert_eq!(
                view.editor().selection(),
                Selection {
                    anchor: SourceOffset(0),
                    active: SourceOffset(6),
                }
            );
            assert_eq!(view.editor().document().full_text(), "needle and thread\n");
        });
    }

    #[gpui::test]
    fn opening_does_not_seed_the_query_from_a_multi_line_selection(cx: &mut gpui::TestAppContext) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("one\ntwo\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        view.update(cx, |view, cx| {
            // "one\ntwo", spanning the line break.
            view.editor_mut()
                .set_selection(Selection {
                    anchor: SourceOffset(0),
                    active: SourceOffset(7),
                })
                .unwrap();
            cx.notify();
        });
        cx.run_until_parked();

        cx.update(|window, app| {
            view.update(app, |view, cx| view.open_document_find(window, cx));
        });
        cx.run_until_parked();

        view.read_with(cx, |view, app| {
            let value = view
                .document_find
                .input
                .as_ref()
                .expect("find input is initialized by open_document_find")
                .read(app)
                .value()
                .to_string();
            assert_eq!(value, "");
        });
    }

    #[gpui::test]
    fn reinvoking_while_open_selects_the_current_query_instead_of_reseeding(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) =
            cx.add_window_view(|_, cx| EditorView::new("needle and thread\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection {
                    anchor: SourceOffset(0),
                    active: SourceOffset(6),
                })
                .unwrap();
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(|window, app| {
            view.update(app, |view, cx| view.open_document_find(window, cx));
        });
        cx.run_until_parked();
        view.read_with(cx, |view, app| {
            let value = view.document_find.input.as_ref().unwrap().read(app).value().to_string();
            assert_eq!(value, "needle");
        });

        // A different single-line selection exists now, but re-invoking the
        // shortcut while the bar is already open must select the existing
        // query text rather than reseed it from this new selection.
        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection {
                    anchor: SourceOffset(11),
                    active: SourceOffset(17),
                })
                .unwrap();
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(|window, app| {
            view.update(app, |view, cx| view.open_document_find(window, cx));
        });
        cx.run_until_parked();

        view.read_with(cx, |view, app| {
            let value = view.document_find.input.as_ref().unwrap().read(app).value().to_string();
            assert_eq!(value, "needle");
        });
    }

    #[gpui::test]
    fn escape_from_the_find_input_clears_focus_so_backspace_reaches_the_editor(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("one\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        cx.update(|window, app| {
            view.update(app, |view, cx| view.open_document_find(window, cx));
        });
        cx.run_until_parked();
        assert!(view.read_with(cx, |view, _| view.document_find_input_is_focused()));

        // Escape must hide the bar, clear `input_focused` (not just drop
        // `open`), and move real GPUI keyboard focus back to the editor, or
        // Backspace keeps being swallowed by the lingering flag in
        // `actions.rs`/reaches a now-unmounted input.
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(!view.document_find.open);
            assert!(!view.document_find_input_is_focused());
        });

        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset(3)))
                .unwrap();
            cx.notify();
        });
        cx.run_until_parked();

        cx.simulate_keystrokes("backspace");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().document().full_text(), "on\n");
        });
    }

    #[gpui::test]
    fn backspace_while_the_find_input_is_focused_does_not_edit_the_document(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("one\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset(3)))
                .unwrap();
            cx.notify();
        });
        cx.run_until_parked();

        cx.update(|window, app| {
            view.update(app, |view, cx| view.open_document_find(window, cx));
        });
        cx.run_until_parked();
        assert!(view.read_with(cx, |view, _| view.document_find_input_is_focused()));

        // While the find bar's own input holds focus, Backspace (and every
        // other document-editing key guarded in `actions.rs`) must never
        // reach the document underneath it.
        cx.simulate_keystrokes("backspace");
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().document().full_text(), "one\n");
        });
    }

    #[gpui::test]
    fn opening_the_find_bar_clears_content_search_input_focus(cx: &mut gpui::TestAppContext) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("one\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        cx.update(|window, app| {
            view.update(app, |view, cx| view.open_content_search(window, cx));
        });
        cx.run_until_parked();
        assert!(view.read_with(cx, |view, _| view.content_search_input_is_focused()));

        // Opening the document-find bar is a separate shortcut/field/focus
        // flag from the Work-folder content search in the sidebar, so the
        // two can coexist, but only one should ever claim keyboard focus.
        cx.update(|window, app| {
            view.update(app, |view, cx| view.open_document_find(window, cx));
        });
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert!(!view.content_search_input_is_focused());
            assert!(view.document_find_input_is_focused());
        });
    }
}
