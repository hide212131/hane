use super::*;

#[cfg(any(feature = "instrument", feature = "timing-probe"))]
impl EditorView {
    pub fn arm_startup_timing(&mut self, process_started: Instant) {
        self.instrumentation.process_started = process_started;
        self.instrumentation.ready_armed = true;
    }

    #[cfg(feature = "instrument")]
    pub fn record_phase0_idle_memory(&mut self, rss_bytes: Option<u64>, idle_seconds: u64) {
        let label = format!("memory_idle_{idle_seconds}s");
        if let Some(output) = &mut self.instrumentation.metrics_output
            && let Err(error) = output.memory(&label, rss_bytes)
        {
            eprintln!("could not write idle memory metrics: {error}");
        }
    }

    /// Returns the scanned work-folder root, indexed Markdown count, and exact
    /// scan-completion elapsed time for the release-only measurement harness.
    #[cfg(feature = "instrument")]
    pub fn measurement_work_folder_state(&self) -> Option<(PathBuf, usize, f64)> {
        let completed_at = self.instrumentation.work_folder_scan_completed_at?;
        self.work_folder.as_ref().map(|folder| {
            (
                folder.root().to_path_buf(),
                folder.len(),
                completed_at
                    .saturating_duration_since(self.instrumentation.process_started)
                    .as_secs_f64()
                    * 1_000.0,
            )
        })
    }

    #[cfg(feature = "instrument")]
    pub fn measurement_work_folder_note_paths(&self) -> Option<Vec<PathBuf>> {
        self.work_folder.as_ref().map(|folder| {
            folder
                .entries()
                .into_iter()
                .map(|entry| entry.path().to_path_buf())
                .collect()
        })
    }

    #[cfg(feature = "instrument")]
    pub fn measurement_note_is_loaded(&self, path: &Path) -> bool {
        !self.loading_paths.contains(path)
            && self
                .sessions
                .sessions()
                .any(|session| session.path() == Some(path))
    }

    #[cfg(feature = "instrument")]
    pub fn record_measurement_memory(&mut self, label: &str) -> (Option<u64>, usize) {
        let rss_bytes = hane_metrics::process_memory_bytes();
        let session_count = self.sessions.sessions().count();
        if let Some(output) = &mut self.instrumentation.metrics_output
            && let Err(error) = output.memory(label, rss_bytes)
        {
            eprintln!("could not write measurement memory metrics: {error}");
        }
        (rss_bytes, session_count)
    }

    #[cfg(feature = "instrument")]
    pub fn measurement_elapsed_ms(&self) -> f64 {
        self.instrumentation.process_started.elapsed().as_secs_f64() * 1_000.0
    }

    #[cfg(feature = "instrument")]
    pub fn run_measurement_edit_cycles(&mut self, count: usize, cx: &mut Context<Self>) {
        for _ in 0..count {
            self.dispatch(EditorCommand::Insert("x"), cx);
            self.dispatch(EditorCommand::Undo, cx);
            self.dispatch(EditorCommand::Redo, cx);
            self.dispatch(EditorCommand::Undo, cx);
        }
    }

    #[cfg(feature = "instrument")]
    pub fn apply_phase0_background_presentation(
        &mut self,
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        self.background_presentation_generation = generation;
        cx.notify();
    }

    #[cfg(feature = "instrument")]
    pub fn enable_display_linked_scroll_measurement(&mut self) {
        self.instrumentation.display_linked_scroll_direction = Some(1.0);
    }

    #[cfg(feature = "instrument")]
    fn apply_phase1_scroll_frame(&mut self) {
        let direction = self
            .instrumentation
            .display_linked_scroll_direction
            .unwrap_or(1.0);
        let max = (self.heights.total_height() - self.viewport_height).max(0.0);
        let proposed = self.scroll_y + direction * 72.0;
        let next_direction = if proposed <= 0.0 {
            1.0
        } else if proposed >= max {
            -1.0
        } else {
            direction
        };
        self.scroll_y = proposed.clamp(0.0, max);
        self.instrumentation.display_linked_scroll_direction = Some(next_direction);
    }

    pub(crate) fn step_measurement_scroll(&mut self, window: &mut Window) {
        #[cfg(feature = "instrument")]
        if self
            .instrumentation
            .display_linked_scroll_direction
            .is_some()
        {
            self.apply_phase1_scroll_frame();
            window.request_animation_frame();
        }
        #[cfg(not(feature = "instrument"))]
        let _ = window;
    }

    #[cfg(feature = "instrument")]
    /// Moves the cursor for an instrumentation run.
    ///
    /// # Errors
    ///
    /// Returns [`BufferError`] when `offset` is not a valid source boundary.
    pub fn set_cursor_offset_for_measurement(
        &mut self,
        offset: usize,
        cx: &mut Context<Self>,
    ) -> Result<(), BufferError> {
        self.editor_mut()
            .set_selection(hane_editor::Selection::caret(hane_document::SourceOffset(
                offset,
            )))?;
        self.after_input(cx);
        Ok(())
    }

    #[cfg(feature = "instrument")]
    /// Moves the cursor down for an instrumentation run.
    ///
    /// # Errors
    ///
    /// Returns [`BufferError`] when an editor command cannot be applied.
    pub fn move_cursor_down_for_development(
        &mut self,
        count: usize,
        cx: &mut Context<Self>,
    ) -> Result<(), BufferError> {
        for _ in 0..count {
            self.editor_mut()
                .dispatch(EditorCommand::MoveDown { extend: false })?;
        }
        self.after_input(cx);
        Ok(())
    }

    pub(super) fn record_block_index_update(&mut self, update: &BlockIndexUpdate) {
        if let Some(output) = &mut self.instrumentation.metrics_output
            && let Err(error) = output.block_index(update)
        {
            eprintln!("could not write block index metrics: {error}");
        }
    }

    /// Marks that this view just received a `ScrollWheelEvent`, so the next
    /// `record_frame_instrumentation` call can pair it with that frame's own
    /// mach-clock paint/submission time (Issue #427's measurement-only
    /// correlation tool). Purely an observation hook: it records nothing
    /// about what the scroll did and never changes scroll behavior.
    ///
    /// Only the first receipt in a pending interval is kept: if another
    /// `ScrollWheelEvent` arrives before `record_frame_instrumentation` has
    /// consumed the pending tick, that later receipt is dropped rather than
    /// overwriting the earlier one, so the measured interval always spans
    /// from the first receipt to the next paint.
    pub(crate) fn record_scroll_receipt_for_measurement(&mut self) {
        if self.instrumentation.pending_scroll_receipt_ticks.is_some() {
            return;
        }
        if self.instrumentation.scroll_event_timing.is_some()
            && let Some(ticks) = hane_metrics::mach_absolute_ticks()
        {
            self.instrumentation.pending_scroll_receipt_ticks = Some(ticks);
        }
    }

    pub(crate) fn record_frame_instrumentation(
        &mut self,
        measurements: &[InputMeasurement],
        interval: Option<Duration>,
        layout: Option<Duration>,
    ) {
        let instrumentation = &mut self.instrumentation;
        // `paint_ticks` is when `InputCapture::paint` ran (inside
        // `Window::draw`), not when the platform renderer actually presented
        // the frame to the compositor; `ScrollEventTimingOutput::record`
        // reports that true presentation time as unavailable rather than
        // treating this as it.
        if let Some(receipt_ticks) = instrumentation.pending_scroll_receipt_ticks.take()
            && let Some(paint_ticks) = hane_metrics::mach_absolute_ticks()
            && let Some(output) = &mut instrumentation.scroll_event_timing
            && let Err(error) = output.record(receipt_ticks, paint_ticks)
        {
            eprintln!("could not write scroll event timing: {error}");
        }
        if instrumentation.ready_armed && !instrumentation.ready_reported {
            instrumentation.ready_reported = true;
            let startup = instrumentation.process_started.elapsed();
            let rss = hane_metrics::process_memory_bytes();
            eprintln!(
                "hane_ready startup_time_ms={:.3} file_open_time_ms={:.3} rss_bytes={}",
                startup.as_secs_f64() * 1_000.0,
                instrumentation.file_open_time.as_secs_f64() * 1_000.0,
                rss.unwrap_or(0),
            );
            if let Some(output) = &mut instrumentation.metrics_output {
                if let Err(error) = output.memory("memory_load", instrumentation.load_rss_bytes) {
                    eprintln!("could not write load memory metrics: {error}");
                }
                if let Err(error) = output.ready(startup, instrumentation.file_open_time, rss) {
                    eprintln!("could not write ready metrics: {error}");
                }
            }
        }
        if let Some(output) = &mut instrumentation.metrics_output {
            let cache = (
                instrumentation.layout_cache_hits,
                instrumentation.layout_cache_misses,
            );
            if let Err(error) = output.paint(interval, layout, cache) {
                eprintln!("could not write paint metrics: {error}");
            }
            for measurement in measurements {
                if let Err(error) = output.input(measurement) {
                    eprintln!("could not write input metrics: {error}");
                }
            }
        }
        instrumentation.layout_cache_hits = 0;
        instrumentation.layout_cache_misses = 0;
        if !measurements.is_empty() {
            log_summary(&self.metrics);
        }
    }
}

#[cfg(not(any(feature = "instrument", feature = "timing-probe")))]
impl EditorView {
    pub(crate) fn step_measurement_scroll(&mut self, _window: &mut Window) {}

    pub(super) fn record_block_index_update(&mut self, _update: &BlockIndexUpdate) {}

    pub(crate) fn record_scroll_receipt_for_measurement(&mut self) {}

    pub(crate) fn record_frame_instrumentation(
        &mut self,
        _measurements: &[InputMeasurement],
        _interval: Option<Duration>,
        _layout: Option<Duration>,
    ) {
    }
}
