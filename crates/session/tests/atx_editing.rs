use hane_document::{Bias, LineId, SourceOffset, SourceRange, TextBuffer};
use hane_editor::{Editor, EditorCommand, Selection};
use hane_markdown::{BlockIndex, NodeKind};
use hane_presentation::{BlockKind, VisualOffset, present_markdown_with_disclosure};
use hane_session::testing::MemoryFileService;
use hane_session::{FileService, SaveDecision, SaveIntent, SaveOutcome, SessionSet, run_save_job};
use std::path::Path;

fn check(editor: &Editor, index: &mut BlockIndex, expected: &str, kind: NodeKind, visual: &str) {
    assert_eq!(editor.document().full_text(), expected);
    let deltas = editor.document().deltas_since(index.revision()).unwrap();
    index.update(editor.document(), &deltas);
    assert_eq!(index.block(0).unwrap().kind, kind);
    let fresh = BlockIndex::from_buffer(editor.document());
    assert_eq!(index.block(0).unwrap().kind, fresh.block(0).unwrap().kind);
    let range = editor.document().line_range(LineId(0)).unwrap();
    let source = editor.document().text(range).unwrap();
    let presented = present_markdown_with_disclosure(
        0,
        editor.document().revision(),
        range,
        &source,
        26.0,
        None,
    );
    assert_eq!(presented.visual_text, visual);
    let display = match kind {
        NodeKind::Heading(level) => BlockKind::Heading(level),
        _ => BlockKind::Paragraph,
    };
    assert_eq!(presented.kind, display);
}

#[test]
fn typing_and_removing_atx_markers_reparses_through_undo_and_redo() {
    let mut editor = Editor::new("羽");
    let mut index = BlockIndex::from_buffer(editor.document());
    for (text, expected, kind, visual) in [
        ("#", "#羽", NodeKind::Paragraph, "#羽"),
        (" ", "# 羽", NodeKind::Heading(1), "羽"),
    ] {
        editor.insert_text(text).unwrap();
        check(&editor, &mut index, expected, kind, visual);
    }
    editor
        .set_selection(Selection::caret(SourceOffset(0)))
        .unwrap();
    editor.insert_text("#####").unwrap();
    check(&editor, &mut index, "###### 羽", NodeKind::Heading(6), "羽");
    editor.insert_text("#").unwrap();
    check(
        &editor,
        &mut index,
        "####### 羽",
        NodeKind::Paragraph,
        "####### 羽",
    );
    editor.dispatch(EditorCommand::Backspace).unwrap();
    check(&editor, &mut index, "###### 羽", NodeKind::Heading(6), "羽");
    editor
        .set_selection(Selection::caret(SourceOffset(7)))
        .unwrap();
    editor.dispatch(EditorCommand::Backspace).unwrap();
    check(
        &editor,
        &mut index,
        "######羽",
        NodeKind::Paragraph,
        "######羽",
    );
    editor.dispatch(EditorCommand::Undo).unwrap();
    check(&editor, &mut index, "###### 羽", NodeKind::Heading(6), "羽");
    editor.dispatch(EditorCommand::Redo).unwrap();
    check(
        &editor,
        &mut index,
        "######羽",
        NodeKind::Paragraph,
        "######羽",
    );
}

#[test]
fn heading_drag_selection_ime_and_save_reopen_preserve_spelling() {
    let source = "##\t  旧🙂\t###\t";
    let service = MemoryFileService::new();
    let path = Path::new("/notes/atx.md");
    service.write_externally(path, source);
    let mut sessions = SessionSet::with_loaded(service.load(path).unwrap());
    // An explicit save of an untouched heading must preserve every source byte.
    save_and_assert(&mut sessions, &service, path, source);
    let editor = sessions.active_mut().editor_mut();
    let range = SourceRange::new(0, source.len());
    let block = present_markdown_with_disclosure(
        0,
        editor.document().revision(),
        range,
        source,
        26.0,
        None,
    );
    assert_eq!(block.visual_text, "旧🙂");
    // Same source mapping used by clicks and drag selection, with the endpoint
    // affinities preventing either closing or opening markup being selected.
    let anchor = block
        .source_map
        .visual_to_source(VisualOffset(0), Bias::After)
        .unwrap()
        .source_offset;
    let active = block
        .source_map
        .visual_to_source(VisualOffset("旧🙂".len()), Bias::Before)
        .unwrap()
        .source_offset;
    editor.set_selection(Selection { anchor, active }).unwrap();
    assert_eq!(editor.selected_text().unwrap(), "旧🙂");
    editor
        .replace_and_mark_text(None, "にほん", Some(0..3))
        .unwrap();
    editor
        .replace_and_mark_text(None, "日本語", Some(0..3))
        .unwrap();
    editor.commit_text(None, "日本語🙂").unwrap();
    let edited = "##\t  日本語🙂\t###\t";
    assert_eq!(editor.document().full_text(), edited);
    editor.dispatch(EditorCommand::Undo).unwrap();
    assert_eq!(editor.document().full_text(), source);
    editor.dispatch(EditorCommand::Redo).unwrap();
    assert_eq!(editor.document().full_text(), edited);
    sessions.active_mut().note_edit();
    save_and_assert(&mut sessions, &service, path, edited);
    let reopened = service.load(path).unwrap();
    assert_eq!(reopened.document.full_text(), edited);
    assert_eq!(
        BlockIndex::from_buffer(&reopened.document)
            .block(0)
            .unwrap()
            .kind,
        NodeKind::Heading(2)
    );
}

fn save_and_assert(
    sessions: &mut SessionSet,
    service: &MemoryFileService,
    path: &Path,
    expected: &str,
) {
    let session = sessions.active_mut();
    let SaveDecision::Write(job) = session.request_save(SaveIntent::Current) else {
        panic!("save must be accepted")
    };
    let result = run_save_job(service, &job.path, &job.document, job.guard);
    assert!(matches!(
        session.finish_save(job.ticket, result),
        SaveOutcome::Saved
    ));
    assert_eq!(service.contents(path).as_deref(), Some(expected));
}

#[test]
fn changing_heading_levels_keeps_large_document_updates_local() {
    let mut editor = Editor::new(&"## 羽 ###\n\n".repeat(20_000));
    let mut index = BlockIndex::from_buffer(editor.document());
    let middle = index.block(10_000).unwrap();
    let far_id = index.block(19_999).unwrap().id;
    editor
        .set_selection(Selection::caret(middle.source_range.start))
        .unwrap();
    let base = editor.document().revision();
    editor.insert_text("#").unwrap();
    let update = index.update(
        editor.document(),
        &editor.document().deltas_since(base).unwrap(),
    );
    assert!(update.resynchronized);
    assert!(update.reparsed_bytes < 1_000);
    assert_eq!(update.invalidated_blocks, 0);
    assert_eq!(index.block(10_000).unwrap().kind, NodeKind::Heading(3));
    assert_eq!(index.block(19_999).unwrap().id, far_id);
}
