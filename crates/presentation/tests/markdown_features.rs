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
use hane_presentation::{
    BlockKind,
    StyleKind::{Bold, CodeBlock, Image, InlineCode, Italic, Link, Table},
};
use support::{MarkdownFixture, style, verify};

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
        style_runs: &[&[], &[]],
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
        style_runs: &[&[], &[style(Bold, 8, 12)]],
    },
    MarkdownFixture {
        name: "multi-line quote",
        source: "> first\n> second",
        tree_paths: &[&[NodeKind::Quote, NodeKind::Paragraph, NodeKind::Text]],
        // One quote node spans both lines; the whole-document parse derives a
        // prefix marker for each so the shared multi-line parse in
        // `present_joined_run` can hide both.
        markers: &["> ", "> "],
        block_kinds: &[BlockKind::Quote, BlockKind::Quote],
        visual_lines: &["first", "second"],
        style_runs: &[&[], &[]],
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
        style_runs: &[
            &[style(CodeBlock, 0, 7)],
            &[style(CodeBlock, 0, 16)],
            &[style(CodeBlock, 0, 3)],
        ],
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
        style_runs: &[&[style(Image, 0, 3)]],
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
        style_runs: &[&[style(Table, 0, 17)], &[], &[style(Table, 0, 11)]],
    },
    MarkdownFixture {
        name: "link",
        source: "see [Hane](https://example.com) now",
        tree_paths: &[&[NodeKind::Paragraph, NodeKind::Link, NodeKind::Text]],
        markers: &["[", "](https://example.com)"],
        block_kinds: &[BlockKind::Paragraph],
        visual_lines: &["see Hane now"],
        style_runs: &[&[style(Link, 4, 8)]],
    },
    MarkdownFixture {
        name: "strong emphasis spanning a soft line break",
        // CommonMark §6.2 treats a soft break as whitespace inside one run of
        // inline content, so `**` opened on one physical line closes on the
        // next. `present_block` must parse both lines together to see that.
        // Runs retain the mapped newline byte on non-final lines even though
        // visual_text omits it; the same applies to the multi-line cases below.
        source: "This is **bold\nacross lines** ok",
        tree_paths: &[&[NodeKind::Paragraph, NodeKind::Strong]],
        markers: &["**", "**"],
        block_kinds: &[BlockKind::Paragraph, BlockKind::Paragraph],
        visual_lines: &["This is bold", "across lines ok"],
        style_runs: &[&[style(Bold, 8, 13)], &[style(Bold, 0, 12)]],
    },
    MarkdownFixture {
        name: "code span spanning a soft line break",
        // A line ending inside a code span normalizes to a single space rather
        // than splitting the span (CommonMark §6.1). Hane's per-source-line
        // editor model presents that normalized boundary as a line break
        // rather than a synthesized space character, so this fixture is the
        // evidence that the span stays one `InlineCode` node (one pair of
        // markers) spanning both editor lines instead of two unmatched
        // halves. The exact input and expected values of CommonMark 0.31.2
        // §6.1 Example 335 are covered separately below.
        source: "See `code\nacross` lines",
        tree_paths: &[&[NodeKind::Paragraph, NodeKind::InlineCode]],
        markers: &["`", "`"],
        block_kinds: &[BlockKind::Paragraph, BlockKind::Paragraph],
        visual_lines: &["See code", "across lines"],
        style_runs: &[&[style(InlineCode, 4, 9)], &[style(InlineCode, 0, 6)]],
    },
    MarkdownFixture {
        name: "code span example 335: interior line endings and trailing spaces",
        // CommonMark 0.31.2 §6.1 Example 335 (`vendor/pulldown-cmark/tests/suite/spec.rs`'s
        // `spec_test_335`): a code span opened and closed by a bare `` `` ``
        // line, with `foo`, `bar  ` (two trailing spaces) and `baz` on the
        // lines between. Line endings normalize to a single space each, and
        // the one leading/trailing space produced that way is stripped, so
        // the reference HTML is `<code>foo bar   baz</code>` — the two
        // original trailing spaces on `bar  ` survive between the two
        // collapsed line-ending spaces. Hane's editor model keeps each
        // physical line's own content instead of collapsing lines into one,
        // so this checks the same interior whitespace is preserved per line
        // and that the delimiter-only first and last lines disclose no other
        // visible text.
        source: "``\nfoo\nbar  \nbaz\n``",
        tree_paths: &[&[NodeKind::Paragraph, NodeKind::InlineCode]],
        markers: &["``", "``"],
        block_kinds: &[
            BlockKind::Paragraph,
            BlockKind::Paragraph,
            BlockKind::Paragraph,
            BlockKind::Paragraph,
            BlockKind::Paragraph,
        ],
        visual_lines: &["", "foo", "bar  ", "baz", ""],
        style_runs: &[
            &[],
            // `foo` and `bar  ` each end mid-span, so their run retains the
            // mapped newline byte that becomes the next line's leading space
            // even though visual_text omits it (same convention as the
            // soft-line-break fixtures above).
            &[style(InlineCode, 0, 4)],
            &[style(InlineCode, 0, 6)],
            // `baz` is the last content line; its trailing newline is the
            // stripped closing padding, not a semantic space, so the run
            // matches visual_text exactly.
            &[style(InlineCode, 0, 3)],
            &[],
        ],
    },
    MarkdownFixture {
        name: "strong emphasis spanning a soft line break inside a list item",
        // A list item's paragraph content joins across physical lines the same
        // way a top-level paragraph does; the bullet stays a per-line marker.
        source: "- **bold\n  across**",
        tree_paths: &[&[
            NodeKind::List { ordered: false },
            NodeKind::ListItem { task: None },
            NodeKind::Strong,
        ]],
        markers: &["- ", "**", "**"],
        block_kinds: &[BlockKind::ListItem, BlockKind::ListItem],
        visual_lines: &["bold", "  across"],
        style_runs: &[&[style(Bold, 0, 5)], &[style(Bold, 0, 8)]],
    },
    MarkdownFixture {
        name: "strong emphasis spanning a soft line break inside a quote",
        // Every quoted physical line carries its own `> ` marker (see
        // `derive_markers`), so joining the quote's paragraph across lines does
        // not lose either line's prefix.
        source: "> **bold\n> across**",
        tree_paths: &[&[NodeKind::Quote, NodeKind::Paragraph, NodeKind::Strong]],
        markers: &["> ", "**", "> ", "**"],
        block_kinds: &[BlockKind::Quote, BlockKind::Quote],
        visual_lines: &["bold", "across"],
        style_runs: &[&[style(Bold, 0, 5)], &[style(Bold, 0, 6)]],
    },
    MarkdownFixture {
        name: "nested strong and emphasis",
        // Not a single numbered CommonMark example; kept as the evidence that
        // Strong and Emphasis compose — the inner Emphasis style unions into
        // the outer Strong run without leaking italics past the outer `**`.
        source: "**bold *and italic* still bold**",
        tree_paths: &[&[NodeKind::Paragraph, NodeKind::Strong, NodeKind::Emphasis]],
        markers: &["**", "*", "*", "**"],
        block_kinds: &[BlockKind::Paragraph],
        visual_lines: &["bold and italic still bold"],
        style_runs: &[&[style(Bold, 0, 26), style(Italic, 5, 15)]],
    },
    MarkdownFixture {
        name: "emphasis follows CommonMark word-boundary rules",
        // `_` cannot open or close emphasis inside a word; `*` can.
        // Both underscores here are intraword. The exact numbered examples
        // 355 and 360 are covered separately below.
        source: "foo_bar_baz and foo*bar*baz",
        tree_paths: &[&[NodeKind::Paragraph, NodeKind::Emphasis]],
        markers: &["*", "*"],
        block_kinds: &[BlockKind::Paragraph],
        visual_lines: &["foo_bar_baz and foobarbaz"],
        style_runs: &[&[style(Italic, 19, 22)]],
    },
    MarkdownFixture {
        name: "code span content is never reinterpreted as markup",
        // The `**` inside the code span stays literal text; only the `**`
        // outside it becomes Strong.
        source: "code contains **not bold** literally: `a **b** c`",
        tree_paths: &[
            &[NodeKind::Paragraph, NodeKind::Strong],
            &[NodeKind::Paragraph, NodeKind::InlineCode],
        ],
        markers: &["**", "**", "`", "`"],
        block_kinds: &[BlockKind::Paragraph],
        visual_lines: &["code contains not bold literally: a **b** c"],
        style_runs: &[&[style(Bold, 14, 22), style(InlineCode, 34, 43)]],
    },
    MarkdownFixture {
        name: "code span padding is trimmed for display only",
        // CommonMark §6.1: content that both opens and closes on a space, and
        // is not all spaces, has one space trimmed from each end on display.
        source: "pad ` code ` pad",
        tree_paths: &[&[NodeKind::Paragraph, NodeKind::InlineCode]],
        markers: &["`", "`"],
        block_kinds: &[BlockKind::Paragraph],
        visual_lines: &["pad code pad"],
        style_runs: &[&[style(InlineCode, 4, 8)]],
    },
    MarkdownFixture {
        name: "code span (CommonMark 0.31.2 §6.1 Example 328): simple case",
        source: "`foo`",
        tree_paths: &[&[NodeKind::Paragraph, NodeKind::InlineCode]],
        markers: &["`", "`"],
        block_kinds: &[BlockKind::Paragraph],
        visual_lines: &["foo"],
        style_runs: &[&[style(InlineCode, 0, 3)]],
    },
    MarkdownFixture {
        name: "code span (CommonMark 0.31.2 §6.1 Example 329): double-backtick \
               fence enclosing an inner backtick and its padding spaces",
        source: "`` foo ` bar ``",
        tree_paths: &[&[NodeKind::Paragraph, NodeKind::InlineCode]],
        markers: &["``", "``"],
        block_kinds: &[BlockKind::Paragraph],
        visual_lines: &["foo ` bar"],
        style_runs: &[&[style(InlineCode, 0, 9)]],
    },
    MarkdownFixture {
        name: "code span (CommonMark 0.31.2 §6.1 Example 338): a backslash inside \
               a code span is literal, not an escape, so it does not protect the \
               following backtick from closing the span",
        source: r"`foo\`bar`",
        tree_paths: &[&[NodeKind::Paragraph, NodeKind::InlineCode]],
        markers: &["`", "`"],
        block_kinds: &[BlockKind::Paragraph],
        visual_lines: &["foo\\bar`"],
        style_runs: &[&[style(InlineCode, 0, 4)]],
    },
    MarkdownFixture {
        name: "code span (CommonMark 0.31.2 §6.1 Example 347): mismatched \
               backtick-string lengths never form a code span",
        source: "```foo``",
        tree_paths: &[],
        markers: &[],
        block_kinds: &[BlockKind::Paragraph],
        visual_lines: &["```foo``"],
        style_runs: &[&[]],
    },
    MarkdownFixture {
        name: "code span (CommonMark 0.31.2 §6.1 Example 348): an unclosed \
               backtick never forms a code span",
        source: "`foo",
        tree_paths: &[],
        markers: &[],
        block_kinds: &[BlockKind::Paragraph],
        visual_lines: &["`foo"],
        style_runs: &[&[]],
    },
    MarkdownFixture {
        name: "emphasis (CommonMark 0.31.2 §6.2 Example 350): basic *...*",
        source: "*foo bar*",
        tree_paths: &[&[NodeKind::Paragraph, NodeKind::Emphasis]],
        markers: &["*", "*"],
        block_kinds: &[BlockKind::Paragraph],
        visual_lines: &["foo bar"],
        style_runs: &[&[style(Italic, 0, 7)]],
    },
    MarkdownFixture {
        name: "emphasis (CommonMark 0.31.2 §6.2 Example 351): an opener \
               immediately followed by whitespace cannot open emphasis",
        source: "a * foo bar*",
        tree_paths: &[],
        markers: &[],
        block_kinds: &[BlockKind::Paragraph],
        visual_lines: &["a * foo bar*"],
        style_runs: &[&[]],
    },
    MarkdownFixture {
        name: "emphasis (CommonMark 0.31.2 §6.2 Example 357): basic _..._",
        source: "_foo bar_",
        tree_paths: &[&[NodeKind::Paragraph, NodeKind::Emphasis]],
        markers: &["_", "_"],
        block_kinds: &[BlockKind::Paragraph],
        visual_lines: &["foo bar"],
        style_runs: &[&[style(Italic, 0, 7)]],
    },
    MarkdownFixture {
        name: "emphasis (CommonMark 0.31.2 §6.2 Example 371): a closer \
               immediately preceded by whitespace is not right-flanking, so it \
               cannot close and the delimiter run never finds a matching pair",
        source: "_foo bar _",
        tree_paths: &[],
        markers: &[],
        block_kinds: &[BlockKind::Paragraph],
        visual_lines: &["_foo bar _"],
        style_runs: &[&[]],
    },
    MarkdownFixture {
        name: "emphasis (CommonMark 0.31.2 §6.2 Example 355): intraword asterisk can open emphasis",
        // https://spec.commonmark.org/0.31.2/#example-355
        source: "foo*bar*",
        tree_paths: &[&[NodeKind::Paragraph, NodeKind::Emphasis]],
        markers: &["*", "*"],
        block_kinds: &[BlockKind::Paragraph],
        visual_lines: &["foobar"],
        style_runs: &[&[style(Italic, 3, 6)]],
    },
    MarkdownFixture {
        name: "emphasis (CommonMark 0.31.2 §6.2 Example 360): intraword underscore cannot open even with a right-flanking closer",
        // https://spec.commonmark.org/0.31.2/#example-360
        source: "foo_bar_",
        tree_paths: &[&[NodeKind::Paragraph, NodeKind::Text]],
        markers: &[],
        block_kinds: &[BlockKind::Paragraph],
        visual_lines: &["foo_bar_"],
        style_runs: &[&[]],
    },
    MarkdownFixture {
        name: "emphasis (CommonMark 0.31.2 §6.2 Example 365): different delimiter kinds cannot pair",
        // https://spec.commonmark.org/0.31.2/#example-365
        source: "_foo*",
        tree_paths: &[&[NodeKind::Paragraph, NodeKind::Text]],
        markers: &[],
        block_kinds: &[BlockKind::Paragraph],
        visual_lines: &["_foo*"],
        style_runs: &[&[]],
    },
];

#[test]
fn markdown_features_satisfy_the_shared_display_contract() {
    for fixture in FIXTURES {
        verify(fixture);
    }
}
