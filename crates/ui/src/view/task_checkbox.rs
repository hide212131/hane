//! Issue #15: pressing an inactive task checkbox toggles it as one ordinary,
//! recorded edit.
//!
//! The press is resolved with the same `BlockLayout` row geometry that draws
//! the checkbox, then re-verified against the *current* document before
//! anything is written: the presentation snapshot only says where the control
//! was, never what may be edited. The edit itself goes through
//! `replace_range_recorded` and `after_input` like every other structural
//! edit, so Undo/Redo, dirty state, reparse and saving need nothing special.

use super::*;
use hane_markdown::plan_task_toggle;

/// A checkbox pressed in one rendered frame, tied to the session and document
/// revision that frame was drawn from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TaskCheckboxTarget {
    pub(super) session: SessionId,
    pub(super) revision: Revision,
    /// The real `[ ]` / `[x]` / `[X]` source range of the pressed checkbox.
    pub(super) marker_range: SourceRange,
    /// The state the pressed checkbox was drawn with.
    pub(super) checked: bool,
}

impl EditorView {
    /// The inactive checkbox under a window-space press on one row, if any.
    ///
    /// Only the last frame's row, at the revision it was drawn for, can
    /// answer; a frame that no longer matches the document finds nothing.
    pub(super) fn task_checkbox_at_window_point(
        &self,
        line: usize,
        fragment: Range<usize>,
        window_x: f32,
        shaper: &dyn LineShaper,
    ) -> Option<TaskCheckboxTarget> {
        let (block_id, visual_line) = *self.line_owners.get(&line)?;
        let visual = self.block_cache.get(&block_id)?;
        let layout = &self.layout_cache.get(&block_id)?.layout;
        let revision = self.editor().document().revision();
        if visual.revision != revision || layout.revision != revision {
            return None;
        }
        let row_index = layout
            .lines
            .iter()
            .position(|row| row.line == visual_line && row.line_visual_range == fragment)?;
        let x = window_x - self.main_column_left - self.theme.line_horizontal_padding;
        let task = layout.task_checkbox_at_x(visual, row_index, x, shaper)?;
        Some(TaskCheckboxTarget {
            session: self.sessions.active_id(),
            revision,
            marker_range: task.marker_range,
            checked: task.checked,
        })
    }

    /// Handles an unmodified left press on a row. Returns true when the press
    /// was a checkbox toggle, in which case the caller must stop: the press
    /// neither moves the caret nor starts a selection drag, so the matching
    /// release and any redraw cannot toggle the same box again.
    pub(super) fn press_task_checkbox(
        &mut self,
        line: usize,
        fragment: Range<usize>,
        event: &MouseDownEvent,
        shaper: &dyn LineShaper,
        cx: &mut Context<Self>,
    ) -> bool {
        if event.button != MouseButton::Left
            || !is_unmodified_click(&event.modifiers)
            || !self.body_editable()
            || self.editor().ime().is_some()
        {
            return false;
        }
        let Some(target) = self.task_checkbox_at_window_point(
            line,
            fragment,
            f32::from(event.position.x),
            shaper,
        ) else {
            return false;
        };
        self.toggle_task_checkbox(target, cx)
    }

    /// Toggles `target` as one Undo unit. Returns whether the document was
    /// edited; every disagreement with the current document is a no-op.
    pub(super) fn toggle_task_checkbox(
        &mut self,
        target: TaskCheckboxTarget,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.body_editable() || self.editor().ime().is_some() {
            return false;
        }
        if self.sessions.active_id() != target.session
            || self.editor().document().revision() != target.revision
        {
            return false;
        }
        let selection = self.editor().selection();
        let plan = {
            let document = self.editor().document();
            let projection = match self.current_index() {
                Some(index) => index.list_edit_projection_at(document, target.marker_range.start),
                None => BlockIndex::from_buffer(document)
                    .list_edit_projection_at(document, target.marker_range.start),
            };
            let Some(projection) = projection else {
                return false;
            };
            plan_task_toggle(
                document,
                &projection,
                target.marker_range,
                target.checked,
                SourceSelection {
                    anchor: selection.anchor,
                    active: selection.active,
                },
            )
        };
        let Ok(Some(MarkdownEditPlan::Replace {
            range,
            replacement,
            selection_after,
        })) = plan
        else {
            return false;
        };
        let selection_after = Selection {
            anchor: selection_after.anchor,
            active: selection_after.active,
        };
        // A press on a box far from the caret must not drag the viewport back
        // to the caret the way typing does.
        let scroll_y = self.scroll_y;
        let caret_check_pending = self.pending_caret_visibility_after_layout;
        match self
            .editor_mut()
            .replace_range_recorded(range, &replacement, selection_after)
        {
            Ok(_) => self.status = None,
            Err(error) => self.report_error("task toggle", error),
        }
        self.after_input(cx);
        self.scroll_y = scroll_y;
        self.pending_caret_visibility_after_layout = caret_check_pending;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hane_presentation::testing::FixedAdvanceShaper;

    type TestView = gpui::Entity<EditorView>;

    fn new_view(cx: &mut gpui::TestAppContext, source: &str) -> TestView {
        let source = source.to_owned();
        gpui::AppContext::new(cx, move |cx| EditorView::new(&source, "Untitled", cx))
    }

    fn target(view: &EditorView, marker: (usize, usize), checked: bool) -> TaskCheckboxTarget {
        TaskCheckboxTarget {
            session: view.sessions.active_id(),
            revision: view.editor().document().revision(),
            marker_range: SourceRange::new(marker.0, marker.1),
            checked,
        }
    }

    fn text(view: &EditorView) -> String {
        view.editor().document().full_text()
    }

    #[gpui::test]
    fn toggle_changes_only_the_middle_byte_and_keeps_the_selection(cx: &mut gpui::TestAppContext) {
        let source = "- [ ] A\r\n- [x] B\r\n  - [ ] C\n";
        let view = new_view(cx, source);
        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection {
                    anchor: SourceOffset(10),
                    active: SourceOffset(12),
                })
                .unwrap();
            let selection = view.editor().selection();

            let first = target(view, (2, 5), false);
            assert!(view.toggle_task_checkbox(first, cx));
            assert_eq!(text(view), "- [x] A\r\n- [x] B\r\n  - [ ] C\n");
            assert_eq!(view.editor().selection(), selection);

            // The sibling and the nested child are toggled independently.
            let second = target(view, (11, 14), true);
            assert!(view.toggle_task_checkbox(second, cx));
            assert_eq!(text(view), "- [x] A\r\n- [ ] B\r\n  - [ ] C\n");
            let nested = target(view, (22, 25), false);
            assert!(view.toggle_task_checkbox(nested, cx));
            assert_eq!(text(view), "- [x] A\r\n- [ ] B\r\n  - [x] C\n");
            assert_eq!(view.editor().selection(), selection);
        });
    }

    #[gpui::test]
    fn each_toggle_is_its_own_undo_unit_and_uppercase_returns(cx: &mut gpui::TestAppContext) {
        let source = "- [X] big\n";
        let view = new_view(cx, source);
        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset(9)))
                .unwrap();
            view.insert_text("!", cx);
            assert_eq!(text(view), "- [X] big!\n");

            let marker = target(view, (2, 5), true);
            assert!(view.toggle_task_checkbox(marker, cx));
            assert_eq!(text(view), "- [ ] big!\n");
            view.insert_text("?", cx);
            assert_eq!(text(view), "- [ ] big!?\n");

            // Undo peels typing, then the toggle, then the earlier typing:
            // the toggle never merges with its neighbours.
            view.dispatch(EditorCommand::Undo, cx);
            assert_eq!(text(view), "- [ ] big!\n");
            view.dispatch(EditorCommand::Undo, cx);
            assert_eq!(text(view), "- [X] big!\n", "the original capital is restored");
            view.dispatch(EditorCommand::Redo, cx);
            assert_eq!(text(view), "- [ ] big!\n");
            view.dispatch(EditorCommand::Redo, cx);
            assert_eq!(text(view), "- [ ] big!?\n");
        });
    }

    #[gpui::test]
    fn stale_wrong_or_forged_targets_change_nothing(cx: &mut gpui::TestAppContext) {
        let source = "- [ ] A\n- B [ ] c\n[ ] plain\n- [x] D\n";
        let view = new_view(cx, source);
        view.update(cx, |view, cx| {
            let revision_before = view.editor().document().revision();
            // Wrong state, shifted range, mid-text brackets and a plain
            // paragraph are all refused.
            for (marker, checked) in [
                ((2, 5), true),
                ((3, 6), false),
                ((13, 16), false),
                ((18, 21), false),
                ((2, 6), false),
            ] {
                let forged = target(view, marker, checked);
                assert!(!view.toggle_task_checkbox(forged, cx), "{marker:?}");
                assert_eq!(text(view), source);
            }
            assert_eq!(view.editor().document().revision(), revision_before);

            // A target from before an edit never applies afterwards.
            let stale = target(view, (2, 5), false);
            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset(source.len())))
                .unwrap();
            view.insert_text("x", cx);
            let edited = text(view);
            assert!(!view.toggle_task_checkbox(stale, cx));
            assert_eq!(text(view), edited);
        });
    }

    #[gpui::test]
    fn reading_mode_and_composition_never_toggle(cx: &mut gpui::TestAppContext) {
        let source = "- [ ] A\n";
        let view = new_view(cx, source);
        view.update(cx, |view, cx| {
            let marker = target(view, (2, 5), false);
            view.editor_mode = EditorMode::Reading;
            assert!(!view.toggle_task_checkbox(marker, cx));
            assert_eq!(text(view), source);
            view.editor_mode = EditorMode::Editing;

            view.editor_mut()
                .replace_and_mark_text(None, "あ", Some(1..1))
                .unwrap();
            let composing = text(view);
            let marker = target(view, (2, 5), false);
            assert!(view.editor().ime().is_some());
            assert!(!view.toggle_task_checkbox(marker, cx));
            assert_eq!(text(view), composing);
        });
    }

    #[gpui::test]
    fn no_frame_means_no_checkbox_to_press(cx: &mut gpui::TestAppContext) {
        let view = new_view(cx, "- [ ] A\n- [x] B\n");
        view.update(cx, |view, _| {
            let shaper = FixedAdvanceShaper::default();
            assert!(
                view.task_checkbox_at_window_point(0, 0..0, 0.0, &shaper)
                    .is_none()
            );
        });
    }

    /// Opens a painted window over `source` with the caret at `caret`, so the
    /// fixture decides which items are disclosed before the first frame.
    fn open_window<'a>(
        cx: &'a mut gpui::TestAppContext,
        source: &'static str,
        caret: usize,
    ) -> (TestView, &'a mut gpui::VisualTestContext) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(move |_, cx| {
            let mut view = EditorView::new(source, "Untitled", cx);
            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset(caret)))
                .unwrap();
            view
        });
        cx.simulate_resize(gpui::size(px(800.0), px(400.0)));
        cx.run_until_parked();
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(100));
        cx.run_until_parked();
        (view, cx)
    }

    /// Window x of the drawn checkbox glyph's centre and of the body text just
    /// right of it, from the real `BlockLayout` row of source `line`.
    fn drawn_checkbox_x(
        view: &TestView,
        cx: &mut gpui::VisualTestContext,
        line: usize,
    ) -> (f32, f32) {
        cx.update(|window, app| {
            let view = view.read(app);
            let shaper = WindowShaper::new(window, view.zoom);
            let (block_id, visual_line) = *view.line_owners.get(&line).expect("line is rendered");
            let visual = view.block_cache.get(&block_id).expect("block is cached");
            let layout = &view.layout_cache.get(&block_id).expect("layout is cached").layout;
            let row = layout
                .lines
                .iter()
                .find(|row| row.line == visual_line && row.fragment == 0)
                .expect("row");
            let line = &visual.lines[visual_line];
            let glyph = line
                .list
                .as_ref()
                .and_then(|list| list.marker.as_ref())
                .and_then(|marker| marker.task)
                .and_then(|task| task.checkbox_visual_range)
                .expect("inactive checkbox");
            let origin = view.main_column_left + view.theme.line_horizontal_padding;
            let left = row.text_x_origin
                + shaper.x_for_offset(line, row.line_visual_range.clone(), glyph.start.0);
            let right = row.text_x_origin
                + shaper.x_for_offset(line, row.line_visual_range.clone(), glyph.end.0);
            (origin + (left + right) / 2.0, origin + row.body_x_origin + 2.0)
        })
    }

    /// Window x of the centre of the disclosed `[ ]` source text on `line`.
    fn disclosed_marker_x(
        view: &TestView,
        cx: &mut gpui::VisualTestContext,
        line: usize,
    ) -> f32 {
        cx.update(|window, app| {
            let view = view.read(app);
            let shaper = WindowShaper::new(window, view.zoom);
            let (block_id, visual_line) = *view.line_owners.get(&line).expect("line is rendered");
            let visual = view.block_cache.get(&block_id).expect("block is cached");
            let layout = &view.layout_cache.get(&block_id).expect("layout is cached").layout;
            let row = layout
                .lines
                .iter()
                .find(|row| row.line == visual_line && row.fragment == 0)
                .expect("row");
            let line = &visual.lines[visual_line];
            assert!(
                line.list
                    .as_ref()
                    .and_then(|list| list.marker.as_ref())
                    .and_then(|marker| marker.task)
                    .and_then(|task| task.checkbox_visual_range)
                    .is_none(),
                "a disclosed item has no checkbox control"
            );
            let origin = view.main_column_left + view.theme.line_horizontal_padding;
            let left = row.text_x_origin
                + shaper.x_for_offset(line, row.line_visual_range.clone(), 2);
            let right = row.text_x_origin
                + shaper.x_for_offset(line, row.line_visual_range.clone(), 5);
            origin + (left + right) / 2.0
        })
    }

    fn click(cx: &mut gpui::VisualTestContext, x: f32, y: Pixels) {
        cx.simulate_mouse_down(point(px(x), y), MouseButton::Left, gpui::Modifiers::none());
        cx.simulate_mouse_up(point(px(x), y), MouseButton::Left, gpui::Modifiers::none());
        cx.run_until_parked();
    }

    /// The caret sits in a separate trailing paragraph, so both items are
    /// undisclosed and drawn as controls. A real press on the drawn glyph
    /// toggles once; a press on the body text beside it only moves the caret,
    /// and neither the release nor a redraw changes anything more.
    #[gpui::test]
    fn pressing_the_drawn_checkbox_toggles_once_and_body_presses_do_not(
        cx: &mut gpui::TestAppContext,
    ) {
        let source = "- [ ] A\n- [x] B\n\ntail\n";
        let (view, cx) = open_window(cx, source, source.len() - 1);

        let row = cx
            .debug_bounds("row-0-0")
            .expect("the first row is painted");
        let (glyph_x, body_x) = drawn_checkbox_x(&view, cx, 0);

        click(cx, glyph_x, row.center().y);
        view.read_with(cx, |view, _| {
            assert_eq!(text(view), "- [x] A\n- [x] B\n\ntail\n");
            assert!(!view.text_selection_drag, "the press starts no selection drag");
        });
        // A further frame draws the same state and does not toggle again.
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(text(view), "- [x] A\n- [x] B\n\ntail\n");
        });

        click(cx, body_x, row.center().y);
        view.read_with(cx, |view, _| {
            assert_eq!(
                text(view),
                "- [x] A\n- [x] B\n\ntail\n",
                "a body press never toggles"
            );
        });
    }

    /// Dragging a selection across rows, ending over the other item's glyph,
    /// selects text and toggles nothing.
    #[gpui::test]
    fn selection_drag_over_a_checkbox_does_not_toggle(cx: &mut gpui::TestAppContext) {
        let source = "- [ ] A\n- [x] B\n\ntail\n";
        let (view, cx) = open_window(cx, source, source.len() - 1);

        let first = cx.debug_bounds("row-0-0").expect("first row is painted");
        let second = cx.debug_bounds("row-1-1").expect("second row is painted");
        let (_, body_x) = drawn_checkbox_x(&view, cx, 0);
        let (glyph_x, _) = drawn_checkbox_x(&view, cx, 1);

        cx.simulate_mouse_down(
            point(px(body_x), first.center().y),
            MouseButton::Left,
            gpui::Modifiers::none(),
        );
        cx.simulate_mouse_move(
            point(px(glyph_x), second.center().y),
            MouseButton::Left,
            gpui::Modifiers::none(),
        );
        cx.simulate_mouse_up(
            point(px(glyph_x), second.center().y),
            MouseButton::Left,
            gpui::Modifiers::none(),
        );
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(text(view), source, "a drag never toggles");
        });
    }

    /// A touched item shows its original source and has no control, so a press
    /// on the `[ ]` text is an ordinary caret move for direct editing.
    #[gpui::test]
    fn pressing_disclosed_marker_text_moves_the_caret_without_toggling(
        cx: &mut gpui::TestAppContext,
    ) {
        let source = "- [ ] A\n- [x] B\n";
        let (view, cx) = open_window(cx, source, 0);

        let row = cx
            .debug_bounds("row-0-0")
            .expect("the first row is painted");
        let marker_x = disclosed_marker_x(&view, cx, 0);

        click(cx, marker_x, row.center().y);
        view.read_with(cx, |view, _| {
            assert_eq!(text(view), source, "the disclosed marker never toggles");
            let caret = view.editor().selection().active.0;
            assert!((2..=5).contains(&caret), "the caret moved into the marker");
        });
    }
}
