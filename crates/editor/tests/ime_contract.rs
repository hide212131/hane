use hane_document::{SourceOffset, SourceRange};
use hane_editor::{Editor, EditorCommand, Selection};

fn char_boundaries(text: &str) -> impl Iterator<Item = usize> + '_ {
    (0..=text.len()).filter(|offset| text.is_char_boundary(*offset))
}

#[test]
fn every_utf8_character_boundary_round_trips_through_utf16() {
    for source in ["ASCII", "日本語", "A🙂羽", "e\u{301} and 👨‍👩‍👧‍👦"] {
        let editor = Editor::new(source);
        for offset in char_boundaries(source) {
            let utf16 = editor
                .source_range_to_utf16(SourceRange::empty(offset))
                .unwrap();
            assert_eq!(utf16.start, utf16.end);
            assert_eq!(
                editor.utf16_range_to_source(utf16).unwrap(),
                SourceRange::empty(offset),
                "UTF-8↔UTF-16 mismatch at {offset} in {source:?}"
            );
        }
    }
}

#[test]
fn ime_marked_range_selection_commit_and_undo_share_one_source_contract() {
    let mut editor = Editor::new("A **旧🙂** Z\nnext");
    let old_start = "A **".len();
    let old_end = old_start + "旧🙂".len();
    editor
        .set_selection(Selection {
            anchor: SourceOffset(old_start),
            active: SourceOffset(old_end),
        })
        .unwrap();

    editor
        .replace_and_mark_text(None, "日本🙂", Some(2..4))
        .unwrap();
    let ime = editor.ime().expect("composition must remain active");
    assert_eq!(ime.marked_text, "日本🙂");
    assert_eq!(
        ime.current_range,
        SourceRange::new(old_start, old_start + "日本🙂".len())
    );
    assert_eq!(ime.selected_utf16_range, 2..4);
    assert_eq!(
        editor.source_range_to_utf16(ime.current_range).unwrap(),
        old_start..old_start + 4
    );

    editor.commit_text(None, "日本語🙂").unwrap();
    assert!(editor.ime().is_none());
    assert_eq!(editor.document().full_text(), "A **日本語🙂** Z\nnext");
    editor.dispatch(EditorCommand::Undo).unwrap();
    assert_eq!(editor.document().full_text(), "A **旧🙂** Z\nnext");
    assert_eq!(
        editor.selection(),
        Selection {
            anchor: SourceOffset(old_start),
            active: SourceOffset(old_end)
        }
    );
}

#[test]
fn ime_commit_on_ordered_list_row_inserts_once_at_expected_offset() {
    let mut editor = Editor::new("3. Alpha row\n4. Beta row");
    let caret = "3. Alpha row".len();
    editor
        .set_selection(Selection::caret(SourceOffset(caret)))
        .unwrap();

    editor.replace_and_mark_text(None, "nihongo", None).unwrap();
    let ime = editor.ime().expect("composition must remain active");
    assert_eq!(ime.marked_text, "nihongo");
    assert_eq!(
        ime.current_range,
        SourceRange::new(caret, caret + "nihongo".len())
    );

    editor.commit_text(None, "日本語").unwrap();
    assert!(editor.ime().is_none());
    assert_eq!(
        editor.document().full_text(),
        "3. Alpha row日本語\n4. Beta row"
    );
    assert_eq!(
        editor.selection(),
        Selection::caret(SourceOffset(caret + "日本語".len()))
    );
}

#[test]
fn deferred_list_indentation_commits_and_undoes_with_nested_ime_text() {
    for (source, prefix) in [
        ("- abc\n  - xyz\n", "  "),
        ("10. abc\n    1. xyz\n", "    "),
    ] {
        let mut editor = Editor::new(source);
        let caret = source.trim_end_matches('\n').len();
        editor
            .set_selection(Selection::caret(SourceOffset(caret)))
            .unwrap();
        editor.dispatch(EditorCommand::Insert("\n")).unwrap();
        let empty_line = editor.selection().active;

        editor
            .replace_and_mark_text_with_prefix(None, prefix, "ni", Some(2..2))
            .unwrap();
        editor
            .replace_and_mark_text_with_prefix(None, "", "nihongo", Some(7..7))
            .unwrap();
        let content = source.trim_end_matches('\n');
        assert_eq!(
            editor.document().full_text(),
            format!("{content}\n{prefix}nihongo\n")
        );
        let ime = editor.ime().expect("composition must remain active");
        assert_eq!(
            ime.current_range,
            SourceRange::new(empty_line.0, empty_line.0 + prefix.len() + "nihongo".len())
        );
        assert_eq!(
            ime.marked_range,
            SourceRange::new(
                empty_line.0 + prefix.len(),
                empty_line.0 + prefix.len() + "nihongo".len()
            )
        );

        let marked_range = editor
            .ime()
            .and_then(|ime| editor.source_range_to_utf16(ime.marked_range).ok())
            .expect("marked text has a UTF-16 range");
        editor.commit_text(Some(marked_range), "日本語").unwrap();
        let committed = format!("{content}\n{prefix}日本語\n");
        assert_eq!(editor.document().full_text(), committed);
        editor.dispatch(EditorCommand::Undo).unwrap();
        assert_eq!(editor.document().full_text(), format!("{source}\n"));
        assert_eq!(editor.selection(), Selection::caret(empty_line));
        editor.dispatch(EditorCommand::Redo).unwrap();
        assert_eq!(editor.document().full_text(), committed);
    }
}

#[test]
fn deferred_list_indentation_is_removed_when_nested_ime_composition_is_cancelled() {
    for (source, prefix) in [
        ("- abc\n  - xyz\n", "  "),
        ("10. abc\n    1. xyz\n", "    "),
    ] {
        let mut editor = Editor::new(source);
        let caret = source.trim_end_matches('\n').len();
        editor
            .set_selection(Selection::caret(SourceOffset(caret)))
            .unwrap();
        editor.dispatch(EditorCommand::Insert("\n")).unwrap();
        let empty_line = editor.selection().active;
        editor
            .replace_and_mark_text_with_prefix(None, prefix, "nihongo", None)
            .unwrap();

        assert_eq!(
            editor.cancel_composition().unwrap(),
            hane_editor::ImeCancelOutcome::Restored
        );
        assert_eq!(editor.document().full_text(), format!("{source}\n"));
        assert_eq!(editor.selection(), Selection::caret(empty_line));
    }
}

#[test]
fn extended_vertical_selection_remains_on_source_boundaries() {
    let mut editor = Editor::new("日本🙂\n短い\ne\u{301}nd");
    editor
        .set_selection(Selection::caret(SourceOffset("日本".len())))
        .unwrap();
    editor
        .dispatch(EditorCommand::MoveDown { extend: true })
        .unwrap();
    editor
        .dispatch(EditorCommand::MoveDown { extend: true })
        .unwrap();

    let selection = editor.selection();
    let source = editor.document().full_text();
    assert!(source.is_char_boundary(selection.anchor.0));
    assert!(source.is_char_boundary(selection.active.0));
    assert!(!selection.range().is_empty());
}
