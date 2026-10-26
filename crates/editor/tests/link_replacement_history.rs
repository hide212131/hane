//! A link inserted over a selection goes through `replace_range_recorded`, so it
//! is its own undo step between ordinary typing and restores the original
//! directional selection.

use hane_document::{SourceOffset, SourceRange};
use hane_editor::{Editor, EditorCommand, Selection};

fn text(editor: &Editor) -> String {
    editor.document().full_text()
}

#[test]
fn link_replacement_is_a_separate_undo_step_between_typing() {
    let mut editor = Editor::new("abc def");
    editor.set_selection(Selection::caret(SourceOffset(7))).unwrap();
    editor.insert_text("X").unwrap();
    assert_eq!(text(&editor), "abc defX");

    let reversed = Selection {
        anchor: SourceOffset(3),
        active: SourceOffset(0),
    };
    editor.set_selection(reversed).unwrap();
    let replacement = "[abc](https://example.com/)";
    let after = Selection::caret(SourceOffset(replacement.len()));
    editor
        .replace_range_recorded(SourceRange::new(0, 3), replacement, after)
        .unwrap();
    assert_eq!(text(&editor), "[abc](https://example.com/) defX");
    assert_eq!(editor.selection(), after);

    // Typing right after the link does not merge into the link step.
    editor.insert_text("Y").unwrap();
    assert_eq!(text(&editor), "[abc](https://example.com/)Y defX");

    editor.dispatch(EditorCommand::Undo).unwrap();
    assert_eq!(text(&editor), "[abc](https://example.com/) defX");
    assert_eq!(editor.selection(), after);

    editor.dispatch(EditorCommand::Undo).unwrap();
    assert_eq!(text(&editor), "abc defX");
    assert_eq!(editor.selection(), reversed);

    editor.dispatch(EditorCommand::Undo).unwrap();
    assert_eq!(text(&editor), "abc def");
    assert!(!editor.can_undo());

    editor.dispatch(EditorCommand::Redo).unwrap();
    editor.dispatch(EditorCommand::Redo).unwrap();
    assert_eq!(text(&editor), "[abc](https://example.com/) defX");
    assert_eq!(editor.selection(), after);
    editor.dispatch(EditorCommand::Redo).unwrap();
    assert_eq!(text(&editor), "[abc](https://example.com/)Y defX");
    assert!(!editor.can_redo());
}

#[test]
fn a_refused_replacement_changes_nothing() {
    let mut editor = Editor::new("あいう");
    let selection = Selection {
        anchor: SourceOffset(0),
        active: SourceOffset(3),
    };
    editor.set_selection(selection).unwrap();
    // Not on a character boundary: refused, with text, selection and history
    // untouched.
    assert!(
        editor
            .replace_range_recorded(
                SourceRange::new(1, 3),
                "[x](https://example.com/)",
                Selection::caret(SourceOffset(0)),
            )
            .is_err()
    );
    assert_eq!(text(&editor), "あいう");
    assert_eq!(editor.selection(), selection);
    assert!(!editor.can_undo());
}
