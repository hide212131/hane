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

    /// Content-space `(top, height)` of the visible row nearest `offset` while
    /// reading. A closed fence or table delimiter row has no height of its
    /// own, so the answer is the first visible row at or after it, held inside
    /// the owning block (a closing fence resolves to the block's last visible
    /// row rather than the next block).
    pub(super) fn reading_visible_row(&self, offset: SourceOffset) -> Option<(f32, f32)> {
        let line_height = self.line_height();
        let top = self.scroll_for_offset(offset);
        if self.granularity != Granularity::Blocks {
            return Some((top, line_height));
        }
        let block = self.current_index()?.block_at(offset)?;
        let block_top = self.heights.prefix_sum(block.ordinal);
        let block_height = self.heights.height(block.ordinal).unwrap_or(0.0);
        let last_row_top = block_top + (block_height - line_height).max(0.0);
        Some((top.clamp(block_top, last_row_top.max(block_top)), line_height))
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
        // The measured heights below are about to be replaced by estimates, and
        // a long wrapped line would be clamped to its estimated one-row height.
        // Remember the position as a fraction of the block at the top of the
        // viewport (kept if an earlier switch is still waiting for a frame);
        // `render` resolves it once the visible blocks are measured again.
        if self.pending_zoom_anchor.is_none() {
            self.pending_zoom_anchor = self.zoom_anchor_at(0.0);
        }
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
            .with_size(px(24.0))
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

    /// Runs foreground work, then lets the 40 ms background-parse timer fire
    /// and runs what that schedules.
    fn settle(cx: &mut TestCx) {
        cx.run_until_parked();
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(100));
        cx.run_until_parked();
    }

    fn open_view<'a>(
        cx: &'a mut gpui::TestAppContext,
        source: &str,
        width: f32,
        height: f32,
    ) -> (TestView, &'a mut TestCx) {
        cx.update(crate::actions::register_key_bindings);
        let source = source.to_owned();
        let (view, cx) = cx.add_window_view(move |_, cx| EditorView::new(&source, "Untitled", cx));
        cx.simulate_resize(gpui::size(px(width), px(height)));
        settle(cx);
        (view, cx)
    }

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
        // Every delimiter cell has three or more dashes, as the table grammar
        // requires; `| - | - |` is not a table delimiter at all.
        let source = "| a | b |\n| --- | --- |\n| 1 | 2 |\n\n```rust\nlet x = 1;\n```\n";
        let delimiter = source.find("| ---").unwrap();
        let fence = source.find("```rust").unwrap();
        let (view, cx) = open_view(cx, source, 800.0, 600.0);
        view.read_with(cx, |view, _| {
            let index = view.current_index().expect("the formal index is published");
            let block = index.block_at(SourceOffset(delimiter)).expect("table block");
            assert!(
                index
                    .table_projection(&block)
                    .and_then(|projection| projection.delimiter_range)
                    .is_some(),
                "the fixture is a table with a delimiter row"
            );
        });
        // Open the table delimiter and the opening fence in turn.
        for (label, offset) in [("table delimiter", delimiter), ("opening fence", fence)] {
            view.update(cx, |view, cx| {
                view.editor_mut()
                    .set_selection(Selection::caret(SourceOffset(offset)))
                    .unwrap();
                view.after_input(cx);
            });
            settle(cx);
            let editing_total = view.read_with(cx, |view, _| view.heights.total_height());
            assert!(toggle(&view, cx));
            settle(cx);
            let reading = view.read_with(cx, |view, _| {
                (
                    view.heights.total_height(),
                    view.active_height_disclosure(),
                    view.caret_geometry().is_none(),
                )
            });
            assert!(
                reading.0 < editing_total,
                "{label}: the open row is gone (reading {} editing {editing_total})",
                reading.0
            );
            assert_eq!(reading.1, None);
            assert!(reading.2, "no body caret is reported while reading");
            // Round trip: editing reopens the caret's row, and the heights are
            // rebuilt rather than carried over from reading.
            assert!(toggle(&view, cx));
            settle(cx);
            let back = view.read_with(cx, |view, _| view.heights.total_height());
            assert!(back > reading.0, "{label}: the caret's row is open again");
        }
    }

    const NAVIGATION_VIEWPORT: (f32, f32) = (800.0, 400.0);

    fn structural_document() -> String {
        let mut source = String::from("```rust\nlet x = 1;\n```\n\n");
        for n in 0..30 {
            source.push_str(&format!("first {n:02}\n\n"));
        }
        source.push_str("| a | b |\n| --- | --- |\n| 1 | 2 |\n\n");
        for n in 0..30 {
            source.push_str(&format!("second {n:02}\n\n"));
        }
        source
    }

    #[gpui::test]
    fn reading_navigation_onto_a_closed_fence_settles_in_finite_frames(
        cx: &mut gpui::TestAppContext,
    ) {
        let source = structural_document();
        let (view, cx) = open_view(cx, &source, NAVIGATION_VIEWPORT.0, NAVIGATION_VIEWPORT.1);
        let late = source.find("second 20").unwrap();
        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset(late)))
                .unwrap();
            view.after_input(cx);
        });
        settle(cx);
        assert!(toggle(&view, cx));
        settle(cx);
        assert!(view.read_with(cx, |view, _| view.scroll_y > 0.0));

        // DocumentStart lands on offset 0, the closed opening fence row.
        view.update(cx, |view, cx| {
            view.dispatch(EditorCommand::MoveToStart { extend: false }, cx);
        });
        settle(cx);
        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().selection().active, SourceOffset(0));
            assert!(
                !view.pending_caret_visibility_after_layout,
                "the pending request is consumed instead of waiting for a row that stays closed"
            );
            assert!(
                view.scroll_y <= view.line_height() * 2.0,
                "the fence block is on screen at the top (scroll_y {})",
                view.scroll_y
            );
            assert!(view.caret_geometry().is_none());
        });

        // Plain Down and Shift+Down walk the table rows, including the closed
        // delimiter row, without leaving a request armed.
        let table = source.find("| a | b |").unwrap();
        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset(table)))
                .unwrap();
            view.after_input(cx);
        });
        settle(cx);
        for extend in [false, true, false, true] {
            view.update(cx, |view, cx| {
                view.dispatch(EditorCommand::MoveDown { extend }, cx);
            });
            settle(cx);
            view.read_with(cx, |view, _| {
                assert!(!view.pending_caret_visibility_after_layout, "extend={extend}");
            });
        }
        assert_eq!(
            view.read_with(cx, |view, _| view.editor_mode()),
            EditorMode::Reading
        );
    }

    #[gpui::test]
    fn a_background_height_snapshot_from_before_a_mode_round_trip_is_not_applied_twice(
        cx: &mut gpui::TestAppContext,
    ) {
        let source: String = (0..60).map(|n| format!("```\ncode{n:02}\n```\n\n")).collect();
        let first = source.find("code05").unwrap();
        let last = source.find("code55").unwrap();
        let (view, cx) = open_view(cx, &source, 800.0, 400.0);
        // The caret disclosure is applied. A non-empty selection starts a
        // snapshot job whose previous disclosure is that caret.
        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection {
                    anchor: SourceOffset(first),
                    active: SourceOffset(last),
                })
                .unwrap();
            view.after_input(cx);
        });
        cx.run_until_parked();
        assert!(view.read_with(cx, |view, _| view.document_parse_job_running));
        // Editing -> reading -> editing before the job finishes: the rebuild
        // has already installed the heights of the selection.
        assert!(toggle(&view, cx));
        cx.run_until_parked();
        assert!(toggle(&view, cx));
        cx.run_until_parked();
        settle(cx);
        settle(cx);
        view.read_with(cx, |view, _| {
            let document = view.editor().document();
            let index = view.current_index().expect("formal index");
            let disclosure = view.active_height_disclosure();
            let expected = block_heights_with_disclosure(
                document,
                index,
                view.line_height(),
                disclosure,
            );
            let collapsed =
                block_heights_with_disclosure(document, index, view.line_height(), None);
            for n in 10..30 {
                let offset = source.find(&format!("code{n:02}")).unwrap();
                let ordinal = index.ordinal_at(SourceOffset(offset)).expect("fence block");
                assert!(
                    expected[ordinal] > collapsed[ordinal],
                    "fence {n} is inside the selection, so it is open"
                );
                let height = view.heights.height(ordinal).unwrap();
                assert!(
                    (height - expected[ordinal]).abs() < 0.01,
                    "off-screen fence {n} keeps its arithmetic height: {height} vs {}",
                    expected[ordinal]
                );
            }
        });
    }

    #[gpui::test]
    fn a_wrapped_long_line_keeps_its_reading_position_across_mode_switches(
        cx: &mut gpui::TestAppContext,
    ) {
        let long = "word ".repeat(600);
        let source = format!("{long}\n\nsecond paragraph\n");
        let (view, cx) = open_view(cx, &source, 400.0, 500.0);
        let (total, viewport) = view.read_with(cx, |view, _| (view.heights.total_height(), view.viewport_height));
        assert!(
            total > viewport * 3.0,
            "the one physical line wraps over several screens ({total} vs {viewport})"
        );
        view.update(cx, |view, cx| {
            view.scroll_y = viewport * 1.5;
            cx.notify();
        });
        settle(cx);
        let before = view.read_with(cx, |view, _| view.scroll_y);
        assert!(before > viewport, "scrolled into the middle of the line");
        for _ in 0..2 {
            assert!(toggle(&view, cx));
            settle(cx);
            assert!(toggle(&view, cx));
            settle(cx);
            view.read_with(cx, |view, _| {
                assert!(
                    (view.scroll_y - before).abs() <= view.line_height() * 1.5,
                    "the same text stays at the top: {} vs {before}",
                    view.scroll_y
                );
            });
        }
    }

    #[gpui::test]
    fn reading_cut_and_paste_leave_the_body_and_the_clipboard_alone(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "hello world\n", 800.0, 400.0);
        cx.update(|window, app| {
            view.update(app, |view, cx| window.focus(&view.focus_handle, cx));
        });
        cx.run_until_parked();
        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection {
                    anchor: SourceOffset(0),
                    active: SourceOffset(5),
                })
                .unwrap();
            view.after_input(cx);
        });
        cx.run_until_parked();
        assert!(toggle(&view, cx));
        let before = snapshot(&view, cx);
        let text_before = view.read_with(cx, |view, _| view.editor().document().full_text());
        cx.write_to_clipboard(gpui::ClipboardItem::new_string("sentinel".to_owned()));
        for keystroke in ["secondary-x", "secondary-v"] {
            cx.simulate_keystrokes(keystroke);
            cx.run_until_parked();
            assert_eq!(snapshot(&view, cx), before, "{keystroke}");
            assert_eq!(
                view.read_with(cx, |view, _| view.editor().document().full_text()),
                text_before,
                "{keystroke}"
            );
            assert_eq!(
                cx.read_from_clipboard().and_then(|item| item.text()),
                Some("sentinel".to_owned()),
                "{keystroke} must not touch the clipboard"
            );
        }
        // Copy still works.
        cx.simulate_keystrokes("secondary-c");
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("hello".to_owned())
        );
        assert_eq!(snapshot(&view, cx), before);
    }

    #[gpui::test]
    fn undo_and_redo_survive_a_mode_round_trip(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "hello\n", 800.0, 400.0);
        view.update(cx, |view, cx| view.insert_text("x", cx));
        let edited = view.read_with(cx, |view, _| view.editor().document().full_text());
        view.update(cx, |view, cx| view.dispatch(EditorCommand::Undo, cx));
        let undone = view.read_with(cx, |view, _| view.editor().document().full_text());
        assert_ne!(edited, undone);
        assert!(toggle(&view, cx));
        settle(cx);
        assert!(toggle(&view, cx));
        settle(cx);
        view.update(cx, |view, cx| view.dispatch(EditorCommand::Redo, cx));
        assert_eq!(
            view.read_with(cx, |view, _| view.editor().document().full_text()),
            edited,
            "the redo stack is still there after reading"
        );
        view.update(cx, |view, cx| view.dispatch(EditorCommand::Undo, cx));
        assert_eq!(
            view.read_with(cx, |view, _| view.editor().document().full_text()),
            undone
        );
    }

    #[gpui::test]
    fn the_button_switches_modes_but_not_during_a_body_composition(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = open_view(cx, "hello\n", 800.0, 400.0);
        let click = |cx: &mut TestCx| {
            let point = cx
                .debug_bounds("reading-mode-toggle")
                .expect("the switch is painted")
                .center();
            cx.simulate_click(point, gpui::Modifiers::none());
            cx.run_until_parked();
        };
        click(cx);
        assert_eq!(
            view.read_with(cx, |view, _| view.editor_mode()),
            EditorMode::Reading
        );
        click(cx);
        assert_eq!(
            view.read_with(cx, |view, _| view.editor_mode()),
            EditorMode::Editing
        );
        // A composition keeps the click from doing anything.
        view.update(cx, |view, _| {
            view.editor_mut()
                .replace_and_mark_text(None, "あ", Some(1..1))
                .unwrap();
        });
        let selection = view.read_with(cx, |view, _| view.editor().selection());
        click(cx);
        view.read_with(cx, |view, _| {
            assert_eq!(view.editor_mode(), EditorMode::Editing);
            assert!(view.editor().ime().is_some(), "the marked text is kept");
            assert_eq!(view.editor().selection(), selection);
        });
        // The OS ends the composition (commit), and a late callback with a
        // UTF-16 range does not slip into the body once it is read-only.
        view.update(cx, |view, _| view.editor_mut().commit_composition());
        assert!(toggle(&view, cx));
        let before = snapshot(&view, cx);
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.replace_text_in_range(Some(0..1), "late", window, cx);
                view.replace_and_mark_text_in_range(Some(0..1), "い", Some(0..1), window, cx);
            });
        });
        assert_eq!(snapshot(&view, cx), before);
        assert!(view.read_with(cx, |view, _| view.editor().ime().is_none()));
    }

    #[gpui::test]
    fn reading_frames_are_still_painted_and_measured(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "plain text\n", 800.0, 400.0);
        assert!(toggle(&view, cx));
        settle(cx);
        let before = snapshot(&view, cx);
        let samples = view.read_with(cx, |view, _| {
            assert!(!view.body_input_capture_enabled());
            view.metrics.keystroke_to_frame_distribution().samples
        });
        view.update(cx, |view, cx| {
            view.dispatch(EditorCommand::MoveRight { extend: false }, cx);
        });
        settle(cx);
        view.read_with(cx, |view, _| {
            assert!(
                view.metrics.keystroke_to_frame_distribution().samples > samples,
                "the painted frame is recorded although input registration is off"
            );
            assert!(!view.body_input_capture_enabled());
        });
        assert_eq!(snapshot(&view, cx), before);
    }
}
