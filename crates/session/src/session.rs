use crate::identity::{ExternalChange, FilePresence, FileStamp, FileState};
use crate::resource::ResourceResolver;
use crate::service::{LoadedFile, OverwriteGuard, SaveFailure, SavedFile};
use hane_document::{Revision, RopeBuffer, TextBuffer};
use hane_editor::Editor;
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
    save_sequence: u64,
    pending_save: Option<SaveIntent>,
    view: SessionViewState,
    /// The title this session's current filename was derived from, when the
    /// filename is auto-managed from the document's H1 (issue #6). `None`
    /// for a session whose name is not (or is no longer) auto-managed: a
    /// file opened from disk, or one where the filename has drifted away
    /// from what auto-naming last set it to.
    auto_title: Option<String>,
}

impl DocumentSession {
    pub fn untitled(id: SessionId, text: &str, label: impl Into<String>) -> Self {
        Self::from_parts(id, Editor::new(text), FileState::untitled(label))
    }

    pub fn from_loaded(id: SessionId, loaded: LoadedFile) -> Self {
        let file = FileState::tracked(loaded.identity, loaded.stamp);
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
            save_sequence: 0,
            pending_save: None,
            view: SessionViewState::default(),
            auto_title: None,
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
        self.saved_revision = self.editor.document().revision();
        self.generation = self.generation.wrapping_add(1);
        self.autosave_generation = 0;
        self.in_flight = None;
        self.pending_save = None;
        self.view = SessionViewState::default();
        self.auto_title = None;
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
        let (path, guard) = match &intent {
            SaveIntent::Current | SaveIntent::Overwrite => {
                let Some(path) = self.file.path().map(Path::to_path_buf) else {
                    return SaveDecision::NeedsPath;
                };
                let guard = if matches!(intent, SaveIntent::Overwrite) {
                    OverwriteGuard::Force
                } else {
                    OverwriteGuard::ExpectStamp(self.file.stamp())
                };
                (path, guard)
            }
            SaveIntent::To(path) => {
                let same_file = self
                    .file
                    .identity()
                    .is_some_and(|identity| identity.matches_path(path));
                let guard = if same_file {
                    OverwriteGuard::ExpectStamp(self.file.stamp())
                } else {
                    // The user named this target in a dialog that already asked
                    // about replacing it.
                    OverwriteGuard::Force
                };
                (path.clone(), guard)
            }
            SaveIntent::CreateNew(path) => (path.clone(), OverwriteGuard::ExpectStamp(None)),
        };
        if self.in_flight.is_some() {
            self.pending_save = Some(intent);
            return SaveDecision::Queued;
        }
        self.save_sequence = self.save_sequence.wrapping_add(1);
        let ticket = SaveTicket {
            generation: self.generation,
            sequence: self.save_sequence,
            revision: self.revision(),
        };
        self.in_flight = Some(ticket);
        SaveDecision::Write(SaveJob {
            path,
            document: self.editor.document().clone(),
            guard,
            ticket,
        })
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
        match result {
            Ok(saved) => {
                self.file.set_identity(saved.identity, saved.stamp);
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
                        self.file.set_stamp(*stamp);
                        FileEventOutcome::ExternalEdit
                    }
                }
            },
        }
    }

    pub fn presence(&self) -> FilePresence {
        self.file.presence()
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
}
