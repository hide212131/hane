//! Document sessions and the file I/O boundary.
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

pub use identity::{ExternalChange, FileIdentity, FilePresence, FileStamp, FileState};
pub use resource::ResourceResolver;
pub use service::{
    FileService, LoadedFile, OsFileService, OverwriteGuard, SaveFailure, SavedFile,
    atomic_write_bytes, run_save_job,
};
pub use session::{
    AutosaveTicket, CloseDecision, DocumentSession, FileEvent, FileEventOutcome, OpenDecision,
    OpenPolicy, SaveDecision, SaveIntent, SaveJob, SaveOutcome, SaveTicket, SessionId, SessionSet,
    SessionViewState, UnsavedChanges, untitled_target,
};
pub use store::{
    FileStateStore, MemoryStateStore, RecentFiles, RecentFilesRepository, Settings,
    SettingsRepository, StateStores, ThemePreference,
};
