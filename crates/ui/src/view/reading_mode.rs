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
                            // The layout is stamped with the document revision it
                            // was drawn at; `block.revision` is only when this
                            // block was last parsed and stays old after an edit
                            // to another block.
                            document.revision(),
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
    ///
    /// The tooltip is a `HoverCard` around the wrapper, like the tab path one:
    /// `Button::tooltip` needs a `gpui_component::Root` as the window root,
    /// which the app does not have, so it would never be shown.
    pub(super) fn reading_mode_button(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
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
            .accessibility_label(label)
            .disabled(blocked)
            .on_click(move |_, window, app| {
                view.update(app, |view, cx| {
                    view.toggle_editor_mode(window, cx);
                });
            });
        let wrapper = div()
            .id("reading-mode-toggle-wrapper")
            .debug_selector(|| "reading-mode-toggle".to_owned())
            .flex_none()
            .px_1()
            .on_mouse_down(MouseButton::Left, |_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
            })
            .child(button);
        HoverCard::new("reading-mode-toggle-tooltip")
            .anchor(Anchor::TopCenter)
            .trigger(wrapper)
            .content(move |_, _, _| {
                div()
                    .px_2()
                    .py_1()
                    .debug_selector(move || format!("reading-mode-toggle-tooltip:{tooltip}"))
                    .child(tooltip)
            })
            .into_any_element()
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

    /// Delivers the frames `render` asked for, at most `limit` rounds of them,
    /// and returns how many rounds it took. Each round runs the window's real
    /// next-frame callbacks (`Window::simulate_next_frame`, the ones
    /// `request_animation_frame` registered) and then lets the resulting
    /// redraw settle. It ends on the first round with no callback left, so a
    /// view that stopped asking is not nudged and a request without a real
    /// callback is not hidden: the callers then assert the pending state is
    /// gone. `mark` is the request counter from before the action under test;
    /// if it moved, at least one real callback must have been delivered.
    fn deliver_requested_frames(
        view: &TestView,
        cx: &mut TestCx,
        mark: u64,
        limit: usize,
    ) -> usize {
        let mut delivered = 0;
        loop {
            // Timers and executor work first, so a callback they register is
            // already queued when the queue is inspected. The round that finds
            // none ends the loop with no settle after it, so no callback made
            // by that settle can be left behind unnoticed.
            settle(cx);
            let callbacks = cx.update(|window, app| window.simulate_next_frame(app));
            if callbacks == 0 {
                break;
            }
            delivered += 1;
            assert!(
                delivered <= limit,
                "the layout correction asks for frames without converging ({delivered} delivered)"
            );
        }
        if requested_frames(view, cx) != mark {
            assert!(
                delivered > 0,
                "frames were requested but no next-frame callback was registered"
            );
        }
        delivered
    }

    /// Switches the mode, then settles it, including the frames the switch's
    /// own anchor resolution asks for.
    fn toggle_and_settle(view: &TestView, cx: &mut TestCx) {
        let mark = requested_frames(view, cx);
        assert!(toggle(view, cx));
        settle(cx);
        deliver_requested_frames(view, cx, mark, 8);
        assert!(
            view.read_with(cx, |view, _| view.pending_mode_anchor.is_none()),
            "the mode anchor was resolved by the delivered frames"
        );
    }

    /// Geometry the last render actually drew for one source offset.
    #[derive(Debug)]
    struct MeasuredRow {
        /// Content-space top and height of the fragment holding the offset.
        top: f32,
        height: f32,
        /// Content-space top and height of the owning block.
        block_top: f32,
        block_height: f32,
    }

    /// Proves `offset` lies on a physical source line that Reading collapses,
    /// from the current formal projection and the document's own line
    /// ownership (half-open per physical line; only the document end belongs
    /// to the last line). A line the presentation still carries must be
    /// closed (zero height, no disclosure); a line it omitted is accepted
    /// only because the projection names it as collapsed.
    fn closed_structural_row(
        view: &EditorView,
        index: &BlockIndex,
        block: &IndexedBlock,
        visual: &VisualBlock,
        offset: SourceOffset,
    ) -> Result<(), String> {
        let document = view.editor().document();
        let line = document
            .line_for_offset(offset)
            .map_err(|error| format!("no physical line owns {offset:?}: {error:?}"))?;
        let line_range = document
            .line_range(line)
            .map_err(|error| format!("no range for {line:?}: {error:?}"))?;
        let block_lines = block_line_span(document, block)
            .ok_or("the owning block has no physical line span")?;
        if !block_lines.contains(&line.0) {
            return Err(format!(
                "{offset:?} is on {line:?}, outside the owning block's lines {block_lines:?}"
            ));
        }
        let relative_line = line.0 - block_lines.start;
        let block_is_final = block.ordinal + 1 == index.len();
        let disclosure = view.active_height_disclosure();
        if disclosure.is_some() {
            return Err("a reading view still has an open height disclosure".to_owned());
        }
        let collapsed_fence = index
            .fence_height_projection(block)
            .is_some_and(|projection| {
                projection.is_fence_row_line(relative_line)
                    && fence_row_is_inactive(
                        projection,
                        block.source_range,
                        relative_line,
                        disclosure,
                        block_is_final,
                    )
            });
        let table_delimiter = index.table_projection(block).is_some_and(|projection| {
            projection.delimiter_range.is_some_and(|delimiter| {
                document
                    .line_for_offset(delimiter.start)
                    .is_ok_and(|delimiter_line| delimiter_line == line)
            })
        });
        if !collapsed_fence && !table_delimiter {
            return Err(format!(
                "no layout row owns {offset:?} and {line:?} is not a known collapsed \
                 structural row of the owning block"
            ));
        }
        if let Some(presented) = visual
            .lines
            .iter()
            .find(|presented| presented.source_range.start == line_range.start)
            && (presented.height() != 0.0 || presented.disclosure.is_some())
        {
            return Err(format!(
                "the presented structural row is not closed (kind {:?}, height {})",
                presented.kind,
                presented.height()
            ));
        }
        Ok(())
    }

    /// Reads only data a render produced: the owning block's presentation and
    /// layout must both exist and agree with the index, the document revision,
    /// the width/font and the mode, and the latest frame must have drawn the
    /// row (`line_owners` is cleared and refilled by every render, so a layout
    /// retained for an off-screen block is not mistaken for a drawn one). A
    /// missing piece is an observation failure; nothing is estimated, laid out
    /// or shaped to fill it in.
    fn measured_row(
        view: &EditorView,
        window: &Window,
        offset: SourceOffset,
    ) -> Result<MeasuredRow, String> {
        let shaper = crate::shape::WindowShaper::new(window, view.zoom);
        let document = view.editor().document();
        let index = view.current_index().ok_or("no current block index")?;
        let block = index
            .block_at(offset)
            .ok_or_else(|| format!("no block owns {offset:?}"))?;
        let visual = view
            .block_cache
            .get(&block.id)
            .ok_or("the owning block has no presentation")?;
        let entry = view
            .layout_cache
            .get(&block.id)
            .ok_or("the owning block has no cached layout")?;
        if !visual.matches(&block) {
            return Err("the presentation no longer matches the index block".to_owned());
        }
        if !entry.is_valid(
            view.content_width,
            view.layout_font_revision,
            document.revision(),
        ) {
            return Err("the layout is stale for the width, font or document revision".to_owned());
        }
        let layout = &entry.layout;
        if layout.block != visual.id {
            return Err("the layout belongs to another block".to_owned());
        }
        if view.editor_mode == EditorMode::Reading
            && visual.lines.iter().any(|line| line.disclosure.is_some())
        {
            return Err("a reading presentation still discloses markers".to_owned());
        }
        let block_top = view.heights.prefix_sum(block.ordinal);
        let block_height = view.heights.height(block.ordinal).unwrap_or(0.0);
        let Some(row_index) = layout.row_for_source(offset) else {
            // Only a source line the formal projection proves Reading
            // collapses (a table delimiter, or a fence row) may have no
            // layout row; the presentation may then omit the line entirely
            // (it is outside the render window) or keep it at zero height.
            // The owning block must then be drawn by the latest render.
            closed_structural_row(view, index, &block, visual, offset)?;
            if !view
                .line_owners
                .values()
                .any(|(owner, _)| *owner == visual.id)
            {
                return Err("the latest render did not draw the owning block".to_owned());
            }
            return Ok(MeasuredRow {
                top: block_top,
                height: 0.0,
                block_top,
                block_height,
            });
        };
        let row = layout
            .lines
            .get(row_index)
            .ok_or("the layout row is missing")?;
        let line = visual
            .lines
            .get(row.line)
            .ok_or("the layout row has no visual line")?;
        if view.line_owners.get(&(line.line_id as usize)) != Some(&(visual.id, row.line)) {
            return Err("the latest render did not draw this row".to_owned());
        }
        let point = layout
            .point_for_source(visual, offset, &shaper)
            .ok_or("the layout has no fragment for the offset")?;
        Ok(MeasuredRow {
            top: block_top + point.y,
            height: point.height,
            block_top,
            block_height,
        })
    }

    /// The row holding `offset` is on screen with its real, drawn geometry.
    /// A positive fragment must itself intersect the viewport (a wrapped cell
    /// fragment off screen is not rescued by the rest of its row). A closed
    /// structural row has no height of its own; there the owning block must be
    /// visible instead.
    fn assert_offset_row_on_screen(
        view: &TestView,
        cx: &mut TestCx,
        offset: SourceOffset,
        label: &str,
    ) -> MeasuredRow {
        let (row, scroll_y, viewport, tolerance) = cx.update(|window, app| {
            let view = view.read(app);
            (
                measured_row(view, window, offset),
                view.scroll_y,
                view.viewport_height,
                view.line_height(),
            )
        });
        let row = row.unwrap_or_else(|error| {
            panic!("{label}: no measured geometry for {offset:?}: {error}")
        });
        let (top, height) = if row.height > 0.0 {
            (row.top, row.height)
        } else {
            (row.block_top, row.block_height)
        };
        assert!(
            top.is_finite() && height.is_finite() && height > 0.0,
            "{label}: measured {row:?}"
        );
        assert!(
            top + height >= scroll_y - tolerance && top <= scroll_y + viewport + tolerance,
            "{label}: {row:?} is outside the viewport at {scroll_y} (+{viewport})"
        );
        row
    }

    /// The caret row is drawn on screen with its real coordinates.
    fn assert_caret_row_on_screen(view: &TestView, cx: &mut TestCx, label: &str) {
        let active = view.read_with(cx, |view, _| view.editor().selection().active);
        assert_offset_row_on_screen(view, cx, active, label);
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

    /// Gives the body the real keyboard focus, as a click into it would.
    fn focus_body_for_keys(view: &TestView, cx: &mut TestCx) {
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                window.focus(&view.focus_handle, cx);
            });
        });
        cx.run_until_parked();
    }

    /// One real key press through the registered key bindings, then the frames
    /// it asked for, and the pending/visibility/geometry checks that must hold
    /// once the layout settled. The oracle is the rendered row geometry
    /// (`source_offset_row_bounds`), not the height estimate.
    fn press_and_check(view: &TestView, cx: &mut TestCx, keys: &str, label: &str) -> SourceOffset {
        let mark = requested_frames(view, cx);
        cx.simulate_keystrokes(keys);
        settle(cx);
        deliver_requested_frames(view, cx, mark, 8);
        view.read_with(cx, |view, _| {
            assert!(
                !view.pending_caret_visibility_after_layout,
                "{label}: the pending visibility request converged"
            );
            assert!(
                view.pending_mode_anchor.is_none(),
                "{label}: no mode anchor is left pending"
            );
        });
        // The oracle is the drawn fragment, not a physical-line estimate.
        let reached = view.read_with(cx, |view, _| view.editor().selection().active);
        assert_offset_row_on_screen(view, cx, reached, label);
        reached
    }

    #[gpui::test]
    fn reading_real_down_keys_cross_tables_and_code_in_a_narrow_viewport(
        cx: &mut gpui::TestAppContext,
    ) {
        // Narrow viewport so the long table cells wrap into several visual
        // lines. Paragraphs surround the closed table and the closed fence.
        let source = "before the table\n\n\
            | a long header cell that wraps | another long header cell that wraps |\n\
            | --- | --- |\n\
            | one cell that also has to wrap around | 2 |\n\n\
            between table and code\n\n\
            ```rust\nlet x = 1;\n```\n\n\
            after the code\n";
        let (view, cx) = open_view(cx, source, 260.0, 300.0);
        let before = snapshot(&view, cx);
        assert!(toggle(&view, cx));
        settle(cx);
        let mark = requested_frames(&view, cx);
        deliver_requested_frames(&view, cx, mark, 8);
        focus_body_for_keys(&view, cx);
        view.update(cx, |view, cx| {
            view.dispatch(EditorCommand::MoveToStart { extend: false }, cx);
        });
        settle(cx);

        // Plain Down: the offset never goes backwards and the document end is
        // reachable by the real key alone.
        let mut previous = view.read_with(cx, |view, _| view.editor().selection().active);
        let mut visited = vec![previous];
        let table_body = source.find("| one cell").unwrap()
            ..=source.find("\n\nbetween table and code").unwrap();
        let code_body = source.find("let x = 1;").unwrap()
            ..=source.find("let x = 1;").unwrap() + "let x = 1;".len();
        let mut table_fragments_seen = false;
        for step in 0..60 {
            let reached = press_and_check(&view, cx, "down", &format!("down #{step}"));
            assert!(reached >= previous, "down #{step}: moved backwards");
            if table_body.contains(&reached.0) {
                // The narrow cells really wrap into several drawn fragments.
                // A table row is one layout row; its cells' wrapped lines are
                // the `fragments` of each `TableCellLayout`.
                table_fragments_seen |= view.read_with(cx, |view, _| {
                    view.current_index()
                        .and_then(|index| index.block_at(reached))
                        .and_then(|block| view.layout_cache.get(&block.id))
                        .and_then(|entry| {
                            let layout = &entry.layout;
                            layout.lines.get(layout.row_for_source(reached)?)
                        })
                        .is_some_and(|row| {
                            row.table_cells.iter().any(|cell| cell.fragments.len() > 1)
                        })
                });
            }
            visited.push(reached);
            if reached == previous {
                break;
            }
            previous = reached;
        }
        let last = *visited.last().unwrap();
        assert!(
            last.0 >= source.find("after the code").unwrap(),
            "Down reaches the last paragraph (stopped at {})",
            last.0
        );
        // Every structural part was actually stood on, not just passed: the
        // reached offsets lie inside the table body and the code body.
        assert!(
            visited.iter().any(|offset| table_body.contains(&offset.0)),
            "Down stood in the table body"
        );
        assert!(
            visited.iter().any(|offset| code_body.contains(&offset.0)),
            "Down stood in the code body"
        );
        assert!(table_fragments_seen, "a narrow table cell has several fragments");

        // Shift+Down from the start keeps the anchor and extends forward.
        view.update(cx, |view, cx| {
            view.dispatch(EditorCommand::MoveToStart { extend: false }, cx);
        });
        settle(cx);
        let mut previous = SourceOffset(0);
        for step in 0..60 {
            let reached = press_and_check(&view, cx, "shift-down", &format!("shift-down #{step}"));
            view.read_with(cx, |view, _| {
                assert_eq!(
                    view.editor().selection().anchor,
                    SourceOffset(0),
                    "shift-down #{step}: the anchor stays"
                );
            });
            assert!(reached >= previous, "shift-down #{step}: moved backwards");
            if reached == previous {
                break;
            }
            previous = reached;
        }
        assert!(
            previous.0 >= source.find("after the code").unwrap(),
            "Shift+Down reaches the last paragraph (stopped at {})",
            previous.0
        );
        assert_eq!(snapshot(&view, cx), before, "reading keys never edit the body");
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
        let info_start = fence + 3;
        let info_end = info_start + info.len();
        let (view, cx) = open_view(cx, &source, 400.0, 400.0);
        let mut trace = String::new();
        // The caret in the last wrapped info row keeps the fence, with its long
        // wrapped info, open while editing.
        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection::caret(info_last))
                .unwrap();
            view.after_input(cx);
        });
        settle(cx);
        let selection = view.read_with(cx, |view, _| view.editor().selection());
        let fence_ordinal = view.read_with(cx, |view, _| {
            view.current_index()
                .and_then(|index| index.block_at(SourceOffset(fence)))
                .expect("fence block")
                .ordinal
        });
        // Setup uses the ordinary scroll-only handlers and delivers the frames
        // they ask for; nothing writes `scroll_y` or a pending field directly.
        let mark = requested_frames(&view, cx);
        view.update(cx, |view, cx| {
            let top = view.heights.prefix_sum(fence_ordinal);
            view.finish_scroll_only(top, cx);
        });
        deliver_requested_frames(&view, cx, mark, 8);
        // Centre the caret row: this draws the long opening line's last
        // fragment and pins the block window around it.
        let mark = requested_frames(&view, cx);
        cx.update(|window, app| {
            view.update(app, |view, cx| view.center_cursor_line(window, cx));
        });
        deliver_requested_frames(&view, cx, mark, 8);
        cx.update(|window, app| {
            trace.push_str(&wrapped_fence_trace(
                view.read(app),
                window,
                "after centring",
                (fence, info_start..info_end, info_last, first_code),
            ));
        });
        // Put the top edge in the middle of the last wrapped info row, with the
        // scroll-only handler, and re-measure after each settle.
        for attempt in 0..3 {
            let measured = cx.update(|window, app| {
                let result = measured_row(view.read(app), window, info_last);
                trace.push_str(&wrapped_fence_trace(
                    view.read(app),
                    window,
                    &format!("scroll attempt {attempt} (before)"),
                    (fence, info_start..info_end, info_last, first_code),
                ));
                result
            });
            let info_row = measured.unwrap_or_else(|error| {
                panic!("the last info fragment is not measured: {error}\n{trace}")
            });
            let (row_top, row_height) = (info_row.top, info_row.height);
            assert!(
                row_height > 0.0 && row_top > info_row.block_top + row_height * 2.0,
                "the info wraps over several rows ({row_top} in the block at {})\n{trace}",
                info_row.block_top
            );
            let scrolled = view.read_with(cx, |view, _| view.scroll_y);
            let target = row_top + row_height * 0.5;
            if scrolled >= row_top - 0.01 && scrolled < row_top + row_height && attempt > 0 {
                break;
            }
            let mark = requested_frames(&view, cx);
            view.update(cx, |view, cx| view.finish_scroll_only(target, cx));
            deliver_requested_frames(&view, cx, mark, 8);
        }
        assert_eq!(view.read_with(cx, |view, _| view.editor().selection()), selection);

        // Precondition: after the last settle the info fragment is measured
        // again, the viewport top is still inside it, and the anchor that would
        // be captured names the right source offset inside it. The trace is
        // part of every failure message.
        let (measured_again, before) = cx.update(|window, app| {
            let view = view.read(app);
            trace.push_str(&wrapped_fence_trace(
                view,
                window,
                "before toggle",
                (fence, info_start..info_end, info_last, first_code),
            ));
            let anchor = view.capture_mode_anchor();
            (
                measured_row(view, window, info_last),
                (
                    view.scroll_y,
                    anchor.map(|anchor| (anchor.offset.0, anchor.inside_row)),
                ),
            )
        });
        let measured_again = measured_again.unwrap_or_else(|error| {
            panic!("the info fragment is not measured after the last settle: {error}\n{trace}")
        });
        let (row_top, row_height) = (measured_again.top, measured_again.height);
        assert!(
            row_height > 0.0
                && before.0 >= row_top - 0.01
                && before.0 < row_top + row_height,
            "the viewport top is inside the measured info fragment: {} vs {row_top}+{row_height}\n{trace}",
            before.0
        );
        assert!(
            before
                .1
                .is_some_and(|(offset, _)| (info_start..info_end).contains(&offset)),
            "the captured anchor names an offset inside the info: {:?} (info {info_start}..{info_end})\n{trace}",
            before.1
        );

        let mark = requested_frames(&view, cx);
        assert!(toggle(&view, cx));
        cx.update(|window, app| {
            trace.push_str(&wrapped_fence_trace(
                view.read(app),
                window,
                "after toggle",
                (fence, info_start..info_end, info_last, first_code),
            ));
        });
        let mut delivered = 0;
        loop {
            settle(cx);
            let callbacks = cx.update(|window, app| window.simulate_next_frame(app));
            cx.update(|window, app| {
                trace.push_str(&wrapped_fence_trace(
                    view.read(app),
                    window,
                    &format!("frame round {delivered} ({callbacks} callbacks)"),
                    (fence, info_start..info_end, info_last, first_code),
                ));
            });
            if callbacks == 0 {
                break;
            }
            delivered += 1;
            assert!(delivered <= 8, "the anchor does not converge\n{trace}");
        }
        if requested_frames(&view, cx) != mark {
            assert!(delivered > 0, "frames were requested but none was registered\n{trace}");
        }
        let (code, opening, scroll_y, line_height, anchor_pending, selection_after) =
            cx.update(|window, app| {
                let view = view.read(app);
                (
                    measured_row(view, window, first_code),
                    measured_row(view, window, SourceOffset(info_start)),
                    view.scroll_y,
                    view.line_height(),
                    view.pending_mode_anchor.is_some(),
                    view.editor().selection(),
                )
            });
        assert!(!anchor_pending, "anchor resolved\n{trace}");
        // The hidden opening line is observed as a closed row (no height) or as
        // no row at all; it is never a drawn positive-height fragment.
        assert!(
            !matches!(&opening, Ok(row) if row.height > 0.0),
            "reading draws no fragment of the hidden info: {opening:?}\n{trace}"
        );
        let code = code.unwrap_or_else(|error| {
            panic!("the first code row is not measured after the switch: {error}\n{trace}")
        });
        assert!(
            code.height > 0.0 && (scroll_y - code.top).abs() <= line_height * 0.5,
            "the hidden info rows resolve to the next visible source row: {scroll_y} vs {}\n{trace}",
            code.top
        );
        assert_eq!(selection_after, selection);
    }

    /// One bounded line of observations about the wrapped-fence test: the
    /// measured info fragment (or why it cannot be measured), the anchor that
    /// would be captured, every pending position request, the fence block's
    /// top/height, its presentation window and its layout's clipped space and
    /// first/last source. Reads only caches; nothing is laid out or estimated.
    fn wrapped_fence_trace(
        view: &EditorView,
        window: &Window,
        stage: &str,
        (fence, info, info_last, first_code): (usize, std::ops::Range<usize>, SourceOffset, SourceOffset),
    ) -> String {
        let block = view
            .current_index()
            .and_then(|index| index.block_at(SourceOffset(fence)));
        let block_geometry = block.as_ref().map(|block| {
            (
                view.heights.prefix_sum(block.ordinal),
                view.heights.height(block.ordinal).unwrap_or(0.0),
            )
        });
        let visual = block.as_ref().and_then(|block| view.block_cache.get(&block.id)).map(|visual| {
            (
                visual.lines_before,
                visual.lines_after,
                visual.zero_height_lines_before,
                visual.zero_height_lines_after,
                visual.lines.first().map(|line| line.source_range.start.0),
                visual.lines.last().map(|line| line.source_range.end.0),
            )
        });
        let layout = block
            .as_ref()
            .and_then(|block| view.layout_cache.get(&block.id))
            .map(|entry| {
                let layout = &entry.layout;
                (
                    layout.leading_space,
                    layout.trailing_space,
                    layout.lines.first().map(|row| row.source_range.start.0),
                    layout.lines.last().map(|row| row.source_range.end.0),
                )
            });
        let anchor = view.capture_mode_anchor();
        let anchor_inside = anchor.map(|anchor| {
            (
                anchor.offset.0,
                anchor.inside_row,
                info.contains(&anchor.offset.0),
            )
        });
        format!(
            "[{stage}] scroll_y={} info={:?} first_code={:?} anchor(offset,inside_row,in_info)={anchor_inside:?} \
             pending(caret={}, centre_frames={}, find={}, zoom={}, mode={}) centre_line={:?} retained={:?} \
             block(top,height)={block_geometry:?} visual(before,after,zero_before,zero_after,first,last)={visual:?} \
             layout(leading,trailing,first,last)={layout:?}\n",
            view.scroll_y,
            measured_row(view, window, info_last),
            measured_row(view, window, first_code),
            view.pending_caret_visibility_after_layout,
            view.pending_center_caret_frames,
            view.pending_find_visibility_after_layout,
            view.pending_zoom_anchor.is_some(),
            view.pending_mode_anchor.is_some(),
            view.center_window_line,
            view.retained_center_window,
        )
    }

    #[gpui::test]
    fn a_caret_after_a_mode_round_trip_collapses_the_interior_of_the_last_selection(
        cx: &mut gpui::TestAppContext,
    ) {
        let source: String = (0..60).map(|n| format!("```\ncode{n:02}\n```\n\n")).collect();
        let at = |n: u32| source.find(&format!("code{n:02}")).unwrap();
        let (view, cx) = open_view(cx, &source, 800.0, 400.0);
        let select = |view: &TestView, cx: &mut TestCx, anchor: usize, active: usize| {
            view.update(cx, |view, cx| {
                view.editor_mut()
                    .set_selection(Selection {
                        anchor: SourceOffset(anchor),
                        active: SourceOffset(active),
                    })
                    .unwrap();
                view.after_input(cx);
            });
        };
        // A: a non-empty selection whose background job is still running.
        select(&view, cx, at(5), at(25));
        cx.run_until_parked();
        assert!(view.read_with(cx, |view, _| view.document_parse_job_running));
        // Reading, then a different non-empty selection B, then editing: the
        // rebuild installs the heights of B and forgets the background basis.
        assert!(toggle(&view, cx));
        cx.run_until_parked();
        select(&view, cx, at(30), at(50));
        cx.run_until_parked();
        assert!(toggle(&view, cx));
        cx.run_until_parked();
        // Before the old job finishes the selection collapses to a caret C
        // outside B.
        select(&view, cx, at(5), at(5));
        cx.run_until_parked();
        settle(cx);
        settle(cx);
        settle(cx);
        view.read_with(cx, |view, _| {
            let document = view.editor().document();
            let index = view.current_index().expect("formal index");
            let closed = block_heights_with_disclosure(
                document,
                index,
                view.line_height(),
                view.active_height_disclosure(),
            );
            let still_open = block_heights_with_disclosure(
                document,
                index,
                view.line_height(),
                Some(SourceRange::new(at(30), at(50))),
            );
            for n in 32..48 {
                let ordinal = index.ordinal_at(SourceOffset(at(n))).expect("fence block");
                assert!(
                    still_open[ordinal] > closed[ordinal],
                    "fence {n} is open under B and closed under the caret"
                );
                let height = view.heights.height(ordinal).unwrap();
                assert!(
                    (height - closed[ordinal]).abs() < 0.01,
                    "off-screen fence {n} inside the old selection is closed: {height} vs {} (open {})",
                    closed[ordinal],
                    still_open[ordinal]
                );
            }
        });
    }

    #[gpui::test]
    fn the_mode_anchor_accepts_a_layout_drawn_after_an_edit_to_another_block(
        cx: &mut gpui::TestAppContext,
    ) {
        let long = "word ".repeat(600);
        // A guard paragraph sits between the edited block and the target: the
        // index re-parses only the dirty block and its direct neighbours
        // synchronously, so the target keeps its old parse revision.
        let source = format!("short\n\nguard\n\n{long}\n\nend\n");
        let (view, cx) = open_view(cx, &source, 400.0, 500.0);
        let target_offset = SourceOffset(source.find("word").unwrap() + 1);
        let (target_id, target_ordinal, parsed_at) = view.read_with(cx, |view, _| {
            let index = view.current_index().expect("the formal index is published");
            let block = index.block_at(target_offset).expect("the long paragraph");
            (block.id, block.ordinal, block.revision)
        });
        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset(0)))
                .unwrap();
            view.insert_text("x", cx);
        });
        // The formal timer is not advanced: only foreground work runs.
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            let index = view.current_index().expect("the index is current after the edit");
            let block = index.block_at(SourceOffset(target_offset.0 + 1)).expect("the long paragraph");
            assert_eq!(block.id, target_id, "the target block keeps its identity");
            assert_eq!(
                block.revision, parsed_at,
                "the target block was not re-parsed by the edit to the other block"
            );
            assert_ne!(
                block.revision,
                view.editor().document().revision(),
                "the block was parsed before the edit to the other block"
            );
        });
        // Scroll into the target with the ordinary scroll-only path.
        let mut pages = 0;
        loop {
            let reached = view.read_with(cx, |view, _| {
                view.scroll_y
                    >= view.heights.prefix_sum(target_ordinal) + view.viewport_height * 1.5
            });
            if reached {
                break;
            }
            pages += 1;
            assert!(pages <= 60, "the target paragraph was not reached by paging");
            view.update(cx, |view, cx| view.scroll_page(true, cx));
            cx.run_until_parked();
        }
        let ordinal = target_ordinal;
        view.read_with(cx, |view, _| {
            let document = view.editor().document();
            let block = view
                .current_index()
                .and_then(|index| index.block(ordinal))
                .expect("the long paragraph");
            assert_eq!(block.id, target_id);
            assert_ne!(
                block.revision,
                document.revision(),
                "the block is still on its old parse revision when drawn"
            );
            let entry = view
                .layout_cache
                .get(&block.id)
                .expect("the long paragraph is drawn");
            assert!(
                entry.is_valid(
                    view.content_width,
                    view.layout_font_revision,
                    document.revision()
                ),
                "its layout is valid for the current document revision"
            );
            let anchor = view
                .capture_mode_anchor()
                .expect("an anchor is captured");
            assert!(
                anchor.offset > block.source_range.start,
                "the anchor names a drawn row inside the paragraph, not its block start \
                 ({:?} vs {:?})",
                anchor.offset,
                block.source_range.start
            );
        });
    }

    fn plain_paragraphs() -> String {
        (0..200).map(|n| format!("paragraph {n:03}\n\n")).collect()
    }

    /// Opens a long plain document, scrolls into the middle, and switches the
    /// mode so an anchor is pending. `then` runs in the same event cycle,
    /// before any render.
    fn switch_then_scroll_in_the_same_cycle(
        cx: &mut gpui::TestAppContext,
        then: impl FnOnce(&mut EditorView, &mut Context<EditorView>),
        check_scroll: impl FnOnce(f32, f32),
    ) {
        let source = plain_paragraphs();
        let (view, cx) = open_view(cx, &source, 800.0, 400.0);
        let middle = source.find("paragraph 120").unwrap();
        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset(middle)))
                .unwrap();
            view.scroll_y = 2_000.0;
            cx.notify();
        });
        settle(cx);
        let selection = view.read_with(cx, |view, _| view.editor().selection());
        let scroll_before = view.read_with(cx, |view, _| view.scroll_y);
        assert!(scroll_before > 1_000.0);

        let mark = requested_frames(&view, cx);
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                assert!(view.toggle_editor_mode(window, cx));
                assert!(view.pending_mode_anchor.is_some());
                then(view, cx);
                assert!(
                    view.pending_mode_anchor.is_none(),
                    "the later explicit scroll owns the position"
                );
            });
        });
        settle(cx);
        deliver_requested_frames(&view, cx, mark, 8);
        view.read_with(cx, |view, _| {
            assert!(view.pending_mode_anchor.is_none());
            assert_eq!(view.editor().selection(), selection);
            check_scroll(view.scroll_y, scroll_before);
        });
    }

    #[gpui::test]
    fn scroll_to_start_in_the_cycle_of_a_switch_is_not_undone_by_the_old_anchor(
        cx: &mut gpui::TestAppContext,
    ) {
        switch_then_scroll_in_the_same_cycle(
            cx,
            |view, cx| view.scroll_to_document_edge(false, cx),
            |scroll_y, _| assert!(scroll_y.abs() < 0.5, "scroll stays at the start: {scroll_y}"),
        );
    }

    #[gpui::test]
    fn a_wheel_event_in_the_cycle_of_a_switch_is_not_undone_by_the_old_anchor(
        cx: &mut gpui::TestAppContext,
    ) {
        switch_then_scroll_in_the_same_cycle(
            cx,
            |view, cx| {
                let event = gpui::ScrollWheelEvent {
                    position: gpui::point(px(100.0), px(100.0)),
                    delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.0), px(-600.0))),
                    modifiers: gpui::Modifiers::none(),
                    touch_phase: gpui::TouchPhase::Moved,
                };
                view.on_scroll_at(&event, std::time::Instant::now(), cx);
            },
            |scroll_y, before| {
                assert!(
                    scroll_y >= before + 500.0,
                    "the wheel's position is kept: {scroll_y} vs {before}"
                )
            },
        );
    }

    fn wrapped_paragraphs() -> String {
        (0..60)
            .map(|n| format!("paragraph {n:02} {}\n\n", "wrapped words ".repeat(40)))
            .collect()
    }

    /// Switches the mode with a pending anchor, delivers `events` before any
    /// render, and checks that the anchor survives them, is resolved by the
    /// real renders, and the captured source row stays at the viewport top.
    fn switch_then_deliver_non_owning_events(
        cx: &mut gpui::TestAppContext,
        start_in_reading: bool,
        events: impl FnOnce(&mut EditorView, &mut Window, &mut Context<EditorView>),
    ) {
        let source = wrapped_paragraphs();
        let (view, cx) = open_view(cx, &source, 600.0, 400.0);
        if start_in_reading {
            assert!(toggle(&view, cx));
            settle(cx);
        }
        let middle = source.find("paragraph 30").unwrap();
        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset(middle)))
                .unwrap();
            view.scroll_y = view.heights.prefix_sum(30) + view.line_height() * 3.0;
            cx.notify();
        });
        settle(cx);
        let selection = view.read_with(cx, |view, _| view.editor().selection());
        let before = snapshot(&view, cx);
        let (anchor, scroll_before) = view.read_with(cx, |view, _| {
            (view.capture_mode_anchor().expect("an anchor is captured"), view.scroll_y)
        });
        assert!(scroll_before > 1_000.0);

        let mark = requested_frames(&view, cx);
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                assert!(view.toggle_editor_mode(window, cx));
                assert!(view.pending_mode_anchor.is_some());
                events(view, window, cx);
                assert!(
                    view.pending_mode_anchor.is_some(),
                    "events that move nothing do not take the position"
                );
            });
        });
        settle(cx);
        deliver_requested_frames(&view, cx, mark, 8);
        let (row, scroll_y, line_height, pending, selection_after) = cx.update(|window, app| {
            let view = view.read(app);
            (
                measured_row(view, window, anchor.offset),
                view.scroll_y,
                view.line_height(),
                view.pending_mode_anchor.is_some(),
                view.editor().selection(),
            )
        });
        assert!(!pending, "the mode anchor was resolved");
        let row = row.unwrap_or_else(|error| panic!("the source row is not measured: {error}"));
        assert!(
            scroll_y >= row.top - line_height * 0.5
                && scroll_y <= row.top + row.height + line_height * 0.5,
            "the source row stays at the viewport top: scroll_y={scroll_y} row=({}, {})",
            row.top,
            row.height
        );
        assert_eq!(selection_after, selection);
        assert_eq!(snapshot(&view, cx), before, "the body is never edited");
    }

    fn no_op_scroll_events(view: &mut EditorView, cx: &mut Context<EditorView>) {
        let at = gpui::point(px(100.0), px(100.0));
        let scroll = |delta, modifiers, touch_phase| gpui::ScrollWheelEvent {
            position: at,
            delta,
            modifiers,
            touch_phase,
        };
        let secondary = gpui::Modifiers::secondary_key();
        let zero_pixels = gpui::ScrollDelta::Pixels(gpui::point(px(0.0), px(0.0)));
        let zero_lines = gpui::ScrollDelta::Lines(gpui::point(0.0, 0.0));
        let horizontal_only = gpui::ScrollDelta::Pixels(gpui::point(px(40.0), px(0.0)));
        for event in [
            scroll(zero_pixels, gpui::Modifiers::none(), gpui::TouchPhase::Ended),
            scroll(zero_pixels, gpui::Modifiers::none(), gpui::TouchPhase::Started),
            scroll(zero_lines, gpui::Modifiers::none(), gpui::TouchPhase::Moved),
            scroll(horizontal_only, gpui::Modifiers::none(), gpui::TouchPhase::Moved),
            scroll(zero_pixels, secondary, gpui::TouchPhase::Ended),
            scroll(zero_lines, secondary, gpui::TouchPhase::Moved),
            scroll(horizontal_only, secondary, gpui::TouchPhase::Moved),
        ] {
            view.on_scroll_at(&event, std::time::Instant::now(), cx);
        }
    }

    #[gpui::test]
    fn zero_movement_wheel_events_keep_the_editing_to_reading_anchor(
        cx: &mut gpui::TestAppContext,
    ) {
        switch_then_deliver_non_owning_events(cx, false, |view, _, cx| no_op_scroll_events(view, cx));
    }

    #[gpui::test]
    fn zero_movement_wheel_events_keep_the_reading_to_editing_anchor(
        cx: &mut gpui::TestAppContext,
    ) {
        switch_then_deliver_non_owning_events(cx, true, |view, _, cx| no_op_scroll_events(view, cx));
    }

    /// Content-local pointer offset (below the viewport top) used by the pinch
    /// tests.
    const POINTER_OFFSET: f32 = 200.0;

    fn pinch_event(view: &TestView, cx: &mut TestCx, delta: f32) -> gpui::PinchEvent {
        let header = view.read_with(cx, |view, _| {
            view.theme.header_height + view.document_find_reserved_height()
        });
        gpui::PinchEvent {
            position: gpui::point(px(100.0), px(header + POINTER_OFFSET)),
            delta,
            modifiers: gpui::Modifiers::none(),
            phase: gpui::TouchPhase::Moved,
        }
    }

    #[gpui::test]
    fn a_pinch_in_the_cycle_of_a_switch_keeps_the_pointer_position_not_the_old_anchor(
        cx: &mut gpui::TestAppContext,
    ) {
        let source = plain_paragraphs();
        let (view, cx) = open_view(cx, &source, 800.0, 400.0);
        view.update(cx, |view, cx| {
            view.scroll_y = 2_000.0;
            cx.notify();
        });
        settle(cx);
        let selection = view.read_with(cx, |view, _| view.editor().selection());
        let before = snapshot(&view, cx);
        // The document position under the pointer, and the one at the top edge
        // the mode switch would restore.
        let (pointer, top, zoom_before) = view.read_with(cx, |view, _| {
            (
                view.zoom_anchor_at(POINTER_OFFSET).expect("laid out"),
                view.zoom_anchor_at(0.0).expect("laid out"),
                view.zoom,
            )
        });
        assert!(
            pointer.ordinal > top.ordinal,
            "the pointer is over a different paragraph than the top edge"
        );

        let event = pinch_event(&view, cx, 0.2);
        let mark = requested_frames(&view, cx);
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                assert!(view.toggle_editor_mode(window, cx));
                assert!(view.pending_mode_anchor.is_some());
                view.on_pinch(&event, window, cx);
                assert!(view.zoom != zoom_before, "the pinch changes the effective zoom");
                assert!(view.pending_zoom_anchor.is_some(), "the new zoom intent is armed");
                assert!(
                    view.pending_mode_anchor.is_none(),
                    "the later explicit zoom owns the position"
                );
            });
        });
        settle(cx);
        deliver_requested_frames(&view, cx, mark, 8);
        view.read_with(cx, |view, _| {
            assert!(view.pending_mode_anchor.is_none());
            assert!(view.pending_zoom_anchor.is_none(), "the zoom anchor was consumed");
            let after = view.zoom_anchor_at(POINTER_OFFSET).expect("laid out");
            assert_eq!(
                after.ordinal, pointer.ordinal,
                "the paragraph under the pointer stays under it"
            );
            assert!(
                (after.fraction - pointer.fraction).abs() <= 0.5,
                "the position inside it is kept: {} vs {}",
                after.fraction,
                pointer.fraction
            );
            assert_eq!(view.editor().selection(), selection);
        });
        let after = snapshot(&view, cx);
        assert_eq!(after, before, "zooming never edits the body");
    }

    #[gpui::test]
    fn a_neutral_pinch_in_the_cycle_of_a_switch_keeps_the_mode_anchor(
        cx: &mut gpui::TestAppContext,
    ) {
        let source = plain_paragraphs();
        let (view, cx) = open_view(cx, &source, 800.0, 400.0);
        view.update(cx, |view, cx| {
            view.scroll_y = 2_000.0;
            cx.notify();
        });
        settle(cx);
        let event = pinch_event(&view, cx, 0.0);
        let mark = requested_frames(&view, cx);
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                assert!(view.toggle_editor_mode(window, cx));
                view.on_pinch(&event, window, cx);
                assert!(
                    view.pending_mode_anchor.is_some(),
                    "a pinch that changes no zoom does not take the position"
                );
            });
        });
        settle(cx);
        deliver_requested_frames(&view, cx, mark, 8);
        view.read_with(cx, |view, _| assert!(view.pending_mode_anchor.is_none()));
    }

    #[gpui::test]
    fn page_down_in_the_cycle_of_a_switch_is_not_undone_by_the_old_anchor(
        cx: &mut gpui::TestAppContext,
    ) {
        switch_then_scroll_in_the_same_cycle(
            cx,
            |view, cx| view.scroll_page(true, cx),
            |scroll_y, before| {
                assert!(
                    scroll_y > before + 100.0,
                    "the page move's position is kept: {scroll_y} vs {before}"
                )
            },
        );
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

    /// Hovers the switch in a window without a `gpui_component::Root`, as the
    /// app has none, and returns the tooltip texts that were painted.
    fn hover_switch_tooltips(cx: &mut TestCx) -> Vec<&'static str> {
        let point = cx
            .debug_bounds("reading-mode-toggle")
            .expect("the switch is painted")
            .center();
        cx.simulate_mouse_move(point, None, gpui::Modifiers::none());
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(1500));
        cx.run_until_parked();
        // `debug_bounds` takes a `&'static str`, so each selector is a literal
        // that spells `reading-mode-toggle-tooltip:{text}`.
        let candidates: [(&'static str, &'static str); 3] = [
            (
                "閲覧モードに切り替え",
                "reading-mode-toggle-tooltip:閲覧モードに切り替え",
            ),
            (
                "編集モードに切り替え",
                "reading-mode-toggle-tooltip:編集モードに切り替え",
            ),
            (
                MODE_SWITCH_BLOCKED_BY_COMPOSITION,
                "reading-mode-toggle-tooltip:文字の変換を確定または取り消してから切り替えてください",
            ),
        ];
        candidates
            .into_iter()
            .filter(|(_, selector)| cx.debug_bounds(selector).is_some())
            .map(|(text, _)| text)
            .collect()
    }

    #[gpui::test]
    fn hovering_the_switch_shows_its_tooltip_without_a_root(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "hello\n", 800.0, 400.0);
        assert_eq!(hover_switch_tooltips(cx), ["閲覧モードに切り替え"]);
        // The hover alone does not switch the mode.
        assert_eq!(
            view.read_with(cx, |view, _| view.editor_mode()),
            EditorMode::Editing
        );
        cx.simulate_mouse_move(point(px(700.0), px(300.0)), None, gpui::Modifiers::none());
        settle(cx);

        assert!(toggle(&view, cx));
        settle(cx);
        assert_eq!(hover_switch_tooltips(cx), ["編集モードに切り替え"]);
        cx.simulate_mouse_move(point(px(700.0), px(300.0)), None, gpui::Modifiers::none());
        settle(cx);
        assert!(toggle(&view, cx));
        settle(cx);

        // During a body composition the reason is shown and hovering neither
        // switches nor ends the composition.
        view.update(cx, |view, _| {
            view.editor_mut()
                .replace_and_mark_text(None, "あ", Some(1..1))
                .unwrap();
        });
        settle(cx);
        assert_eq!(
            hover_switch_tooltips(cx),
            [MODE_SWITCH_BLOCKED_BY_COMPOSITION]
        );
        view.read_with(cx, |view, _| {
            assert_eq!(view.editor_mode(), EditorMode::Editing);
            assert!(view.editor().ime().is_some(), "the marked text is kept");
        });
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
