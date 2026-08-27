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
    /// Sorted, non-overlapping source ranges of the syntactic markers (heading
    /// hashes, quote/list prefixes, fence delimiters, emphasis/code delimiters,
    /// link brackets). Derived here so presentation and UI never re-lex markup.
    pub markers: Vec<SourceRange>,
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

/// Fenced-code and table classification for a contiguous window of source lines.
/// The window is assumed to start outside any open fence; callers that need this
/// to hold exactly (the whole-document background job) start at line 0, while the
/// bounded fallback accepts the approximation inherent to a lookback window.
fn scan_block_context(lines: &[String]) -> (Vec<bool>, Vec<bool>) {
    let mut fenced_lines = Vec::with_capacity(lines.len());
    let mut pipe_rows = Vec::with_capacity(lines.len());
    let mut table_delimiters = Vec::with_capacity(lines.len());
    let mut fence: Option<FenceDelimiter> = None;
    for source in lines {
        let delimiter = fence_delimiter(source);
        pipe_rows.push(is_pipe_row(source));
        table_delimiters.push(is_table_delimiter(source));
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
    (fenced_lines, table_lines)
}

fn line_source(buffer: &RopeBuffer, line: usize) -> String {
    buffer
        .line_range(LineId(line))
        .ok()
        .and_then(|range| buffer.text(range).ok())
        .unwrap_or_default()
}

/// Builds document-wide fenced-block context from a shared Rope snapshot.
/// This is intended for a single coalesced background job, never an input path.
pub fn parse_block_context(buffer: &RopeBuffer) -> BlockContextIndex {
    let lines = (0..buffer.line_count())
        .map(|line| line_source(buffer, line))
        .collect::<Vec<_>>();
    let (fenced_lines, table_lines) = scan_block_context(&lines);
    BlockContextIndex {
        revision: buffer.revision(),
        fenced_lines,
        table_lines,
    }
}

/// Lines scanned before the viewport when recovering fence/table state without
/// the background index. Bounds the fallback so visible parsing never depends on
/// total document size.
pub const LOCAL_CONTEXT_LOOKBACK: usize = 2_048;

/// Fenced-code and table context for a viewport, computed from a bounded window.
/// Indexed by absolute document line; lines outside the scanned window return
/// `None`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalBlockContext {
    base_line: usize,
    fenced_lines: Vec<bool>,
    table_lines: Vec<bool>,
}

impl LocalBlockContext {
    pub fn line_is_fenced(&self, line: usize) -> Option<bool> {
        line.checked_sub(self.base_line)
            .and_then(|index| self.fenced_lines.get(index).copied())
    }

    pub fn line_is_table(&self, line: usize) -> Option<bool> {
        line.checked_sub(self.base_line)
            .and_then(|index| self.table_lines.get(index).copied())
    }
}

/// Single bounded synchronous fallback used only while the background
/// [`BlockContextIndex`] is stale. Scans a lookback window before `visible.start`
/// to recover fence and table state, then classifies the visible lines. Never
/// scans the whole document, so it is safe on the visible-parse path.
pub fn local_block_context(
    buffer: &RopeBuffer,
    visible: std::ops::Range<usize>,
) -> LocalBlockContext {
    let line_count = buffer.line_count();
    let base_line = visible.start.saturating_sub(LOCAL_CONTEXT_LOOKBACK);
    // Include one line past the viewport so a header row whose delimiter sits
    // just below the last visible line is still recognized as a table.
    let scan_end = visible.end.saturating_add(1).min(line_count);
    let lines = (base_line..scan_end)
        .map(|line| line_source(buffer, line))
        .collect::<Vec<_>>();
    let (fenced_lines, table_lines) = scan_block_context(&lines);
    LocalBlockContext {
        base_line,
        fenced_lines,
        table_lines,
    }
}

fn absolute_range(base: usize, range: std::ops::Range<usize>) -> SourceRange {
    SourceRange::new(base + range.start, base + range.end)
}

/// Derives marker source ranges by lexing only inside the source ranges that
/// pulldown-cmark already attributed to each block/span. The event ranges stay
/// authoritative; this only recovers open/close delimiter positions that the
/// event stream does not expose. Returned ranges are sorted and merged.
fn derive_markers(
    blocks: &[MarkdownBlock],
    spans: &[MarkdownSpan],
    range: SourceRange,
    source: &str,
) -> Vec<SourceRange> {
    let mut markers = Vec::new();
    for block in blocks {
        let relative = block.source_range.start.0.saturating_sub(range.start.0);
        let tail = source.get(relative..).unwrap_or_default();
        match block.kind {
            BlockKind::Heading(_) => {
                let hashes = tail
                    .as_bytes()
                    .iter()
                    .take_while(|byte| **byte == b'#')
                    .count();
                if hashes > 0 {
                    let suffix = usize::from(tail.as_bytes().get(hashes) == Some(&b' '));
                    markers.push(SourceRange::new(
                        block.source_range.start.0,
                        block.source_range.start.0 + hashes + suffix,
                    ));
                }
            }
            BlockKind::Quote => {
                if tail.starts_with("> ") {
                    markers.push(SourceRange::new(
                        block.source_range.start.0,
                        block.source_range.start.0 + 2,
                    ));
                }
            }
            BlockKind::ListItem => {
                let prefix = tail
                    .find(|character: char| !character.is_ascii_whitespace())
                    .unwrap_or(0);
                let item = &tail[prefix..];
                let marker_len =
                    if item.starts_with("- ") || item.starts_with("* ") || item.starts_with("+ ") {
                        2
                    } else {
                        item.find(". ").map_or(0, |end| end + 2)
                    };
                if marker_len > 0 {
                    markers.push(SourceRange::new(
                        block.source_range.start.0 + prefix,
                        block.source_range.start.0 + prefix + marker_len,
                    ));
                }
            }
            BlockKind::CodeBlock => {
                if fence_delimiter(tail).is_some() {
                    let delimiter_end = tail.trim_end_matches(['\r', '\n']).len();
                    if delimiter_end > 0 {
                        markers.push(SourceRange::new(
                            block.source_range.start.0,
                            block.source_range.start.0 + delimiter_end,
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    for span in spans {
        let start = span.source_range.start.0;
        let end = span.source_range.end.0;
        if start < range.start.0 || end > range.end.0 || start >= end {
            continue;
        }
        let text = &source[start - range.start.0..end - range.start.0];
        let marker_len = match span.kind {
            InlineKind::Bold | InlineKind::Italic => text
                .as_bytes()
                .first()
                .filter(|marker| matches!(marker, b'*' | b'_'))
                .map_or(0, |marker| {
                    text.as_bytes()
                        .iter()
                        .take_while(|byte| *byte == marker)
                        .count()
                }),
            InlineKind::Strikethrough => 2,
            InlineKind::InlineCode => text.bytes().take_while(|byte| *byte == b'`').count(),
            InlineKind::Link => {
                if let (Some(open), Some(close)) = (text.find('['), text.find("](")) {
                    markers.push(SourceRange::new(start + open, start + open + 1));
                    markers.push(SourceRange::new(start + close, end));
                }
                0
            }
            InlineKind::CodeBlock => 0,
        };
        if marker_len > 0 && marker_len * 2 <= text.len() {
            markers.push(SourceRange::new(start, start + marker_len));
            markers.push(SourceRange::new(end - marker_len, end));
        }
    }
    markers.sort_by_key(|marker| (marker.start, marker.end));
    let mut merged: Vec<SourceRange> = Vec::with_capacity(markers.len());
    for marker in markers {
        if let Some(previous) = merged.last_mut()
            && marker.start < previous.end
        {
            previous.end = previous.end.max(marker.end);
        } else {
            merged.push(marker);
        }
    }
    merged
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
    let markers = derive_markers(&blocks, &spans, source_range, source);
    MarkdownParse {
        revision,
        source_range,
        blocks,
        spans,
        markers,
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

    #[test]
    fn markers_cover_inline_open_and_close_delimiters() {
        let source = "**b** _i_ `c` ~~s~~ [t](u)";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        for expected in [
            (0, 2),
            (3, 5),
            (6, 7),
            (8, 9),
            (10, 11),
            (12, 13),
            (14, 16),
            (17, 19),
            (20, 21),
            (22, 26),
        ] {
            let range = SourceRange::new(expected.0, expected.1);
            assert!(parsed.markers.contains(&range), "missing marker {range:?}");
        }
        // Merged and sorted: strictly increasing, non-overlapping.
        assert!(
            parsed
                .markers
                .windows(2)
                .all(|pair| pair[0].end <= pair[1].start)
        );
    }

    #[test]
    fn markers_cover_block_prefixes_for_heading_quote_and_list() {
        let heading = parse_document(Revision(1), SourceRange::new(0, 8), "## Head\n");
        assert_eq!(heading.markers.first().copied(), Some(SourceRange::new(0, 3)));

        let quote = parse_document(Revision(1), SourceRange::new(0, 8), "> quote\n");
        assert_eq!(quote.markers.first().copied(), Some(SourceRange::new(0, 2)));

        let bullet = parse_document(Revision(1), SourceRange::new(0, 7), "- item\n");
        assert_eq!(bullet.markers.first().copied(), Some(SourceRange::new(0, 2)));

        let ordered = parse_document(Revision(1), SourceRange::new(0, 8), "1. item\n");
        assert_eq!(ordered.markers.first().copied(), Some(SourceRange::new(0, 3)));
    }

    #[test]
    fn local_fallback_matches_background_index_on_the_visible_window() {
        let buffer = RopeBuffer::from_text(
            "intro\n```rust\ncode\n```\n| Name | 値 |\n|:---|---:|\n| 羽 | 3 |\ntail\n",
        );
        let background = parse_block_context(&buffer);
        let local = local_block_context(&buffer, 0..buffer.line_count());
        for line in 0..buffer.line_count() {
            assert_eq!(local.line_is_fenced(line), background.line_is_fenced(line));
            assert_eq!(local.line_is_table(line), background.line_is_table(line));
        }
    }

    #[test]
    fn local_fallback_returns_none_outside_the_scanned_window() {
        let mut source = String::from("head\n");
        source.push_str(&"body\n".repeat(4_000));
        let buffer = RopeBuffer::from_text(&source);
        let local = local_block_context(&buffer, 3_500..3_510);
        assert_eq!(local.line_is_fenced(3_505), Some(false));
        // Far above the lookback window is not scanned.
        assert_eq!(local.line_is_fenced(0), None);
    }
}
