//! Issue #494: word / paragraph movement and word deletion reach the same
//! offsets the OS-standard keys document, without ever stopping inside a
//! grapheme or a CRLF.

use hane_document::SourceOffset;
use hane_editor::{Editor, EditorCommand, ForwardStop, Selection};
use unicode_segmentation::UnicodeSegmentation;

fn editor_at(text: &str, offset: usize) -> Editor {
    let mut editor = Editor::new(text);
    editor
        .set_selection(Selection::caret(SourceOffset(offset)))
        .unwrap();
    editor
}

fn word(editor: &mut Editor, forward: bool, stop: ForwardStop, extend: bool) {
    editor
        .dispatch(EditorCommand::MoveWord {
            forward,
            stop,
            extend,
        })
        .unwrap();
}

fn paragraph(editor: &mut Editor, forward: bool, stop: ForwardStop, extend: bool) {
    editor
        .dispatch(EditorCommand::MoveParagraph {
            forward,
            stop,
            extend,
        })
        .unwrap();
}

fn active(editor: &Editor) -> usize {
    editor.selection().active.0
}

/// Offsets reached by repeating a move from `start`, until it stops moving.
fn walk(text: &str, start: usize, step: impl Fn(&mut Editor)) -> Vec<usize> {
    let mut editor = editor_at(text, start);
    let mut reached = vec![start];
    for _ in 0..text.len() + 2 {
        step(&mut editor);
        let now = active(&editor);
        if reached.last() == Some(&now) {
            return reached;
        }
        reached.push(now);
    }
    panic!("movement kept going: {reached:?}");
}

#[test]
fn word_forward_stops_at_the_word_end_on_mac_and_next_start_on_windows() {
    let mac = walk("one two", 0, |e| word(e, true, ForwardStop::End, false));
    assert_eq!(mac, [0, 3, 7]);
    let windows = walk("one two", 0, |e| {
        word(e, true, ForwardStop::NextStart, false);
    });
    assert_eq!(windows, [0, 4, 7]);

    let mac = walk("one, two", 0, |e| word(e, true, ForwardStop::End, false));
    assert_eq!(mac, [0, 3, 8]);
    let windows = walk("one, two", 0, |e| {
        word(e, true, ForwardStop::NextStart, false);
    });
    assert_eq!(windows, [0, 5, 8]);
}

#[test]
fn word_backward_is_shared_by_both_platforms() {
    for stop in [ForwardStop::End, ForwardStop::NextStart] {
        assert_eq!(
            walk("one two", 7, |e| word(e, false, stop, false)),
            [7, 4, 0]
        );
        assert_eq!(
            walk("one, two", 8, |e| word(e, false, stop, false)),
            [8, 5, 0]
        );
    }
}

#[test]
fn word_moves_from_mid_word_and_across_line_breaks_without_entering_crlf() {
    let mut editor = editor_at("hello world", 2);
    word(&mut editor, true, ForwardStop::End, false);
    assert_eq!(active(&editor), 5);
    let mut editor = editor_at("hello world", 8);
    word(&mut editor, false, ForwardStop::End, false);
    assert_eq!(active(&editor), 6);

    // Mac: 0 -> end of "a", then over the CRLF to the end of "b".
    let text = "a\r\nb";
    assert_eq!(
        walk(text, 0, |e| word(e, true, ForwardStop::End, false)),
        [0, 1, 4]
    );
    assert_eq!(
        walk(text, 0, |e| word(e, true, ForwardStop::NextStart, false)),
        [0, 3, 4]
    );
    assert_eq!(
        walk(text, 4, |e| word(e, false, ForwardStop::End, false)),
        [4, 3, 0]
    );

    // Bare CR and an empty line still advance.
    let text = "a\r\rb";
    for stop in [ForwardStop::End, ForwardStop::NextStart] {
        let reached = walk(text, 0, |e| word(e, true, stop, false));
        assert_eq!(reached.last(), Some(&text.len()));
        assert!(reached.windows(2).all(|pair| pair[0] < pair[1]));
    }
}

#[test]
fn word_moves_never_split_graphemes_for_cjk_combining_and_family_emoji() {
    let text = "日本語 e\u{301} 👨\u{200d}👩\u{200d}👧\u{200d}👦 x!!";
    let boundaries: Vec<usize> = text
        .grapheme_indices(true)
        .map(|(index, _)| index)
        .chain(std::iter::once(text.len()))
        .collect();
    for stop in [ForwardStop::End, ForwardStop::NextStart] {
        let forward = walk(text, 0, |e| word(e, true, stop, false));
        assert_eq!(forward.last(), Some(&text.len()));
        assert!(forward.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(forward.iter().all(|offset| boundaries.contains(offset)));

        let backward = walk(text, text.len(), |e| word(e, false, stop, false));
        assert_eq!(backward.last(), Some(&0));
        assert!(backward.windows(2).all(|pair| pair[0] > pair[1]));
        assert!(backward.iter().all(|offset| boundaries.contains(offset)));
    }
}

#[test]
fn symbol_and_emoji_only_text_still_makes_progress() {
    for text in ["!!! ???", "👨\u{200d}👩\u{200d}👧\u{200d}👦🙂"] {
        for stop in [ForwardStop::End, ForwardStop::NextStart] {
            let forward = walk(text, 0, |e| word(e, true, stop, false));
            assert_eq!(forward.last(), Some(&text.len()), "{text:?}");
            let backward = walk(text, text.len(), |e| word(e, false, stop, false));
            assert_eq!(backward.last(), Some(&0), "{text:?}");
        }
    }
}

#[test]
fn shift_word_keeps_the_anchor_and_plain_word_collapses_a_selection_to_its_edge() {
    let mut editor = editor_at("one two three", 0);
    word(&mut editor, true, ForwardStop::End, true);
    word(&mut editor, true, ForwardStop::End, true);
    assert_eq!(
        editor.selection(),
        Selection {
            anchor: SourceOffset(0),
            active: SourceOffset(7)
        }
    );
    word(&mut editor, false, ForwardStop::End, true);
    assert_eq!(editor.selection().anchor, SourceOffset(0));
    assert_eq!(active(&editor), 4);

    // Forward collapses to the end, without a further word step.
    let mut editor = editor_at("one two three", 0);
    editor
        .set_selection(Selection {
            anchor: SourceOffset(0),
            active: SourceOffset(3),
        })
        .unwrap();
    word(&mut editor, true, ForwardStop::End, false);
    assert_eq!(editor.selection(), Selection::caret(SourceOffset(3)));

    // Backward collapses a reversed selection to its start.
    editor
        .set_selection(Selection {
            anchor: SourceOffset(7),
            active: SourceOffset(4),
        })
        .unwrap();
    word(&mut editor, false, ForwardStop::End, false);
    assert_eq!(editor.selection(), Selection::caret(SourceOffset(4)));
}

#[test]
fn paragraph_moves_follow_source_lines_with_the_documented_stops() {
    let text = "one\ntwo\nlast";
    assert_eq!(
        walk(text, 0, |e| paragraph(e, true, ForwardStop::NextStart, false)),
        [0, 4, 8, 12]
    );
    assert_eq!(
        walk(text, 0, |e| paragraph(e, true, ForwardStop::End, false)),
        [0, 3, 7, 12]
    );
    for stop in [ForwardStop::End, ForwardStop::NextStart] {
        assert_eq!(
            walk(text, 9, |e| paragraph(e, false, stop, false)),
            [9, 8, 4, 0]
        );
    }
}

#[test]
fn paragraph_moves_cross_blank_lines_crlf_and_stop_at_document_edges() {
    let text = "a\n\n\nb";
    assert_eq!(
        walk(text, 0, |e| paragraph(e, true, ForwardStop::NextStart, false)),
        [0, 2, 3, 4, 5]
    );
    assert_eq!(
        walk(text, 0, |e| paragraph(e, true, ForwardStop::End, false)),
        [0, 1, 2, 3, 5]
    );
    assert_eq!(
        walk(text, 5, |e| paragraph(e, false, ForwardStop::End, false)),
        [5, 4, 3, 2, 0]
    );

    let text = "a\r\nb";
    assert_eq!(
        walk(text, 0, |e| paragraph(e, true, ForwardStop::NextStart, false)),
        [0, 3, 4]
    );
    assert_eq!(
        walk(text, 0, |e| paragraph(e, true, ForwardStop::End, false)),
        [0, 1, 4]
    );
    assert_eq!(
        walk(text, 4, |e| paragraph(e, false, ForwardStop::End, false)),
        [4, 3, 0]
    );

    let empty = walk("", 0, |e| paragraph(e, true, ForwardStop::End, false));
    assert_eq!(empty, [0]);
}

#[test]
fn shift_paragraph_extends_and_plain_paragraph_collapses_the_selection() {
    let mut editor = editor_at("one\ntwo\nlast", 1);
    paragraph(&mut editor, true, ForwardStop::NextStart, true);
    assert_eq!(
        editor.selection(),
        Selection {
            anchor: SourceOffset(1),
            active: SourceOffset(4)
        }
    );
    paragraph(&mut editor, false, ForwardStop::NextStart, false);
    assert_eq!(editor.selection(), Selection::caret(SourceOffset(1)));
}

#[test]
fn word_delete_is_one_undo_step_and_restores_text_and_selection() {
    let mut editor = editor_at("one two", 7);
    let summary = editor
        .dispatch(EditorCommand::DeleteWord {
            forward: false,
            stop: ForwardStop::End,
        })
        .unwrap();
    assert!(summary.is_some());
    assert_eq!(editor.document().full_text(), "one ");
    assert_eq!(editor.selection(), Selection::caret(SourceOffset(4)));

    editor.dispatch(EditorCommand::Undo).unwrap();
    assert_eq!(editor.document().full_text(), "one two");
    assert_eq!(editor.selection(), Selection::caret(SourceOffset(7)));
    editor.dispatch(EditorCommand::Redo).unwrap();
    assert_eq!(editor.document().full_text(), "one ");
    assert_eq!(editor.selection(), Selection::caret(SourceOffset(4)));
}

#[test]
fn word_delete_forward_uses_the_platform_stop() {
    let mut mac = editor_at("one two", 0);
    mac.dispatch(EditorCommand::DeleteWord {
        forward: true,
        stop: ForwardStop::End,
    })
    .unwrap();
    assert_eq!(mac.document().full_text(), " two");

    let mut windows = editor_at("one two", 0);
    windows
        .dispatch(EditorCommand::DeleteWord {
            forward: true,
            stop: ForwardStop::NextStart,
        })
        .unwrap();
    assert_eq!(windows.document().full_text(), "two");
}

#[test]
fn word_delete_with_a_selection_deletes_only_the_selection_and_undo_restores_it() {
    let mut editor = editor_at("one two three", 0);
    let selection = Selection {
        anchor: SourceOffset(7),
        active: SourceOffset(4),
    };
    editor.set_selection(selection).unwrap();
    editor
        .dispatch(EditorCommand::DeleteWord {
            forward: true,
            stop: ForwardStop::NextStart,
        })
        .unwrap();
    assert_eq!(editor.document().full_text(), "one  three");
    assert_eq!(editor.selection(), Selection::caret(SourceOffset(4)));
    editor.dispatch(EditorCommand::Undo).unwrap();
    assert_eq!(editor.document().full_text(), "one two three");
    assert_eq!(editor.selection(), selection);
}

#[test]
fn word_delete_does_not_merge_with_following_character_deletes() {
    let mut editor = editor_at("one two", 7);
    editor
        .dispatch(EditorCommand::DeleteWord {
            forward: false,
            stop: ForwardStop::End,
        })
        .unwrap();
    editor.dispatch(EditorCommand::Backspace).unwrap();
    assert_eq!(editor.document().full_text(), "one");
    editor.dispatch(EditorCommand::Undo).unwrap();
    assert_eq!(editor.document().full_text(), "one ");
    editor.dispatch(EditorCommand::Undo).unwrap();
    assert_eq!(editor.document().full_text(), "one two");
}

#[test]
fn empty_word_delete_leaves_text_and_history_alone() {
    let mut editor = editor_at("", 0);
    for forward in [false, true] {
        let summary = editor
            .dispatch(EditorCommand::DeleteWord {
                forward,
                stop: ForwardStop::End,
            })
            .unwrap();
        assert!(summary.is_none());
    }
    assert!(!editor.can_undo());
    assert_eq!(editor.document().full_text(), "");
}
