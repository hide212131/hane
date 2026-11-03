//! Issue #591: a Save As that has to rewrite the text it saves (the references
//! of copied images renamed in the new directory). The write saves the
//! rewritten text; the live document, its selection and its whole edit history
//! follow only when that write lands, and never otherwise.

use hane_document::{SourceOffset, TextBuffer};
use hane_editor::{EditorCommand, RewritePlan, Selection};
use hane_session::testing::MemoryFileService;
use hane_session::{
    DocumentSession, FileService, SaveDecision, SaveFailure, SaveIntent, SaveJob, SaveOutcome,
    SessionId, run_save_job,
};
use std::path::Path;
use std::sync::Arc;

const PATH: &str = "/notes/a.md";
const TARGET: &str = "/other/b.md";
const OLD: &str = "old.png";
const NEW: &str = "renamed-longer.png";

fn opened(service: &MemoryFileService, contents: &str) -> DocumentSession {
    service.write_externally(PATH, contents);
    DocumentSession::from_loaded(SessionId(0), service.load(Path::new(PATH)).unwrap())
}

fn rename_plan() -> Arc<RewritePlan> {
    Arc::new(|text: &str| {
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

fn start_rewritten_save(session: &mut DocumentSession) -> SaveJob {
    assert!(session.arm_save_rewrite(rename_plan()));
    let SaveDecision::Write(job) = session.request_save(SaveIntent::To(TARGET.into())) else {
        panic!("expected an accepted write");
    };
    job
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

#[test]
fn the_write_saves_the_rewritten_text_and_the_document_follows_on_success() {
    let service = MemoryFileService::new();
    let mut session = opened(&service, "前![](old.png)\n![](old.png)後");
    session.editor_mut().insert_text("A").unwrap();
    type_at_end(&mut session, "B");
    let before = text(&session);
    let job = start_rewritten_save(&mut session);
    assert_eq!(job.document.full_text(), before.replace(OLD, NEW));
    assert_eq!(text(&session), before, "nothing changes before the result");
    let result = run_save_job(&service, &job.path, &job.document, job.guard);
    assert!(matches!(
        session.finish_save(job.ticket, result),
        SaveOutcome::Saved
    ));
    let saved = before.replace(OLD, NEW);
    assert_eq!(text(&session), saved);
    assert_eq!(service.contents(TARGET).as_deref(), Some(saved.as_str()));
    assert!(!session.is_dirty(), "the rewritten text is what was saved");

    let undone = undo_all(&mut session);
    assert_eq!(
        undone,
        vec![
            saved.strip_suffix('B').unwrap().to_owned(),
            saved
                .strip_suffix('B')
                .unwrap()
                .strip_prefix('A')
                .unwrap()
                .to_owned(),
        ]
    );
    let redone = redo_all(&mut session);
    assert_eq!(redone.last(), Some(&saved));
    for state in undone.iter().chain(&redone) {
        assert!(!state.contains(OLD), "{state:?}");
    }
    let end = session.editor().document().len_bytes().0;
    let selection = session.editor().selection();
    assert!(selection.anchor.0 <= end && selection.active.0 <= end);
}

#[test]
fn a_failed_write_leaves_text_selection_history_and_dirty_state_alone() {
    let service = MemoryFileService::new();
    let mut session = opened(&service, "a![](old.png)b");
    session.editor_mut().insert_text("X").unwrap();
    type_at_end(&mut session, "Y");
    let (before, selection, revision) = (
        text(&session),
        session.editor().selection(),
        session.revision(),
    );
    let dirty = session.is_dirty();
    let job = start_rewritten_save(&mut session);
    let outcome = session.finish_save(
        job.ticket,
        Err(SaveFailure::Io(std::io::Error::other("disk full"))),
    );
    assert!(matches!(outcome, SaveOutcome::Failed(_)));
    assert_eq!(text(&session), before);
    assert_eq!(session.editor().selection(), selection);
    assert_eq!(session.revision(), revision);
    assert_eq!(session.is_dirty(), dirty);
    assert_eq!(undo_all(&mut session), vec!["Xa![](old.png)b", "a![](old.png)b"]);

    // A later ordinary save is not rewritten by the failed attempt.
    let SaveDecision::Write(next) = session.request_save(SaveIntent::To(TARGET.into())) else {
        panic!("expected a write");
    };
    assert_eq!(next.document.full_text(), text(&session));
}

#[test]
fn an_edit_made_while_the_write_runs_survives_and_stays_unsaved() {
    let service = MemoryFileService::new();
    let mut session = opened(&service, "![](old.png)");
    let job = start_rewritten_save(&mut session);
    type_at_end(&mut session, "いま![](old.png)");
    let result = run_save_job(&service, &job.path, &job.document, job.guard);
    assert!(matches!(
        session.finish_save(job.ticket, result),
        SaveOutcome::SavedStale
    ));
    assert_eq!(text(&session), format!("![]({NEW})いま![]({NEW})"));
    assert_eq!(
        service.contents(TARGET).as_deref(),
        Some(format!("![]({NEW})").as_str())
    );
    assert!(session.is_dirty());
    assert_eq!(undo_all(&mut session), vec![format!("![]({NEW})")]);
}

#[test]
fn a_write_in_flight_refuses_the_rewrite_and_a_replaced_document_drops_it() {
    let service = MemoryFileService::new();
    let mut session = opened(&service, "![](old.png)");
    let SaveDecision::Write(running) = session.request_save(SaveIntent::Current) else {
        panic!("expected a write");
    };
    assert!(
        !session.arm_save_rewrite(rename_plan()),
        "a queued request could not carry the rewrite"
    );
    let result = run_save_job(&service, &running.path, &running.document, running.guard);
    assert!(matches!(
        session.finish_save(running.ticket, result),
        SaveOutcome::Saved
    ));

    let job = start_rewritten_save(&mut session);
    service.write_externally(PATH, "another document ![](old.png)");
    session.adopt(service.load(Path::new(PATH)).unwrap());
    let result = run_save_job(&service, &job.path, &job.document, job.guard);
    assert!(matches!(
        session.finish_save(job.ticket, result),
        SaveOutcome::Superseded
    ));
    assert_eq!(text(&session), "another document ![](old.png)");
    assert!(!session.disarm_save_rewrite(), "the write took the plan");
}
