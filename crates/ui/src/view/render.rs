use super::*;

impl Render for EditorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_external_open_guard();
        // Erase the opaque type so the element no longer borrows `self`.
        let element = self.render_frame(window, cx).into_any_element();
        // `editor_mut` raises the external-open guard even for access that does
        // not edit (layout, selection, scroll). Without this second sync a clean
        // session would keep the guard raised until the next frame, and a Finder
        // request received in between would be stamped protected and refused.
        self.sync_external_open_guard();
        element
    }
}

impl EditorView {
    fn render_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let layout_started = Instant::now();
        if self._input_mode_focus_subscription.is_none() {
            let focus_handle = self.focus_handle.clone();
            self._input_mode_focus_subscription =
                Some(cx.on_focus(&focus_handle, window, |view, _, cx| {
                    if update_keyboard_input_mode(
                        &mut view.caret_input_mode,
                        active_keyboard_input_mode(),
                    ) {
                        cx.notify();
                    }
                }));
        }
        let resolved_theme = resolve_theme(self.settings.theme, window.appearance());
        if resolved_theme != self.theme {
            self.theme = resolved_theme;
            sync_component_theme(self.settings.theme, window.appearance(), window, cx);
            self.block_cache.clear();
            self.layout_cache.clear();
            let (granularity, _) = self.desired_layout();
            let heights = HeightIndex::new(self.item_heights());
            self.install_heights(granularity, heights);
        }
        self.step_wheel_zoom_animation(window);
        self.step_scroll_inertia(window);
        // Whether a live coast is still eligible to resync onto the
        // height-anchor recompute below instead of reading as stale on the
        // next frame (issue #389). Tracked separately from
        // `self.scroll_inertia` so the render-entry clamp further down can
        // disqualify it: if that clamp itself moves `scroll_y`, this is a
        // genuine external change (e.g. a viewport shrink), not the
        // height-anchor's own correction, and the coast must still stop.
        let mut scroll_inertia_tracks_layout = self
            .scroll_inertia
            .is_some_and(|inertia| inertia.last_applied == self.scroll_y);
        if self.settings_open {
            return self
                .settings_screen_element(window, cx)
                .on_drop(cx.listener(Self::on_external_paths_drop));
        }
        // Discards a context menu or close-confirmation left over for a tab
        // that closed through some other route while it was still showing,
        // or whose id now names a different document instance (a
        // work-folder switch reusing the id, or an in-place reopen), rather
        // than acting on (or displaying stale information about) a document
        // the action was never taken against.
        if self
            .file_tab_context_menu
            .as_ref()
            .is_some_and(|menu| self.document_instance(menu.id) != Some(menu.instance))
        {
            self.file_tab_context_menu = None;
        }
        if self
            .tab_close_confirm
            .as_ref()
            .is_some_and(|confirm| self.document_instance(confirm.id) != Some(confirm.instance))
        {
            self.tab_close_confirm = None;
        }
        // The body menu belongs to one document instance, revision and
        // selection, and is never shown together with another menu or a
        // modal. Anything that changed since it opened closes it.
        if self.editor_context_menu.as_ref().is_some_and(|menu| {
            !self.editor_context_menu_is_current(menu)
                || self.file_tab_context_menu.is_some()
                || self.sidebar_context_menu.is_some()
        }) {
            self.editor_context_menu = None;
        }
        self.begin_mermaid_frame(window, cx);
        self.schedule_document_parse(cx);
        self.viewport_height = (f32::from(window.viewport_size().height)
            - self.theme.header_height
            - self.theme.footer_height
            - self.document_find_reserved_height())
        .max(self.line_height());
        self.step_measurement_scroll(window);
        // The width of the text column decides where every row breaks, so it is
        // read once per frame and every layout is keyed by it. A sidebar takes
        // its width out of the same window, so it must be subtracted here too,
        // not just in the element tree, or wrapping would be computed for a
        // column wider than what is actually drawn.
        let sidebar_visible = self.sidebar_shown();
        let sidebar_width = if sidebar_visible {
            self.sidebar_width + SIDEBAR_RESIZER_WIDTH
        } else {
            0.0
        };
        self.main_column_left = sidebar_width;
        self.content_width = text_column_width(
            f32::from(window.viewport_size().width),
            sidebar_width,
            self.theme.line_horizontal_padding,
        );
        let shaper = WindowShaper::new(window, self.zoom);
        let font_revision = shaper.font_revision();
        if font_revision != self.layout_font_revision {
            // `VisualLine::estimated_height` is part of the presentation, not
            // just the shaped layout. Reusing cached presentation/layout rows
            // across zoom generations can clip or overlap text. During wheel
            // interpolation the document-wide height estimates are deliberately
            // retained until the settled frame; visible entries are remeasured
            // below using the new zoom.
            self.invalidate_layout_font_revision(font_revision);
        }
        self.scroll_y = clamp_scroll_y(
            self.scroll_y,
            self.scrollable_content_height(),
            self.viewport_height,
        );
        scroll_inertia_tracks_layout &= self
            .scroll_inertia
            .is_some_and(|inertia| inertia.last_applied == self.scroll_y);
        let visible =
            self.heights
                .visible_range(self.scroll_y, self.viewport_height, self.theme.overscan);
        let blocks = self.visible_blocks(visible.clone());
        self.schedule_joined_parse(&blocks, cx);
        self.schedule_mermaid(&blocks, cx);
        // Keep a margin of presentations around the viewport so scrolling back
        // does not re-present, and drop the rest.
        let retained = self
            .current_index()
            .map(|index| {
                (visible.start.saturating_sub(BLOCK_CACHE_MARGIN)..visible.end + BLOCK_CACHE_MARGIN)
                    .filter_map(|ordinal| index.block(ordinal))
                    .map(|block| block.id)
                    .collect::<HashSet<_>>()
            })
            .unwrap_or_else(|| blocks.iter().map(|block| block.id).collect());
        self.block_cache.retain(|id, _| retained.contains(id));
        // Diagrams follow the same window: a render for a block that left it is
        // stopped and its picture handed back to the window.
        self.mermaid.retain_blocks(&retained);

        self.layout_cache.retain(|id, _| retained.contains(id));
        self.joined_parse_cache
            .retain(|id, _| retained.contains(id));

        let lines = self.visible_line_window(&blocks, &visible);
        self.follow_retained_center_window(&blocks);
        let mut rendered = Vec::with_capacity(blocks.len());
        for block in &blocks {
            let window = self.render_window_for(block, &lines);
            if let Some((visual, reused)) = self.cached_block(block, &window) {
                let layout = self.block_layout(&visual, reused, &shaper);
                rendered.push((block.ordinal, visual, layout));
            }
        }
        // Height entries follow whatever the index is keyed by: one per block
        // once the document has been parsed, one per physical line until then.
        // Either way the height is the laid-out one, so a wrapped line takes the
        // room its rows actually need.
        let mut first_item = usize::MAX;
        let mut last_item = 0;
        let height_anchor = (self.granularity == Granularity::Blocks && !self.heights.is_empty())
            .then(|| {
                let ordinal = self.heights.block_at_y(self.scroll_y);
                (ordinal, self.scroll_y - self.heights.prefix_sum(ordinal))
            });
        self.line_owners.clear();
        for (ordinal, visual, layout) in &rendered {
            match self.granularity {
                Granularity::Blocks => {
                    if *ordinal < self.heights.len() {
                        self.heights.update(*ordinal, layout.height());
                    }
                    first_item = first_item.min(*ordinal);
                    last_item = last_item.max(ordinal + 1);
                }
                Granularity::Lines => {
                    for (at, line) in visual.lines.iter().enumerate() {
                        let line_id = line.line_id as usize;
                        if line_id < self.heights.len() {
                            self.heights.update(line_id, layout.line_height_of(at));
                        }
                        first_item = first_item.min(line_id);
                        last_item = last_item.max(line_id + 1);
                    }
                }
            }
            for (at, line) in visual.lines.iter().enumerate() {
                self.line_owners
                    .insert(line.line_id as usize, (visual.id, at));
            }
        }
        // A zoom gesture is anchored after these visible blocks have been
        // remeasured. Resolving it earlier against estimated heights is wrong
        // for a soft-wrapped paragraph: the measured block height can differ by
        // several rows in the same frame. The next frame is notified below if
        // the corrected anchor changed which blocks should be visible.
        // A mode switch is anchored on the source row instead (refused for any
        // other document), against the layouts measured above.
        let mode_anchor_applied = self.resolve_mode_anchor(&rendered, window, cx);
        let zoom_anchor_applied = self
            .pending_zoom_anchor
            .take()
            .filter(|_| !self.heights.is_empty() && !mode_anchor_applied)
            .map(|anchor| {
                self.scroll_y = self.scroll_y_for_zoom_anchor(anchor);
                true
            })
            .unwrap_or(false)
            || mode_anchor_applied;
        if zoom_anchor_applied {
            // The visible block set was selected before measured heights were
            // installed. Ask GPUI for one more frame so virtualization and
            // spacers are recomputed from the corrected anchor position.
            self.request_layout_followup_frame(window, cx);
        }
        // Measuring wrapped rows can correct blocks above the viewport. Keep the
        // same block and the same position inside it at the top instead of
        // letting those corrections visibly move the document, unless the
        // zoom gesture has a more specific pointer/pinch anchor.
        if !zoom_anchor_applied {
            self.apply_height_anchor(height_anchor, scroll_inertia_tracks_layout);
        }
        // A newly measured block can shrink at the old bottom. Anchoring
        // preserves its block-relative position, which can now sit below the
        // new scroll limit and move all content above the viewport.
        self.scroll_y = clamp_scroll_y(
            self.scroll_y,
            self.scrollable_content_height(),
            self.viewport_height,
        );
        // Resolve the caret from the layouts produced in this frame, not from
        // a cache that may still describe the pre-disclosure zero-height row.
        // A fence can become editable one frame after the input event; keep the
        // request armed until that row has a real positive height.
        let caret = self.editor().selection().active;
        let fresh_caret = self.fresh_point_for_offset(&rendered, &shaper, caret);
        if self.pending_caret_visibility_after_layout {
            // Reading keeps fences and table delimiters at zero height for
            // good, so a caret on one never gets a positive-height row: it
            // resolves to the nearest visible position instead of waiting.
            // Nothing paints a caret badge there, so no clearance is asked.
            let badge_height = if self.body_caret_visible() {
                CARET_MODE_BADGE_HEIGHT
            } else {
                0.0
            };
            let caret_row = match fresh_caret {
                Some((_, top, height)) if height > 0.0 => Some((top, height)),
                _ if !self.body_editable() => self.reading_visible_row(caret),
                _ => None,
            };
            if let Some((top, height)) = caret_row {
                let before = self.scroll_y;
                self.scroll_y = scroll_y_for_cursor(
                    self.scroll_y,
                    top,
                    height + badge_height,
                    self.viewport_height,
                );
                self.scroll_y = clamp_scroll_y(
                    self.scroll_y,
                    self.scrollable_content_height(),
                    self.viewport_height,
                );
                let visible_bottom = top + height + badge_height - self.scroll_y;
                let settled = visible_bottom <= self.viewport_height + CARET_VISIBILITY_TOLERANCE;
                if settled && self.scroll_y == before {
                    self.pending_caret_visibility_after_layout = false;
                } else if self.scroll_y == before && self.body_editable() {
                    // Editing keeps the established behaviour (no new frame
                    // is requested for an unmoved, unsettled request).
                    cx.notify();
                } else if self.scroll_y != before {
                    // Keep the request armed for one more frame whenever this
                    // correction moved the scroll. A following frame may apply
                    // the height anchor to newly measured rows; clearing here
                    // would let that anchor erase the badge clearance before
                    // the caret has reached a stable layout. The visible block
                    // set was also selected before the corrected position, so
                    // the next frame lets virtualization follow it. `render`
                    // cannot dirty the frame it is drawing, so an actual frame
                    // is requested.
                    self.request_layout_followup_frame(window, cx);
                } else {
                    // Reading, and the scroll can not move any further (the
                    // end of the document): the request has nothing left to
                    // improve, so it ends instead of asking for frames forever.
                    self.pending_caret_visibility_after_layout = false;
                }
            } else {
                // The fresh disclosed row has not been laid out yet. Keep the
                // request alive for the next frame instead of consuming it
                // against stale zero-height geometry.
                cx.notify();
            }
        }
        if self.pending_center_caret_frames > 0 {
            // Re-centre on the drawn row. A row that was not laid out this
            // frame keeps the request alive for a bounded number of frames.
            self.pending_center_caret_frames -= 1;
            if self.scroll_inertia.is_some() {
                self.cancel_center_caret();
            } else if let Some((_, top, height)) = fresh_caret
                && height > 0.0
            {
                let desired = clamp_scroll_y(
                    top + height / 2.0 - self.viewport_height / 2.0,
                    self.scrollable_content_height(),
                    self.viewport_height,
                );
                if (desired - self.scroll_y).abs() > CARET_VISIBILITY_TOLERANCE {
                    self.scroll_y = desired;
                    cx.notify();
                } else {
                    self.pending_center_caret_frames = 0;
                }
            } else {
                // The caret row is not in this frame's drawn blocks (the first
                // scroll target came from estimated heights). Aim at the
                // height index's current answer so the next frame draws the
                // block holding it; the pinned window then draws the row.
                if let Some((top, height)) = self.source_offset_row_bounds(caret) {
                    let desired = clamp_scroll_y(
                        top + height / 2.0 - self.viewport_height / 2.0,
                        self.scrollable_content_height(),
                        self.viewport_height,
                    );
                    if (desired - self.scroll_y).abs() > CARET_VISIBILITY_TOLERANCE {
                        self.scroll_y = desired;
                    }
                }
                cx.notify();
            }
        }
        let find_current_match = self.document_find_current_match();
        if self.pending_find_visibility_after_layout {
            let fresh_match = find_current_match
                .and_then(|range| self.fresh_point_for_offset(&rendered, &shaper, range.start));
            // While reading, a match on a closed fence or delimiter row has no
            // height to wait for; show the nearest visible position.
            let fresh_match = match (fresh_match, find_current_match) {
                (Some((_, top, height)), _) if height > 0.0 => Some((0.0, top, height)),
                (_, Some(range)) if !self.body_editable() => self
                    .reading_visible_row(range.start)
                    .map(|(top, height)| (0.0, top, height)),
                (Some(_), _) => Some((0.0, 0.0, 0.0)),
                (None, _) => None,
            };
            match fresh_match {
                Some((_, top, height)) if height > 0.0 => {
                    let before = self.scroll_y;
                    self.scroll_y =
                        scroll_y_for_cursor(self.scroll_y, top, height, self.viewport_height);
                    self.scroll_y = clamp_scroll_y(
                        self.scroll_y,
                        self.scrollable_content_height(),
                        self.viewport_height,
                    );
                    let visible_bottom = top + height - self.scroll_y;
                    let settled = visible_bottom <= self.viewport_height + CARET_VISIBILITY_TOLERANCE;
                    if settled && self.scroll_y == before {
                        self.pending_find_visibility_after_layout = false;
                    } else if self.scroll_y == before && self.body_editable() {
                        cx.notify();
                    } else if self.scroll_y != before {
                        self.request_layout_followup_frame(window, cx);
                    } else {
                        // Reading at a scroll limit: nothing more to correct.
                        self.pending_find_visibility_after_layout = false;
                    }
                }
                Some(_) => {
                    // The current match's row has not been laid out with a
                    // real height yet (its block may still need the forced
                    // disclosure this frame installs). Keep the request
                    // alive instead of consuming it early.
                    cx.notify();
                }
                None => {
                    // No current match (the bar closed, or navigated away
                    // from): nothing left to scroll to.
                    self.pending_find_visibility_after_layout = false;
                }
            }
        }
        let find_matches = self.document_find_matches_in_view(SourceRange {
            start: rendered
                .iter()
                .map(|(_, visual, _)| visual.source_range.start)
                .min()
                .unwrap_or(SourceOffset(0)),
            end: rendered
                .iter()
                .map(|(_, visual, _)| visual.source_range.end)
                .max()
                .unwrap_or(SourceOffset(0)),
        });
        // Where the caret was drawn, for the IME candidate window.
        self.caret_geometry = fresh_caret
            .filter(|_| self.body_caret_visible())
            .map(|(x, top, height)| CaretGeometry {
                x: self.theme.line_horizontal_padding + x,
                y: top - self.scroll_y,
                height,
            });
        // Blocks are drawn whole, so the rendered span can start above the
        // viewport; the spacers have to match what was actually drawn.
        let items = if first_item < last_item {
            first_item..last_item
        } else {
            visible.clone()
        };
        let top_space = self.heights.prefix_sum(items.start);
        let bottom_space =
            (self.heights.total_height() - self.heights.prefix_sum(items.end)).max(0.0);
        let root = div()
            .size_full()
            .flex()
            .flex_row()
            .bg(rgb(self.theme.editor_background))
            .text_color(rgb(self.theme.foreground))
            .key_context("HaneEditor")
            .track_focus(&self.focus_handle(cx));
        let sidebar_viewport_height = f32::from(window.viewport_size().height);
        let sidebar = self.work_folder_sidebar(sidebar_viewport_height, cx);
        let resizer = if sidebar_visible {
            Some(self.sidebar_resizer(cx))
        } else {
            None
        };
        let root = install_action_listeners(root, cx)
            .on_action(cx.listener(EditorView::open_ai_selection_action))
            .children(sidebar)
            .children(resizer)
            .on_mouse_move(cx.listener(Self::on_panel_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_panel_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::finish_panel_drag))
            // The drop listener stops the mouse-up it rides on, so a drop is
            // never read as a body click or a selection.
            .on_drop(cx.listener(Self::on_external_paths_drop));
        let main_column = div()
            .flex_1()
            .min_w(px(0.0))
            .min_h(px(0.0))
            .flex()
            .flex_col()
            .child(self.header_element(cx))
            // A fixed-height flex sibling directly under the tab bar, inside
            // the body area, rather than an absolute overlay covering the
            // document's first row: `self.viewport_height` and every
            // window-to-content y conversion below account for this row's
            // height via `document_find_reserved_height` (Issue #413).
            .children(self.document_find_bar(cx));
        // Relative image destinations resolve against the session's own file,
        // never against the directory the process happens to run in.
        let resolver = self.sessions.active().resource_resolver();
        let editor = self.sessions.active().editor();
        let editor_scrollbar = self.editor_scrollbar(self.scrollable_content_height(), cx);
        let main_column = main_column.child(
            div()
                .relative()
                .flex_1()
                .min_h(px(0.0))
                .overflow_hidden()
                .on_mouse_down(MouseButton::Left, cx.listener(Self::on_editor_mouse_down))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(Self::on_editor_right_mouse_down),
                )
                .on_scroll_wheel(cx.listener(Self::on_scroll))
                .on_pinch(cx.listener(Self::on_pinch))
                .child(InputCapture { input: cx.entity() })
                .child(
                    div()
                        .absolute()
                        .top(px(content_top_for_scroll(self.scroll_y)))
                        .flex()
                        .flex_col()
                        .w_full()
                        .child(div().h(px(top_space)))
                        // One element per Markdown block, and inside it one per
                        // row: what is generated scales with visible blocks, and
                        // within a block with the rows that reach the viewport.
                        .children(rendered.into_iter().map(|(_, visual, layout)| {
                            let diagram_paint = self.diagram_paint(&visual);
                            let note = self.mermaid_note_element(&visual);
                            block_element(
                                &layout,
                                (0..layout.lines.len()).map(|row_index| {
                                    let row = &layout.lines[row_index];
                                    let line = row.line_id as usize;
                                    let fragment = row.line_visual_range.clone();
                                    let dragged = fragment.clone();
                                    let table_row = !row.table_cells.is_empty();
                                    row_element(
                                        editor,
                                        &visual,
                                        &layout,
                                        row_index,
                                        &shaper,
                                        self.theme,
                                        self.zoom,
                                        &resolver,
                                        self.caret_input_mode,
                                        self.body_caret_visible(),
                                        &find_matches,
                                        find_current_match,
                                        diagram_paint.as_ref(),
                                    )
                                    // Lets GPUI-event regression tests read a row's real
                                    // painted window bounds via `VisualTestContext::debug_bounds`
                                    // instead of duplicating the render-time offset math. A
                                    // no-op outside test builds.
                                    // The layout row index is dense over painted rows, so an
                                    // inactive table delimiter makes the body row at physical
                                    // line 2 become layout row 1. Keep the test/debug identity
                                    // tied to the source line for table rows; otherwise callers
                                    // cannot address the row that was actually painted through
                                    // the same physical-line identity used by hit testing.
                                    .debug_selector(move || {
                                        let debug_row = if table_row { line } else { row_index };
                                        format!("row-{line}-{debug_row}")
                                    })
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |view, event, window, cx| {
                                            view.on_row_mouse_down(
                                                line,
                                                fragment.clone(),
                                                event,
                                                window,
                                                cx,
                                            )
                                        }),
                                    )
                                    .on_mouse_move(
                                        cx.listener(move |view, event, window, cx| {
                                            view.on_row_mouse_move(
                                                line,
                                                dragged.clone(),
                                                event,
                                                window,
                                                cx,
                                            )
                                        }),
                                    )
                                }),
                            )
                            .when_some(note, |element, note| element.relative().child(note))
                        }))
                        .child(div().h(px(bottom_space))),
                )
                .children(editor_scrollbar),
        );
        let editor_column_width =
            (f32::from(window.viewport_size().width) - sidebar_width).max(0.0);
        let rendered = root.child(main_column.child(self.footer_element(editor_column_width, cx)));
        let rendered = if let Some(menu) = self.file_tab_context_menu.as_ref() {
            rendered.child(
                anchored()
                    .position(menu.position)
                    .snap_to_window()
                    .child(self.file_tab_context_menu_element(menu, cx)),
            )
        } else {
            rendered
        };
        let rendered = if let Some(menu) = self.editor_context_menu.as_ref() {
            rendered.child(
                anchored()
                    .position(menu.position)
                    .snap_to_window()
                    .child(self.editor_context_menu_element(menu, cx)),
            )
        } else {
            rendered
        };
        let rendered = if let Some(menu) = self.sidebar_context_menu.as_ref() {
            rendered.child(
                anchored()
                    .position(menu.position)
                    .snap_to_window()
                    .child(self.sidebar_context_menu_element(menu, cx)),
            )
        } else {
            rendered
        };
        let rendered = if let Some(confirm) = self.tab_close_confirm.as_ref() {
            rendered.child(self.tab_close_confirm_element(confirm, cx))
        } else {
            rendered
        };
        let rendered = if let Some(overlay) = self.ai_selection_overlay(window, cx) {
            rendered.child(overlay)
        } else {
            rendered
        };
        self.metrics.record_layout(layout_started.elapsed());
        rendered
    }
}
