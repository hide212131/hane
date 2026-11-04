//! Issue #591: the history rewrite of a Save As is prepared off the UI thread
//! from a snapshot and only applied when the write lands. Applying it must not
//! evaluate the plan again (that is the document size times history depth cost
//! this split removes), must give exactly what evaluating the plan gives, and
//! must fall back to evaluating it when the editor moved on.

use hane_document::{SourceOffset, TextBuffer};
use hane_editor::{EditorCommand, RewritePlan, Selection};
use hane_session::testing::MemoryFileService;
use hane_session::{
    DocumentSession, FileService, SaveDecision, SaveIntent, SaveJob, SaveOutcome, SessionId,
    run_save_job,
};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const PATH: &str = "/notes/a.md";
const TARGET: &str = "/other/b.md";
const OLD: &str = "old.png";
const NEW: &str = "renamed-longer.png";

fn opened(service: &MemoryFileService, contents: &str) -> DocumentSession {
    service.write_externally(PATH, contents);
    DocumentSession::from_loaded(SessionId(0), service.load(Path::new(PATH)).unwrap())
}

fn counting_plan(calls: &Arc<AtomicUsize>) -> Arc<RewritePlan> {
    let calls = Arc::clone(calls);
    Arc::new(move |text: &str| {
        calls.fetch_add(1, Ordering::SeqCst);
        text.match_indices(OLD)
            .map(|(start, _)| (start..start + OLD.len(), NEW.to_owned()))
            .collect()
    })
}

fn text(session: &DocumentSession) -> String {
    session.editor().document().full_text()
}

fn type_at_end(session: &mut DocumentSession, typed: &str) {
    let end = session.editor().document().len_bytes().0;
    session
        .editor_mut()
        .set_selection(Selection::caret(SourceOffset(end)))
        .unwrap();
    session.editor_mut().insert_text(typed).unwrap();
    session.note_edit();
}

/// A document whose history holds several separate steps, some of them
/// creating references, with the last two undone (so Redo is not empty).
fn with_history(service: &MemoryFileService) -> DocumentSession {
    let mut session = opened(service, "前![](old.png)\n");
    for step in ["a\n", "![](old.png)\n", "いろは\n", "b![](old.png)\n"] {
        type_at_end(&mut session, step);
    }
    session.editor_mut().dispatch(EditorCommand::Undo).unwrap();
    session.editor_mut().dispatch(EditorCommand::Undo).unwrap();
    session
}

fn undo_all(session: &mut DocumentSession) -> Vec<String> {
    let mut states = Vec::new();
    while session.editor().can_undo() {
        session.editor_mut().dispatch(EditorCommand::Undo).unwrap();
        states.push(text(session));
    }
    states
}

fn redo_all(session: &mut DocumentSession) -> Vec<String> {
    let mut states = Vec::new();
    while session.editor().can_redo() {
        session.editor_mut().dispatch(EditorCommand::Redo).unwrap();
        states.push(text(session));
    }
    states
}

fn write_job(session: &mut DocumentSession) -> SaveJob {
    let SaveDecision::Write(job) = session.request_save(SaveIntent::To(TARGET.into())) else {
        panic!("expected an accepted write");
    };
    job
}

fn land(service: &MemoryFileService, session: &mut DocumentSession, job: SaveJob) -> SaveOutcome {
    let result = run_save_job(service, &job.path, &job.document, job.guard);
    session.finish_save(job.ticket, result)
}

#[test]
fn applying_a_prepared_rewrite_never_evaluates_the_plan_and_matches_the_plain_one() {
    let calls = Arc::new(AtomicUsize::new(0));

    let plain_service = MemoryFileService::new();
    let mut plain = with_history(&plain_service);
    assert!(plain.arm_save_rewrite(counting_plan(&calls)));
    let job = write_job(&mut plain);
    assert!(matches!(land(&plain_service, &mut plain, job), SaveOutcome::Saved));
    assert!(calls.load(Ordering::SeqCst) > 0, "the plain path evaluates it");

    let service = MemoryFileService::new();
    let mut session = with_history(&service);
    let plan = counting_plan(&calls);
    let snapshot = session.editor().rewrite_snapshot().unwrap();
    let prepared = snapshot.prepare(&*plan);
    assert!(prepared.is_some());
    calls.store(0, Ordering::SeqCst);
    assert!(session.arm_prepared_save_rewrite(plan, prepared));
    let job = write_job(&mut session);
    assert_eq!(job.document.full_text(), text(&session).replace(OLD, NEW));
    assert!(matches!(land(&service, &mut session, job), SaveOutcome::Saved));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "starting and landing a prepared rewrite does not evaluate the plan"
    );

    assert_eq!(text(&session), text(&plain));
    assert_eq!(session.editor().selection(), plain.editor().selection());
    assert_eq!(undo_all(&mut session), undo_all(&mut plain));
    assert_eq!(redo_all(&mut session), redo_all(&mut plain));
    assert!(!text(&session).contains(OLD));
}

#[test]
fn a_rewrite_prepared_for_an_older_state_is_not_used_after_an_edit() {
    let calls = Arc::new(AtomicUsize::new(0));
    let service = MemoryFileService::new();
    let mut session = with_history(&service);
    let plan = counting_plan(&calls);
    let prepared = session.editor().rewrite_snapshot().unwrap().prepare(&*plan);
    // The document moves on between the preparation and the write.
    type_at_end(&mut session, "新![](old.png)");
    assert!(session.arm_prepared_save_rewrite(Arc::clone(&plan), prepared));
    let before = text(&session);
    let job = write_job(&mut session);
    assert_eq!(job.document.full_text(), before.replace(OLD, NEW));
    assert!(matches!(land(&service, &mut session, job), SaveOutcome::Saved));
    assert_eq!(text(&session), before.replace(OLD, NEW));
    for state in undo_all(&mut session).iter().chain(&redo_all(&mut session)) {
        assert!(!state.contains(OLD), "{state:?}");
    }
}

#[test]
fn an_edit_made_while_a_prepared_write_runs_still_follows_and_stays_unsaved() {
    let calls = Arc::new(AtomicUsize::new(0));
    let service = MemoryFileService::new();
    let mut session = with_history(&service);
    let plan = counting_plan(&calls);
    let prepared = session.editor().rewrite_snapshot().unwrap().prepare(&*plan);
    assert!(session.arm_prepared_save_rewrite(plan, prepared));
    let before = text(&session);
    let job = write_job(&mut session);
    type_at_end(&mut session, "書込中![](old.png)");
    let outcome = land(&service, &mut session, job);
    assert!(matches!(outcome, SaveOutcome::SavedStale));
    assert_eq!(
        service.contents(TARGET).as_deref(),
        Some(before.replace(OLD, NEW).as_str())
    );
    assert_eq!(
        text(&session),
        format!("{}書込中![]({NEW})", before.replace(OLD, NEW))
    );
    assert!(session.is_dirty());
    for state in undo_all(&mut session).iter().chain(&redo_all(&mut session)) {
        assert!(!state.contains(OLD), "{state:?}");
    }
}

#[test]
fn an_open_composition_has_no_snapshot_and_a_composition_opened_later_is_stale() {
    let service = MemoryFileService::new();
    let mut session = with_history(&service);
    let calls = Arc::new(AtomicUsize::new(0));
    let plan = counting_plan(&calls);
    let prepared = session.editor().rewrite_snapshot().unwrap().prepare(&*plan).unwrap();
    assert!(session.editor().rewrite_is_current(&prepared));
    let end = session.editor().document().len_bytes().0;
    session
        .editor_mut()
        .set_selection(Selection::caret(SourceOffset(end)))
        .unwrap();
    session
        .editor_mut()
        .replace_and_mark_text(None, "かな", None)
        .unwrap();
    assert!(session.editor().rewrite_snapshot().is_none());
    assert!(
        !session.editor().rewrite_is_current(&prepared),
        "the pending composition entry is not in the prepared history"
    );
}
