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
                NodeKind::List { start: None },
                NodeKind::ListItem { task: Some(false) },
                NodeKind::TaskMarker(false),
            ],
            &[
                NodeKind::List { start: None },
                NodeKind::ListItem { task: Some(true) },
                NodeKind::TaskMarker(true),
            ],
        ],
        // The checkbox itself is content, not markup: no presenter hides it
        // yet, so it stays visible; the bullet collapses and is replaced by a
        // synthesized `•` the same as any other unordered item's marker.
        markers: &["- ", "- "],
        block_kinds: &[BlockKind::ListItem, BlockKind::ListItem],
        visual_lines: &["\u{2022} [ ] todo", "\u{2022} [x] done"],
        style_runs: &[&[], &[]],
    },
    MarkdownFixture {
        name: "nested list",
        source: "- outer\n  - inner **bold**",
        tree_paths: &[&[
            NodeKind::List { start: None },
            NodeKind::ListItem { task: None },
            NodeKind::List { start: None },
            NodeKind::ListItem { task: None },
            NodeKind::Strong,
        ]],
        markers: &["- ", "- ", "**", "**"],
        block_kinds: &[BlockKind::ListItem, BlockKind::ListItem],
        // Structural indentation is layout geometry, not fake visual text;
        // only the nested item's synthesized marker remains in the
        // presentation string.
        visual_lines: &["\u{2022} outer", "\u{2022} inner bold"],
        // "\u{2022} " is 4 UTF-8 bytes ("•" is 3 bytes, plus the space), so
        // the bold run begins after the marker and "inner ".
        style_runs: &[&[], &[style(Bold, 10, 14)]],
    },
    MarkdownFixture {
        name: "ordered list display ignores its own non-sequential source digits",
        // CommonMark only requires later items to share the first item's
        // delimiter kind, not an increasing value; the owning list's `start`
        // (from the first item alone) plus each item's sibling position is
        // what the display number comes from, never the item's own written
        // digits.
        source: "3. 甲\n1. 乙\n9. 丙",
        tree_paths: &[&[
            NodeKind::List { start: Some(3) },
            NodeKind::ListItem { task: None },
        ]],
        markers: &["3. ", "1. ", "9. "],
        block_kinds: &[
            BlockKind::ListItem,
            BlockKind::ListItem,
            BlockKind::ListItem,
        ],
        visual_lines: &["3. 甲", "4. 乙", "5. 丙"],
        style_runs: &[&[], &[], &[]],
    },
    MarkdownFixture {
        name: "ordered list display normalizes a leading zero and a `)` delimiter",
        // Leading zeros and the `)` delimiter are source bytes only
        // (`NodeKind::List::start` parses them losslessly to their numeric
        // value); the synthesized display is always `{n}. `.
        source: "003) three\n004) four",
        tree_paths: &[&[
            NodeKind::List { start: Some(3) },
            NodeKind::ListItem { task: None },
        ]],
        markers: &["003) ", "004) "],
        block_kinds: &[BlockKind::ListItem, BlockKind::ListItem],
        visual_lines: &["3. three", "4. four"],
        style_runs: &[&[], &[]],
    },
    MarkdownFixture {
        name: "ordered list display rolls from a single digit to two digits",
        source: "9. a\n1. b",
        tree_paths: &[&[
            NodeKind::List { start: Some(9) },
            NodeKind::ListItem { task: None },
        ]],
        markers: &["9. ", "1. "],
        block_kinds: &[BlockKind::ListItem, BlockKind::ListItem],
        visual_lines: &["9. a", "10. b"],
        style_runs: &[&[], &[]],
    },
    MarkdownFixture {
        name: "empty bullet item still gets a synthesized marker",
        // An item whose opening line ends right after the marker has no
        // separator byte in its source (see the marker-derivation tests in
        // `hane_markdown`), but the synthesized bullet is independent of
        // that width.
        source: "-\n- next",
        tree_paths: &[&[
            NodeKind::List { start: None },
            NodeKind::ListItem { task: None },
        ]],
        markers: &["-", "- "],
        block_kinds: &[BlockKind::ListItem, BlockKind::ListItem],
        visual_lines: &["\u{2022} ", "\u{2022} next"],
        style_runs: &[&[], &[]],
    },
    MarkdownFixture {
        name: "ordered list inside a quote keeps the quote prefix and the list marker separate",
        source: "> 1. 甲\n> 2. 乙",
        tree_paths: &[&[
            NodeKind::Quote,
            NodeKind::List { start: Some(1) },
            NodeKind::ListItem { task: None },
        ]],
        // Every quoted physical line carries its own `> ` prefix (owned by
        // the quote), separate from that line's own list marker (owned by
        // its `ListItem`); the two owners are never conflated.
        markers: &["> ", "1. ", "> ", "2. "],
        // The quote is the outer top-level construct, so it — not the
        // nested list — is the indexed block kind for both lines.
        block_kinds: &[BlockKind::Quote, BlockKind::Quote],
        visual_lines: &["1. 甲", "2. 乙"],
        style_runs: &[&[], &[]],
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
        name: "thematic break with hyphens",
        source: "---",
        tree_paths: &[&[NodeKind::Rule]],
        // A rule owns its whole source line, so its punctuation is disclosed
        // as one source-mapped row rather than derived as inline markers.
        markers: &[],
        block_kinds: &[BlockKind::Rule],
        visual_lines: &[""],
        style_runs: &[&[]],
    },
    MarkdownFixture {
        name: "thematic break with asterisks",
        source: "***",
        tree_paths: &[&[NodeKind::Rule]],
        markers: &[],
        block_kinds: &[BlockKind::Rule],
        visual_lines: &[""],
        style_runs: &[&[]],
    },
    MarkdownFixture {
        name: "thematic break with underscores",
        source: "___",
        tree_paths: &[&[NodeKind::Rule]],
        markers: &[],
        block_kinds: &[BlockKind::Rule],
        visual_lines: &[""],
        style_runs: &[&[]],
    },
    MarkdownFixture {
        name: "multi-line fenced code",
        source: "```rust\nlet answer = 42;\n```",
        tree_paths: &[&[NodeKind::CodeBlock, NodeKind::Text]],
        markers: &["```", "```"],
        // The opening/closing fence rows and opening info string collapse;
        // code content in between stays fully literal.
        block_kinds: &[
            BlockKind::CodeBlock,
            BlockKind::CodeBlock,
            BlockKind::CodeBlock,
        ],
        visual_lines: &["", "let answer = 42;", ""],
        style_runs: &[
            &[],
            &[style(CodeBlock, 0, 16)],
            &[],
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
            NodeKind::List { start: None },
            NodeKind::ListItem { task: None },
            NodeKind::Strong,
        ]],
        markers: &["- ", "**", "**"],
        block_kinds: &[BlockKind::ListItem, BlockKind::ListItem],
        // Only the item's own opening line gets a synthesized `•`; the
        // continuation line's required indentation is supplied by layout
        // geometry rather than synthesized spaces.
        visual_lines: &["\u{2022} bold", "across"],
        // "\u{2022} " is 4 UTF-8 bytes, shifting line 0's Bold run from its
        // former [0, 5) by 4 bytes.
        style_runs: &[&[style(Bold, 4, 9)], &[style(Bold, 0, 6)]],
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
        name: "soft line break is an ordinary source join, not markup",
        // An unadorned line ending inside a paragraph is CommonMark's soft
        // break: a join point in one run of inline content, not a construct
        // with its own markup to hide (contrast the hard-break fixtures
        // below, which do hide syntax bytes).
        source: "line one\nline two",
        tree_paths: &[&[NodeKind::Paragraph, NodeKind::SoftBreak]],
        markers: &[],
        block_kinds: &[BlockKind::Paragraph, BlockKind::Paragraph],
        visual_lines: &["line one", "line two"],
        style_runs: &[&[], &[]],
    },
    MarkdownFixture {
        name: "hard line break from two trailing spaces",
        // Two or more trailing spaces before a line ending force a hard
        // break. The spaces are markup and collapse; the line ending itself
        // stays an ordinary source join, matching the soft-break fixture
        // above except for the hidden marker.
        source: "line one  \nline two",
        tree_paths: &[&[NodeKind::Paragraph, NodeKind::HardBreak]],
        markers: &["  "],
        block_kinds: &[BlockKind::Paragraph, BlockKind::Paragraph],
        visual_lines: &["line one", "line two"],
        style_runs: &[&[], &[]],
    },
    MarkdownFixture {
        name: "hard line break from a trailing backslash",
        source: "line one\\\nline two",
        tree_paths: &[&[NodeKind::Paragraph, NodeKind::HardBreak]],
        markers: &["\\"],
        block_kinds: &[BlockKind::Paragraph, BlockKind::Paragraph],
        visual_lines: &["line one", "line two"],
        style_runs: &[&[], &[]],
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
