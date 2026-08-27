use crate::actions::install_action_listeners;
use crate::capture::InputCapture;
#[cfg(feature = "instrument")]
use crate::instrument::{Instrumentation, log_summary};
use crate::line::{
    block_font_size, disclosure_for_line, inline_display_for, line_element_from_block,
    presented_line,
};
use crate::storage::{PersistentState, atomic_save_document};
use crate::theme::{DEFAULT_THEME, Theme, resolve_theme};
use gpui::{
    App, Context, FocusHandle, Focusable, FontStyle, FontWeight, InteractiveElement, IntoElement,
    MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement, PathPromptOptions, Render,
    ScrollWheelEvent, StatefulInteractiveElement, Styled, TextRun, Window, div, px, rgb,
};
use hane_document::{
    Bias, BufferError, LineId, Revision, RevisionDelta, RopeBuffer, SourceOffset, SourceRange,
    TextBuffer,
};
use hane_editor::{Editor, EditorCommand, InputMeasurement, Selection};
use hane_markdown::{BlockContextIndex, local_block_context, parse_block_context};
use hane_metrics::FrameMetrics;
use hane_presentation::{BlockWeight, HeightIndex, LineContext, VisualOffset};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const METRICS_CAPACITY: usize = 4_096;

fn scroll_y_for_cursor(
    scroll_y: f32,
    cursor_top: f32,
    line_height: f32,
    viewport_height: f32,
) -> f32 {
    if cursor_top < scroll_y {
        cursor_top
    } else if cursor_top + line_height > scroll_y + viewport_height {
        cursor_top + line_height - viewport_height
    } else {
        scroll_y
    }
}

fn content_top_for_scroll(scroll_y: f32) -> f32 {
    -scroll_y
}

fn autosave_request_is_current(
    current_generation: u64,
    requested_generation: u64,
    current_revision: Revision,
    requested_revision: Revision,
    enabled: bool,
    has_path: bool,
) -> bool {
    enabled
        && has_path
        && current_generation == requested_generation
        && current_revision == requested_revision
}

fn block_context_revision_is_current(current: Revision, candidate: Revision) -> bool {
    current == candidate
}

pub struct EditorView {
    pub(crate) editor: Editor,
    pub(crate) focus_handle: FocusHandle,
    heights: HeightIndex,
    scroll_y: f32,
    viewport_height: f32,
    pub(crate) metrics: FrameMetrics,
    file_label: String,
    file_path: Option<PathBuf>,
    saved_revision: Revision,
    status: Option<String>,
    theme: Theme,
    #[cfg(feature = "instrument")]
    pub(crate) instrumentation: Instrumentation,
    background_presentation_generation: u64,
    line_cache: HashMap<usize, hane_presentation::VisualBlock>,
    block_context: Option<BlockContextIndex>,
    block_context_job_running: bool,
    persistent_state: PersistentState,
    save_generation: u64,
    save_job_running: bool,
    pending_save_path: Option<PathBuf>,
}

impl EditorView {
    pub fn new(text: &str, file_label: impl Into<String>, cx: &mut Context<Self>) -> Self {
        Self::from_editor(
            Editor::new(text),
            file_label,
            None,
            PersistentState::load_default(),
            cx,
        )
    }

    fn from_editor(
        editor: Editor,
        file_label: impl Into<String>,
        file_path: Option<PathBuf>,
        persistent_state: PersistentState,
        cx: &mut Context<Self>,
    ) -> Self {
        let theme = DEFAULT_THEME;
        let saved_revision = editor.document().revision();
        let heights = HeightIndex::new(std::iter::repeat_n(
            theme.line_height,
            editor.document().line_count(),
        ));
        Self {
            editor,
            focus_handle: cx.focus_handle(),
            heights,
            scroll_y: 0.0,
            viewport_height: theme.line_height,
            metrics: FrameMetrics::new(METRICS_CAPACITY),
            file_label: file_label.into(),
            file_path,
            saved_revision,
            status: None,
            theme,
            #[cfg(feature = "instrument")]
            instrumentation: Instrumentation::from_environment(),
            background_presentation_generation: 0,
            line_cache: HashMap::new(),
            block_context: None,
            block_context_job_running: false,
            persistent_state,
            save_generation: 0,
            save_job_running: false,
            pending_save_path: None,
        }
    }

    pub fn open(path: &Path, cx: &mut Context<Self>) -> std::io::Result<Self> {
        #[cfg(feature = "instrument")]
        let started = Instant::now();
        let file = std::fs::File::open(path)?;
        let document = RopeBuffer::from_reader(std::io::BufReader::new(file))?;
        let mut persistent_state = PersistentState::load_default();
        persistent_state.remember(path);
        if let Err(error) = persistent_state.save() {
            eprintln!("could not save recent files: {error}");
        }
        cx.add_recent_document(path);
        #[cfg_attr(not(feature = "instrument"), allow(unused_mut))]
        let mut view = Self::from_editor(
            Editor::from_document(document),
            path.display().to_string(),
            Some(path.to_path_buf()),
            persistent_state,
            cx,
        );
        #[cfg(feature = "instrument")]
        {
            view.instrumentation.file_open_time = started.elapsed();
            view.instrumentation.load_rss_bytes = hane_metrics::process_memory_bytes();
        }
        Ok(view)
    }

    pub fn editor(&self) -> &Editor {
        &self.editor
    }

    pub(crate) fn after_input(&mut self, cx: &mut Context<Self>) {
        let lines = self.editor.document().line_count();
        if lines != self.heights.len() {
            self.heights = HeightIndex::new(std::iter::repeat_n(self.theme.line_height, lines));
            self.line_cache.clear();
        }
        self.scroll_cursor_into_view();
        self.schedule_block_context(cx);
        self.schedule_autosave(cx);
        cx.notify();
    }

    fn schedule_autosave(&mut self, cx: &mut Context<Self>) {
        self.save_generation = self.save_generation.wrapping_add(1);
        if !self.persistent_state.settings.autosave
            || self.file_path.is_none()
            || self.editor.document().revision() == self.saved_revision
        {
            return;
        }
        let generation = self.save_generation;
        let revision = self.editor.document().revision();
        cx.spawn(async move |view, cx| {
            gpui::Timer::after(Duration::from_millis(750)).await;
            let should_save = view
                .read_with(cx, |view, _| {
                    autosave_request_is_current(
                        view.save_generation,
                        generation,
                        view.editor.document().revision(),
                        revision,
                        view.persistent_state.settings.autosave,
                        view.file_path.is_some(),
                    )
                })
                .unwrap_or(false);
            if should_save {
                let _ = view.update(cx, |view, cx| view.save_current(cx));
            }
        })
        .detach();
    }

    pub(crate) fn save_current(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.file_path.clone() else {
            self.status = Some("Use Save As for an untitled document".to_owned());
            cx.notify();
            return;
        };
        self.save_to(path, cx);
    }

    pub(crate) fn save_or_prompt(&mut self, cx: &mut Context<Self>) {
        if self.file_path.is_some() {
            self.save_current(cx);
        } else {
            self.prompt_save_as(cx);
        }
    }

    fn save_to(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.save_job_running {
            self.pending_save_path = Some(path);
            self.status = Some("Save queued…".to_owned());
            cx.notify();
            return;
        }
        self.save_job_running = true;
        let snapshot = self.editor.document().clone();
        let revision = snapshot.revision();
        self.status = Some("Saving…".to_owned());
        cx.spawn(async move |view, cx| {
            let save_path = path.clone();
            let result = cx
                .background_executor()
                .spawn(async move { atomic_save_document(&save_path, &snapshot) })
                .await;
            let _ = view.update(cx, |view, cx| {
                view.save_job_running = false;
                match result {
                    Ok(()) => {
                        view.file_path = Some(path.clone());
                        view.file_label = path.display().to_string();
                        if view.editor.document().revision() == revision {
                            view.saved_revision = revision;
                            view.status = Some("Saved".to_owned());
                        } else {
                            view.status = Some("Saved snapshot; newer edits pending".to_owned());
                            view.schedule_autosave(cx);
                        }
                        view.persistent_state.remember(&path);
                        if let Err(error) = view.persistent_state.save() {
                            view.status =
                                Some(format!("Saved document; recent files failed: {error}"));
                        }
                        cx.add_recent_document(&path);
                    }
                    Err(error) => view.status = Some(format!("Save failed: {error}")),
                }
                if let Some(pending) = view.pending_save_path.take() {
                    view.save_to(pending, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn prompt_save_as(&mut self, cx: &mut Context<Self>) {
        let directory = self
            .file_path
            .as_deref()
            .and_then(Path::parent)
            .unwrap_or_else(|| Path::new("."));
        let receiver = cx.prompt_for_new_path(directory, Some("Untitled.md"));
        cx.spawn(async move |view, cx| match receiver.await {
            Ok(Ok(Some(path))) => {
                let _ = view.update(cx, |view, cx| view.save_to(path, cx));
            }
            Ok(Err(error)) => {
                let _ = view.update(cx, |view, cx| {
                    view.status = Some(format!("Save As failed: {error}"));
                    cx.notify();
                });
            }
            _ => {}
        })
        .detach();
    }

    pub(crate) fn prompt_open(&mut self, cx: &mut Context<Self>) {
        if self.editor.document().revision() != self.saved_revision {
            self.status = Some("Save current changes before opening another file".to_owned());
            cx.notify();
            return;
        }
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open Markdown".into()),
        });
        cx.spawn(async move |view, cx| match receiver.await {
            Ok(Ok(Some(paths))) => {
                if let Some(path) = paths.into_iter().next() {
                    let _ = view.update(cx, |view, cx| view.load_path(&path, cx));
                }
            }
            Ok(Err(error)) => {
                let _ = view.update(cx, |view, cx| {
                    view.status = Some(format!("Open failed: {error}"));
                    cx.notify();
                });
            }
            _ => {}
        })
        .detach();
    }

    fn load_path(&mut self, path: &Path, cx: &mut Context<Self>) {
        let result = std::fs::File::open(path)
            .and_then(|file| RopeBuffer::from_reader(std::io::BufReader::new(file)));
        match result {
            Ok(document) => {
                self.editor = Editor::from_document(document);
                self.file_path = Some(path.to_path_buf());
                self.file_label = path.display().to_string();
                self.saved_revision = self.editor.document().revision();
                self.heights = HeightIndex::new(std::iter::repeat_n(
                    self.theme.line_height,
                    self.editor.document().line_count(),
                ));
                self.scroll_y = 0.0;
                self.line_cache.clear();
                self.block_context = None;
                self.persistent_state.remember(path);
                if let Err(error) = self.persistent_state.save() {
                    self.status = Some(format!("Opened; recent files failed: {error}"));
                } else {
                    self.status = Some("Opened".to_owned());
                }
                cx.add_recent_document(path);
                self.schedule_block_context(cx);
            }
            Err(error) => self.status = Some(format!("Open failed: {error}")),
        }
        cx.notify();
    }

    fn open_recent_path(&mut self, path: &Path, cx: &mut Context<Self>) {
        if self.editor.document().revision() != self.saved_revision {
            self.status = Some("Save current changes before opening a recent file".to_owned());
            cx.notify();
        } else {
            self.load_path(path, cx);
        }
    }

    pub(crate) fn toggle_autosave(&mut self, cx: &mut Context<Self>) {
        self.persistent_state.settings.autosave = !self.persistent_state.settings.autosave;
        self.status = Some(format!(
            "Autosave {}",
            if self.persistent_state.settings.autosave {
                "on"
            } else {
                "off"
            }
        ));
        if let Err(error) = self.persistent_state.save() {
            self.status = Some(format!("Settings failed: {error}"));
        }
        self.schedule_autosave(cx);
        cx.notify();
    }

    pub(crate) fn cycle_theme(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.persistent_state.settings.theme = self.persistent_state.settings.theme.next();
        self.theme = resolve_theme(self.persistent_state.settings.theme, window.appearance());
        self.heights = HeightIndex::new(std::iter::repeat_n(
            self.theme.line_height,
            self.editor.document().line_count(),
        ));
        self.line_cache.clear();
        if let Err(error) = self.persistent_state.save() {
            self.status = Some(format!("Settings failed: {error}"));
        }
        cx.notify();
    }

    fn schedule_block_context(&mut self, cx: &mut Context<Self>) {
        let revision = self.editor.document().revision();
        if self
            .block_context
            .as_ref()
            .is_some_and(|index| index.revision == revision)
        {
            return;
        }
        if self.block_context_job_running {
            return;
        }
        self.block_context_job_running = true;
        let snapshot = self.editor.document().clone();
        cx.spawn(async move |view, cx| {
            gpui::Timer::after(Duration::from_millis(40)).await;
            let current = view
                .update(cx, |view, _| {
                    block_context_revision_is_current(view.editor.document().revision(), revision)
                })
                .unwrap_or(false);
            if !current {
                let _ = view.update(cx, |view, cx| {
                    view.block_context_job_running = false;
                    view.schedule_block_context(cx);
                });
                return;
            }
            let index = cx
                .background_executor()
                .spawn(async move { parse_block_context(&snapshot) })
                .await;
            let _ = view.update(cx, |view, cx| {
                view.block_context_job_running = false;
                if block_context_revision_is_current(
                    view.editor.document().revision(),
                    index.revision,
                ) {
                    view.background_presentation_generation = index.revision.0 + 1;
                    view.block_context = Some(index);
                    view.line_cache.clear();
                    cx.notify();
                } else {
                    view.schedule_block_context(cx);
                }
            });
        })
        .detach();
    }

    pub(crate) fn report_error(&mut self, operation: &str, error: BufferError) {
        self.status = Some(format!("{operation} rejected: {error}"));
    }

    pub(crate) fn dispatch(&mut self, command: EditorCommand<'_>, cx: &mut Context<Self>) {
        match self.editor.dispatch(command) {
            Ok(_) => self.status = None,
            Err(error) => self.report_error("editor command", error),
        }
        self.after_input(cx);
    }

    pub(crate) fn perform_cancel_composition(&mut self, cx: &mut Context<Self>) {
        if let Err(error) = self.editor.cancel_composition() {
            self.report_error("composition cancel", error);
        }
        self.after_input(cx);
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let delta = event.delta.pixel_delta(px(self.theme.line_height));
        let max = (self.heights.total_height() - self.viewport_height).max(0.0);
        self.scroll_y = (self.scroll_y - f32::from(delta.y)).clamp(0.0, max);
        cx.notify();
    }

    fn on_line_mouse_down(
        &mut self,
        line: usize,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        let Some(block) = self
            .line_cache
            .get(&line)
            .cloned()
            .or_else(|| presented_line(&self.editor, line, LineContext::Normal))
        else {
            return;
        };
        let visual_offset = visual_offset_at_x(&block, event.position.x, self.theme, window);
        let offset = source_offset_for_visual_position(&self.editor, line, &block, visual_offset);
        let selection = if event.modifiers.shift {
            Selection {
                anchor: self.editor.selection().anchor,
                active: offset,
            }
        } else {
            Selection::caret(offset)
        };
        if let Err(error) = self.editor.set_selection(selection) {
            self.report_error("mouse selection", error);
        } else {
            self.status = None;
        }
        self.after_input(cx);
    }

    fn on_line_mouse_move(
        &mut self,
        line: usize,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !event.dragging() {
            return;
        }
        let Some(block) = self
            .line_cache
            .get(&line)
            .cloned()
            .or_else(|| presented_line(&self.editor, line, LineContext::Normal))
        else {
            return;
        };
        let visual = visual_offset_at_x(&block, event.position.x, self.theme, window);
        let offset = source_offset_for_visual_position(&self.editor, line, &block, visual);
        let selection = Selection {
            anchor: self.editor.selection().anchor,
            active: offset,
        };
        if self.editor.set_selection(selection).is_ok() {
            self.after_input(cx);
        }
    }

    fn scroll_cursor_into_view(&mut self) {
        let Ok(line) = self
            .editor
            .document()
            .line_for_offset(self.editor.selection().active)
        else {
            return;
        };
        let top = self.heights.prefix_sum(line.0);
        self.scroll_y = scroll_y_for_cursor(
            self.scroll_y,
            top,
            self.theme.line_height,
            self.viewport_height,
        );
    }

    fn cached_line(
        &mut self,
        line: usize,
        context: LineContext,
    ) -> Option<hane_presentation::VisualBlock> {
        let current_revision = self.editor.document().revision();
        if let Some(mut block) = self.line_cache.remove(&line) {
            let expected_disclosure = self
                .editor
                .document()
                .line_range(LineId(line))
                .ok()
                .and_then(|range| disclosure_for_line(&self.editor, line, range));
            let reusable = if block.revision == current_revision {
                block.disclosure == expected_disclosure
            } else if let Ok(deltas) = self.editor.document().deltas_since(block.revision) {
                !deltas
                    .iter()
                    .any(|delta| edit_affects_range(*delta, block.source_range))
                    && block.rebase(&deltas, current_revision)
                    && block.disclosure == expected_disclosure
                    && self
                        .editor
                        .document()
                        .line_range(LineId(line))
                        .is_ok_and(|range| range == block.source_range)
            } else {
                false
            };
            // A cached block records the context it was presented with, so the
            // check is an equality test rather than a re-derivation of the kind.
            if reusable && block.context == context {
                self.line_cache.insert(line, block.clone());
                return Some(block);
            }
        }
        let block = presented_line(&self.editor, line, context)?;
        self.line_cache.insert(line, block.clone());
        Some(block)
    }
}

fn edit_affects_range(delta: RevisionDelta, range: SourceRange) -> bool {
    let edit = delta.edited_source_range_before;
    if edit.is_empty() {
        range.start <= edit.start && edit.start <= range.end
    } else {
        range.intersects(edit)
    }
}

impl Focusable for EditorView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

#[cfg(feature = "instrument")]
impl EditorView {
    pub fn arm_startup_timing(&mut self, process_started: Instant) {
        self.instrumentation.process_started = process_started;
        self.instrumentation.ready_armed = true;
    }

    pub fn record_phase0_idle_memory(&mut self, rss_bytes: Option<u64>) {
        if let Some(output) = &mut self.instrumentation.metrics_output
            && let Err(error) = output.memory("memory_idle_30s", rss_bytes)
        {
            eprintln!("could not write idle memory metrics: {error}");
        }
    }

    pub fn apply_phase0_background_presentation(
        &mut self,
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        self.background_presentation_generation = generation;
        cx.notify();
    }

    pub fn enable_display_linked_scroll_measurement(&mut self) {
        self.instrumentation.display_linked_scroll_direction = Some(1.0);
    }

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
        if self
            .instrumentation
            .display_linked_scroll_direction
            .is_some()
        {
            self.apply_phase1_scroll_frame();
            window.request_animation_frame();
        }
    }

    pub fn set_cursor_offset_for_measurement(
        &mut self,
        offset: usize,
        cx: &mut Context<Self>,
    ) -> Result<(), BufferError> {
        self.editor
            .set_selection(hane_editor::Selection::caret(hane_document::SourceOffset(
                offset,
            )))?;
        self.after_input(cx);
        Ok(())
    }

    pub fn move_cursor_down_for_development(
        &mut self,
        count: usize,
        cx: &mut Context<Self>,
    ) -> Result<(), BufferError> {
        for _ in 0..count {
            self.editor
                .dispatch(EditorCommand::MoveDown { extend: false })?;
        }
        self.after_input(cx);
        Ok(())
    }

    pub(crate) fn record_frame_instrumentation(
        &mut self,
        measurements: &[InputMeasurement],
        interval: Option<Duration>,
        layout: Option<Duration>,
    ) {
        let instrumentation = &mut self.instrumentation;
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
            if let Err(error) = output.paint(interval, layout) {
                eprintln!("could not write paint metrics: {error}");
            }
            for measurement in measurements {
                if let Err(error) = output.input(measurement) {
                    eprintln!("could not write input metrics: {error}");
                }
            }
        }
        if !measurements.is_empty() {
            log_summary(&self.metrics);
        }
    }
}

#[cfg(not(feature = "instrument"))]
impl EditorView {
    pub(crate) fn step_measurement_scroll(&mut self, _window: &mut Window) {}

    pub(crate) fn record_frame_instrumentation(
        &mut self,
        _measurements: &[InputMeasurement],
        _interval: Option<Duration>,
        _layout: Option<Duration>,
    ) {
    }
}

impl Render for EditorView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let layout_started = Instant::now();
        self.theme = resolve_theme(self.persistent_state.settings.theme, window.appearance());
        self.schedule_block_context(cx);
        self.viewport_height = (f32::from(window.viewport_size().height)
            - self.theme.header_height)
            .max(self.theme.line_height);
        self.step_measurement_scroll(window);
        let max_scroll = (self.heights.total_height() - self.viewport_height).max(0.0);
        self.scroll_y = self.scroll_y.clamp(0.0, max_scroll);
        let visible =
            self.heights
                .visible_range(self.scroll_y, self.viewport_height, self.theme.overscan);
        let cache_start = visible.start.saturating_sub(128);
        let cache_end = (visible.end + 128).min(self.heights.len());
        self.line_cache
            .retain(|line, _| cache_start <= *line && *line < cache_end);
        let background_context = self.block_context.as_ref().filter(|index| {
            index.revision == self.editor.document().revision()
                && index.line_count() == self.editor.document().line_count()
        });
        // Prefer the formal document-wide index; only fall back to a single
        // bounded scan of the viewport neighborhood while that index is stale.
        let local_context = background_context
            .is_none()
            .then(|| local_block_context(self.editor.document(), visible.clone()));
        let contexts = visible
            .clone()
            .map(|line| {
                let fenced = background_context
                    .and_then(|index| index.line_is_fenced(line))
                    .or_else(|| local_context.as_ref().and_then(|c| c.line_is_fenced(line)))
                    .unwrap_or(false);
                let table = background_context
                    .and_then(|index| index.line_is_table(line))
                    .or_else(|| local_context.as_ref().and_then(|c| c.line_is_table(line)))
                    .unwrap_or(false);
                LineContext::from_document_context(fenced, table)
            })
            .collect::<Vec<_>>();
        let mut rendered_lines = Vec::with_capacity(visible.len());
        for (line, context) in visible.clone().zip(contexts) {
            if let Some(block) = self.cached_line(line, context) {
                rendered_lines.push((line, block));
            }
        }
        for (line, block) in &rendered_lines {
            self.heights.update(*line, block.height());
        }
        let top_space = self.heights.prefix_sum(visible.start);
        let bottom_space =
            (self.heights.total_height() - self.heights.prefix_sum(visible.end)).max(0.0);
        let revision = self.editor.document().revision().0;
        let bytes = self.editor.document().len_bytes().0;
        let p95 = self
            .metrics
            .painted_percentile(0.95)
            .map_or(0.0, |duration| duration.as_secs_f64() * 1000.0);
        let background = if self.background_presentation_generation > 0 {
            format!(" · bg {}", self.background_presentation_generation)
        } else {
            String::new()
        };
        let dirty = if self.editor.document().revision() == self.saved_revision {
            ""
        } else {
            " · modified"
        };
        let status = self.status.clone().unwrap_or_else(|| {
            format!("rev {revision} · {bytes} bytes · frame p95 {p95:.2} ms{background}{dirty}")
        });

        let root = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(self.theme.editor_background))
            .text_color(rgb(self.theme.foreground))
            .key_context("HaneEditor")
            .track_focus(&self.focus_handle(cx));
        let root = install_action_listeners(root, cx)
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .child(self.header_element(status, cx));
        let document_directory = self
            .file_path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf);
        let rendered = root.child(
            div()
                .relative()
                .flex_1()
                .overflow_hidden()
                .child(InputCapture { input: cx.entity() })
                .child(
                    div()
                        .absolute()
                        .top(px(content_top_for_scroll(self.scroll_y)))
                        .flex()
                        .flex_col()
                        .w_full()
                        .child(div().h(px(top_space)))
                        .children(rendered_lines.into_iter().map(|(line, block)| {
                            line_element_from_block(
                                &self.editor,
                                line,
                                &block,
                                self.theme,
                                document_directory.as_deref(),
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |view, event, window, cx| {
                                    view.on_line_mouse_down(line, event, window, cx)
                                }),
                            )
                            .on_mouse_move(cx.listener(
                                move |view, event, window, cx| {
                                    view.on_line_mouse_move(line, event, window, cx)
                                },
                            ))
                        }))
                        .child(div().h(px(bottom_space))),
                ),
        );
        self.metrics.record_layout(layout_started.elapsed());
        rendered
    }
}

impl EditorView {
    fn header_element(&self, status: String, cx: &mut Context<Self>) -> gpui::Div {
        let autosave = if self.persistent_state.settings.autosave {
            "Autosave on"
        } else {
            "Autosave off"
        };
        let theme = format!("Theme {:?}", self.persistent_state.settings.theme);
        let recent = self
            .persistent_state
            .recent_files
            .iter()
            .filter(|path| Some(path.as_path()) != self.file_path.as_deref())
            .take(3)
            .cloned()
            .enumerate()
            .map(|(index, path)| {
                let label = path.file_name().map_or_else(
                    || path.display().to_string(),
                    |name| name.to_string_lossy().into_owned(),
                );
                div()
                    .id(("recent-file", index))
                    .px_2()
                    .rounded_sm()
                    .bg(rgb(self.theme.code_background))
                    .text_color(rgb(self.theme.foreground))
                    .cursor_pointer()
                    .child(label)
                    .on_click(cx.listener(move |view, _, _, cx| view.open_recent_path(&path, cx)))
            })
            .collect::<Vec<_>>();
        div()
            .h(px(self.theme.header_height))
            .flex_none()
            .flex()
            .flex_col()
            .bg(rgb(self.theme.header_background))
            .text_color(rgb(self.theme.header_foreground))
            .child(
                div()
                    .h(px(38.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .child(self.file_label.clone())
                    .child(status),
            )
            .child(
                div()
                    .h(px(30.0))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .text_size(px(11.0))
                    .child(
                        div()
                            .id("toggle-autosave")
                            .cursor_pointer()
                            .child(autosave)
                            .on_click(cx.listener(|view, _, _, cx| view.toggle_autosave(cx))),
                    )
                    .child(
                        div()
                            .id("cycle-theme")
                            .cursor_pointer()
                            .child(theme)
                            .on_click(
                                cx.listener(|view, _, window, cx| view.cycle_theme(window, cx)),
                            ),
                    )
                    .child("Recent:")
                    .children(recent),
            )
    }
}

fn visual_offset_at_x(
    block: &hane_presentation::VisualBlock,
    position_x: gpui::Pixels,
    theme: Theme,
    window: &mut Window,
) -> usize {
    if block.visual_text.is_empty() {
        return 0;
    }
    let style = window.text_style();
    let mut boundaries = vec![0, block.visual_text.len()];
    for run in &block.style_runs {
        boundaries.push(run.visual_range.start.0.min(block.visual_text.len()));
        boundaries.push(run.visual_range.end.0.min(block.visual_text.len()));
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    let runs = boundaries
        .windows(2)
        .filter_map(|pair| {
            let range = pair[0]..pair[1];
            if range.is_empty() {
                return None;
            }
            // Same policy the painted elements use, so hit testing and painting
            // cannot drift apart.
            let inline = inline_display_for(&range, &block.style_runs);
            let mut font = style.font();
            if block.display().weight == BlockWeight::Semibold || inline.bold {
                font.weight = if inline.bold {
                    FontWeight::BOLD
                } else {
                    FontWeight::SEMIBOLD
                };
            }
            if inline.italic {
                font.style = FontStyle::Italic;
            }
            if inline.monospace {
                font.family = "ui-monospace".into();
            }
            Some(TextRun {
                len: range.len(),
                font,
                color: style.color,
                background_color: None,
                underline: None,
                strikethrough: None,
            })
        })
        .collect::<Vec<_>>();
    let font_size = px(block_font_size(block));
    let layout =
        window
            .text_system()
            .shape_line(block.visual_text.clone().into(), font_size, &runs, None);
    layout.closest_index_for_x(position_x - px(theme.line_horizontal_padding))
}

fn source_offset_for_visual_position(
    editor: &Editor,
    line: usize,
    block: &hane_presentation::VisualBlock,
    visual_offset: usize,
) -> SourceOffset {
    block
        .source_map
        .visual_to_source(VisualOffset(visual_offset), Bias::After)
        .map(|candidate| candidate.source_offset)
        .or_else(|| {
            editor
                .document()
                .line_content_range(LineId(line))
                .ok()
                .map(|range| range.start)
        })
        .unwrap_or(block.source_range.start)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hane_document::LineId;

    #[test]
    fn visual_click_positions_map_back_to_source_offsets() {
        let editor = Editor::new("ab🙂\n\n**bold**");

        let first = presented_line(&editor, 0, LineContext::Normal).unwrap();
        assert_eq!(
            source_offset_for_visual_position(&editor, 0, &first, 2),
            SourceOffset(2)
        );
        assert_eq!(
            source_offset_for_visual_position(&editor, 0, &first, first.visual_text.len()),
            SourceOffset(6)
        );

        let empty = presented_line(&editor, 1, LineContext::Normal).unwrap();
        assert_eq!(
            source_offset_for_visual_position(&editor, 1, &empty, 0),
            SourceOffset(7)
        );

        let bold = presented_line(&editor, 2, LineContext::Normal).unwrap();
        assert_eq!(
            source_offset_for_visual_position(&editor, 2, &bold, 0),
            SourceOffset(10)
        );
        assert_eq!(
            source_offset_for_visual_position(&editor, 2, &bold, bold.visual_text.len()),
            SourceOffset(14)
        );
    }

    #[test]
    fn moving_down_through_forty_lines_scrolls_cursor_to_viewport_bottom() {
        let text = (1..=40)
            .map(|line| format!("line {line:02}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut editor = Editor::new(&text);

        for _ in 0..32 {
            editor
                .dispatch(EditorCommand::MoveDown { extend: false })
                .unwrap();
        }

        let line = editor
            .document()
            .line_for_offset(editor.selection().active)
            .unwrap();
        let heights = HeightIndex::new(std::iter::repeat_n(DEFAULT_THEME.line_height, 40));
        let viewport_height = 722.0;
        let cursor_top = heights.prefix_sum(line.0);
        let scroll_y =
            scroll_y_for_cursor(0.0, cursor_top, DEFAULT_THEME.line_height, viewport_height);

        assert_eq!(line, LineId(32));
        assert_eq!(scroll_y, 136.0);
        assert_eq!(content_top_for_scroll(scroll_y), -136.0);
        assert_eq!(cursor_top - scroll_y, 696.0);
    }

    #[test]
    fn cache_invalidation_only_marks_intersecting_lines() {
        let range = SourceRange::new(10, 20);
        let before = hane_document::Revision(1);
        let after = hane_document::Revision(2);
        let inside = RevisionDelta {
            from_revision: before,
            to_revision: after,
            edited_source_range_before: SourceRange::empty(15),
            edited_source_range_after: SourceRange::new(15, 16),
            byte_delta: 1,
        };
        let before_line = RevisionDelta {
            edited_source_range_before: SourceRange::empty(3),
            edited_source_range_after: SourceRange::new(3, 4),
            ..inside
        };
        assert!(edit_affects_range(inside, range));
        assert!(!edit_affects_range(before_line, range));
    }

    #[test]
    fn local_fallback_tracks_fenced_code_open_and_close_markers() {
        let editor = Editor::new("before\n```rust\nlet answer = 42;\n```\nafter\n");
        let context = local_block_context(editor.document(), 0..6);
        assert_eq!(context.line_is_fenced(0), Some(false));
        for inside in 1..=3 {
            assert_eq!(context.line_is_fenced(inside), Some(true));
        }
        assert_eq!(context.line_is_fenced(4), Some(false));
    }

    #[test]
    fn autosave_rejects_stale_generation_or_revision() {
        assert!(autosave_request_is_current(
            8,
            8,
            Revision(4),
            Revision(4),
            true,
            true,
        ));
        assert!(!autosave_request_is_current(
            9,
            8,
            Revision(4),
            Revision(4),
            true,
            true,
        ));
        assert!(!autosave_request_is_current(
            8,
            8,
            Revision(5),
            Revision(4),
            true,
            true,
        ));
    }

    #[test]
    fn block_context_rejects_stale_revision() {
        assert!(block_context_revision_is_current(Revision(4), Revision(4)));
        assert!(!block_context_revision_is_current(Revision(5), Revision(4)));
    }
}
