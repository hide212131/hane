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
