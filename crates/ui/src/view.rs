#![allow(
    clippy::single_match_else,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    clippy::redundant_closure_for_method_calls,
    clippy::too_many_lines,
    clippy::map_unwrap_or,
    clippy::doc_markdown,
    clippy::needless_pass_by_value,
    clippy::trivially_copy_pass_by_ref,
    clippy::semicolon_if_nothing_returned,
    clippy::unused_self,
    reason = "GPUI view callbacks and pixel geometry require these local framework-bound conventions"
)]

use crate::actions::install_action_listeners;
use crate::capture::InputCapture;
#[cfg(any(feature = "instrument", feature = "timing-probe"))]
use crate::instrument::{Instrumentation, log_summary};
use crate::line::{block_element, disclosure_for_line, presented_block, row_element};
use crate::shape::WindowShaper;
use crate::theme::{DEFAULT_THEME, Theme, resolve_theme};
use gpui::{
    App, Context, FocusHandle, Focusable, InteractiveElement, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, ParentElement, PathPromptOptions, Render, ScrollWheelEvent,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px, rgb,
};
use hane_document::{
    Bias, BufferError, LineId, Revision, RevisionDelta, RopeBuffer, SourceOffset, SourceRange,
    TextBuffer,
};
use hane_editor::{Editor, EditorCommand, InputMeasurement, Selection};
use hane_markdown::{
    BlockId, BlockIndex, BlockIndexState, BlockIndexUpdate, IndexSource, IndexedBlock,
    local_block_index,
};
use hane_metrics::FrameMetrics;
use hane_presentation::{
    BlockLayout, HeightIndex, LineShaper, VerticalMove, VisualBlock, VisualLine, VisualOffset,
    block_heights, block_line_span, layout_block,
};
use hane_session::{
    DocumentSession, DraftId, DraftStore, FileService, LoadedFile, OpenDecision, OpenPolicy,
    OsDraftStore, OsFileService, OsWorkFolderScanner, RecentFiles, RecoveredDrafts, SaveDecision,
    SaveFailure, SaveIntent, SaveOutcome, SaveTicket, SavedFile, SessionId, SessionSet,
    SessionViewState, Settings, StateStores, WorkFolder, WorkFolderScanner, run_save_job,
};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const METRICS_CAPACITY: usize = 4_096;

/// Blocks kept presented on each side of the viewport, so scrolling back a
/// screen does not re-present what was just drawn.
const BLOCK_CACHE_MARGIN: usize = 64;

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

fn clamp_scroll_y(scroll_y: f32, content_height: f32, viewport_height: f32) -> f32 {
    let max_scroll = (content_height - viewport_height).max(0.0);
    scroll_y.clamp(0.0, max_scroll)
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
    /// The Markdown index of the directory this window was opened onto, if
    /// any. `None` keeps single-file editing exactly as it was: no sidebar,
    /// no folder concept anywhere else in the view.
    work_folder: Option<WorkFolder>,
    /// Where a not-yet-named work-folder note's content is journalled, so a
    /// crash before it earns a real filename never loses it. Removed once the
    /// session it belongs to gets a real path or closes.
    draft_store: Arc<dyn DraftStore>,
    work_folder_drafts: HashMap<SessionId, DraftId>,
    /// Paths with a background read in flight, so a second click on a note
    /// that has not finished loading yet does not start a second read.
    loading_paths: HashSet<PathBuf>,
    /// The path most recently asked to be opened, whether or not it has
    /// finished loading yet. A load that lands after a newer request must
    /// not switch what is on screen away from that newer request.
    latest_open_target: Option<PathBuf>,
    pub(crate) focus_handle: FocusHandle,
    heights: HeightIndex,
    scroll_y: f32,
    viewport_height: f32,
    pub(crate) metrics: FrameMetrics,
    status: Option<String>,
    theme: Theme,
    #[cfg(any(feature = "instrument", feature = "timing-probe"))]
    pub(crate) instrumentation: Instrumentation,
    background_presentation_generation: u64,
    /// Presented blocks, keyed by the index's stable block id so an entry
    /// survives typing elsewhere in the document.
    block_cache: HashMap<BlockId, VisualBlock>,
    /// Physical line to the block that drew it, rebuilt each frame. Mouse hit
    /// testing addresses lines, so it needs the reverse of the render mapping.
    line_owners: HashMap<usize, (BlockId, usize)>,
    /// Rows for each presented block, keyed like `block_cache`. Kept across
    /// frames so scrolling back, or typing in another block, does not re-shape
    /// text that has not changed.
    layout_cache: HashMap<BlockId, LayoutCacheEntry>,
    /// Width of the text column the rows were laid out for. A change to it
    /// invalidates every layout, which is why it is recorded rather than
    /// recomputed.
    content_width: f32,
    /// Hash of the window font properties used by `WindowShaper`. Width and
    /// document revision live in each cache entry; this is the remaining global
    /// invalidation generation.
    layout_font_revision: u64,
    /// Where the caret was drawn last frame, relative to the content area. The
    /// IME asks for this to place its candidate window.
    caret_geometry: Option<CaretGeometry>,
    /// Markdown block boundaries for the current revision. Updated incrementally
    /// on the input path and republished by the background parse; the publish
    /// priority between the two lives in `BlockIndexState`.
    block_index: BlockIndexState,
    /// What one entry of `heights` measures. Blocks as soon as an index is
    /// published, physical lines until then.
    granularity: Granularity,
    /// Identity and cheap height input aligned with a block-granularity height
    /// index. This lets an incremental BlockIndex update splice only its changed
    /// run while retaining measured heights on both sides.
    height_blocks: HeightBlocks,
    document_parse_job_running: bool,
}

/// The caret rectangle, in coordinates relative to the top-left of the content
/// area, as the last frame drew it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CaretGeometry {
    pub x: f32,
    pub y: f32,
    pub height: f32,
}

/// The unit `heights` is keyed by.
///
/// Block granularity is the R4A target and the steady state. Line granularity is
/// the startup path: a document-wide index costs a full parse, so the first
/// frames after a document is opened are laid out per line, with block kinds for
/// the viewport coming from a bounded local parse. The renderer itself always
/// draws whole blocks; only what a height entry measures differs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Granularity {
    Blocks,
    Lines,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HeightBlock {
    id: BlockId,
    line_count: usize,
}

const HEIGHT_BLOCK_CHUNK_TARGET: usize = 128;

#[derive(Clone, Debug, Default)]
struct HeightBlocks {
    chunks: Vec<Vec<HeightBlock>>,
    counts: Vec<usize>,
    len: usize,
}

impl FromIterator<HeightBlock> for HeightBlocks {
    fn from_iter<T: IntoIterator<Item = HeightBlock>>(iter: T) -> Self {
        let blocks = iter.into_iter().collect::<Vec<_>>();
        let chunks = blocks
            .chunks(HEIGHT_BLOCK_CHUNK_TARGET)
            .map(<[HeightBlock]>::to_vec)
            .collect();
        let mut this = Self {
            chunks,
            counts: Vec::new(),
            len: blocks.len(),
        };
        this.retree();
        this
    }
}

impl HeightBlocks {
    fn retree(&mut self) {
        self.counts.clear();
        self.counts.push(0);
        self.counts
            .extend(self.chunks.iter().map(|chunk| chunk.len()));
        for index in 1..self.counts.len() {
            let parent = index + (index & index.wrapping_neg());
            if parent < self.counts.len() {
                self.counts[parent] += self.counts[index];
            }
        }
    }

    fn len(&self) -> usize {
        self.len
    }

    fn clear(&mut self) {
        self.chunks.clear();
        self.counts.clear();
        self.len = 0;
    }

    fn locate(&self, ordinal: usize) -> Option<(usize, usize)> {
        if ordinal >= self.len {
            return None;
        }
        let mut chunk = 0;
        let mut before = 0;
        let mut step = 1;
        while step << 1 < self.counts.len() {
            step <<= 1;
        }
        while step > 0 {
            let next = chunk + step;
            if next < self.counts.len() && before + self.counts[next] <= ordinal {
                chunk = next;
                before += self.counts[next];
            }
            step >>= 1;
        }
        Some((chunk, ordinal - before))
    }

    fn locate_insert(&self, ordinal: usize) -> (usize, usize) {
        self.locate(ordinal).unwrap_or_else(|| {
            self.chunks
                .last()
                .map_or((0, 0), |chunk| (self.chunks.len() - 1, chunk.len()))
        })
    }

    fn get(&self, ordinal: usize) -> Option<HeightBlock> {
        let (chunk, slot) = self.locate(ordinal)?;
        self.chunks.get(chunk)?.get(slot).copied()
    }

    fn range_eq(&self, range: Range<usize>, blocks: &[HeightBlock]) -> bool {
        range.len() == blocks.len()
            && blocks
                .iter()
                .enumerate()
                .all(|(at, block)| self.get(range.start + at).as_ref() == Some(block))
    }

    fn splice(&mut self, range: Range<usize>, blocks: &[HeightBlock]) {
        let (first_chunk, first_slot) = self.locate_insert(range.start);
        let (last_chunk, last_slot) = self.locate_insert(range.end);
        let mut merged = Vec::with_capacity(blocks.len() + 2 * HEIGHT_BLOCK_CHUNK_TARGET);
        if let Some(chunk) = self.chunks.get(first_chunk) {
            merged.extend_from_slice(&chunk[..first_slot.min(chunk.len())]);
        }
        merged.extend_from_slice(blocks);
        if let Some(chunk) = self.chunks.get(last_chunk) {
            merged.extend_from_slice(&chunk[last_slot.min(chunk.len())..]);
        }
        let replacement = merged
            .chunks(HEIGHT_BLOCK_CHUNK_TARGET)
            .map(<[HeightBlock]>::to_vec)
            .collect::<Vec<_>>();
        let end_chunk = (last_chunk + 1).min(self.chunks.len());
        self.chunks
            .splice(first_chunk.min(end_chunk)..end_chunk, replacement);
        self.len = self.len - range.len() + blocks.len();
        self.retree();
    }
}

fn rebase_ordinal_after_splice(
    ordinal: usize,
    anchor_id: Option<BlockId>,
    replaced: Range<usize>,
    inserted: &[HeightBlock],
    new_len: usize,
) -> usize {
    if new_len == 0 {
        return 0;
    }
    if ordinal < replaced.start {
        return ordinal;
    }
    if ordinal >= replaced.end {
        return (ordinal - replaced.len() + inserted.len()).min(new_len - 1);
    }
    anchor_id
        .and_then(|id| inserted.iter().position(|block| block.id == id))
        .map_or(replaced.start.min(new_len - 1), |at| replaced.start + at)
}

#[derive(Clone, Debug)]
struct LayoutCacheEntry {
    layout: BlockLayout,
    font_revision: u64,
}

impl LayoutCacheEntry {
    fn is_valid(&self, width: f32, font_revision: u64, revision: Revision) -> bool {
        self.layout.width == width
            && self.font_revision == font_revision
            && self.layout.revision == revision
    }
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
            work_folder: None,
            draft_store: Arc::new(OsDraftStore),
            work_folder_drafts: HashMap::new(),
            loading_paths: HashSet::new(),
            latest_open_target: None,
            focus_handle: cx.focus_handle(),
            heights,
            scroll_y: 0.0,
            viewport_height: theme.line_height,
            metrics: FrameMetrics::new(METRICS_CAPACITY),
            status: None,
            theme,
            #[cfg(any(feature = "instrument", feature = "timing-probe"))]
            instrumentation: Instrumentation::from_environment(),
            background_presentation_generation: 0,
            block_cache: HashMap::new(),
            line_owners: HashMap::new(),
            layout_cache: HashMap::new(),
            content_width: 0.0,
            layout_font_revision: 0,
            caret_geometry: None,
            block_index: BlockIndexState::new(),
            granularity: Granularity::Lines,
            height_blocks: HeightBlocks::default(),
            document_parse_job_running: false,
        }
    }

    /// Opens `path` as the first editor session.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the document or persisted state cannot be read.
    pub fn open(path: &Path, cx: &mut Context<Self>) -> std::io::Result<Self> {
        #[cfg(any(feature = "instrument", feature = "timing-probe"))]
        let started = Instant::now();
        let files: Arc<dyn FileService> = Arc::new(OsFileService);
        // The first document is read before the window exists, so this one read
        // is synchronous by construction; every later one goes to a thread.
        let loaded = files.load(path)?;
        #[cfg_attr(
            not(any(feature = "instrument", feature = "timing-probe")),
            allow(unused_mut)
        )]
        let mut view = Self::from_sessions(
            SessionSet::with_loaded(loaded),
            files,
            StateStores::from_environment(),
            cx,
        );
        view.remember_recent(path);
        cx.add_recent_document(path);
        #[cfg(any(feature = "instrument", feature = "timing-probe"))]
        {
            view.instrumentation.file_open_time = started.elapsed();
            view.instrumentation.load_rss_bytes = hane_metrics::process_memory_bytes();
        }
        Ok(view)
    }

    /// Opens `root` as a work folder. The window is created immediately with
    /// an empty untitled session; the directory scan and the first note's
    /// load both happen on a background thread, so a folder with many entries
    /// or a large first document never delays the window from appearing, the
    /// same way every later switch does not block on I/O.
    pub fn open_work_folder(root: &Path, cx: &mut Context<Self>) -> Self {
        let mut view = Self::new("", "Untitled", cx);
        view.begin_work_folder_scan(root.to_path_buf(), cx);
        view
    }

    fn begin_work_folder_scan(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.status = Some("Opening work folder…".to_owned());
        let draft_store = self.draft_store.clone();
        cx.spawn(async move |view, cx| {
            let scan_root = root.clone();
            let scanned = cx
                .background_executor()
                .spawn(async move {
                    let work_folder = OsWorkFolderScanner.scan(&scan_root);
                    let drafts = draft_store.recover(&scan_root);
                    (work_folder, drafts)
                })
                .await;
            let _ = view.update(cx, |view, cx| view.finish_work_folder_scan(scanned, cx));
        })
        .detach();
        cx.notify();
    }

    fn finish_work_folder_scan(
        &mut self,
        scanned: (
            std::io::Result<WorkFolder>,
            std::io::Result<RecoveredDrafts>,
        ),
        cx: &mut Context<Self>,
    ) {
        let (work_folder, drafts) = scanned;
        match work_folder {
            Err(error) => {
                self.status = Some(format!("Could not open work folder: {error}"));
            }
            Ok(work_folder) => {
                let first = work_folder
                    .entries()
                    .first()
                    .map(|entry| entry.path().to_path_buf());
                // The window still holds the empty untitled session `new`
                // created above; it stays clean and in place while any
                // recovered drafts are installed alongside it, so switching
                // to the first real note below can still reuse it.
                let initial_session = self.sessions.active_id();
                self.work_folder = Some(work_folder);

                // A recovery failure must not look like the drafts were
                // simply gone: `OsDraftStore::recover` already keeps
                // whatever individual files it could read, so surface the
                // error (or, short of that, how many entries could not be
                // read) instead of silently falling back to an empty list
                // and clearing status as if nothing happened.
                let mut last_recovered = None;
                match drafts {
                    Ok(drafts) => {
                        for draft in drafts.drafts {
                            let id = self.sessions.open_untitled(&draft.text, "Untitled");
                            self.work_folder_drafts.insert(id, draft.id);
                            last_recovered = Some(id);
                        }
                        self.status = (drafts.failed > 0).then(|| {
                            format!(
                                "{} unsaved draft{} could not be recovered",
                                drafts.failed,
                                if drafts.failed == 1 { "" } else { "s" }
                            )
                        });
                    }
                    Err(error) => {
                        self.status = Some(format!("Could not recover unsaved drafts: {error}"));
                    }
                }

                if let Some(path) = first {
                    // Opening the first entry replaces whichever session is
                    // active through the same background-loading path as any
                    // other open, instead of leaving a spare empty session
                    // around; that must be the original clean one, not
                    // whichever draft was installed last.
                    self.sessions.activate(initial_session);
                    self.open_path(&path, cx);
                } else if last_recovered.is_some() {
                    self.on_document_replaced();
                    self.schedule_document_parse(cx);
                }
            }
        }
        cx.notify();
    }

    #[must_use]
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

    #[must_use]
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
        self.granularity = Granularity::Lines;
        self.heights = HeightIndex::new(std::iter::repeat_n(self.theme.line_height, lines));
        self.height_blocks.clear();
        self.scroll_y = self.sessions.active().view_state().scroll_y;
        self.block_cache.clear();
        self.line_owners.clear();
        self.layout_cache.clear();
        self.caret_geometry = None;
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
    #[must_use]
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
            if !self.apply_height_index_update(&update) {
                self.resync_heights();
            }
        } else {
            self.resync_heights();
        }
        self.scroll_cursor_into_view();
        self.schedule_document_parse(cx);
        self.schedule_autosave(cx);
        self.schedule_draft_save(cx);
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

    /// Journals the active session's text into the recovery drafts on the
    /// same debounce cadence as autosave: a no-op unless the active session
    /// is an unnamed note in the current work folder. Kept separate from
    /// `schedule_autosave` because an unnamed note has no path to write to
    /// yet and must not wait for one to earn crash safety.
    ///
    /// The scheduled save targets the session it was armed for by id, not
    /// whichever session is active when the timer fires: switching to
    /// another note within the debounce window must not cancel the write, or
    /// edits made just before switching away are lost on a crash until the
    /// draft is revisited and edited again.
    fn schedule_draft_save(&mut self, cx: &mut Context<Self>) {
        let id = self.sessions.active_id();
        let Some(&draft_id) = self.work_folder_drafts.get(&id) else {
            return;
        };
        let Some(root) = self
            .work_folder
            .as_ref()
            .map(|folder| folder.root().to_path_buf())
        else {
            return;
        };
        let revision = self.sessions.active().revision();
        let draft_store = self.draft_store.clone();
        cx.spawn(async move |view, cx| {
            gpui::Timer::after(Duration::from_millis(750)).await;
            let text = view
                .read_with(cx, |view, _| {
                    let session = view.sessions.get(id)?;
                    let current =
                        session.revision() == revision && view.work_folder_drafts.contains_key(&id);
                    current.then(|| session.editor().document().full_text())
                })
                .ok()
                .flatten();
            if let Some(text) = text {
                let _ = cx
                    .background_executor()
                    .spawn(async move { draft_store.write(&root, draft_id, &text) })
                    .await;
            }
        })
        .detach();
    }

    /// Issue #5: starts a brand-new, unnamed note in the current work folder.
    /// No filename prompt: it opens blank and ready for input immediately,
    /// and is journalled into the recovery drafts as soon as it holds
    /// anything, so a crash before it earns a real name never loses it.
    pub fn new_work_folder_note(&mut self, cx: &mut Context<Self>) {
        if self.work_folder.is_none() {
            return;
        }
        let scroll_y = self.scroll_y;
        self.sessions
            .active_mut()
            .set_view_state(SessionViewState { scroll_y });
        let id = self.sessions.open_untitled("", "Untitled");
        self.work_folder_drafts.insert(id, DraftId::generate());
        self.on_document_replaced();
        self.schedule_document_parse(cx);
        self.status = None;
        cx.notify();
    }

    /// A note stops being a draft once it earns a real path, whether through
    /// the (future) H1-derived rename or a manual Save As. The recovery
    /// journal entry is removed on a background thread; a failure here just
    /// leaves a harmless leftover file, never lost content.
    fn retire_work_folder_draft(&mut self, id: SessionId, cx: &mut Context<Self>) {
        let Some(draft_id) = self.work_folder_drafts.remove(&id) else {
            return;
        };
        let Some(root) = self
            .work_folder
            .as_ref()
            .map(|folder| folder.root().to_path_buf())
        else {
            return;
        };
        let draft_store = self.draft_store.clone();
        cx.background_executor()
            .spawn(async move {
                let _ = draft_store.remove(&root, draft_id);
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
                self.retire_work_folder_draft(id, cx);
            }
            SaveOutcome::SavedStale => {
                self.status = Some("Saved snapshot; newer edits pending".to_owned());
                self.remember_recent(path);
                cx.add_recent_document(path);
                self.schedule_autosave(cx);
                self.retire_work_folder_draft(id, cx);
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
        self.open_with_policy(path, OpenPolicy::ReuseActive, cx);
    }

    /// Opens a work folder sidebar entry: an already-open note is reused, and
    /// an unloaded one is loaded into a session of its own, so switching notes
    /// never asks the user to save whatever else happens to be open.
    pub fn open_work_folder_entry(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.open_with_policy(path, OpenPolicy::NewSession, cx);
    }

    fn open_with_policy(&mut self, path: &Path, policy: OpenPolicy, cx: &mut Context<Self>) {
        // Whichever path was asked for most recently is what the user wants
        // to see; a load that lands after a newer request must not steal
        // focus back to what it was asked for.
        self.latest_open_target = Some(path.to_path_buf());
        match self.sessions.open_decision(path, policy) {
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
                if !self.loading_paths.insert(path.to_path_buf()) {
                    // Already loading this path: the in-flight read will
                    // apply once it lands, and `latest_open_target` above is
                    // enough to make it win if nothing newer was requested
                    // meanwhile. Starting a second read here is what used to
                    // produce two sessions for one file.
                    cx.notify();
                    return;
                }
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
        self.loading_paths.remove(path);
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
                    // A newer request may have been made and even resolved
                    // while this one was in flight; only the load that still
                    // matches what was most recently asked for is allowed to
                    // take over what is on screen.
                    let is_latest_request = self.latest_open_target.as_deref() == Some(path);
                    let previously_active = self.sessions.active_id();
                    // `ReuseActive` always targets whatever session was active
                    // when the request was made, so a stale completion here
                    // can name the very session a newer, already-applied
                    // completion put on screen. Overwriting it would corrupt
                    // what the user is now looking at with no way back, so a
                    // stale result that targets the current active session is
                    // discarded instead of applied.
                    if !is_latest_request && into == Some(previously_active) {
                        self.status =
                            Some("A newer document is open; this load was discarded".to_owned());
                    } else {
                        if is_latest_request {
                            let scroll_y = self.scroll_y;
                            self.sessions
                                .active_mut()
                                .set_view_state(SessionViewState { scroll_y });
                        }
                        self.sessions.apply_open(into, loaded);
                        self.remember_recent(path);
                        cx.add_recent_document(path);
                        if is_latest_request {
                            self.on_document_replaced();
                            self.status = Some("Opened".to_owned());
                            self.schedule_document_parse(cx);
                        } else {
                            // The session now holds the loaded document and is
                            // ready to be reused instantly next time it is
                            // selected, but it must not visibly replace
                            // whatever the user has since switched to.
                            self.sessions.activate(previously_active);
                        }
                    }
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
        self.block_cache.clear();
        self.layout_cache.clear();
        self.heights = HeightIndex::new(self.item_heights());
        self.store_settings();
        cx.notify();
    }

    /// Coalesced background job producing the formal, document-wide `BlockIndex`.
    /// One job at a time; a result that no longer matches the document revision
    /// is rebased or re-scheduled rather than published stale.
    fn schedule_document_parse(&mut self, cx: &mut Context<Self>) {
        if !self
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
        let revision = self.sessions.active().editor().document().revision();
        let line_height = self.theme.line_height;
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
            // Sizing the height index is proportional to the block count, so it
            // is done here rather than on the main thread: for a 100 MB document
            // that is tens of milliseconds that would otherwise land in one
            // frame.
            let (index, heights) = cx
                .background_executor()
                .spawn(async move {
                    let index = BlockIndex::from_buffer(&snapshot);
                    let heights = HeightIndex::new(block_heights(&snapshot, &index, line_height));
                    (index, heights)
                })
                .await;
            let _ = view.update(cx, |view, cx| {
                view.document_parse_job_running = false;
                if view.document_key() != key {
                    view.schedule_document_parse(cx);
                    return;
                }
                let document = view.sessions.active().editor().document();
                view.block_index
                    .publish(index, IndexSource::Formal, document);
                view.background_presentation_generation = revision.0 + 1;
                // Formal boundaries can disagree with what the bounded local
                // parse showed, so every cached presentation is re-derived once.
                view.block_cache.clear();
                let (granularity, len) = view.desired_layout();
                if granularity == Granularity::Blocks && len == heights.len() {
                    view.install_heights(granularity, heights);
                } else {
                    // The parse was rebased onto edits made while it ran, so the
                    // block count moved and the prepared heights no longer fit.
                    view.resync_heights();
                }
                cx.notify();
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
        self.scroll_y = clamp_scroll_y(
            self.scroll_y - f32::from(delta.y),
            self.heights.total_height(),
            self.viewport_height,
        );
        cx.notify();
    }

    /// The presented line under a mouse event, from the mapping the last frame
    /// recorded. Only rendered lines can be clicked, so a miss means the frame
    /// moved under the pointer and there is nothing to do.
    fn rendered_line(&self, line: usize) -> Option<VisualLine> {
        let (id, at) = self.line_owners.get(&line)?;
        self.block_cache.get(id)?.lines.get(*at).cloned()
    }

    /// The source offset a click lands on, inside one row of a presented line.
    /// Only that row's own stretch of text is measured, so a click past the end
    /// of a soft-wrapped row cannot reach text drawn on the next one.
    fn offset_at_row_x(
        &self,
        line: usize,
        fragment: Range<usize>,
        window_x: f32,
        window: &Window,
    ) -> Option<SourceOffset> {
        let visual = self.rendered_line(line)?;
        let x = window_x - self.theme.line_horizontal_padding;
        let visual_offset = WindowShaper::new(window).offset_for_x(&visual, fragment, x);
        Some(source_offset_for_visual_position(
            self.editor(),
            line,
            &visual,
            visual_offset,
        ))
    }

    fn on_row_mouse_down(
        &mut self,
        line: usize,
        fragment: Range<usize>,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle);
        let Some(offset) =
            self.offset_at_row_x(line, fragment, f32::from(event.position.x), window)
        else {
            return;
        };
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

    fn on_row_mouse_move(
        &mut self,
        line: usize,
        fragment: Range<usize>,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !event.dragging() {
            return;
        }
        let Some(offset) =
            self.offset_at_row_x(line, fragment, f32::from(event.position.x), window)
        else {
            return;
        };
        let selection = Selection {
            anchor: self.editor().selection().anchor,
            active: offset,
        };
        if self.editor_mut().set_selection(selection).is_ok() {
            self.after_input(cx);
        }
    }

    /// Moves the caret one row up or down.
    ///
    /// A row is not a source line: a wrapped paragraph has several, and a
    /// heading that fits has one. The target is therefore resolved against the
    /// layout, aiming at the x of the caret when the run of vertical moves
    /// started. Without block boundaries — the first frames after a document is
    /// opened — there is no layout to resolve against, and the source-line move
    /// stands in so the caret is never stuck.
    pub(crate) fn move_vertical(
        &mut self,
        down: bool,
        extend: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let shaper = WindowShaper::new(window);
        if self.move_vertical_by_layout(down, extend, &shaper) {
            self.after_input(cx);
        } else if down {
            self.dispatch(EditorCommand::MoveDown { extend }, cx);
        } else {
            self.dispatch(EditorCommand::MoveUp { extend }, cx);
        }
    }

    /// Resolves and applies one vertical move against the layout. Returns false
    /// when the caret's block cannot be laid out, which is the caller's signal
    /// to fall back.
    fn move_vertical_by_layout(
        &mut self,
        down: bool,
        extend: bool,
        shaper: &dyn LineShaper,
    ) -> bool {
        let caret = self.editor().selection().active;
        let Some((block, layout)) = self.layout_around(caret, shaper) else {
            return false;
        };
        let Some(x) = self.editor().preferred_visual_x().or_else(|| {
            layout
                .point_for_source(&block, caret, shaper)
                .map(|point| point.x)
        }) else {
            return false;
        };
        let target = match layout.vertical_target(&block, caret, down, x, shaper) {
            VerticalMove::To(offset) => Some(offset),
            VerticalMove::PastEdge => self.neighbor_row_target(&block, down, x, shaper),
            VerticalMove::Unknown => return false,
        };
        // No neighbor means the caret is on the first or last row of the
        // document: staying put is the move.
        if let Some(target) = target
            && let Err(error) = self.editor_mut().move_vertical_to(target, extend, x)
        {
            self.report_error("vertical move", error);
        }
        true
    }

    /// The block holding `offset`, laid out, with the line above and below it so
    /// one vertical step always lands on a row that exists.
    ///
    /// The caret is nearly always inside a block the last frame drew, so this
    /// usually costs neither a parse nor a shape. When it is not — the caret was
    /// just moved off screen — only three lines of the block are presented, which
    /// matters because a block can be the whole document.
    fn layout_around(
        &self,
        offset: SourceOffset,
        shaper: &dyn LineShaper,
    ) -> Option<(VisualBlock, BlockLayout)> {
        let indexed = self.block_at_offset(offset)?;
        let line = self.editor().document().line_for_offset(offset).ok()?.0;
        let window = line.saturating_sub(1)..line + 2;
        let drawn = self
            .block_cache
            .get(&indexed.id)
            .filter(|block| block.matches(&indexed) && block.covers(&window))
            .filter(|block| self.disclosures_are_current(block));
        if let Some(block) = drawn
            && let Some(entry) = self.layout_cache.get(&indexed.id).filter(|entry| {
                entry.is_valid(
                    self.content_width,
                    self.layout_font_revision,
                    block.revision,
                )
            })
        {
            return Some((block.clone(), entry.layout.clone()));
        }
        let visual = presented_block(self.editor(), &indexed, &window)?;
        let layout = layout_block(&visual, self.content_width, shaper);
        Some((visual, layout))
    }

    /// The caret target on the last row of the block above, or the first row of
    /// the block below.
    fn neighbor_row_target(
        &self,
        block: &VisualBlock,
        down: bool,
        x: f32,
        shaper: &dyn LineShaper,
    ) -> Option<SourceOffset> {
        neighbor_row_target(
            self.editor(),
            self.current_index(),
            block,
            down,
            x,
            self.content_width,
            shaper,
        )
    }

    /// Block boundaries around one source offset: the formal index when it
    /// describes the current revision, and a bounded local parse otherwise, the
    /// same two sources the renderer draws from.
    fn block_at_offset(&self, offset: SourceOffset) -> Option<IndexedBlock> {
        block_at_offset(self.current_index(), self.editor().document(), offset)
    }

    /// The caret rectangle the last frame drew, for the IME candidate window.
    pub(crate) fn caret_geometry(&self) -> Option<CaretGeometry> {
        self.caret_geometry
    }

    /// Scrolls so the row holding the caret is on screen.
    ///
    /// The row is the exact answer and the layout cache holds it whenever the
    /// caret's block has been drawn at the current revision, which is the case
    /// while moving around. Right after an edit the layout is a revision behind,
    /// and the caret's physical line stands in for its row — the same thing
    /// wherever nothing wraps.
    fn scroll_cursor_into_view(&mut self) {
        let editor = self.sessions.active().editor();
        let cursor = editor.selection().active;
        let Ok(line) = editor.document().line_for_offset(cursor) else {
            return;
        };
        let (top, height) = match self.granularity {
            Granularity::Lines => (self.heights.prefix_sum(line.0), self.theme.line_height),
            Granularity::Blocks => {
                let Some(block) = self
                    .current_index()
                    .and_then(|index| index.block_at(cursor))
                else {
                    return;
                };
                let block_top = self.heights.prefix_sum(block.ordinal);
                let row = self
                    .layout_cache
                    .get(&block.id)
                    .filter(|entry| {
                        entry.is_valid(
                            self.content_width,
                            self.layout_font_revision,
                            editor.document().revision(),
                        )
                    })
                    .and_then(|entry| entry.layout.row_bounds_for_source(cursor));
                match row {
                    Some((y, height)) => (block_top + y, height),
                    None => {
                        let first = block_line_span(editor.document(), &block)
                            .map_or(line.0, |span| span.start);
                        let inside = line.0.saturating_sub(first) as f32 * self.theme.line_height;
                        (block_top + inside, self.theme.line_height)
                    }
                }
            }
        };
        self.scroll_y = scroll_y_for_cursor(self.scroll_y, top, height, self.viewport_height);
    }

    /// The published index, but only while it describes the current revision.
    /// Between an edit and the incremental update that follows it, and after an
    /// edit history gap drops the index, there is none.
    fn current_index(&self) -> Option<&BlockIndex> {
        self.block_index
            .index()
            .filter(|index| index.revision() == self.editor().document().revision())
            .filter(|index| !index.is_empty())
    }

    /// What `heights` should measure, and how many entries it needs.
    fn desired_layout(&self) -> (Granularity, usize) {
        self.current_index().map_or_else(
            || (Granularity::Lines, self.editor().document().line_count()),
            |index| (Granularity::Blocks, index.len()),
        )
    }

    /// Initial height of every entry, from the line height alone. Measured
    /// heights replace these as blocks are drawn; R4C keeps them across rebuilds.
    fn item_heights(&self) -> Vec<f32> {
        let line_height = self.theme.line_height;
        let (granularity, len) = self.desired_layout();
        match granularity {
            Granularity::Lines => vec![line_height; len],
            Granularity::Blocks => {
                let document = self.sessions.active().editor().document();
                let index = self
                    .current_index()
                    .expect("block granularity has an index");
                block_heights(document, index, line_height)
            }
        }
    }

    /// Keeps `heights` keyed to the same thing the renderer enumerates. This is
    /// the full synchronization path used for startup and index publication;
    /// input uses `apply_height_index_update` to touch only its parse window.
    fn resync_heights(&mut self) {
        let (granularity, len) = self.desired_layout();
        if granularity == self.granularity && len == self.heights.len() {
            return;
        }
        let heights = HeightIndex::new(self.item_heights());
        self.install_heights(granularity, heights);
    }

    /// Swaps in a height index, keeping the reader where they were: the scroll
    /// position is carried across on the source offset at the top of the
    /// viewport, read before the swap and resolved after it.
    fn install_heights(&mut self, granularity: Granularity, heights: HeightIndex) {
        let anchor = self.top_source_offset();
        let intra = (!self.heights.is_empty()).then(|| {
            let item = self.heights.block_at_y(self.scroll_y);
            self.scroll_y - self.heights.prefix_sum(item)
        });
        self.granularity = granularity;
        self.heights = heights;
        self.height_blocks = if granularity == Granularity::Blocks {
            self.current_index()
                .map(|index| {
                    index
                        .blocks()
                        .map(|block| HeightBlock {
                            id: block.id,
                            line_count: block.line_count,
                        })
                        .collect()
                })
                .unwrap_or_default()
        } else {
            HeightBlocks::default()
        };
        self.scroll_y = anchor.map_or(self.scroll_y, |offset| {
            let top = self.scroll_for_offset(offset);
            let item = self.heights.block_at_y(top);
            let inside = intra
                .zip(self.heights.height(item))
                .map_or(0.0, |(intra, height)| intra.clamp(0.0, height));
            top + inside
        });
    }

    /// Applies the small splice reported by the incremental block index. The
    /// common case (typing without changing block boundaries or line count)
    /// compares a handful of entries and leaves the height tree untouched.
    fn apply_height_index_update(&mut self, update: &BlockIndexUpdate) -> bool {
        if self.granularity != Granularity::Blocks {
            return false;
        }
        let first = update.first_replaced_block;
        let old_end = first.saturating_add(update.replaced_blocks);
        if old_end > self.height_blocks.len() {
            return false;
        }
        let Some(index) = self.current_index() else {
            return false;
        };
        if index.len() != self.height_blocks.len() - update.replaced_blocks + update.inserted_blocks
        {
            return false;
        }
        let next = (first..first + update.inserted_blocks)
            .filter_map(|ordinal| index.block(ordinal))
            .map(|block| HeightBlock {
                id: block.id,
                line_count: block.line_count,
            })
            .collect::<Vec<_>>();
        if next.len() != update.inserted_blocks {
            return false;
        }
        if self.height_blocks.range_eq(first..old_end, &next) {
            return true;
        }
        let old_top = (!self.heights.is_empty()).then(|| {
            let ordinal = self.heights.block_at_y(self.scroll_y);
            let id = self.height_blocks.get(ordinal).map(|block| block.id);
            let intra = self.scroll_y - self.heights.prefix_sum(ordinal);
            (id, ordinal, intra)
        });
        self.heights.splice(
            first..old_end,
            next.iter()
                .map(|block| self.theme.line_height * block.line_count as f32),
        );
        self.height_blocks.splice(first..old_end, &next);

        if let Some((id, old_ordinal, intra)) = old_top {
            let ordinal = rebase_ordinal_after_splice(
                old_ordinal,
                id,
                first..old_end,
                &next,
                self.heights.len(),
            );
            let inside = self
                .heights
                .height(ordinal)
                .map_or(0.0, |height| intra.clamp(0.0, height));
            self.scroll_y = self.heights.prefix_sum(ordinal) + inside;
        }
        true
    }

    /// Source offset of the item currently at the top of the viewport.
    fn top_source_offset(&self) -> Option<SourceOffset> {
        if self.heights.is_empty() {
            return None;
        }
        let item = self.heights.block_at_y(self.scroll_y);
        match self.granularity {
            Granularity::Lines => self
                .editor()
                .document()
                .line_range(LineId(item))
                .ok()
                .map(|range| range.start),
            Granularity::Blocks => self
                .current_index()
                .and_then(|index| index.block(item))
                .map(|block| block.source_range.start),
        }
    }

    fn scroll_for_offset(&self, offset: SourceOffset) -> f32 {
        match self.granularity {
            Granularity::Lines => self
                .editor()
                .document()
                .line_for_offset(offset)
                .map(|line| line.0)
                .ok()
                .map_or(self.scroll_y, |line| self.heights.prefix_sum(line)),
            Granularity::Blocks => {
                let document = self.editor().document();
                let Some(block) = self
                    .current_index()
                    .and_then(|index| index.block_at(offset))
                else {
                    return self.scroll_y;
                };
                let first_line = block_line_span(document, &block).map_or(0, |span| span.start);
                let line = document
                    .line_for_offset(offset)
                    .map_or(first_line, |line| line.0);
                self.heights.prefix_sum(block.ordinal)
                    + line.saturating_sub(first_line) as f32 * self.drawn_line_height(&block)
            }
        }
    }

    /// The blocks covering the viewport. `visible` is a range of height entries,
    /// which at block granularity are ordinals and until an index exists are
    /// physical lines.
    fn visible_blocks(&self, visible: Range<usize>) -> Vec<IndexedBlock> {
        match self.granularity {
            Granularity::Blocks => {
                let Some(index) = self.current_index() else {
                    return Vec::new();
                };
                visible.filter_map(|ordinal| index.block(ordinal)).collect()
            }
            Granularity::Lines => {
                let document = self.sessions.active().editor().document();
                let last_line = document.line_count().saturating_sub(1);
                let first = visible.start.min(last_line);
                let last = visible.end.saturating_sub(1).min(last_line);
                let (Ok(first_range), Ok(last_range)) = (
                    document.line_range(LineId(first)),
                    document.line_range(LineId(last)),
                ) else {
                    return Vec::new();
                };
                // One bounded parse of the viewport neighborhood, never the
                // document: this runs on the render path.
                let local = local_block_index(document, first..last + 1);
                let span = SourceRange::new(
                    first_range.start.0,
                    last_range.end.0.max(first_range.start.0),
                );
                let blocks = local.blocks_in(span).collect::<Vec<_>>();
                if blocks.is_empty() {
                    // A document with no Markdown block at all still has a line
                    // to draw the caret on.
                    let empty = IndexedBlock::provisional_paragraph(
                        document.revision(),
                        SourceRange::new(first_range.start.0, last_range.end.0),
                        last - first + 1,
                    );
                    return vec![empty];
                }
                blocks
            }
        }
    }

    /// Document lines the viewport shows, which is what each block presents the
    /// intersection with.
    ///
    /// At line granularity the visible range already is that window. At block
    /// granularity it has to be read back out of the scroll geometry, because a
    /// block can be taller than the viewport — a document with no blank line in
    /// it is one block. How tall a line is depends on how often it wraps, so the
    /// block's own laid-out rows answer it where they exist and the line height
    /// stands in where they do not; the overscan absorbs the difference.
    fn visible_line_window(&self, blocks: &[IndexedBlock], items: &Range<usize>) -> Range<usize> {
        match self.granularity {
            Granularity::Lines => items.clone(),
            Granularity::Blocks => {
                let document = self.sessions.active().editor().document();
                let top = (self.scroll_y - self.theme.overscan).max(0.0);
                let bottom = self.scroll_y + self.viewport_height + self.theme.overscan;
                let (Some(first), Some(last)) = (blocks.first(), blocks.last()) else {
                    return 0..0;
                };
                let (Some(first_span), Some(last_span)) = (
                    block_line_span(document, first),
                    block_line_span(document, last),
                ) else {
                    return 0..0;
                };
                let into_block = |y: f32, block: &IndexedBlock| {
                    ((y - self.heights.prefix_sum(block.ordinal)) / self.drawn_line_height(block))
                        .max(0.0)
                };
                let start = first_span.start + into_block(top, first).floor() as usize;
                let end = last_span.start + into_block(bottom, last).ceil() as usize + 1;
                let start = start.min(first_span.end.saturating_sub(1));
                start..end.max(start + 1).min(last_span.end)
            }
        }
    }

    /// How tall one line of a block draws, on average. A wrapped line occupies
    /// several rows, so the plain line height under-counts it; the block's own
    /// layout says by how much once it has been drawn once.
    fn drawn_line_height(&self, block: &IndexedBlock) -> f32 {
        self.layout_cache
            .get(&block.id)
            .filter(|entry| {
                entry.layout.width == self.content_width
                    && entry.font_revision == self.layout_font_revision
            })
            .and_then(|entry| entry.layout.average_line_height())
            .unwrap_or(self.theme.line_height)
            .max(1.0)
    }

    /// Presents the part of a block that reaches `visible`, reusing the cached
    /// presentation when the document has not touched it, the index still
    /// describes it the same way, and it already covers the wanted lines.
    ///
    /// Reports whether the presentation was reused, because the rows laid out
    /// from it can be reused exactly when it was.
    fn cached_block(
        &mut self,
        block: &IndexedBlock,
        visible: &Range<usize>,
    ) -> Option<(VisualBlock, bool)> {
        let revision = self.editor().document().revision();
        if let Some(mut cached) = self.block_cache.remove(&block.id) {
            let editor = self.sessions.active().editor();
            let reusable = if cached.revision == revision {
                true
            } else if let Ok(deltas) = editor.document().deltas_since(cached.revision) {
                !deltas
                    .iter()
                    .any(|delta| edit_affects_range(*delta, cached.source_range))
                    && cached.rebase(&deltas, revision)
                    // The rows describe the same text at shifted offsets, so
                    // they move with the block rather than being rebuilt.
                    && self
                        .layout_cache
                        .get_mut(&block.id)
                        .is_none_or(|entry| entry.layout.rebase(&deltas, revision))
            } else {
                false
            };
            if reusable
                && cached.matches(block)
                && cached.covers(visible)
                && self.disclosures_are_current(&cached)
            {
                self.block_cache.insert(block.id, cached.clone());
                return Some((cached, true));
            }
        }
        let presented = presented_block(self.sessions.active().editor(), block, visible)?;
        self.block_cache.insert(block.id, presented.clone());
        Some((presented, false))
    }

    /// Rows for a presented block, shaped only when the presentation is new or
    /// the text column changed width.
    fn block_layout(
        &mut self,
        block: &VisualBlock,
        reused: bool,
        shaper: &dyn LineShaper,
    ) -> BlockLayout {
        if reused
            && let Some(cached) = self.layout_cache.get(&block.id)
            && cached.is_valid(
                self.content_width,
                self.layout_font_revision,
                block.revision,
            )
        {
            #[cfg(any(feature = "instrument", feature = "timing-probe"))]
            {
                self.instrumentation.layout_cache_hits += 1;
            }
            return cached.layout.clone();
        }
        #[cfg(any(feature = "instrument", feature = "timing-probe"))]
        {
            self.instrumentation.layout_cache_misses += 1;
        }
        let layout = layout_block(block, self.content_width, shaper);
        self.layout_cache.insert(
            block.id,
            LayoutCacheEntry {
                layout: layout.clone(),
                font_revision: self.layout_font_revision,
            },
        );
        layout
    }

    /// True while every line of a cached block still discloses what the caret,
    /// selection and IME say it should. Cheap: a block holds a handful of lines.
    fn disclosures_are_current(&self, block: &VisualBlock) -> bool {
        let editor = self.sessions.active().editor();
        block.lines.iter().all(|line| {
            let expected = editor
                .document()
                .line_range(LineId(line.line_id as usize))
                .ok()
                .filter(|range| *range == line.source_range)
                .and_then(|range| disclosure_for_line(editor, line.line_id as usize, range));
            expected == line.disclosure
        })
    }
}

/// The caret target one row into the next or previous block, aiming at `x`.
///
/// Only the one line of the neighbor that the caret can land on is presented, so
/// stepping off the edge of a block costs the same whatever the neighbor's size.
fn neighbor_row_target(
    editor: &Editor,
    index: Option<&BlockIndex>,
    block: &VisualBlock,
    down: bool,
    x: f32,
    width: f32,
    shaper: &dyn LineShaper,
) -> Option<SourceOffset> {
    let document = editor.document();
    let probe = if down {
        let next = block.source_range.end;
        (next.0 < document.len_bytes().0).then_some(next)?
    } else {
        SourceOffset(block.source_range.start.0.checked_sub(1)?)
    };
    let indexed = block_at_offset(index, document, probe)?;
    let span = block_line_span(document, &indexed)?;
    let window = if down {
        span.start..span.start + 1
    } else {
        span.end.saturating_sub(1)..span.end
    };
    let visual = presented_block(editor, &indexed, &window)?;
    let layout = layout_block(&visual, width, shaper);
    let row = if down {
        0
    } else {
        layout.lines.len().checked_sub(1)?
    };
    layout.source_at_x(&visual, row, x, shaper)
}

/// The block holding one source offset: the formal index while it describes the
/// current revision, and a bounded local parse otherwise — the same two sources
/// the renderer draws from.
fn block_at_offset(
    index: Option<&BlockIndex>,
    document: &RopeBuffer,
    offset: SourceOffset,
) -> Option<IndexedBlock> {
    if let Some(index) = index {
        return index.block_at(offset);
    }
    let line = document.line_for_offset(offset).ok()?.0;
    local_block_index(document, line..line + 2).block_at(offset)
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

#[cfg(any(feature = "instrument", feature = "timing-probe"))]
impl EditorView {
    pub fn arm_startup_timing(&mut self, process_started: Instant) {
        self.instrumentation.process_started = process_started;
        self.instrumentation.ready_armed = true;
    }

    #[cfg(feature = "instrument")]
    pub fn record_phase0_idle_memory(&mut self, rss_bytes: Option<u64>) {
        if let Some(output) = &mut self.instrumentation.metrics_output
            && let Err(error) = output.memory("memory_idle_30s", rss_bytes)
        {
            eprintln!("could not write idle memory metrics: {error}");
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
        let resolved_theme = resolve_theme(self.settings.theme, window.appearance());
        if resolved_theme != self.theme {
            self.theme = resolved_theme;
            self.block_cache.clear();
            self.layout_cache.clear();
            let (granularity, _) = self.desired_layout();
            let heights = HeightIndex::new(self.item_heights());
            self.install_heights(granularity, heights);
        }
        self.schedule_document_parse(cx);
        self.viewport_height = (f32::from(window.viewport_size().height)
            - self.theme.header_height)
            .max(self.theme.line_height);
        self.step_measurement_scroll(window);
        // The width of the text column decides where every row breaks, so it is
        // read once per frame and every layout is keyed by it. A sidebar takes
        // its width out of the same window, so it must be subtracted here too,
        // not just in the element tree, or wrapping would be computed for a
        // column wider than what is actually drawn.
        let sidebar_width = if self.work_folder.is_some() {
            self.theme.sidebar_width
        } else {
            0.0
        };
        self.content_width = (f32::from(window.viewport_size().width)
            - sidebar_width
            - 2.0 * self.theme.line_horizontal_padding)
            .max(1.0);
        let shaper = WindowShaper::new(window);
        let font_revision = shaper.font_revision();
        if font_revision != self.layout_font_revision {
            self.layout_font_revision = font_revision;
            self.layout_cache.clear();
            let (granularity, _) = self.desired_layout();
            let heights = HeightIndex::new(self.item_heights());
            self.install_heights(granularity, heights);
        }
        self.scroll_y = clamp_scroll_y(
            self.scroll_y,
            self.heights.total_height(),
            self.viewport_height,
        );
        let visible =
            self.heights
                .visible_range(self.scroll_y, self.viewport_height, self.theme.overscan);
        let blocks = self.visible_blocks(visible.clone());
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

        self.layout_cache.retain(|id, _| retained.contains(id));

        let lines = self.visible_line_window(&blocks, &visible);
        let mut rendered = Vec::with_capacity(blocks.len());
        for block in &blocks {
            if let Some((visual, reused)) = self.cached_block(block, &lines) {
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
        // Measuring wrapped rows can correct blocks above the viewport. Keep the
        // same block and the same position inside it at the top instead of
        // letting those corrections visibly move the document.
        if let Some((old_ordinal, intra)) = height_anchor {
            let ordinal = old_ordinal.min(self.heights.len().saturating_sub(1));
            let inside = self
                .heights
                .height(ordinal)
                .map_or(0.0, |height| intra.clamp(0.0, height));
            self.scroll_y = self.heights.prefix_sum(ordinal) + inside;
        }
        // A newly measured block can shrink at the old bottom. Anchoring
        // preserves its block-relative position, which can now sit below the
        // new scroll limit and move all content above the viewport.
        self.scroll_y = clamp_scroll_y(
            self.scroll_y,
            self.heights.total_height(),
            self.viewport_height,
        );
        // Where the caret was drawn, for the IME candidate window. Only the
        // block that holds it can answer, and only while it is on screen.
        let caret = self.editor().selection().active;
        self.caret_geometry = rendered.iter().find_map(|(ordinal, visual, layout)| {
            if caret < visual.source_range.start || visual.source_range.end < caret {
                return None;
            }
            let point = layout.point_for_source(visual, caret, &shaper)?;
            Some(CaretGeometry {
                x: self.theme.line_horizontal_padding + point.x,
                y: self.heights.prefix_sum(*ordinal) + point.y - self.scroll_y,
                height: point.height,
            })
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
            .flex_row()
            .bg(rgb(self.theme.editor_background))
            .text_color(rgb(self.theme.foreground))
            .key_context("HaneEditor")
            .track_focus(&self.focus_handle(cx));
        let root = install_action_listeners(root, cx)
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .children(self.work_folder_sidebar(cx));
        let main_column = div()
            .flex_1()
            .min_w(px(0.0))
            .flex()
            .flex_col()
            .child(self.header_element(status, cx));
        // Relative image destinations resolve against the session's own file,
        // never against the directory the process happens to run in.
        let resolver = self.sessions.active().resource_resolver();
        let editor = self.sessions.active().editor();
        let main_column = main_column.child(
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
                        // One element per Markdown block, and inside it one per
                        // row: what is generated scales with visible blocks, and
                        // within a block with the rows that reach the viewport.
                        .children(rendered.into_iter().map(|(_, visual, layout)| {
                            block_element(
                                &layout,
                                (0..layout.lines.len()).map(|row_index| {
                                    let row = &layout.lines[row_index];
                                    let line = row.line_id as usize;
                                    let fragment = row.line_visual_range.clone();
                                    let dragged = fragment.clone();
                                    row_element(
                                        editor, &visual, &layout, row_index, self.theme, &resolver,
                                    )
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
                        }))
                        .child(div().h(px(bottom_space))),
                ),
        );
        let rendered = root.child(main_column);
        self.metrics.record_layout(layout_started.elapsed());
        rendered
    }
}

impl EditorView {
    /// The Markdown list for the sidebar, when this window was opened onto a
    /// work folder. Every entry switches into it on click, reusing an
    /// already-open session or loading it lazily; nothing here reads a file.
    /// A `+` above the list starts a brand-new unnamed note, and any unnamed
    /// note already open (freshly created, or recovered from a crash) is
    /// listed below the named ones so it stays reachable while it has no
    /// file of its own yet.
    fn work_folder_sidebar(&self, cx: &mut Context<Self>) -> Option<gpui::Stateful<gpui::Div>> {
        let work_folder = self.work_folder.as_ref()?;
        let active_id = self.sessions.active_id();
        let active_path = self.sessions.active().path();
        let new_note_button = div()
            .id("work-folder-new-note")
            .px_2()
            .py_1()
            .rounded_sm()
            .cursor_pointer()
            .bg(rgb(self.theme.code_background))
            .text_color(rgb(self.theme.foreground))
            .child("+")
            .on_click(cx.listener(|view, _, _, cx| view.new_work_folder_note(cx)));
        let entries = work_folder
            .entries()
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let is_active = active_path == Some(entry.path());
                let path = entry.path().to_path_buf();
                div()
                    .id(("work-folder-entry", index))
                    .px_2()
                    .py_1()
                    .rounded_sm()
                    .cursor_pointer()
                    .when(is_active, |element| {
                        element.bg(rgb(self.theme.sidebar_active_background))
                    })
                    .child(entry.name().to_owned())
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.open_work_folder_entry(&path, cx);
                    }))
            });
        let mut draft_ids: Vec<SessionId> = self.work_folder_drafts.keys().copied().collect();
        draft_ids.sort_by_key(|id| id.0);
        let drafts = draft_ids
            .into_iter()
            .filter_map(|id| self.sessions.get(id).map(|session| (id, session)))
            .map(|(id, session)| {
                let is_active = active_id == id;
                div()
                    .id(("work-folder-draft", id.0 as usize))
                    .px_2()
                    .py_1()
                    .rounded_sm()
                    .cursor_pointer()
                    .when(is_active, |element| {
                        element.bg(rgb(self.theme.sidebar_active_background))
                    })
                    .child(draft_preview(session))
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.activate_session(id, cx);
                    }))
            });
        Some(
            div()
                .id("work-folder-sidebar")
                .flex_none()
                .w(px(self.theme.sidebar_width))
                .h_full()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .bg(rgb(self.theme.sidebar_background))
                .text_color(rgb(self.theme.sidebar_foreground))
                .child(new_note_button)
                .children(entries)
                .children(drafts),
        )
    }
}

/// A short label for an unnamed note in the sidebar: its first non-blank
/// line (an H1 marker stripped, since that will become its title), or a
/// placeholder for a note that is still completely empty.
fn draft_preview(session: &DocumentSession) -> String {
    let text = session.editor().document().full_text();
    let first_line = text.lines().map(str::trim).find(|line| !line.is_empty());
    match first_line {
        Some(line) => line.trim_start_matches('#').trim().to_owned(),
        None => "Untitled note".to_owned(),
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

fn source_offset_for_visual_position(
    editor: &Editor,
    line: usize,
    block: &VisualLine,
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
    use hane_presentation::testing::FixedAdvanceShaper;
    use hane_session::RecoveredDraft;

    #[test]
    fn layout_cache_key_rejects_each_geometry_input_independently() {
        let entry = LayoutCacheEntry {
            layout: BlockLayout {
                block: BlockId(7),
                revision: Revision(3),
                width: 640.0,
                lines: Vec::new(),
                leading_space: 0.0,
                trailing_space: 0.0,
            },
            font_revision: 11,
        };
        assert!(entry.is_valid(640.0, 11, Revision(3)));
        assert!(!entry.is_valid(639.0, 11, Revision(3)));
        assert!(!entry.is_valid(640.0, 12, Revision(3)));
        assert!(!entry.is_valid(640.0, 11, Revision(4)));
    }

    /// Presents a whole document the way `render` does: index first, then one
    /// `presented_block` call per block.
    fn presented_lines(editor: &Editor) -> Vec<VisualLine> {
        let index = BlockIndex::from_buffer(editor.document());
        index
            .blocks()
            .flat_map(|block| {
                presented_block(editor, &block, &(0..usize::MAX))
                    .expect("block presents")
                    .lines
            })
            .collect()
    }

    #[test]
    fn visual_click_positions_map_back_to_source_offsets() {
        let editor = Editor::new("ab🙂\n\n**bold**");
        let lines = presented_lines(&editor);

        let first = &lines[0];
        assert_eq!(
            source_offset_for_visual_position(&editor, 0, first, 2),
            SourceOffset(2)
        );
        assert_eq!(
            source_offset_for_visual_position(&editor, 0, first, first.visual_text.len()),
            SourceOffset(6)
        );

        let empty = &lines[1];
        assert_eq!(
            source_offset_for_visual_position(&editor, 1, empty, 0),
            SourceOffset(7)
        );

        let bold = &lines[2];
        assert_eq!(
            source_offset_for_visual_position(&editor, 2, bold, 0),
            SourceOffset(10)
        );
        assert_eq!(
            source_offset_for_visual_position(&editor, 2, bold, bold.visual_text.len()),
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
    fn height_remeasurement_cannot_leave_scroll_position_below_new_bottom() {
        // This is the position retained by the height anchor after a block at
        // the old bottom shrinks from 1,000px to 600px.
        assert_eq!(clamp_scroll_y(600.0, 600.0, 300.0), 300.0);
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
    fn local_fallback_resolves_the_fenced_block_before_a_formal_index_exists() {
        let editor = Editor::new("before\n```rust\nlet answer = 42;\n```\nafter\n");
        let document = editor.document();
        let local = local_block_index(document, 0..6);
        let kind = |line: usize| {
            local
                .block_at(document.line_range(LineId(line)).unwrap().start)
                .map(|block| block.kind)
        };
        assert_eq!(kind(0), Some(hane_markdown::NodeKind::Paragraph));
        for inside in 1..=3 {
            assert_eq!(kind(inside), Some(hane_markdown::NodeKind::CodeBlock));
        }
        assert_eq!(kind(4), Some(hane_markdown::NodeKind::Paragraph));
    }

    #[test]
    fn a_block_taller_than_the_viewport_presents_only_the_visible_lines() {
        // No blank line anywhere, so CommonMark reads the whole document as a
        // single paragraph: the block is the document, and clipping is the only
        // thing keeping element generation bounded.
        let editor = Editor::new(&"短い段落です。\n".repeat(100_000));
        let index = BlockIndex::from_buffer(editor.document());
        assert_eq!(index.len(), 1, "the whole document is one block");
        let lines = editor.document().line_count();

        let block = index.block(0).unwrap();
        let visual = presented_block(&editor, &block, &(40_000..40_050)).unwrap();
        assert_eq!(visual.lines.len(), 50, "only the visible lines are built");
        assert_eq!(visual.lines_before, 40_000);
        assert_eq!(visual.lines_after, lines - 40_050);
        // The clipped lines still account for their height, so the scroll range
        // does not depend on what has been drawn.
        assert_eq!(visual.height(), lines as f32 * 26.0);
        assert_eq!(visual.leading_space(), 40_000.0 * 26.0);
        assert!(visual.covers(&(40_010..40_040)));
        assert!(!visual.covers(&(39_000..39_050)));
    }

    #[test]
    fn one_block_covers_every_physical_line_of_its_construct() {
        let source = "# title\n\n```rust\nlet x = 1;\nlet y = 2;\n```\n\ntail\n";
        let editor = Editor::new(source);
        let index = BlockIndex::from_buffer(editor.document());
        let spans = index
            .blocks()
            .map(|block| block_line_span(editor.document(), &block).expect("block spans lines"))
            .collect::<Vec<_>>();

        // Three blocks for nine lines: virtualization is driven by the three,
        // not by the nine.
        assert_eq!(spans, vec![0..2, 2..7, 7..9]);
        assert_eq!(
            spans.last().map(|span| span.end),
            Some(editor.document().line_count()),
            "the empty final line a trailing newline creates is still drawn"
        );
        // Every line is drawn exactly once.
        assert_eq!(
            spans.iter().map(std::ops::Range::len).sum::<usize>(),
            editor.document().line_count()
        );
    }

    #[test]
    fn a_presented_block_carries_all_of_its_lines() {
        let editor = Editor::new("```rust\nlet x = 1;\nlet y = 2;\n```\n\ntail\n");
        let index = BlockIndex::from_buffer(editor.document());
        let code = presented_block(&editor, &index.block(0).unwrap(), &(0..usize::MAX)).unwrap();
        assert_eq!(code.lines.len(), 5, "four fence lines plus the blank below");
        assert_eq!(code.source_range, index.block(0).unwrap().source_range);
        assert_eq!(
            code.height(),
            code.lines.iter().map(VisualLine::height).sum::<f32>()
        );
        assert!(
            code.matches(&index.block(0).unwrap()),
            "a freshly presented block matches the index it came from"
        );
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

    /// Ten columns at the test shaper's 8 px advance.
    const TEST_WIDTH: f32 = 80.0;

    fn laid_out(
        editor: &Editor,
        index: &BlockIndex,
        ordinal: usize,
        shaper: &FixedAdvanceShaper,
    ) -> (VisualBlock, BlockLayout) {
        let block = presented_block(editor, &index.block(ordinal).unwrap(), &(0..usize::MAX))
            .expect("block presents");
        let layout = layout_block(&block, TEST_WIDTH, shaper);
        (block, layout)
    }

    #[test]
    fn moving_down_past_a_block_lands_on_the_first_row_of_the_next() {
        let editor = Editor::new("alpha\n\nbravo charlie delta echo\n");
        let index = BlockIndex::from_buffer(editor.document());
        let shaper = FixedAdvanceShaper::new(8.0);
        let (first, layout) = laid_out(&editor, &index, 0, &shaper);
        let x = 3.0 * 8.0;

        // The blank line tiling folded into the first block is still a row of
        // it, so one move down stays inside the block.
        let VerticalMove::To(blank) =
            layout.vertical_target(&first, SourceOffset(3), true, x, &shaper)
        else {
            panic!("the blank line is a row of the first block");
        };
        assert_eq!(
            layout.vertical_target(&first, blank, true, x, &shaper),
            VerticalMove::PastEdge,
            "the blank line is the last row of the block"
        );

        let target =
            neighbor_row_target(&editor, Some(&index), &first, true, x, TEST_WIDTH, &shaper)
                .expect("there is a block below");
        let (second, below) = laid_out(&editor, &index, 1, &shaper);
        let point = below
            .point_for_source(&second, target, &shaper)
            .expect("the target is in the block below");
        assert_eq!(
            (point.row, point.x),
            (0, x),
            "the caret lands on the first row of the next block, at the x it was aiming at"
        );
    }

    #[test]
    fn moving_up_past_a_block_lands_on_its_last_row() {
        // A heading wide enough to wrap, so the block above has more rows than
        // it has source lines and only the last of them is the target. A heading
        // ends its block at its own line, so no blank row sits between the two.
        let editor = Editor::new("# one two three four five six\nsecond block\n");
        let index = BlockIndex::from_buffer(editor.document());
        let shaper = FixedAdvanceShaper::new(8.0);
        let (second, _) = laid_out(&editor, &index, 1, &shaper);
        let (first, above) = laid_out(&editor, &index, 0, &shaper);
        assert!(
            above.lines.len() > 1,
            "the heading above wraps onto several rows"
        );

        let target = neighbor_row_target(
            &editor,
            Some(&index),
            &second,
            false,
            0.0,
            TEST_WIDTH,
            &shaper,
        )
        .expect("there is a block above");
        assert_eq!(
            above
                .point_for_source(&first, target, &shaper)
                .map(|point| point.row),
            Some(above.lines.len() - 1),
            "moving up enters the block above on its last row, not its last line"
        );
    }

    #[test]
    fn the_document_edges_have_no_neighbor_row() {
        let editor = Editor::new(
            "only block
",
        );
        let index = BlockIndex::from_buffer(editor.document());
        let shaper = FixedAdvanceShaper::new(8.0);
        let (block, _) = laid_out(&editor, &index, 0, &shaper);
        for down in [true, false] {
            assert_eq!(
                neighbor_row_target(
                    &editor,
                    Some(&index),
                    &block,
                    down,
                    0.0,
                    TEST_WIDTH,
                    &shaper
                ),
                None
            );
        }
    }

    #[test]
    fn block_context_rejects_stale_revision() {
        assert!(block_context_revision_is_current(Revision(4), Revision(4)));
        assert!(!block_context_revision_is_current(Revision(5), Revision(4)));
    }

    #[test]
    fn height_anchor_rebases_without_scanning_untouched_blocks() {
        let inserted = [
            HeightBlock {
                id: BlockId(20),
                line_count: 1,
            },
            HeightBlock {
                id: BlockId(21),
                line_count: 1,
            },
            HeightBlock {
                id: BlockId(22),
                line_count: 1,
            },
        ];
        assert_eq!(
            rebase_ordinal_after_splice(2, Some(BlockId(2)), 4..6, &inserted, 9),
            2
        );
        assert_eq!(
            rebase_ordinal_after_splice(7, Some(BlockId(7)), 4..6, &inserted, 9),
            8
        );
        assert_eq!(
            rebase_ordinal_after_splice(5, Some(BlockId(21)), 4..6, &inserted, 9),
            5
        );
        assert_eq!(
            rebase_ordinal_after_splice(5, Some(BlockId(99)), 4..6, &inserted, 9),
            4
        );
    }

    #[test]
    fn height_block_metadata_splices_across_chunk_boundaries() {
        let mut flat = (0..300)
            .map(|id| HeightBlock {
                id: BlockId(id),
                line_count: id as usize % 5 + 1,
            })
            .collect::<Vec<_>>();
        let mut blocks = flat.iter().copied().collect::<HeightBlocks>();
        let inserted = [
            HeightBlock {
                id: BlockId(1_000),
                line_count: 2,
            },
            HeightBlock {
                id: BlockId(1_001),
                line_count: 3,
            },
            HeightBlock {
                id: BlockId(1_002),
                line_count: 4,
            },
        ];
        flat.splice(127..131, inserted);
        blocks.splice(127..131, &inserted);
        assert_eq!(blocks.len(), flat.len());
        assert!(blocks.range_eq(0..flat.len(), &flat));
        assert_eq!(blocks.get(126), flat.get(126).copied());
        assert_eq!(blocks.get(130), flat.get(130).copied());
    }

    fn draft_test_root(label: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "hane-draft-save-{label}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    // Regression test for the P1 review finding on `schedule_draft_save`:
    // switching to another note within the 750ms debounce window used to
    // cancel the pending write outright, because the timer only saved when
    // its own session was still the active one. Typing into an unnamed note
    // and switching away before the timer fires must still journal what was
    // typed.
    #[gpui::test]
    fn draft_save_survives_switching_sessions_within_the_debounce_window(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = draft_test_root("switch");
        std::fs::create_dir_all(&root).unwrap();
        let work_folder = OsWorkFolderScanner.scan(&root).unwrap();

        let view = gpui::AppContext::new(cx, |cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("", "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });

        view.update(cx, |view, cx| {
            view.work_folder = Some(work_folder);
            // Start the first unnamed note and type into it.
            view.new_work_folder_note(cx);
            view.editor_mut()
                .insert_text("today I thought about this design")
                .unwrap();
            view.after_input(cx);
            // Switch away to a second unnamed note before the debounce timer
            // for the first one fires.
            view.new_work_folder_note(cx);
        });

        // `schedule_draft_save` debounces on a real `gpui::Timer` (wall-clock,
        // not the deterministic test dispatcher), so the test has to wait for
        // real time to pass rather than fast-forwarding a virtual clock. The
        // spawned task must run once first to reach its `Timer::after` await
        // and register with the real clock before that wait is worth doing.
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(900));
        cx.run_until_parked();

        let recovered = OsDraftStore.recover(&root).unwrap();
        assert_eq!(recovered.drafts.len(), 1);
        assert_eq!(
            recovered.drafts[0].text,
            "today I thought about this design"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    // Regression test for the P2 review finding on `finish_work_folder_scan`:
    // a `DraftStore::recover` failure used to be swallowed via
    // `drafts.unwrap_or_default()`, leaving `status` at `None` exactly as if
    // there had simply been no drafts to recover. The error must be visible.
    #[gpui::test]
    fn a_draft_recovery_failure_is_surfaced_instead_of_silently_dropped(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = draft_test_root("recovery-error");
        std::fs::create_dir_all(&root).unwrap();
        let work_folder = OsWorkFolderScanner.scan(&root).unwrap();

        let view = gpui::AppContext::new(cx, |cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("", "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });

        view.update(cx, |view, cx| {
            let error = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
            view.finish_work_folder_scan((Ok(work_folder), Err(error)), cx);
        });

        let status = view.read_with(cx, |view, _| view.status.clone());
        assert!(
            status.is_some_and(|status| status.contains("recover")),
            "expected the drafts-recovery error to be surfaced in status"
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    // Regression test for the follow-up P2 finding: a per-file recovery
    // failure (as opposed to the whole directory being unreadable) must also
    // be visible, not just silently drop the one draft that could not be
    // read while saying nothing about it. The drafts that *were* readable
    // must still come back.
    #[gpui::test]
    fn a_partial_draft_recovery_failure_is_surfaced_while_readable_drafts_still_recover(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = draft_test_root("partial-recovery-error");
        std::fs::create_dir_all(&root).unwrap();
        let work_folder = OsWorkFolderScanner.scan(&root).unwrap();

        let view = gpui::AppContext::new(cx, |cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("", "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });

        let readable = RecoveredDraft {
            id: DraftId::generate(),
            text: "kept".to_owned(),
        };
        let partial = RecoveredDrafts {
            drafts: vec![readable.clone()],
            failed: 1,
        };

        view.update(cx, |view, cx| {
            view.finish_work_folder_scan((Ok(work_folder), Ok(partial)), cx);
        });

        view.read_with(cx, |view, _| {
            assert!(
                view.status
                    .as_deref()
                    .is_some_and(|status| status.contains('1')),
                "expected the one unreadable draft to be surfaced in status, got {:?}",
                view.status
            );
            assert!(
                view.work_folder_drafts
                    .values()
                    .any(|id| *id == readable.id),
                "the readable draft must still be recovered as a session"
            );
        });

        std::fs::remove_dir_all(&root).unwrap();
    }
}
