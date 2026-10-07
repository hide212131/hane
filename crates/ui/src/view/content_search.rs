//! Work-folder Markdown body search UI and bounded worker coordination.

use super::*;
use gpui::{AppContext, Entity};
use gpui_component::input::{Input, InputEvent, InputState};
use hane_document::{RopeSnapshot, SourceRange, TextBuffer};
use hane_session::search::{
    FileSearchResult, MAX_SEARCH_ERROR_DETAILS, MAX_SEARCH_HITS_TOTAL, MAX_SEARCH_QUEUED_FILES,
    MAX_SEARCH_RESULT_TEXT_BYTES, MAX_SEARCH_ROWS_PER_FRAME, SearchCancellationToken,
    SearchCompletion, SearchEngine, SearchEvent, SearchFileCompletion, SearchInput, SearchKey,
    SearchOutcome, SearchQuery, SearchRequest, SearchTarget, SearchVersion, SearchWarning,
    SearchWarningKind,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};

const SEARCH_DEBOUNCE: Duration = Duration::from_millis(250);
const SEARCH_DELIVERY_POLL: Duration = Duration::from_millis(16);
const SEARCH_SEND_RETRY: Duration = Duration::from_millis(8);
const MAX_SEARCH_CONCURRENT_WORKERS: usize = 2;
/// Caps how many channel events one delivery poll tick may examine, separate
/// from `MAX_SEARCH_ROWS_PER_FRAME`. Zero/low-hit `SearchEvent::File` results
/// add no delivered rows, so without this bound a single poll tick could
/// keep draining the channel for as long as workers kept refilling it
/// instead of yielding back every `SEARCH_DELIVERY_POLL` (Issue #414/#417
/// S10 follow-up). Set to the same size as the channel itself so one tick
/// processes at most one buffer's worth of work.
const MAX_SEARCH_EVENTS_PER_POLL: usize = MAX_SEARCH_QUEUED_FILES;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum SidebarMode {
    #[default]
    Files,
    Content,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ContentSearchStatus {
    Idle,
    Waiting,
    Searching,
    Stopping,
    Complete,
    Partial,
    Cancelled,
    Failed,
    InvalidQuery,
    NeedsWorkFolder,
}

#[derive(Clone)]
enum SearchSource {
    Disk(PathBuf),
    FileBuffer {
        path: PathBuf,
        session: SessionId,
        generation: u64,
        revision: Revision,
        snapshot: RopeSnapshot,
    },
    Draft {
        session: SessionId,
        generation: u64,
        revision: Revision,
        snapshot: RopeSnapshot,
    },
}

impl SearchSource {
    fn input(&self) -> Option<SearchInput> {
        match self {
            Self::Disk(_) => None,
            Self::FileBuffer {
                path,
                session,
                generation,
                revision,
                snapshot,
            } => Some(SearchInput::file_buffer(
                path.clone(),
                *session,
                *generation,
                *revision,
                snapshot.clone(),
            )),
            Self::Draft {
                session,
                generation,
                revision,
                snapshot,
                ..
            } => Some(SearchInput::draft_buffer(
                *session,
                *generation,
                *revision,
                snapshot.clone(),
            )),
        }
    }

    fn disk_path(&self) -> Option<&Path> {
        match self {
            Self::Disk(path) => Some(path),
            Self::FileBuffer { .. } | Self::Draft { .. } => None,
        }
    }
}

struct SearchWork {
    key: SearchKey,
    query: SearchQuery,
    cancellation: SearchCancellationToken,
    sources: Arc<[SearchSource]>,
    next_source: AtomicUsize,
    sender: SyncSender<SearchEvent>,
    /// Shared, monotonically-decreasing hit/text ceilings that every worker
    /// must reserve from before queuing a file's hits. Bounds the aggregate
    /// retained context text across worker-produced, queued, and displayed
    /// results to `MAX_SEARCH_HITS_TOTAL` / `MAX_SEARCH_RESULT_TEXT_BYTES`,
    /// instead of relying only on the channel's message-count bound (whose
    /// entries could otherwise each carry up to `MAX_SEARCH_HITS_PER_DOCUMENT`
    /// x `MAX_SEARCH_CONTEXT_BYTES`).
    remaining_hit_budget: AtomicUsize,
    remaining_text_budget: AtomicUsize,
}

impl SearchWork {
    fn new(
        key: SearchKey,
        query: SearchQuery,
        cancellation: SearchCancellationToken,
        sources: Arc<[SearchSource]>,
        sender: SyncSender<SearchEvent>,
    ) -> Self {
        Self {
            key,
            query,
            cancellation,
            sources,
            next_source: AtomicUsize::new(0),
            sender,
            remaining_hit_budget: AtomicUsize::new(MAX_SEARCH_HITS_TOTAL),
            remaining_text_budget: AtomicUsize::new(MAX_SEARCH_RESULT_TEXT_BYTES),
        }
    }
}

struct ActiveSearchWorker {
    key: SearchKey,
    cancellation: SearchCancellationToken,
}

struct DisplayedSearchFile {
    key: SearchKey,
    target: SearchTarget,
    version: SearchVersion,
    identity: Option<hane_session::FileIdentity>,
    label: String,
    hits: Vec<hane_session::search::SearchHit>,
    completion: SearchFileCompletion,
}

struct PendingFileDelivery {
    group_index: usize,
    result: FileSearchResult,
}

#[derive(Clone)]
struct PendingSearchNavigation {
    id: u64,
    key: SearchKey,
    target: SearchTarget,
    version: SearchVersion,
    identity: Option<hane_session::FileIdentity>,
    range: SourceRange,
    context_range: SourceRange,
    context_source: String,
}

/// State for one controller per EditorView. A worker remains in
/// `active_workers` until its background task returns, including after cancel.
pub(super) struct ContentSearchState {
    pub(super) mode: SidebarMode,
    input: Option<Entity<InputState>>,
    input_subscription: Option<Subscription>,
    input_focused: bool,
    result_scroll: ScrollHandle,
    query_text: String,
    case_sensitive: bool,
    workspace_epoch: u64,
    query_epoch: u64,
    debounce_task_id: u64,
    debounce_active: bool,
    last_query_change: Option<Instant>,
    status: ContentSearchStatus,
    status_detail: Option<String>,
    current_work: Option<Arc<SearchWork>>,
    receiver: Option<Receiver<SearchEvent>>,
    active_workers: HashMap<u64, ActiveSearchWorker>,
    next_worker_id: u64,
    next_navigation_id: u64,
    pending_navigation: Option<PendingSearchNavigation>,
    authorized_navigation_id: Option<u64>,
    workers_started: usize,
    delivery_poll_active: bool,
    pending_file: Option<PendingFileDelivery>,
    displayed_files: Vec<DisplayedSearchFile>,
    warnings: Vec<SearchWarning>,
    warning_count: usize,
    result_hit_count: usize,
    result_text_bytes: usize,
    files_finished: usize,
    total_files: usize,
    has_stale_results: bool,
    terminal_key: Option<SearchKey>,
    terminal_reason: Option<SearchCompletion>,
    results_focused: bool,
    selected_file: usize,
    selected_hit: usize,
}

impl Default for ContentSearchState {
    fn default() -> Self {
        Self {
            mode: SidebarMode::Files,
            input: None,
            input_subscription: None,
            input_focused: false,
            result_scroll: ScrollHandle::new(),
            query_text: String::new(),
            case_sensitive: false,
            workspace_epoch: 0,
            query_epoch: 0,
            debounce_task_id: 0,
            debounce_active: false,
            last_query_change: None,
            status: ContentSearchStatus::Idle,
            status_detail: None,
            current_work: None,
            receiver: None,
            active_workers: HashMap::new(),
            next_worker_id: 1,
            next_navigation_id: 1,
            pending_navigation: None,
            authorized_navigation_id: None,
            workers_started: 0,
            delivery_poll_active: false,
            pending_file: None,
            displayed_files: Vec::new(),
            warnings: Vec::new(),
            warning_count: 0,
            result_hit_count: 0,
            result_text_bytes: 0,
            files_finished: 0,
            total_files: 0,
            has_stale_results: false,
            terminal_key: None,
            terminal_reason: None,
            results_focused: false,
            selected_file: 0,
            selected_hit: 0,
        }
    }
}

impl Drop for ContentSearchState {
    fn drop(&mut self) {
        for worker in self.active_workers.values() {
            worker.cancellation.cancel();
        }
        if let Some(work) = self.current_work.as_ref() {
            work.cancellation.cancel();
        }
    }
}

impl ContentSearchState {
    fn should_leave_on_escape(&self, editor_has_ime_composition: bool) -> bool {
        self.input_focused
            || self.results_focused
            || (self.mode == SidebarMode::Content && !editor_has_ime_composition)
    }

    fn key(&self) -> SearchKey {
        SearchKey {
            workspace_epoch: self.workspace_epoch,
            query_epoch: self.query_epoch,
        }
    }

    fn clear_result_state(&mut self) {
        self.displayed_files.clear();
        self.warnings.clear();
        self.warning_count = 0;
        self.result_hit_count = 0;
        self.result_text_bytes = 0;
        self.files_finished = 0;
        self.total_files = 0;
        self.pending_file = None;
        self.has_stale_results = false;
        self.selected_file = 0;
        self.selected_hit = 0;
    }

    fn invalidate(&mut self, clear_results: bool) {
        self.query_epoch = self.query_epoch.wrapping_add(1);
        self.debounce_task_id = self.debounce_task_id.wrapping_add(1);
        self.debounce_active = false;
        self.last_query_change = None;
        if let Some(work) = self.current_work.take() {
            work.cancellation.cancel();
        }
        self.receiver = None;
        self.pending_file = None;
        self.terminal_key = None;
        self.terminal_reason = None;
        self.pending_navigation = None;
        self.authorized_navigation_id = None;
        if clear_results {
            self.clear_result_state();
        }
    }

    fn active_workers_for(&self, key: SearchKey) -> usize {
        self.active_workers
            .values()
            .filter(|worker| worker.key == key)
            .count()
    }

    fn has_worker_capacity(&self) -> bool {
        self.active_workers.len() < MAX_SEARCH_CONCURRENT_WORKERS
    }

    fn has_search_work(&self) -> bool {
        self.current_work.is_some() || self.receiver.is_some() || self.pending_file.is_some()
    }

    fn query_status_text(&self) -> String {
        if let Some(detail) = self.status_detail.as_ref() {
            return detail.clone();
        }
        match &self.status {
            ContentSearchStatus::Idle => "検索語を入力してください".to_owned(),
            ContentSearchStatus::Waiting => "検索を待っています…".to_owned(),
            ContentSearchStatus::Searching => {
                format!(
                    "検索中 · {}/{} ファイル · {} 件",
                    self.files_finished, self.total_files, self.result_hit_count
                )
            }
            ContentSearchStatus::Stopping => "停止しています…".to_owned(),
            ContentSearchStatus::Complete
                if self.result_hit_count == 0 && self.warning_count == 0 =>
            {
                "一致なし".to_owned()
            }
            ContentSearchStatus::Complete => {
                format!(
                    "{} 件 · {} 文書",
                    self.result_hit_count,
                    self.displayed_files.len()
                )
            }
            ContentSearchStatus::Partial => {
                format!(
                    "一部未検索 · {} 件 · {} 警告",
                    self.result_hit_count, self.warning_count
                )
            }
            ContentSearchStatus::Cancelled => {
                format!("中断 · {} 件", self.result_hit_count)
            }
            ContentSearchStatus::Failed => "検索に失敗しました".to_owned(),
            ContentSearchStatus::InvalidQuery => "検索語を確認してください".to_owned(),
            ContentSearchStatus::NeedsWorkFolder => {
                "Work folderを開くと本文を検索できます".to_owned()
            }
        }
    }
}

impl EditorView {
    fn invalidate_content_search(&mut self, clear_results: bool) {
        let pending = self.content_search.pending_navigation.take();
        self.content_search.authorized_navigation_id = None;
        if let Some(PendingSearchNavigation {
            target: SearchTarget::File(path),
            ..
        }) = pending
            && self.latest_open_target.as_deref() == Some(path.as_path())
        {
            self.latest_open_target = None;
        }
        self.content_search.invalidate(clear_results);
    }

    pub(super) fn preserve_content_search_navigation_for_action(&mut self) {
        let authorized = self.content_search.authorized_navigation_id;
        let is_authorized = authorized.is_some_and(|id| {
            self.content_search
                .pending_navigation
                .as_ref()
                .is_some_and(|navigation| navigation.id == id)
        });
        if is_authorized {
            return;
        }
        self.invalidate_content_search_navigation();
    }

    fn invalidate_content_search_navigation(&mut self) {
        let pending = self.content_search.pending_navigation.take();
        self.content_search.authorized_navigation_id = None;
        if let Some(PendingSearchNavigation {
            target: SearchTarget::File(path),
            ..
        }) = pending
            && self.latest_open_target.as_deref() == Some(path.as_path())
        {
            self.latest_open_target = None;
        }
    }

    pub(super) fn cancel_pending_search_navigation_for_path(&mut self, path: &Path) {
        let matches = self.content_search.pending_navigation.as_ref().is_some_and(
            |navigation| matches!(&navigation.target, SearchTarget::File(target) if target == path),
        );
        if matches {
            self.content_search.pending_navigation = None;
            self.content_search.authorized_navigation_id = None;
            if self.latest_open_target.as_deref() == Some(path) {
                self.latest_open_target = None;
            }
        }
    }

    pub(super) fn initialize_content_search_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.content_search.input.is_some() {
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("本文を検索"));
        let subscription =
            cx.subscribe(&input, |view, input, event: &InputEvent, cx| match event {
                InputEvent::Change => {
                    let text = input.read(cx).value().to_string();
                    view.content_search_query_changed(text, cx);
                }
                InputEvent::PressEnter { .. } => view.content_search_enter(cx),
                InputEvent::Focus => {
                    view.content_search.input_focused = true;
                    view.sidebar_keyboard_focus = true;
                    cx.notify();
                }
                InputEvent::Blur => {
                    view.content_search.input_focused = false;
                    cx.notify();
                }
            });
        self.content_search.input = Some(input);
        self.content_search.input_subscription = Some(subscription);
    }

    pub(super) fn content_search_sidebar_visible(&self) -> bool {
        self.content_search.mode == SidebarMode::Content
    }

    pub(crate) fn content_search_results_focused(&self) -> bool {
        self.content_search.results_focused
    }

    pub(crate) fn content_search_input_is_focused(&self) -> bool {
        self.content_search.input_focused
    }

    /// Clears both content-search focus flags without changing the search
    /// mode or results, so a click into the document body stops routing
    /// Backspace/Delete/Undo/Paste etc. to the content-search no-ops in
    /// `actions.rs` instead of the editor.
    pub(crate) fn blur_content_search_focus(&mut self, cx: &mut Context<Self>) {
        if self.content_search.input_focused || self.content_search.results_focused {
            self.content_search.input_focused = false;
            self.content_search.results_focused = false;
            cx.notify();
        }
    }

    pub(crate) fn content_search_should_leave_on_escape(&self) -> bool {
        self.content_search
            .should_leave_on_escape(self.editor().ime().is_some())
    }

    pub(crate) fn open_content_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_open || self.inline_rename_active() {
            return;
        }
        self.initialize_content_search_input(window, cx);
        self.content_search.mode = SidebarMode::Content;
        self.content_search.results_focused = false;
        self.content_search.input_focused = true;
        self.content_search.status_detail = None;
        self.sidebar_keyboard_focus = true;
        if self.work_folder.is_none() {
            self.invalidate_content_search(true);
            self.content_search.status = ContentSearchStatus::NeedsWorkFolder;
            if let Some(input) = self.content_search.input.as_ref() {
                input.update(cx, |state, cx| state.set_disabled(true, cx));
            }
        } else {
            if let Some(input) = self.content_search.input.as_ref() {
                input.update(cx, |state, cx| state.set_disabled(false, cx));
            }
            self.request_current_query(cx, true);
        }
        if let Some(input) = self.content_search.input.as_ref() {
            input.update(cx, |state, cx| state.focus(window, cx));
        }
        cx.notify();
    }

    pub(super) fn toggle_content_search_mode(
        &mut self,
        mode: SidebarMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if mode == self.content_search.mode {
            if mode == SidebarMode::Content
                && let Some(input) = self.content_search.input.as_ref()
            {
                input.update(cx, |state, cx| state.focus(window, cx));
            }
            return;
        }
        if mode == SidebarMode::Files {
            self.invalidate_content_search(false);
            self.content_search.results_focused = false;
            self.content_search.input_focused = false;
            self.content_search.status = ContentSearchStatus::Cancelled;
            self.content_search.status_detail = None;
        } else {
            self.initialize_content_search_input(window, cx);
            self.content_search.results_focused = false;
            self.content_search.input_focused = true;
            if self.work_folder.is_none() {
                self.content_search.status = ContentSearchStatus::NeedsWorkFolder;
                self.content_search.status_detail = None;
                if let Some(input) = self.content_search.input.as_ref() {
                    input.update(cx, |state, cx| state.set_disabled(true, cx));
                }
            } else {
                if let Some(input) = self.content_search.input.as_ref() {
                    input.update(cx, |state, cx| state.set_disabled(false, cx));
                }
                self.request_current_query(cx, true);
            }
            if let Some(input) = self.content_search.input.as_ref() {
                input.update(cx, |state, cx| state.focus(window, cx));
            }
        }
        self.content_search.mode = mode;
        cx.notify();
    }

    fn content_search_query_changed(&mut self, text: String, cx: &mut Context<Self>) {
        self.content_search.query_text = text;
        self.invalidate_content_search(true);
        self.content_search.status_detail = None;
        self.content_search.status = if self.content_search.query_text.is_empty() {
            ContentSearchStatus::Idle
        } else {
            ContentSearchStatus::Waiting
        };
        self.content_search.last_query_change = Some(Instant::now());
        self.schedule_content_search_debounce(cx);
        cx.notify();
    }

    fn schedule_content_search_debounce(&mut self, cx: &mut Context<Self>) {
        if self.content_search.debounce_active {
            return;
        }
        self.content_search.debounce_active = true;
        self.content_search.debounce_task_id = self.content_search.debounce_task_id.wrapping_add(1);
        let task_id = self.content_search.debounce_task_id;
        cx.spawn(async move |view, cx| {
            loop {
                let delay = view
                    .read_with(cx, |view, _| {
                        let state = &view.content_search;
                        (state.debounce_active && state.debounce_task_id == task_id).then(|| {
                            state.last_query_change.map_or(SEARCH_DEBOUNCE, |at| {
                                SEARCH_DEBOUNCE.saturating_sub(at.elapsed())
                            })
                        })
                    })
                    .unwrap_or(None);
                let Some(delay) = delay else {
                    break;
                };
                cx.background_executor().timer(delay).await;
                let keep_waiting = view
                    .update(cx, |view, cx| {
                        let input_has_uncommitted_text =
                            view.content_search.input.as_ref().is_some_and(|input| {
                                input.read(cx).value().as_ref()
                                    != view.content_search.query_text.as_str()
                            });
                        let state = &mut view.content_search;
                        if !state.debounce_active || state.debounce_task_id != task_id {
                            return false;
                        }
                        if input_has_uncommitted_text {
                            state.last_query_change = Some(Instant::now());
                            return true;
                        }
                        if state
                            .last_query_change
                            .is_some_and(|at| at.elapsed() < SEARCH_DEBOUNCE)
                        {
                            return true;
                        }
                        state.debounce_active = false;
                        state.last_query_change = None;
                        view.start_content_search(cx);
                        false
                    })
                    .unwrap_or(false);
                if !keep_waiting {
                    break;
                }
            }
        })
        .detach();
    }

    fn request_current_query(&mut self, cx: &mut Context<Self>, debounce: bool) {
        self.invalidate_content_search(true);
        if self.content_search.query_text.is_empty() {
            self.content_search.status = ContentSearchStatus::Idle;
            self.content_search.status_detail = None;
            cx.notify();
        } else if debounce {
            self.content_search.last_query_change = Some(Instant::now());
            self.content_search.status = ContentSearchStatus::Waiting;
            self.schedule_content_search_debounce(cx);
            cx.notify();
        } else {
            self.start_content_search(cx);
        }
    }

    fn content_search_enter(&mut self, cx: &mut Context<Self>) {
        self.invalidate_content_search(true);
        self.start_content_search(cx);
    }

    fn start_content_search(&mut self, cx: &mut Context<Self>) {
        if self.content_search.mode != SidebarMode::Content {
            return;
        }
        if self.work_folder.is_none() {
            self.content_search.status = ContentSearchStatus::NeedsWorkFolder;
            self.content_search.status_detail = None;
            cx.notify();
            return;
        }
        if self.content_search.query_text.is_empty() {
            self.content_search.status = ContentSearchStatus::Idle;
            self.content_search.status_detail = None;
            cx.notify();
            return;
        }
        let query = match SearchQuery::new(
            self.content_search.query_text.clone(),
            self.content_search.case_sensitive,
        ) {
            Ok(query) => query,
            Err(error) => {
                self.content_search.status = ContentSearchStatus::InvalidQuery;
                self.content_search.status_detail = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        self.content_search.clear_result_state();
        let (sender, receiver) = mpsc::sync_channel(MAX_SEARCH_QUEUED_FILES);
        let sources = Arc::<[SearchSource]>::from(self.content_search_sources());
        let key = self.content_search.key();
        if sources.is_empty() {
            self.content_search.total_files = 0;
            self.content_search.status = ContentSearchStatus::Complete;
            self.content_search.status_detail = None;
            cx.notify();
            return;
        }
        self.content_search.total_files = sources.len();
        let work = Arc::new(SearchWork::new(
            key,
            query,
            SearchCancellationToken::new(),
            sources,
            sender,
        ));
        self.content_search.current_work = Some(work);
        self.content_search.receiver = Some(receiver);
        self.content_search.workers_started = 0;
        self.content_search.terminal_key = None;
        self.content_search.terminal_reason = None;
        self.content_search.status = ContentSearchStatus::Searching;
        self.content_search.status_detail = None;
        self.start_available_content_search_workers(cx);
        self.ensure_content_search_delivery_poll(cx);
        cx.notify();
    }

    fn content_search_sources(&self) -> Vec<SearchSource> {
        let Some(folder) = self.work_folder.as_ref() else {
            return Vec::new();
        };
        let mut sources = Vec::new();
        let mut indexed = HashSet::new();
        for entry in folder.entries() {
            let path = entry.path().to_path_buf();
            if !indexed.insert(path.clone()) {
                continue;
            }
            if let Some(session_id) = self.sessions.session_for_path(&path)
                && let Some(session) = self.sessions.get(session_id)
            {
                sources.push(SearchSource::FileBuffer {
                    path,
                    session: session.id(),
                    generation: session.generation(),
                    revision: session.revision(),
                    snapshot: session.editor().document().snapshot(),
                });
            } else {
                sources.push(SearchSource::Disk(path));
            }
        }
        let mut drafts: Vec<_> = self
            .work_folder_drafts
            .iter()
            .filter(|(_, draft)| draft.target_directory.starts_with(folder.root()))
            .collect();
        drafts.sort_by_key(|(session_id, _)| session_id.0);
        for (&session_id, _draft) in drafts {
            if let Some(session) = self.sessions.get(session_id) {
                sources.push(SearchSource::Draft {
                    session: session.id(),
                    generation: session.generation(),
                    revision: session.revision(),
                    snapshot: session.editor().document().snapshot(),
                });
            }
        }
        sources
    }

    fn start_available_content_search_workers(&mut self, cx: &mut Context<Self>) {
        let Some(work) = self.content_search.current_work.clone() else {
            return;
        };
        while self.content_search.has_worker_capacity()
            && self.content_search.workers_started < MAX_SEARCH_CONCURRENT_WORKERS
            && work.next_source.load(Ordering::Acquire) < work.sources.len()
        {
            let worker_id = self.content_search.next_worker_id;
            self.content_search.next_worker_id = worker_id.wrapping_add(1).max(1);
            self.content_search.workers_started += 1;
            self.content_search.active_workers.insert(
                worker_id,
                ActiveSearchWorker {
                    key: work.key,
                    cancellation: work.cancellation.clone(),
                },
            );
            let files = self.files.clone();
            let worker_work = work.clone();
            let key = work.key;
            cx.spawn(async move |view, cx| {
                let result = cx
                    .background_executor()
                    .spawn(async move { run_search_worker(worker_work, files) })
                    .await;
                let _ = view.update(cx, |view, cx| {
                    view.finish_content_search_worker(worker_id, key, result, cx);
                });
            })
            .detach();
        }
    }

    fn finish_content_search_worker(
        &mut self,
        worker_id: u64,
        key: SearchKey,
        result: Result<(), String>,
        cx: &mut Context<Self>,
    ) {
        self.content_search.active_workers.remove(&worker_id);
        if self
            .content_search
            .current_work
            .as_ref()
            .is_some_and(|work| work.key == key)
        {
            if let Err(detail) = result {
                self.content_search.status = ContentSearchStatus::Failed;
                self.content_search.status_detail = Some(detail);
                if let Some(work) = self.content_search.current_work.as_ref() {
                    work.cancellation.cancel();
                }
                self.content_search.current_work = None;
                self.content_search.receiver = None;
            } else {
                self.start_available_content_search_workers(cx);
            }
        } else if self.content_search.current_work.is_some() {
            // A cancelled older query still occupied this controller slot.
            // Reuse it for the latest coalesced request only after it exits.
            self.start_available_content_search_workers(cx);
        }
        if let Some(terminal_key) = self.content_search.terminal_key
            && terminal_key == key
            && self.content_search.active_workers_for(terminal_key) == 0
        {
            self.content_search.status = self.content_search.terminal_reason.map_or(
                ContentSearchStatus::Cancelled,
                |completion| match completion {
                    SearchCompletion::Partial => ContentSearchStatus::Partial,
                    SearchCompletion::Cancelled => ContentSearchStatus::Cancelled,
                    SearchCompletion::Failed => ContentSearchStatus::Failed,
                    SearchCompletion::Complete => ContentSearchStatus::Complete,
                },
            );
            self.content_search.terminal_key = None;
            self.content_search.terminal_reason = None;
        }
        self.ensure_content_search_delivery_poll(cx);
        cx.notify();
    }

    fn ensure_content_search_delivery_poll(&mut self, cx: &mut Context<Self>) {
        if self.content_search.delivery_poll_active || !self.content_search.has_search_work() {
            return;
        }
        self.content_search.delivery_poll_active = true;
        cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor().timer(SEARCH_DELIVERY_POLL).await;
                let keep_polling = view
                    .update(cx, |view, cx| view.poll_content_search_delivery(cx))
                    .unwrap_or(false);
                if !keep_polling {
                    break;
                }
            }
        })
        .detach();
    }

    fn poll_content_search_delivery(&mut self, cx: &mut Context<Self>) -> bool {
        let mut rows = 0;
        let mut events = 0;
        let mut channel_empty = false;
        while rows < MAX_SEARCH_ROWS_PER_FRAME && events < MAX_SEARCH_EVENTS_PER_POLL {
            events += 1;
            if let Some(key) = self
                .content_search
                .pending_file
                .as_ref()
                .map(|pending| pending.result.key)
            {
                if key != self.content_search.key() {
                    self.content_search.pending_file = None;
                    continue;
                }
                let pending_is_current = self
                    .content_search
                    .pending_file
                    .as_ref()
                    .is_some_and(|pending| file_result_is_current(&pending.result, self));
                if !pending_is_current {
                    self.content_search.has_stale_results = true;
                    self.content_search.status_detail =
                        Some("内容が変わりました。再検索してください".to_owned());
                    self.content_search.pending_file = None;
                    continue;
                }
                let remaining_hits =
                    MAX_SEARCH_HITS_TOTAL.saturating_sub(self.content_search.result_hit_count);
                if remaining_hits == 0 {
                    self.content_search.stop_at_global_limit();
                    break;
                }
                let remaining_text = MAX_SEARCH_RESULT_TEXT_BYTES
                    .saturating_sub(self.content_search.result_text_bytes);
                let (group_index, completion, take, hits) = {
                    let pending = self
                        .content_search
                        .pending_file
                        .as_mut()
                        .expect("pending result was checked above");
                    let take = delivery_hit_count(
                        &pending.result.hits,
                        MAX_SEARCH_ROWS_PER_FRAME - rows,
                        remaining_hits,
                        remaining_text,
                    );
                    let hits = pending.result.hits.drain(..take).collect::<Vec<_>>();
                    (pending.group_index, pending.result.completion, take, hits)
                };
                if take == 0 {
                    self.content_search.stop_at_global_limit();
                    break;
                }
                for hit in hits {
                    self.content_search.result_text_bytes += hit.context_source.len();
                    self.content_search.result_hit_count += 1;
                    rows += 1;
                    self.content_search.displayed_files[group_index]
                        .hits
                        .push(hit);
                }
                let file_complete = self
                    .content_search
                    .pending_file
                    .as_ref()
                    .is_some_and(|pending| pending.result.hits.is_empty());
                if file_complete {
                    if completion == SearchFileCompletion::LimitReached {
                        self.content_search.mark_partial_for_current_work();
                    }
                    self.content_search.pending_file = None;
                }
                if self.content_search.result_hit_count >= MAX_SEARCH_HITS_TOTAL
                    && self
                        .content_search
                        .pending_file
                        .as_ref()
                        .is_some_and(|pending| !pending.result.hits.is_empty())
                {
                    self.content_search.stop_at_global_limit();
                    break;
                }
                continue;
            }
            let Some(receiver) = self.content_search.receiver.as_ref() else {
                break;
            };
            match receiver.try_recv() {
                Ok(event) => {
                    self.content_search
                        .accept_search_event(event, self.work_folder.as_ref());
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => {
                    channel_empty = true;
                    break;
                }
            }
        }
        if self.content_search.receiver.is_none() {
            channel_empty = true;
        }
        self.content_search
            .finish_search_if_drained(channel_empty && self.content_search.pending_file.is_none());
        let keep_polling = self.content_search.has_search_work();
        if !keep_polling {
            self.content_search.delivery_poll_active = false;
        }
        cx.notify();
        keep_polling
    }

    pub(super) fn stop_content_search(&mut self, cx: &mut Context<Self>) {
        let Some(work) = self.content_search.current_work.take() else {
            return;
        };
        let old_key = work.key;
        self.content_search.debounce_active = false;
        self.content_search.last_query_change = None;
        work.cancellation.cancel();
        self.content_search.receiver = None;
        self.content_search.pending_file = None;
        self.content_search.status = ContentSearchStatus::Stopping;
        self.content_search.status_detail = None;
        self.content_search.terminal_key = Some(old_key);
        self.content_search.terminal_reason = Some(SearchCompletion::Cancelled);
        if self.content_search.active_workers_for(old_key) == 0 {
            self.content_search.status = ContentSearchStatus::Cancelled;
            self.content_search.terminal_key = None;
            self.content_search.terminal_reason = None;
        }
        cx.notify();
    }

    pub(super) fn toggle_content_search_case(&mut self, cx: &mut Context<Self>) {
        self.content_search.case_sensitive = !self.content_search.case_sensitive;
        self.invalidate_content_search(true);
        self.content_search.status_detail = None;
        self.content_search.status = ContentSearchStatus::Waiting;
        self.content_search.last_query_change = Some(Instant::now());
        self.schedule_content_search_debounce(cx);
        cx.notify();
    }

    pub(crate) fn focus_content_search_results(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self
            .content_search
            .displayed_files
            .iter()
            .any(|file| !file.hits.is_empty())
        {
            return;
        }
        self.content_search.results_focused = true;
        self.content_search.input_focused = false;
        self.content_search.select_first_result();
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    pub(crate) fn focus_content_search_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.content_search.results_focused = false;
        self.content_search.input_focused = true;
        if let Some(input) = self.content_search.input.as_ref() {
            input.update(cx, |state, cx| state.focus(window, cx));
        }
        cx.notify();
    }

    pub(crate) fn move_content_search_selection(&mut self, down: bool, cx: &mut Context<Self>) {
        self.content_search.move_selection(down);
        cx.notify();
    }

    pub(crate) fn handle_content_search_enter(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.content_search.results_focused {
            return false;
        }
        let selected = self.content_search.selected_result();
        let Some((file_index, hit_index)) = selected else {
            return true;
        };
        self.open_content_search_result(file_index, hit_index, window, cx);
        true
    }

    pub(crate) fn leave_content_search(&mut self, cx: &mut Context<Self>) -> bool {
        if self.content_search.mode != SidebarMode::Content {
            return false;
        }
        self.invalidate_content_search(false);
        self.content_search.mode = SidebarMode::Files;
        self.content_search.results_focused = false;
        self.content_search.status = ContentSearchStatus::Cancelled;
        self.content_search.status_detail = None;
        cx.notify();
        true
    }

    pub(super) fn invalidate_content_search_session(
        &mut self,
        session_id: SessionId,
        cx: &mut Context<Self>,
    ) {
        let before = self.content_search.result_hit_count;
        self.content_search.displayed_files.retain(|file| match &file.target {
            SearchTarget::Draft(id) => *id != session_id,
            SearchTarget::File(_) => !matches!(file.version, SearchVersion::Buffer { session, .. } if session == session_id),
        });
        if self
            .content_search
            .current_work
            .as_ref()
            .is_some_and(|work| {
                work.sources.iter().any(|source| match source {
                    SearchSource::FileBuffer { session, .. }
                    | SearchSource::Draft { session, .. } => *session == session_id,
                    SearchSource::Disk(_) => false,
                })
            })
        {
            self.content_search.has_stale_results = true;
        }
        self.content_search.recount_results();
        if before != self.content_search.result_hit_count || self.content_search.has_stale_results {
            self.content_search.status_detail =
                Some("内容が変わりました。再検索してください".to_owned());
            if self.content_search.status == ContentSearchStatus::Complete {
                self.content_search.status = ContentSearchStatus::Partial;
            }
            cx.notify();
        }
    }

    pub(super) fn content_search_workspace_changed(&mut self, cx: &mut Context<Self>) {
        self.content_search.workspace_epoch = self.content_search.workspace_epoch.wrapping_add(1);
        if self.content_search.mode == SidebarMode::Content {
            self.invalidate_content_search(true);
            if let Some(input) = self.content_search.input.as_ref() {
                input.update(cx, |state, cx| {
                    state.set_disabled(self.work_folder.is_none(), cx);
                });
            }
            self.content_search.status_detail = None;
            if self.work_folder.is_none() {
                self.content_search.status = ContentSearchStatus::NeedsWorkFolder;
            } else if self.content_search.query_text.is_empty() {
                self.content_search.status = ContentSearchStatus::Idle;
            } else {
                self.content_search.status = ContentSearchStatus::Waiting;
                self.content_search.last_query_change = Some(Instant::now());
                self.schedule_content_search_debounce(cx);
            }
        }
        cx.notify();
    }

    pub(super) fn content_search_sidebar(
        &self,
        viewport_height: f32,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let tab = |id: &'static str, label: &'static str, mode: SidebarMode| {
            div()
                .id(id)
                .h(px(24.0))
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .rounded_sm()
                .cursor_pointer()
                .when(self.content_search.mode == mode, |element| {
                    element.bg(rgb(self.theme.sidebar_active_background))
                })
                .child(label)
                .on_click(cx.listener(move |view, _, window, cx| {
                    view.toggle_content_search_mode(mode, window, cx)
                }))
        };
        let tabs = div()
            .id("work-folder-mode-tabs")
            .debug_selector(|| "sidebar-mode-tabs".to_owned())
            .h(px(SIDEBAR_TOOLBAR_HEIGHT))
            .flex_none()
            .flex()
            .gap_1()
            .mb(px(SIDEBAR_TOOLBAR_GAP))
            .child(tab("sidebar-files-mode", "ファイル", SidebarMode::Files))
            .child(tab(
                "sidebar-content-mode",
                "本文検索",
                SidebarMode::Content,
            ));
        let work_folder = self.work_folder.as_ref();
        let input = self
            .content_search
            .input
            .as_ref()
            .expect("content search mode initializes its input before rendering");
        let search_input = Input::new(input)
            .id("content-search-input")
            .h(px(SIDEBAR_FILTER_HEIGHT))
            .appearance(false)
            .bordered(false)
            .focus_bordered(false)
            .disabled(work_folder.is_none());
        let search_box = div()
            .id("content-search-query")
            .h(px(SIDEBAR_FILTER_HEIGHT))
            .flex_none()
            .mb(px(SIDEBAR_FILTER_GAP))
            .px(px(4.0))
            .rounded_sm()
            .border_1()
            .border_color(rgb(self.theme.sidebar_foreground))
            .bg(rgb(self.theme.code_background))
            .child(search_input);
        let toolbar_button = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .h(px(24.0))
                .px(px(6.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded_sm()
                .cursor_pointer()
                .bg(rgb(self.theme.code_background))
                .child(label)
        };
        let stop = toolbar_button("content-search-stop", "停止")
            .when(
                !matches!(
                    self.content_search.status,
                    ContentSearchStatus::Searching | ContentSearchStatus::Stopping
                ),
                |el| el.opacity(0.45),
            )
            .on_click(cx.listener(|view, _, _, cx| view.stop_content_search(cx)));
        let rescan = toolbar_button("content-search-rescan", "再検索")
            .when(work_folder.is_none(), |el| el.opacity(0.45))
            .on_click(cx.listener(|view, _, _, cx| view.rescan_content_search(cx)));
        let case_toggle = toolbar_button(
            "content-search-case",
            if self.content_search.case_sensitive {
                "Aa✓"
            } else {
                "Aa"
            },
        )
        .on_click(cx.listener(|view, _, _, cx| view.toggle_content_search_case(cx)));
        let controls = div()
            .id("content-search-controls")
            .h(px(SIDEBAR_FILTER_HEIGHT))
            .flex_none()
            .mb(px(SIDEBAR_FILTER_GAP))
            .flex()
            .items_center()
            .gap_1()
            .child(case_toggle)
            .child(stop)
            .child(rescan);
        let status = div()
            .id("content-search-status")
            .debug_selector(|| "content-search-status".to_owned())
            .h(px(22.0))
            .flex_none()
            .mb(px(SIDEBAR_FILTER_GAP))
            .text_color(rgb(self.theme.quote_foreground))
            .truncate()
            .child(self.content_search.query_status_text());

        let list_top = SIDEBAR_PADDING
            + SIDEBAR_TOOLBAR_HEIGHT
            + SIDEBAR_TOOLBAR_GAP
            + SIDEBAR_FILTER_HEIGHT
            + SIDEBAR_FILTER_GAP
            + SIDEBAR_FILTER_HEIGHT
            + SIDEBAR_FILTER_GAP
            + 22.0
            + SIDEBAR_FILTER_GAP;
        let list_viewport_height = (viewport_height
            - list_top
            - SIDEBAR_SETTINGS_GAP
            - SIDEBAR_SETTINGS_HEIGHT
            - SIDEBAR_PADDING)
            .max(0.0);
        let row_height = SIDEBAR_ROW_HEIGHT;
        let scroll_offset = (-f32::from(self.content_search.result_scroll.offset().y)).max(0.0);
        let visible_start = (scroll_offset / row_height).floor() as usize;
        let visible_count = (list_viewport_height / row_height).ceil() as usize + 2;
        let rows = self.content_search.visible_rows(
            visible_start,
            visible_count,
            work_folder.map(WorkFolder::root),
            cx,
        );
        let top_spacer = visible_start as f32 * row_height;
        let content_rows = self
            .content_search
            .result_row_count(work_folder.map(WorkFolder::root));
        let bottom_spacer =
            content_rows.saturating_sub(visible_start + rows.len()) as f32 * row_height;
        let result_list = div()
            .id("content-search-results")
            .debug_selector(|| "content-search-results".to_owned())
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .track_scroll(&self.content_search.result_scroll)
            .when(self.content_search.results_focused, |el| {
                el.border_1()
                    .border_color(rgb(self.theme.sidebar_foreground))
            })
            .on_key_down(cx.listener(|view, event: &gpui::KeyDownEvent, window, cx| {
                let key = event.keystroke.key.as_str();
                if key == "up" || key == "down" {
                    view.move_content_search_selection(key == "down", cx);
                    cx.stop_propagation();
                } else if key == "enter" {
                    view.handle_content_search_enter(window, cx);
                    cx.stop_propagation();
                } else if key == "escape" {
                    view.leave_content_search(cx);
                    cx.stop_propagation();
                } else if key == "tab" {
                    view.content_search.results_focused = false;
                    if let Some(input) = view.content_search.input.as_ref() {
                        input.update(cx, |state, cx| state.focus(window, cx));
                    }
                    cx.stop_propagation();
                }
            }))
            .child(div().h(px(top_spacer)))
            .children(rows)
            .child(div().h(px(bottom_spacer)));
        let no_folder = work_folder.is_none().then(|| {
            div()
                .id("content-search-no-work-folder")
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_color(rgb(self.theme.quote_foreground))
                .child("Work folderを開くと本文を検索できます")
        });
        let view = cx.entity();
        let settings_button = div()
            .id("sidebar-settings")
            .debug_selector(|| "sidebar-settings-footer".to_owned())
            .w_full()
            .h_full()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .px_2()
            .rounded_sm()
            .cursor_pointer()
            .hover(|style| style.bg(rgb(self.theme.sidebar_active_background)))
            .child(
                gpui::svg()
                    .path(icons::ICON_SETTINGS)
                    .size_4()
                    .flex_none()
                    .text_color(rgb(self.theme.sidebar_foreground)),
            )
            .child("設定")
            .on_click(move |_, window, app| {
                view.update(app, |view, cx| view.open_settings(window, cx));
            });
        let footer = div()
            .id("sidebar-settings-footer")
            .h(px(SIDEBAR_SETTINGS_HEIGHT))
            .mt(px(SIDEBAR_SETTINGS_GAP))
            .flex_none()
            .child(settings_button);
        let result_area = if work_folder.is_some() {
            result_list.into_any_element()
        } else {
            no_folder.unwrap().into_any_element()
        };
        div()
            .id("work-folder-panel")
            .relative()
            .flex_none()
            .w(px(self.sidebar_width))
            .h_full()
            .overflow_hidden()
            .bg(rgb(self.theme.sidebar_background))
            .child(
                div()
                    .id("work-folder-content-search")
                    .size_full()
                    .flex()
                    .flex_col()
                    .pt(px(SIDEBAR_PADDING))
                    .pb(px(SIDEBAR_PADDING))
                    .pl(px(SIDEBAR_PADDING))
                    .pr(px(SIDEBAR_PADDING + SCROLLBAR_TRACK_WIDTH))
                    .bg(rgb(self.theme.sidebar_background))
                    .text_color(rgb(self.theme.sidebar_foreground))
                    .text_size(px(BODY_FONT_SIZE))
                    .child(tabs)
                    .child(search_box)
                    .child(controls)
                    .child(status)
                    .child(result_area)
                    .child(footer),
            )
    }

    fn rescan_content_search(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self
            .work_folder
            .as_ref()
            .map(|folder| folder.root().to_path_buf())
        else {
            self.content_search.status = ContentSearchStatus::NeedsWorkFolder;
            self.content_search.status_detail = None;
            cx.notify();
            return;
        };
        if self.content_search.status == ContentSearchStatus::Waiting {
            return;
        }
        self.invalidate_content_search(true);
        self.content_search.status = ContentSearchStatus::Waiting;
        self.content_search.status_detail = Some("Work folder一覧を更新しています…".to_owned());
        let generation = self.work_folder_generation;
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { OsWorkFolderScanner.scan(&root) })
                .await;
            let _ = view.update(cx, |view, cx| {
                if generation != view.work_folder_generation {
                    return;
                }
                match result {
                    Ok(folder) => {
                        view.work_folder = Some(folder);
                        view.content_search.workspace_epoch =
                            view.content_search.workspace_epoch.wrapping_add(1);
                        view.content_search.status_detail = None;
                        view.content_search.status = ContentSearchStatus::Waiting;
                        view.request_current_query(cx, false);
                    }
                    Err(error) => {
                        view.content_search.status = ContentSearchStatus::Failed;
                        view.content_search.status_detail =
                            Some(format!("Work folder一覧の更新に失敗しました: {error}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn open_content_search_result(
        &mut self,
        file_index: usize,
        hit_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        let Some(file) = self.content_search.displayed_files.get(file_index) else {
            return;
        };
        let Some(hit) = file.hits.get(hit_index) else {
            return;
        };
        let target = file.target.clone();
        let version = file.version;
        let key = file.key;
        let navigation = PendingSearchNavigation {
            id: self.content_search.next_navigation_id,
            key,
            target: target.clone(),
            version,
            identity: file.identity.clone(),
            range: hit.range,
            context_range: hit.context_range,
            context_source: hit.context_source.clone(),
        };
        self.content_search.next_navigation_id = navigation.id.wrapping_add(1).max(1);
        if key != self.content_search.key() || !target_version_is_current(&target, version, self) {
            self.mark_search_navigation_stale(cx);
            return;
        }
        self.content_search.pending_navigation = Some(navigation.clone());
        self.content_search.authorized_navigation_id = Some(navigation.id);
        match target {
            SearchTarget::File(path) => {
                self.open_work_folder_entry(&path, cx);
                if let Some(session) = self.sessions.session_for_path(&path)
                    && self.sessions.active_id() == session
                    && matches!(version, SearchVersion::Buffer { .. })
                    && self.complete_search_navigation(navigation.id, false, cx)
                {
                    window.focus(&self.focus_handle, cx);
                }
            }
            SearchTarget::Draft(session) => {
                if self.sessions.get(session).is_some()
                    && self.activate_session(session, cx)
                    && self.complete_search_navigation(navigation.id, false, cx)
                {
                    window.focus(&self.focus_handle, cx);
                }
            }
        }
        if self.content_search.authorized_navigation_id == Some(navigation.id) {
            self.content_search.authorized_navigation_id = None;
        }
    }

    fn complete_search_navigation(
        &mut self,
        navigation_id: u64,
        disk_content_verified: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(navigation) = self.content_search.pending_navigation.clone() else {
            return false;
        };
        if navigation.id != navigation_id || navigation.key != self.content_search.key() {
            return false;
        }
        let session_matches = match (&navigation.target, navigation.version) {
            (
                SearchTarget::Draft(target),
                SearchVersion::Buffer {
                    session,
                    generation,
                    revision,
                },
            ) => {
                *target == session
                    && self.sessions.active_id() == session
                    && self.sessions.get(session).is_some_and(|current| {
                        current.generation() == generation && current.revision() == revision
                    })
            }
            (
                SearchTarget::File(path),
                SearchVersion::Buffer {
                    session,
                    generation,
                    revision,
                },
            ) => {
                self.sessions.active_id() == session
                    && self.sessions.session_for_path(path) == Some(session)
                    && self.sessions.get(session).is_some_and(|current| {
                        current.generation() == generation && current.revision() == revision
                    })
            }
            (SearchTarget::File(path), SearchVersion::Disk { .. }) => {
                disk_content_verified && self.sessions.active().path() == Some(path.as_path())
            }
            _ => false,
        };
        if !session_matches {
            self.mark_search_navigation_stale(cx);
            return false;
        }
        if matches!(&navigation.target, SearchTarget::File(_))
            && matches!(navigation.version, SearchVersion::Disk { .. })
        {
            let session = self.sessions.active();
            let buffer_version = SearchVersion::Buffer {
                session: session.id(),
                generation: session.generation(),
                revision: session.revision(),
            };
            for file in &mut self.content_search.displayed_files {
                if file.target == navigation.target && file.version == navigation.version {
                    file.version = buffer_version;
                }
            }
        }
        if self
            .editor_mut()
            .set_selection(Selection {
                anchor: navigation.range.start,
                active: navigation.range.end,
            })
            .is_err()
        {
            self.mark_search_navigation_stale(cx);
            return false;
        }
        self.content_search.pending_navigation = None;
        self.content_search.results_focused = false;
        self.content_search.input_focused = false;
        self.scroll_cursor_into_view();
        cx.notify();
        true
    }

    pub(super) fn verify_loaded_search_navigation(
        &self,
        path: &Path,
        loaded: &LoadedFile,
    ) -> Option<Result<u64, ()>> {
        let navigation = self.content_search.pending_navigation.as_ref()?;
        if navigation.key != self.content_search.key()
            || !matches!(&navigation.target, SearchTarget::File(target) if target == path)
        {
            return None;
        }
        let SearchVersion::Disk {
            stamp: Some(search_stamp),
        } = navigation.version
        else {
            return Some(Err(()));
        };
        let Some(search_identity) = navigation.identity.as_ref() else {
            return Some(Err(()));
        };
        let query = match SearchQuery::new(
            self.content_search.query_text.clone(),
            self.content_search.case_sensitive,
        ) {
            Ok(query) => query,
            Err(_) => return Some(Err(())),
        };
        let engine = match SearchEngine::new(SearchRequest::new(
            navigation.key,
            query,
            SearchCancellationToken::new(),
        )) {
            Ok(engine) => engine,
            Err(_) => return Some(Err(())),
        };
        let context_matches = loaded
            .document
            .text(navigation.context_range)
            .is_ok_and(|context| context == navigation.context_source);
        let hit_matches = loaded
            .document
            .text(navigation.range)
            .is_ok_and(|matched| engine.matches_exact(matched.as_bytes()));
        if loaded.identity.is_same_file(search_identity)
            && loaded.stamp == Some(search_stamp)
            && context_matches
            && hit_matches
        {
            Some(Ok(navigation.id))
        } else {
            Some(Err(()))
        }
    }

    pub(super) fn finish_pending_search_navigation(
        &mut self,
        navigation_id: u64,
        disk_content_verified: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        self.complete_search_navigation(navigation_id, disk_content_verified, cx)
    }

    pub(super) fn discard_pending_search_navigation_for_path(
        &mut self,
        path: &Path,
        cx: &mut Context<Self>,
    ) {
        let matches = self.content_search.pending_navigation.as_ref().is_some_and(
            |navigation| matches!(&navigation.target, SearchTarget::File(target) if target == path),
        );
        if matches {
            self.mark_search_navigation_stale(cx);
        }
    }

    fn mark_search_navigation_stale(&mut self, cx: &mut Context<Self>) {
        self.content_search.pending_navigation = None;
        self.content_search.authorized_navigation_id = None;
        self.content_search.has_stale_results = true;
        self.content_search.status = ContentSearchStatus::Partial;
        self.content_search.status_detail =
            Some("内容が変わりました。再検索してから選び直してください".to_owned());
        cx.notify();
    }
}

impl ContentSearchState {
    fn select_first_result(&mut self) {
        self.selected_file = 0;
        self.selected_hit = 0;
    }

    fn selected_result(&self) -> Option<(usize, usize)> {
        let file = self.displayed_files.get(self.selected_file)?;
        (self.selected_hit < file.hits.len()).then_some((self.selected_file, self.selected_hit))
    }

    fn move_selection(&mut self, down: bool) {
        let rows = self
            .displayed_files
            .iter()
            .map(|file| file.hits.len())
            .sum::<usize>();
        if rows == 0 {
            return;
        }
        let current = self
            .displayed_files
            .iter()
            .take(self.selected_file)
            .map(|file| file.hits.len())
            .sum::<usize>()
            + self.selected_hit;
        let next = if down {
            (current + 1).min(rows - 1)
        } else {
            current.saturating_sub(1)
        };
        let mut ordinal = next;
        for (file_index, file) in self.displayed_files.iter().enumerate() {
            if ordinal < file.hits.len() {
                self.selected_file = file_index;
                self.selected_hit = ordinal;
                return;
            }
            ordinal -= file.hits.len();
        }
    }

    fn result_row_count(&self, root: Option<&Path>) -> usize {
        let hits = self
            .displayed_files
            .iter()
            .map(|file| file.hits.len())
            .sum::<usize>();
        let headings = self
            .displayed_files
            .iter()
            .filter(|file| !file.hits.is_empty())
            .count();
        let warnings = self.warnings.len() + usize::from(self.warning_count > self.warnings.len());
        hits + headings + warnings + usize::from(root.is_none())
    }

    fn visible_rows(
        &self,
        start: usize,
        count: usize,
        root: Option<&Path>,
        cx: &mut Context<EditorView>,
    ) -> Vec<gpui::AnyElement> {
        use gpui::IntoElement as _;
        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        let mut ordinal = 0usize;
        let wanted_end = start.saturating_add(count);
        if root.is_none() && ordinal < wanted_end {
            ordinal += 1;
        }
        for (file_index, file) in self.displayed_files.iter().enumerate() {
            if file.hits.is_empty() {
                continue;
            }
            if ordinal >= start && ordinal < wanted_end {
                rows.push(
                    div()
                        .id(("content-search-file", file_index))
                        .h(px(SIDEBAR_ROW_HEIGHT))
                        .px_1()
                        .text_color(rgb(0x9aa4b2))
                        .truncate()
                        .child(file.label.clone())
                        .into_any_element(),
                );
            }
            ordinal += 1;
            for (hit_index, hit) in file.hits.iter().enumerate() {
                if ordinal >= start && ordinal < wanted_end {
                    let selected = self.results_focused
                        && self.selected_file == file_index
                        && self.selected_hit == hit_index;
                    let excerpt = search_excerpt(&hit.context_source, hit.context_range, hit.range);
                    rows.push(
                        div()
                            .id(format!("content-search-hit-{file_index}-{hit_index}"))
                            .h(px(SIDEBAR_ROW_HEIGHT))
                            .px_1()
                            .rounded_sm()
                            .when(selected, |element| element.bg(rgb(0x32455f)))
                            .cursor_pointer()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_color(rgb(0x9aa4b2))
                                            .child(format!("{}", hit.line_number)),
                                    )
                                    .child(div().flex_1().min_w(px(0.0)).truncate().child(excerpt)),
                            )
                            .on_click(cx.listener(move |view, _, window, cx| {
                                view.open_content_search_result(file_index, hit_index, window, cx);
                            }))
                            .into_any_element(),
                    );
                }
                ordinal += 1;
                if ordinal >= wanted_end {
                    break;
                }
            }
            if ordinal >= wanted_end {
                break;
            }
        }
        for (index, warning) in self.warnings.iter().enumerate() {
            if ordinal >= start && ordinal < wanted_end {
                rows.push(
                    div()
                        .id(("content-search-warning", index))
                        .h(px(SIDEBAR_ROW_HEIGHT))
                        .px_1()
                        .text_color(rgb(0xb42318))
                        .truncate()
                        .child(search_warning_text(warning))
                        .into_any_element(),
                );
            }
            ordinal += 1;
        }
        if self.warning_count > self.warnings.len() && ordinal >= start && ordinal < wanted_end {
            rows.push(
                div()
                    .id("content-search-more-warnings")
                    .h(px(SIDEBAR_ROW_HEIGHT))
                    .px_1()
                    .text_color(rgb(0xb42318))
                    .child(format!(
                        "他 {} 件の警告",
                        self.warning_count - self.warnings.len()
                    ))
                    .into_any_element(),
            );
        }
        rows
    }

    fn accept_search_event(&mut self, event: SearchEvent, work_folder: Option<&WorkFolder>) {
        let key = match &event {
            SearchEvent::File { key, .. }
            | SearchEvent::Warning { key, .. }
            | SearchEvent::Progress { key, .. }
            | SearchEvent::Finished { key, .. } => *key,
        };
        if key != self.key() {
            return;
        }
        match event {
            SearchEvent::File { result, .. } => {
                self.files_finished = self.files_finished.saturating_add(1);
                let label = match &result.target {
                    SearchTarget::File(path) => work_folder.map_or_else(
                        || path.display().to_string(),
                        |folder| {
                            path.strip_prefix(folder.root())
                                .unwrap_or(path)
                                .display()
                                .to_string()
                        },
                    ),
                    SearchTarget::Draft(id) => format!("未保存のメモ · {}", id.0),
                };
                if result.hits.is_empty() {
                    if result.completion == SearchFileCompletion::LimitReached {
                        self.mark_partial();
                    }
                } else {
                    let group_index = self.displayed_files.len();
                    self.displayed_files.push(DisplayedSearchFile {
                        key: result.key,
                        target: result.target.clone(),
                        version: result.version,
                        identity: result.identity.clone(),
                        label,
                        hits: Vec::new(),
                        completion: result.completion,
                    });
                    self.pending_file = Some(PendingFileDelivery {
                        group_index,
                        result,
                    });
                }
            }
            SearchEvent::Warning { warning, .. } => {
                self.files_finished = self.files_finished.saturating_add(1);
                self.warning_count += 1;
                if self.warnings.len() < MAX_SEARCH_ERROR_DETAILS {
                    self.warnings.push(warning);
                }
                self.mark_partial();
            }
            SearchEvent::Progress { .. } => {}
            SearchEvent::Finished { completion, .. } => {
                self.terminal_key = Some(key);
                self.terminal_reason = Some(completion);
            }
        }
    }

    fn mark_partial(&mut self) {
        if self.status == ContentSearchStatus::Searching {
            self.status_detail = Some("一部のファイルを検索できませんでした".to_owned());
        }
    }

    fn mark_partial_for_current_work(&mut self) {
        self.mark_partial();
    }

    fn stop_at_global_limit(&mut self) {
        let Some(work) = self.current_work.take() else {
            return;
        };
        let key = work.key;
        work.cancellation.cancel();
        self.receiver = None;
        self.pending_file = None;
        self.status = ContentSearchStatus::Stopping;
        self.status_detail = Some("検索上限に達しました。表示中は検出済みの一致です".to_owned());
        self.terminal_key = Some(key);
        self.terminal_reason = Some(SearchCompletion::Partial);
        if self.active_workers_for(key) == 0 {
            self.status = ContentSearchStatus::Partial;
            self.terminal_key = None;
            self.terminal_reason = None;
        }
    }

    fn finish_search_if_drained(&mut self, channel_empty: bool) {
        let Some(work) = self.current_work.as_ref() else {
            return;
        };
        let key = work.key;
        let no_active = self.active_workers_for(key) == 0;
        let all_sources_claimed = work.next_source.load(Ordering::Acquire) >= work.sources.len();
        if no_active && all_sources_claimed && channel_empty && self.pending_file.is_none() {
            let partial = self.warning_count > 0
                || self
                    .displayed_files
                    .iter()
                    .any(|file| file.completion == SearchFileCompletion::LimitReached)
                || self.has_stale_results
                || self.terminal_reason == Some(SearchCompletion::Partial);
            self.current_work = None;
            self.receiver = None;
            self.status = if partial {
                ContentSearchStatus::Partial
            } else {
                ContentSearchStatus::Complete
            };
            self.status_detail = self
                .has_stale_results
                .then(|| "内容が変わりました。再検索してください".to_owned());
            self.terminal_key = None;
            self.terminal_reason = None;
        }
    }

    fn recount_results(&mut self) {
        self.result_hit_count = self
            .displayed_files
            .iter()
            .map(|file| file.hits.len())
            .sum();
        self.result_text_bytes = self
            .displayed_files
            .iter()
            .flat_map(|file| &file.hits)
            .map(|hit| hit.context_source.len())
            .sum();
    }
}

fn run_search_worker(work: Arc<SearchWork>, files: Arc<dyn FileService>) -> Result<(), String> {
    let request = SearchRequest::new(work.key, work.query.clone(), work.cancellation.clone());
    let mut engine = SearchEngine::new(request).map_err(|error| error.to_string())?;
    loop {
        if work.cancellation.is_cancelled() {
            break;
        }
        let index = work.next_source.fetch_add(1, Ordering::AcqRel);
        let Some(source) = work.sources.get(index) else {
            break;
        };
        if work.cancellation.is_cancelled() {
            break;
        }
        let outcome = if let Some(input) = source.input() {
            engine.search(input)
        } else if let Some(path) = source.disk_path() {
            engine.search_file(files.as_ref(), path)
        } else {
            continue;
        };
        if matches!(outcome, SearchOutcome::Cancelled { .. }) {
            break;
        }
        let event = match outcome {
            SearchOutcome::File(mut result) => {
                reserve_global_delivery_budget(&work, &mut result);
                SearchEvent::File {
                    key: result.key,
                    result,
                }
            }
            SearchOutcome::Warning(warning) => SearchEvent::Warning {
                key: warning.key,
                warning,
            },
            SearchOutcome::Cancelled { .. } => break,
        };
        if !send_search_event(&work.sender, event, &work.cancellation) {
            break;
        }
    }
    Ok(())
}

fn file_result_is_current(result: &FileSearchResult, view: &EditorView) -> bool {
    target_version_is_current(&result.target, result.version, view)
}

/// Reserves this file's share of the search-wide hit/text ceilings before
/// its result is queued, so the bounded channel can never hold more total
/// retained context text than the ceiling the delivery loop already
/// enforces for displayed results. Without this, a count-bounded channel
/// alone let up to `MAX_SEARCH_QUEUED_FILES` queued files each carry up to
/// `MAX_SEARCH_HITS_PER_DOCUMENT` x `MAX_SEARCH_CONTEXT_BYTES` of context,
/// far above the intended aggregate ceiling (Issue #414/#417 S10 follow-up).
fn reserve_global_delivery_budget(work: &SearchWork, result: &mut FileSearchResult) {
    if result.hits.is_empty() {
        return;
    }
    loop {
        let hits_remaining = work.remaining_hit_budget.load(Ordering::Acquire);
        let text_remaining = work.remaining_text_budget.load(Ordering::Acquire);
        let take = delivery_hit_count(&result.hits, usize::MAX, hits_remaining, text_remaining);
        let text_bytes: usize = result.hits[..take]
            .iter()
            .map(|hit| hit.context_source.len())
            .sum();
        if work
            .remaining_hit_budget
            .compare_exchange(
                hits_remaining,
                hits_remaining - take,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            continue;
        }
        if work
            .remaining_text_budget
            .compare_exchange(
                text_remaining,
                text_remaining - text_bytes,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            work.remaining_hit_budget.fetch_add(take, Ordering::AcqRel);
            continue;
        }
        if take < result.hits.len() {
            result.completion = SearchFileCompletion::LimitReached;
            result.hits.truncate(take);
        }
        break;
    }
}

fn delivery_hit_count(
    hits: &[hane_session::search::SearchHit],
    rows_remaining: usize,
    hits_remaining: usize,
    text_bytes_remaining: usize,
) -> usize {
    let mut count = 0;
    let mut text_bytes = 0usize;
    for hit in hits.iter().take(rows_remaining.min(hits_remaining)) {
        let next_text_bytes = text_bytes.saturating_add(hit.context_source.len());
        if next_text_bytes > text_bytes_remaining {
            break;
        }
        text_bytes = next_text_bytes;
        count += 1;
    }
    count
}

fn target_version_is_current(
    target: &SearchTarget,
    version: SearchVersion,
    view: &EditorView,
) -> bool {
    match (target, version) {
        (
            SearchTarget::Draft(target_session),
            SearchVersion::Buffer {
                session,
                generation,
                revision,
            },
        ) => {
            *target_session == session
                && view.work_folder_drafts.contains_key(target_session)
                && view.sessions.get(session).is_some_and(|current| {
                    current.generation() == generation && current.revision() == revision
                })
        }
        (
            SearchTarget::File(path),
            SearchVersion::Buffer {
                session,
                generation,
                revision,
            },
        ) => {
            view.sessions.session_for_path(path) == Some(session)
                && view.sessions.get(session).is_some_and(|current| {
                    current.generation() == generation && current.revision() == revision
                })
        }
        (SearchTarget::File(path), SearchVersion::Disk { stamp: Some(_) }) => {
            view.work_folder
                .as_ref()
                .is_some_and(|folder| folder.entry_for_path(path).is_some())
                && view.sessions.session_for_path(path).is_none()
        }
        _ => false,
    }
}

fn send_search_event(
    sender: &SyncSender<SearchEvent>,
    mut event: SearchEvent,
    cancellation: &SearchCancellationToken,
) -> bool {
    loop {
        if cancellation.is_cancelled() {
            return false;
        }
        match sender.try_send(event) {
            Ok(()) => return true,
            Err(TrySendError::Disconnected(_)) => return false,
            Err(TrySendError::Full(returned)) => {
                event = returned;
                std::thread::sleep(SEARCH_SEND_RETRY);
            }
        }
    }
}

fn search_warning_text(warning: &SearchWarning) -> String {
    let target = match &warning.target {
        SearchTarget::File(path) => path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        ),
        SearchTarget::Draft(id) => format!("未保存のメモ {}", id.0),
    };
    let reason = match warning.kind {
        SearchWarningKind::OpenFailed => "開けませんでした",
        SearchWarningKind::InvalidUtf8 => "UTF-8ではありません",
        SearchWarningKind::NulByte => "NULを含むため検索できません",
        SearchWarningKind::ReadFailed => "読み取りに失敗しました",
        SearchWarningKind::StampChanged => "検索中に変更されました",
        SearchWarningKind::StampUnavailable => "内容の確認ができませんでした",
        SearchWarningKind::LineTooLong => "1行が長すぎます",
        SearchWarningKind::Internal => "検索に失敗しました",
    };
    warning.detail.as_ref().map_or_else(
        || format!("{target}: {reason}"),
        |detail| format!("{target}: {reason} ({detail})"),
    )
}

fn search_excerpt(
    text: &str,
    context: hane_document::SourceRange,
    hit: hane_document::SourceRange,
) -> String {
    let prefix_len = hit.start.0.saturating_sub(context.start.0).min(text.len());
    let full_match_len = hit.end.0.saturating_sub(hit.start.0);
    let match_len = full_match_len.min(text.len().saturating_sub(prefix_len));
    // `context` covers exactly `text`'s byte span, so if the match extends past
    // `context.end`, the excerpt only holds a prefix of the match, not the whole hit.
    let match_truncated = hit.end.0 > context.end.0;
    let start = if context.start.0 > 0 { "…" } else { "" };
    let match_ellipsis = if match_truncated { "…" } else { "" };
    let end = if !match_truncated && context.end.0 > hit.end.0 {
        "…"
    } else {
        ""
    };
    format!(
        "{start}{}【{}{match_ellipsis}】{}{end}",
        &text[..prefix_len],
        &text[prefix_len..prefix_len + match_len],
        &text[prefix_len + match_len..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use hane_session::search::MAX_SEARCH_CONTEXT_BYTES;

    fn key(query_epoch: u64) -> SearchKey {
        SearchKey {
            workspace_epoch: 4,
            query_epoch,
        }
    }

    fn search_hit(context_source: &str) -> hane_session::search::SearchHit {
        hane_session::search::SearchHit {
            range: SourceRange::new(0, 1),
            line_number: 1,
            context_range: SourceRange::new(0, context_source.len()),
            context_source: context_source.to_owned(),
        }
    }

    #[test]
    fn excerpt_marks_a_match_cut_by_the_context_limit_inside_the_brackets() {
        // A match over 1 KiB long cannot fit in `context_source`, which the
        // searcher caps at `MAX_SEARCH_CONTEXT_BYTES`: the excerpt only holds
        // the match's first bytes, not the whole hit.
        let full_match_len = MAX_SEARCH_CONTEXT_BYTES + 976;
        assert!(full_match_len > 1024);
        let text = "a".repeat(MAX_SEARCH_CONTEXT_BYTES);
        let context = SourceRange::new(0, MAX_SEARCH_CONTEXT_BYTES);
        let hit = SourceRange::new(0, full_match_len);

        let excerpt = search_excerpt(&text, context, hit);

        assert_eq!(
            excerpt,
            format!("【{}…】", "a".repeat(MAX_SEARCH_CONTEXT_BYTES))
        );
    }

    #[test]
    fn excerpt_keeps_the_trailing_ellipsis_when_only_following_context_is_cut() {
        // The match itself fits entirely inside the excerpt; only the
        // context that follows it is truncated. This must still render the
        // ellipsis after the closing bracket, as before this fix.
        let text = format!("{}MATCH{}", "x".repeat(10), "y".repeat(10));
        let context = SourceRange::new(0, text.len());
        let hit = SourceRange::new(10, 15);

        let excerpt = search_excerpt(&text, context, hit);

        assert_eq!(
            excerpt,
            format!("{}【MATCH】{}…", "x".repeat(10), "y".repeat(10))
        );
    }

    #[test]
    fn result_delivery_respects_frame_hit_and_cumulative_text_limits() {
        let hits = vec![
            search_hit("a".repeat(700).as_str()),
            search_hit("b".repeat(700).as_str()),
        ];

        assert_eq!(
            delivery_hit_count(&hits, 128, 10_000, 1_000),
            1,
            "the sum of delivered excerpts must stay under the text budget"
        );
        assert_eq!(delivery_hit_count(&hits, 1, 10_000, 2_000), 1);
        assert_eq!(delivery_hit_count(&hits, 128, 1, 2_000), 1);
    }

    #[test]
    fn global_delivery_budget_trims_a_single_files_hits_to_the_remaining_ceiling() {
        let (sender, _receiver) = mpsc::sync_channel(MAX_SEARCH_QUEUED_FILES);
        let work = SearchWork::new(
            key(1),
            SearchQuery::new("query", false).unwrap(),
            SearchCancellationToken::new(),
            Arc::from([]),
            sender,
        );
        work.remaining_hit_budget.store(3, Ordering::Relaxed);
        let mut result = FileSearchResult {
            key: key(1),
            target: SearchTarget::File(PathBuf::from("note.md")),
            version: SearchVersion::Disk { stamp: None },
            identity: None,
            hits: (0..5).map(|_| search_hit("x")).collect(),
            completion: SearchFileCompletion::Complete,
        };

        reserve_global_delivery_budget(&work, &mut result);

        assert_eq!(result.hits.len(), 3);
        assert_eq!(result.completion, SearchFileCompletion::LimitReached);
        assert_eq!(work.remaining_hit_budget.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn global_delivery_budget_trims_hits_once_the_text_ceiling_is_reached() {
        let (sender, _receiver) = mpsc::sync_channel(MAX_SEARCH_QUEUED_FILES);
        let work = SearchWork::new(
            key(1),
            SearchQuery::new("query", false).unwrap(),
            SearchCancellationToken::new(),
            Arc::from([]),
            sender,
        );
        work.remaining_text_budget.store(1_000, Ordering::Relaxed);
        let mut result = FileSearchResult {
            key: key(1),
            target: SearchTarget::File(PathBuf::from("note.md")),
            version: SearchVersion::Disk { stamp: None },
            identity: None,
            hits: vec![search_hit(&"a".repeat(700)), search_hit(&"b".repeat(700))],
            completion: SearchFileCompletion::Complete,
        };

        reserve_global_delivery_budget(&work, &mut result);

        assert_eq!(result.hits.len(), 1, "the second 700-byte hit must stay under the text ceiling");
        assert_eq!(result.completion, SearchFileCompletion::LimitReached);
        assert_eq!(work.remaining_text_budget.load(Ordering::Relaxed), 300);
    }

    // Issue #414/#417 S10 follow-up: a count-bounded channel alone let every
    // queued file carry up to `MAX_SEARCH_HITS_PER_DOCUMENT` (1,000) x
    // `MAX_SEARCH_CONTEXT_BYTES` (1 KiB) of context, so `MAX_SEARCH_QUEUED_FILES`
    // queued files could together retain far more context text than the
    // aggregate ceiling the delivery loop otherwise enforces for displayed
    // results. Reserving from the same shared ceiling before a file is ever
    // queued keeps the worker-produced-plus-queued total bounded regardless
    // of how many files are in flight; this exercises enough files, each
    // with a representative per-document hit count, that the unbounded
    // total would clearly exceed `MAX_SEARCH_HITS_TOTAL` without the fix.
    #[test]
    fn global_delivery_budget_is_shared_so_a_bounded_queue_cannot_exceed_it() {
        const HITS_PER_FILE: usize = 150;
        let (sender, _receiver) = mpsc::sync_channel(MAX_SEARCH_QUEUED_FILES);
        let work = SearchWork::new(
            key(1),
            SearchQuery::new("query", false).unwrap(),
            SearchCancellationToken::new(),
            Arc::from([]),
            sender,
        );
        let big_context = "x".repeat(MAX_SEARCH_CONTEXT_BYTES);
        let make_result = || FileSearchResult {
            key: key(1),
            target: SearchTarget::File(PathBuf::from("note.md")),
            version: SearchVersion::Disk { stamp: None },
            identity: None,
            hits: (0..HITS_PER_FILE)
                .map(|_| search_hit(&big_context))
                .collect(),
            completion: SearchFileCompletion::Complete,
        };

        let mut total_hits = 0usize;
        let mut total_text = 0usize;
        for _ in 0..(MAX_SEARCH_QUEUED_FILES + 1) {
            let mut result = make_result();
            reserve_global_delivery_budget(&work, &mut result);
            total_hits += result.hits.len();
            total_text += result
                .hits
                .iter()
                .map(|hit| hit.context_source.len())
                .sum::<usize>();
        }

        assert!(
            (MAX_SEARCH_QUEUED_FILES + 1) * HITS_PER_FILE > MAX_SEARCH_HITS_TOTAL,
            "this scenario must exceed the hit ceiling for the assertions below to be meaningful"
        );
        assert!(total_hits <= MAX_SEARCH_HITS_TOTAL);
        assert!(total_text <= MAX_SEARCH_RESULT_TEXT_BYTES);
        assert_eq!(
            work.remaining_hit_budget.load(Ordering::Relaxed),
            MAX_SEARCH_HITS_TOTAL - total_hits
        );
        assert_eq!(
            work.remaining_text_budget.load(Ordering::Relaxed),
            MAX_SEARCH_RESULT_TEXT_BYTES - total_text
        );
    }

    #[test]
    fn reaching_a_global_delivery_limit_is_reported_as_partial() {
        let cancellation = SearchCancellationToken::new();
        let (sender, _receiver) = mpsc::sync_channel(MAX_SEARCH_QUEUED_FILES);
        let work = Arc::new(SearchWork::new(
            key(1),
            SearchQuery::new("query", false).unwrap(),
            cancellation.clone(),
            Arc::from([]),
            sender,
        ));
        let mut state = ContentSearchState::default();
        state.current_work = Some(work);
        state.status = ContentSearchStatus::Searching;

        state.stop_at_global_limit();

        assert!(cancellation.is_cancelled());
        assert_eq!(state.status, ContentSearchStatus::Partial);
        assert!(state.current_work.is_none());
        assert!(
            state
                .status_detail
                .as_deref()
                .is_some_and(|text| text.contains("上限"))
        );
    }

    #[test]
    fn cancelled_workers_still_use_the_two_controller_slots_until_removed() {
        let mut state = ContentSearchState::default();
        let first = SearchCancellationToken::new();
        let second = SearchCancellationToken::new();
        first.cancel();
        state.active_workers.insert(
            1,
            ActiveSearchWorker {
                key: key(1),
                cancellation: first,
            },
        );
        state.active_workers.insert(
            2,
            ActiveSearchWorker {
                key: key(2),
                cancellation: second,
            },
        );

        assert!(!state.has_worker_capacity());
        state.active_workers.remove(&1);
        assert!(state.has_worker_capacity());
    }

    #[test]
    fn returning_to_a_query_after_an_intermediate_query_gets_a_new_epoch() {
        let mut state = ContentSearchState::default();
        let first_a = state.key();
        state.invalidate(false);
        let b = state.key();
        state.invalidate(false);
        let second_a = state.key();

        assert_ne!(first_a, b);
        assert_ne!(b, second_a);
        assert_ne!(first_a, second_a);
    }

    #[test]
    fn escape_leaves_search_after_navigation_but_preserves_editor_ime_composition() {
        let mut state = ContentSearchState::default();
        state.mode = SidebarMode::Content;

        // Opening a hit clears both search focus flags while the sidebar stays
        // in content-search mode. Escape must still return to file mode.
        assert!(!state.input_focused);
        assert!(!state.results_focused);
        assert!(state.should_leave_on_escape(false));
        assert!(!state.should_leave_on_escape(true));
    }

    #[gpui::test]
    fn ctrl_tab_does_not_switch_tabs_while_content_search_input_is_focused(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("one\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        let first = view.update(cx, |view, cx| {
            let first = view.sessions.active_id();
            view.sessions.open_untitled("two\n", "Second");
            assert!(view.sessions.activate(first));
            view.on_document_replaced();
            cx.notify();
            first
        });
        cx.run_until_parked();

        // Focus the editor's "HaneEditor" key context, which ctrl-tab is
        // scoped to, the same way a real click would before opening search.
        let point = cx
            .debug_bounds("row-0-0")
            .expect("first row painted")
            .center();
        cx.simulate_mouse_down(point, MouseButton::Left, gpui::Modifiers::none());
        cx.simulate_mouse_up(point, MouseButton::Left, gpui::Modifiers::none());
        cx.run_until_parked();

        cx.update(|window, app| {
            view.update(app, |view, cx| view.open_content_search(window, cx));
        });
        cx.run_until_parked();
        assert!(view.read_with(cx, |view, _| view.content_search_input_is_focused()));

        cx.simulate_keystrokes("ctrl-tab");
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |view, _| view.sessions.active_id()), first);
        assert!(view.read_with(cx, |view, _| view.content_search_input_is_focused()));
    }

    #[gpui::test]
    fn ctrl_tab_does_not_switch_tabs_while_content_search_results_are_focused(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("one\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        let first = view.update(cx, |view, cx| {
            let first = view.sessions.active_id();
            view.sessions.open_untitled("two\n", "Second");
            assert!(view.sessions.activate(first));
            view.on_document_replaced();
            cx.notify();
            first
        });
        cx.run_until_parked();

        let point = cx
            .debug_bounds("row-0-0")
            .expect("first row painted")
            .center();
        cx.simulate_mouse_down(point, MouseButton::Left, gpui::Modifiers::none());
        cx.simulate_mouse_up(point, MouseButton::Left, gpui::Modifiers::none());
        cx.run_until_parked();

        // Open content search through the normal entry point first so the
        // sidebar's InputState exists before rendering, then move focus onto
        // the results list the way opening a hit list and pressing tab/down
        // would, without going through a real search.
        cx.update(|window, app| {
            view.update(app, |view, cx| view.open_content_search(window, cx));
        });
        cx.run_until_parked();
        view.update(cx, |view, cx| {
            view.content_search.input_focused = false;
            view.content_search.results_focused = true;
            cx.notify();
        });
        cx.run_until_parked();

        cx.simulate_keystrokes("ctrl-tab");
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |view, _| view.sessions.active_id()), first);
        assert!(view.read_with(cx, |view, _| view.content_search_results_focused()));
    }

    #[gpui::test]
    fn clicking_the_editor_body_clears_content_search_focus_so_backspace_reaches_the_editor(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("one\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        // Open content search through the normal entry point and then move
        // focus onto the results list the way tabbing into a hit list would,
        // without going through a real search.
        cx.update(|window, app| {
            view.update(app, |view, cx| view.open_content_search(window, cx));
        });
        cx.run_until_parked();
        view.update(cx, |view, cx| {
            view.content_search.input_focused = false;
            view.content_search.results_focused = true;
            cx.notify();
        });
        cx.run_until_parked();
        assert!(view.read_with(cx, |view, _| view.content_search_results_focused()));

        // Click into the document body the way a user does after inspecting
        // search results, without going through Escape/leave first.
        let point = cx
            .debug_bounds("row-0-0")
            .expect("first row painted")
            .center();
        cx.simulate_mouse_down(point, MouseButton::Left, gpui::Modifiers::none());
        cx.simulate_mouse_up(point, MouseButton::Left, gpui::Modifiers::none());
        cx.run_until_parked();

        assert!(!view.read_with(cx, |view, _| view.content_search_input_is_focused()));
        assert!(!view.read_with(cx, |view, _| view.content_search_results_focused()));

        // Backspace must now reach the editor instead of being swallowed by
        // the lingering content-search focus flags (see `actions.rs`).
        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset(3)))
                .unwrap();
            cx.notify();
        });
        cx.run_until_parked();

        cx.simulate_keystrokes("backspace");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().document().full_text(), "on\n");
        });
    }

    #[gpui::test]
    fn clicking_the_editor_body_clears_content_search_input_focus(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("one\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        cx.update(|window, app| {
            view.update(app, |view, cx| view.open_content_search(window, cx));
        });
        cx.run_until_parked();
        assert!(view.read_with(cx, |view, _| view.content_search_input_is_focused()));

        let point = cx
            .debug_bounds("row-0-0")
            .expect("first row painted")
            .center();
        cx.simulate_mouse_down(point, MouseButton::Left, gpui::Modifiers::none());
        cx.simulate_mouse_up(point, MouseButton::Left, gpui::Modifiers::none());
        cx.run_until_parked();

        assert!(!view.read_with(cx, |view, _| view.content_search_input_is_focused()));
        assert!(!view.read_with(cx, |view, _| view.content_search_results_focused()));
    }

    #[test]
    fn dropping_the_search_controller_cancels_current_and_active_workers() {
        let active_cancellation = SearchCancellationToken::new();
        let current_cancellation = SearchCancellationToken::new();
        let (sender, _receiver) = mpsc::sync_channel(MAX_SEARCH_QUEUED_FILES);
        let mut state = ContentSearchState::default();
        state.active_workers.insert(
            1,
            ActiveSearchWorker {
                key: key(1),
                cancellation: active_cancellation.clone(),
            },
        );
        state.current_work = Some(Arc::new(SearchWork::new(
            key(2),
            SearchQuery::new("query", false).unwrap(),
            current_cancellation.clone(),
            Arc::from([]),
            sender,
        )));

        drop(state);

        assert!(active_cancellation.is_cancelled());
        assert!(current_cancellation.is_cancelled());
    }

    #[test]
    fn a_late_terminal_event_cannot_complete_a_newer_query() {
        let mut state = ContentSearchState::default();
        state.query_epoch = 3;
        state.status = ContentSearchStatus::Searching;

        state.accept_search_event(
            SearchEvent::Finished {
                key: key(2),
                completion: SearchCompletion::Complete,
            },
            None,
        );

        assert_eq!(state.status, ContentSearchStatus::Searching);
        assert_eq!(state.terminal_key, None);
    }

    #[test]
    fn a_full_result_queue_stops_sending_after_cancellation() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let queued = SearchEvent::Finished {
            key: key(1),
            completion: SearchCompletion::Complete,
        };
        sender.try_send(queued.clone()).unwrap();
        let cancellation = SearchCancellationToken::new();
        let worker_cancellation = cancellation.clone();
        let (started, wait_started) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            started.send(()).unwrap();
            send_search_event(
                &sender,
                SearchEvent::Progress {
                    key: key(1),
                    files_scanned: 1,
                    files_total: 2,
                    hits_found: 0,
                },
                &worker_cancellation,
            )
        });

        wait_started.recv().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        cancellation.cancel();

        assert!(!worker.join().unwrap());
        assert_eq!(receiver.try_recv().unwrap(), queued);
    }

    // Issue #414/#417: delivery was previously capped at
    // `MAX_SEARCH_DELIVERIES_PER_FRAME` (4) file results per
    // `SEARCH_DELIVERY_POLL` (16ms) tick regardless of how many files the
    // workers had already scanned, capping sustained throughput at ~250
    // files/sec no matter how fast disk/CPU search itself was. This drives a
    // real warm, zero-hit search over more files than the old per-tick cap
    // through the actual worker/channel/poll pipeline and requires it to
    // finish within a single poll tick instead of needing one tick per 4
    // files.
    #[gpui::test]
    fn content_search_delivers_many_zero_hit_files_within_one_poll_tick(
        cx: &mut gpui::TestAppContext,
    ) {
        use hane_session::OsWorkFolderScanner;

        static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "hane-417-search-throughput-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        const FILE_COUNT: usize = 40;
        for index in 0..FILE_COUNT {
            std::fs::write(root.join(format!("Note-{index:02}.md")), "no match here").unwrap();
        }
        let folder = OsWorkFolderScanner.scan(&root).unwrap();
        assert_eq!(folder.entries().len(), FILE_COUNT);

        let (view, cx) = cx.add_window_view(|_, cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("", "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.initialize_content_search_input(window, cx);
                view.work_folder = Some(folder);
                view.content_search.mode = SidebarMode::Content;
                view.content_search.query_text = "needle".to_owned();
                view.start_content_search(cx);
            });
        });

        // Lets the workers finish scanning and queue their results, then
        // fires exactly one delivery poll tick.
        cx.run_until_parked();
        cx.executor().advance_clock(SEARCH_DELIVERY_POLL);
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert_eq!(view.content_search.files_finished, FILE_COUNT);
            assert_eq!(view.content_search.status, ContentSearchStatus::Complete);
        });

        std::fs::remove_dir_all(root).unwrap();
    }

    // Issue #414/#417 S10 follow-up: zero-hit `SearchEvent::File` results add
    // no delivered rows, so `MAX_SEARCH_ROWS_PER_FRAME` alone did not bound
    // how many such events one poll tick could drain; a single tick could
    // keep draining the channel for as long as workers kept refilling it.
    // This pre-fills a channel with more zero-hit events than
    // `MAX_SEARCH_EVENTS_PER_POLL` (without any worker threads, so the
    // bounded channel can hold all of them at once) and checks that one
    // poll tick stops at the per-tick event budget and yields back to the
    // scheduler instead of draining the whole backlog in one call.
    #[gpui::test]
    fn content_search_zero_hit_events_are_bounded_per_poll_tick(cx: &mut gpui::TestAppContext) {
        const EVENT_COUNT: usize = MAX_SEARCH_EVENTS_PER_POLL * 3;
        let (sender, receiver) = mpsc::sync_channel(EVENT_COUNT);
        let search_key = key(1);
        for _ in 0..EVENT_COUNT {
            sender
                .try_send(SearchEvent::File {
                    key: search_key,
                    result: FileSearchResult {
                        key: search_key,
                        target: SearchTarget::File(PathBuf::from("note.md")),
                        version: SearchVersion::Disk { stamp: None },
                        identity: None,
                        hits: Vec::new(),
                        completion: SearchFileCompletion::Complete,
                    },
                })
                .unwrap();
        }

        let (view, cx) = cx.add_window_view(|_, cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("", "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });
        view.update(cx, |view, cx| {
            view.content_search.workspace_epoch = search_key.workspace_epoch;
            view.content_search.query_epoch = search_key.query_epoch;
            view.content_search.current_work = Some(Arc::new(SearchWork::new(
                search_key,
                SearchQuery::new("needle", false).unwrap(),
                SearchCancellationToken::new(),
                Arc::from([]),
                sender,
            )));
            view.content_search.receiver = Some(receiver);
            view.content_search.total_files = EVENT_COUNT;
            view.content_search.status = ContentSearchStatus::Searching;

            let keep_polling = view.poll_content_search_delivery(cx);

            assert!(
                keep_polling,
                "more zero-hit events remained queued than a single poll tick may drain"
            );
            assert_eq!(
                view.content_search.files_finished, MAX_SEARCH_EVENTS_PER_POLL,
                "one poll tick drained more than its per-tick event budget"
            );
            assert_eq!(view.content_search.status, ContentSearchStatus::Searching);
        });
    }

    #[gpui::test]
    fn a_draft_search_result_selects_the_original_utf8_byte_range(cx: &mut gpui::TestAppContext) {
        let source = "日本語🙂 **needle** の本文";
        let start = source.find("needle").unwrap();
        let end = start + "needle".len();
        let (view, cx) = cx.add_window_view(|_, cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled(source, "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });
        let (session, generation, revision) = view.read_with(cx, |view, _| {
            let session = view.sessions.active();
            (session.id(), session.generation(), session.revision())
        });

        cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.initialize_content_search_input(window, cx);
                view.work_folder_drafts.insert(
                    session,
                    WorkFolderDraft {
                        draft_id: hane_session::DraftId::generate(),
                        target_directory: PathBuf::from("/work"),
                    },
                );
                view.content_search.mode = SidebarMode::Content;
                view.content_search.query_text = "needle".to_owned();
                let key = view.content_search.key();
                view.content_search
                    .displayed_files
                    .push(DisplayedSearchFile {
                        key,
                        target: SearchTarget::Draft(session),
                        version: SearchVersion::Buffer {
                            session,
                            generation,
                            revision,
                        },
                        identity: None,
                        label: "未保存のメモ".to_owned(),
                        hits: vec![hane_session::search::SearchHit {
                            range: SourceRange::new(start, end),
                            line_number: 1,
                            context_range: SourceRange::new(0, source.len()),
                            context_source: source.to_owned(),
                        }],
                        completion: SearchFileCompletion::Complete,
                    });

                view.open_content_search_result(0, 0, window, cx);
            });
        });
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert_eq!(
                view.editor().selection(),
                Selection {
                    anchor: SourceOffset(start),
                    active: SourceOffset(end),
                }
            );
        });
    }

    #[gpui::test]
    fn search_sources_include_hidden_buffer_and_work_folder_draft_without_disk_duplicate(
        cx: &mut gpui::TestAppContext,
    ) {
        use hane_session::{FileIdentity, LoadedFile, OsWorkFolderScanner, WorkFolderScanner};

        static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "hane-414-buffer-sources-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("Note.md");
        std::fs::write(&path, "disk-only").unwrap();
        let folder = OsWorkFolderScanner.scan(&root).unwrap();

        let mut sessions = SessionSet::with_untitled("draft-only", "Untitled");
        let draft_id = sessions.active_id();
        let file_id = sessions.apply_open(
            None,
            LoadedFile {
                document: hane_document::RopeBuffer::from_text("buffer-only"),
                identity: FileIdentity::lexical(&path),
                stamp: None,
            },
        );
        assert!(sessions.activate(draft_id));

        let (view, cx) = cx.add_window_view(|_, cx| {
            EditorView::from_sessions(sessions, Arc::new(OsFileService), StateStores::memory(), cx)
        });
        let sources = view.update(cx, |view, _| {
            view.work_folder = Some(folder);
            view.work_folder_drafts.insert(
                draft_id,
                WorkFolderDraft {
                    draft_id: hane_session::DraftId::generate(),
                    target_directory: root.clone(),
                },
            );
            assert_eq!(view.sessions.active_id(), draft_id);
            assert!(view.sessions.get(file_id).is_some());
            view.content_search_sources()
        });

        assert_eq!(
            sources
                .iter()
                .filter(|source| matches!(source, SearchSource::FileBuffer { path: candidate, .. } if candidate == &path))
                .count(),
            1
        );
        assert!(
            !sources.iter().any(
                |source| matches!(source, SearchSource::Disk(candidate) if candidate == &path)
            )
        );
        assert!(sources.iter().any(
            |source| matches!(source, SearchSource::Draft { session, .. } if *session == draft_id)
        ));

        std::fs::remove_dir_all(root).unwrap();
    }

    // Issue #414: `work_folder_drafts` is keyed by `SessionId`, not by which
    // work folder created it, so a draft left over from a folder that has
    // since been switched away from (or a draft destined for an unrelated
    // subtree) must not be searched as if it belonged to the *current*
    // work folder just because its session id still resolves.
    #[gpui::test]
    fn search_sources_exclude_a_draft_targeting_a_different_work_folder(
        cx: &mut gpui::TestAppContext,
    ) {
        use hane_session::OsWorkFolderScanner;

        static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);
        let fixture = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let current_root = std::env::temp_dir().join(format!(
            "hane-414-draft-ownership-current-{}-{}",
            std::process::id(),
            fixture
        ));
        let other_root = std::env::temp_dir().join(format!(
            "hane-414-draft-ownership-other-{}-{}",
            std::process::id(),
            fixture
        ));
        std::fs::create_dir_all(&current_root).unwrap();
        std::fs::create_dir_all(&other_root).unwrap();
        let folder = OsWorkFolderScanner.scan(&current_root).unwrap();

        let (view, cx) = cx.add_window_view(|_, cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("current draft", "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });
        let current_draft_session = view.read_with(cx, |view, _| view.sessions.active_id());
        let other_draft_session = view.update(cx, |view, _| {
            view.sessions.open_untitled("other folder's draft", "Untitled")
        });

        let sources = view.update(cx, |view, _| {
            view.work_folder = Some(folder);
            view.work_folder_drafts.insert(
                current_draft_session,
                WorkFolderDraft {
                    draft_id: hane_session::DraftId::generate(),
                    target_directory: current_root.clone(),
                },
            );
            view.work_folder_drafts.insert(
                other_draft_session,
                WorkFolderDraft {
                    draft_id: hane_session::DraftId::generate(),
                    target_directory: other_root.clone(),
                },
            );
            view.content_search_sources()
        });

        assert!(
            sources.iter().any(
                |source| matches!(source, SearchSource::Draft { session, .. } if *session == current_draft_session)
            ),
            "the draft that belongs to the current work folder must be searched"
        );
        assert!(
            !sources.iter().any(
                |source| matches!(source, SearchSource::Draft { session, .. } if *session == other_draft_session)
            ),
            "a draft targeting an unrelated folder must not be searched as a current source"
        );

        std::fs::remove_dir_all(&current_root).unwrap();
        std::fs::remove_dir_all(&other_root).unwrap();
    }

    #[gpui::test]
    fn selecting_a_draft_hit_preserves_dirty_text_and_undo_history(cx: &mut gpui::TestAppContext) {
        let source = "before needle";
        let start = source.find("needle").unwrap();
        let end = start + "needle".len();
        let (view, cx) = cx.add_window_view(|_, cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled(source, "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });
        let (session, generation, revision, edited_source) = view.update(cx, |view, cx| {
            let session = view.sessions.active_id();
            let end = SourceOffset(view.editor().document().len_bytes().0);
            view.editor_mut()
                .set_selection(Selection::caret(end))
                .unwrap();
            view.editor_mut().insert_text(" edited").unwrap();
            view.after_input(cx);
            (
                session,
                view.sessions.active().generation(),
                view.sessions.active().revision(),
                view.editor().document().full_text(),
            )
        });
        let context_range = SourceRange::new(0, edited_source.len());
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.initialize_content_search_input(window, cx);
                view.work_folder_drafts.insert(
                    session,
                    WorkFolderDraft {
                        draft_id: hane_session::DraftId::generate(),
                        target_directory: PathBuf::from("/work"),
                    },
                );
                view.content_search.mode = SidebarMode::Content;
                view.content_search.query_text = "needle".to_owned();
                let key = view.content_search.key();
                view.content_search
                    .displayed_files
                    .push(DisplayedSearchFile {
                        key,
                        target: SearchTarget::Draft(session),
                        version: SearchVersion::Buffer {
                            session,
                            generation,
                            revision,
                        },
                        identity: None,
                        label: "未保存のメモ".to_owned(),
                        hits: vec![hane_session::search::SearchHit {
                            range: SourceRange::new(start, end),
                            line_number: 1,
                            context_range,
                            context_source: edited_source.clone(),
                        }],
                        completion: SearchFileCompletion::Complete,
                    });
                view.open_content_search_result(0, 0, window, cx);
            });
        });

        view.read_with(cx, |view, _| {
            let session = view.sessions.get(session).unwrap();
            assert_eq!(view.editor().document().full_text(), edited_source);
            assert!(session.is_dirty());
            assert!(view.editor().can_undo());
            assert_eq!(
                view.editor().selection(),
                Selection {
                    anchor: SourceOffset(start),
                    active: SourceOffset(end),
                }
            );
        });
    }

    #[gpui::test]
    fn a_disk_search_result_opens_through_the_existing_path_and_selects_its_utf8_range(
        cx: &mut gpui::TestAppContext,
    ) {
        use hane_session::search::{
            SearchCancellationToken, SearchEngine, SearchKey, SearchOutcome, SearchQuery,
            SearchRequest,
        };
        use hane_session::{OsWorkFolderScanner, WorkFolderScanner};

        static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "hane-414-search-navigation-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("本文.md");
        let source = "日本語🙂 **needle** の本文";
        std::fs::write(&path, source.as_bytes()).unwrap();
        let start = source.find("needle").unwrap();
        let end = start + "needle".len();

        let folder = OsWorkFolderScanner.scan(&root).unwrap();
        assert_eq!(folder.entries().len(), 1);
        let key = SearchKey {
            workspace_epoch: 0,
            query_epoch: 0,
        };
        let request = SearchRequest::new(
            key,
            SearchQuery::new("needle", false).unwrap(),
            SearchCancellationToken::new(),
        );
        let mut engine = SearchEngine::new(request).unwrap();
        let result = match engine.search_file(&OsFileService, &path) {
            SearchOutcome::File(result) => result,
            other => panic!("expected a disk search result, got {other:?}"),
        };

        let (view, cx) = cx.add_window_view(|_, cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("", "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.initialize_content_search_input(window, cx);
                view.work_folder = Some(folder);
                view.content_search.mode = SidebarMode::Content;
                view.content_search.query_text = "needle".to_owned();
                view.content_search
                    .displayed_files
                    .push(DisplayedSearchFile {
                        key: result.key,
                        target: result.target,
                        version: result.version,
                        identity: result.identity,
                        label: "本文.md".to_owned(),
                        hits: result.hits,
                        completion: result.completion,
                    });
                view.open_content_search_result(0, 0, window, cx);
            });
        });
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert_eq!(view.sessions.active().path(), Some(path.as_path()));
            assert_eq!(
                view.editor().selection(),
                Selection {
                    anchor: SourceOffset(start),
                    active: SourceOffset(end),
                }
            );
            assert!(view.content_search.pending_navigation.is_none());
        });
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn a_late_open_completion_does_not_override_the_newer_search_result_selection(
        cx: &mut gpui::TestAppContext,
    ) {
        use hane_session::{OsWorkFolderScanner, WorkFolderScanner};

        static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "hane-414-search-open-race-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let first = root.join("A.md");
        let second = root.join("B.md");
        let first_text = "先の文書 needle";
        let second_text = "後の文書🙂 needle";
        std::fs::write(&first, first_text).unwrap();
        std::fs::write(&second, second_text).unwrap();
        let folder = OsWorkFolderScanner.scan(&root).unwrap();
        let key = SearchKey {
            workspace_epoch: 0,
            query_epoch: 0,
        };
        let search_file = |path: &Path| {
            let mut engine = SearchEngine::new(SearchRequest::new(
                key,
                SearchQuery::new("needle", false).unwrap(),
                SearchCancellationToken::new(),
            ))
            .unwrap();
            match engine.search_file(&OsFileService, path) {
                SearchOutcome::File(result) => DisplayedSearchFile {
                    key: result.key,
                    target: result.target,
                    version: result.version,
                    identity: result.identity,
                    label: path.file_name().unwrap().to_string_lossy().into_owned(),
                    hits: result.hits,
                    completion: result.completion,
                },
                outcome => panic!("expected disk search result, got {outcome:?}"),
            }
        };
        let first_result = search_file(&first);
        let second_result = search_file(&second);
        let second_start = second_text.find("needle").unwrap();
        let second_end = second_start + "needle".len();
        let second_hit = second_result.hits[0].clone();
        let (view, cx) = cx.add_window_view(|_, cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("", "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.initialize_content_search_input(window, cx);
                view.work_folder = Some(folder);
                view.content_search.mode = SidebarMode::Content;
                view.content_search.query_text = "needle".to_owned();
                view.content_search.displayed_files = vec![first_result, second_result];
                // Model two quick selections. The second navigation replaces
                // the first before either load completion is applied.
                let first_hit = view.content_search.displayed_files[0].hits[0].clone();
                let first_navigation = PendingSearchNavigation {
                    id: 1,
                    key,
                    target: SearchTarget::File(first.clone()),
                    version: view.content_search.displayed_files[0].version,
                    identity: view.content_search.displayed_files[0].identity.clone(),
                    range: first_hit.range,
                    context_range: first_hit.context_range,
                    context_source: first_hit.context_source,
                };
                view.latest_open_target = Some(first.clone());
                view.content_search.pending_navigation = Some(first_navigation);
                view.content_search.authorized_navigation_id = Some(1);

                let second_navigation = PendingSearchNavigation {
                    id: 2,
                    key,
                    target: SearchTarget::File(second.clone()),
                    version: view.content_search.displayed_files[1].version,
                    identity: view.content_search.displayed_files[1].identity.clone(),
                    range: second_hit.range,
                    context_range: second_hit.context_range,
                    context_source: second_hit.context_source,
                };
                view.latest_open_target = Some(second.clone());
                view.content_search.pending_navigation = Some(second_navigation);
                view.content_search.authorized_navigation_id = Some(2);

                let generation = view.work_folder_generation;
                view.finish_open(None, generation, &second, OsFileService.load(&second), cx);
                // The first request completes after the user's newer choice.
                view.finish_open(None, generation, &first, OsFileService.load(&first), cx);
            });
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.sessions.active().path(), Some(second.as_path()));
            assert_eq!(
                view.editor().selection(),
                Selection {
                    anchor: SourceOffset(second_start),
                    active: SourceOffset(second_end),
                }
            );
            assert!(view.content_search.pending_navigation.is_none());
            assert!(view.sessions.session_for_path(&first).is_some());
        });
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn changed_disk_content_opens_without_reusing_the_old_search_position(
        cx: &mut gpui::TestAppContext,
    ) {
        use hane_session::search::{
            SearchCancellationToken, SearchEngine, SearchKey, SearchOutcome, SearchQuery,
            SearchRequest,
        };
        use hane_session::{OsWorkFolderScanner, WorkFolderScanner};

        static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "hane-414-stale-search-navigation-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("本文.md");
        let original = "日本語🙂 needle の元本文";
        std::fs::write(&path, original.as_bytes()).unwrap();
        let folder = OsWorkFolderScanner.scan(&root).unwrap();
        let request = SearchRequest::new(
            SearchKey {
                workspace_epoch: 0,
                query_epoch: 0,
            },
            SearchQuery::new("needle", false).unwrap(),
            SearchCancellationToken::new(),
        );
        let mut engine = SearchEngine::new(request).unwrap();
        let result = match engine.search_file(&OsFileService, &path) {
            SearchOutcome::File(result) => result,
            other => panic!("expected a disk search result, got {other:?}"),
        };

        // Change the file after search but before the normal open path reads it.
        std::fs::write(
            &path,
            "内容が置き換わりました。needleの場所は別です。".as_bytes(),
        )
        .unwrap();
        let (view, cx) = cx.add_window_view(|_, cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled("", "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.initialize_content_search_input(window, cx);
                view.work_folder = Some(folder);
                view.content_search.mode = SidebarMode::Content;
                view.content_search.query_text = "needle".to_owned();
                view.content_search
                    .displayed_files
                    .push(DisplayedSearchFile {
                        key: result.key,
                        target: result.target,
                        version: result.version,
                        identity: result.identity,
                        label: "本文.md".to_owned(),
                        hits: result.hits,
                        completion: result.completion,
                    });
                view.open_content_search_result(0, 0, window, cx);
            });
        });
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert_eq!(view.sessions.active().path(), Some(path.as_path()));
            assert_eq!(view.editor().selection(), Selection::caret(SourceOffset(0)));
            assert!(view.content_search.has_stale_results);
            assert!(view.content_search.pending_navigation.is_none());
        });
        std::fs::remove_dir_all(root).unwrap();
    }

    #[gpui::test]
    fn a_stale_buffer_revision_does_not_move_to_an_old_search_result(
        cx: &mut gpui::TestAppContext,
    ) {
        let source = "日本語🙂 needle の本文";
        let start = source.find("needle").unwrap();
        let end = start + "needle".len();
        let (view, cx) = cx.add_window_view(|_, cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled(source, "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });
        let (session, generation, revision) = view.read_with(cx, |view, _| {
            let session = view.sessions.active();
            (session.id(), session.generation(), session.revision())
        });

        cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.initialize_content_search_input(window, cx);
                view.work_folder_drafts.insert(
                    session,
                    WorkFolderDraft {
                        draft_id: hane_session::DraftId::generate(),
                        target_directory: PathBuf::from("/work"),
                    },
                );
                view.content_search.mode = SidebarMode::Content;
                view.content_search.query_text = "needle".to_owned();
                let key = view.content_search.key();
                view.content_search
                    .displayed_files
                    .push(DisplayedSearchFile {
                        key,
                        target: SearchTarget::Draft(session),
                        version: SearchVersion::Buffer {
                            session,
                            generation,
                            revision: Revision(revision.0.wrapping_add(1)),
                        },
                        identity: None,
                        label: "未保存のメモ".to_owned(),
                        hits: vec![hane_session::search::SearchHit {
                            range: SourceRange::new(start, end),
                            line_number: 1,
                            context_range: SourceRange::new(0, source.len()),
                            context_source: source.to_owned(),
                        }],
                        completion: SearchFileCompletion::Complete,
                    });

                view.open_content_search_result(0, 0, window, cx);
            });
        });
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().selection(), Selection::caret(SourceOffset(0)));
            assert!(view.content_search.has_stale_results);
        });
    }

    #[gpui::test]
    fn a_superseded_navigation_id_cannot_select_a_search_hit(cx: &mut gpui::TestAppContext) {
        let source = "needle";
        let (view, cx) = cx.add_window_view(|_, cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled(source, "Untitled"),
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            )
        });
        let (session, generation, revision) = view.read_with(cx, |view, _| {
            let session = view.sessions.active();
            (session.id(), session.generation(), session.revision())
        });
        view.update(cx, |view, cx| {
            view.content_search.query_text = "needle".to_owned();
            let navigation = PendingSearchNavigation {
                id: 8,
                key: view.content_search.key(),
                target: SearchTarget::Draft(session),
                version: SearchVersion::Buffer {
                    session,
                    generation,
                    revision,
                },
                identity: None,
                range: SourceRange::new(0, 6),
                context_range: SourceRange::new(0, 6),
                context_source: source.to_owned(),
            };
            view.content_search.pending_navigation = Some(navigation);
            assert!(!view.finish_pending_search_navigation(7, false, cx));
            assert_eq!(view.editor().selection(), Selection::caret(SourceOffset(0)));
        });
    }
}
