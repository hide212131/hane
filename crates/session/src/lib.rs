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

mod identity;
mod resource;
mod service;
mod session;
mod store;
pub mod testing;
mod workfolder;

pub use identity::{ExternalChange, FileIdentity, FilePresence, FileStamp, FileState};
pub use resource::ResourceResolver;
pub use service::{
    FileService, LoadedFile, OsFileService, OverwriteGuard, SaveFailure, SavedFile, run_save_job,
};
pub use session::{
    AutosaveTicket, CloseDecision, DocumentSession, FileEvent, FileEventOutcome, OpenDecision,
    OpenPolicy, SaveDecision, SaveIntent, SaveJob, SaveOutcome, SaveTicket, SessionId, SessionSet,
    SessionViewState, UnsavedChanges,
};
pub use store::{
    FileStateStore, MemoryStateStore, RecentFiles, RecentFilesRepository, Settings,
    SettingsRepository, StateStores, ThemePreference,
};
pub use workfolder::{OsWorkFolderScanner, WorkFolder, WorkFolderEntry, WorkFolderScanner};
