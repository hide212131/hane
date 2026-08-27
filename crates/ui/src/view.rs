use crate::actions::install_action_listeners;
use crate::capture::InputCapture;
#[cfg(feature = "instrument")]
use crate::instrument::{Instrumentation, log_summary};
use crate::line::{
    block_font_size, disclosure_for_line, inline_display_for, line_element_from_block,
    presented_line,
};
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
use hane_markdown::{
    BlockContextIndex, BlockIndex, BlockIndexState, BlockIndexUpdate, IndexSource, IndexedBlock,
    local_block_context, parse_block_context,
};
use hane_metrics::FrameMetrics;
use hane_presentation::{BlockWeight, HeightIndex, LineContext, VisualOffset};
use hane_session::{
    DocumentSession, FileService, LoadedFile, OpenDecision, OpenPolicy, OsFileService, RecentFiles,
    SaveDecision, SaveFailure, SaveIntent, SaveOutcome, SaveTicket, SavedFile, SessionId,
    SessionSet, SessionViewState, Settings, StateStores, run_save_job,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
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

fn block_context_revision_is_current(current: Revision, candidate: Revision) -> bool {
    current == candidate
}

/// Identifies the document a background job was started for. A result that
/// comes back for another session, or for a document that has since been
/// replaced, is dropped instead of published.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DocumentKey {
    session: SessionId,
    generation: u64,
}

pub struct EditorView {
    /// Every open document. Holding a set rather than one editor is what lets a
    /// filer add and switch documents without the renderer changing.
    sessions: SessionSet,
    /// The only route to the filesystem. Reads and writes are handed to it on a
    /// background thread, so no file operation sits on the input path.
    files: Arc<dyn FileService>,
    stores: StateStores,
    settings: Settings,
    recent: RecentFiles,
    pub(crate) focus_handle: FocusHandle,
    heights: HeightIndex,
    scroll_y: f32,
    viewport_height: f32,
    pub(crate) metrics: FrameMetrics,
    status: Option<String>,
    theme: Theme,
    #[cfg(feature = "instrument")]
    pub(crate) instrumentation: Instrumentation,
    background_presentation_generation: u64,
    line_cache: HashMap<usize, hane_presentation::VisualBlock>,
    block_context: Option<BlockContextIndex>,
    /// Markdown block boundaries for the current revision. Updated incrementally
    /// on the input path and republished by the background parse; the publish
    /// priority between the two lives in `BlockIndexState`.
    block_index: BlockIndexState,
    document_parse_job_running: bool,
}

impl EditorView {
    pub fn new(text: &str, file_label: impl Into<String>, cx: &mut Context<Self>) -> Self {
        Self::from_sessions(
            SessionSet::with_untitled(text, file_label),
            Arc::new(OsFileService),
            StateStores::from_environment(),
            cx,
        )
    }

    fn from_sessions(
        sessions: SessionSet,
        files: Arc<dyn FileService>,
        stores: StateStores,
        cx: &mut Context<Self>,
    ) -> Self {
        let theme = DEFAULT_THEME;
        let settings = stores.settings().load();
        let recent = stores.recent_files().load();
        let heights = HeightIndex::new(std::iter::repeat_n(
            theme.line_height,
            sessions.active().editor().document().line_count(),
        ));
        Self {
            sessions,
            files,
            stores,
            settings,
            recent,
            focus_handle: cx.focus_handle(),
            heights,
            scroll_y: 0.0,
            viewport_height: theme.line_height,
            metrics: FrameMetrics::new(METRICS_CAPACITY),
            status: None,
            theme,
            #[cfg(feature = "instrument")]
            instrumentation: Instrumentation::from_environment(),
            background_presentation_generation: 0,
            line_cache: HashMap::new(),
            block_context: None,
            block_index: BlockIndexState::new(),
            document_parse_job_running: false,
        }
    }

    pub fn open(path: &Path, cx: &mut Context<Self>) -> std::io::Result<Self> {
        #[cfg(feature = "instrument")]
        let started = Instant::now();
        let files: Arc<dyn FileService> = Arc::new(OsFileService);
        // The first document is read before the window exists, so this one read
        // is synchronous by construction; every later one goes to a thread.
        let loaded = files.load(path)?;
        #[cfg_attr(not(feature = "instrument"), allow(unused_mut))]
        let mut view = Self::from_sessions(
            SessionSet::with_loaded(loaded),
            files,
            StateStores::from_environment(),
            cx,
        );
        view.remember_recent(path);
        cx.add_recent_document(path);
        #[cfg(feature = "instrument")]
        {
            view.instrumentation.file_open_time = started.elapsed();
            view.instrumentation.load_rss_bytes = hane_metrics::process_memory_bytes();
        }
        Ok(view)
    }

    pub fn editor(&self) -> &Editor {
        self.sessions.active().editor()
    }

    pub(crate) fn editor_mut(&mut self) -> &mut Editor {
        self.sessions.active_mut().editor_mut()
    }

    /// The open documents, for a tab strip or filer to render.
    pub fn sessions(&self) -> impl Iterator<Item = &DocumentSession> {
        self.sessions.sessions()
    }

    pub fn active_session(&self) -> &DocumentSession {
        self.sessions.active()
    }

    /// Switches to another open document, carrying the current one's scroll
    /// position with it and rebuilding everything derived from the document.
    pub fn activate_session(&mut self, id: SessionId, cx: &mut Context<Self>) -> bool {
        if id == self.sessions.active_id() {
            return true;
        }
        let scroll_y = self.scroll_y;
        self.sessions
            .active_mut()
            .set_view_state(SessionViewState { scroll_y });
        if !self.sessions.activate(id) {
            return false;
        }
        self.on_document_replaced();
        self.schedule_document_parse(cx);
        cx.notify();
        true
    }

    fn document_key(&self) -> DocumentKey {
        DocumentKey {
            session: self.sessions.active_id(),
            generation: self.sessions.active().generation(),
        }
    }

    /// Rebuilds the view state that only makes sense for one document instance.
    fn on_document_replaced(&mut self) {
        let lines = self.sessions.active().editor().document().line_count();
        self.heights = HeightIndex::new(std::iter::repeat_n(self.theme.line_height, lines));
        self.scroll_y = self.sessions.active().view_state().scroll_y;
        self.line_cache.clear();
        self.block_context = None;
        self.block_index = BlockIndexState::new();
    }

    fn remember_recent(&mut self, path: &Path) {
        self.recent.remember(path);
        if let Err(error) = self.stores.recent_files().store(&self.recent) {
            self.status = Some(format!("Recent files failed: {error}"));
        }
    }

    fn store_settings(&mut self) {
        if let Err(error) = self.stores.settings().store(&self.settings) {
            self.status = Some(format!("Settings failed: {error}"));
        }
    }

    /// Block that owns a physical source line, with the confidence the index has
    /// in it. `None` while no index is published yet, or for a line in a document
    /// that holds no Markdown block at all.
    pub fn block_at_line(&self, line: usize) -> Option<IndexedBlock> {
        block_at_line(self.block_index.index()?, self.editor().document(), line)
    }

    pub(crate) fn after_input(&mut self, cx: &mut Context<Self>) {
        // Keep block boundaries current without waiting for the background parse:
        // this re-parses only the edited window, never the document.
        if let Some(update) = self
            .block_index
            .apply_edits(self.sessions.active().editor().document())
        {
            self.record_block_index_update(&update);
        }
        let lines = self.editor().document().line_count();
        if lines != self.heights.len() {
            self.heights = HeightIndex::new(std::iter::repeat_n(self.theme.line_height, lines));
            self.line_cache.clear();
        }
        self.scroll_cursor_into_view();
        self.schedule_document_parse(cx);
        self.schedule_autosave(cx);
        cx.notify();
    }

    /// Arms the debounce timer for the active session. Each call invalidates the
    /// timer armed by the previous keystroke, so a burst of typing produces one
    /// write at the end rather than one per key.
    fn schedule_autosave(&mut self, cx: &mut Context<Self>) {
        let autosave = self.settings.autosave;
        let session = self.sessions.active_mut();
        session.note_edit();
        let Some(ticket) = session.autosave_ticket(autosave) else {
            return;
        };
        let id = session.id();
        cx.spawn(async move |view, cx| {
            gpui::Timer::after(Duration::from_millis(750)).await;
            let should_save = view
                .read_with(cx, |view, _| {
                    view.sessions.active_id() == id
                        && view
                            .sessions
                            .active()
                            .autosave_is_current(ticket, view.settings.autosave)
                })
                .unwrap_or(false);
            if should_save {
                let _ = view.update(cx, |view, cx| view.save_current(cx));
            }
        })
        .detach();
    }

    pub(crate) fn save_current(&mut self, cx: &mut Context<Self>) {
        self.save_active(SaveIntent::Current, cx);
    }

    pub(crate) fn save_or_prompt(&mut self, cx: &mut Context<Self>) {
        if self.sessions.active().path().is_some() {
            self.save_current(cx);
        } else {
            self.prompt_save_as(cx);
        }
    }

    fn save_active(&mut self, intent: SaveIntent, cx: &mut Context<Self>) {
        self.save_session(self.sessions.active_id(), intent, cx);
    }

    /// Hands one accepted write to the I/O boundary. The session decides whether
    /// there is a write to do at all; the view only reports what happened.
    fn save_session(&mut self, id: SessionId, intent: SaveIntent, cx: &mut Context<Self>) {
        let Some(decision) = self
            .sessions
            .get_mut(id)
            .map(|session| session.request_save(intent))
        else {
            return;
        };
        match decision {
            SaveDecision::NeedsPath => {
                self.status = Some("Use Save As for an untitled document".to_owned());
            }
            SaveDecision::Queued => {
                self.status = Some("Save queued…".to_owned());
            }
            SaveDecision::Write(job) => {
                self.status = Some("Saving…".to_owned());
                let files = self.files.clone();
                let path = job.path.clone();
                let ticket = job.ticket;
                cx.spawn(async move |view, cx| {
                    let result = cx
                        .background_executor()
                        .spawn(async move {
                            run_save_job(files.as_ref(), &job.path, &job.document, job.guard)
                        })
                        .await;
                    let _ = view.update(cx, |view, cx| {
                        view.finish_save(id, ticket, &path, result, cx);
                    });
                })
                .detach();
            }
        }
        cx.notify();
    }

    fn finish_save(
        &mut self,
        id: SessionId,
        ticket: SaveTicket,
        path: &Path,
        result: Result<SavedFile, SaveFailure>,
        cx: &mut Context<Self>,
    ) {
        let Some(outcome) = self
            .sessions
            .get_mut(id)
            .map(|session| session.finish_save(ticket, result))
        else {
            return;
        };
        match outcome {
            SaveOutcome::Saved => {
                self.status = Some("Saved".to_owned());
                self.remember_recent(path);
                cx.add_recent_document(path);
            }
            SaveOutcome::SavedStale => {
                self.status = Some("Saved snapshot; newer edits pending".to_owned());
                self.remember_recent(path);
                cx.add_recent_document(path);
                self.schedule_autosave(cx);
            }
            SaveOutcome::Conflict => {
                self.status = Some(
                    "Save refused: the file changed on disk. Save As, or save again to overwrite"
                        .to_owned(),
                );
            }
            SaveOutcome::Failed(error) => self.status = Some(format!("Save failed: {error}")),
            // The document this write belonged to is gone; nothing to report.
            SaveOutcome::Superseded => {}
        }
        if let Some(pending) = self
            .sessions
            .get_mut(id)
            .and_then(DocumentSession::take_pending_save)
        {
            self.save_session(id, pending, cx);
        }
        cx.notify();
    }

    pub(crate) fn prompt_save_as(&mut self, cx: &mut Context<Self>) {
        let directory = self
            .sessions
            .active()
            .file()
            .directory()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let receiver = cx.prompt_for_new_path(&directory, Some("Untitled.md"));
        cx.spawn(async move |view, cx| match receiver.await {
            Ok(Ok(Some(path))) => {
                let _ = view.update(cx, |view, cx| view.save_active(SaveIntent::To(path), cx));
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
        if self.sessions.active().is_dirty() {
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
                    let _ = view.update(cx, |view, cx| view.open_path(&path, cx));
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

    /// Opens a path the way a filer will: the session set decides whether this
    /// is a switch, a load, or a refusal, and the read itself happens on a
    /// background thread so a large file never blocks typing.
    pub fn open_path(&mut self, path: &Path, cx: &mut Context<Self>) {
        match self.sessions.open_decision(path, OpenPolicy::ReuseActive) {
            OpenDecision::Reject(_) => {
                self.status = Some("Save current changes before opening another file".to_owned());
                cx.notify();
            }
            OpenDecision::Activate(id) => {
                self.activate_session(id, cx);
                self.status = Some("Already open".to_owned());
                cx.notify();
            }
            OpenDecision::Load { into } => {
                self.status = Some("Opening…".to_owned());
                let files = self.files.clone();
                let path = path.to_path_buf();
                cx.spawn(async move |view, cx| {
                    let loaded = cx
                        .background_executor()
                        .spawn({
                            let path = path.clone();
                            async move { files.load(&path) }
                        })
                        .await;
                    let _ = view.update(cx, |view, cx| view.finish_open(into, &path, loaded, cx));
                })
                .detach();
                cx.notify();
            }
        }
    }

    fn finish_open(
        &mut self,
        into: Option<SessionId>,
        path: &Path,
        loaded: std::io::Result<LoadedFile>,
        cx: &mut Context<Self>,
    ) {
        match loaded {
            Err(error) => self.status = Some(format!("Open failed: {error}")),
            Ok(loaded) => {
                // The read took time, and the target session may have been
                // edited in the meantime: re-check before replacing it.
                if into
                    .is_some_and(|id| self.sessions.get(id).is_some_and(DocumentSession::is_dirty))
                {
                    self.status =
                        Some("Save current changes before opening another file".to_owned());
                } else {
                    self.sessions.apply_open(into, loaded);
                    self.on_document_replaced();
                    self.status = Some("Opened".to_owned());
                    self.remember_recent(path);
                    cx.add_recent_document(path);
                    self.schedule_document_parse(cx);
                }
            }
        }
        cx.notify();
    }

    pub(crate) fn toggle_autosave(&mut self, cx: &mut Context<Self>) {
        self.settings.autosave = !self.settings.autosave;
        self.status = Some(format!(
            "Autosave {}",
            if self.settings.autosave { "on" } else { "off" }
        ));
        self.store_settings();
        self.schedule_autosave(cx);
        cx.notify();
    }

    pub(crate) fn cycle_theme(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.settings.theme = self.settings.theme.next();
        self.theme = resolve_theme(self.settings.theme, window.appearance());
        self.heights = HeightIndex::new(std::iter::repeat_n(
            self.theme.line_height,
            self.editor().document().line_count(),
        ));
        self.line_cache.clear();
        self.store_settings();
        cx.notify();
    }

    /// Coalesced background job producing the formal, document-wide parse: the
    /// fenced/table line context the line presenter still consumes, and the
    /// formal `BlockIndex`. One job at a time; a result that no longer matches
    /// the document revision is re-scheduled instead of published.
    fn schedule_document_parse(&mut self, cx: &mut Context<Self>) {
        let revision = self.sessions.active().editor().document().revision();
        let context_is_current = self
            .block_context
            .as_ref()
            .is_some_and(|index| index.revision == revision);
        if context_is_current
            && !self
                .block_index
                .needs_formal_parse(self.sessions.active().editor().document())
        {
            return;
        }
        if self.document_parse_job_running {
            return;
        }
        self.document_parse_job_running = true;
        let key = self.document_key();
        let snapshot = self.editor().document().clone();
        cx.spawn(async move |view, cx| {
            gpui::Timer::after(Duration::from_millis(40)).await;
            let current = view
                .update(cx, |view, _| {
                    view.document_key() == key
                        && block_context_revision_is_current(
                            view.editor().document().revision(),
                            revision,
                        )
                })
                .unwrap_or(false);
            if !current {
                let _ = view.update(cx, |view, cx| {
                    view.document_parse_job_running = false;
                    view.schedule_document_parse(cx);
                });
                return;
            }
            let (context, index) = cx
                .background_executor()
                .spawn(async move {
                    let context = parse_block_context(&snapshot);
                    let index = BlockIndex::from_buffer(&snapshot);
                    (context, index)
                })
                .await;
            let _ = view.update(cx, |view, cx| {
                view.document_parse_job_running = false;
                if view.document_key() != key {
                    view.schedule_document_parse(cx);
                    return;
                }
                // The index carries its own staleness rule, so it can still be
                // rebased onto edits the context index has to be re-run for.
                let document = view.sessions.active().editor().document();
                view.block_index
                    .publish(index, IndexSource::Formal, document);
                if block_context_revision_is_current(
                    view.editor().document().revision(),
                    context.revision,
                ) {
                    view.background_presentation_generation = context.revision.0 + 1;
                    view.block_context = Some(context);
                    view.line_cache.clear();
                    cx.notify();
                } else {
                    view.schedule_document_parse(cx);
                }
            });
        })
        .detach();
    }

    pub(crate) fn report_error(&mut self, operation: &str, error: BufferError) {
        self.status = Some(format!("{operation} rejected: {error}"));
    }

    pub(crate) fn dispatch(&mut self, command: EditorCommand<'_>, cx: &mut Context<Self>) {
        match self.editor_mut().dispatch(command) {
            Ok(_) => self.status = None,
            Err(error) => self.report_error("editor command", error),
        }
        self.after_input(cx);
    }

    pub(crate) fn perform_cancel_composition(&mut self, cx: &mut Context<Self>) {
        if let Err(error) = self.editor_mut().cancel_composition() {
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
            .or_else(|| presented_line(self.editor(), line, LineContext::Normal))
        else {
            return;
        };
        let visual_offset = visual_offset_at_x(&block, event.position.x, self.theme, window);
        let offset = source_offset_for_visual_position(self.editor(), line, &block, visual_offset);
        let selection = if event.modifiers.shift {
            Selection {
                anchor: self.editor().selection().anchor,
                active: offset,
            }
        } else {
            Selection::caret(offset)
        };
        if let Err(error) = self.editor_mut().set_selection(selection) {
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
            .or_else(|| presented_line(self.editor(), line, LineContext::Normal))
        else {
            return;
        };
        let visual = visual_offset_at_x(&block, event.position.x, self.theme, window);
        let offset = source_offset_for_visual_position(self.editor(), line, &block, visual);
        let selection = Selection {
            anchor: self.editor().selection().anchor,
            active: offset,
        };
        if self.editor_mut().set_selection(selection).is_ok() {
            self.after_input(cx);
        }
    }

    fn scroll_cursor_into_view(&mut self) {
        let editor = self.sessions.active().editor();
        let Ok(line) = editor.document().line_for_offset(editor.selection().active) else {
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
        let current_revision = self.editor().document().revision();
        if let Some(mut block) = self.line_cache.remove(&line) {
            let editor = self.sessions.active().editor();
            let expected_disclosure = editor
                .document()
                .line_range(LineId(line))
                .ok()
                .and_then(|range| disclosure_for_line(editor, line, range));
            let reusable = if block.revision == current_revision {
                block.disclosure == expected_disclosure
            } else if let Ok(deltas) = editor.document().deltas_since(block.revision) {
                !deltas
                    .iter()
                    .any(|delta| edit_affects_range(*delta, block.source_range))
                    && block.rebase(&deltas, current_revision)
                    && block.disclosure == expected_disclosure
                    && editor
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
        let block = presented_line(self.sessions.active().editor(), line, context)?;
        self.line_cache.insert(line, block.clone());
        Some(block)
    }
}

/// Resolves a physical source line to the block that owns it. Every byte belongs
/// to exactly one block, so the line's start offset is enough; this is the seam
/// the block-based renderer will pull on in R4A.
fn block_at_line(index: &BlockIndex, document: &RopeBuffer, line: usize) -> Option<IndexedBlock> {
    let range = document.line_range(LineId(line)).ok()?;
    index.block_at(range.start)
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
        self.editor_mut()
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
            self.editor_mut()
                .dispatch(EditorCommand::MoveDown { extend: false })?;
        }
        self.after_input(cx);
        Ok(())
    }

    fn record_block_index_update(&mut self, update: &BlockIndexUpdate) {
        if let Some(output) = &mut self.instrumentation.metrics_output
            && let Err(error) = output.block_index(update)
        {
            eprintln!("could not write block index metrics: {error}");
        }
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

    fn record_block_index_update(&mut self, _update: &BlockIndexUpdate) {}

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
        self.theme = resolve_theme(self.settings.theme, window.appearance());
        self.schedule_document_parse(cx);
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
        let document = self.sessions.active().editor().document();
        let background_context = self.block_context.as_ref().filter(|index| {
            index.revision == document.revision() && index.line_count() == document.line_count()
        });
        // Prefer the formal document-wide index; only fall back to a single
        // bounded scan of the viewport neighborhood while that index is stale.
        let local_context = background_context
            .is_none()
            .then(|| local_block_context(document, visible.clone()));
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
        let revision = self.editor().document().revision().0;
        let bytes = self.editor().document().len_bytes().0;
        let p95 = self
            .metrics
            .painted_percentile(0.95)
            .map_or(0.0, |duration| duration.as_secs_f64() * 1000.0);
        let background = if self.background_presentation_generation > 0 {
            format!(" · bg {}", self.background_presentation_generation)
        } else {
            String::new()
        };
        let dirty = if self.sessions.active().is_dirty() {
            " · modified"
        } else {
            ""
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
        // Relative image destinations resolve against the session's own file,
        // never against the directory the process happens to run in.
        let resolver = self.sessions.active().resource_resolver();
        let editor = self.sessions.active().editor();
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
                            line_element_from_block(editor, line, &block, self.theme, &resolver)
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |view, event, window, cx| {
                                        view.on_line_mouse_down(line, event, window, cx)
                                    }),
                                )
                                .on_mouse_move(cx.listener(move |view, event, window, cx| {
                                    view.on_line_mouse_move(line, event, window, cx)
                                }))
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
        let autosave = if self.settings.autosave {
            "Autosave on"
        } else {
            "Autosave off"
        };
        let theme = format!("Theme {:?}", self.settings.theme);
        let active_path = self.sessions.active().path();
        let recent = self
            .recent
            .entries()
            .iter()
            .filter(|path| Some(path.as_path()) != active_path)
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
                    .on_click(cx.listener(move |view, _, _, cx| view.open_path(&path, cx)))
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
                    .child(self.sessions.active().label())
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
    fn every_line_resolves_to_its_markdown_block() {
        let source = "# title\n\nparagraph one\ncontinued\n\n```rust\nlet x = 1;\n```\n\ntail\n";
        let mut document = RopeBuffer::from_text(source);
        let mut index = BlockIndex::from_buffer(&document);
        let kinds = (0..document.line_count())
            .map(|line| block_at_line(&index, &document, line).map(|block| block.kind))
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec![
                Some(hane_markdown::NodeKind::Heading(1)),
                Some(hane_markdown::NodeKind::Heading(1)),
                Some(hane_markdown::NodeKind::Paragraph),
                Some(hane_markdown::NodeKind::Paragraph),
                Some(hane_markdown::NodeKind::Paragraph),
                Some(hane_markdown::NodeKind::CodeBlock),
                Some(hane_markdown::NodeKind::CodeBlock),
                Some(hane_markdown::NodeKind::CodeBlock),
                Some(hane_markdown::NodeKind::CodeBlock),
                Some(hane_markdown::NodeKind::Paragraph),
                Some(hane_markdown::NodeKind::Paragraph),
            ],
            "each line reports the block it sits in"
        );

        // After an edit the incremental index still answers for every line, and
        // the fenced block still owns its interior lines.
        let base = document.revision();
        document.edit(SourceRange::empty(9), "extra ").unwrap();
        let deltas = document.deltas_since(base).unwrap();
        index.update(&document, &deltas);
        assert_eq!(
            block_at_line(&index, &document, 6).map(|block| block.kind),
            Some(hane_markdown::NodeKind::CodeBlock)
        );
        assert!(
            (0..document.line_count()).all(|line| block_at_line(&index, &document, line).is_some())
        );
    }

    #[test]
    fn block_context_rejects_stale_revision() {
        assert!(block_context_revision_is_current(Revision(4), Revision(4)));
        assert!(!block_context_revision_is_current(Revision(5), Revision(4)));
    }
}
