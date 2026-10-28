use crate::identity::{ContentFingerprint, ExternalChange, FilePresence, FileStamp, FileState};
use crate::resource::ResourceResolver;
use crate::service::{
    DiskVersion, FileService, LoadedFile, ObservedFile, OverwriteGuard, SaveFailure, SavedFile,
    observe_disk,
};
use hane_document::{Revision, RopeBuffer, SourceOffset, TextBuffer};
use hane_editor::{Editor, Selection};
use std::path::{Path, PathBuf};

/// Stable identifier for one open document. Survives renames and reloads, so a
/// filer or tab strip can address a session without holding a path.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SessionId(pub u64);

/// Per-session display state that must survive switching away and back. Layout
/// caches are not here: they are derived and belong to the renderer.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SessionViewState {
    pub scroll_y: f32,
}

/// What the caller wants written, and how much it is allowed to overwrite.
#[derive(Clone, Debug)]
pub enum SaveIntent {
    /// Save to the session's own file; refuses to clobber an external edit.
    Current,
    /// Save As: the user named the target, so replacing it is their decision.
    To(PathBuf),
    /// Retry after the user confirmed the overwrite of an external change.
    Overwrite,
    /// First write for a session with no file yet, at a path the caller
    /// picked without asking the user (an H1-derived filename). Unlike `To`,
    /// which follows a dialog that already confirmed an overwrite, this
    /// refuses if anything already exists at the target: the name was picked
    /// automatically, so a collision means the caller's candidate was
    /// already stale and must be recomputed, not written over.
    CreateNew(PathBuf),
    /// The user confirmed replacing the external version they were shown (or
    /// recreating a file that is gone). Valid only while the approval still
    /// describes the document and the external version; queued behind a
    /// running write it is re-validated when it runs.
    Confirmed(ResolutionApproval),
}

/// One accepted write, ready to hand to the I/O boundary.
pub struct SaveJob {
    pub path: PathBuf,
    pub document: RopeBuffer,
    pub guard: OverwriteGuard,
    pub ticket: SaveTicket,
}

impl std::fmt::Debug for SaveJob {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SaveJob")
            .field("path", &self.path)
            .field("guard", &self.guard)
            .field("ticket", &self.ticket)
            .finish_non_exhaustive()
    }
}

/// Identifies the write a result belongs to. A result whose ticket no longer
/// matches the session is dropped rather than applied, which is what keeps a
/// slow write from resurrecting state after the document was replaced.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SaveTicket {
    generation: u64,
    sequence: u64,
    revision: Revision,
}

#[derive(Debug)]
pub enum SaveDecision {
    Write(SaveJob),
    /// A write is already running; this target replaced any earlier queued one.
    Queued,
    /// Untitled document: the caller must ask for a path first.
    NeedsPath,
    /// A confirmed overwrite no longer matches the document or the external
    /// version (an edit, a newer external change, a reload happened since).
    /// Nothing was written; the caller asks the user again.
    ApprovalExpired,
}

#[derive(Debug)]
pub enum SaveOutcome {
    /// Written, and the document has not changed since the snapshot was taken.
    Saved,
    /// Written, but edits arrived while the write was in flight.
    SavedStale,
    /// The file changed on disk; nothing was written.
    Conflict,
    Failed(std::io::Error),
    /// The result belongs to a document this session no longer holds.
    Superseded,
}

/// Proof that an autosave timer belongs to the edit that armed it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AutosaveTicket {
    generation: u64,
    autosave_generation: u64,
    revision: Revision,
}

/// A filer- or watcher-originated change to a file on disk.
#[derive(Clone, Debug)]
pub enum FileEvent {
    Renamed {
        from: PathBuf,
        to: PathBuf,
    },
    Deleted(PathBuf),
    ChangedOnDisk {
        path: PathBuf,
        stamp: Option<FileStamp>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileEventOutcome {
    /// The event was about some other file.
    Ignored,
    /// The session now points at the new path; the document is untouched.
    Renamed,
    /// The backing file is gone. A dirty session keeps its content so a save can
    /// recreate the file.
    Missing { dirty: bool },
    /// Someone else edited the file and this session has nothing to lose, so the
    /// caller may reload it.
    ExternalEdit,
    /// Someone else edited the file and this session has unsaved edits. Never
    /// resolved automatically.
    Conflict,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CloseDecision {
    Close,
    Reject(UnsavedChanges),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnsavedChanges;

/// Binds one background check of the disk to the document it was started for.
/// A result is applied only while every field still describes the session, so
/// a slow read can never act on a replaced document, a renamed path, or a
/// newer request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalCheckTicket {
    generation: u64,
    sequence: u64,
    revision: Revision,
    /// Count of successful writes when the check started: a write that lands
    /// while the disk is being read makes the read stale.
    saves: u64,
    path: PathBuf,
}

impl ExternalCheckTicket {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// One accepted background check, ready for [`run_external_check`].
pub struct ExternalCheckRequest {
    pub ticket: ExternalCheckTicket,
    /// A snapshot of the clean document, present only when the session has no
    /// baseline fingerprint yet: the check fingerprints it in the background
    /// so the UI thread never hashes a document.
    local: Option<RopeBuffer>,
}

impl ExternalCheckRequest {
    pub fn path(&self) -> &Path {
        self.ticket.path()
    }
}

/// What the I/O side found. Applied with [`DocumentSession::finish_external_check`].
pub struct ExternalCheckResult {
    disk: DiskVersion,
    local: Option<ContentFingerprint>,
}

/// Reads the file once as a consistent version (bounded retries) and
/// fingerprints it. Blocking: call from a background executor.
pub fn run_external_check(
    service: &dyn FileService,
    request: &ExternalCheckRequest,
) -> ExternalCheckResult {
    ExternalCheckResult {
        disk: observe_disk(service, request.path()),
        local: request.local.as_ref().map(ContentFingerprint::of_document),
    }
}

/// Whether a check was started, or folded into the one already running.
pub enum ExternalCheckStart {
    Run(ExternalCheckRequest),
    /// A check is running; one more follows it so a burst of notifications
    /// costs one job plus one follow-up, never a queue.
    Coalesced,
    /// Untitled documents are not watched.
    NotTracked,
}

/// Result of applying a background check.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExternalCheckOutcome {
    /// The ticket no longer describes this session; nothing changed.
    Stale,
    /// The disk holds the version this session already has.
    Unchanged,
    /// The document had nothing to lose and was replaced with the disk
    /// version in a new document generation.
    Reloaded,
    /// The disk differs and the document was kept. `dirty` is a conflict with
    /// unsaved edits; otherwise the reload is deferred because input, an IME
    /// composition or a write was in progress.
    Changed { dirty: bool },
    /// The file is gone or moved. The content is kept.
    Missing { dirty: bool },
    /// The file could not be read as text or kept changing. Not "no change".
    Unverifiable { reason: String },
}

/// What this session currently knows about the file on disk, apart from
/// being dirty or saving.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExternalStatus {
    InSync,
    Changed,
    Missing,
    Unverifiable,
}

/// The external version a session has seen but not accepted.
struct ObservedExternal {
    fingerprint: ContentFingerprint,
    stamp: Option<FileStamp>,
    document: RopeBuffer,
}

enum ExternalState {
    InSync,
    Changed(ObservedExternal),
    Missing,
    Unverifiable(String),
}

/// Which destructive resolution the user confirmed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolutionKind {
    /// Replace the disk version with this document.
    Overwrite,
    /// Discard unsaved edits and take the disk version.
    DiscardAndReload,
}

/// A one-time confirmation bound to the document revision and the external
/// version the user was shown. It is consumed by the operation it approves
/// and dies with any edit, reload, or newer external version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolutionApproval {
    kind: ResolutionKind,
    generation: u64,
    revision: Revision,
    shown: Option<ContentFingerprint>,
    serial: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReloadOutcome {
    Reloaded,
    /// The approval no longer matches the document or the external version;
    /// the caller must ask again. Nothing was discarded.
    StaleApproval,
    /// An IME composition or a write is running; try again afterwards.
    Busy,
}

/// A confirmed reload waiting for the disk to be read again. The read happens
/// after the answer, so what is applied is what was read *then*, and only if it
/// is the version the user was shown.
pub struct ReloadVerifyRequest {
    approval: ResolutionApproval,
    generation: u64,
    revision: Revision,
    sequence: u64,
    path: PathBuf,
}

impl ReloadVerifyRequest {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// What the I/O side found for a [`ReloadVerifyRequest`].
pub struct ReloadVerifyResult {
    disk: DiskVersion,
}

/// Reads the file once as a consistent version. Blocking: call from a
/// background executor, never the UI thread.
pub fn run_reload_verification(
    service: &dyn FileService,
    request: &ReloadVerifyRequest,
) -> ReloadVerifyResult {
    ReloadVerifyResult {
        disk: observe_disk(service, request.path()),
    }
}

/// Result of applying a [`ReloadVerifyResult`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReloadVerifyOutcome {
    /// The disk still held the version the user was shown; it was taken.
    Reloaded,
    /// The request, document or approval no longer matches; nothing changed.
    Stale,
    /// An IME composition or a write is running; nothing changed.
    Busy,
    /// The disk now holds a different version. It is remembered as the new
    /// external version; the text, history and draft are kept and the user
    /// must confirm again.
    DiskChanged,
    /// The file is gone. The content is kept.
    Missing,
    /// The file could not be read consistently. The content is kept.
    Unverifiable { reason: String },
}

/// One open document: its editor, the file it came from, and everything about
/// persistence that is not I/O. Holds no window, no renderer, and no executor,
/// so every rule below is testable without a UI.
pub struct DocumentSession {
    id: SessionId,
    /// Bumped whenever the document is replaced. Background work started for an
    /// older document compares generations and drops its result.
    generation: u64,
    editor: Editor,
    file: FileState,
    saved_revision: Revision,
    autosave_generation: u64,
    in_flight: Option<SaveTicket>,
    /// The path the running write targets, which differs from the session's
    /// current file for a first save or a Save As until the write lands.
    in_flight_target: Option<PathBuf>,
    save_sequence: u64,
    pending_save: Option<SaveIntent>,
    view: SessionViewState,
    /// The title this session's current filename was derived from, when the
    /// filename is auto-managed from the document's H1 (issue #6). `None`
    /// for a session whose name is not (or is no longer) auto-managed: a
    /// file opened from disk, or one where the filename has drifted away
    /// from what auto-naming last set it to.
    auto_title: Option<String>,
    /// What the last accepted background check said about the disk.
    external: ExternalState,
    check_sequence: u64,
    /// Successful writes so far; see [`ExternalCheckTicket`].
    saves_completed: u64,
    /// Sequence of the one check allowed to run at a time.
    check_in_flight: Option<u64>,
    /// A check was requested while one ran; one follow-up is owed.
    recheck_pending: bool,
    approval_serial: u64,
    /// The only approval that may still be consumed.
    live_approval: Option<u64>,
    verify_sequence: u64,
    /// Sequence of the one reload verification whose answer is still wanted.
    verify_in_flight: Option<u64>,
}

impl DocumentSession {
    pub fn untitled(id: SessionId, text: &str, label: impl Into<String>) -> Self {
        Self::from_parts(id, Editor::new(text), FileState::untitled(label))
    }

    pub fn from_loaded(id: SessionId, loaded: LoadedFile) -> Self {
        let mut file = FileState::tracked(loaded.identity, loaded.stamp);
        file.set_baseline(loaded.fingerprint);
        Self::from_parts(id, Editor::from_document(loaded.document), file)
    }

    fn from_parts(id: SessionId, editor: Editor, file: FileState) -> Self {
        let saved_revision = editor.document().revision();
        Self {
            id,
            generation: 0,
            editor,
            file,
            saved_revision,
            autosave_generation: 0,
            in_flight: None,
            in_flight_target: None,
            save_sequence: 0,
            pending_save: None,
            view: SessionViewState::default(),
            auto_title: None,
            external: ExternalState::InSync,
            check_sequence: 0,
            saves_completed: 0,
            check_in_flight: None,
            recheck_pending: false,
            approval_serial: 0,
            live_approval: None,
            verify_sequence: 0,
            verify_in_flight: None,
        }
    }

    pub fn id(&self) -> SessionId {
        self.id
    }

    /// Identifies the document instance. Any state derived from the document —
    /// parse indexes, layout caches, background results — is only valid for the
    /// generation it was derived from.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn editor(&self) -> &Editor {
        &self.editor
    }

    pub fn editor_mut(&mut self) -> &mut Editor {
        &mut self.editor
    }

    pub fn file(&self) -> &FileState {
        &self.file
    }

    pub fn path(&self) -> Option<&Path> {
        self.file.path()
    }

    pub fn label(&self) -> String {
        self.file.label()
    }

    pub fn revision(&self) -> Revision {
        self.editor.document().revision()
    }

    pub fn saved_revision(&self) -> Revision {
        self.saved_revision
    }

    pub fn is_dirty(&self) -> bool {
        self.revision() != self.saved_revision
    }

    pub fn view_state(&self) -> SessionViewState {
        self.view
    }

    pub fn set_view_state(&mut self, view: SessionViewState) {
        self.view = view;
    }

    /// Resolves relative resources (images, includes) against the session's own
    /// file, never against the process working directory.
    pub fn resource_resolver(&self) -> ResourceResolver {
        ResourceResolver::for_directory(self.file.directory())
    }

    /// Replaces the document in place, as when opening a file into this session.
    /// Everything derived from the old document is invalidated by the generation
    /// bump; a write still in flight for the old document can no longer land.
    pub fn adopt(&mut self, loaded: LoadedFile) {
        self.editor = Editor::from_document(loaded.document);
        self.file = FileState::tracked(loaded.identity, loaded.stamp);
        self.file.set_baseline(loaded.fingerprint);
        self.saved_revision = self.editor.document().revision();
        self.generation = self.generation.wrapping_add(1);
        self.autosave_generation = 0;
        self.in_flight = None;
        self.in_flight_target = None;
        self.pending_save = None;
        self.view = SessionViewState::default();
        self.auto_title = None;
        self.reset_external_tracking();
    }

    /// Forgets everything learned about the previous document's disk file, so
    /// no check, notice or approval of the old document can reach the new one.
    fn reset_external_tracking(&mut self) {
        self.external = ExternalState::InSync;
        self.check_in_flight = None;
        self.recheck_pending = false;
        self.live_approval = None;
    }

    /// The title this session's current filename was derived from, if the
    /// filename is currently auto-managed.
    pub fn auto_title(&self) -> Option<&str> {
        self.auto_title.as_deref()
    }

    /// Records that the current filename was just derived from `title`,
    /// after a successful create-or-rename write for it. Called only once
    /// the write actually landed, so a failed or superseded write never
    /// marks a session auto-managed for a name it does not have.
    pub fn note_auto_named(&mut self, title: String) {
        self.auto_title = Some(title);
    }

    /// Stops auto-managing this session's filename: the current name has
    /// drifted away from what auto-naming last derived it from (an external
    /// rename, or the user renaming it another way), so further H1 edits
    /// must not resume renaming it.
    pub fn stop_auto_naming(&mut self) {
        self.auto_title = None;
    }

    /// Records that the document was edited. Arms a fresh autosave window and
    /// invalidates any autosave timer from an earlier keystroke.
    pub fn note_edit(&mut self) {
        self.autosave_generation = self.autosave_generation.wrapping_add(1);
    }

    /// A ticket when an autosave is worth arming: enabled, backed by a file, and
    /// with something to write.
    pub fn autosave_ticket(&self, enabled: bool) -> Option<AutosaveTicket> {
        (enabled && self.file.path().is_some() && self.is_dirty()).then_some(AutosaveTicket {
            generation: self.generation,
            autosave_generation: self.autosave_generation,
            revision: self.revision(),
        })
    }

    /// True when the armed timer still describes the current document: same
    /// document instance, no newer keystroke, same revision, still enabled.
    pub fn autosave_is_current(&self, ticket: AutosaveTicket, enabled: bool) -> bool {
        enabled
            && self.file.path().is_some()
            && self.generation == ticket.generation
            && self.autosave_generation == ticket.autosave_generation
            && self.revision() == ticket.revision
    }

    pub fn save_in_flight(&self) -> bool {
        self.in_flight.is_some()
    }

    /// Whether an H1-derived "create the note's first file" write should be
    /// skipped this cycle rather than attempted or queued: the session
    /// already has a file (through this or some other route), or a write —
    /// including a manual Save As that has not landed yet, which is why
    /// `path()` alone is not enough — already holds the save slot.
    ///
    /// Queuing behind that other write instead of skipping would risk two
    /// failure modes once it lands: its own completion could be mistaken for
    /// this H1 write landing (marking a file the user named some other way
    /// as auto-managed), and this write would still run afterwards, silently
    /// moving the session onto a second, unwanted file. Skipping is safe
    /// either way: the caller re-decides fresh once that other write is done.
    #[must_use]
    pub fn should_defer_h1_create(&self) -> bool {
        self.file.path().is_some() || self.save_in_flight()
    }

    /// Decides what a save request means right now. At most one write runs at a
    /// time; a request that arrives during a write replaces the queued target,
    /// so a burst of autosaves collapses to one follow-up write.
    pub fn request_save(&mut self, intent: SaveIntent) -> SaveDecision {
        if let SaveIntent::Confirmed(approval) = intent {
            if self.in_flight.is_some() {
                self.pending_save = Some(SaveIntent::Confirmed(approval));
                return SaveDecision::Queued;
            }
            return match self.request_confirmed_overwrite(approval) {
                Some(job) => SaveDecision::Write(job),
                None => SaveDecision::ApprovalExpired,
            };
        }
        let (path, guard) = match &intent {
            SaveIntent::Current | SaveIntent::Overwrite => {
                let Some(path) = self.file.path().map(Path::to_path_buf) else {
                    return SaveDecision::NeedsPath;
                };
                let guard = if matches!(intent, SaveIntent::Overwrite) {
                    OverwriteGuard::Force
                } else {
                    self.content_guard()
                };
                (path, guard)
            }
            SaveIntent::To(path) => {
                let same_file = self
                    .file
                    .identity()
                    .is_some_and(|identity| identity.is_same_file_on_disk(path));
                let guard = if same_file {
                    self.content_guard()
                } else {
                    // The user named this target in a dialog that already asked
                    // about replacing it.
                    OverwriteGuard::Force
                };
                (path.clone(), guard)
            }
            SaveIntent::CreateNew(path) => (path.clone(), OverwriteGuard::ExpectStamp(None)),
            SaveIntent::Confirmed(_) => unreachable!("handled before the match"),
        };
        if self.in_flight.is_some() {
            self.pending_save = Some(intent);
            return SaveDecision::Queued;
        }
        SaveDecision::Write(self.start_write(path, guard))
    }

    /// Takes the save slot and snapshots the document for one write.
    fn start_write(&mut self, path: PathBuf, guard: OverwriteGuard) -> SaveJob {
        self.save_sequence = self.save_sequence.wrapping_add(1);
        let ticket = SaveTicket {
            generation: self.generation,
            sequence: self.save_sequence,
            revision: self.revision(),
        };
        self.in_flight = Some(ticket);
        self.in_flight_target = Some(path.clone());
        SaveJob {
            path,
            document: self.editor.document().clone(),
            guard,
            ticket,
        }
    }

    /// The guard of every ordinary write to this session's own file: the disk
    /// must still hold the baseline content. A pending or repeated ordinary
    /// save is never promoted to a forced write.
    fn content_guard(&self) -> OverwriteGuard {
        OverwriteGuard::ExpectContent {
            stamp: self.file.stamp(),
            baseline: self.file.baseline(),
        }
    }

    /// Applies the outcome of a write. A result whose ticket is not the one this
    /// session is waiting for changes nothing.
    pub fn finish_save(
        &mut self,
        ticket: SaveTicket,
        result: Result<SavedFile, SaveFailure>,
    ) -> SaveOutcome {
        if self.in_flight != Some(ticket) {
            return SaveOutcome::Superseded;
        }
        self.in_flight = None;
        self.in_flight_target = None;
        match result {
            Ok(saved) => {
                self.file.set_identity(saved.identity, saved.stamp);
                // The baseline follows the bytes that reached the disk, and
                // only now: a refused or failed write never moves it.
                self.file.set_baseline(saved.fingerprint);
                self.saves_completed = self.saves_completed.wrapping_add(1);
                if self.external_status() != ExternalStatus::InSync {
                    self.external = ExternalState::InSync;
                    self.live_approval = None;
                }
                if self.revision() == ticket.revision {
                    self.saved_revision = ticket.revision;
                    SaveOutcome::Saved
                } else {
                    // The snapshot is on disk but the document moved on; the
                    // caller re-arms so the newer bytes follow.
                    SaveOutcome::SavedStale
                }
            }
            Err(SaveFailure::ExternalChange) => SaveOutcome::Conflict,
            Err(SaveFailure::Io(error)) => SaveOutcome::Failed(error),
        }
    }

    pub fn take_pending_save(&mut self) -> Option<SaveIntent> {
        self.pending_save.take()
    }

    /// Reserves the save slot for a rename: not itself a write, but one that
    /// must not run at the same time as one, or a concurrent autosave can
    /// write new content to the path being renamed away from and resurrect
    /// it after the rename has already moved the file. `None` means a write
    /// is already in flight; the caller skips renaming this cycle and tries
    /// again on the next debounce instead of racing it.
    pub fn begin_rename(&mut self) -> Option<SaveTicket> {
        if self.in_flight.is_some() {
            return None;
        }
        self.save_sequence = self.save_sequence.wrapping_add(1);
        let ticket = SaveTicket {
            generation: self.generation,
            sequence: self.save_sequence,
            revision: self.revision(),
        };
        self.in_flight = Some(ticket);
        Some(ticket)
    }

    /// Releases the slot reserved by `begin_rename`, once the rename has
    /// landed (or failed). A save queued while the rename held the slot is
    /// left in `pending_save` for the caller to drain with
    /// `take_pending_save`, the same as after any other write.
    pub fn finish_rename(&mut self, ticket: SaveTicket) {
        if self.in_flight == Some(ticket) {
            self.in_flight = None;
            self.in_flight_target = None;
        }
    }

    /// Compares the session's recorded stamp with what the disk reports now.
    pub fn external_change(&self, current: Option<FileStamp>) -> ExternalChange {
        self.file.compare(current)
    }

    /// Applies a filer or watcher event. Never reloads and never discards
    /// unsaved edits by itself: it reports what the caller has to decide.
    pub fn apply_file_event(&mut self, event: &FileEvent) -> FileEventOutcome {
        let path = match event {
            FileEvent::Renamed { from, .. } => from,
            FileEvent::Deleted(path) | FileEvent::ChangedOnDisk { path, .. } => path,
        };
        let Some(identity) = self.file.identity() else {
            return FileEventOutcome::Ignored;
        };
        if !identity.matches_path(path) {
            return FileEventOutcome::Ignored;
        }
        match event {
            FileEvent::Renamed { to, .. } => {
                let moved = identity.moved_to(to);
                let stamp = self.file.stamp();
                self.file.set_identity(moved, stamp);
                FileEventOutcome::Renamed
            }
            FileEvent::Deleted(_) => {
                self.file.mark_missing();
                FileEventOutcome::Missing {
                    dirty: self.is_dirty(),
                }
            }
            FileEvent::ChangedOnDisk { stamp, .. } => match self.file.compare(*stamp) {
                ExternalChange::Unchanged => FileEventOutcome::Ignored,
                ExternalChange::Deleted => {
                    self.file.mark_missing();
                    FileEventOutcome::Missing {
                        dirty: self.is_dirty(),
                    }
                }
                ExternalChange::Modified | ExternalChange::Unknown => {
                    if self.is_dirty() {
                        FileEventOutcome::Conflict
                    } else {
                        // The recorded stamp is deliberately left alone: a
                        // notification only says "look again". The stamp and
                        // baseline move when a background check has read and
                        // accepted the new version (`finish_external_check`).
                        FileEventOutcome::ExternalEdit
                    }
                }
            },
        }
    }

    /// Whether this session's file lives at `folder` or somewhere under it.
    pub fn is_under_folder(&self, folder: &Path) -> bool {
        self.file
            .identity()
            .is_some_and(|identity| identity.path().starts_with(folder))
    }

    /// Whether a write that is running right now targets `folder` or somewhere
    /// under it, including a first save or Save As whose destination the
    /// session's current file path does not show yet.
    pub fn writes_under_folder(&self, folder: &Path) -> bool {
        if self
            .in_flight_target
            .as_deref()
            .is_some_and(|target| target.starts_with(folder))
        {
            return true;
        }
        // A save queued behind the running write runs next and would recreate
        // the path after the folder is gone, so its destination counts too.
        match self.pending_save.as_ref() {
            Some(SaveIntent::To(path) | SaveIntent::CreateNew(path)) => path.starts_with(folder),
            Some(SaveIntent::Current | SaveIntent::Overwrite | SaveIntent::Confirmed(_)) => {
                self.is_under_folder(folder)
            }
            None => false,
        }
    }

    /// Follows a folder this app itself just moved to the Trash: a session
    /// whose file lived under `folder` keeps its content and is marked missing
    /// from disk, the same way a single-file `FileEvent::Deleted` would.
    pub fn apply_folder_delete(&mut self, folder: &Path) -> FileEventOutcome {
        if !self.is_under_folder(folder) {
            return FileEventOutcome::Ignored;
        }
        self.file.mark_missing();
        FileEventOutcome::Missing {
            dirty: self.is_dirty(),
        }
    }

    /// Follows a folder rename this app itself just performed: if this
    /// session's file lives at `from` or somewhere under it, its path moves
    /// to the same position under `to`, the same way a single-file
    /// `FileEvent::Renamed` would, but for every path a folder rename carries
    /// with it instead of just the one path an exact match would require.
    /// A session outside `from` is untouched.
    pub fn apply_folder_rename(&mut self, from: &Path, to: &Path) -> FileEventOutcome {
        let Some(identity) = self.file.identity() else {
            return FileEventOutcome::Ignored;
        };
        let Ok(relative) = identity.path().strip_prefix(from) else {
            return FileEventOutcome::Ignored;
        };
        let new_path = if relative.as_os_str().is_empty() {
            to.to_path_buf()
        } else {
            to.join(relative)
        };
        let moved = identity.moved_to(new_path);
        let stamp = self.file.stamp();
        self.file.set_identity(moved, stamp);
        FileEventOutcome::Renamed
    }

    pub fn presence(&self) -> FilePresence {
        self.file.presence()
    }

    /// What the last accepted check (or deletion event) says about the disk.
    pub fn external_status(&self) -> ExternalStatus {
        match &self.external {
            ExternalState::Changed(_) => ExternalStatus::Changed,
            ExternalState::Missing => ExternalStatus::Missing,
            ExternalState::Unverifiable(_) => ExternalStatus::Unverifiable,
            ExternalState::InSync => {
                if self.file.presence() == FilePresence::Missing {
                    ExternalStatus::Missing
                } else {
                    ExternalStatus::InSync
                }
            }
        }
    }

    /// Why the disk version could not be verified, for the notice text.
    pub fn external_failure(&self) -> Option<&str> {
        match &self.external {
            ExternalState::Unverifiable(reason) => Some(reason.as_str()),
            _ => None,
        }
    }

    /// The disk version seen but not accepted, as a read-only document for a
    /// side-by-side comparison. Never the editing buffer.
    pub fn observed_document(&self) -> Option<&RopeBuffer> {
        match &self.external {
            ExternalState::Changed(observed) => Some(&observed.document),
            _ => None,
        }
    }

    /// Identifies the observed external version, so a comparison view can tell
    /// when it is showing something older than the session now knows.
    pub fn observed_fingerprint(&self) -> Option<ContentFingerprint> {
        match &self.external {
            ExternalState::Changed(observed) => Some(observed.fingerprint),
            _ => None,
        }
    }

    /// An unedited document whose reload was held back (input, IME or a write
    /// was in progress) and should be checked again once that is over.
    pub fn needs_external_recheck(&self) -> bool {
        matches!(self.external, ExternalState::Changed(_)) && !self.is_dirty()
    }

    /// Whether another check was requested while one ran. Taking it clears it.
    pub fn take_recheck_request(&mut self) -> bool {
        std::mem::take(&mut self.recheck_pending)
    }

    /// Starts a background check of the disk for this document. At most one
    /// runs per document; further requests collapse into one follow-up.
    pub fn begin_external_check(&mut self) -> ExternalCheckStart {
        let Some(path) = self.file.path().map(Path::to_path_buf) else {
            return ExternalCheckStart::NotTracked;
        };
        if self.check_in_flight.is_some() {
            self.recheck_pending = true;
            return ExternalCheckStart::Coalesced;
        }
        self.check_sequence = self.check_sequence.wrapping_add(1);
        self.check_in_flight = Some(self.check_sequence);
        // Without a baseline the clean document itself stands for it.
        let local = (self.file.baseline().is_none() && !self.is_dirty())
            .then(|| self.editor.document().clone());
        ExternalCheckStart::Run(ExternalCheckRequest {
            ticket: ExternalCheckTicket {
                generation: self.generation,
                sequence: self.check_sequence,
                revision: self.revision(),
                saves: self.saves_completed,
                path,
            },
            local,
        })
    }

    /// Releases a check that will never report (its job was dropped), so the
    /// next request can start one.
    pub fn abandon_external_check(&mut self, ticket: &ExternalCheckTicket) {
        if self.check_in_flight == Some(ticket.sequence) && self.generation == ticket.generation {
            self.check_in_flight = None;
        }
    }

    /// Applies the result of a background check. Nothing is changed unless the
    /// ticket still describes this exact document, path and request. A disk
    /// version is accepted (baseline and stamp advance, the document is
    /// replaced) only when the document has nothing to lose *now*; otherwise
    /// the version is only remembered.
    pub fn finish_external_check(
        &mut self,
        ticket: &ExternalCheckTicket,
        result: ExternalCheckResult,
    ) -> ExternalCheckOutcome {
        if self.check_in_flight != Some(ticket.sequence) {
            return ExternalCheckOutcome::Stale;
        }
        // The slot is released even when the result is dropped below, or a
        // renamed or replaced document would never be checked again.
        self.check_in_flight = None;
        if self.generation != ticket.generation || self.file.path() != Some(ticket.path.as_path())
        {
            return ExternalCheckOutcome::Stale;
        }
        if self.saves_completed != ticket.saves {
            // A write landed while the disk was read, so the read may predate
            // it. Drop it and owe a fresh check against the saved baseline.
            self.recheck_pending = true;
            return ExternalCheckOutcome::Stale;
        }
        match result.disk {
            DiskVersion::Missing => {
                self.set_external(ExternalState::Missing);
                self.file.mark_missing();
                ExternalCheckOutcome::Missing {
                    dirty: self.is_dirty(),
                }
            }
            DiskVersion::Unreadable(reason) => {
                self.set_external(ExternalState::Unverifiable(reason.clone()));
                ExternalCheckOutcome::Unverifiable { reason }
            }
            DiskVersion::Present(observed) => self.apply_observed(ticket, *observed, result.local),
        }
    }

    fn apply_observed(
        &mut self,
        ticket: &ExternalCheckTicket,
        observed: ObservedFile,
        local: Option<ContentFingerprint>,
    ) -> ExternalCheckOutcome {
        if self.file.baseline().is_none() {
            // The clean document the check started from is the baseline, but
            // only if no write has advanced it since (a write sets it).
            if let Some(local) = local {
                self.file.set_baseline(Some(local));
            }
        }
        let same = match self.file.baseline() {
            Some(baseline) => baseline == observed.fingerprint,
            // Still unknown (edited before the first check): the stamp is the
            // only evidence there is.
            None => observed.stamp.is_some() && observed.stamp == self.file.stamp(),
        };
        if same {
            self.file.set_stamp(observed.stamp);
            self.file.set_baseline(Some(observed.fingerprint));
            self.set_external(ExternalState::InSync);
            return ExternalCheckOutcome::Unchanged;
        }
        let dirty = self.is_dirty();
        let can_replace = !dirty
            && self.revision() == ticket.revision
            && self.editor.ime().is_none()
            && self.in_flight.is_none();
        if can_replace {
            self.replace_with_disk(observed);
            return ExternalCheckOutcome::Reloaded;
        }
        self.file.mark_present();
        self.set_external(ExternalState::Changed(ObservedExternal {
            fingerprint: observed.fingerprint,
            stamp: observed.stamp,
            document: observed.document,
        }));
        ExternalCheckOutcome::Changed { dirty }
    }

    /// Stores a new external state. An approval given for an earlier external
    /// version dies when the version changes.
    fn set_external(&mut self, state: ExternalState) {
        let same_version = match (&self.external, &state) {
            (ExternalState::Changed(old), ExternalState::Changed(new)) => {
                old.fingerprint == new.fingerprint
            }
            (ExternalState::Missing, ExternalState::Missing)
            | (ExternalState::InSync, ExternalState::InSync) => true,
            _ => false,
        };
        if !same_version {
            self.live_approval = None;
        }
        self.external = state;
    }

    /// Replaces the document with an accepted disk version. Starts a new
    /// document generation: history, parse/layout caches and background
    /// results of the old text are invalidated by the bump, and the old
    /// undo/redo is not replayed on the new text. The selection is carried
    /// over by offset, rounded to a valid character boundary.
    fn replace_with_disk(&mut self, observed: ObservedFile) {
        let before = self.editor.selection();
        self.editor = Editor::from_document(observed.document);
        let anchor = self.nearest_valid_offset(before.anchor);
        let active = self.nearest_valid_offset(before.active);
        // Offsets were just validated, so this cannot fail; a failure would
        // only leave the caret at the start, never invalidate the document.
        let _ = self.editor.set_selection(Selection { anchor, active });
        self.file.set_stamp(observed.stamp);
        self.file.set_baseline(Some(observed.fingerprint));
        self.saved_revision = self.editor.document().revision();
        self.generation = self.generation.wrapping_add(1);
        self.autosave_generation = self.autosave_generation.wrapping_add(1);
        self.pending_save = None;
        self.live_approval = None;
        self.external = ExternalState::InSync;
    }

    /// The closest offset at or before `offset` that is inside the document
    /// and on a character boundary.
    fn nearest_valid_offset(&self, offset: SourceOffset) -> SourceOffset {
        let document = self.editor.document();
        let mut candidate = offset.0.min(document.len_bytes().0);
        while candidate > 0 && document.validate_offset(SourceOffset(candidate)).is_err() {
            candidate -= 1;
        }
        SourceOffset(candidate)
    }

    /// Records that the user confirmed `kind` while looking at `shown` (the
    /// fingerprint of the external version on screen, `None` for "the file is
    /// gone"). `None` back means that is not what the session knows any more:
    /// ask again. The approval is bound to the current revision and document.
    pub fn approve_resolution(
        &mut self,
        kind: ResolutionKind,
        shown: Option<ContentFingerprint>,
    ) -> Option<ResolutionApproval> {
        if self.current_external_fingerprint()? != shown {
            return None;
        }
        self.approval_serial = self.approval_serial.wrapping_add(1);
        self.live_approval = Some(self.approval_serial);
        Some(ResolutionApproval {
            kind,
            generation: self.generation,
            revision: self.revision(),
            shown,
            serial: self.approval_serial,
        })
    }

    /// `Some(Some(fp))` for an observed external version, `Some(None)` for a
    /// missing file, `None` when there is nothing to resolve.
    fn current_external_fingerprint(&self) -> Option<Option<ContentFingerprint>> {
        match &self.external {
            ExternalState::Changed(observed) => Some(Some(observed.fingerprint)),
            ExternalState::Missing => Some(None),
            ExternalState::InSync => (self.file.presence() == FilePresence::Missing).then_some(None),
            ExternalState::Unverifiable(_) => None,
        }
    }

    fn approval_is_current(&self, approval: ResolutionApproval, kind: ResolutionKind) -> bool {
        approval.kind == kind
            && self.live_approval == Some(approval.serial)
            && approval.generation == self.generation
            && approval.revision == self.revision()
            && self.current_external_fingerprint() == Some(approval.shown)
    }

    /// Starts the confirmed overwrite of the external version the user saw.
    /// The write is guarded by that exact version, so a newer external change
    /// that lands before the bytes are replaced is refused again instead of
    /// overwritten. `None`: the approval is stale or the save slot is busy;
    /// nothing was consumed in the busy case.
    pub fn request_confirmed_overwrite(&mut self, approval: ResolutionApproval) -> Option<SaveJob> {
        if self.in_flight.is_some() {
            return None;
        }
        if !self.approval_is_current(approval, ResolutionKind::Overwrite) {
            return None;
        }
        let path = self.file.path().map(Path::to_path_buf)?;
        let guard = match &self.external {
            ExternalState::Changed(observed) => OverwriteGuard::ExpectContent {
                stamp: observed.stamp,
                baseline: Some(observed.fingerprint),
            },
            // Recreating is allowed only while the file is still gone.
            _ => OverwriteGuard::ExpectContent {
                stamp: None,
                baseline: None,
            },
        };
        // One use: a second attempt needs a new confirmation.
        self.live_approval = None;
        Some(self.start_write(path, guard))
    }

    /// Discards unsaved edits and takes the external version the user saw.
    /// The text comes from the already-read version, so a failed or cancelled
    /// step never leaves the document emptied.
    pub fn apply_confirmed_reload(&mut self, approval: ResolutionApproval) -> ReloadOutcome {
        if self.editor.ime().is_some() || self.in_flight.is_some() {
            return ReloadOutcome::Busy;
        }
        if !self.approval_is_current(approval, ResolutionKind::DiscardAndReload) {
            return ReloadOutcome::StaleApproval;
        }
        // Missing / unreadable: there is no disk text to take.
        if !matches!(self.external, ExternalState::Changed(_)) {
            return ReloadOutcome::StaleApproval;
        }
        let ExternalState::Changed(observed) =
            std::mem::replace(&mut self.external, ExternalState::InSync)
        else {
            return ReloadOutcome::StaleApproval;
        };
        self.replace_with_disk(ObservedFile {
            document: observed.document,
            fingerprint: observed.fingerprint,
            stamp: observed.stamp,
        });
        ReloadOutcome::Reloaded
    }

    /// First half of a confirmed reload: checks the approval against the
    /// session as it is now and returns the read to run in the background.
    /// Nothing is discarded or consumed here; a newer request supersedes an
    /// older one.
    pub fn begin_confirmed_reload(
        &mut self,
        approval: ResolutionApproval,
    ) -> Result<ReloadVerifyRequest, ReloadOutcome> {
        if self.editor.ime().is_some() || self.in_flight.is_some() {
            return Err(ReloadOutcome::Busy);
        }
        if !self.approval_is_current(approval, ResolutionKind::DiscardAndReload)
            || !matches!(self.external, ExternalState::Changed(_))
        {
            return Err(ReloadOutcome::StaleApproval);
        }
        let Some(path) = self.file.path().map(Path::to_path_buf) else {
            return Err(ReloadOutcome::StaleApproval);
        };
        self.verify_sequence = self.verify_sequence.wrapping_add(1);
        self.verify_in_flight = Some(self.verify_sequence);
        Ok(ReloadVerifyRequest {
            approval,
            generation: self.generation,
            revision: self.revision(),
            sequence: self.verify_sequence,
            path,
        })
    }

    /// Second half: the text is taken only if the file read after the answer
    /// is the version the user was shown. Otherwise the text, history and
    /// draft stay as they are, the newly read state replaces the remembered
    /// external state (which kills the old approval), and the user is asked
    /// again.
    pub fn finish_confirmed_reload(
        &mut self,
        request: &ReloadVerifyRequest,
        result: ReloadVerifyResult,
    ) -> ReloadVerifyOutcome {
        if self.verify_in_flight != Some(request.sequence)
            || self.generation != request.generation
            || self.file.path() != Some(request.path.as_path())
            || self.revision() != request.revision
        {
            return ReloadVerifyOutcome::Stale;
        }
        self.verify_in_flight = None;
        if self.editor.ime().is_some() || self.in_flight.is_some() {
            return ReloadVerifyOutcome::Busy;
        }
        match result.disk {
            DiskVersion::Present(observed)
                if self.approval_is_current(request.approval, ResolutionKind::DiscardAndReload)
                    && request.approval.shown == Some(observed.fingerprint) =>
            {
                self.replace_with_disk(*observed);
                ReloadVerifyOutcome::Reloaded
            }
            DiskVersion::Present(observed) => {
                let observed = *observed;
                self.file.mark_present();
                self.set_external(ExternalState::Changed(ObservedExternal {
                    fingerprint: observed.fingerprint,
                    stamp: observed.stamp,
                    document: observed.document,
                }));
                ReloadVerifyOutcome::DiskChanged
            }
            DiskVersion::Missing => {
                self.set_external(ExternalState::Missing);
                self.file.mark_missing();
                ReloadVerifyOutcome::Missing
            }
            DiskVersion::Unreadable(reason) => {
                self.set_external(ExternalState::Unverifiable(reason.clone()));
                ReloadVerifyOutcome::Unverifiable { reason }
            }
        }
    }

    pub fn close_decision(&self) -> CloseDecision {
        if self.is_dirty() {
            CloseDecision::Reject(UnsavedChanges)
        } else {
            CloseDecision::Close
        }
    }
}

/// How an open request should be routed when the file is not already open.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenPolicy {
    /// Current single-window behaviour: load into the active session, which is
    /// only allowed when it has nothing to lose.
    ReuseActive,
    /// Load into a session of its own.
    NewSession,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenDecision {
    /// Read the file and hand the result to `SessionSet::apply_open`.
    Load {
        into: Option<SessionId>,
    },
    /// Already open: just switch to it. A dirty session is never reloaded from
    /// disk behind the user's back.
    Activate(SessionId),
    Reject(UnsavedChanges),
}

/// Every open document, plus which one is active. One session is the current
/// behaviour; the API is the same for many.
pub struct SessionSet {
    sessions: Vec<DocumentSession>,
    active: SessionId,
    next_id: u64,
}

impl SessionSet {
    pub fn with_untitled(text: &str, label: impl Into<String>) -> Self {
        let id = SessionId(0);
        Self {
            sessions: vec![DocumentSession::untitled(id, text, label)],
            active: id,
            next_id: 1,
        }
    }

    pub fn with_loaded(loaded: LoadedFile) -> Self {
        let id = SessionId(0);
        Self {
            sessions: vec![DocumentSession::from_loaded(id, loaded)],
            active: id,
            next_id: 1,
        }
    }

    pub fn active_id(&self) -> SessionId {
        self.active
    }

    pub fn active(&self) -> &DocumentSession {
        self.get(self.active).expect("active session exists")
    }

    pub fn active_mut(&mut self) -> &mut DocumentSession {
        let active = self.active;
        self.get_mut(active).expect("active session exists")
    }

    pub fn get(&self, id: SessionId) -> Option<&DocumentSession> {
        self.sessions.iter().find(|session| session.id() == id)
    }

    pub fn get_mut(&mut self, id: SessionId) -> Option<&mut DocumentSession> {
        self.sessions.iter_mut().find(|session| session.id() == id)
    }

    pub fn sessions(&self) -> impl Iterator<Item = &DocumentSession> {
        self.sessions.iter()
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    pub fn activate(&mut self, id: SessionId) -> bool {
        if self.get(id).is_some() {
            self.active = id;
            true
        } else {
            false
        }
    }

    /// The session already showing `path`, if any. Same-file, not same-spelling.
    pub fn session_for_path(&self, path: &Path) -> Option<SessionId> {
        self.sessions
            .iter()
            .find(|session| {
                session
                    .file()
                    .identity()
                    .is_some_and(|identity| identity.matches_path(path))
            })
            .map(DocumentSession::id)
    }

    pub fn open_decision(&self, path: &Path, policy: OpenPolicy) -> OpenDecision {
        if let Some(id) = self.session_for_path(path) {
            return OpenDecision::Activate(id);
        }
        match policy {
            OpenPolicy::NewSession => OpenDecision::Load { into: None },
            OpenPolicy::ReuseActive => {
                if self.active().is_dirty() {
                    OpenDecision::Reject(UnsavedChanges)
                } else {
                    OpenDecision::Load {
                        into: Some(self.active),
                    }
                }
            }
        }
    }

    /// Installs a loaded file, either into an existing session or a new one, and
    /// makes it active.
    pub fn apply_open(&mut self, into: Option<SessionId>, loaded: LoadedFile) -> SessionId {
        match into.and_then(|id| self.get_mut(id)) {
            Some(session) => {
                session.adopt(loaded);
                let id = session.id();
                self.active = id;
                id
            }
            None => self.push(|id| DocumentSession::from_loaded(id, loaded)),
        }
    }

    pub fn open_untitled(&mut self, text: &str, label: impl Into<String>) -> SessionId {
        let label = label.into();
        self.push(|id| DocumentSession::untitled(id, text, label))
    }

    fn push(&mut self, build: impl FnOnce(SessionId) -> DocumentSession) -> SessionId {
        let id = SessionId(self.next_id);
        self.next_id = self.next_id.wrapping_add(1);
        self.sessions.push(build(id));
        self.active = id;
        id
    }

    /// Closes a session. The last session is never removed: it is the surface
    /// the window renders, so it is replaced by an empty untitled document.
    pub fn close(&mut self, id: SessionId, label: impl Into<String>) -> CloseDecision {
        let Some(session) = self.get(id) else {
            return CloseDecision::Close;
        };
        if let CloseDecision::Reject(unsaved) = session.close_decision() {
            return CloseDecision::Reject(unsaved);
        }
        if self.sessions.len() == 1 {
            let replacement = SessionId(self.next_id);
            self.next_id = self.next_id.wrapping_add(1);
            self.sessions = vec![DocumentSession::untitled(replacement, "", label)];
            self.active = replacement;
            return CloseDecision::Close;
        }
        self.sessions.retain(|session| session.id() != id);
        if self.active == id {
            self.active = self.sessions[0].id();
        }
        CloseDecision::Close
    }

    /// Routes a filer event to every session that is looking at the file.
    pub fn apply_file_event(&mut self, event: &FileEvent) -> Vec<(SessionId, FileEventOutcome)> {
        self.sessions
            .iter_mut()
            .filter_map(|session| {
                let outcome = session.apply_file_event(event);
                (outcome != FileEventOutcome::Ignored).then_some((session.id(), outcome))
            })
            .collect()
    }

    /// Whether any session whose file lives under `folder` has unsaved changes
    /// or a write in flight, i.e. moving the folder away would put it at risk.
    pub fn has_unsaved_under(&self, folder: &Path) -> bool {
        self.sessions.iter().any(|session| {
            session.writes_under_folder(folder)
                || (session.is_under_folder(folder)
                    && (session.is_dirty() || session.save_in_flight()))
        })
    }

    /// Routes a folder deletion to every session whose file lives under
    /// `folder`: each keeps its content and is marked missing from disk.
    pub fn delete_folder(&mut self, folder: &Path) -> Vec<(SessionId, FileEventOutcome)> {
        self.sessions
            .iter_mut()
            .filter_map(|session| {
                let outcome = session.apply_folder_delete(folder);
                (outcome != FileEventOutcome::Ignored).then_some((session.id(), outcome))
            })
            .collect()
    }

    /// Routes a folder rename to every session whose file lives under `from`,
    /// leaving every other open session untouched. Complements
    /// `apply_file_event`'s single-file `FileEvent::Renamed`, which this does
    /// not change the meaning of.
    pub fn rename_folder(&mut self, from: &Path, to: &Path) -> Vec<(SessionId, FileEventOutcome)> {
        self.sessions
            .iter_mut()
            .filter_map(|session| {
                let outcome = session.apply_folder_rename(from, to);
                (outcome != FileEventOutcome::Ignored).then_some((session.id(), outcome))
            })
            .collect()
    }
}
