use super::*;

impl EditorView {
    pub(crate) fn sidebar_filter_is_focused(&self) -> bool {
        self.sidebar_filter_focused
            || self.content_search_input_is_focused()
            || self.content_search_results_focused()
    }

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
        self.sidebar_filter_composition.is_some()
    }

    pub(crate) fn sidebar_filter_text_for_range(
        &self,
        range_utf16: Range<usize>,
    ) -> Option<(String, Range<usize>)> {
        let range = byte_range_from_utf16(&self.sidebar_filter, &range_utf16);
        Some((
            self.sidebar_filter[range.clone()].to_owned(),
            range_to_utf16(&self.sidebar_filter, &range),
        ))
    }

    pub(crate) fn sidebar_filter_selection(&self) -> Option<(Range<usize>, bool)> {
        if !self.sidebar_filter_focused {
            return None;
        }
        Some((
            range_to_utf16(&self.sidebar_filter, &self.sidebar_filter_selected_range),
            self.sidebar_filter_selection_reversed,
        ))
    }

    pub(crate) fn sidebar_filter_marked_range(&self) -> Option<Range<usize>> {
        self.sidebar_filter_marked_range
            .as_ref()
            .map(|range| range_to_utf16(&self.sidebar_filter, range))
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
        let range = range_utf16
            .as_ref()
            .map(|range| byte_range_from_utf16(&self.sidebar_filter, range))
            .or_else(|| self.sidebar_filter_marked_range.clone())
            .unwrap_or_else(|| self.sidebar_filter_selected_range.clone());
        let replacement: String = new_text
            .chars()
            .filter(|character| *character != '\n' && *character != '\r')
            .collect();
        self.sidebar_filter
            .replace_range(range.clone(), &replacement);
        let next = range.start + replacement.len();
        self.sidebar_filter_selected_range = next..next;
        self.sidebar_filter_selection_reversed = false;
        self.sidebar_filter_marked_range = None;
        self.sidebar_filter_composition = None;
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
        if self.sidebar_filter_composition.is_none() {
            self.sidebar_filter_composition = Some(SidebarFilterComposition {
                text: self.sidebar_filter.clone(),
                selected_range: self.sidebar_filter_selected_range.clone(),
                selection_reversed: self.sidebar_filter_selection_reversed,
            });
        }
        let range = range_utf16
            .as_ref()
            .map(|range| byte_range_from_utf16(&self.sidebar_filter, range))
            .or_else(|| self.sidebar_filter_marked_range.clone())
            .unwrap_or_else(|| self.sidebar_filter_selected_range.clone());
        let replacement: String = new_text
            .chars()
            .filter(|character| *character != '\n' && *character != '\r')
            .collect();
        self.sidebar_filter
            .replace_range(range.clone(), &replacement);
        let marked_end = range.start + replacement.len();
        self.sidebar_filter_marked_range =
            (!replacement.is_empty()).then_some(range.start..marked_end);
        self.sidebar_filter_selected_range = inline_rename_selected_range(
            range.start,
            &replacement,
            new_selected_range_utf16,
            marked_end..marked_end,
        );
        self.sidebar_filter_selection_reversed = false;
        self.sidebar_filter_changed(cx);
        true
    }

    pub(crate) fn commit_sidebar_filter_composition(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_filter_composition.take().is_some() {
            self.sidebar_filter_marked_range = None;
            cx.notify();
        }
    }

    pub(crate) fn cancel_sidebar_filter_composition(&mut self, cx: &mut Context<Self>) {
        let Some(composition) = self.sidebar_filter_composition.take() else {
            return;
        };
        self.sidebar_filter = composition.text;
        self.sidebar_filter_selected_range = composition.selected_range;
        self.sidebar_filter_selection_reversed = composition.selection_reversed;
        self.sidebar_filter_marked_range = None;
        self.sidebar_filter_changed(cx);
    }

    pub(crate) fn selected_sidebar_filter_text(&self) -> Option<String> {
        (!self.sidebar_filter_selected_range.is_empty())
            .then(|| self.sidebar_filter[self.sidebar_filter_selected_range.clone()].to_owned())
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
        let cursor = if self.sidebar_filter_selection_reversed {
            self.sidebar_filter_selected_range.start
        } else {
            self.sidebar_filter_selected_range.end
        };
        let target = if right {
            next_inline_rename_boundary(&self.sidebar_filter, cursor)
        } else {
            previous_inline_rename_boundary(&self.sidebar_filter, cursor)
        };
        if extend {
            select_inline_rename_to_fields(
                &mut self.sidebar_filter_selected_range,
                &mut self.sidebar_filter_selection_reversed,
                target,
            );
        } else if self.sidebar_filter_selected_range.is_empty() {
            self.sidebar_filter_selected_range = target..target;
            self.sidebar_filter_selection_reversed = false;
        } else {
            let target = if right {
                self.sidebar_filter_selected_range.end
            } else {
                self.sidebar_filter_selected_range.start
            };
            self.sidebar_filter_selected_range = target..target;
            self.sidebar_filter_selection_reversed = false;
        }
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
        let cursor = if self.sidebar_filter_selection_reversed {
            self.sidebar_filter_selected_range.start
        } else {
            self.sidebar_filter_selected_range.end
        };
        let target = if right {
            next_inline_rename_boundary(&self.sidebar_filter, cursor)
        } else {
            previous_inline_rename_boundary(&self.sidebar_filter, cursor)
        };
        select_inline_rename_to_fields(
            &mut self.sidebar_filter_selected_range,
            &mut self.sidebar_filter_selection_reversed,
            target,
        );
        cx.notify();
    }

    pub(crate) fn select_all_sidebar_filter(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_filter_focused {
            self.sidebar_filter_selected_range = 0..self.sidebar_filter.len();
            self.sidebar_filter_selection_reversed = false;
            cx.notify();
        }
    }

    pub(crate) fn select_sidebar_filter_home(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_filter_focused {
            select_inline_rename_to_fields(
                &mut self.sidebar_filter_selected_range,
                &mut self.sidebar_filter_selection_reversed,
                0,
            );
            cx.notify();
        }
    }

    pub(crate) fn select_sidebar_filter_end(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_filter_focused {
            let target = self.sidebar_filter.len();
            select_inline_rename_to_fields(
                &mut self.sidebar_filter_selected_range,
                &mut self.sidebar_filter_selection_reversed,
                target,
            );
            cx.notify();
        }
    }

    pub(crate) fn move_sidebar_filter_home(&mut self, cx: &mut Context<Self>) {
        self.move_sidebar_filter_to(0, cx);
    }

    pub(crate) fn move_sidebar_filter_end(&mut self, cx: &mut Context<Self>) {
        let target = self.sidebar_filter.len();
        self.move_sidebar_filter_to(target, cx);
    }

    fn move_sidebar_filter_to(&mut self, target: usize, cx: &mut Context<Self>) {
        if self.sidebar_filter_focused {
            self.sidebar_filter_selected_range = target..target;
            self.sidebar_filter_selection_reversed = false;
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
        let range = if self.sidebar_filter_selected_range.is_empty() {
            let cursor = if self.sidebar_filter_selection_reversed {
                self.sidebar_filter_selected_range.start
            } else {
                self.sidebar_filter_selected_range.end
            };
            if backwards {
                previous_inline_rename_boundary(&self.sidebar_filter, cursor)..cursor
            } else {
                cursor..next_inline_rename_boundary(&self.sidebar_filter, cursor)
            }
        } else {
            self.sidebar_filter_selected_range.clone()
        };
        self.sidebar_filter.replace_range(range.clone(), "");
        self.sidebar_filter_selected_range = range.start..range.start;
        self.sidebar_filter_selection_reversed = false;
        self.sidebar_filter_marked_range = None;
        self.sidebar_filter_composition = None;
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
                let byte = byte_offset_from_utf16(&self.sidebar_filter, index);
                self.sidebar_filter_selected_range = byte..byte;
                self.sidebar_filter_selection_reversed = false;
            }
        } else {
            self.sidebar_filter_selected_range = 0..self.sidebar_filter.len();
            self.sidebar_filter_selection_reversed = false;
        }
        cx.notify();
    }

    pub(crate) fn blur_sidebar_filter(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_filter_focused {
            self.sidebar_filter_focused = false;
            self.sidebar_filter_marked_range = None;
            self.sidebar_filter_composition = None;
            self.sidebar_keyboard_focus = false;
            cx.notify();
        }
    }
}
