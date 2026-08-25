//! CommonMark parsing with source-byte ranges.

use hane_document::{Revision, SourceRange};
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InlineKind {
    Bold,
    Italic,
    Strikethrough,
    InlineCode,
    Link,
    CodeBlock,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockKind {
    Paragraph,
    Heading(u8),
    CodeBlock,
    Quote,
    ListItem,
    Rule,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownSpan {
    pub kind: InlineKind,
    pub source_range: SourceRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownBlock {
    pub kind: BlockKind,
    pub source_range: SourceRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkdownParse {
    pub revision: Revision,
    pub source_range: SourceRange,
    pub blocks: Vec<MarkdownBlock>,
    pub spans: Vec<MarkdownSpan>,
}

fn absolute_range(base: usize, range: std::ops::Range<usize>) -> SourceRange {
    SourceRange::new(base + range.start, base + range.end)
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// Parses a source slice and retains the byte range of every presentation item.
/// The returned offsets are absolute within the document, even for a local slice.
pub fn parse_document(
    revision: Revision,
    source_range: SourceRange,
    source: &str,
) -> MarkdownParse {
    debug_assert_eq!(source_range.end.0 - source_range.start.0, source.len());
    let options = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut blocks = Vec::new();
    let mut spans = Vec::new();
    for (event, relative_range) in Parser::new_ext(source, options).into_offset_iter() {
        let range = absolute_range(source_range.start.0, relative_range);
        match event {
            Event::Start(Tag::Heading { level, .. }) => blocks.push(MarkdownBlock {
                kind: BlockKind::Heading(heading_level(level)),
                source_range: range,
            }),
            Event::Start(Tag::Paragraph) => blocks.push(MarkdownBlock {
                kind: BlockKind::Paragraph,
                source_range: range,
            }),
            Event::Start(Tag::CodeBlock(_)) => {
                blocks.push(MarkdownBlock {
                    kind: BlockKind::CodeBlock,
                    source_range: range,
                });
                spans.push(MarkdownSpan {
                    kind: InlineKind::CodeBlock,
                    source_range: range,
                });
            }
            Event::Start(Tag::BlockQuote(_)) => blocks.push(MarkdownBlock {
                kind: BlockKind::Quote,
                source_range: range,
            }),
            Event::Start(Tag::Item) => blocks.push(MarkdownBlock {
                kind: BlockKind::ListItem,
                source_range: range,
            }),
            Event::Start(Tag::Strong) => spans.push(MarkdownSpan {
                kind: InlineKind::Bold,
                source_range: range,
            }),
            Event::Start(Tag::Emphasis) => spans.push(MarkdownSpan {
                kind: InlineKind::Italic,
                source_range: range,
            }),
            Event::Start(Tag::Strikethrough) => spans.push(MarkdownSpan {
                kind: InlineKind::Strikethrough,
                source_range: range,
            }),
            Event::Start(Tag::Link { .. }) => spans.push(MarkdownSpan {
                kind: InlineKind::Link,
                source_range: range,
            }),
            Event::Code(_) => spans.push(MarkdownSpan {
                kind: InlineKind::InlineCode,
                source_range: range,
            }),
            Event::Rule => blocks.push(MarkdownBlock {
                kind: BlockKind::Rule,
                source_range: range,
            }),
            _ => {}
        }
    }
    MarkdownParse {
        revision,
        source_range,
        blocks,
        spans,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InlineSpan {
    pub kind: InlineKind,
    pub full_range: SourceRange,
    pub content_range: SourceRange,
    pub open_marker: SourceRange,
    pub close_marker: SourceRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalParse {
    pub revision: Revision,
    pub source_range: SourceRange,
    pub spans: Vec<InlineSpan>,
}

/// Finds non-nested `**bold**` pairs in one block. Escapes and cross-block spans are
/// deliberately excluded from the Phase 0 experiment.
pub fn parse_bold(revision: Revision, source_range: SourceRange, source: &str) -> LocalParse {
    let mut spans = Vec::new();
    let bytes = source.as_bytes();
    let mut cursor = 0;
    while cursor + 1 < bytes.len() {
        let Some(open_relative) = source[cursor..].find("**") else {
            break;
        };
        let open = cursor + open_relative;
        let content_start = open + 2;
        let Some(close_relative) = source[content_start..].find("**") else {
            break;
        };
        let close = content_start + close_relative;
        if close > content_start && !source[content_start..close].contains('\n') {
            let base = source_range.start.0;
            spans.push(InlineSpan {
                kind: InlineKind::Bold,
                full_range: SourceRange::new(base + open, base + close + 2),
                content_range: SourceRange::new(base + content_start, base + close),
                open_marker: SourceRange::new(base + open, base + content_start),
                close_marker: SourceRange::new(base + close, base + close + 2),
            });
        }
        cursor = close + 2;
    }
    LocalParse {
        revision,
        source_range,
        spans,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_unicode_bold_ranges_in_bytes() {
        let parsed = parse_bold(Revision(3), SourceRange::new(10, 30), "a**日本🙂**z");
        assert_eq!(parsed.spans[0].content_range, SourceRange::new(13, 23));
        assert_eq!(parsed.spans[0].full_range, SourceRange::new(11, 25));
    }

    #[test]
    fn ignores_empty_and_unclosed_markers() {
        assert!(
            parse_bold(Revision(0), SourceRange::new(0, 4), "****")
                .spans
                .is_empty()
        );
        assert!(
            parse_bold(Revision(0), SourceRange::new(0, 5), "**abc")
                .spans
                .is_empty()
        );
    }

    #[test]
    fn commonmark_ranges_remain_absolute_for_unicode_and_nested_styles() {
        let source = "## 日本語 **太字と _斜体_** `code` ~~del~~";
        let parsed = parse_document(
            Revision(7),
            SourceRange::new(100, 100 + source.len()),
            source,
        );
        assert_eq!(parsed.revision, Revision(7));
        assert!(parsed.blocks.iter().any(|block| {
            block.kind == BlockKind::Heading(2)
                && block.source_range == SourceRange::new(100, 100 + source.len())
        }));
        for kind in [
            InlineKind::Bold,
            InlineKind::Italic,
            InlineKind::InlineCode,
            InlineKind::Strikethrough,
        ] {
            assert!(parsed.spans.iter().any(|span| span.kind == kind));
        }
        assert!(
            parsed
                .spans
                .iter()
                .all(|span| span.source_range.start.0 >= 100
                    && span.source_range.end.0 <= 100 + source.len())
        );
    }

    #[test]
    fn parses_fenced_code_as_a_code_block() {
        let source = "```rust\nlet answer = 42;\n```\n";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        assert!(
            parsed
                .blocks
                .iter()
                .any(|block| block.kind == BlockKind::CodeBlock)
        );
        assert!(
            parsed
                .spans
                .iter()
                .any(|span| span.kind == InlineKind::CodeBlock)
        );
    }
}
