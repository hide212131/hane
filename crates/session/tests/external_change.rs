//! Issue #653: a file changed by someone else while it is open. The rules are
//! exercised without a UI: a background check reads the disk as one consistent
//! version, the session accepts it only when it has nothing to lose, and every
//! destructive resolution needs a one-time confirmation bound to what the user
//! saw. These are session/service-level tests; the GUI wiring is validated
//! separately.

use hane_session::testing::MemoryFileService;
use hane_session::{
    ContentFingerprint, DocumentSession, ExternalCheckOutcome, ExternalCheckStart, ExternalStatus,
    FileService, OverwriteGuard, ReloadOutcome, ReloadVerifyOutcome, ReloadVerifyRequest,
    ResolutionKind, SaveDecision, SaveIntent, SaveJob, SaveOutcome, SessionId, run_external_check,
    run_reload_verification, run_save_job,
};
use std::path::Path;

const PATH: &str = "/notes/a.md";

fn opened(service: &MemoryFileService, contents: &str) -> DocumentSession {
    service.write_externally(PATH, contents);
    DocumentSession::from_loaded(SessionId(0), service.load(Path::new(PATH)).unwrap())
}

fn type_into(session: &mut DocumentSession, text: &str) {
    session.editor_mut().insert_text(text).expect("insert");
    session.note_edit();
}

fn text(session: &DocumentSession) -> String {
    session.editor().document().full_text()
}

/// One full background check, the way the UI drives it.
fn check(session: &mut DocumentSession, service: &MemoryFileService) -> ExternalCheckOutcome {
    let ExternalCheckStart::Run(request) = session.begin_external_check() else {
        panic!("expected a check to start");
    };
    let result = run_external_check(service, &request);
    session.finish_external_check(&request.ticket, result)
}

fn complete_job(
    session: &mut DocumentSession,
    service: &MemoryFileService,
    job: SaveJob,
) -> SaveOutcome {
    let result = run_save_job(service, &job.path, &job.document, job.guard);
    session.finish_save(job.ticket, result)
}

fn complete(
    session: &mut DocumentSession,
    service: &MemoryFileService,
    decision: SaveDecision,
) -> SaveOutcome {
    let SaveDecision::Write(job) = decision else {
        panic!("expected an accepted write, got {decision:?}");
    };
    complete_job(session, service, job)
}

#[test]
fn fingerprints_tell_same_length_contents_apart() {
    let one = ContentFingerprint::of_bytes(b"aaaa");
    let two = ContentFingerprint::of_bytes(b"aaab");
    assert_eq!(one.len(), two.len());
    assert_ne!(one, two);
    assert_eq!(one, ContentFingerprint::of_bytes(b"aaaa"));
    assert_ne!(
        ContentFingerprint::of_bytes(b""),
        ContentFingerprint::of_bytes(b"\0")
    );
}

#[test]
fn a_clean_document_follows_an_external_save() {
    let service = MemoryFileService::new();
    let mut session = opened(&service, "before\n");
    let generation = session.generation();

    assert_eq!(check(&mut session, &service), ExternalCheckOutcome::Unchanged);
    service.write_externally(PATH, "after\n");
    assert_eq!(check(&mut session, &service), ExternalCheckOutcome::Reloaded);

    assert_eq!(text(&session), "after\n");
    assert!(!session.is_dirty());
    assert_ne!(session.generation(), generation, "a reload starts a new document");
    assert_eq!(session.external_status(), ExternalStatus::InSync);
    // The new version is the baseline now: looking again finds nothing.
    assert_eq!(check(&mut session, &service), ExternalCheckOutcome::Unchanged);
}

#[test]
fn a_reload_keeps_the_caret_on_a_valid_character_boundary() {
    let service = MemoryFileService::new();
    let mut session = opened(&service, "あいうえおかきくけこ\n");
    // Byte 6 is a boundary of the old text (the third character).
    session
        .editor_mut()
        .set_selection(hane_editor::Selection::caret(hane_document::SourceOffset(6)))
        .unwrap();

    // In the new text byte 6 falls inside the second kana (bytes 5..8), so
    // the caret has to be rounded down to the boundary at 5.
    service.write_externally(PATH, "ab\u{3042}\u{3044}\n");
    assert_eq!(check(&mut session, &service), ExternalCheckOutcome::Reloaded);

    let selection = session.editor().selection();
    assert!(text(&session).is_char_boundary(selection.active.0));
    assert_eq!(selection.active.0, 5);
    assert_eq!(selection.anchor.0, 5);
}

#[test]
fn unsaved_edits_are_never_replaced_and_ordinary_saves_keep_refusing() {
    let service = MemoryFileService::new();
    let mut session = opened(&service, "base\n");
    type_into(&mut session, "mine ");
    let mine = text(&session);

    service.write_externally(PATH, "theirs\n");
    assert_eq!(
        check(&mut session, &service),
        ExternalCheckOutcome::Changed { dirty: true }
    );
    assert_eq!(text(&session), mine);
    assert!(session.is_dirty());
    assert_eq!(session.external_status(), ExternalStatus::Changed);
    assert!(session.observed_document().is_some());

    // Neither the first nor a repeated ordinary save writes over theirs.
    for _ in 0..3 {
        let decision = session.request_save(SaveIntent::Current);
        assert!(matches!(
            complete(&mut session, &service, decision),
            SaveOutcome::Conflict
        ));
        assert_eq!(service.contents(PATH).as_deref(), Some("theirs\n"));
        assert!(session.is_dirty());
    }
}

#[test]
fn a_queued_ordinary_save_is_not_promoted_to_a_forced_write() {
    let service = MemoryFileService::new();
    let mut session = opened(&service, "base\n");
    type_into(&mut session, "mine ");

    let first = session.request_save(SaveIntent::Current);
    assert!(matches!(first, SaveDecision::Write(_)));
    // A second request while the first runs is queued as what it was.
    assert!(matches!(
        session.request_save(SaveIntent::Current),
        SaveDecision::Queued
    ));
    service.write_externally(PATH, "theirs\n");
    assert!(matches!(
        complete(&mut session, &service, first),
        SaveOutcome::Conflict
    ));

    let queued = session.take_pending_save().expect("the queued save");
    assert!(matches!(queued, SaveIntent::Current));
    let decision = session.request_save(queued);
    assert!(matches!(
        &decision,
        SaveDecision::Write(job) if !matches!(job.guard, OverwriteGuard::Force)
    ));
    assert!(matches!(
        complete(&mut session, &service, decision),
        SaveOutcome::Conflict
    ));
    assert_eq!(service.contents(PATH).as_deref(), Some("theirs\n"));
}

#[test]
fn a_save_checks_the_disk_even_if_no_notification_arrived() {
    let service = MemoryFileService::new();
    let mut session = opened(&service, "base\n");
    // Establish the baseline, then save once so the fingerprint is the
    // session's own write.
    assert_eq!(check(&mut session, &service), ExternalCheckOutcome::Unchanged);
    type_into(&mut session, "one ");
    let decision = session.request_save(SaveIntent::Current);
    assert!(matches!(
        complete(&mut session, &service, decision),
        SaveOutcome::Saved
    ));

    // Someone else writes; the session never hears about it.
    service.write_externally(PATH, "theirs\n");
    type_into(&mut session, "two ");
    let decision = session.request_save(SaveIntent::Current);
    assert!(matches!(
        complete(&mut session, &service, decision),
        SaveOutcome::Conflict
    ));
    assert_eq!(service.contents(PATH).as_deref(), Some("theirs\n"));
}

#[test]
fn a_busy_document_defers_the_reload_and_asks_for_a_recheck() {
    let service = MemoryFileService::new();
    let mut session = opened(&service, "base\n");
    // A write holds the save slot while the check result arrives.
    let SaveDecision::Write(job) = session.request_save(SaveIntent::Current) else {
        panic!("a write");
    };
    service.write_externally(PATH, "theirs\n");
    assert_eq!(
        check(&mut session, &service),
        ExternalCheckOutcome::Changed { dirty: false }
    );
    assert_eq!(text(&session), "base\n", "not replaced while a write runs");
    assert!(session.needs_external_recheck());

    // The write is refused against theirs; afterwards the recheck reloads.
    assert!(matches!(
        complete_job(&mut session, &service, job),
        SaveOutcome::Conflict
    ));
    assert_eq!(check(&mut session, &service), ExternalCheckOutcome::Reloaded);
    assert_eq!(text(&session), "theirs\n");
}

#[test]
fn an_edit_made_while_the_check_ran_blocks_the_reload() {
    let service = MemoryFileService::new();
    let mut session = opened(&service, "base\n");
    service.write_externally(PATH, "theirs\n");

    let ExternalCheckStart::Run(request) = session.begin_external_check() else {
        panic!("a check");
    };
    let result = run_external_check(&service, &request);
    type_into(&mut session, "typed ");
    let typed = text(&session);

    assert_eq!(
        session.finish_external_check(&request.ticket, result),
        ExternalCheckOutcome::Changed { dirty: true }
    );
    assert_eq!(text(&session), typed);
}

#[test]
fn results_for_a_replaced_document_or_path_are_rejected() {
    let service = MemoryFileService::new();
    let mut session = opened(&service, "base\n");
    service.write_externally(PATH, "theirs\n");

    let ExternalCheckStart::Run(request) = session.begin_external_check() else {
        panic!("a check");
    };
    let result = run_external_check(&service, &request);
    service.write_externally("/notes/other.md", "other\n");
    session.adopt(service.load(Path::new("/notes/other.md")).unwrap());
    assert_eq!(
        session.finish_external_check(&request.ticket, result),
        ExternalCheckOutcome::Stale
    );
    assert_eq!(text(&session), "other\n");

    // A rename while the check runs also invalidates it.
    service.write_externally("/notes/b.md", "b\n");
    session.adopt(service.load(Path::new("/notes/b.md")).unwrap());
    let ExternalCheckStart::Run(request) = session.begin_external_check() else {
        panic!("a check");
    };
    let result = run_external_check(&service, &request);
    session.apply_file_event(&hane_session::FileEvent::Renamed {
        from: "/notes/b.md".into(),
        to: "/notes/c.md".into(),
    });
    assert_eq!(
        session.finish_external_check(&request.ticket, result),
        ExternalCheckOutcome::Stale
    );
}

#[test]
fn notifications_during_a_check_collapse_into_one_follow_up() {
    let service = MemoryFileService::new();
    let mut session = opened(&service, "base\n");
    let ExternalCheckStart::Run(request) = session.begin_external_check() else {
        panic!("a check");
    };
    for _ in 0..5 {
        assert!(matches!(
            session.begin_external_check(),
            ExternalCheckStart::Coalesced
        ));
    }
    let result = run_external_check(&service, &request);
    session.finish_external_check(&request.ticket, result);
    assert!(session.take_recheck_request());
    assert!(!session.take_recheck_request(), "exactly one follow-up");
    assert!(matches!(
        session.begin_external_check(),
        ExternalCheckStart::Run(_)
    ));
}

#[test]
fn an_untitled_document_is_not_watched() {
    let mut session = DocumentSession::untitled(SessionId(0), "", "Untitled");
    assert!(matches!(
        session.begin_external_check(),
        ExternalCheckStart::NotTracked
    ));
}

#[test]
fn a_deleted_file_keeps_the_text_and_is_not_silently_recreated() {
    let service = MemoryFileService::new();
    let mut session = opened(&service, "base\n");
    service.delete(PATH);

    assert_eq!(
        check(&mut session, &service),
        ExternalCheckOutcome::Missing { dirty: false }
    );
    assert_eq!(text(&session), "base\n");
    assert_eq!(session.external_status(), ExternalStatus::Missing);

    type_into(&mut session, "edit ");
    let decision = session.request_save(SaveIntent::Current);
    assert!(matches!(
        complete(&mut session, &service, decision),
        SaveOutcome::Conflict
    ));
    assert!(service.contents(PATH).is_none());

    // Recreating needs the explicit, bound confirmation.
    let approval = session
        .approve_resolution(ResolutionKind::Overwrite, None)
        .expect("the file is known to be gone");
    let job = session.request_confirmed_overwrite(approval).unwrap();
    assert!(matches!(
        complete_job(&mut session, &service, job),
        SaveOutcome::Saved
    ));
    assert!(service.contents(PATH).unwrap().starts_with("edit "));
}

#[test]
fn a_recreate_confirmation_does_not_cover_a_file_that_reappeared() {
    let service = MemoryFileService::new();
    let mut session = opened(&service, "base\n");
    type_into(&mut session, "edit ");
    service.delete(PATH);
    assert!(matches!(
        check(&mut session, &service),
        ExternalCheckOutcome::Missing { dirty: true }
    ));
    let approval = session
        .approve_resolution(ResolutionKind::Overwrite, None)
        .unwrap();
    let job = session.request_confirmed_overwrite(approval).unwrap();

    service.write_externally(PATH, "someone created it\n");
    assert!(matches!(
        complete_job(&mut session, &service, job),
        SaveOutcome::Conflict
    ));
    assert_eq!(service.contents(PATH).as_deref(), Some("someone created it\n"));
}

#[test]
fn an_unreadable_file_is_unverifiable_not_unchanged() {
    let service = MemoryFileService::new();
    let mut session = opened(&service, "base\n");
    service.write_bytes_externally(PATH, &[0xff, 0xfe, 0xfd]);

    assert!(matches!(
        check(&mut session, &service),
        ExternalCheckOutcome::Unverifiable { .. }
    ));
    assert_eq!(text(&session), "base\n");
    assert_eq!(session.external_status(), ExternalStatus::Unverifiable);
    assert!(session.external_failure().is_some());
    assert!(
        session
            .approve_resolution(ResolutionKind::DiscardAndReload, None)
            .is_none(),
        "there is nothing to approve when the disk version is unknown"
    );

    type_into(&mut session, "edit ");
    let decision = session.request_save(SaveIntent::Current);
    assert!(matches!(
        complete(&mut session, &service, decision),
        SaveOutcome::Failed(_)
    ));
}

#[test]
fn a_confirmed_overwrite_is_bound_to_the_version_the_user_saw() {
    let service = MemoryFileService::new();
    let mut session = opened(&service, "base\n");
    type_into(&mut session, "mine ");
    service.write_externally(PATH, "theirs\n");
    check(&mut session, &service);
    let shown = session.observed_fingerprint();
    assert!(shown.is_some());

    // Approving a version that is not the one known is refused.
    assert!(
        session
            .approve_resolution(
                ResolutionKind::Overwrite,
                Some(ContentFingerprint::of_bytes(b"something else")),
            )
            .is_none()
    );

    let approval = session
        .approve_resolution(ResolutionKind::Overwrite, shown)
        .unwrap();
    let job = session.request_confirmed_overwrite(approval).unwrap();
    // The external file changes again before the bytes are replaced.
    service.write_externally(PATH, "theirs, again\n");
    assert!(matches!(
        complete_job(&mut session, &service, job),
        SaveOutcome::Conflict
    ));
    assert_eq!(service.contents(PATH).as_deref(), Some("theirs, again\n"));
    assert!(session.is_dirty());
    assert!(
        session.request_confirmed_overwrite(approval).is_none(),
        "an approval is used once"
    );
}

#[test]
fn a_confirmed_overwrite_writes_when_the_disk_is_still_what_was_shown() {
    let service = MemoryFileService::new();
    let mut session = opened(&service, "base\n");
    type_into(&mut session, "mine ");
    service.write_externally(PATH, "theirs\n");
    check(&mut session, &service);

    let approval = session
        .approve_resolution(ResolutionKind::Overwrite, session.observed_fingerprint())
        .unwrap();
    let job = session.request_confirmed_overwrite(approval).unwrap();
    assert!(matches!(
        complete_job(&mut session, &service, job),
        SaveOutcome::Saved
    ));
    assert!(service.contents(PATH).unwrap().starts_with("mine "));
    assert!(!session.is_dirty());
    assert_eq!(session.external_status(), ExternalStatus::InSync);
    assert_eq!(check(&mut session, &service), ExternalCheckOutcome::Unchanged);
}

#[test]
fn an_approval_dies_with_a_new_edit_or_a_newer_external_version() {
    let service = MemoryFileService::new();
    let mut session = opened(&service, "base\n");
    type_into(&mut session, "mine ");
    service.write_externally(PATH, "theirs\n");
    check(&mut session, &service);

    let approval = session
        .approve_resolution(ResolutionKind::Overwrite, session.observed_fingerprint())
        .unwrap();
    type_into(&mut session, "more ");
    assert!(session.request_confirmed_overwrite(approval).is_none());

    let approval = session
        .approve_resolution(ResolutionKind::Overwrite, session.observed_fingerprint())
        .unwrap();
    service.write_externally(PATH, "theirs v2\n");
    check(&mut session, &service);
    assert!(session.request_confirmed_overwrite(approval).is_none());
    assert_eq!(service.contents(PATH).as_deref(), Some("theirs v2\n"));
}

#[test]
fn a_confirmed_reload_takes_the_disk_version_only_with_a_current_approval() {
    let service = MemoryFileService::new();
    let mut session = opened(&service, "base\n");
    type_into(&mut session, "mine ");
    service.write_externally(PATH, "theirs\n");
    check(&mut session, &service);
    let generation = session.generation();

    // A stale approval discards nothing.
    let stale = session
        .approve_resolution(ResolutionKind::DiscardAndReload, session.observed_fingerprint())
        .unwrap();
    type_into(&mut session, "later ");
    let kept = text(&session);
    assert_eq!(session.apply_confirmed_reload(stale), ReloadOutcome::StaleApproval);
    assert_eq!(text(&session), kept);
    assert!(session.is_dirty());

    // An approval for another operation does not reload either.
    let wrong_kind = session
        .approve_resolution(ResolutionKind::Overwrite, session.observed_fingerprint())
        .unwrap();
    assert_eq!(
        session.apply_confirmed_reload(wrong_kind),
        ReloadOutcome::StaleApproval
    );

    let approval = session
        .approve_resolution(ResolutionKind::DiscardAndReload, session.observed_fingerprint())
        .unwrap();
    assert_eq!(session.apply_confirmed_reload(approval), ReloadOutcome::Reloaded);
    assert_eq!(text(&session), "theirs\n");
    assert!(!session.is_dirty());
    assert_ne!(session.generation(), generation);
    assert_eq!(session.apply_confirmed_reload(approval), ReloadOutcome::StaleApproval);
}

/// Confirms a reload of what the session currently shows and returns the
/// request that reads the disk again.
fn confirm_reload(session: &mut DocumentSession) -> ReloadVerifyRequest {
    let approval = session
        .approve_resolution(ResolutionKind::DiscardAndReload, session.observed_fingerprint())
        .unwrap();
    session.begin_confirmed_reload(approval).unwrap()
}

#[test]
fn a_reload_confirmed_for_the_version_still_on_disk_is_applied() {
    let service = MemoryFileService::new();
    let mut session = opened(&service, "base\n");
    type_into(&mut session, "mine ");
    service.write_externally(PATH, "theirs\n");
    check(&mut session, &service);

    let request = confirm_reload(&mut session);
    let result = run_reload_verification(&service, &request);
    assert_eq!(
        session.finish_confirmed_reload(&request, result),
        ReloadVerifyOutcome::Reloaded
    );
    assert_eq!(text(&session), "theirs\n");
    assert!(!session.is_dirty());
}

#[test]
fn a_disk_change_during_the_dialog_refuses_the_old_approval_and_asks_again() {
    let service = MemoryFileService::new();
    let mut session = opened(&service, "base\n");
    type_into(&mut session, "mine ");
    service.write_externally(PATH, "theirs v1\n");
    check(&mut session, &service);
    let kept = text(&session);
    let shown = session.observed_fingerprint();

    let request = confirm_reload(&mut session);
    // Only the disk changes: no watcher check reaches the session.
    service.write_externally(PATH, "theirs v2\n");
    assert_eq!(session.observed_fingerprint(), shown);
    let result = run_reload_verification(&service, &request);
    assert_eq!(
        session.finish_confirmed_reload(&request, result),
        ReloadVerifyOutcome::DiskChanged
    );

    // The stale observation was not applied; the user's text is intact.
    assert_eq!(text(&session), kept);
    assert!(session.is_dirty());
    assert_eq!(session.external_status(), ExternalStatus::Changed);
    assert_ne!(session.observed_fingerprint(), shown);
    assert_eq!(
        session.observed_document().map(|d| d.full_text()).as_deref(),
        Some("theirs v2\n")
    );

    // Re-approving the old version is refused; the new one needs a new answer.
    assert!(
        session
            .approve_resolution(ResolutionKind::DiscardAndReload, shown)
            .is_none()
    );
    let request = confirm_reload(&mut session);
    let result = run_reload_verification(&service, &request);
    assert_eq!(
        session.finish_confirmed_reload(&request, result),
        ReloadVerifyOutcome::Reloaded
    );
    assert_eq!(text(&session), "theirs v2\n");
}

#[test]
fn a_file_that_vanishes_or_cannot_be_read_during_the_dialog_keeps_the_text() {
    let service = MemoryFileService::new();
    let mut session = opened(&service, "base\n");
    type_into(&mut session, "mine ");
    service.write_externally(PATH, "theirs\n");
    check(&mut session, &service);
    let kept = text(&session);

    let request = confirm_reload(&mut session);
    service.delete(PATH);
    let result = run_reload_verification(&service, &request);
    assert_eq!(
        session.finish_confirmed_reload(&request, result),
        ReloadVerifyOutcome::Missing
    );
    assert_eq!(text(&session), kept);
    assert!(session.is_dirty());
    assert_eq!(session.external_status(), ExternalStatus::Missing);

    // Back on disk as bytes that are not text: unverifiable, still kept.
    service.write_externally(PATH, "theirs again\n");
    check(&mut session, &service);
    let request = confirm_reload(&mut session);
    service.write_bytes_externally(PATH, &[0xff, 0xfe, 0x00, 0xff]);
    let result = run_reload_verification(&service, &request);
    assert!(matches!(
        session.finish_confirmed_reload(&request, result),
        ReloadVerifyOutcome::Unverifiable { .. }
    ));
    assert_eq!(text(&session), kept);
    assert!(session.is_dirty());
}

#[test]
fn a_verification_for_an_edited_document_or_an_older_request_is_dropped() {
    let service = MemoryFileService::new();
    let mut session = opened(&service, "base\n");
    type_into(&mut session, "mine ");
    service.write_externally(PATH, "theirs\n");
    check(&mut session, &service);

    let request = confirm_reload(&mut session);
    type_into(&mut session, "later ");
    let kept = text(&session);
    let result = run_reload_verification(&service, &request);
    assert_eq!(
        session.finish_confirmed_reload(&request, result),
        ReloadVerifyOutcome::Stale
    );
    assert_eq!(text(&session), kept);

    let older = confirm_reload(&mut session);
    let newer = confirm_reload(&mut session);
    let result = run_reload_verification(&service, &older);
    assert_eq!(
        session.finish_confirmed_reload(&older, result),
        ReloadVerifyOutcome::Stale
    );
    assert_eq!(text(&session), kept);
    let result = run_reload_verification(&service, &newer);
    assert_eq!(
        session.finish_confirmed_reload(&newer, result),
        ReloadVerifyOutcome::Reloaded
    );
}

#[test]
fn a_confirmed_reload_waits_while_a_write_is_running() {
    let service = MemoryFileService::new();
    let mut session = opened(&service, "base\n");
    type_into(&mut session, "mine ");
    service.write_externally(PATH, "theirs\n");
    check(&mut session, &service);
    let approval = session
        .approve_resolution(ResolutionKind::DiscardAndReload, session.observed_fingerprint())
        .unwrap();

    let SaveDecision::Write(job) = session.request_save(SaveIntent::Current) else {
        panic!("a write");
    };
    assert_eq!(session.apply_confirmed_reload(approval), ReloadOutcome::Busy);
    assert!(matches!(
        complete_job(&mut session, &service, job),
        SaveOutcome::Conflict
    ));
    assert!(text(&session).starts_with("mine "));
}

#[test]
fn a_file_outside_the_session_file_identity_is_not_touched() {
    // A session keeps checking its own path even when another file with the
    // same name lives elsewhere.
    let service = MemoryFileService::new();
    let mut session = opened(&service, "base\n");
    service.write_externally("/elsewhere/a.md", "unrelated\n");
    assert_eq!(check(&mut session, &service), ExternalCheckOutcome::Unchanged);
    assert_eq!(text(&session), "base\n");
}
