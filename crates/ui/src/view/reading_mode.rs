//! Reading mode (Issue #547): the body keeps its current Markdown rendering
//! but cannot be changed, and no caret, selection, composition or find match
//! opens Markdown markers. The mode is one window-wide value owned by
//! `EditorView`; sessions, editors and settings never see it.

use super::*;
use gpui_component::Icon;

/// Shown as the tooltip of the disabled switch, and as the status when the
/// programmatic entry is refused, while the body holds a composition.
pub(super) const MODE_SWITCH_BLOCKED_BY_COMPOSITION: &str =
    "文字の変換を確定または取り消してから切り替えてください";

/// Whether the body can be changed or only read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum EditorMode {
    #[default]
    Editing,
    Reading,
}

impl EditorMode {
    fn toggled(self) -> Self {
        match self {
            Self::Editing => Self::Reading,
            Self::Reading => Self::Editing,
        }
    }
}

/// Whether `command` can change the document text (as opposed to moving or
/// selecting). Undo and Redo count: they rewrite the body from history.
pub(super) fn command_changes_text(command: &EditorCommand<'_>) -> bool {
    matches!(
        command,
        EditorCommand::Insert(_)
            | EditorCommand::Backspace
            | EditorCommand::Delete
            | EditorCommand::DeleteWord { .. }
            | EditorCommand::Undo
            | EditorCommand::Redo
    )
}

impl EditorView {
    pub(crate) fn editor_mode(&self) -> EditorMode {
        self.editor_mode
    }

    /// Whether the document text may change now. Kept apart from
    /// `input_target`, which only says where input goes: a read-only body is
    /// still the target of movement, selection and copy.
    pub(crate) fn body_editable(&self) -> bool {
        self.editor_mode() == EditorMode::Editing
    }

    /// Whether the body's own caret (and input-mode badge) is painted.
    /// Selection colour is unaffected.
    pub(super) fn body_caret_visible(&self) -> bool {
        self.body_editable()
    }

    /// The presentation policy for the current mode, handed to every
    /// presentation, cache check and movement calculation.
    pub(super) fn disclosure_policy(&self) -> DisclosurePolicy {
        match self.editor_mode {
            EditorMode::Editing => DisclosurePolicy::Editing,
            EditorMode::Reading => DisclosurePolicy::Reading,
        }
    }

    /// Only a composition in the body blocks the switch; one in the search
    /// field, filter or rename is a different surface.
    pub(crate) fn editor_mode_switch_blocked(&self) -> bool {
        self.editor().ime().is_some()
    }

    /// Switches to the other mode. Returns whether the mode is now the
    /// requested one.
    pub(crate) fn toggle_editor_mode(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let next = self.editor_mode.toggled();
        self.set_editor_mode(next, window, cx)
    }

    pub(crate) fn set_editor_mode(
        &mut self,
        mode: EditorMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.editor_mode == mode {
            return true;
        }
        if self.editor_mode_switch_blocked() {
            // The marked text is neither committed nor dropped here; the
            // platform ends it, after which the switch is available again.
            self.status = Some(MODE_SWITCH_BLOCKED_BY_COMPOSITION.to_owned());
            cx.notify();
            return false;
        }
        // A drag or autoscroll belongs to the geometry of the old mode.
        self.cancel_text_selection_autoscroll();
        if mode == EditorMode::Reading {
            // Cancels any in-flight request, drops its result (the generation
            // moves on) and returns focus to the body.
            self.close_ai_selection(window, cx);
        }
        self.editor_mode = mode;
        self.rebuild_presentation_for_mode_change();
        cx.notify();
        true
    }

    /// Drops everything that was derived under the old mode and re-derives
    /// the height tree for the new one, without touching the document, its
    /// history, the selection or the tabs. The scroll position is carried by
    /// `install_heights` on the source offset at the top of the viewport.
    fn rebuild_presentation_for_mode_change(&mut self) {
        self.block_cache.clear();
        self.line_owners.clear();
        self.layout_cache.clear();
        self.caret_geometry = None;
        self.pending_caret_visibility_after_layout = false;
        self.cancel_center_caret();
        // Snapshots requested under the old disclosure are not comparable
        // with the new one; in-flight jobs notice the mode change themselves.
        self.last_background_height_disclosure = None;
        self.force_height_disclosure_snapshot = false;
        // Measured heights include the structural rows (table delimiter, code
        // fences) the old mode had open, and `install_heights` would keep them
        // for any block whose estimate happens to be unchanged. Forget the
        // width the measurements were taken at so none is carried over; the
        // visible blocks are measured again in the next frame.
        self.height_blocks.content_width_bits = None;
        let (granularity, _) = self.desired_layout();
        let heights = HeightIndex::new(self.item_heights());
        self.install_heights(granularity, heights);
    }

    /// The tab-row switch: an eye while editing, a pencil while reading. It sits
    /// outside the scrolling tab strip so it keeps its size, and swallows the
    /// press so neither the strip's body-click handling nor focus handling
    /// reaches it (a disabled button would otherwise still move focus and end
    /// a composition).
    pub(super) fn reading_mode_button(&self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let reading = self.editor_mode == EditorMode::Reading;
        let (icon, label) = if reading {
            (icons::ICON_EDITING_MODE, "編集モードに切り替え")
        } else {
            (icons::ICON_READING_MODE, "閲覧モードに切り替え")
        };
        let blocked = self.editor_mode_switch_blocked();
        let tooltip = if blocked {
            MODE_SWITCH_BLOCKED_BY_COMPOSITION
        } else {
            label
        };
        let view = cx.entity();
        let button = Button::new("reading-mode-toggle")
            .ghost()
            .xsmall()
            .icon(Icon::empty().path(icon))
            .tooltip(tooltip)
            .accessibility_label(label)
            .disabled(blocked)
            .on_click(move |_, window, app| {
                view.update(app, |view, cx| {
                    view.toggle_editor_mode(window, cx);
                });
            });
        div()
            .id("reading-mode-toggle-wrapper")
            .debug_selector(|| "reading-mode-toggle".to_owned())
            .flex_none()
            .px_1()
            .on_mouse_down(MouseButton::Left, |_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
            })
            .child(button)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::EntityInputHandler;

    type TestView = gpui::Entity<EditorView>;
    type TestCx = gpui::VisualTestContext;

    fn toggle(view: &TestView, cx: &mut TestCx) -> bool {
        cx.update(|window, app| view.update(app, |view, cx| view.toggle_editor_mode(window, cx)))
    }

    fn snapshot(view: &TestView, cx: &mut TestCx) -> (Revision, usize, bool, bool) {
        view.read_with(cx, |view, _| {
            (
                view.editor().document().revision(),
                view.editor().document().len_bytes().0,
                view.sessions.active().is_dirty(),
                view.editor().can_undo(),
            )
        })
    }

    #[gpui::test]
    fn the_window_starts_in_editing_and_the_switch_is_explicit(cx: &mut gpui::TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("hello\n", "Untitled", cx));
        assert_eq!(view.read_with(cx, |view, _| view.editor_mode()), EditorMode::Editing);
        assert!(toggle(&view, cx));
        assert_eq!(view.read_with(cx, |view, _| view.editor_mode()), EditorMode::Reading);
        // New documents and reloads do not reset it.
        view.update(cx, |view, cx| {
            view.on_document_replaced();
            cx.notify();
        });
        assert_eq!(view.read_with(cx, |view, _| view.editor_mode()), EditorMode::Reading);
        assert!(toggle(&view, cx));
        assert_eq!(view.read_with(cx, |view, _| view.editor_mode()), EditorMode::Editing);
    }

    #[gpui::test]
    fn reading_refuses_every_body_change_but_allows_movement(cx: &mut gpui::TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("hello\n", "Untitled", cx));
        view.update(cx, |view, cx| view.insert_text("a", cx));
        assert!(toggle(&view, cx));
        let before = snapshot(&view, cx);
        view.update(cx, |view, cx| {
            view.insert_text("x", cx);
            view.insert_newline(cx);
            view.dispatch(EditorCommand::Insert("y"), cx);
            view.dispatch(EditorCommand::Backspace, cx);
            view.dispatch(EditorCommand::Delete, cx);
            view.dispatch(
                EditorCommand::DeleteWord {
                    forward: true,
                    stop: hane_editor::ForwardStop::End,
                },
                cx,
            );
            view.dispatch(EditorCommand::Undo, cx);
            view.dispatch(EditorCommand::Redo, cx);
            for intent in [
                ListEditIntent::Enter,
                ListEditIntent::ShiftEnter,
                ListEditIntent::Backspace,
                ListEditIntent::Indent,
                ListEditIntent::Outdent,
            ] {
                assert!(view.apply_list_edit_intent(intent, cx), "{intent:?}");
            }
        });
        assert_eq!(snapshot(&view, cx), before);
        // Movement and selection still work.
        view.update(cx, |view, cx| {
            view.dispatch(EditorCommand::SelectAll, cx);
        });
        assert!(
            !view.read_with(cx, |view, _| view.editor().selection().range().is_empty()),
            "select all works while reading"
        );
        assert_eq!(snapshot(&view, cx), before);
        // Back in editing the same operations work again.
        assert!(toggle(&view, cx));
        view.update(cx, |view, cx| view.insert_text("z", cx));
        assert_ne!(snapshot(&view, cx).0, before.0);
    }

    #[gpui::test]
    fn late_os_text_callbacks_do_not_change_a_read_only_body(cx: &mut gpui::TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("hello\n", "Untitled", cx));
        assert!(toggle(&view, cx));
        let before = snapshot(&view, cx);
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                assert!(!view.body_input_capture_enabled());
                view.replace_text_in_range(None, "late", window, cx);
                view.replace_and_mark_text_in_range(None, "あ", Some(1..1), window, cx);
                view.unmark_text(window, cx);
                view.replace_text_in_range(None, "", window, cx);
            });
        });
        assert_eq!(snapshot(&view, cx), before);
        assert!(view.read_with(cx, |view, _| view.editor().ime().is_none()));
    }

    #[gpui::test]
    fn a_body_composition_blocks_the_switch_until_it_ends(cx: &mut gpui::TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("hello\n", "Untitled", cx));
        view.update(cx, |view, _| {
            view.editor_mut()
                .replace_and_mark_text(None, "あ", Some(1..1))
                .unwrap();
        });
        assert!(!toggle(&view, cx));
        view.read_with(cx, |view, _| {
            assert_eq!(view.editor_mode(), EditorMode::Editing);
            assert!(view.editor().ime().is_some(), "the marked text is kept");
            assert_eq!(
                view.status.as_deref(),
                Some(MODE_SWITCH_BLOCKED_BY_COMPOSITION)
            );
        });
        view.update(cx, |view, _| view.editor_mut().commit_composition());
        assert!(toggle(&view, cx));
        assert_eq!(view.read_with(cx, |view, _| view.editor_mode()), EditorMode::Reading);
    }

    #[gpui::test]
    fn switching_to_reading_closes_the_ai_popup(cx: &mut gpui::TestAppContext) {
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("hello\n", "Untitled", cx));
        view.update(cx, |view, _| {
            view.editor_mut()
                .set_selection(Selection {
                    anchor: SourceOffset(0),
                    active: SourceOffset(5),
                })
                .unwrap();
        });
        cx.update(|window, app| view.update(app, |view, cx| view.open_ai_selection(window, cx)));
        assert!(view.read_with(cx, |view, _| view.ai_selection_blocks_editor_keys()));
        assert!(toggle(&view, cx));
        assert!(!view.read_with(cx, |view, _| view.ai_selection_blocks_editor_keys()));
        assert_eq!(
            view.read_with(cx, |view, _| view.editor().selection().range()),
            SourceRange::new(0, 5),
            "the selection survives the switch"
        );
    }

    #[gpui::test]
    fn reading_closes_table_and_code_syntax_and_keeps_heights_in_step(
        cx: &mut gpui::TestAppContext,
    ) {
        let source = "| a | b |\n| - | - |\n| 1 | 2 |\n\n```rust\nlet x = 1;\n```\n";
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new(source, "Untitled", cx));
        cx.simulate_resize(gpui::size(px(800.0), px(600.0)));
        cx.run_until_parked();
        // Open the table delimiter and the opening fence in turn.
        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset(10)))
                .unwrap();
            view.after_input(cx);
        });
        cx.run_until_parked();
        let editing_total = view.read_with(cx, |view, _| view.heights.total_height());
        assert!(toggle(&view, cx));
        cx.run_until_parked();
        let reading = view.read_with(cx, |view, _| {
            (
                view.heights.total_height(),
                view.active_height_disclosure(),
                view.caret_geometry().is_none(),
            )
        });
        assert!(reading.0 < editing_total, "the open delimiter row is gone");
        assert_eq!(reading.1, None);
        assert!(reading.2, "no body caret is reported while reading");
        // Round trip: editing reopens the caret's row, and the heights are
        // rebuilt rather than carried over from reading.
        assert!(toggle(&view, cx));
        cx.run_until_parked();
        let back = view.read_with(cx, |view, _| view.heights.total_height());
        assert!(back > reading.0, "the caret's delimiter row is open again");
    }
}
