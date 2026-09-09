mod support;
use hane_markdown::NodeKind;
use hane_presentation::BlockKind;
use support::{MarkdownFixture, verify};

#[test]
fn atx_headings_preserve_display_disclosure_positions_and_source() {
    for fixture in [
        MarkdownFixture {
            name: "ATX levels and empty headings",
            source: "# H1\n## H2 ##\n### H3\n#### H4\n##### H5\n###### H6\n#\n## ###",
            tree_paths: &[&[NodeKind::Heading(6), NodeKind::Text]],
            markers: &[
                "# ", "## ", " ##", "### ", "#### ", "##### ", "###### ", "#", "## ", "###",
            ],
            block_kinds: &[
                BlockKind::Heading(1),
                BlockKind::Heading(2),
                BlockKind::Heading(3),
                BlockKind::Heading(4),
                BlockKind::Heading(5),
                BlockKind::Heading(6),
                BlockKind::Heading(1),
                BlockKind::Heading(2),
            ],
            visual_lines: &["H1", "H2", "H3", "H4", "H5", "H6", "", ""],
        },
        MarkdownFixture {
            name: "ATX tabs spaces and inline syntax",
            source: "  ##\t  *羽* [link](url) `code`\t###\t",
            tree_paths: &[
                &[NodeKind::Heading(2), NodeKind::Emphasis],
                &[NodeKind::Heading(2), NodeKind::Link],
                &[NodeKind::Heading(2), NodeKind::InlineCode],
            ],
            markers: &["##\t  ", "*", "*", "[", "](url)", "`", "`", "\t###\t"],
            block_kinds: &[BlockKind::Heading(2)],
            visual_lines: &["  羽 link code"],
        },
        MarkdownFixture {
            name: "ATX literal hashes and escapes retain source spelling",
            source: "# foo###\n## foo \\###\n### foo ### bar",
            tree_paths: &[&[NodeKind::Heading(1), NodeKind::Text]],
            markers: &["# ", "## ", "### "],
            block_kinds: &[
                BlockKind::Heading(1),
                BlockKind::Heading(2),
                BlockKind::Heading(3),
            ],
            visual_lines: &["foo###", "foo \\###", "foo ### bar"],
        },
        MarkdownFixture {
            name: "ATX paragraph interruption",
            source: "before\n## title ###\nafter",
            tree_paths: &[&[NodeKind::Heading(2), NodeKind::Text]],
            markers: &["## ", " ###"],
            block_kinds: &[
                BlockKind::Paragraph,
                BlockKind::Heading(2),
                BlockKind::Paragraph,
            ],
            visual_lines: &["before", "title", "after"],
        },
        MarkdownFixture {
            name: "invalid ATX is literal",
            source: "#foo\n####### foo\n\\# foo",
            tree_paths: &[&[NodeKind::Paragraph, NodeKind::Text]],
            markers: &[],
            block_kinds: &[
                BlockKind::Paragraph,
                BlockKind::Paragraph,
                BlockKind::Paragraph,
            ],
            visual_lines: &["#foo", "####### foo", "\\# foo"],
        },
        MarkdownFixture {
            name: "ATX inside containers uses existing container display",
            source: "> ## quote ###\n\n- ### item ###",
            tree_paths: &[
                &[NodeKind::Quote, NodeKind::Heading(2)],
                &[
                    NodeKind::List { ordered: false },
                    NodeKind::ListItem { task: None },
                    NodeKind::Heading(3),
                ],
            ],
            markers: &["> ", "## ", " ###", "- ", "### ", " ###"],
            block_kinds: &[
                BlockKind::Heading(2),
                BlockKind::Paragraph,
                BlockKind::Heading(3),
            ],
            visual_lines: &["quote", "", "item"],
        },
        MarkdownFixture {
            name: "ATX in code stays literal",
            source: "    # indented\n\n```\n## fenced ###\n```",
            tree_paths: &[&[NodeKind::CodeBlock, NodeKind::Text]],
            markers: &["```", "```"],
            block_kinds: &[
                BlockKind::CodeBlock,
                BlockKind::Paragraph,
                BlockKind::CodeBlock,
                BlockKind::CodeBlock,
                BlockKind::CodeBlock,
            ],
            visual_lines: &["    # indented", "", "```", "## fenced ###", "```"],
        },
    ] {
        verify(&fixture);
    }
}
