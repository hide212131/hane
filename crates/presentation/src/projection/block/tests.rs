use super::*;

#[test]
fn nested_rule_projects_container_prefixes_into_source_map_and_layout_metadata() {
    for source in ["- item\n\n  ---\n", "> - item\n>\n>   ---\n"] {
        let rule_start = source.find("---").expect("rule source");
        let line_start = source[..rule_start]
            .rfind('\n')
            .map_or(0, |newline| newline + 1);
        let rule_line = |active: bool| {
            let mut offset = 0;
            let lines = source
                .split_inclusive('\n')
                .enumerate()
                .map(|(line, text)| {
                    let line_range = SourceRange::new(offset, offset + text.len());
                    offset = line_range.end.0;
                    BlockLine {
                        line,
                        range: line_range,
                        text,
                        disclosure: active
                            .then_some(SourceRange::empty(rule_start + 1))
                            .filter(|disclosure| range_touches(line_range, *disclosure)),
                    }
                })
                .collect::<Vec<_>>();
            let joined = parse_joined_block(&lines, Revision(1));
            let mut presented = Vec::new();
            present_joined_run(
                &lines,
                Revision(1),
                26.0,
                &(0..lines.len()),
                Some(&joined),
                None,
                &mut presented,
            );
            presented
                .into_iter()
                .find(|line| line.source_range.start.0 == line_start)
                .expect("the rule line is presented")
        };

        let inactive = rule_line(false);
        assert_eq!(inactive.kind, BlockKind::Rule, "source: {source:?}");
        assert!(inactive.visual_text.is_empty(), "source: {source:?}");
        let list = inactive
            .list
            .as_ref()
            .expect("nested rule keeps list metadata");
        assert_eq!(list.body_visual_start, VisualOffset(0));
        let hidden_prefix_bytes = inactive
            .source_map
            .segments
            .iter()
            .filter(|segment| {
                segment.source_range.start.0 >= line_start
                    && segment.source_range.end.0 <= rule_start
                    && segment.visibility == Visibility::HiddenMarkup
            })
            .map(|segment| segment.source_range.len_bytes())
            .sum::<usize>();
        assert_eq!(hidden_prefix_bytes, rule_start - line_start);
        assert_eq!(inactive.quote.is_some(), source.starts_with('>'));

        let active = rule_line(true);
        assert_eq!(active.kind, BlockKind::Rule, "source: {source:?}");
        assert_eq!(
            active.visual_text,
            source[line_start..].trim_end_matches(['\r', '\n']),
            "source: {source:?}"
        );
        let active_list = active
            .list
            .as_ref()
            .expect("active rule keeps list metadata");
        assert_eq!(
            active_list.body_visual_start,
            VisualOffset(rule_start - line_start)
        );
        let expanded_prefix_bytes = active
            .source_map
            .segments
            .iter()
            .filter(|segment| {
                segment.source_range.start.0 >= line_start
                    && segment.source_range.end.0 <= rule_start
                    && segment.visibility == Visibility::ExpandedMarkup
            })
            .map(|segment| segment.source_range.len_bytes())
            .sum::<usize>();
        assert_eq!(expanded_prefix_bytes, rule_start - line_start);
        assert!(
            active
                .source_map
                .segments
                .iter()
                .all(|segment| segment.source_range.is_empty()
                    || segment.visibility == Visibility::ExpandedMarkup)
        );
    }
}

#[test]
fn a_bare_cr_hard_break_hides_its_syntax_without_touching_the_line_ending() {
    // `RopeBuffer` and `BlockIndex` now split physical lines on a bare CR
    // exactly like `\n`/`\r\n`, so `line.rs` never hands a range spanning
    // two physical lines to the per-line presenter: a hard break's syntax
    // and its terminator both sit at the very end of their own line's
    // range, same as an LF-based hard break's. A hard break only forms
    // with a following line to break to (see
    // `hard_break_never_forms_inside_code_spans_html_tags_or_at_block_end`),
    // so this goes through the real two-line joined pipeline.
    for (first_line, second_line) in [("a  \r", "b"), ("a\\\r", "b")] {
        let base = 5;
        let first_range = SourceRange::new(base, base + first_line.len());
        let second_range =
            SourceRange::new(first_range.end.0, first_range.end.0 + second_line.len());
        let lines = [
            BlockLine {
                line: 0,
                range: first_range,
                text: first_line,
                disclosure: None,
            },
            BlockLine {
                line: 1,
                range: second_range,
                text: second_line,
                disclosure: None,
            },
        ];
        let joined = parse_joined_block(&lines, Revision(1));
        assert!(
            joined
                .parsed
                .tree
                .iter()
                .any(|(_, node)| node.kind == NodeKind::HardBreak),
            "a following line makes this a hard break: {first_line:?}"
        );
        let mut out = Vec::new();
        present_joined_run(
            &lines,
            Revision(1),
            26.0,
            &(0..2),
            Some(&joined),
            None,
            &mut out,
        );
        // The break syntax ("  " or "\") hides as markup; the terminator
        // is trimmed like any other trailing line ending, not turned into
        // a literal control character or a synthesized space.
        assert_eq!(out[0].visual_text, "a", "source: {first_line:?}");
        assert_eq!(out[1].visual_text, "b");
    }
}

#[test]
fn an_ordinary_bare_cr_soft_break_keeps_each_physical_line_its_own_visible_text() {
    let base = 5;
    let first_range = SourceRange::new(base, base + 2);
    let second_range = SourceRange::new(first_range.end.0, first_range.end.0 + 1);
    let lines = [
        BlockLine {
            line: 0,
            range: first_range,
            text: "a\r",
            disclosure: None,
        },
        BlockLine {
            line: 1,
            range: second_range,
            text: "b",
            disclosure: None,
        },
    ];
    let joined = parse_joined_block(&lines, Revision(1));
    assert!(
        joined
            .parsed
            .tree
            .iter()
            .any(|(_, node)| node.kind == NodeKind::SoftBreak),
        "an ordinary bare-CR line ending is a soft break"
    );
    let mut out = Vec::new();
    present_joined_run(
        &lines,
        Revision(1),
        26.0,
        &(0..2),
        Some(&joined),
        None,
        &mut out,
    );
    assert_eq!(out[0].visual_text, "a");
    assert_eq!(out[1].visual_text, "b");
    assert!(
        out[0]
            .source_map
            .segments
            .iter()
            .all(|segment| segment.visibility == Visibility::Visible),
        "an ordinary line ending has no markup to hide"
    );
}

#[test]
fn line_break_padding_hides_insignificant_spaces_from_rendered_presentation() {
    // CommonMark drops leading indentation of the line a break continues
    // onto, and the single trailing space/tab a soft break folds away
    // (spec §6.6/§6.7); neither should remain as extra visible whitespace,
    // but both stay addressable source bytes disclosed like other markup.
    for (first_line, second_line, expected_first, expected_second, hidden) in [
        ("foo  \n", "     bar", "foo", "bar", &[(6, 11)][..]),
        ("foo\\\n", "     bar", "foo", "bar", &[(5, 10)][..]),
        ("foo \n", " baz", "foo", "baz", &[(3, 4), (5, 6)][..]),
    ] {
        let base = 12;
        let first_range = SourceRange::new(base, base + first_line.len());
        let second_range =
            SourceRange::new(first_range.end.0, first_range.end.0 + second_line.len());
        let lines = [
            BlockLine {
                line: 0,
                range: first_range,
                text: first_line,
                disclosure: None,
            },
            BlockLine {
                line: 1,
                range: second_range,
                text: second_line,
                disclosure: None,
            },
        ];
        let joined = parse_joined_block(&lines, Revision(1));
        let mut out = Vec::new();
        present_joined_run(
            &lines,
            Revision(1),
            26.0,
            &(0..2),
            Some(&joined),
            None,
            &mut out,
        );
        assert_eq!(
            out[0].visual_text, expected_first,
            "source: {first_line:?}{second_line:?}"
        );
        assert_eq!(
            out[1].visual_text, expected_second,
            "source: {first_line:?}{second_line:?}"
        );
        for &(start, end) in hidden {
            let hidden_range = SourceRange::new(base + start, base + end);
            assert!(
                out.iter().any(|line| {
                    line.source_map.segments.iter().any(|segment| {
                        segment.source_range == hidden_range
                            && segment.visibility == Visibility::HiddenMarkup
                    })
                }),
                "expected {hidden_range:?} hidden but addressable for {first_line:?}{second_line:?}"
            );
        }
    }
}

#[test]
fn line_break_padding_hides_lazy_continuation_indentation_from_rendered_presentation() {
    // Lazy continuation lines drop some (quote) or all (list, under-indented)
    // ancestor container prefixes, but pulldown-cmark still folds them into
    // the same paragraph as an ordinary soft break, so their leading
    // whitespace must disappear from inactive visual text the same way — all
    // while every source byte, including the hidden padding, stays mapped.
    // The list fixture's opening line carries the synthesized bullet
    // marker even while inactive (#126 contract); the quote fixture has
    // no such marker since `>` markup is hidden rather than synthesized.
    for (first_line, second_line, hidden_marker, hidden_padding, expected_first) in [
        ("> foo\n", "  bar", (0, 2), (6, 8), "foo"),
        ("- foo\n", " bar", (0, 2), (6, 7), "• foo"),
    ] {
        let base = 12;
        let first_range = SourceRange::new(base, base + first_line.len());
        let second_range =
            SourceRange::new(first_range.end.0, first_range.end.0 + second_line.len());
        let lines = [
            BlockLine {
                line: 0,
                range: first_range,
                text: first_line,
                disclosure: None,
            },
            BlockLine {
                line: 1,
                range: second_range,
                text: second_line,
                disclosure: None,
            },
        ];
        let joined = parse_joined_block(&lines, Revision(1));
        let mut out = Vec::new();
        present_joined_run(
            &lines,
            Revision(1),
            26.0,
            &(0..2),
            Some(&joined),
            None,
            &mut out,
        );
        assert_eq!(
            out[0].visual_text, expected_first,
            "source: {first_line:?}{second_line:?}"
        );
        assert_eq!(
            out[1].visual_text, "bar",
            "source: {first_line:?}{second_line:?}"
        );
        for (line, range) in [(&lines[0], first_range), (&lines[1], second_range)] {
            assert!(
                segments_tile_range(range, &out[line.line].source_map.segments),
                "source bytes must stay fully mapped for line {}: {first_line:?}{second_line:?}",
                line.line
            );
        }
        for &(start, end) in &[hidden_marker, hidden_padding] {
            let hidden_range = SourceRange::new(base + start, base + end);
            assert!(
                out.iter().any(|line| {
                    line.source_map.segments.iter().any(|segment| {
                        segment.source_range == hidden_range
                            && segment.visibility == Visibility::HiddenMarkup
                    })
                }),
                "expected {hidden_range:?} hidden but addressable for {first_line:?}{second_line:?}"
            );
        }
    }
}

#[test]
fn shared_heading_kinds_follow_each_physical_lines_source_range() {
    for (source, expected) in [
        (
            "> ## first\n> plain\n> ### second\n\n",
            &[
                BlockKind::Heading(2),
                BlockKind::Quote,
                BlockKind::Heading(3),
                BlockKind::Paragraph,
            ][..],
        ),
        (
            "- ## first\n  plain\n  ### second\n",
            &[
                BlockKind::Heading(2),
                BlockKind::ListItem,
                BlockKind::Heading(3),
            ][..],
        ),
    ] {
        let mut offset = 50;
        let lines = source
            .split_inclusive('\n')
            .enumerate()
            .map(|(line, text)| {
                let range = SourceRange::new(offset, offset + text.len());
                offset = range.end.0;
                BlockLine {
                    line,
                    range,
                    text,
                    disclosure: None,
                }
            })
            .collect::<Vec<_>>();
        let joined = parse_joined_block(&lines, Revision(1));
        let mut wide = Vec::new();
        present_joined_run(
            &lines,
            Revision(1),
            26.0,
            &(0..lines.len()),
            None,
            None,
            &mut wide,
        );
        for (index, &expected_kind) in expected.iter().enumerate() {
            assert_eq!(
                wide[index].kind, expected_kind,
                "line {index} in {source:?}"
            );
            let mut narrow = Vec::new();
            present_joined_run(
                &lines[index..index + 1],
                Revision(1),
                26.0,
                &(index..index + 1),
                Some(&joined),
                None,
                &mut narrow,
            );
            assert_eq!(narrow[0].kind, expected_kind);
            assert_eq!(narrow[0].estimated_height, wide[index].estimated_height);
            assert_eq!(narrow[0].visual_text, wide[index].visual_text);
            assert_eq!(
                narrow[0].source_map.segments,
                wide[index].source_map.segments
            );
        }
    }
}

#[test]
fn list_rows_expose_semantic_geometry_policy_without_reparsing_in_layout() {
    let source = "- parent\n\n  3. child\n\n     - grandchild\n  8. child2\n\n  parent second\n- sibling";
    let mut offset = 0;
    let lines = source
        .split_inclusive('\n')
        .enumerate()
        .map(|(line, text)| {
            let range = SourceRange::new(offset, offset + text.len());
            offset = range.end.0;
            BlockLine {
                line,
                range,
                text,
                disclosure: None,
            }
        })
        .collect::<Vec<_>>();
    let joined = parse_joined_block(&lines, Revision(1));
    let mut presented = Vec::new();
    present_joined_run(
        &lines,
        Revision(1),
        26.0,
        &(0..lines.len()),
        Some(&joined),
        None,
        &mut presented,
    );

    let rows = presented
        .iter()
        .filter_map(|line| line.list.as_ref().map(|list| (line, list)))
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 9);

    let (parent, parent_meta) = rows[0];
    assert_eq!(parent.visual_text, "\u{2022} parent");
    assert_eq!(parent_meta.owner.depth, 1);
    assert_eq!(parent_meta.owner.marker_kind, ListMarkerKind::Bullet);
    assert_eq!(parent_meta.owner.ordinal, 0);
    assert_eq!(parent_meta.owner.alignment.max_marker_label, "\u{2022} ");
    assert_eq!(parent_meta.owner.alignment.max_marker_columns, 2);
    assert_eq!(parent_meta.role, ListRowRole::Opening);
    let parent_marker = parent_meta.marker.as_ref().expect("parent marker");
    assert!(parent_marker.synthesized);
    assert_eq!(parent_marker.label, "\u{2022} ");
    assert_eq!(parent_marker.anchor, parent_marker.source_range.start);
    assert_eq!(
        parent_meta.body_visual_start,
        parent_marker.visual_range.end
    );

    let (_, child_meta) = rows[2];
    assert_eq!(child_meta.owner.depth, 2);
    assert_eq!(child_meta.owner.marker_kind, ListMarkerKind::Ordered);
    assert_eq!(child_meta.owner.start, Some(3));
    assert_eq!(child_meta.owner.ordinal, 0);
    assert_eq!(child_meta.owner.alignment.max_marker_label, "3. ");
    assert_eq!(child_meta.owner.alignment.max_marker_columns, 3);
    assert_eq!(child_meta.role, ListRowRole::Opening);

    let (_, grandchild_meta) = rows[4];
    assert_eq!(grandchild_meta.owner.depth, 3);
    assert_eq!(grandchild_meta.owner.marker_kind, ListMarkerKind::Bullet);
    assert_eq!(grandchild_meta.role, ListRowRole::Opening);
    assert_eq!(grandchild_meta.structural_prefixes.len(), 2);

    let (_, second_child_meta) = rows[5];
    assert_eq!(second_child_meta.owner.list_id, child_meta.owner.list_id);
    assert_eq!(second_child_meta.owner.ordinal, 1);
    assert_eq!(second_child_meta.marker.as_ref().unwrap().label, "4. ");

    let (_, parent_second_meta) = rows[7];
    assert_eq!(parent_second_meta.owner.item_id, parent_meta.owner.item_id);
    assert_eq!(
        parent_second_meta.role,
        ListRowRole::ParentParagraphAfterNestedList { ordinal: 1 }
    );
    assert_eq!(parent_second_meta.owner.depth, 1);

    let (_, sibling_meta) = rows[8];
    assert_eq!(sibling_meta.owner.list_id, parent_meta.owner.list_id);
    assert_eq!(sibling_meta.owner.ordinal, 1);
    assert_eq!(sibling_meta.role, ListRowRole::Opening);
}

#[test]
fn list_row_metadata_distinguishes_continuation_and_loose_paragraphs() {
    let source = "- first\ncontinued\n\n  second\n\n  third\n- next";
    let mut offset = 0;
    let lines = source
        .split_inclusive('\n')
        .enumerate()
        .map(|(line, text)| {
            let range = SourceRange::new(offset, offset + text.len());
            offset = range.end.0;
            BlockLine {
                line,
                range,
                text,
                disclosure: None,
            }
        })
        .collect::<Vec<_>>();
    let joined = parse_joined_block(&lines, Revision(1));
    let mut presented = Vec::new();
    present_joined_run(
        &lines,
        Revision(1),
        26.0,
        &(0..lines.len()),
        Some(&joined),
        None,
        &mut presented,
    );

    let metadata = |line: usize| presented[line].list.as_ref().expect("list row");
    assert_eq!(metadata(0).role, ListRowRole::Opening);
    assert_eq!(metadata(1).role, ListRowRole::Continuation);
    assert!(metadata(1).structural_prefixes.is_empty());
    assert_eq!(metadata(3).role, ListRowRole::LooseParagraph { ordinal: 1 });
    assert_eq!(metadata(5).role, ListRowRole::LooseParagraph { ordinal: 2 });
    assert_eq!(metadata(6).role, ListRowRole::Opening);
    assert_eq!(metadata(6).owner.ordinal, 1);
    assert_eq!(metadata(0).owner.item_id, metadata(1).owner.item_id);
    assert_eq!(metadata(3).owner.item_id, metadata(0).owner.item_id);
}

#[test]
fn ordered_row_metadata_uses_inactive_labels_for_shared_alignment() {
    let source = "9. first\n1. second";
    let mut offset = 0;
    let lines = source
        .split_inclusive('\n')
        .enumerate()
        .map(|(line, text)| {
            let range = SourceRange::new(offset, offset + text.len());
            offset = range.end.0;
            BlockLine {
                line,
                range,
                text,
                disclosure: None,
            }
        })
        .collect::<Vec<_>>();
    let joined = parse_joined_block(&lines, Revision(1));
    let mut presented = Vec::new();
    present_joined_run(
        &lines,
        Revision(1),
        26.0,
        &(0..lines.len()),
        Some(&joined),
        None,
        &mut presented,
    );

    let first = presented[0].list.as_ref().expect("first list row");
    let second = presented[1].list.as_ref().expect("second list row");
    assert_eq!(first.marker.as_ref().unwrap().label, "9. ");
    assert_eq!(second.marker.as_ref().unwrap().label, "10. ");
    assert_eq!(first.owner.list_id, second.owner.list_id);
    assert_eq!(first.owner.alignment.max_marker_label, "10. ");
    assert_eq!(first.owner.alignment.max_marker_columns, 4);
    assert_eq!(second.owner.alignment, first.owner.alignment);
}

#[test]
fn disclosed_ordered_marker_metadata_keeps_leading_zero_and_delimiter() {
    let source = "003) first\n9) second";
    let mut offset = 0;
    let lines = source
        .split_inclusive('\n')
        .enumerate()
        .map(|(line, text)| {
            let range = SourceRange::new(offset, offset + text.len());
            offset = range.end.0;
            BlockLine {
                line,
                range,
                text,
                disclosure: (line == 0).then_some(SourceRange::empty(0)),
            }
        })
        .collect::<Vec<_>>();
    let joined = parse_joined_block(&lines, Revision(1));
    let mut presented = Vec::new();
    present_joined_run(
        &lines,
        Revision(1),
        26.0,
        &(0..lines.len()),
        Some(&joined),
        None,
        &mut presented,
    );

    let disclosed = presented[0].list.as_ref().expect("disclosed list row");
    assert_eq!(disclosed.marker.as_ref().unwrap().label, "003) ");
    assert!(!disclosed.marker.as_ref().unwrap().synthesized);
    assert_eq!(
        presented[1]
            .list
            .as_ref()
            .unwrap()
            .marker
            .as_ref()
            .unwrap()
            .label,
        "4. "
    );
    assert_eq!(disclosed.owner.start, Some(3));
    assert_eq!(disclosed.owner.alignment.max_marker_label, "3. ");
}

#[test]
fn code_fence_disclosure_only_expands_the_touched_fence_marker() {
    let source = "- item\n  ```rust\n  code\n  ```";
    let base = 0;
    let mut line_start = base;
    let lines = source
        .split_inclusive('\n')
        .enumerate()
        .map(|(line, text)| {
            let start = line_start;
            line_start += text.len();
            BlockLine {
                line,
                range: SourceRange::new(start, start + text.len()),
                text,
                disclosure: None,
            }
        })
        .collect::<Vec<_>>();
    let index = BlockIndex::build(Revision(1), source);
    let block = index.block(0).expect("list block");
    let projection = index.list_projection(&block).expect("list projection");
    let joined = parse_joined_block(&lines, Revision(1));
    let code_start = base + source.find("code").expect("code content");
    let mut presented = Vec::new();

    present_joined_run_with_list_projection(
        &lines,
        Revision(1),
        26.0,
        &(0..lines.len()),
        Some(&joined),
        Some(SourceRange::new(code_start, code_start + 4)),
        Some(projection),
        &mut presented,
    );

    let opening = presented.get(1).expect("opening fence");
    assert!(!opening.visual_text.contains("```"));
    assert!(opening.source_map.segments.iter().any(|segment| {
        segment.visibility == Visibility::HiddenMarkup
            && segment.marker_edge == Some(MarkerEdge::Opening)
    }));
    let fence_start = source.find("```").expect("opening fence") + base;
    let fence = opening
        .source_map
        .segments
        .iter()
        .find(|segment| segment.source_range == SourceRange::new(fence_start, fence_start + 3))
        .expect("fence mapping");
    assert_eq!(fence.visibility, Visibility::HiddenMarkup);
    assert_ne!(fence.visibility, Visibility::ExpandedMarkup);
}

#[test]
fn nested_code_block_kind_wins_over_its_enclosing_quote() {
    // A heading nested in a quote keeps its own heading level rather than
    // showing as quoted plain text (see `shared_heading_kinds_follow_...`);
    // a nested code block must carry the same priority, or a quoted fence
    // would lose its code background, monospace and fence hiding.
    let source = "> ```rust\n> code\n> ```\n";
    let mut offset = 0;
    let lines = source
        .split_inclusive('\n')
        .enumerate()
        .map(|(line, text)| {
            let range = SourceRange::new(offset, offset + text.len());
            offset = range.end.0;
            BlockLine {
                line,
                range,
                text,
                disclosure: None,
            }
        })
        .collect::<Vec<_>>();
    let joined = parse_joined_block(&lines, Revision(1));
    let mut presented = Vec::new();
    present_joined_run(
        &lines,
        Revision(1),
        26.0,
        &(0..lines.len()),
        Some(&joined),
        None,
        &mut presented,
    );
    for line in &presented {
        assert_eq!(line.kind, BlockKind::CodeBlock, "{:?}", line.visual_text);
    }
    assert_eq!(presented[0].visual_text, "");
    assert_eq!(presented[1].visual_text, "code");
    assert_eq!(
        presented[2].visual_text, "",
        "the closing fence collapses inside a quote the same way it does at the top level"
    );
}
