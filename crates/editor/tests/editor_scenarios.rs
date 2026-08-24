use hane_editor::{Editor, EditorCommand};

fn move_right(editor: &mut Editor, times: usize) {
    for _ in 0..times {
        editor
            .dispatch(EditorCommand::MoveRight { extend: false })
            .unwrap();
    }
}

#[test]
fn moving_down_then_inserting_text_edits_the_expected_column() {
    let mut editor = Editor::new("abc\nあいう");
    move_right(&mut editor, 2);
    editor
        .dispatch(EditorCommand::MoveDown { extend: false })
        .unwrap();

    editor.insert_text("X").unwrap();

    assert_eq!(editor.document().full_text(), "abc\nあいXう");
}

#[test]
fn vertical_movement_keeps_the_preferred_column_across_a_short_line() {
    let mut editor = Editor::new("abcd\nx\nwxyz");
    move_right(&mut editor, 3);
    editor
        .dispatch(EditorCommand::MoveDown { extend: false })
        .unwrap();
    editor
        .dispatch(EditorCommand::MoveDown { extend: false })
        .unwrap();

    editor.insert_text("X").unwrap();

    assert_eq!(editor.document().full_text(), "abcd\nx\nwxyXz");
}
