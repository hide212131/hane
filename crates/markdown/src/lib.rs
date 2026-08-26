//! CommonMark parsing with source-byte ranges.

use hane_document::{LineId, Revision, RopeBuffer, SourceRange, TextBuffer};
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FenceDelimiter {
    pub marker: u8,
    pub len: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockContextIndex {
    pub revision: Revision,
    fenced_lines: Vec<bool>,
    table_lines: Vec<bool>,
}

impl BlockContextIndex {
    pub fn line_is_fenced(&self, line: usize) -> Option<bool> {
        self.fenced_lines.get(line).copied()
    }

    pub fn line_count(&self) -> usize {
        self.fenced_lines.len()
    }

    pub fn line_is_table(&self, line: usize) -> Option<bool> {
        self.table_lines.get(line).copied()
    }
}

pub fn is_pipe_row(source: &str) -> bool {
    let content = source.trim_end_matches(['\r', '\n']);
    content.matches('|').count() >= 2
        && (content.trim_start().starts_with('|') || content.trim_end().ends_with('|'))
}

pub fn is_table_delimiter(source: &str) -> bool {
    let content = source.trim_end_matches(['\r', '\n']).trim();
    let cells = content.trim_matches('|').split('|').collect::<Vec<_>>();
    cells.len() >= 2
        && cells.iter().all(|cell| {
            let trimmed = cell.trim().trim_matches(':');
            trimmed.len() >= 3 && trimmed.bytes().all(|byte| byte == b'-')
        })
}

pub fn fence_delimiter(source: &str) -> Option<FenceDelimiter> {
    let trimmed = source.trim_start_matches(' ');
    if source.len() - trimmed.len() > 3 {
        return None;
    }
    let marker = *trimmed.as_bytes().first()?;
    if !matches!(marker, b'`' | b'~') {
        return None;
    }
    let len = trimmed
        .as_bytes()
        .iter()
        .take_while(|byte| **byte == marker)
        .count();
    (len >= 3).then_some(FenceDelimiter { marker, len })
}

/// Builds document-wide fenced-block context from a shared Rope snapshot.
/// This is intended for a single coalesced background job, never an input path.
pub fn parse_block_context(buffer: &RopeBuffer) -> BlockContextIndex {
    let mut fenced_lines = Vec::with_capacity(buffer.line_count());
    let mut pipe_rows = Vec::with_capacity(buffer.line_count());
    let mut table_delimiters = Vec::with_capacity(buffer.line_count());
    let mut fence: Option<FenceDelimiter> = None;
    for line in 0..buffer.line_count() {
        let source = buffer
            .line_range(LineId(line))
            .ok()
            .and_then(|range| buffer.text(range).ok())
            .unwrap_or_default();
        let delimiter = fence_delimiter(&source);
        pipe_rows.push(is_pipe_row(&source));
        table_delimiters.push(is_table_delimiter(&source));
        fenced_lines.push(fence.is_some() || delimiter.is_some());
        if let Some(delimiter) = delimiter {
            fence = match fence {
                Some(open) if open.marker == delimiter.marker && delimiter.len >= open.len => None,
                None => Some(delimiter),
                current => current,
            };
        }
    }
    let mut table_lines = vec![false; pipe_rows.len()];
    for delimiter in 1..pipe_rows.len() {
        if !table_delimiters[delimiter] || !pipe_rows[delimiter - 1] {
            continue;
        }
        table_lines[delimiter - 1] = true;
        table_lines[delimiter] = true;
        let mut row = delimiter + 1;
        while row < pipe_rows.len() && pipe_rows[row] && !table_delimiters[row] {
            table_lines[row] = true;
            row += 1;
        }
    }
    BlockContextIndex {
        revision: buffer.revision(),
        fenced_lines,
        table_lines,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn background_context_tracks_fences_longer_than_local_overscan() {
        let mut source = String::from("```rust\n");
        source.push_str(&"inside\n".repeat(2_100));
        source.push_str("```\nafter\n");
        let buffer = RopeBuffer::from_text(&source);
        let index = parse_block_context(&buffer);
        assert_eq!(index.revision, Revision(0));
        assert_eq!(index.line_count(), buffer.line_count());
        assert_eq!(index.line_is_fenced(2_050), Some(true));
        assert_eq!(index.line_is_fenced(2_102), Some(false));
    }

    #[test]
    fn background_context_tracks_gfm_pipe_tables() {
        let buffer = RopeBuffer::from_text(
            "before\n| Name | 値 |\n|:---|---:|\n| 羽 | 3 |\n| 鳥 | 4 |\nafter\n",
        );
        let index = parse_block_context(&buffer);
        assert_eq!(index.line_is_table(0), Some(false));
        for line in 1..=4 {
            assert_eq!(index.line_is_table(line), Some(true));
        }
        assert_eq!(index.line_is_table(5), Some(false));
    }
}
