//! Cursor movement and deletion over pinned grapheme boundaries.
//!
//! The expected boundaries below are byte offsets written out by hand, not
//! values asked of the segmentation library. `unicode-segmentation` was updated
//! (1.12.0 to 1.13.3, Unicode 16 to 17) together with the Mermaid dependency, so
//! the body's movement, selection and deletion over Japanese text, combining
//! marks, joined and regional-indicator emoji, Hangul jamo and CRLF are pinned
//! independently of it.

use hane_document::{SourceOffset, TextBuffer};
use hane_editor::{Editor, EditorCommand, Selection};

/// Text and the byte offsets of every cursor position inside it.
const PINNED: &[(&str, &[usize])] = &[
    ("日本語", &[0, 3, 6, 9]),
    ("e\u{301}x", &[0, 3, 4]),
    ("👨\u{200d}👩\u{200d}👧\u{200d}👦", &[0, 25]),
    ("🇯🇵🇺🇸", &[0, 8, 16]),
    ("👍🏽", &[0, 8]),
    ("か\u{3099}き", &[0, 6, 9]),
    ("☺\u{fe0f}!", &[0, 6, 7]),
    ("1\u{fe0f}\u{20e3}", &[0, 7]),
    ("\u{1112}\u{1161}\u{11ab}", &[0, 9]),
    ("a\r\nb", &[0, 1, 3, 4]),
];

fn editor_at(text: &str, offset: usize) -> Editor {
    let mut editor = Editor::new(text);
    editor
        .set_selection(Selection::caret(SourceOffset(offset)))
        .unwrap();
    editor
}

fn text_of(editor: &Editor) -> String {
    let document = editor.document();
    document
        .text(hane_document::SourceRange::new(0, document.len_bytes().0))
        .unwrap()
}

#[test]
fn arrows_visit_exactly_the_pinned_boundaries_in_both_directions() {
    for (text, boundaries) in PINNED {
        let mut editor = editor_at(text, 0);
        let mut forward = vec![0];
        loop {
            let before = editor.selection().active.0;
            editor
                .dispatch(EditorCommand::MoveRight { extend: false })
                .unwrap();
            let after = editor.selection().active.0;
            if after == before {
                break;
            }
            forward.push(after);
        }
        assert_eq!(&forward, boundaries, "right over {text:?}");

        let mut backward = vec![text.len()];
        loop {
            let before = editor.selection().active.0;
            editor
                .dispatch(EditorCommand::MoveLeft { extend: false })
                .unwrap();
            let after = editor.selection().active.0;
            if after == before {
                break;
            }
            backward.push(after);
        }
        backward.reverse();
        assert_eq!(&backward, boundaries, "left over {text:?}");
    }
}

#[test]
fn shift_arrows_select_whole_pinned_clusters() {
    for (text, boundaries) in PINNED {
        let mut editor = editor_at(text, 0);
        editor
            .dispatch(EditorCommand::MoveRight { extend: true })
            .unwrap();
        assert_eq!(
            editor.selection(),
            Selection {
                anchor: SourceOffset(0),
                active: SourceOffset(boundaries[1]),
            },
            "shift+right at the start of {text:?}"
        );

        let mut editor = editor_at(text, text.len());
        editor
            .dispatch(EditorCommand::MoveLeft { extend: true })
            .unwrap();
        assert_eq!(
            editor.selection(),
            Selection {
                anchor: SourceOffset(text.len()),
                active: SourceOffset(boundaries[boundaries.len() - 2]),
            },
            "shift+left at the end of {text:?}"
        );
    }
}

#[test]
fn backspace_and_delete_remove_one_pinned_cluster() {
    for (text, boundaries) in PINNED {
        let mut editor = editor_at(text, text.len());
        editor.dispatch(EditorCommand::Backspace).unwrap();
        assert_eq!(
            text_of(&editor),
            text[..boundaries[boundaries.len() - 2]],
            "backspace at the end of {text:?}"
        );

        let mut editor = editor_at(text, 0);
        editor.dispatch(EditorCommand::Delete).unwrap();
        assert_eq!(
            text_of(&editor),
            text[boundaries[1]..],
            "delete at the start of {text:?}"
        );
    }
}

#[test]
fn a_column_counts_pinned_clusters_when_moving_between_lines() {
    for (text, boundaries) in PINNED {
        if text.contains('\r') {
            continue;
        }
        // The caret sits after the first cluster of the first line and goes down
        // to a second line made of the same clusters: it lands after the first
        // cluster there too, not after the first scalar.
        let doubled = format!("{text}\n{text}");
        let mut editor = editor_at(&doubled, boundaries[1]);
        editor
            .dispatch(EditorCommand::MoveDown { extend: false })
            .unwrap();
        assert_eq!(
            editor.selection().active.0,
            text.len() + 1 + boundaries[1],
            "down over {text:?}"
        );
    }
}
