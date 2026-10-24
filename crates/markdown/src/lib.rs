//! CommonMark parsing with source-byte ranges.

// Parsed syntax values are routinely inspected without retaining their result
// in streaming and background-index call sites.
#![allow(
    clippy::must_use_candidate,
    reason = "syntax query APIs are intentionally discardable during incremental parsing"
)]
#![allow(
    clippy::doc_markdown,
    reason = "crate documentation uses established Markdown terminology as prose"
)]
#![allow(
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation,
    reason = "block-store indices are bounded by the in-memory source representation"
)]
#![allow(
    clippy::format_push_string,
    reason = "test fixture construction intentionally appends formatted fragments"
)]
#![allow(
    clippy::items_after_statements,
    reason = "a local parser helper stays beside its only call site"
)]
#![allow(
    clippy::too_many_lines,
    reason = "the formal event-to-block translation is deliberately kept as one audited table"
)]

mod block_index;
mod block_store;
mod fence_height;
mod list_editing;
mod list_projection;
mod local_block_index;
mod markers;
mod mermaid;
mod parser;
mod syntax;

pub use block_index::{
    BlockId, BlockIndex, BlockIndexState, BlockIndexUpdate, Confidence, IndexSource, IndexedBlock,
    PublishOutcome, TableProjection, TableProjectionCell, TableProjectionLink,
    TableProjectionRow,
};
pub use fence_height::FenceHeightProjection;
pub use list_editing::{
    ListEditIntent, ListEditItem, ListEditPlanResult, ListEditProjection, ListMarker,
    MarkdownEditPlan, SourceSelection, build_list_edit_projection, plan_list_edit,
};
pub use list_projection::{
    ListProjection, ListProjectionItem, ListProjectionList, ListProjectionPrefix,
    ListProjectionRow, QuoteProjection,
};
pub use local_block_index::{LocalBlockIndex, local_block_index};
pub(crate) use markers::markdown_lines;
pub use markers::{
    FenceDelimiter, FenceMarkerEdge, fence_closes, fence_closing_delimiter, fence_delimiter,
    has_delimiter_markers, is_table_delimiter,
};
pub use mermaid::{MermaidFence, mermaid_fence};
pub use parser::parse_document;
pub use syntax::{
    LinkParse, MarkdownNode, MarkdownParse, MarkdownTree, NodeId, NodeKind, TableAlignment,
    TableParse, links_intersecting,
};

#[cfg(test)]
mod tests {
    use super::*;
    use hane_document::{Revision, SourceRange};

    #[test]
    fn code_padding_tracks_semantic_spaces_instead_of_container_bytes() {
        for (source, expected) in [
            ("` code `", vec![(1, 2), (6, 7)]),
            // A CRLF contributes one semantic space but covers two source bytes.
            ("`\r\nx\r\n`", vec![(1, 3), (4, 6)]),
            ("> `\r\n> x\r\n> `", vec![(3, 5), (8, 10)]),
            ("> ` \n>  `", vec![]),
            ("` `", vec![]),
            ("`code `", vec![]),
            ("` \t `", vec![(1, 2), (3, 4)]),
        ] {
            let base = 31;
            let parsed = parse_document(
                Revision(1),
                SourceRange::new(base, base + source.len()),
                source,
            );
            let expected: Vec<_> = expected
                .into_iter()
                .map(|(start, end)| SourceRange::new(base + start, base + end))
                .collect();
            assert_eq!(parsed.code_padding, expected, "{source:?}");
            for padding in &parsed.code_padding {
                assert!(
                    parsed
                        .markers
                        .iter()
                        .all(|marker| !padding.intersects(*marker))
                );
            }
        }
    }

    #[test]
    fn nested_fence_markers_skip_the_quotes_own_prefix_on_every_line() {
        // The parser reports a nested block's own range in the original,
        // un-stripped source, so the raw bytes between a nested fence's
        // opening and closing line still contain the quote's "> " prefix.
        // Marker derivation must skip exactly that prefix per line to find
        // the fence's real closing line, not a prefix-contaminated
        // lookalike such as "> ```".
        let source = "> ```rust\n> code\n> ```\n";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        let covered = parsed
            .markers
            .iter()
            .map(|marker| &source[marker.start.0..marker.end.0])
            .collect::<Vec<_>>();
        assert_eq!(covered, vec!["> ", "```", "> ", "> ", "```"]);
    }

    #[test]
    fn an_unterminated_fences_closing_lookalike_last_line_stays_unhidden() {
        for source in [
            // The closing run is shorter than the opening's: CommonMark
            // leaves the fence open, so the whole document is one code
            // block and its last physical line — even though it is shaped
            // like a fence — is literal content, not markup.
            "````rust\ncode\n```\n",
            // The closing line's marker character differs from the
            // opening's: same rule, different reason.
            "```rust\ncode\n~~~\n",
            // A closing fence cannot carry an info string or other content;
            // the marker run is literal when non-whitespace follows it.
            "```rust\ncode\n```oops\n",
        ] {
            let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
            let code_block = parsed
                .tree
                .blocks()
                .find(|(_, node)| node.kind == NodeKind::CodeBlock)
                .unwrap_or_else(|| panic!("expected a code block in {source:?}"));
            assert_eq!(
                code_block.1.source_range,
                SourceRange::new(0, source.len()),
                "the unterminated fence should swallow the rest of the document: {source:?}"
            );
            let trimmed = source.trim_end_matches('\n');
            let last_line_start = trimmed.rfind('\n').map_or(0, |at| at + 1);
            assert!(
                parsed
                    .markers
                    .iter()
                    .all(|marker| marker.start.0 < last_line_start),
                "the closing-lookalike last line must not be derived as markup for {source:?}: {:?}",
                parsed.markers
            );
            assert!(
                parsed.markers.iter().any(|marker| marker.start.0 == 0),
                "the real opening fence should still be derived as markup for {source:?}"
            );
        }
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
        assert!(parsed.tree.blocks().any(|(_, block)| {
            block.kind == NodeKind::Heading(2)
                && block.source_range == SourceRange::new(100, 100 + source.len())
        }));
        for kind in [
            NodeKind::Strong,
            NodeKind::Emphasis,
            NodeKind::InlineCode,
            NodeKind::Strikethrough,
        ] {
            assert!(parsed.tree.iter().any(|(_, node)| node.kind == kind));
        }
        assert!(parsed.tree.iter().all(|(_, node)| {
            node.source_range.start.0 >= 100 && node.source_range.end.0 <= 100 + source.len()
        }));
    }

    #[test]
    fn parses_fenced_code_as_a_code_block() {
        let source = "```rust\nlet answer = 42;\n```\n";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        assert!(
            parsed
                .tree
                .blocks()
                .any(|(_, block)| block.kind == NodeKind::CodeBlock)
        );
    }

    #[test]
    fn blank_line_after_four_spaces_is_not_a_lazy_continuation_and_does_not_panic() {
        // Issue #562: the first source is the 13-byte reproduction input.
        for source in [
            "- [o]:u\n    \t",
            "- [o]:u\n    ",
            "- a\n    \t\n",
            "- a\n    \r\n",
            "a\n    \t\nb",
            "> a\n>     \t",
            "1. [o]:u\n     \t",
        ] {
            let range = SourceRange::new(40, 40 + source.len());
            let parsed = parse_document(Revision(3), range, source);
            assert_eq!(parsed.source_range, range, "{source:?}");
            assert!(
                parsed.tree.iter().all(|(_, node)| {
                    node.source_range.start.0 >= range.start.0
                        && node.source_range.end.0 <= range.end.0
                        && node.source_range.start.0 <= node.source_range.end.0
                }),
                "{source:?}"
            );
        }
        assert_eq!("- [o]:u\n    \t".len(), 13);

        let fixture = "- [o]:u\n    \t";
        let range = SourceRange::new(40, 40 + fixture.len());
        let parsed = parse_document(Revision(3), range, fixture);
        assert!(
            parsed
                .tree
                .iter()
                .any(|(_, node)| node.kind == NodeKind::List { start: None }
                    && node.source_range.start.0 == range.start.0),
            "the 13-byte fixture keeps its bullet list at the absolute start"
        );
        assert!(
            parsed
                .tree
                .iter()
                .any(|(_, node)| matches!(node.kind, NodeKind::ListItem { task: None })
                    && node.source_range.start.0 == range.start.0),
            "the 13-byte fixture keeps its list item at the absolute start"
        );
        assert!(
            parsed
                .tree
                .iter()
                .all(|(_, node)| node.source_range.start.0 < node.source_range.end.0),
            "every node of the 13-byte fixture has a nonzero range"
        );
    }

    // Fewer than four SPACEs make `scan_space(4)` fail after partially
    // consuming them; the remainder must still be checked as blank. Each
    // indent is its own test so one failure cannot hide the others.
    #[test]
    fn partial_indent_0_before_blank_line_ends_the_list_item_paragraph() {
        check_partial_indent_before_blank_line(0);
    }

    #[test]
    fn partial_indent_1_before_blank_line_ends_the_list_item_paragraph() {
        check_partial_indent_before_blank_line(1);
    }

    #[test]
    fn partial_indent_2_before_blank_line_ends_the_list_item_paragraph() {
        check_partial_indent_before_blank_line(2);
    }

    #[test]
    fn partial_indent_3_before_blank_line_ends_the_list_item_paragraph() {
        check_partial_indent_before_blank_line(3);
    }

    fn check_partial_indent_before_blank_line(indent: usize) {
        {
            let source = format!("- a\n{}\n", " ".repeat(indent));
            let base = 40;
            let range = SourceRange::new(base, base + source.len());
            let parsed = parse_document(Revision(3), range, &source);
            assert_eq!(parsed.source_range, range, "{source:?}");

            let count = |wanted: fn(&NodeKind) -> bool| {
                parsed
                    .tree
                    .iter()
                    .filter(|(_, node)| wanted(&node.kind))
                    .count()
            };
            assert_eq!(count(|k| matches!(k, NodeKind::List { start: None })), 1);
            assert_eq!(count(|k| matches!(k, NodeKind::ListItem { task: None })), 1);
            // The list is tight, so pulldown emits no Paragraph event.
            assert_eq!(count(|k| *k == NodeKind::Paragraph), 0, "{source:?}");

            assert!(
                parsed.tree.iter().all(|(_, node)| {
                    node.source_range.start.0 >= range.start.0
                        && node.source_range.end.0 <= range.end.0
                        && node.source_range.start.0 < node.source_range.end.0
                }),
                "{source:?}"
            );
            let text = parsed
                .tree
                .iter()
                .find(|(_, node)| node.kind == NodeKind::Text)
                .map(|(_, node)| node.source_range)
                .expect("list item text node");
            assert_eq!(
                text,
                SourceRange::new(base + 2, base + 3),
                "the paragraph text `a` keeps its absolute byte range for {source:?}"
            );
            let item = parsed
                .tree
                .iter()
                .find(|(_, node)| matches!(node.kind, NodeKind::ListItem { .. }))
                .map(|(_, node)| node.source_range)
                .expect("list item");
            assert_eq!(item.start.0, base, "{source:?}");
        }
    }

    // A reference definition inside a list item is followed by a line of 0..=3
    // SPACEs and then body text. This goes through the refdef loop that calls
    // `scan_next_line_or_lazy_continuation`, so the blank-line check there is
    // protected directly. Each indent is its own test.
    #[test]
    fn refdef_partial_indent_0_blank_line_does_not_lazily_continue_into_body() {
        check_refdef_partial_indent_blank_line_then_body(0);
    }

    #[test]
    fn refdef_partial_indent_1_blank_line_does_not_lazily_continue_into_body() {
        check_refdef_partial_indent_blank_line_then_body(1);
    }

    #[test]
    fn refdef_partial_indent_2_blank_line_does_not_lazily_continue_into_body() {
        check_refdef_partial_indent_blank_line_then_body(2);
    }

    #[test]
    fn refdef_partial_indent_3_blank_line_does_not_lazily_continue_into_body() {
        check_refdef_partial_indent_blank_line_then_body(3);
    }

    fn check_refdef_partial_indent_blank_line_then_body(indent: usize) {
        let source = format!("- [o]:u\n{}\ncontinued\n", " ".repeat(indent));
        let base = 40;
        let range = SourceRange::new(base, base + source.len());
        let parsed = parse_document(Revision(3), range, &source);
        assert_eq!(parsed.source_range, range, "{source:?}");

        let count = |wanted: fn(&NodeKind) -> bool| {
            parsed
                .tree
                .iter()
                .filter(|(_, node)| wanted(&node.kind))
                .count()
        };
        assert_eq!(
            count(|k| matches!(k, NodeKind::List { start: None })),
            1,
            "{source:?}"
        );
        assert_eq!(
            count(|k| matches!(k, NodeKind::ListItem { task: None })),
            1,
            "{source:?}"
        );
        assert_eq!(count(|k| *k == NodeKind::Paragraph), 1, "{source:?}");
        assert_eq!(count(|k| *k == NodeKind::Text), 1, "{source:?}");

        assert!(
            parsed.tree.iter().all(|(_, node)| {
                node.source_range.start.0 >= range.start.0
                    && node.source_range.end.0 <= range.end.0
                    && node.source_range.start.0 < node.source_range.end.0
            }),
            "{source:?}"
        );

        // "- [o]:u\n" is 8 bytes, then the blank line is `indent` SPACEs + "\n".
        let body_start = base + 8 + indent + 1;
        let body_end = body_start + "continued".len();

        let (_, text) = parsed
            .tree
            .iter()
            .find(|(_, node)| node.kind == NodeKind::Text)
            .expect("body text node");
        assert_eq!(
            text.source_range,
            SourceRange::new(body_start, body_end),
            "the body text keeps its absolute byte range for {source:?}"
        );

        let paragraph = parsed
            .tree
            .iter()
            .find(|(_, node)| node.kind == NodeKind::Paragraph)
            .map(|(_, node)| node.source_range)
            .expect("body paragraph");
        assert_eq!(paragraph.start.0, body_start, "{source:?}");
        assert!(paragraph.end.0 >= body_end, "{source:?}");

        // The body is not a descendant of the list item.
        let mut ancestor = text.parent;
        while let Some(id) = ancestor {
            let node = parsed.tree.node(id).expect("ancestor node");
            assert!(
                !matches!(node.kind, NodeKind::ListItem { .. }),
                "the body text must not be inside the list item for {source:?}"
            );
            ancestor = node.parent;
        }

        let item = parsed
            .tree
            .iter()
            .find(|(_, node)| matches!(node.kind, NodeKind::ListItem { .. }))
            .map(|(_, node)| node.source_range)
            .expect("list item");
        assert_eq!(item.start.0, base, "{source:?}");
        assert!(
            item.end.0 <= body_start,
            "the list item must end before the body for {source:?}"
        );
    }

    #[test]
    fn tree_nests_children_inside_their_parent_source_ranges() {
        let source = "- outer\n  - inner **bold**\n";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        for (id, node) in parsed.tree.iter() {
            let parent = parsed
                .tree
                .node(node.parent.expect("non-root node"))
                .unwrap();
            assert!(
                parent.source_range.start <= node.source_range.start
                    && node.source_range.end <= parent.source_range.end,
                "{id:?} {:?} escapes its parent {:?}",
                node.kind,
                parent.kind
            );
            assert_eq!(node.depth, parent.depth + 1);
        }
        let inner = parsed
            .tree
            .iter()
            .filter(|(_, node)| matches!(node.kind, NodeKind::ListItem { .. }))
            .map(|(id, _)| parsed.tree.list_depth(id))
            .collect::<Vec<_>>();
        assert_eq!(inner, vec![1, 2]);
    }

    #[test]
    fn task_list_items_carry_their_checkbox_state() {
        let source = "- [ ] todo\n- [x] done\n";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        let tasks = parsed
            .tree
            .iter()
            .filter_map(|(_, node)| match node.kind {
                NodeKind::ListItem { task } => Some(task),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(tasks, vec![Some(false), Some(true)]);
    }

    #[test]
    fn tables_nest_rows_and_cells_under_the_table() {
        let source = "| a | b |\n|---|---|\n| 1 | 2 |\n";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        let (table, _) = parsed
            .tree
            .iter()
            .find(|(_, node)| node.kind == NodeKind::Table)
            .expect("pipe table must parse as a table");
        let rows = parsed.tree.children(table);
        assert_eq!(rows.len(), 2);
        for row in rows {
            assert_eq!(
                parsed.tree.children(*row).len(),
                2,
                "each row has two cells"
            );
        }
    }

    #[test]
    fn table_parse_retains_delimiter_alignments() {
        let source =
            "| left | center | right | default |\n|:---|:---:|---:|---|\n| a | b | c | d |";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        assert_eq!(parsed.table_parses.len(), 1);
        assert_eq!(
            parsed.table_parses[0].alignments.as_ref(),
            &[
                TableAlignment::Left,
                TableAlignment::Center,
                TableAlignment::Right,
                TableAlignment::Default,
            ]
        );
    }

    #[test]
    fn formal_block_index_exposes_compact_table_projection() {
        let source = "| left | right |\n|:---|---:|\n| a | b |";
        let index = BlockIndex::build(Revision(1), source);
        let block = index.block(0).expect("table block");
        let projection = index.table_projection(&block).expect("table projection");
        assert_eq!(projection.delimiter_range, Some(SourceRange::new(17, 29)));
        assert_eq!(
            projection.alignments.as_ref(),
            &[TableAlignment::Left, TableAlignment::Right]
        );
        assert_eq!(projection.rows.len(), 2);
        assert!(projection.rows[0].header);
        assert_eq!(
            projection.rows[0].source.trim_end_matches(['\r', '\n']),
            "| left | right |"
        );
        assert_eq!(projection.rows[0].cells.len(), 2);
        assert!(!projection.rows[1].header);
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
        assert_eq!(
            heading.markers.first().copied(),
            Some(SourceRange::new(0, 3))
        );

        let quote = parse_document(Revision(1), SourceRange::new(0, 8), "> quote\n");
        assert_eq!(quote.markers.first().copied(), Some(SourceRange::new(0, 2)));

        let bullet = parse_document(Revision(1), SourceRange::new(0, 7), "- item\n");
        assert_eq!(
            bullet.markers.first().copied(),
            Some(SourceRange::new(0, 2))
        );

        let ordered = parse_document(Revision(1), SourceRange::new(0, 8), "1. item\n");
        assert_eq!(
            ordered.markers.first().copied(),
            Some(SourceRange::new(0, 3))
        );
    }

    #[test]
    fn quote_markers_follow_each_lines_actual_container_prefixes() {
        for (source, expected) in [
            (
                "> > first\n> > second\n",
                vec![(0, 2), (2, 4), (10, 12), (12, 14)],
            ),
            (">>first\n>>second\n", vec![(0, 1), (1, 2), (8, 9), (9, 10)]),
            (
                "  > > first\n >  >second\n",
                vec![(2, 4), (4, 6), (13, 15), (16, 17)],
            ),
            (
                ">\t>first\n>\t>second\n",
                vec![(0, 2), (2, 3), (9, 11), (11, 12)],
            ),
            (
                "> > first\n> lazy\nlazy too\n> > last\n",
                vec![(0, 2), (2, 4), (10, 12), (26, 28), (28, 30)],
            ),
            (
                "> > first\r\n> > second\r\n",
                vec![(0, 2), (2, 4), (11, 13), (13, 15)],
            ),
            (
                "> > 日本\n> > 語\n",
                vec![(0, 2), (2, 4), (11, 13), (13, 15)],
            ),
        ] {
            // A nonzero slice origin catches accidental mixing of local and
            // absolute positions, including on the first physical line.
            let base = 37;
            let parsed = parse_document(
                Revision(1),
                SourceRange::new(base, base + source.len()),
                source,
            );
            let expected: Vec<_> = expected
                .into_iter()
                .map(|(start, end)| SourceRange::new(base + start, base + end))
                .collect();
            assert_eq!(parsed.markers, expected, "source: {source:?}");
        }
    }

    #[test]
    fn quote_markers_follow_commonmark_line_endings_with_raw_offsets() {
        for newline in ["\r", "\n", "\r\n"] {
            for template in [
                "> 日本\n> 語\n",
                "> > first\n> > second\n",
                "intro\n\n- > first\n  > second\n",
                "intro\n\n> -\n>   > first\n>   > second\n",
            ] {
                let source = template.replace('\n', newline);
                let base = 37;
                let parsed = parse_document(
                    Revision(1),
                    SourceRange::new(base, base + source.len()),
                    &source,
                );
                let mut actual: Vec<_> = parsed.quote_markers.iter().map(|(r, _)| *r).collect();
                actual.sort_by_key(|r| r.start);
                let expected: Vec<_> = source
                    .match_indices('>')
                    .map(|(at, _)| SourceRange::new(base + at, base + at + 2))
                    .collect();
                assert_eq!(actual, expected, "source: {source:?}");
            }
        }
        let source = "> first\r> second\r\n> third\n> fourth";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        let expected: Vec<_> = source
            .match_indices('>')
            .map(|(at, _)| SourceRange::new(at, at + 2))
            .collect();
        assert_eq!(parsed.markers, expected);
    }

    #[test]
    fn ordered_list_start_value_is_preserved_losslessly() {
        for (source, expected) in [
            ("- item\n", None),
            ("* item\n", None),
            ("+ item\n", None),
            ("1. item\n", Some(1)),
            ("0. item\n", Some(0)),
            ("5) item\n", Some(5)),
            // Leading zeros round-trip to their numeric value here; the
            // original digits are preserved separately in the marker's own
            // source range, not in this parsed value.
            ("007. item\n", Some(7)),
            ("123456789. item\n", Some(123_456_789)),
        ] {
            let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
            let (_, list) = parsed
                .tree
                .blocks()
                .find(|(_, node)| matches!(node.kind, NodeKind::List { .. }))
                .unwrap_or_else(|| panic!("no list parsed for {source:?}"));
            assert_eq!(
                list.kind,
                NodeKind::List { start: expected },
                "source: {source:?}"
            );
        }
    }

    #[test]
    fn ten_digit_ordered_marker_exceeds_commonmarks_limit_and_is_not_a_list() {
        let source = "1234567890. item\n";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        assert!(
            !parsed
                .tree
                .blocks()
                .any(|(_, node)| matches!(node.kind, NodeKind::List { .. })),
            "a 10-digit start exceeds CommonMark's ordered-list marker limit"
        );
    }

    #[test]
    fn four_space_indent_is_an_indented_code_block_not_a_list_item() {
        let source = "    - item\n";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        assert!(
            parsed
                .tree
                .blocks()
                .any(|(_, node)| node.kind == NodeKind::CodeBlock)
        );
        assert!(
            !parsed
                .tree
                .blocks()
                .any(|(_, node)| matches!(node.kind, NodeKind::ListItem { .. }))
        );
    }

    #[test]
    fn list_item_markers_cover_bullet_and_ordered_delimiters_exactly() {
        for (source, expected) in [
            ("- item\n", (0, 2)),
            ("* item\n", (0, 2)),
            ("+ item\n", (0, 2)),
            // A tab is as valid a separator as a single space.
            ("-\titem\n", (0, 2)),
            // An empty item whose marker touches the line ending has no
            // separator byte to include.
            ("-\n", (0, 1)),
            ("-\r\n", (0, 1)),
            // Trailing whitespace on an otherwise-empty opening line still
            // contributes exactly one separator byte, the same as content.
            ("-   \n", (0, 2)),
            ("1. item\n", (0, 3)),
            ("1) item\n", (0, 3)),
            ("0. item\n", (0, 3)),
            // Leading zeros are part of the marker's source bytes, not
            // reconstructed from the parsed start value.
            ("003) item\n", (0, 5)),
            // 0-3 leading spaces are part of the marker range at this
            // nesting level.
            ("  - item\n", (2, 4)),
            ("   - item\n", (3, 5)),
            // 5+ spaces after the marker still contribute only one
            // separator byte; the rest is the item's own indentation.
            ("-     item\n", (0, 2)),
            // Nine digits is CommonMark's maximum ordered-marker width.
            ("123456789. item\n", (0, 11)),
        ] {
            let base = 37;
            let parsed = parse_document(
                Revision(1),
                SourceRange::new(base, base + source.len()),
                source,
            );
            let (item_id, _) = parsed
                .tree
                .blocks()
                .find(|(_, node)| matches!(node.kind, NodeKind::ListItem { .. }))
                .unwrap_or_else(|| panic!("no list item parsed for {source:?}"));
            let expected = SourceRange::new(base + expected.0, base + expected.1);
            assert_eq!(
                parsed.list_item_markers,
                vec![(expected, item_id)],
                "source: {source:?}"
            );
            assert!(parsed.markers.contains(&expected), "source: {source:?}");
        }
    }

    #[test]
    fn list_item_ordinal_reports_owner_and_sibling_position() {
        let source = "- a\n- b\n  - nested a\n  - nested b\n- c\n";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        let (outer_list, _) = parsed
            .tree
            .blocks()
            .find(|(_, node)| matches!(node.kind, NodeKind::List { .. }))
            .expect("outer list");
        let outer_items = parsed.tree.children(outer_list).to_vec();
        assert_eq!(outer_items.len(), 3);
        for (position, item) in outer_items.iter().enumerate() {
            assert_eq!(
                parsed.tree.list_item_ordinal(*item),
                Some((outer_list, position))
            );
        }
        // The nested list's items are siblings of each other, not of the
        // outer list's items, even though they share a document.
        let nested_list = parsed
            .tree
            .children(outer_items[1])
            .iter()
            .copied()
            .find(|id| matches!(parsed.tree.node(*id).unwrap().kind, NodeKind::List { .. }))
            .expect("nested list under the second outer item");
        let nested_items = parsed.tree.children(nested_list).to_vec();
        assert_eq!(nested_items.len(), 2);
        for (position, item) in nested_items.iter().enumerate() {
            assert_eq!(
                parsed.tree.list_item_ordinal(*item),
                Some((nested_list, position))
            );
        }
        assert_eq!(parsed.tree.list_item_ordinal(outer_list), None);
    }

    /// Every derived structural-prefix range must slice to whitespace only:
    /// it is markup-like padding, never a byte of actual content.
    fn assert_structural_prefixes_are_whitespace(
        source: &str,
        prefixes: &[(SourceRange, NodeId, usize)],
    ) {
        for (range, _, _) in prefixes {
            let slice = &source[range.start.0..range.end.0];
            assert!(
                slice.bytes().all(|byte| matches!(byte, b' ' | b'\t')),
                "prefix {range:?} is not whitespace in {source:?}: {slice:?}"
            );
        }
    }

    #[test]
    fn list_structural_prefixes_are_empty_for_a_lazy_paragraph_continuation() {
        // A paragraph continuation line may drop container indentation
        // entirely (CommonMark's lazy continuation); `line_break_padding`
        // already hides that shortfall as one undivided run, so this owned,
        // per-item mechanism must not also claim part of it.
        let source = "- foo\n bar\n";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        assert_eq!(parsed.list_structural_prefixes, Vec::new());
    }

    #[test]
    fn list_structural_prefixes_are_empty_for_an_empty_item() {
        let source = "-\n\n- next\n";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        assert_eq!(parsed.list_structural_prefixes, Vec::new());
    }

    #[test]
    fn list_structural_prefixes_cover_a_loose_items_second_and_third_paragraph() {
        let source = "- first\n\n  second\n\n  third\n";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        let (item, _) = parsed
            .tree
            .blocks()
            .find(|(_, node)| matches!(node.kind, NodeKind::ListItem { .. }))
            .expect("a list item");
        assert_eq!(
            parsed.list_structural_prefixes,
            vec![
                (SourceRange::new(9, 11), item, 2),
                (SourceRange::new(19, 21), item, 2),
            ]
        );
        assert_structural_prefixes_are_whitespace(source, &parsed.list_structural_prefixes);
    }

    #[test]
    fn list_structural_prefixes_split_ownership_for_a_nested_child_then_parent_paragraph() {
        // A nested/mixed fixture (#126): an outer item whose own directly-owned
        // content is interrupted by a nested child list with its own second
        // paragraph, followed by the outer item's own second paragraph.
        let source = "- outer\n\n  - inner\n\n    second\n\n  outer second\n\n- sibling";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        let lists = parsed
            .tree
            .blocks()
            .filter(|(_, node)| matches!(node.kind, NodeKind::List { .. }))
            .map(|(id, _)| id)
            .collect::<Vec<_>>();
        let outer_list = lists[0];
        let outer = parsed.tree.children(outer_list)[0];
        let nested_list = *parsed
            .tree
            .children(outer)
            .iter()
            .find(|id| matches!(parsed.tree.node(**id).unwrap().kind, NodeKind::List { .. }))
            .expect("nested list under the outer item");
        let inner = parsed.tree.children(nested_list)[0];

        // The outer item owns its own share of every line its content
        // continues onto, including the nested list's own opening line and
        // its own later paragraph, but not the nested item's own inner share
        // of "second"'s indentation.
        assert!(
            parsed
                .list_structural_prefixes
                .contains(&(SourceRange::new(9, 11), outer, 2))
        );
        assert!(
            parsed
                .list_structural_prefixes
                .contains(&(SourceRange::new(20, 22), outer, 2))
        );
        assert!(
            parsed
                .list_structural_prefixes
                .contains(&(SourceRange::new(32, 34), outer, 2))
        );
        // The nested item owns only the remainder of "second"'s indentation,
        // past the outer item's own share of the same physical line.
        assert!(
            parsed
                .list_structural_prefixes
                .contains(&(SourceRange::new(22, 24), inner, 2))
        );
        assert_structural_prefixes_are_whitespace(source, &parsed.list_structural_prefixes);
    }

    #[test]
    fn list_structural_prefixes_account_for_leading_spaces_and_marker_padding() {
        for (source, expected_width, continuation) in [
            // 0-3 leading spaces before the opening marker are owned by the
            // item too; the continuation width remains the full parsed prefix.
            ("  - item\n    continued\n", 4, (9, 13)),
            // 5+ spaces after the marker are one separator column, not a
            // wider required continuation indent.
            ("-     item\n  more\n", 2, (11, 13)),
        ] {
            let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
            let (item, _) = parsed
                .tree
                .blocks()
                .find(|(_, node)| matches!(node.kind, NodeKind::ListItem { .. }))
                .expect("a list item");
            let expected = if source.starts_with("  -") {
                vec![
                    (SourceRange::new(0, 2), item, 2),
                    (
                        SourceRange::new(continuation.0, continuation.1),
                        item,
                        expected_width,
                    ),
                ]
            } else {
                vec![(
                    SourceRange::new(continuation.0, continuation.1),
                    item,
                    expected_width,
                )]
            };
            assert_eq!(
                parsed.list_structural_prefixes, expected,
                "source: {source:?}"
            );
            assert_structural_prefixes_are_whitespace(source, &parsed.list_structural_prefixes);
        }
    }

    #[test]
    fn list_structural_prefixes_cover_opening_marker_padding_up_to_the_body() {
        for (source, expected_range, expected_columns) in [
            ("-   item\n", SourceRange::new(2, 4), 2),
            ("1.    item\n", SourceRange::new(3, 6), 3),
        ] {
            let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
            let (item, _) = parsed
                .tree
                .blocks()
                .find(|(_, node)| matches!(node.kind, NodeKind::ListItem { .. }))
                .expect("a list item");
            assert_eq!(
                parsed.list_structural_prefixes,
                vec![(expected_range, item, expected_columns)],
                "source: {source:?}"
            );
            assert_structural_prefixes_are_whitespace(source, &parsed.list_structural_prefixes);
        }

        // Five or more spaces are parser content indentation after the one
        // separator column, so the excess must remain visible.
        let source = "-     item\n";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        assert!(parsed.list_structural_prefixes.is_empty());
    }

    #[test]
    fn list_structural_prefixes_use_column_width_across_a_tab_and_a_two_digit_marker() {
        // A two-digit ordered marker ("10. ") is four columns wide; a single
        // leading tab on the continuation line reaches that width in one
        // byte, so the derived prefix's byte length and its column width
        // (what an inactive presentation synthesizes) legitimately differ.
        let source = "10. item\n\tcontinued\n";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        let (item, _) = parsed
            .tree
            .blocks()
            .find(|(_, node)| matches!(node.kind, NodeKind::ListItem { .. }))
            .expect("a list item");
        assert_eq!(
            parsed.list_structural_prefixes,
            vec![(SourceRange::new(9, 10), item, 4)]
        );
    }

    #[test]
    fn list_structural_prefixes_are_owned_separately_from_the_quote_prefix_sharing_a_line() {
        let source = "> - item\n>   continued\n";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        let (item, _) = parsed
            .tree
            .blocks()
            .find(|(_, node)| matches!(node.kind, NodeKind::ListItem { .. }))
            .expect("a list item");
        assert_eq!(
            parsed.list_structural_prefixes,
            vec![(SourceRange::new(11, 13), item, 2)]
        );
        // Immediately preceded by, and never overlapping, the quote's own
        // per-line prefix on the same continuation line.
        assert!(
            parsed
                .quote_markers
                .iter()
                .any(|(range, _)| *range == SourceRange::new(9, 11))
        );
    }

    #[test]
    fn list_structural_prefixes_cover_every_line_of_an_items_heading_and_fenced_code() {
        // Only the container indentation is markup; the fence delimiters and
        // code content stay literal (recovered separately by `derive_markers`
        // and left untouched here).
        let source = "- # Heading\n\n  ```\n  code\n  ```\n";
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        let (item, _) = parsed
            .tree
            .blocks()
            .find(|(_, node)| matches!(node.kind, NodeKind::ListItem { .. }))
            .expect("a list item");
        assert_eq!(
            parsed.list_structural_prefixes,
            vec![
                (SourceRange::new(13, 15), item, 2),
                (SourceRange::new(19, 21), item, 2),
                (SourceRange::new(26, 28), item, 2),
            ]
        );
        assert_structural_prefixes_are_whitespace(source, &parsed.list_structural_prefixes);
    }

    #[test]
    fn list_structural_prefixes_follow_commonmark_line_endings_with_raw_offsets() {
        for newline in ["\n", "\r\n", "\r"] {
            let template = "- first\n\n  second\n";
            let source = template.replace('\n', newline);
            let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), &source);
            let (item, _) = parsed
                .tree
                .blocks()
                .find(|(_, node)| matches!(node.kind, NodeKind::ListItem { .. }))
                .expect("a list item");
            let prefix_start = source.find("  second").expect("second paragraph");
            assert_eq!(
                parsed.list_structural_prefixes,
                vec![(SourceRange::new(prefix_start, prefix_start + 2), item, 2)],
                "newline: {newline:?}"
            );
        }
    }
}
