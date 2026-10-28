//! Document sessions and the file I/O boundary.
// Session identity and state accessors are intentionally usable as event-loop
// probes without requiring callers to retain a value.
#![allow(
    clippy::must_use_candidate,
    reason = "session accessors are intentionally discardable event-loop probes"
)]
#![allow(
    clippy::missing_errors_doc,
    reason = "session error contracts are documented on the owning service and store abstractions"
)]
#![allow(
    clippy::missing_panics_doc,
    reason = "test-only invariant panics are evident at the assertion site"
)]
#![allow(
    clippy::unnested_or_patterns,
    reason = "the service match is clearer when related variants remain grouped"
)]
#![allow(
    clippy::match_same_arms,
    reason = "identical outcome handling makes the exhaustive state transition explicit"
)]
//!
//! Everything about "which file is open, is it dirty, when does it get written,
//! and what happens when it changes underneath us" lives here, with no GPUI and
//! no renderer. The UI issues requests and applies results; it never owns a
//! `PathBuf`, never writes a file, and never reaches for the process working
//! directory.

mod date_badge;
mod draft;
mod identity;
mod naming;
mod pins;
mod resource;
pub mod search;
mod service;
mod session;
mod store;
pub mod testing;
mod watch;
mod workfolder;

pub use date_badge::{
    CalendarDate, DateBadgeRange, FileNameDateBadge, date_badge_range, format_relative_date_label,
    local_today, split_file_name_for_badge,
};
pub use draft::{
    DraftId, DraftStore, OsDraftStore, RecoveredDraft, RecoveredDrafts,
    next_draft_snapshot_sequence,
};
pub use identity::{
    ContentFingerprint, ExternalChange, FileIdentity, FilePresence, FileStamp, FileState,
    Fingerprinter,
};
pub use naming::{
    TitleSyncAction, decide_title_sync, extract_h1_title, unique_folder_name,
    unique_markdown_filename,
};
pub use pins::{
    FilePinStore, MemoryPinStore, PinError, PinKind, PinRepository, PinSnapshot, PinUpdate,
    PinnedEntry, apply_pin_update, validate_pin_path,
};
pub use resource::ResourceResolver;
pub use service::{
    DiskVersion, FileService, LoadedFile, ObservedFile, OsFileService, OverwriteGuard, ReadFile,
    SaveFailure, SavedFile, observe_disk, run_create_new_job, run_save_job,
};
pub use session::{
    AutosaveTicket, CloseDecision, DocumentSession, ExternalCheckOutcome, ExternalCheckRequest,
    ExternalCheckResult, ExternalCheckStart, ExternalCheckTicket, ExternalStatus, FileEvent,
    FileEventOutcome, OpenDecision, OpenPolicy, ReloadOutcome, ReloadVerifyOutcome,
    ReloadVerifyRequest, ReloadVerifyResult, ResolutionApproval, ResolutionKind, SaveDecision,
    SaveIntent, SaveJob, SaveOutcome, SaveTicket, SessionId, SessionSet, SessionViewState,
    UnsavedChanges, run_external_check, run_reload_verification,
};
pub use store::{
    FileStateStore, InvalidNetworkConfig, MemoryStateStore, NetworkConfig, RecentFiles,
    RecentFilesRepository, Settings, SettingsRepository, StateStores, ThemePreference, pin_state_root,
};
pub use watch::{
    DEFAULT_MAX_WAIT, DEFAULT_QUIET_PERIOD, FolderWatcher, WatchBatch, WatchTiming, WatchedNames,
};
pub use workfolder::{
    OsWorkFolderScanner, WorkFolder, WorkFolderEntry, WorkFolderFolder, WorkFolderNode,
    WorkFolderScanner, WorkFolderSortOrder, next_modified_sequence, stamp_with_sequence,
};
