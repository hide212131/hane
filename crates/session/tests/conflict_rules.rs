//! The rules a filer, an autosave timer and an external editor have to agree on,
//! fixed without a UI. Every scenario here is one a filer will produce as soon
//! as it exists: open a file that is already open, rename the file under the
//! caret, delete it, or let someone else edit it while the session is dirty.

#![allow(
    clippy::float_cmp,
    reason = "the fixture asserts exact serialization of deterministic ratios"
)]

use hane_session::testing::MemoryFileService;
use hane_session::{
    CloseDecision, DocumentSession, FileEvent, FileEventOutcome, FileService, OpenDecision,
    OpenPolicy, OverwriteGuard, SaveDecision, SaveIntent, SaveJob, SaveOutcome, SessionId,
    SessionSet, UnsavedChanges, run_save_job,
};
use std::path::Path;

fn service_with(files: &[(&str, &str)]) -> MemoryFileService {
    let service = MemoryFileService::new();
    for (path, contents) in files {
        service.write_externally(path, contents);
    }
    service
}

fn opened(service: &MemoryFileService, path: &str) -> SessionSet {
    SessionSet::with_loaded(service.load(Path::new(path)).expect("file exists"))
}

fn type_into(session: &mut DocumentSession, text: &str) {
    session.editor_mut().insert_text(text).expect("insert");
    session.note_edit();
}

/// Runs a decided save all the way through the I/O boundary, the way the UI does
/// on a background thread.
fn complete(
    session: &mut DocumentSession,
    service: &MemoryFileService,
    decision: SaveDecision,
) -> SaveOutcome {
    let SaveDecision::Write(SaveJob {
        path,
        document,
        guard,
        ticket,
    }) = decision
    else {
        panic!("expected an accepted write, got {decision:?}");
    };
    let result = run_save_job(service, &path, &document, guard);
    session.finish_save(ticket, result)
}

#[test]
fn opening_a_file_into_a_dirty_session_is_refused() {
    let service = service_with(&[("/notes/a.md", "a\n"), ("/notes/b.md", "b\n")]);
    let mut sessions = opened(&service, "/notes/a.md");
    type_into(sessions.active_mut(), "x");

    assert_eq!(
        sessions.open_decision(Path::new("/notes/b.md"), OpenPolicy::ReuseActive),
        OpenDecision::Reject(UnsavedChanges)
    );

    // A session of its own is always allowed: nothing is displaced.
    assert_eq!(
        sessions.open_decision(Path::new("/notes/b.md"), OpenPolicy::NewSession),
        OpenDecision::Load { into: None }
    );
}

#[test]
fn opening_a_clean_session_reuses_it_and_invalidates_derived_state() {
    let service = service_with(&[("/notes/a.md", "a\n"), ("/notes/b.md", "b\n")]);
    let mut sessions = opened(&service, "/notes/a.md");
    let generation = sessions.active().generation();

    let OpenDecision::Load { into } =
        sessions.open_decision(Path::new("/notes/b.md"), OpenPolicy::ReuseActive)
    else {
        panic!("a clean session accepts an open");
    };
    assert_eq!(into, Some(sessions.active_id()));

    let loaded = service.load(Path::new("/notes/b.md")).unwrap();
    sessions.apply_open(into, loaded);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions.active().path(), Some(Path::new("/notes/b.md")));
    assert!(!sessions.active().is_dirty());
    assert_ne!(
        sessions.active().generation(),
        generation,
        "everything derived from the previous document must be invalidated"
    );
}

#[test]
fn opening_a_file_that_is_already_open_switches_instead_of_reloading() {
    let service = service_with(&[("/notes/a.md", "a\n"), ("/notes/b.md", "b\n")]);
    let mut sessions = opened(&service, "/notes/a.md");
    let first = sessions.active_id();
    type_into(sessions.active_mut(), "unsaved");
    let second = sessions.apply_open(None, service.load(Path::new("/notes/b.md")).unwrap());
    assert_eq!(sessions.active_id(), second);

    // Spelled differently on purpose: identity is by file, not by string.
    assert_eq!(
        sessions.open_decision(Path::new("/notes/./sub/../a.md"), OpenPolicy::ReuseActive),
        OpenDecision::Activate(first)
    );
    assert!(sessions.activate(first));
    assert!(
        sessions.active().is_dirty(),
        "re-opening the same file must not discard its unsaved edits"
    );
}

#[test]
fn one_write_runs_at_a_time_and_a_burst_collapses_to_the_last_target() {
    let service = service_with(&[("/notes/a.md", "a\n")]);
    let mut sessions = opened(&service, "/notes/a.md");
    let session = sessions.active_mut();
    type_into(session, "one");

    let first = session.request_save(SaveIntent::Current);
    assert!(matches!(first, SaveDecision::Write(_)));
    assert!(session.save_in_flight());

    type_into(session, "two");
    assert!(matches!(
        session.request_save(SaveIntent::Current),
        SaveDecision::Queued
    ));
    assert!(matches!(
        session.request_save(SaveIntent::To("/notes/copy.md".into())),
        SaveDecision::Queued
    ));

    // The snapshot that was already in flight lands, but the document moved on.
    assert!(matches!(
        complete(session, &service, first),
        SaveOutcome::SavedStale
    ));
    assert!(session.is_dirty());

    let pending = session
        .take_pending_save()
        .expect("a queued target survives");
    assert!(matches!(pending, SaveIntent::To(path) if path == Path::new("/notes/copy.md")));
    assert!(
        session.take_pending_save().is_none(),
        "only one queued write"
    );
}

#[test]
fn a_write_that_outlives_its_document_is_dropped() {
    let service = service_with(&[("/notes/a.md", "a\n"), ("/notes/b.md", "b\n")]);
    let mut sessions = opened(&service, "/notes/a.md");
    let session = sessions.active_mut();
    type_into(session, "one");
    let SaveDecision::Write(job) = session.request_save(SaveIntent::Current) else {
        panic!("expected a write");
    };

    // The user opens another file before the write comes back.
    session.adopt(service.load(Path::new("/notes/b.md")).unwrap());
    let result = run_save_job(&service, &job.path, &job.document, job.guard);
    assert!(matches!(
        session.finish_save(job.ticket, result),
        SaveOutcome::Superseded
    ));
    assert_eq!(session.path(), Some(Path::new("/notes/b.md")));
    assert!(!session.is_dirty());
}

#[test]
fn autosave_only_fires_for_the_edit_that_armed_it() {
    let service = service_with(&[("/notes/a.md", "a\n")]);
    let mut sessions = opened(&service, "/notes/a.md");
    let session = sessions.active_mut();

    assert!(
        session.autosave_ticket(true).is_none(),
        "a clean document has nothing to autosave"
    );
    type_into(session, "one");
    assert!(
        session.autosave_ticket(false).is_none(),
        "autosave off means no timer at all"
    );
    let ticket = session.autosave_ticket(true).expect("armed");
    assert!(session.autosave_is_current(ticket, true));
    assert!(!session.autosave_is_current(ticket, false));

    type_into(session, "two");
    assert!(
        !session.autosave_is_current(ticket, true),
        "a newer keystroke invalidates the older timer"
    );
}

#[test]
fn an_untitled_session_never_autosaves_and_asks_for_a_path() {
    let mut session = DocumentSession::untitled(SessionId(0), "", "Untitled");
    type_into(&mut session, "draft");
    assert!(session.autosave_ticket(true).is_none());
    assert!(matches!(
        session.request_save(SaveIntent::Current),
        SaveDecision::NeedsPath
    ));
}

#[test]
fn a_save_refuses_to_clobber_an_external_edit_until_the_user_says_so() {
    let service = service_with(&[("/notes/a.md", "a\n")]);
    let mut sessions = opened(&service, "/notes/a.md");
    let session = sessions.active_mut();
    type_into(session, "mine");

    service.write_externally("/notes/a.md", "theirs\n");
    let decision = session.request_save(SaveIntent::Current);
    assert!(matches!(
        complete(session, &service, decision),
        SaveOutcome::Conflict
    ));
    assert_eq!(
        service.contents("/notes/a.md").as_deref(),
        Some("theirs\n"),
        "a refused save leaves the other edit on disk"
    );
    assert!(session.is_dirty());

    let decision = session.request_save(SaveIntent::Overwrite);
    assert!(matches!(
        complete(session, &service, decision),
        SaveOutcome::Saved
    ));
    assert!(service.contents("/notes/a.md").unwrap().starts_with("mine"));
    assert!(!session.is_dirty());
}

#[test]
fn save_as_to_a_new_target_does_not_need_the_stamp_of_the_old_file() {
    let service = service_with(&[("/notes/a.md", "a\n")]);
    let mut sessions = opened(&service, "/notes/a.md");
    let session = sessions.active_mut();
    type_into(session, "mine");

    let decision = session.request_save(SaveIntent::To("/notes/copy.md".into()));
    assert!(matches!(&decision, SaveDecision::Write(job) if job.guard == OverwriteGuard::Force));
    assert!(matches!(
        complete(session, &service, decision),
        SaveOutcome::Saved
    ));
    assert_eq!(session.path(), Some(Path::new("/notes/copy.md")));
    assert!(!session.is_dirty());
    assert_eq!(
        service.contents("/notes/a.md").as_deref(),
        Some("a\n"),
        "Save As leaves the original alone"
    );
}

#[test]
fn a_rename_moves_the_session_without_touching_the_document() {
    let service = service_with(&[("/notes/a.md", "a\n")]);
    let mut sessions = opened(&service, "/notes/a.md");
    type_into(sessions.active_mut(), "unsaved");
    let generation = sessions.active().generation();

    service.rename("/notes/a.md", "/archive/a.md");
    let outcomes = sessions.apply_file_event(&FileEvent::Renamed {
        from: "/notes/a.md".into(),
        to: "/archive/a.md".into(),
    });
    assert_eq!(
        outcomes,
        vec![(sessions.active_id(), FileEventOutcome::Renamed)]
    );
    let session = sessions.active();
    assert_eq!(session.path(), Some(Path::new("/archive/a.md")));
    assert!(session.is_dirty(), "a rename is not a save");
    assert_eq!(
        session.generation(),
        generation,
        "a rename must not invalidate parse or layout state"
    );
    assert_eq!(
        session.resource_resolver().resolve("img/feather.png"),
        Path::new("/archive/img/feather.png"),
        "relative resources follow the file, not the process directory"
    );
}

#[test]
fn a_delete_keeps_the_unsaved_document_and_a_save_recreates_the_file() {
    let service = service_with(&[("/notes/a.md", "a\n")]);
    let mut sessions = opened(&service, "/notes/a.md");
    type_into(sessions.active_mut(), "unsaved");

    service.delete("/notes/a.md");
    let outcomes = sessions.apply_file_event(&FileEvent::Deleted("/notes/a.md".into()));
    assert_eq!(
        outcomes,
        vec![(
            sessions.active_id(),
            FileEventOutcome::Missing { dirty: true }
        )]
    );

    let session = sessions.active_mut();
    let decision = session.request_save(SaveIntent::Current);
    assert!(matches!(
        complete(session, &service, decision),
        SaveOutcome::Saved
    ));
    assert!(
        service
            .contents("/notes/a.md")
            .unwrap()
            .starts_with("unsaved")
    );
}

#[test]
fn an_external_edit_is_reloadable_only_while_the_session_has_nothing_to_lose() {
    let service = service_with(&[("/notes/a.md", "a\n")]);
    let mut sessions = opened(&service, "/notes/a.md");

    service.write_externally("/notes/a.md", "theirs\n");
    let stamp = service.stamp(Path::new("/notes/a.md"));
    let event = FileEvent::ChangedOnDisk {
        path: "/notes/a.md".into(),
        stamp,
    };
    assert_eq!(
        sessions.apply_file_event(&event),
        vec![(sessions.active_id(), FileEventOutcome::ExternalEdit)]
    );

    type_into(sessions.active_mut(), "mine");
    service.write_externally("/notes/a.md", "theirs again\n");
    let event = FileEvent::ChangedOnDisk {
        path: "/notes/a.md".into(),
        stamp: service.stamp(Path::new("/notes/a.md")),
    };
    assert_eq!(
        sessions.apply_file_event(&event),
        vec![(sessions.active_id(), FileEventOutcome::Conflict)],
        "a dirty session is never resolved automatically"
    );
}

#[test]
fn events_for_other_files_leave_every_session_alone() {
    let service = service_with(&[("/notes/a.md", "a\n")]);
    let mut sessions = opened(&service, "/notes/a.md");
    assert!(
        sessions
            .apply_file_event(&FileEvent::Deleted("/notes/other.md".into()))
            .is_empty()
    );
    assert_eq!(sessions.active().path(), Some(Path::new("/notes/a.md")));
}

#[test]
fn a_failed_load_leaves_the_session_untouched() {
    let service = service_with(&[("/notes/a.md", "a\n")]);
    let sessions = opened(&service, "/notes/a.md");
    assert!(service.load(Path::new("/notes/missing.md")).is_err());
    assert_eq!(sessions.active().path(), Some(Path::new("/notes/a.md")));
    assert!(!sessions.active().is_dirty());
}

#[test]
fn closing_refuses_to_drop_unsaved_work_and_never_empties_the_window() {
    let service = service_with(&[("/notes/a.md", "a\n")]);
    let mut sessions = opened(&service, "/notes/a.md");
    let first = sessions.active_id();
    type_into(sessions.active_mut(), "unsaved");
    assert_eq!(
        sessions.close(first, "Untitled"),
        CloseDecision::Reject(UnsavedChanges)
    );

    let decision = sessions.active_mut().request_save(SaveIntent::Current);
    complete(sessions.active_mut(), &service, decision);
    assert_eq!(sessions.close(first, "Untitled"), CloseDecision::Close);
    assert_eq!(sessions.len(), 1, "the window always shows a document");
    assert_eq!(sessions.active().path(), None);
}

#[test]
fn switching_sessions_keeps_each_documents_own_state() {
    let service = service_with(&[("/notes/a.md", "a\n"), ("/notes/b.md", "b\n")]);
    let mut sessions = opened(&service, "/notes/a.md");
    let first = sessions.active_id();
    type_into(sessions.active_mut(), "one");
    sessions
        .active_mut()
        .set_view_state(hane_session::SessionViewState { scroll_y: 120.0 });

    let second = sessions.apply_open(None, service.load(Path::new("/notes/b.md")).unwrap());
    assert_ne!(first, second);
    assert!(!sessions.active().is_dirty());
    assert_eq!(sessions.active().view_state().scroll_y, 0.0);

    assert!(sessions.activate(first));
    assert!(sessions.active().is_dirty());
    assert_eq!(sessions.active().view_state().scroll_y, 120.0);
    assert_eq!(sessions.active().path(), Some(Path::new("/notes/a.md")));
}
