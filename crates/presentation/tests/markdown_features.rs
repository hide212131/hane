//! R3.25 Markdown feature fixtures.
//!
//! One entry per construct, checked through the shared harness in
//! [`support`]. These are the initial extension targets of the phase: task list,
//! nested list, multi-line quote, multi-line fenced code, image, table, and link.
//! Each is exercised end to end — parse tree, markers, presented display kind and
//! visual text, SourceMap round-trips under every cursor position, and the bytes
//! a save would write — without a single feature-specific branch outside this
//! table, which is what shows the constructs need nothing from the UI crate.

#![allow(
    clippy::doc_markdown,
    reason = "fixture descriptions use Markdown names as prose"
)]

mod support;

use hane_markdown::NodeKind;
use hane_presentation::BlockKind;
use support::{MarkdownFixture, verify};

const FIXTURES: &[MarkdownFixture] = &[
    MarkdownFixture {
        name: "task list",
        source: "- [ ] todo\n- [x] done",
        tree_paths: &[
            &[
                NodeKind::List { ordered: false },
                NodeKind::ListItem { task: Some(false) },
                NodeKind::TaskMarker(false),
            ],
            &[
                NodeKind::List { ordered: false },
                NodeKind::ListItem { task: Some(true) },
                NodeKind::TaskMarker(true),
            ],
        ],
        // The checkbox itself is content, not markup: no presenter hides it yet,
        // so it stays visible and only the bullet collapses.
        markers: &["- ", "- "],
        block_kinds: &[BlockKind::ListItem, BlockKind::ListItem],
        visual_lines: &["[ ] todo", "[x] done"],
    },
    MarkdownFixture {
        name: "nested list",
        source: "- outer\n  - inner **bold**",
        tree_paths: &[&[
            NodeKind::List { ordered: false },
            NodeKind::ListItem { task: None },
            NodeKind::List { ordered: false },
            NodeKind::ListItem { task: None },
            NodeKind::Strong,
        ]],
        markers: &["- ", "- ", "**", "**"],
        block_kinds: &[BlockKind::ListItem, BlockKind::ListItem],
        visual_lines: &["outer", "  inner bold"],
    },
    MarkdownFixture {
        name: "multi-line quote",
        source: "> first\n> second",
        tree_paths: &[&[NodeKind::Quote, NodeKind::Paragraph, NodeKind::Text]],
        // One quote node spans both lines, so the whole-document parse derives a
        // single prefix; the per-line presentation below hides both.
        markers: &["> "],
        block_kinds: &[BlockKind::Quote, BlockKind::Quote],
        visual_lines: &["first", "second"],
    },
    MarkdownFixture {
        name: "multi-line fenced code",
        source: "```rust\nlet answer = 42;\n```",
        tree_paths: &[&[NodeKind::CodeBlock, NodeKind::Text]],
        markers: &["```rust", "```"],
        // Inside a fence every line is literal, including the delimiters.
        block_kinds: &[
            BlockKind::CodeBlock,
            BlockKind::CodeBlock,
            BlockKind::CodeBlock,
        ],
        visual_lines: &["```rust", "let answer = 42;", "```"],
    },
    MarkdownFixture {
        name: "image",
        source: "![羽](assets/feather.svg)",
        tree_paths: &[&[NodeKind::Paragraph, NodeKind::Image, NodeKind::Text]],
        // An inactive standalone image is presented by `present_image`, which
        // owns its own segments rather than going through marker derivation.
        markers: &[],
        block_kinds: &[BlockKind::Image],
        visual_lines: &["羽"],
    },
    MarkdownFixture {
        name: "table",
        source: "| 名前 | 値 |\n|:---|---:|\n| 羽 | 3 |",
        tree_paths: &[
            &[NodeKind::Table, NodeKind::TableHead, NodeKind::TableCell],
            &[NodeKind::Table, NodeKind::TableRow, NodeKind::TableCell],
        ],
        // Pipes are replaced by synthesized separators in `present_table_line`,
        // so no marker derivation is involved.
        markers: &[],
        block_kinds: &[
            BlockKind::TableRow,
            BlockKind::TableDelimiter,
            BlockKind::TableRow,
        ],
        visual_lines: &[" 名前 │ 値 ", "", " 羽 │ 3 "],
    },
    MarkdownFixture {
        name: "link",
        source: "see [Hane](https://example.com) now",
        tree_paths: &[&[NodeKind::Paragraph, NodeKind::Link, NodeKind::Text]],
        markers: &["[", "](https://example.com)"],
        block_kinds: &[BlockKind::Paragraph],
        visual_lines: &["see Hane now"],
    },
];

#[test]
fn markdown_features_satisfy_the_shared_display_contract() {
    for fixture in FIXTURES {
        verify(fixture);
    }
}
