//! Front Matter of a Markdown source, found without evaluating the YAML.

use hane_document::SourceRange;

/// The closing line must end inside this many Unicode scalar values.
pub const SEARCH_CHARS: usize = 8_192;
/// The inner raw text may not exceed this many Unicode scalar values.
pub const MAX_INNER_CHARS: usize = 2_048;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FrontMatter {
    Absent,
    /// An opening line exists but the block cannot be read safely: no closing
    /// line in the first [`SEARCH_CHARS`] scalars, or an inner text that is too
    /// large.
    Unavailable,
    Present {
        /// The raw text between the delimiter lines, line breaks included.
        inner: String,
        /// From the opening `---` to the end of the closing line, without its
        /// line break.
        range: SourceRange,
    },
}

/// A line of `text` starting at `from`: its content and the offset after its
/// terminator (LF, CRLF or a bare CR). `None` at the end of the text.
fn next_line(text: &str, from: usize) -> Option<(&str, usize, usize)> {
    if from >= text.len() {
        return None;
    }
    let rest = &text[from..];
    match rest.find(['\n', '\r']) {
        None => Some((rest, from + rest.len(), from + rest.len())),
        Some(at) => {
            let terminator = if rest[at..].starts_with("\r\n") { 2 } else { 1 };
            Some((&rest[..at], from + at, from + at + terminator))
        }
    }
}

/// Looks for Front Matter at the start of a document. `head` is the beginning
/// of the document, at least [`SEARCH_CHARS`] scalar values of it when it is
/// that long; the function only reads that much. A BOM before the opening line
/// is allowed.
pub fn extract(head: &str) -> FrontMatter {
    let head: &str = match head.char_indices().nth(SEARCH_CHARS) {
        Some((end, _)) => &head[..end],
        None => head,
    };
    let bom = if head.starts_with('\u{feff}') {
        '\u{feff}'.len_utf8()
    } else {
        0
    };
    let Some((first, _, mut cursor)) = next_line(head, bom) else {
        return FrontMatter::Absent;
    };
    if first != "---" {
        return FrontMatter::Absent;
    }
    let inner_start = cursor;
    while let Some((line, line_end, after)) = next_line(head, cursor) {
        if line == "---" || line == "..." {
            let inner = &head[inner_start..cursor];
            if inner.chars().count() > MAX_INNER_CHARS {
                return FrontMatter::Unavailable;
            }
            return FrontMatter::Present {
                inner: inner.to_owned(),
                range: SourceRange::new(bom, line_end),
            };
        }
        cursor = after;
    }
    // An opening line with no closing line inside the search window. When the
    // text simply ends the same answer is given: it cannot be told apart from a
    // block that continues beyond the window.
    FrontMatter::Unavailable
}

#[cfg(test)]
mod tests {
    use super::*;

    fn present(inner: &str, start: usize, end: usize) -> FrontMatter {
        FrontMatter::Present {
            inner: inner.to_owned(),
            range: SourceRange::new(start, end),
        }
    }

    #[test]
    fn finds_a_block_closed_by_dashes_or_dots() {
        assert_eq!(
            extract("---\ntitle: a\n---\nbody"),
            present("title: a\n", 0, 16)
        );
        assert_eq!(extract("---\nx: 1\n...\nbody"), present("x: 1\n", 0, 12));
        assert_eq!(extract("---\n---\nbody"), present("", 0, 7));
    }

    #[test]
    fn a_bom_is_allowed_and_not_part_of_the_range() {
        assert_eq!(
            extract("\u{feff}---\na: 1\n---\n"),
            present("a: 1\n", 3, 15)
        );
    }

    #[test]
    fn crlf_and_cr_line_endings_work() {
        assert_eq!(
            extract("---\r\na: 1\r\n---\r\nz"),
            present("a: 1\r\n", 0, 14)
        );
        assert_eq!(extract("---\ra: 1\r---\rz"), present("a: 1\r", 0, 12));
    }

    #[test]
    fn a_document_that_does_not_start_with_the_marker_has_none() {
        assert_eq!(extract(""), FrontMatter::Absent);
        assert_eq!(extract("body\n---\na: 1\n---\n"), FrontMatter::Absent);
        assert_eq!(extract("\n---\na: 1\n---\n"), FrontMatter::Absent);
        assert_eq!(extract("--- \na: 1\n---\n"), FrontMatter::Absent);
        assert_eq!(extract("----\na: 1\n---\n"), FrontMatter::Absent);
        assert_eq!(extract(" ---\na: 1\n---\n"), FrontMatter::Absent);
    }

    #[test]
    fn an_opening_line_without_a_closing_line_is_unavailable() {
        assert_eq!(extract("---\na: 1\n"), FrontMatter::Unavailable);
        assert_eq!(extract("---"), FrontMatter::Unavailable);
        // A closing marker that is not alone on its line does not count.
        assert_eq!(extract("---\na: 1\n--- x\n"), FrontMatter::Unavailable);
    }

    #[test]
    fn a_closing_line_beyond_the_search_window_is_unavailable() {
        let filler = "k: v\n".repeat(SEARCH_CHARS / 5 + 10);
        let text = format!("---\n{filler}---\n");
        assert_eq!(extract(&text), FrontMatter::Unavailable);
    }

    #[test]
    fn an_oversized_inner_text_is_unavailable() {
        let ok = format!("---\n{}\n---\n", "a".repeat(MAX_INNER_CHARS - 1));
        assert!(matches!(extract(&ok), FrontMatter::Present { .. }));
        let big = format!("---\n{}\n---\n", "a".repeat(MAX_INNER_CHARS));
        assert_eq!(extract(&big), FrontMatter::Unavailable);
    }

    #[test]
    fn the_inner_limit_counts_scalar_values_not_bytes() {
        let text = format!("---\n{}\n---\n", "あ".repeat(MAX_INNER_CHARS - 1));
        assert!(matches!(extract(&text), FrontMatter::Present { .. }));
    }

    #[test]
    fn yaml_is_never_evaluated() {
        let hostile = "---\na: &x [*x]\nb: !!python/object x\n---\n";
        assert!(matches!(extract(hostile), FrontMatter::Present { .. }));
    }
}
