#![allow(
    clippy::map_unwrap_or,
    reason = "the assertion keeps its mapped fallback explicit"
)]

use hane_document::SourceOffset;
use hane_editor::{Editor, EditorCommand, Selection};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy, Debug)]
enum Arrow {
    Left,
    Right,
    Up,
    Down,
}

impl Arrow {
    fn command(self) -> EditorCommand<'static> {
        match self {
            Self::Left => EditorCommand::MoveLeft { extend: false },
            Self::Right => EditorCommand::MoveRight { extend: false },
            Self::Up => EditorCommand::MoveUp { extend: false },
            Self::Down => EditorCommand::MoveDown { extend: false },
        }
    }
}

fn split_cursor(marked: &str) -> (String, usize) {
    assert_eq!(
        marked.matches('|').count(),
        1,
        "scenario must contain exactly one cursor marker: {marked:?}"
    );
    let offset = marked.find('|').unwrap();
    (marked.replacen('|', "", 1), offset)
}

fn assert_arrow(case: &str, initial: &str, arrow: Arrow, expected: &str) {
    let (initial_text, initial_offset) = split_cursor(initial);
    let (expected_text, expected_offset) = split_cursor(expected);
    assert_eq!(initial_text, expected_text, "invalid scenario {case}");

    let mut editor = Editor::new(&initial_text);
    editor
        .set_selection(Selection::caret(SourceOffset(initial_offset)))
        .unwrap();
    editor.dispatch(arrow.command()).unwrap();

    assert_eq!(
        editor.selection(),
        Selection::caret(SourceOffset(expected_offset)),
        "cursor mismatch in {case}: {arrow:?} from {initial:?}"
    );
}

fn grapheme_boundaries(text: &str, start: usize, end: usize) -> Vec<usize> {
    let mut boundaries: Vec<_> = text[start..end]
        .grapheme_indices(true)
        .map(|(offset, _)| start + offset)
        .collect();
    boundaries.push(end);
    boundaries.sort_unstable();
    boundaries.dedup();
    boundaries
}

fn line_boundaries(text: &str) -> Vec<Vec<usize>> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (newline, _) in text.match_indices('\n') {
        let content_end = if text[start..newline].ends_with('\r') {
            newline - 1
        } else {
            newline
        };
        lines.push(grapheme_boundaries(text, start, content_end));
        start = newline + 1;
    }
    lines.push(grapheme_boundaries(text, start, text.len()));
    lines
}

#[test]
fn horizontal_arrows_at_line_and_document_boundaries() {
    let cases = [
        ("left at document start", "|abc", Arrow::Left, "|abc"),
        ("right at document start", "|abc", Arrow::Right, "a|bc"),
        ("left at document end", "abc|", Arrow::Left, "ab|c"),
        ("right at document end", "abc|", Arrow::Right, "abc|"),
        (
            "left at LF line start",
            "abc\n|def",
            Arrow::Left,
            "abc|\ndef",
        ),
        (
            "right at LF line end",
            "abc|\ndef",
            Arrow::Right,
            "abc\n|def",
        ),
        (
            "right at LF line start",
            "abc\n|def",
            Arrow::Right,
            "abc\nd|ef",
        ),
        ("left at LF line end", "abc|\ndef", Arrow::Left, "ab|c\ndef"),
        (
            "left at CRLF line start",
            "abc\r\n|def",
            Arrow::Left,
            "abc|\r\ndef",
        ),
        (
            "right at CRLF line end",
            "abc|\r\ndef",
            Arrow::Right,
            "abc\r\n|def",
        ),
        (
            "left after an empty line",
            "abc\n\n|def",
            Arrow::Left,
            "abc\n|\ndef",
        ),
        (
            "right from an empty line",
            "abc\n|\ndef",
            Arrow::Right,
            "abc\n\n|def",
        ),
        (
            "left after a Japanese line",
            "あい\n|う",
            Arrow::Left,
            "あい|\nう",
        ),
        (
            "left after a combining grapheme",
            "a\u{301}\n|b",
            Arrow::Left,
            "a\u{301}|\nb",
        ),
        (
            "left at end after trailing LF",
            "abc\n|",
            Arrow::Left,
            "abc|\n",
        ),
        (
            "left at end after trailing CRLF",
            "abc\r\n|",
            Arrow::Left,
            "abc|\r\n",
        ),
    ];

    for (name, initial, arrow, expected) in cases {
        assert_arrow(name, initial, arrow, expected);
    }
}

#[test]
fn vertical_arrows_at_line_starts() {
    let cases = [
        ("up at first line", "|abc\ndef", Arrow::Up, "|abc\ndef"),
        ("up at first line end", "abc|\ndef", Arrow::Up, "abc|\ndef"),
        (
            "down from line start",
            "|abc\ndef",
            Arrow::Down,
            "abc\n|def",
        ),
        ("up from line start", "abc\n|def", Arrow::Up, "|abc\ndef"),
        (
            "down into empty line",
            "|abc\n\ndef",
            Arrow::Down,
            "abc\n|\ndef",
        ),
        (
            "down from empty line",
            "abc\n|\ndef",
            Arrow::Down,
            "abc\n\n|def",
        ),
        (
            "up from empty line",
            "abc\n|\ndef",
            Arrow::Up,
            "|abc\n\ndef",
        ),
        (
            "down from CRLF line start",
            "|abc\r\ndef",
            Arrow::Down,
            "abc\r\n|def",
        ),
        (
            "down at final line start",
            "abc\n|def",
            Arrow::Down,
            "abc\n|def",
        ),
    ];

    for (name, initial, arrow, expected) in cases {
        assert_arrow(name, initial, arrow, expected);
    }
}

#[test]
fn vertical_arrows_at_line_ends() {
    let cases = [
        (
            "down to same column",
            "abc|\nabcdef",
            Arrow::Down,
            "abc\nabc|def",
        ),
        (
            "up to same column",
            "abcdef\nabc|def",
            Arrow::Up,
            "abc|def\nabcdef",
        ),
        (
            "down clamps to shorter line",
            "abc|\nx",
            Arrow::Down,
            "abc\nx|",
        ),
        ("up clamps to shorter line", "x\nabc|", Arrow::Up, "x|\nabc"),
        (
            "down from Japanese line end",
            "あい|\nうえお",
            Arrow::Down,
            "あい\nうえ|お",
        ),
        (
            "down counts a combining sequence as one column",
            "a\u{301}|\nxy",
            Arrow::Down,
            "a\u{301}\nx|y",
        ),
        (
            "down from CRLF line end",
            "abc|\r\ndefghi",
            Arrow::Down,
            "abc\r\ndef|ghi",
        ),
        ("down at final line", "abc\ndef|", Arrow::Down, "abc\ndef|"),
        (
            "down to trailing empty line",
            "abc|\n",
            Arrow::Down,
            "abc\n|",
        ),
    ];

    for (name, initial, arrow, expected) in cases {
        assert_arrow(name, initial, arrow, expected);
    }
}

#[test]
fn every_arrow_is_a_no_op_in_an_empty_document() {
    for arrow in [Arrow::Left, Arrow::Right, Arrow::Up, Arrow::Down] {
        assert_arrow("empty document", "|", arrow, "|");
    }
}

#[test]
fn horizontal_arrows_visit_every_grapheme_boundary() {
    let fixtures = [
        "",
        "abc",
        "abc\ndef",
        "abc\r\ndef\r\n",
        "\n\n",
        "あい\nうえお",
        "a\u{301}🙂\n羽\r単独\u{2028}行",
    ];

    for text in fixtures {
        let expected = grapheme_boundaries(text, 0, text.len());
        let mut editor = Editor::new(text);
        let mut visited = vec![0];
        loop {
            let before = editor.selection().active.0;
            editor
                .dispatch(EditorCommand::MoveRight { extend: false })
                .unwrap();
            let after = editor.selection().active.0;
            if after == before {
                break;
            }
            visited.push(after);
        }
        assert_eq!(visited, expected, "right traversal mismatch for {text:?}");

        for pair in expected.windows(2) {
            editor
                .set_selection(Selection::caret(SourceOffset(pair[0])))
                .unwrap();
            editor
                .dispatch(EditorCommand::MoveRight { extend: false })
                .unwrap();
            assert_eq!(editor.selection().active.0, pair[1], "right in {text:?}");

            editor
                .dispatch(EditorCommand::MoveLeft { extend: false })
                .unwrap();
            assert_eq!(editor.selection().active.0, pair[0], "left in {text:?}");
        }
    }
}

#[test]
fn vertical_arrows_map_every_line_column() {
    let fixtures = [
        "",
        "abc",
        "abc\ndefghi\nx",
        "abc\r\ndef\r\n",
        "\n\n",
        "あいう\nえ\nおかき",
        "a\u{301}🙂\nx\n羽\r単独\u{2028}行",
    ];

    for text in fixtures {
        let lines = line_boundaries(text);
        for (line_index, source_line) in lines.iter().enumerate() {
            for (column, &source) in source_line.iter().enumerate() {
                for (arrow, target_line_index) in [
                    (Arrow::Up, line_index.checked_sub(1)),
                    (
                        Arrow::Down,
                        (line_index + 1 < lines.len()).then_some(line_index + 1),
                    ),
                ] {
                    let expected = target_line_index
                        .map(|target| {
                            let target_line = &lines[target];
                            target_line[column.min(target_line.len() - 1)]
                        })
                        .unwrap_or(source);
                    let mut editor = Editor::new(text);
                    editor
                        .set_selection(Selection::caret(SourceOffset(source)))
                        .unwrap();
                    editor.dispatch(arrow.command()).unwrap();
                    assert_eq!(
                        editor.selection().active.0,
                        expected,
                        "{arrow:?} mismatch at line {line_index}, column {column} in {text:?}"
                    );
                }
            }
        }
    }
}
