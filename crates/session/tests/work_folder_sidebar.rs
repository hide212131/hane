//! The rules a work folder sidebar relies on: switching to a note the sidebar
//! lists either reuses an already-open session or lazily loads the file, and
//! never discards unsaved edits in whatever else happens to be open. Fixed
//! without a UI, the same way `conflict_rules.rs` fixes the single-file rules.

use hane_session::{
    FileService, OpenDecision, OpenPolicy, OsFileService, OsWorkFolderScanner, SessionSet,
    WorkFolderScanner,
};
use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};

fn temporary_directory(label: &str) -> std::path::PathBuf {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "hane-work-folder-sidebar-{label}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ))
}

#[test]
fn a_sidebar_entry_not_yet_open_loads_into_a_session_of_its_own() {
    let root = temporary_directory("new-session");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("LangChain4j.md"), "# LangChain4j\n").unwrap();
    fs::write(root.join("Meeting.md"), "# Meeting\n").unwrap();

    let work_folder = OsWorkFolderScanner.scan(&root).unwrap();
    let first = work_folder.entries()[0].path().to_path_buf();
    let second = work_folder.entries()[1].path().to_path_buf();

    let loaded = OsFileService.load(&first).expect("first entry loads");
    let mut sessions = SessionSet::with_loaded(loaded);
    let first_session = sessions.active_id();

    // The sidebar never blocks input on the current note to switch to
    // another: a `+` press or edit in the first note before the second
    // finishes loading is still safe, because the second note is not routed
    // into the active session.
    match sessions.open_decision(&second, OpenPolicy::NewSession) {
        OpenDecision::Load { into } => assert_eq!(into, None),
        other => panic!("expected a fresh session for an unopened entry, got {other:?}"),
    }
    let loaded_second = OsFileService.load(&second).expect("second entry loads");
    let second_session = sessions.apply_open(None, loaded_second);

    assert_ne!(first_session, second_session);
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions.active_id(), second_session);
    assert_eq!(
        sessions.get(first_session).unwrap().path(),
        Some(first.as_path())
    );

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn reselecting_an_already_open_sidebar_entry_switches_without_reloading() {
    let root = temporary_directory("activate");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("LangChain4j.md"), "# LangChain4j\n").unwrap();
    fs::write(root.join("Meeting.md"), "# Meeting\n").unwrap();

    let work_folder = OsWorkFolderScanner.scan(&root).unwrap();
    let first = work_folder.entries()[0].path().to_path_buf();
    let second = work_folder.entries()[1].path().to_path_buf();

    let mut sessions = SessionSet::with_loaded(OsFileService.load(&first).unwrap());
    let first_session = sessions.active_id();
    let second_session = sessions.apply_open(None, OsFileService.load(&second).unwrap());
    assert_eq!(sessions.active_id(), second_session);

    // Clicking the first entry again in the sidebar must switch back, not
    // read the file a second time.
    match sessions.open_decision(&first, OpenPolicy::NewSession) {
        OpenDecision::Activate(id) => assert_eq!(id, first_session),
        other => panic!("expected an already-open entry to activate, got {other:?}"),
    }

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn switching_to_a_sidebar_entry_never_discards_a_dirty_session_elsewhere() {
    let root = temporary_directory("dirty-preserved");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("LangChain4j.md"), "# LangChain4j\n").unwrap();
    fs::write(root.join("Meeting.md"), "# Meeting\n").unwrap();

    let work_folder = OsWorkFolderScanner.scan(&root).unwrap();
    let first = work_folder.entries()[0].path().to_path_buf();
    let second = work_folder.entries()[1].path().to_path_buf();

    let mut sessions = SessionSet::with_loaded(OsFileService.load(&first).unwrap());
    sessions
        .active_mut()
        .editor_mut()
        .insert_text("unsaved")
        .unwrap();
    sessions.active_mut().note_edit();
    assert!(sessions.active().is_dirty());

    let OpenDecision::Load { into } = sessions.open_decision(&second, OpenPolicy::NewSession)
    else {
        panic!("a session of its own is always allowed, dirty active session or not");
    };
    let loaded = OsFileService.load(&second).unwrap();
    sessions.apply_open(into, loaded);

    assert_eq!(sessions.len(), 2);
    assert!(
        sessions
            .sessions()
            .any(|session| session.path() == Some(first.as_path()) && session.is_dirty()),
        "the dirty first note must still be open, unsaved"
    );

    fs::remove_dir_all(root).unwrap();
}
