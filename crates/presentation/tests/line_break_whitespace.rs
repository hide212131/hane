//! CommonMark 0.31.2 source-level line-break whitespace fixtures for Issue #177.
//!
//! These cases use the shared end-to-end fixture harness, so each fixture checks
//! parse structure, inactive presentation, SourceMap round-trips at every source
//! boundary, disclosure, and exact saved source bytes. Viewport-width wrapping
//! is deliberately outside this file's scope.

#[allow(dead_code)]
mod support;

use hane_markdown::NodeKind;
use hane_presentation::{BlockKind, StyleRun};
use support::{MarkdownFixture, verify};

const NO_STYLES: &[StyleRun] = &[];

const FIXTURES: &[MarkdownFixture] = &[
    MarkdownFixture {
        name: "soft break whitespace with CRLF",
        source: "foo \r\n baz",
        tree_paths: &[&[NodeKind::Paragraph, NodeKind::SoftBreak]],
        markers: &[],
        block_kinds: &[BlockKind::Paragraph, BlockKind::Paragraph],
        style_runs: &[NO_STYLES, NO_STYLES],
        visual_lines: &["foo", "baz"],
    },
    MarkdownFixture {
        name: "soft break whitespace with bare CR",
        source: "foo \r baz",
        tree_paths: &[&[NodeKind::Paragraph, NodeKind::SoftBreak]],
        markers: &[],
        block_kinds: &[BlockKind::Paragraph, BlockKind::Paragraph],
        style_runs: &[NO_STYLES, NO_STYLES],
        visual_lines: &["foo", "baz"],
    },
    MarkdownFixture {
        name: "tab before a soft break stays textual content",
        // CommonMark defines `space` as U+0020 and treats tab separately. The
        // §6.8 rule removes spaces around a soft break, not arbitrary ASCII
        // whitespace, so a trailing tab must not be projected away as padding.
        source: "foo\t\nbaz",
        tree_paths: &[&[NodeKind::Paragraph, NodeKind::SoftBreak]],
        markers: &[],
        block_kinds: &[BlockKind::Paragraph, BlockKind::Paragraph],
        style_runs: &[NO_STYLES, NO_STYLES],
        visual_lines: &["foo\t", "baz"],
    },
    MarkdownFixture {
        name: "hard break trailing spaces and leading whitespace with CRLF",
        source: "foo  \r\n     baz",
        tree_paths: &[&[NodeKind::Paragraph, NodeKind::HardBreak]],
        markers: &["  "],
        block_kinds: &[BlockKind::Paragraph, BlockKind::Paragraph],
        style_runs: &[NO_STYLES, NO_STYLES],
        visual_lines: &["foo", "baz"],
    },
    MarkdownFixture {
        name: "backslash hard break and leading whitespace with bare CR",
        source: "foo\\\r     baz",
        tree_paths: &[&[NodeKind::Paragraph, NodeKind::HardBreak]],
        markers: &["\\"],
        block_kinds: &[BlockKind::Paragraph, BlockKind::Paragraph],
        style_runs: &[NO_STYLES, NO_STYLES],
        visual_lines: &["foo", "baz"],
    },
    MarkdownFixture {
        name: "soft break whitespace inside a quote",
        source: "> foo \n>  baz",
        tree_paths: &[&[NodeKind::Quote, NodeKind::Paragraph, NodeKind::SoftBreak]],
        markers: &["> ", "> "],
        block_kinds: &[BlockKind::Quote, BlockKind::Quote],
        style_runs: &[NO_STYLES, NO_STYLES],
        visual_lines: &["foo", "baz"],
    },
    MarkdownFixture {
        name: "hard break inside a list keeps existing continuation indentation",
        source: "- foo  \n  baz",
        // Tight list items omit the Paragraph node: pulldown-cmark suppresses
        // Start/End(Paragraph) for a tight item's `TightParagraph`, so HardBreak
        // sits directly under ListItem, matching every other tight-list fixture.
        tree_paths: &[&[
            NodeKind::List { ordered: false },
            NodeKind::ListItem { task: None },
            NodeKind::HardBreak,
        ]],
        markers: &["- ", "  "],
        block_kinds: &[BlockKind::ListItem, BlockKind::ListItem],
        style_runs: &[NO_STYLES, NO_STYLES],
        // The two spaces required by the list continuation belong to the
        // existing list presentation; break-adjacent whitespace behavior is
        // covered independently above and inside the quote fixture.
        visual_lines: &["foo", "  baz"],
    },
    MarkdownFixture {
        name: "internal paragraph spaces stay visible",
        source: "Multiple     spaces",
        tree_paths: &[&[NodeKind::Paragraph, NodeKind::Text]],
        markers: &[],
        block_kinds: &[BlockKind::Paragraph],
        style_runs: &[NO_STYLES],
        visual_lines: &["Multiple     spaces"],
    },
];

#[test]
fn commonmark_line_break_whitespace_preserves_source_and_container_semantics() {
    for fixture in FIXTURES {
        verify(fixture);
    }
}
