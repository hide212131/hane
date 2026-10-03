use super::*;

impl EditorView {
    /// The content height scroll position is actually bounded to:
    /// `self.heights.total_height()` extended by `CARET_MODE_BADGE_HEIGHT`.
    /// Clamping to the bare content height at the document's end would put
    /// the input-mode badge drawn under the last line's caret back under the
    /// viewport's `overflow_hidden`, undoing the clearance
    /// `scroll_cursor_into_view` reserved for it (issue #240).
    pub(super) fn scrollable_content_height(&self) -> f32 {
        self.heights.total_height() + CARET_MODE_BADGE_HEIGHT
    }

    /// The item (block, or physical line before a `BlockIndex` exists) and
    /// fractional position under `window_offset` (content-local window y: the
    /// mouse/gesture position's window y minus the header height), for a zoom
    /// gesture to anchor to. `None` when there is nothing laid out yet.
    pub(super) fn zoom_anchor_at(&self, window_offset: f32) -> Option<PendingZoomAnchor> {
        if self.heights.is_empty() {
            return None;
        }
        let content_y = (self.scroll_y + window_offset).clamp(0.0, self.heights.total_height());
        let ordinal = self.heights.block_at_y(content_y);
        let top = self.heights.prefix_sum(ordinal);
        let height = self.heights.height(ordinal).unwrap_or(0.0);
        let fraction = if height > 0.0 {
            ((content_y - top) / height).clamp(0.0, 1.0)
        } else {
            0.0
        };
        Some(PendingZoomAnchor {
            ordinal,
            fraction,
            window_offset,
        })
    }

    /// Changes the zoom level, anchoring the document position under
    /// `window_offset` (see `zoom_anchor_at`) so it stays at the same window
    /// position once `render` installs the new zoom's heights.
    fn set_zoom_from_raw(&mut self, raw: f32, window_offset: f32, cx: &mut Context<Self>) {
        let raw = raw.clamp(MIN_ZOOM, MAX_ZOOM);
        self.raw_zoom = raw;
        self.wheel_zoom_animation = None;
        let next = clamp_and_snap_zoom(raw);
        if next == self.zoom {
            return;
        }
        self.pending_zoom_anchor = self.zoom_anchor_at(window_offset);
        self.zoom = next;
        cx.notify();
    }

    pub(super) fn set_zoom(&mut self, next: f32, window_offset: f32, cx: &mut Context<Self>) {
        self.set_zoom_from_raw(next, window_offset, cx);
    }

    fn apply_zoom_factor(&mut self, factor: f32, window_offset: f32, cx: &mut Context<Self>) {
        self.set_zoom_from_raw(self.raw_zoom * factor, window_offset, cx);
    }

    /// Applies a direct-manipulation zoom such as a trackpad pinch. A pinch
    /// takes ownership from any still-running wheel animation and continues
    /// from the zoom that is actually painted, not from the wheel's future
    /// target.
    pub(super) fn apply_direct_zoom_factor(
        &mut self,
        factor: f32,
        window_offset: f32,
        cx: &mut Context<Self>,
    ) {
        let interrupted_wheel = self.wheel_zoom_animation.take().is_some();
        if interrupted_wheel {
            // A pinch takes over from what is actually painted, not from the
            // wheel's future target. A neutral pinch-begin event must not snap
            // an intermediate painted value back toward 100%.
            self.raw_zoom = self.zoom;
            if factor == 1.0 {
                self.rebuild_height_estimates();
                cx.notify();
                return;
            }
        }

        let previous_zoom = self.zoom;
        self.apply_zoom_factor(factor, window_offset, cx);
        if interrupted_wheel && self.zoom == previous_zoom {
            // The direct delta can land inside the 100% snap band and leave
            // the effective zoom unchanged. There will then be no font
            // revision on the next frame to finalize the document-wide height
            // index that interpolation intentionally kept stale offscreen.
            self.rebuild_height_estimates();
            cx.notify();
        }
    }

    /// Records a new discrete wheel target without jumping the painted zoom to
    /// it. Repeated wheel events extend the same target stream and only update
    /// its anchor; the render loop consumes at most one interpolation step per
    /// display frame.
    pub(super) fn queue_wheel_zoom_factor(
        &mut self,
        factor: f32,
        window_offset: f32,
        cx: &mut Context<Self>,
    ) {
        self.raw_zoom = (self.raw_zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let target = clamp_and_snap_zoom(self.raw_zoom);
        if target == self.zoom {
            if self.wheel_zoom_animation.take().is_some() {
                // Reversing a wheel gesture can move its target back onto the
                // currently painted zoom before the interpolation naturally
                // settles. Intermediate frames intentionally keep offscreen
                // height estimates from the previous zoom, so cancelling here
                // must perform the same one-time finalization as a normally
                // completed animation.
                self.rebuild_height_estimates();
                cx.notify();
            }
            return;
        }

        if let Some(animation) = self.wheel_zoom_animation.as_mut() {
            animation.window_offset = window_offset;
        } else {
            self.wheel_zoom_animation = Some(WheelZoomAnimation {
                window_offset,
                last_frame: Instant::now(),
            });
        }
        cx.notify();
    }

    /// Runs before layout for each requested animation frame. The new zoom is
    /// installed before shaping, so every frame has one internally consistent
    /// geometry generation; the existing pending anchor is then resolved after
    /// visible blocks have been remeasured later in the same render.
    pub(super) fn step_wheel_zoom_animation(&mut self, window: &Window) {
        let Some(mut animation) = self.wheel_zoom_animation else {
            return;
        };
        let target = clamp_and_snap_zoom(self.raw_zoom);
        if target == self.zoom {
            self.wheel_zoom_animation = None;
            return;
        }

        let now = Instant::now();
        let next = eased_wheel_zoom_step(
            self.zoom,
            target,
            now.saturating_duration_since(animation.last_frame),
        );
        self.pending_zoom_anchor = self.zoom_anchor_at(animation.window_offset);
        self.zoom = next;

        if next == target {
            self.wheel_zoom_animation = None;
        } else {
            animation.last_frame = now;
            self.wheel_zoom_animation = Some(animation);
            window.request_animation_frame();
        }
    }

    /// Resets zoom to 100%, anchored at the viewport's vertical center since
    /// (unlike a wheel or pinch gesture) this has no pointer position of its
    /// own.
    pub(crate) fn reset_zoom(&mut self, cx: &mut Context<Self>) {
        self.set_zoom(1.0, self.viewport_height / 2.0, cx);
    }

    /// Resolves a `PendingZoomAnchor` captured before a zoom-driven height
    /// change back to the `scroll_y` that keeps the anchored item and
    /// fraction at the same `window_offset` now that `self.heights` reflects
    /// the new zoom level. Callers must check `self.heights` is non-empty.
    pub(super) fn scroll_y_for_zoom_anchor(&self, anchor: PendingZoomAnchor) -> f32 {
        let ordinal = anchor.ordinal.min(self.heights.len() - 1);
        let top = self.heights.prefix_sum(ordinal);
        let height = self.heights.height(ordinal).unwrap_or(0.0);
        let content_y = top + anchor.fraction * height;
        content_y - anchor.window_offset
    }

    pub(super) fn on_scroll(
        &mut self,
        event: &ScrollWheelEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.record_scroll_receipt_for_measurement();
        self.on_scroll_at(event, Instant::now(), cx);
    }

    /// Same as `on_scroll`, but takes the "now" instant that a live coast's
    /// elapsed gap is measured against explicitly instead of reading
    /// `Instant::now()` internally. `on_scroll` always passes a fresh
    /// `Instant::now()`, so production behavior is unchanged; this split
    /// only exists so tests can pin that instant themselves (see
    /// `queue_scroll_inertia_at`'s doc comment) instead of depending on how
    /// long the test harness takes between two separate real-clock reads,
    /// which can exceed `SCROLL_INERTIA_MIN_FRAME_TIME` on a loaded CI
    /// runner and make the decay this measures nondeterministic.
    pub(super) fn on_scroll_at(&mut self, event: &ScrollWheelEvent, now: Instant, cx: &mut Context<Self>) {
        if event.modifiers.secondary() {
            // A still-coasting scroll inertia must not keep advancing
            // `scroll_y` underneath a zoom gesture started from the same
            // wheel stream: `step_scroll_inertia` has no awareness of zoom
            // and would otherwise keep scrolling every frame in parallel
            // with the zoom animation (issue #389).
            self.scroll_inertia = None;
            let factor = zoom_factor_for_wheel(event.delta, self.line_height());
            let window_offset = f32::from(event.position.y) - self.theme.header_height;
            self.queue_wheel_zoom_factor(factor, window_offset, cx);
            return;
        }
        let delta = event.delta.pixel_delta(px(self.line_height()));
        let scroll_delta = -f32::from(delta.y);
        match event.delta {
            ScrollDelta::Lines(_) => {
                // The direct jump is folded into the coast's own first step
                // instead of being applied on top of it (see
                // `queue_scroll_inertia_at`), so the combined immediate and
                // inertial movement matches a single plain scroll of this
                // delta instead of doubling it (issue #389).
                let velocity = scroll_inertia_velocity_for_lines_delta(scroll_delta);
                self.queue_scroll_inertia_at(velocity, now, cx);
            }
            ScrollDelta::Pixels(_) => {
                self.scroll_y = clamp_scroll_y(
                    self.scroll_y + scroll_delta,
                    self.scrollable_content_height(),
                    self.viewport_height,
                );
                // The OS already supplies trackpad momentum for pixel deltas;
                // layering the app's own inertia on top would double it and
                // fight direct tracking as the OS event stream itself
                // decelerates.
                self.scroll_inertia = None;
                cx.notify();
            }
        }
    }

    /// Starts or replaces the short post-wheel inertia that continues a
    /// plain `ScrollDelta::Lines` scroll after the input stops (issue #389).
    /// Applies the coast's own first exponential-decay step to `scroll_y`
    /// immediately (so the response is visible from this same event instead
    /// of waiting for the next animation frame), then queues the remaining,
    /// already-decayed velocity for `step_scroll_inertia` to keep advancing.
    /// Reusing `eased_scroll_inertia_step` for this first step, rather than
    /// separately jumping `scroll_y` by the full delta before queuing a coast
    /// sized off that same delta, is what keeps the combined movement equal
    /// to `velocity * SCROLL_INERTIA_TIME_CONSTANT` (the delta itself)
    /// instead of doubling it: the discrete steps of an exponential decay
    /// telescope back to that exact total regardless of where the sequence
    /// is split.
    ///
    /// A new event in the same direction as a still-live coast adds its
    /// velocity to what remains of the old one instead of replacing it
    /// (issue #389), so the previous event's not-yet-moved distance keeps
    /// contributing rather than being dropped. This is exact, not just
    /// approximate: the same telescoping identity that keeps one event's
    /// combined movement equal to its own delta also holds when the velocity
    /// fed into a later step already carries an earlier step's remaining
    /// velocity, so a run of same-direction events still settles at the sum
    /// of their deltas. A new event in the opposite direction, or arriving
    /// while the previous coast has gone stale (`scroll_y` moved out from
    /// under it since its last step, the same condition `step_scroll_inertia`
    /// checks via `ScrollInertia::last_applied`), replaces the velocity
    /// outright instead, so it cancels immediately rather than fighting or
    /// inheriting a coast it should not be combined with.
    ///
    /// The immediate step below is sized off how long it has actually been
    /// since a live coast last touched `scroll_y` (`ScrollInertia::last_frame`),
    /// not a fixed nominal frame duration, so a reversal that arrives well
    /// after the previous coast's last rendered step (a real input event can
    /// land noticeably later than the animation-frame cadence, e.g. under
    /// system load) folds that whole gap into this one synchronous call
    /// instead of only ever advancing by one nominal frame's worth and
    /// leaving the rest to animation frames that may be delayed (issue #389:
    /// the old direction's coast could otherwise still read as unchanged for
    /// several frames after a hard reversal). A brand new coast, with no
    /// prior `ScrollInertia` to measure a gap against, uses the larger
    /// `SCROLL_INERTIA_COLD_START_FRAME_TIME` instead of
    /// `SCROLL_INERTIA_MIN_FRAME_TIME` for its own first step: unlike a
    /// reversal, which arrives while a `request_animation_frame` loop is
    /// already running (so the next real frame is imminent), this coast has
    /// no such loop in flight yet, and understating that first real paint's
    /// latency left it reading as unchanged on the actual first frame drawn
    /// (issue #389's first-response regression).
    ///
    /// Takes the "now" instant used both to measure the elapsed gap since a
    /// live coast's `last_frame` and to stamp the new `last_frame`
    /// explicitly, instead of reading `Instant::now()` internally.
    /// `on_scroll_at` always passes a fresh `Instant::now()` here, so
    /// production behavior is unchanged; this split exists purely as a test
    /// seam, so a test can pin that instant itself rather than depending on
    /// wall-clock time actually elapsing between separate real-clock reads.
    pub(super) fn queue_scroll_inertia_at(&mut self, velocity: f32, now: Instant, cx: &mut Context<Self>) {
        let (velocity, elapsed) = match self.scroll_inertia {
            Some(inertia) => {
                let elapsed = now.saturating_duration_since(inertia.last_frame);
                let velocity = if self.scroll_y == inertia.last_applied
                    && inertia.velocity * velocity > 0.0
                {
                    inertia.velocity + velocity
                } else {
                    velocity
                };
                (velocity, elapsed)
            }
            None => (velocity, SCROLL_INERTIA_COLD_START_FRAME_TIME),
        };
        match eased_scroll_inertia_step(velocity, elapsed) {
            Some((distance, next_velocity)) => {
                self.scroll_y = clamp_scroll_y(
                    self.scroll_y + distance,
                    self.scrollable_content_height(),
                    self.viewport_height,
                );
                self.scroll_inertia = Some(ScrollInertia {
                    velocity: next_velocity,
                    last_frame: now,
                    last_applied: self.scroll_y,
                });
            }
            None => {
                // A settle-epsilon velocity means the coast has nothing left
                // to animate, but `velocity * SCROLL_INERTIA_TIME_CONSTANT` is
                // still the caller's actual input delta (see
                // `scroll_inertia_velocity_for_lines_delta`'s doc comment) and
                // must still land on `scroll_y` once; dropping it here silently
                // ate every sub-pixel-precision wheel event (issue #389). This
                // is a one-time application, not a new coast: `scroll_inertia`
                // stays `None` so no further frames are scheduled for it.
                self.scroll_y = clamp_scroll_y(
                    self.scroll_y + velocity * SCROLL_INERTIA_TIME_CONSTANT.as_secs_f32(),
                    self.scrollable_content_height(),
                    self.viewport_height,
                );
                self.scroll_inertia = None;
            }
        }
        cx.notify();
    }

    /// Runs once per requested animation frame, before `render`'s own
    /// `scroll_y` clamp. `advance_scroll_inertia` clamps each step and records
    /// that bounded value as its last applied position; the render clamp also
    /// handles changes to the viewport or content height between frames.
    /// Delegates the actual state advance to `advance_scroll_inertia`, which
    /// touches nothing window-related, so `window.request_animation_frame()`
    /// (only callable during `request_layout`, `prepaint`, or `paint`) stays
    /// confined to this wrapper instead of being invoked from tests that call
    /// the step outside a real frame.
    pub(super) fn step_scroll_inertia(&mut self, window: &Window) {
        if self.advance_scroll_inertia() {
            window.request_animation_frame();
        }
    }

    /// Advances scroll inertia by one animation frame's worth of decay,
    /// mutating only `self`. Stops silently the moment something else has
    /// moved `scroll_y` since the last step (see `ScrollInertia::last_applied`),
    /// so stale wheel momentum never overwrites cursor-follow, selection
    /// autoscroll, a scrollbar drag, a document switch, zoom, or a pinch.
    /// Returns whether the coast is still running, i.e. whether a real
    /// caller must follow up with `window.request_animation_frame()`.
    pub(super) fn advance_scroll_inertia(&mut self) -> bool {
        let Some(mut inertia) = self.scroll_inertia else {
            return false;
        };
        if self.scroll_y != inertia.last_applied {
            self.scroll_inertia = None;
            return false;
        }
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(inertia.last_frame);
        let Some((distance, next_velocity)) = eased_scroll_inertia_step(inertia.velocity, elapsed)
        else {
            self.scroll_inertia = None;
            return false;
        };
        self.scroll_y = clamp_scroll_y(
            self.scroll_y + distance,
            self.scrollable_content_height(),
            self.viewport_height,
        );
        inertia.velocity = next_velocity;
        inertia.last_frame = now;
        inertia.last_applied = self.scroll_y;
        self.scroll_inertia = Some(inertia);
        true
    }

    /// Re-anchors `scroll_y` to the same item and intra-item offset
    /// `height_anchor` captured before this frame's visible blocks were
    /// remeasured (see the `render` call site), so a wrapped row's corrected
    /// height does not visibly move the document. `height_anchor` is `None`
    /// when there is nothing to anchor to (line granularity, or an empty
    /// document).
    ///
    /// `scroll_inertia_tracks_layout` tells this call whether a live inertia
    /// coast still owned `scroll_y` going into this frame's layout, i.e.
    /// nothing else (the render-entry clamp, cursor follow, a scrollbar drag,
    /// a document switch, zoom, or a pinch) had already moved it away from
    /// the coast's own `last_applied`. When that holds, this correction's own
    /// `scroll_y` change is resynced onto the coast's `last_applied`; left
    /// unsynced, the next frame's staleness check in `advance_scroll_inertia`
    /// would mistake this frame's own height-anchor correction for an
    /// unrelated ownership change and cancel a coast that never actually lost
    /// ownership of the position (issue #389).
    pub(super) fn apply_height_anchor(
        &mut self,
        height_anchor: Option<(usize, f32)>,
        scroll_inertia_tracks_layout: bool,
    ) {
        let Some((old_ordinal, intra)) = height_anchor else {
            return;
        };
        let ordinal = old_ordinal.min(self.heights.len().saturating_sub(1));
        let inside = if ordinal + 1 == self.heights.len() {
            // At the document's last item, `intra` can already be carrying
            // the `CARET_MODE_BADGE_HEIGHT` clearance `scroll_cursor_into_view`
            // reserved past its bottom. Clamping it to the item's own height
            // would throw that clearance away before the badge is ever drawn
            // (issue #240); the clamp below still bounds the result.
            intra.max(0.0)
        } else {
            self.heights
                .height(ordinal)
                .map_or(0.0, |height| intra.clamp(0.0, height))
        };
        self.scroll_y = self.heights.prefix_sum(ordinal) + inside;
        if scroll_inertia_tracks_layout && let Some(inertia) = self.scroll_inertia.as_mut() {
            inertia.last_applied = self.scroll_y;
        }
    }

    /// macOS trackpad pinch. `event.delta` is the incremental scale
    /// change since the previous event in the same gesture (see
    /// `gpui::PinchEvent`), so it is applied directly as a multiplicative
    /// factor rather than accumulated first.
    pub(super) fn on_pinch(&mut self, event: &PinchEvent, _: &mut Window, cx: &mut Context<Self>) {
        let factor = (1.0 + event.delta).max(0.1);
        let window_offset = f32::from(event.position.y) - self.theme.header_height;
        self.apply_direct_zoom_factor(factor, window_offset, cx);
    }
}
