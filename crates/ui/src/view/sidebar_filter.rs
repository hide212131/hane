use super::*;

impl EditorView {
    pub(crate) fn sidebar_filter_character_index_for_point(
        &self,
        position: gpui::Point<Pixels>,
        window: &mut Window,
    ) -> Option<usize> {
        let bounds = self.sidebar_filter_input_bounds?;
        let state = self.text_input_render_state()?;
        let line = shape_inline_rename_line(&state, window);
        let x = if position.x <= bounds.left() {
            px(0.0)
        } else {
            position.x - bounds.left()
        };
        let byte = line.closest_index_for_x(x).min(state.text.len());
        Some(utf16_offset_from_byte(&state.text, byte))
    }

    pub(crate) fn sidebar_filter_has_composition(&self) -> bool {
        self.sidebar_filter.has_composition()
    }

    pub(crate) fn sidebar_filter_text_for_range(
        &self,
        range_utf16: Range<usize>,
    ) -> Option<(String, Range<usize>)> {
        Some(self.sidebar_filter.text_for_utf16_range(range_utf16))
    }

    pub(crate) fn sidebar_filter_selection(&self) -> Option<(Range<usize>, bool)> {
        if !self.sidebar_filter_focused {
            return None;
        }
        Some(self.sidebar_filter.selection_utf16())
    }

    pub(crate) fn sidebar_filter_marked_range(&self) -> Option<Range<usize>> {
        self.sidebar_filter.marked_range_utf16()
    }

    pub(crate) fn replace_sidebar_filter_text(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.sidebar_filter_focused {
            return false;
        }
        self.sidebar_filter.replace(range_utf16, new_text);
        self.sidebar_filter_changed(cx);
        true
    }

    pub(crate) fn replace_and_mark_sidebar_filter_text(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.sidebar_filter_focused {
            return false;
        }
        self.sidebar_filter
            .replace_and_mark(range_utf16, new_text, new_selected_range_utf16);
        self.sidebar_filter_changed(cx);
        true
    }

    pub(crate) fn commit_sidebar_filter_composition(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_filter.commit_composition() {
            cx.notify();
        }
    }

    pub(crate) fn cancel_sidebar_filter_composition(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_filter.cancel_composition() {
            self.sidebar_filter_changed(cx);
        }
    }

    pub(crate) fn selected_sidebar_filter_text(&self) -> Option<String> {
        self.sidebar_filter.selected_text().map(str::to_owned)
    }

    pub(crate) fn move_sidebar_filter_left(&mut self, extend: bool, cx: &mut Context<Self>) {
        self.move_sidebar_filter_horizontal(false, extend, cx);
    }

    pub(crate) fn move_sidebar_filter_right(&mut self, extend: bool, cx: &mut Context<Self>) {
        self.move_sidebar_filter_horizontal(true, extend, cx);
    }

    fn move_sidebar_filter_horizontal(
        &mut self,
        right: bool,
        extend: bool,
        cx: &mut Context<Self>,
    ) {
        if !self.sidebar_filter_focused {
            return;
        }
        self.sidebar_filter.move_horizontal(right, extend);
        cx.notify();
    }

    pub(crate) fn select_sidebar_filter_left(&mut self, cx: &mut Context<Self>) {
        self.select_sidebar_filter_to(false, cx);
    }

    pub(crate) fn select_sidebar_filter_right(&mut self, cx: &mut Context<Self>) {
        self.select_sidebar_filter_to(true, cx);
    }

    fn select_sidebar_filter_to(&mut self, right: bool, cx: &mut Context<Self>) {
        if !self.sidebar_filter_focused {
            return;
        }
        self.sidebar_filter.move_horizontal(right, true);
        cx.notify();
    }

    pub(crate) fn select_all_sidebar_filter(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_filter_focused {
            self.sidebar_filter.select_all();
            cx.notify();
        }
    }

    pub(crate) fn select_sidebar_filter_home(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_filter_focused {
            self.sidebar_filter.move_home(true);
            cx.notify();
        }
    }

    pub(crate) fn select_sidebar_filter_end(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_filter_focused {
            self.sidebar_filter.move_end(true);
            cx.notify();
        }
    }

    pub(crate) fn move_sidebar_filter_home(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_filter_focused {
            self.sidebar_filter.move_home(false);
            cx.notify();
        }
    }

    pub(crate) fn move_sidebar_filter_end(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_filter_focused {
            self.sidebar_filter.move_end(false);
            cx.notify();
        }
    }

    pub(crate) fn backspace_sidebar_filter(&mut self, cx: &mut Context<Self>) {
        self.delete_sidebar_filter_with_direction(true, cx);
    }

    pub(crate) fn delete_sidebar_filter(&mut self, cx: &mut Context<Self>) {
        self.delete_sidebar_filter_with_direction(false, cx);
    }

    fn delete_sidebar_filter_with_direction(&mut self, backwards: bool, cx: &mut Context<Self>) {
        if !self.sidebar_filter_focused {
            return;
        }
        self.sidebar_filter.delete(backwards);
        self.sidebar_filter_changed(cx);
    }

    fn sidebar_filter_changed(&mut self, cx: &mut Context<Self>) {
        self.sidebar_scroll.set_offset(point(px(0.0), px(0.0)));
        cx.notify();
    }

    pub(super) fn focus_sidebar_filter(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.cancel_inline_rename(cx) {
            return;
        }
        let was_focused = self.sidebar_filter_focused;
        self.sidebar_filter_focused = true;
        self.sidebar_keyboard_focus = true;
        window.focus(&self.focus_handle, cx);
        if was_focused {
            if let Some(index) =
                self.sidebar_filter_character_index_for_point(event.position, window)
            {
                self.sidebar_filter.set_caret_utf16(index);
            }
        } else {
            self.sidebar_filter.select_all();
        }
        cx.notify();
    }

    pub(crate) fn blur_sidebar_filter(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_filter_focused {
            self.sidebar_filter_focused = false;
            self.sidebar_filter.discard_composition();
            self.sidebar_keyboard_focus = false;
            cx.notify();
        }
    }
}
