use crate::markdown_lines;
use crate::markers::{fence_closes, fence_closing_delimiter, fence_delimiter};
use crate::parser::parser_options;
use hane_document::SourceRange;
use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag};

/// A fenced code block whose language is `mermaid`, as the formal parser sees it.
///
/// Only a block that is, in its own source text, exactly one top-level fenced
/// code block is recognized. A fence inside a quote or a list item is parsed as
/// part of that container, so it never becomes a diagram here and stays ordinary
/// code. All offsets are absolute document offsets.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MermaidFence {
    /// The opening fence line through the end of the closing fence line. For an
    /// unclosed fence this reaches the end of the block's source text.
    pub construct_range: SourceRange,
    /// The body: from the start of the line after the opening fence to the start
    /// of the closing fence line. Empty for an empty body. For an unclosed fence
    /// the end is the end of the block's source text.
    pub body_range: SourceRange,
    /// Body text without line endings between lines and without the indentation
    /// the opening fence strips. Meaningful only when [`Self::closed`].
    pub code: String,
    /// Whether a closing fence ends the construct.
    pub closed: bool,
}

/// Reads one indexed block's source text and reports the Mermaid fence it holds.
///
/// `source` must be the text of `source_range`. The parser decides whether the
/// text is one top-level fenced code block and what its language is; the fence
/// helpers shared with presentation decide where the opening and closing lines
/// are. `None` for every other kind of block, an indented code block, and a
/// fence whose first language word is not `mermaid` (ASCII case-insensitive).
pub fn mermaid_fence(source_range: SourceRange, source: &str) -> Option<MermaidFence> {
    if source.len() != source_range.end.0.checked_sub(source_range.start.0)? {
        return None;
    }
    if !is_single_mermaid_code_block(source) {
        return None;
    }
    let mut lines = Vec::new();
    let mut offset = 0;
    for line in markdown_lines(source) {
        lines.push((offset, line));
        offset += line.len();
    }
    let first = lines.iter().position(|(_, text)| !text.trim().is_empty())?;
    let opening_text = lines[first].1;
    let opening = fence_delimiter(opening_text)?;
    let indent = opening_text.len() - opening_text.trim_start_matches(' ').len();
    let last = lines.iter().rposition(|(_, text)| !text.trim().is_empty())?;
    let closed = last > first
        && fence_closing_delimiter(lines[last].1)
            .is_some_and(|candidate| fence_closes(opening, candidate));
    let base = source_range.start.0;
    let body_start = lines.get(first + 1).map_or(source.len(), |(at, _)| *at);
    let (body_end, construct_end, code) = if closed {
        let code = lines[first + 1..last]
            .iter()
            .map(|(_, text)| {
                let text = text.trim_end_matches(['\r', '\n']);
                let stripped = text.len() - text.trim_start_matches(' ').len();
                &text[stripped.min(indent)..]
            })
            .collect::<Vec<_>>()
            .join("\n");
        (lines[last].0, lines[last].0 + lines[last].1.len(), code)
    } else {
        (source.len(), source.len(), String::new())
    };
    Some(MermaidFence {
        construct_range: SourceRange::new(base + lines[first].0, base + construct_end),
        body_range: SourceRange::new(base + body_start.min(body_end), base + body_end),
        code,
        closed,
    })
}

/// True when the parser reads `source` as exactly one top-level fenced code
/// block whose first info-string word is `mermaid`.
fn is_single_mermaid_code_block(source: &str) -> bool {
    let mut depth = 0usize;
    let mut blocks = 0usize;
    let mut mermaid = false;
    for event in Parser::new_ext(source, parser_options()) {
        match event {
            Event::Start(tag) => {
                if depth == 0 {
                    blocks += 1;
                    match &tag {
                        Tag::CodeBlock(CodeBlockKind::Fenced(info)) if blocks == 1 => {
                            mermaid = info
                                .split_whitespace()
                                .next()
                                .is_some_and(|word| word.eq_ignore_ascii_case("mermaid"));
                        }
                        _ => return false,
                    }
                }
                depth += 1;
            }
            Event::End(_) => depth = depth.saturating_sub(1),
            _ if depth == 0 => return false,
            _ => {}
        }
    }
    blocks == 1 && mermaid
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fence(source: &str) -> Option<MermaidFence> {
        mermaid_fence(SourceRange::new(100, 100 + source.len()), source)
    }

    #[test]
    fn a_closed_fence_reports_body_construct_and_code_in_absolute_offsets() {
        let source = "```mermaid\nflowchart TD\n  A --> B\n```\n";
        let fence = fence(source).expect("mermaid fence");
        assert!(fence.closed);
        assert_eq!(fence.code, "flowchart TD\n  A --> B");
        assert_eq!(fence.construct_range, SourceRange::new(100, 100 + source.len()));
        let body_start = "```mermaid\n".len();
        let body_end = source.len() - "```\n".len();
        assert_eq!(
            fence.body_range,
            SourceRange::new(100 + body_start, 100 + body_end)
        );
    }

    #[test]
    fn language_is_the_first_word_and_ignores_case_and_spacing() {
        for source in [
            "```Mermaid\nflowchart TD\n```",
            "~~~ MERMAID \nflowchart TD\n~~~",
            "```mermaid title=x\nflowchart TD\n```",
            "  ```mermaid\nflowchart TD\n  ```",
        ] {
            assert!(fence(source).is_some_and(|fence| fence.closed), "{source:?}");
        }
        for source in [
            "```mermaidx\nflowchart TD\n```",
            "```text mermaid\nflowchart TD\n```",
            "```\nflowchart TD\n```",
            "    indented mermaid\n",
            "paragraph\n",
        ] {
            assert!(fence(source).is_none(), "{source:?}");
        }
    }

    #[test]
    fn a_fence_inside_a_container_is_not_recognized() {
        for source in [
            "> ```mermaid\n> flowchart TD\n> ```\n",
            "- ```mermaid\n  flowchart TD\n  ```\n",
            "1. item\n\n   ```mermaid\n   flowchart TD\n   ```\n",
        ] {
            assert!(fence(source).is_none(), "{source:?}");
        }
    }

    #[test]
    fn an_unclosed_or_mismatched_closing_fence_is_reported_as_unclosed() {
        for source in [
            "```mermaid\nflowchart TD\n",
            "````mermaid\nflowchart TD\n```\n",
            "```mermaid\nflowchart TD\n~~~\n",
            "```mermaid\n",
        ] {
            let fence = fence(source).expect("still a mermaid fence");
            assert!(!fence.closed, "{source:?}");
            assert!(fence.code.is_empty());
        }
    }

    #[test]
    fn the_opening_indent_is_stripped_from_body_lines_and_crlf_is_normalized() {
        let source = "  ```mermaid\r\n  flowchart TD\r\n    A --> B\r\n  ```\r\n";
        let fence = fence(source).expect("mermaid fence");
        assert!(fence.closed);
        assert_eq!(fence.code, "flowchart TD\n  A --> B");
    }

    #[test]
    fn an_empty_body_is_an_empty_range_at_the_closing_fence() {
        let source = "```mermaid\n```\n";
        let fence = fence(source).expect("mermaid fence");
        assert!(fence.closed);
        assert!(fence.body_range.is_empty());
        assert_eq!(fence.body_range.start.0, 100 + "```mermaid\n".len());
        assert_eq!(fence.code, "");
    }

    #[test]
    fn blank_lines_around_the_construct_stay_outside_it() {
        let source = "\n```mermaid\nflowchart TD\n```\n\n";
        let fence = fence(source).expect("mermaid fence");
        assert!(fence.closed);
        assert_eq!(
            fence.construct_range,
            SourceRange::new(101, 101 + "```mermaid\nflowchart TD\n```\n".len())
        );
    }

    #[test]
    fn source_text_that_does_not_match_the_range_is_refused() {
        assert!(mermaid_fence(SourceRange::new(0, 3), "```mermaid\n```\n").is_none());
    }
}
