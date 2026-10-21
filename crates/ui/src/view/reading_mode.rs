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

/// The source row at the top of the viewport when the mode was switched, and
/// the document it was read from. Unlike an ordinal and fraction it names the
/// same text under either policy, and it is refused for any other document
/// instance or revision.
#[derive(Clone, Copy, Debug)]
pub(super) struct ModeAnchor {
    session: SessionId,
    instance: DocumentInstance,
    revision: Revision,
    /// Start of the row that was at the top.
    offset: SourceOffset,
    /// How far the top edge was below that row's top.
    inside_row: f32,
    /// Frames spent waiting for the anchor's block to be drawn.
    attempts: u8,
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
        // Remember the position as the source row at the top of the viewport,
        // read from the layouts that are still measured (kept if an earlier
        // switch for this same document is still waiting for a frame);
        // `render` resolves it against the new policy's layout.
        if !self
            .pending_mode_anchor
            .is_some_and(|anchor| self.mode_anchor_is_current(&anchor))
        {
            self.pending_mode_anchor = self.capture_mode_anchor();
        }
        self.mode_generation = self.mode_generation.wrapping_add(1);
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

    /// Whether `anchor` still names the document it was read from: the same
    /// session and instance (a tab, new document, reload or work-folder switch
    /// changes one of them) at the same revision.
    pub(super) fn mode_anchor_is_current(&self, anchor: &ModeAnchor) -> bool {
        let id = self.sessions.active_id();
        anchor.session == id
            && self.document_instance(id) == Some(anchor.instance)
            && anchor.revision == self.editor().document().revision()
    }

    /// The source row at the top of the viewport and how far into it the top
    /// edge falls, from the layout that is measured right now. Reads one block
    /// and its cached layout only; nothing is parsed or shaped.
    fn capture_mode_anchor(&self) -> Option<ModeAnchor> {
        if self.heights.is_empty() {
            return None;
        }
        let document = self.editor().document();
        let item = self.heights.block_at_y(self.scroll_y);
        let local = (self.scroll_y - self.heights.prefix_sum(item)).max(0.0);
        let (offset, inside_row) = match self.granularity {
            Granularity::Lines => (document.line_range(LineId(item)).ok()?.start, local),
            Granularity::Blocks => {
                let block = self.current_index()?.block(item)?;
                let measured = self
                    .layout_cache
                    .get(&block.id)
                    .filter(|entry| {
                        entry.is_valid(
                            self.content_width,
                            self.layout_font_revision,
                            block.revision,
                        )
                    })
                    .and_then(|entry| {
                        let layout = &entry.layout;
                        let row = layout.lines.get(layout.row_at_y(local)?)?;
                        let row_top = layout.leading_space + row.y;
                        Some((row.source_range.start, (local - row_top).clamp(0.0, row.height)))
                    });
                measured.unwrap_or((block.source_range.start, local))
            }
        };
        let session = self.sessions.active_id();
        Some(ModeAnchor {
            session,
            instance: self.document_instance(session)?,
            revision: document.revision(),
            offset,
            inside_row,
            attempts: 0,
        })
    }

    /// Puts the remembered source row back at the top of the viewport, using
    /// the rows `render` just measured under the new policy. Returns whether it
    /// set `scroll_y` (the caller then skips the generic height anchor). An
    /// anchor for another document or revision is dropped without touching the
    /// scroll position. If the anchor's block was not drawn this frame, the
    /// estimate is applied and the anchor is tried again for a few frames.
    pub(super) fn resolve_mode_anchor(
        &mut self,
        rendered: &[(usize, VisualBlock, BlockLayout)],
        window: &Window,
        cx: &mut Context<Self>,
    ) -> bool {
        /// Frames an unmeasured anchor block is waited for.
        const MAX_ATTEMPTS: u8 = 3;
        let Some(mut anchor) = self.pending_mode_anchor.take() else {
            return false;
        };
        if self.heights.is_empty() || !self.mode_anchor_is_current(&anchor) {
            return false;
        }
        let measured = match self.granularity {
            Granularity::Blocks => self
                .current_index()
                .and_then(|index| index.block_at(anchor.offset))
                .and_then(|block| {
                    let (_, _, layout) = rendered
                        .iter()
                        .find(|(ordinal, _, _)| *ordinal == block.ordinal)?;
                    let (y, height) = layout.row_bounds_for_source(anchor.offset)?;
                    Some(self.heights.prefix_sum(block.ordinal) + y + anchor.inside_row.min(height))
                }),
            Granularity::Lines => {
                let top = self.scroll_for_offset(anchor.offset);
                let item = self.heights.block_at_y(top);
                let height = self.heights.height(item).unwrap_or(0.0);
                Some(top + anchor.inside_row.min(height))
            }
        };
        match measured {
            Some(target) => self.scroll_y = target,
            None if anchor.attempts < MAX_ATTEMPTS => {
                self.scroll_y = self.scroll_for_offset(anchor.offset);
                anchor.attempts += 1;
                self.pending_mode_anchor = Some(anchor);
            }
            None => return false,
        }
        self.request_layout_followup_frame(window, cx);
        true
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

    fn requested_frames(view: &TestView, cx: &mut TestCx) -> u64 {
        view.read_with(cx, |view, _| view.layout_followup_frames_requested)
    }

    /// Delivers the frames `render` asked for, at most `limit` of them, and
    /// returns how many it took. The test platform never fires
    /// `request_animation_frame` callbacks (the vendored gpui has no
    /// `simulate_next_frame`), so a requested frame is delivered as the view
    /// notification that callback makes. Frames are only delivered while the
    /// request counter moved, so a view that stopped asking is not nudged.
    /// `handled` is the counter value from before the action under test.
    fn deliver_requested_frames(
        view: &TestView,
        cx: &mut TestCx,
        mut handled: u64,
        limit: usize,
    ) -> usize {
        let mut delivered = 0;
        loop {
            let now = requested_frames(view, cx);
            if now == handled {
                return delivered;
            }
            assert!(
                delivered < limit,
                "the layout correction asks for frames without converging ({delivered} delivered)"
            );
            handled = now;
            delivered += 1;
            view.update(cx, |_, cx| cx.notify());
            settle(cx);
        }
    }

    /// Switches the mode, then settles it, including the frames the switch's
    /// own anchor resolution asks for.
    fn toggle_and_settle(view: &TestView, cx: &mut TestCx) {
        let mark = requested_frames(view, cx);
        assert!(toggle(view, cx));
        settle(cx);
        deliver_requested_frames(view, cx, mark, 8);
    }

    /// The caret row is drawn on screen with its real coordinates.
    fn assert_caret_row_on_screen(view: &TestView, cx: &mut TestCx, label: &str) {
        view.read_with(cx, |view, _| {
            let active = view.editor().selection().active;
            let (top, height) = view
                .source_offset_row_bounds(active)
                .unwrap_or_else(|| panic!("{label}: the row is laid out"));
            let tolerance = view.line_height();
            assert!(
                top + height >= view.scroll_y - tolerance
                    && top <= view.scroll_y + view.viewport_height + tolerance,
                "{label}: row {top}+{height} is outside the viewport at {} (+{})",
                view.scroll_y,
                view.viewport_height
            );
        });
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
        let mark = requested_frames(&view, cx);
        assert!(toggle(&view, cx));
        settle(cx);
        // The anchor resolution of the switch itself asks for frames.
        deliver_requested_frames(&view, cx, mark, 8);
        assert!(view.read_with(cx, |view, _| view.scroll_y > 0.0));

        // DocumentStart lands on offset 0, the closed opening fence row.
        let mark = requested_frames(&view, cx);
        view.update(cx, |view, cx| {
            view.dispatch(EditorCommand::MoveToStart { extend: false }, cx);
        });
        settle(cx);
        // The helper returns only once a delivered frame asked for nothing more.
        deliver_requested_frames(&view, cx, mark, 6);
        let settled_requests = requested_frames(&view, cx);
        settle(cx);
        assert_eq!(
            requested_frames(&view, cx),
            settled_requests,
            "no further frame is asked for once the request is consumed"
        );
        assert_caret_row_on_screen(&view, cx, "closed opening fence");
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
            let mark = requested_frames(&view, cx);
            view.update(cx, |view, cx| {
                view.dispatch(EditorCommand::MoveDown { extend }, cx);
            });
            settle(cx);
            deliver_requested_frames(&view, cx, mark, 6);
            view.read_with(cx, |view, _| {
                assert!(!view.pending_caret_visibility_after_layout, "extend={extend}");
            });
            assert_caret_row_on_screen(&view, cx, "table or code row");
            let settled_requests = requested_frames(&view, cx);
            settle(cx);
            assert_eq!(
                requested_frames(&view, cx),
                settled_requests,
                "extend={extend}: no frame is requested after the request ended"
            );
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
            toggle_and_settle(&view, cx);
            toggle_and_settle(&view, cx);
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
    fn a_wrapped_fence_info_hidden_by_reading_keeps_the_same_source_at_the_top(
        cx: &mut gpui::TestAppContext,
    ) {
        let info = "info ".repeat(120);
        let mut source = String::new();
        for n in 0..20 {
            source.push_str(&format!("before {n:02}\n\n"));
        }
        let fence = source.len();
        source.push_str(&format!("```{info}\nfirst code\nsecond code\nthird code\n```\n\n"));
        for n in 0..20 {
            source.push_str(&format!("after {n:02}\n\n"));
        }
        let info_last = SourceOffset(fence + 3 + info.len() - 2);
        let first_code = SourceOffset(fence + 3 + info.len() + 1);
        let (view, cx) = open_view(cx, &source, 400.0, 400.0);
        // The caret inside the opening line keeps the fence, with its long
        // wrapped info, open while editing.
        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset(fence + 3)))
                .unwrap();
            view.after_input(cx);
        });
        settle(cx);
        view.update(cx, |view, cx| {
            let ordinal = view
                .current_index()
                .and_then(|index| index.block_at(SourceOffset(fence)))
                .expect("fence block")
                .ordinal;
            view.scroll_y = view.heights.prefix_sum(ordinal);
            cx.notify();
        });
        settle(cx);
        // Put the top edge in the middle of the last wrapped info row.
        let (block_top, row_top, row_height) = view.read_with(cx, |view, _| {
            let ordinal = view
                .current_index()
                .and_then(|index| index.block_at(SourceOffset(fence)))
                .expect("fence block")
                .ordinal;
            let (top, height) = view
                .source_offset_row_bounds(info_last)
                .expect("the info row is laid out");
            (view.heights.prefix_sum(ordinal), top, height)
        });
        assert!(
            row_top > block_top + row_height * 2.0,
            "the info wraps over several rows ({row_top} in the block at {block_top})"
        );
        view.update(cx, |view, cx| {
            view.scroll_y = row_top + row_height * 0.5;
            cx.notify();
        });
        settle(cx);
        let selection = view.read_with(cx, |view, _| view.editor().selection());

        toggle_and_settle(&view, cx);
        view.read_with(cx, |view, _| {
            let (first_top, _) = view
                .source_offset_row_bounds(first_code)
                .expect("the first code row is laid out");
            assert!(
                (view.scroll_y - first_top).abs() <= view.line_height() * 0.5,
                "the hidden info rows resolve to the next visible source row: {} vs {first_top}",
                view.scroll_y
            );
            assert_eq!(view.editor().selection(), selection);
        });
    }

    #[gpui::test]
    fn a_switch_does_not_carry_its_position_into_another_document(
        cx: &mut gpui::TestAppContext,
    ) {
        let long: String = (0..200).map(|n| format!("line {n:03}\n")).collect();
        let (view, cx) = open_view(cx, &long, 800.0, 400.0);
        let (first, second) = view.update(cx, |view, cx| {
            let first = view.sessions.active_id();
            let second = view.sessions.open_untitled(&long, "B.md");
            // Whether or not opening activates it, start on the first one.
            assert!(view.sessions.activate(first));
            view.on_document_replaced();
            assert!(view.activate_session(second, cx));
            (first, second)
        });
        settle(cx);
        view.update(cx, |view, cx| {
            view.scroll_y = 120.0;
            view.activate_session(first, cx);
        });
        settle(cx);
        // Scroll the first document into the middle, keep a selection there.
        let middle = long.find("line 120").unwrap();
        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset(middle)))
                .unwrap();
            view.scroll_y = 2_000.0;
            cx.notify();
        });
        settle(cx);
        let first_selection = view.read_with(cx, |view, _| view.editor().selection());

        // Tab switch in the same event cycle as the mode switch.
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                assert!(view.toggle_editor_mode(window, cx));
                assert!(view.pending_mode_anchor.is_some());
                view.activate_session(second, cx);
                assert!(view.pending_mode_anchor.is_none());
            });
        });
        let mark = requested_frames(&view, cx);
        settle(cx);
        deliver_requested_frames(&view, cx, mark, 8);
        view.read_with(cx, |view, _| {
            assert_eq!(view.sessions.active_id(), second);
            assert!(
                (view.scroll_y - 120.0).abs() <= view.line_height(),
                "the incoming document keeps its saved scroll: {}",
                view.scroll_y
            );
            assert_eq!(
                view.sessions.get(second).map(|session| session.view_state().scroll_y),
                Some(120.0)
            );
            assert_eq!(
                view.sessions.get(first).map(|session| session.editor().selection()),
                Some(first_selection)
            );
        });

        // A replaced document (new document, reload) in the same cycle as the
        // switch back to editing.
        view.update(cx, |view, cx| {
            view.scroll_y = 2_000.0;
            cx.notify();
        });
        settle(cx);
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                assert!(view.toggle_editor_mode(window, cx));
                assert!(view.pending_mode_anchor.is_some());
                view.sessions
                    .active_mut()
                    .set_view_state(SessionViewState { scroll_y: 60.0 });
                view.on_document_replaced();
                assert!(view.pending_mode_anchor.is_none());
                cx.notify();
            });
        });
        let mark = requested_frames(&view, cx);
        settle(cx);
        deliver_requested_frames(&view, cx, mark, 8);
        view.read_with(cx, |view, _| {
            assert!(
                (view.scroll_y - 60.0).abs() <= view.line_height(),
                "the replacement keeps its saved scroll: {}",
                view.scroll_y
            );
        });
    }

    #[gpui::test]
    fn a_background_snapshot_on_another_basis_does_not_leave_selection_only_heights(
        cx: &mut gpui::TestAppContext,
    ) {
        let source: String = (0..60).map(|n| format!("```\ncode{n:02}\n```\n\n")).collect();
        let at = |n: usize| SourceOffset(source.find(&format!("code{n:02}")).unwrap());
        let (view, cx) = open_view(cx, &source, 800.0, 400.0);
        let select = |view: &TestView, cx: &mut TestCx, from: usize, to: usize| {
            view.update(cx, |view, cx| {
                view.editor_mut()
                    .set_selection(Selection {
                        anchor: at(from),
                        active: at(to),
                    })
                    .unwrap();
                view.after_input(cx);
            });
        };
        // D (the caret) -> A starts a job that has not run yet.
        select(&view, cx, 5, 25);
        cx.run_until_parked();
        assert!(view.read_with(cx, |view, _| view.document_parse_job_running));
        // Reading with selection B, then back: every height is based on B.
        assert!(toggle(&view, cx));
        select(&view, cx, 30, 50);
        assert!(toggle(&view, cx));
        cx.run_until_parked();
        // The selection returns to A before the old job completes.
        select(&view, cx, 5, 25);
        for _ in 0..4 {
            settle(cx);
        }
        view.read_with(cx, |view, _| {
            let document = view.editor().document();
            let index = view.current_index().expect("formal index");
            let expected = block_heights_with_disclosure(
                document,
                index,
                view.line_height(),
                view.active_height_disclosure(),
            );
            let collapsed =
                block_heights_with_disclosure(document, index, view.line_height(), None);
            for n in 38..48 {
                let ordinal = index.ordinal_at(at(n)).expect("fence block");
                assert!(
                    (expected[ordinal] - collapsed[ordinal]).abs() < 0.01,
                    "fence {n} is outside the final selection"
                );
                let height = view.heights.height(ordinal).unwrap();
                assert!(
                    (height - expected[ordinal]).abs() < 0.01,
                    "fence {n} only B opened is closed again: {height} vs {}",
                    expected[ordinal]
                );
            }
            for n in 10..20 {
                let ordinal = index.ordinal_at(at(n)).expect("fence block");
                let height = view.heights.height(ordinal).unwrap();
                assert!(
                    height >= expected[ordinal] - 0.01,
                    "fence {n} inside A stays open: {height} vs {}",
                    expected[ordinal]
                );
            }
        });
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

    /// The state a refused switch must leave alone.
    fn composition_state(
        view: &TestView,
        cx: &mut TestCx,
    ) -> (String, Option<std::ops::Range<usize>>, Selection, bool) {
        let marked = cx.update(|window, app| {
            view.update(app, |view, cx| view.marked_text_range(window, cx))
        });
        view.read_with(cx, |view, _| {
            (
                view.editor().document().full_text(),
                marked,
                view.editor().selection(),
                view.editor().ime().is_some(),
            )
        })
    }

    fn press_switch_button(view: &TestView, cx: &mut TestCx) {
        let point = cx
            .debug_bounds("reading-mode-toggle")
            .expect("the switch is painted")
            .center();
        cx.simulate_mouse_down(point, MouseButton::Left, gpui::Modifiers::none());
        cx.simulate_mouse_up(point, MouseButton::Left, gpui::Modifiers::none());
        cx.run_until_parked();
        let _ = view;
    }

    fn body_is_focused(view: &TestView, cx: &mut TestCx) -> bool {
        cx.update(|window, app| view.read(app).focus_handle.is_focused(window))
    }

    #[gpui::test]
    fn pressing_the_disabled_switch_during_a_real_body_composition_changes_nothing(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = open_view(cx, "hello\n", 800.0, 400.0);
        let row = cx
            .debug_bounds("row-0-0")
            .expect("the first row is painted")
            .center();
        cx.simulate_click(row, gpui::Modifiers::none());
        cx.run_until_parked();
        assert!(body_is_focused(&view, cx));

        for (label, commit) in [("commit", true), ("cancel", false)] {
            // A composition started through the real input handler.
            let started = if commit { "に" } else { "き" };
            cx.update(|window, app| {
                view.update(app, |view, cx| {
                    view.replace_and_mark_text_in_range(None, started, Some(1..1), window, cx);
                });
            });
            settle(cx);
            assert!(view.read_with(cx, |view, _| view.editor_mode_switch_blocked()));
            let before = composition_state(&view, cx);
            assert!(before.3 && before.1.is_some(), "{label}: composing");

            press_switch_button(&view, cx);
            assert!(body_is_focused(&view, cx), "{label}: focus stays in the body");
            assert_eq!(composition_state(&view, cx), before, "{label}: nothing changed");
            assert_eq!(
                view.read_with(cx, |view, _| view.editor_mode()),
                EditorMode::Editing,
                "{label}"
            );

            // The end of the composition through the real handler / key.
            if commit {
                cx.update(|window, app| {
                    view.update(app, |view, cx| view.unmark_text(window, cx));
                });
            } else {
                cx.simulate_keystrokes("escape");
            }
            settle(cx);
            let after = composition_state(&view, cx);
            assert!(!after.3 && after.1.is_none(), "{label}: composition ended");
            assert_eq!(
                after.0.contains(started),
                commit,
                "{label}: the text is kept on commit and removed on cancel"
            );
            assert!(body_is_focused(&view, cx), "{label}");

            // Now the switch works, and returns to editing for the next round.
            press_switch_button(&view, cx);
            assert_eq!(
                view.read_with(cx, |view, _| view.editor_mode()),
                EditorMode::Reading,
                "{label}: the switch is available after the composition"
            );
            press_switch_button(&view, cx);
            assert_eq!(
                view.read_with(cx, |view, _| view.editor_mode()),
                EditorMode::Editing,
                "{label}"
            );
            cx.simulate_click(row, gpui::Modifiers::none());
            cx.run_until_parked();
        }
    }

    #[gpui::test]
    fn the_switch_is_reachable_and_big_enough_with_many_long_named_tabs(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = open_view(cx, "body\n", 320.0, 240.0);
        view.update(cx, |view, cx| {
            for index in 0..8 {
                view.sessions
                    .open_untitled("body\n", format!("A-very-long-file-name-{index}.md"));
            }
            cx.notify();
        });
        settle(cx);
        let check_layout = |cx: &mut TestCx, label: &str| {
            let button = cx
                .debug_bounds("reading-mode-toggle")
                .unwrap_or_else(|| panic!("{label}: the switch is painted"));
            let tabs = cx
                .debug_bounds("file-tabs")
                .unwrap_or_else(|| panic!("{label}: the tab area is painted"));
            let strip = cx
                .debug_bounds("file-tab-strip")
                .unwrap_or_else(|| panic!("{label}: the tab strip is painted"));
            assert!(f32::from(button.size.width) >= 24.0, "{label}: hit width");
            assert!(f32::from(button.size.height) >= 24.0, "{label}: hit height");
            // `file-tabs` is the whole header row; the strip is the part that
            // scrolls. The switch sits outside the strip and inside the row.
            assert!(
                button.left() >= strip.right() - px(0.5) || button.right() <= strip.left() + px(0.5),
                "{label}: the switch does not overlap the tab strip ({button:?} vs {strip:?})"
            );
            assert!(
                button.left() >= tabs.left()
                    && button.right() <= tabs.right() + px(0.5)
                    && button.top() >= tabs.top()
                    && button.bottom() <= tabs.bottom() + px(0.5),
                "{label}: the switch is inside the header ({button:?} vs {tabs:?})"
            );
            assert!(tabs.right() <= px(320.5), "{label}: the header fits the window");
            (button, strip)
        };
        let (button_before, _) = check_layout(cx, "before scrolling");

        // Scroll the tab strip sideways: the switch stays where it is.
        let strip = cx.debug_bounds("file-tab-strip").expect("strip painted");
        cx.simulate_event(ScrollWheelEvent {
            position: strip.center(),
            delta: ScrollDelta::Pixels(point(px(0.0), px(-800.0))),
            modifiers: gpui::Modifiers::none(),
            touch_phase: gpui::TouchPhase::Moved,
        });
        cx.run_until_parked();
        assert!(view.read_with(cx, |view, _| view.file_tabs_scroll.offset().x < px(0.0)));
        let (button_after, _) = check_layout(cx, "after scrolling");
        assert_eq!(button_before, button_after, "the switch is fixed");

        // A real click on it works and neither moves the tab nor the focus
        // away from the body.
        let row = cx
            .debug_bounds("row-0-0")
            .expect("the first row is painted")
            .center();
        cx.simulate_click(row, gpui::Modifiers::none());
        cx.run_until_parked();
        assert!(body_is_focused(&view, cx));
        let active = view.read_with(cx, |view, _| view.sessions.active_id());
        let selection = view.read_with(cx, |view, _| view.editor().selection());
        press_switch_button(&view, cx);
        view.read_with(cx, |view, _| {
            assert_eq!(view.editor_mode(), EditorMode::Reading);
            assert_eq!(view.sessions.active_id(), active);
            assert_eq!(view.editor().selection(), selection);
        });
        assert!(body_is_focused(&view, cx), "the body keeps focus");
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
