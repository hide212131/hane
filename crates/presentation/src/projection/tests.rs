use super::*;

#[test]
fn initial_block_heights_account_for_heading_scale_and_trailing_blank_lines() {
    let document = RopeBuffer::from_text("# title\n\n## section\n\nbody\n");
    let index = BlockIndex::from_buffer(&document);

    assert_eq!(
        block_heights(&document, &index, 20.0),
        vec![53.0, 49.0, 40.0],
        "heading rows use their display scale and blank rows keep normal height"
    );
}

#[test]
fn initial_block_heights_keep_the_caret_owned_fence_row_visible() {
    let document = RopeBuffer::from_text("```\n```");
    let index = BlockIndex::from_buffer(&document);
    let inactive = block_heights(&document, &index, 26.0);
    assert_eq!(inactive, vec![0.0]);

    let editing =
        block_heights_with_disclosure(&document, &index, 26.0, Some(SourceRange::empty(1)));
    assert_eq!(
        editing,
        vec![26.0],
        "the opening fence owned by the caret must survive initial virtualization"
    );
}


#[test]
fn inactive_table_delimiter_does_not_reserve_virtual_height() {
    let document = RopeBuffer::from_text("| head | value |\n| --- | --- |\n| body | cell |");
    let index = BlockIndex::from_buffer(&document);
    let block = index.blocks().next().expect("table block");
    let delimiter = index
        .table_projection(&block)
        .and_then(|projection| projection.delimiter_range)
        .expect("table delimiter");

    assert_eq!(block_heights(&document, &index, 26.0), vec![52.0]);
    assert_eq!(
        block_heights_with_disclosure(
            &document,
            &index,
            26.0,
            Some(SourceRange::empty(delimiter.start.0 + 2)),
        ),
        vec![78.0],
        "disclosing the delimiter must restore its editable source row"
    );
}

#[test]
fn selection_outside_a_quote_fence_block_does_not_disclose_its_rows() {
    let source = "before\n\n> ```\n> code\n> ```";
    let document = RopeBuffer::from_text(source);
    let index = BlockIndex::from_buffer(&document);
    let quote_block = index
        .blocks()
        .find(|block| block.kind == NodeKind::Quote)
        .expect("quote block");
    assert!(
        index.fence_height_projection(&quote_block).is_some(),
        "the fixture must exercise the quote fence projection"
    );
    let inactive = block_heights(&document, &index, 26.0);
    let selection_before_block = SourceRange::new(0, quote_block.source_range.start.0);
    let disclosed =
        block_heights_with_disclosure(&document, &index, 26.0, Some(selection_before_block));

    assert_eq!(
        disclosed, inactive,
        "a selection ending at the block boundary must not activate its fence rows"
    );
}

#[test]
fn caret_at_the_next_block_start_does_not_disclose_the_previous_quote() {
    let source = "> ```\n> code\n> ```\n\nplain";
    let document = RopeBuffer::from_text(source);
    let index = BlockIndex::from_buffer(&document);
    let quote_block = index
        .blocks()
        .find(|block| block.kind == NodeKind::Quote)
        .expect("quoted block");
    let inactive = block_heights(&document, &index, 26.0);
    let disclosed = block_heights_with_disclosure(
        &document,
        &index,
        26.0,
        Some(SourceRange::empty(quote_block.source_range.end.0)),
    );

    assert_eq!(
        disclosed, inactive,
        "the caret at the following block start must not make the preceding quote visible"
    );
}

#[test]
fn caret_at_a_quote_owner_end_discloses_fence_rows_before_following_list_text() {
    let source = "- > ```\n  > code\n  > ```\n  after";
    let document = RopeBuffer::from_text(source);
    let index = BlockIndex::from_buffer(&document);
    let after = source.rfind('\n').expect("following list row") + 1;
    let inactive = block_heights(&document, &index, 26.0);
    let disclosed =
        block_heights_with_disclosure(&document, &index, 26.0, Some(SourceRange::empty(after)));

    assert_eq!(inactive.len(), 1, "the fixture must stay in one list block");
    assert_eq!(disclosed, vec![26.0 * 4.0]);
    assert_eq!(inactive, vec![26.0 * 3.0]);
}

#[test]
fn quote_depth_and_inline_style_come_from_the_shared_parse() {
    let source = "> outer\n> > **inner**\n";
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
    let block = IndexedBlock {
        ordinal: 0,
        id: BlockId(0),
        kind: NodeKind::Quote,
        source_range: SourceRange::new(0, source.len()),
        revision: Revision(1),
        confidence: Confidence::Formal,
        line_count: lines.len(),
        leading_content_lines: 0,
    };
    let joined = parse_joined_block(&lines, Revision(1));
    let visual = present_block(
        &block,
        Revision(1),
        &BlockWindow {
            span: 0..lines.len(),
            trailing_blank_lines: 0,
            lines: &lines,
            clipped_fence_lines: &[],
            zero_height_fence_rows_before: 0,
            zero_height_fence_rows_after: 0,
            table_delimiter_line: None,
            render: 0..lines.len(),
            joined: Some(&joined),
            block_disclosure: None,
        },
        26.0,
    );

    assert_eq!(visual.lines[0].visual_text, "outer");
    assert_eq!(visual.lines[1].visual_text, "inner");
    assert_eq!(
        visual.lines[0]
            .quote
            .map(|quote| (quote.depth, quote.disclosed_depth)),
        Some((1, 0))
    );
    assert_eq!(
        visual.lines[1]
            .quote
            .map(|quote| (quote.depth, quote.disclosed_depth)),
        Some((2, 0))
    );
    assert_ne!(
        visual.lines[0].quote.map(|quote| quote.owner),
        visual.lines[1].quote.map(|quote| quote.owner)
    );
    assert!(
        visual.lines[1]
            .style_runs
            .iter()
            .any(|run| run.kind == StyleKind::Bold)
    );
    assert!(
        visual.lines[1]
            .source_map
            .segments
            .iter()
            .any(|segment| segment.visibility == Visibility::HiddenMarkup)
    );
    let layout = layout_block(&visual, 400.0, &testing::FixedAdvanceShaper::default());
    assert_eq!(layout.lines[0].text_x_origin, QUOTE_DEPTH_INDENT);
    assert_eq!(layout.lines[1].text_x_origin, QUOTE_DEPTH_INDENT * 2.0);
    assert!(layout.lines[1].quote_bar_x_origin.is_some());
}

#[test]
fn formal_quote_projection_carries_lazy_continuation_depth() {
    let source = "> outer\ncontinuation\n";
    let index = BlockIndex::build(Revision(1), source);
    let block = index.block(0).expect("quote block");
    let projection = index
        .list_projection(&block)
        .expect("formal quote projection");
    let line_start = "> outer\n".len();
    let line_range = SourceRange::new(line_start, source.len());
    let lines = [BlockLine {
        line: 1,
        range: line_range,
        text: &source[line_start..],
        disclosure: None,
    }];
    let visual = present_block_with_list_projection(
        &block,
        Revision(1),
        &BlockWindow {
            span: 0..2,
            trailing_blank_lines: 0,
            lines: &lines,
            clipped_fence_lines: &[],
            zero_height_fence_rows_before: 0,
            zero_height_fence_rows_after: 0,
            table_delimiter_line: None,
            render: 1..2,
            joined: None,
            block_disclosure: None,
        },
        26.0,
        Some(projection),
    );

    assert_eq!(visual.lines[0].visual_text, "continuation");
    assert_eq!(visual.lines[0].kind, BlockKind::Quote);
    assert_eq!(
        visual.lines[0]
            .quote
            .map(|quote| (quote.depth, quote.disclosed_depth)),
        Some((1, 0))
    );
}

#[test]
fn formal_quote_owner_wins_over_same_depth_viewport_owner_at_large_boundary() {
    const SYNC_LINE_BUDGET: usize = 4_096;
    const SYNC_BYTE_BUDGET: usize = 256 * 1024;

    let cases = [
        (
            "line-large",
            format!(
                "> opening\n{}> boundary\n![alt](dest)\n",
                "> filler\n".repeat(SYNC_LINE_BUDGET - 2)
            ),
        ),
        (
            "byte-large",
            format!(
                "> opening\n> {}\n> boundary\n![alt](dest)\n",
                "x".repeat(SYNC_BYTE_BUDGET)
            ),
        ),
    ];

    for (case, source) in cases {
        assert!(
            source.lines().count() > SYNC_LINE_BUDGET || source.len() > SYNC_BYTE_BUDGET,
            "{case} fixture must exceed a synchronous join budget"
        );
        let index = BlockIndex::build(Revision(1), &source);
        let block = index.block(0).expect("quote block");
        let projection = index
            .list_projection(&block)
            .expect("formal quote projection");

        let image_start = source.rfind("![alt](dest)\n").expect("image line");
        let boundary_start = source[..image_start]
            .rfind("> boundary\n")
            .expect("boundary line");
        let boundary_line = source[..boundary_start]
            .bytes()
            .filter(|&byte| byte == b'\n')
            .count();
        let image_line = boundary_line + 1;
        let boundary_range = SourceRange::new(boundary_start, image_start);
        let image_range = SourceRange::new(image_start, source.len());
        let lines = [
            BlockLine {
                line: boundary_line,
                range: boundary_range,
                text: &source[boundary_start..image_start],
                disclosure: None,
            },
            BlockLine {
                line: image_line,
                range: image_range,
                text: &source[image_start..],
                disclosure: None,
            },
        ];
        let formal_owner = projection
            .quotes_in(boundary_range)
            .max_by_key(|quote| quote.depth)
            .expect("formal owner for the boundary")
            .source_range;
        assert!(
            formal_owner.start.0 < boundary_start,
            "{case} must distinguish the document-wide owner from the viewport parse"
        );
        let visual = present_block_with_list_projection(
            &block,
            Revision(1),
            &BlockWindow {
                span: 0..block.line_count,
                trailing_blank_lines: 0,
                lines: &lines,
                clipped_fence_lines: &[],
                zero_height_fence_rows_before: 0,
                zero_height_fence_rows_after: 0,
                table_delimiter_line: None,
                render: boundary_line..image_line + 1,
                joined: None,
                block_disclosure: None,
            },
            26.0,
            Some(projection),
        );

        assert_eq!(visual.lines.len(), 2, "{case} viewport lines");
        assert_eq!(visual.lines[1].kind, BlockKind::Image, "{case} image line");
        assert_eq!(
            visual.lines[0].quote.map(|quote| quote.owner),
            Some(formal_owner),
            "{case} normal quote line must use the formal owner"
        );
        assert_eq!(
            visual.lines[1].quote.map(|quote| quote.owner),
            Some(formal_owner),
            "{case} lazy continuation image must share the formal owner"
        );
    }
}

#[test]
fn thematic_break_uses_parser_kind_and_discloses_its_source() {
    let source = "---\n";
    let range = SourceRange::new(0, source.len());
    let inactive = present_polished_line(
        0,
        Revision(1),
        range,
        source,
        26.0,
        None,
        LineContext::Normal,
    );
    assert_eq!(inactive.kind, BlockKind::Rule);
    assert!(inactive.visual_text.is_empty());
    assert!(inactive.rule_body_is_collapsed());
    assert_eq!(inactive.source_map.segments[0].source_range, range);
    assert_eq!(
        inactive.source_map.segments[0].visibility,
        Visibility::HiddenMarkup
    );

    let active = present_polished_line(
        0,
        Revision(1),
        range,
        source,
        26.0,
        Some(SourceRange::empty(1)),
        LineContext::Normal,
    );
    assert_eq!(active.kind, BlockKind::Rule);
    assert_eq!(active.visual_text, source);
    assert!(!active.rule_body_is_collapsed());
    assert_eq!(
        active.source_map.segments[0].visibility,
        Visibility::ExpandedMarkup
    );

    for (source, is_rule, has_list_item) in [
        ("---\n", true, false),
        ("title\n---\n", false, false),
        ("- item\n", false, true),
        // The leading bullet is not a list marker here: the complete line
        // is a thematic break, so the parser emits a top-level Rule.
        ("- ---\n", true, false),
        ("--- text\n", false, false),
    ] {
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        assert_eq!(
            parsed
                .tree
                .iter()
                .any(|(_, node)| node.kind == NodeKind::Rule),
            is_rule,
            "Rule must follow parser context for {source:?}"
        );
        assert_eq!(
            parsed
                .tree
                .iter()
                .any(|(_, node)| matches!(node.kind, NodeKind::ListItem { .. })),
            has_list_item,
            "list-item ancestry must follow parser context for {source:?}"
        );
    }
}

#[test]
fn thematic_break_parser_tree_preserves_list_and_quote_ancestors() {
    for (source, quoted) in [
        ("- item\n\n  ---\n", false),
        ("> - item\n>\n>   ---\n", true),
    ] {
        let range = SourceRange::new(0, source.len());
        let parsed = parse_document(Revision(1), range, source);
        let rule_id = parsed
            .tree
            .iter()
            .find_map(|(id, node)| (node.kind == NodeKind::Rule).then_some(id))
            .expect("parser-confirmed thematic break");
        let ancestors = parsed
            .tree
            .ancestors(rule_id)
            .skip(1)
            .filter_map(|id| parsed.tree.node(id).map(|node| node.kind))
            .collect::<Vec<_>>();
        assert!(
            ancestors
                .iter()
                .any(|kind| matches!(kind, NodeKind::List { .. })),
            "Rule must be nested in a List: {source:?}"
        );
        assert!(
            ancestors
                .iter()
                .any(|kind| matches!(kind, NodeKind::ListItem { .. })),
            "Rule must be nested in a ListItem: {source:?}"
        );
        assert_eq!(
            ancestors.contains(&NodeKind::Quote),
            quoted,
            "quote ancestry must follow parser context for {source:?}"
        );
    }
}


#[test]
fn source_index_skips_offscreen_spans_under_a_long_enclosing_construct() {
    // A prefix-max-only index would scan all preceding spans because the
    // enclosing construct reaches the end. Subtree maxima must prune them.
    let mut ranges = vec![(SourceRange::new(0, 1_000_000), 0)];
    ranges.extend((1..100_000).map(|i| (SourceRange::new(i * 10, i * 10 + 5), i)));
    let index = SourceIndex::new(ranges);
    let mut hits = Vec::new();
    let mut visited = 0;
    index.query(
        0,
        index.entries.len(),
        SourceRange::new(999_980, 999_985),
        &mut hits,
        &mut visited,
    );
    hits.sort();
    assert_eq!(hits, vec![&0, &99_998]);
    assert!(
        visited < 100,
        "visited {visited} nodes for two intersecting spans"
    );
}

#[test]
fn cached_hundred_thousand_line_quote_projects_only_viewport_markers() {
    let source = format!("> **opening\n{}> closing**\n", "> plain\n".repeat(99_998));
    let mut start = 40;
    let lines = source
        .split_inclusive('\n')
        .enumerate()
        .map(|(line, text)| {
            let range = SourceRange::new(start, start + text.len());
            start = range.end.0;
            BlockLine {
                line,
                range,
                text,
                disclosure: None,
            }
        })
        .collect::<Vec<_>>();
    let joined = parse_joined_block(&lines, Revision(7));
    assert_eq!(joined.projection.markers.len(), 100_002);
    let shared = SharedParse {
        parsed: &joined.parsed,
        projection: &joined.projection,
        list_projection: None,
    };
    // The caret is off-screen inside the strong span. Prefix ownership and
    // both distant delimiters still use the same complete snapshot.
    let disclosure = Some(SourceRange::empty(lines[1].range.start.0 + 3));
    for line in &lines[99_950..] {
        let presented = present_markdown_from_parse(
            line.line as u64,
            Revision(7),
            line.range,
            line.text,
            26.0,
            disclosure,
            &shared,
        );
        assert_eq!(presented.visual_text, line.text);
        assert!(
            presented
                .style_runs
                .iter()
                .any(|run| run.kind == StyleKind::Bold)
        );
        assert!(segments_tile_range(
            line.range,
            &presented.source_map.segments
        ));
        assert!(
            presented.source_map.segments.capacity() <= 5,
            "mapping storage must depend on this line's markers, not all 100,002"
        );
        let hidden = present_markdown_from_parse(
            line.line as u64,
            Revision(7),
            line.range,
            line.text,
            26.0,
            None,
            &shared,
        );
        assert_eq!(
            hidden.visual_text,
            if line.line == 99_999 {
                "closing\n"
            } else {
                "plain\n"
            }
        );
    }
}

#[test]
fn plain_presentation_preserves_markdown_source_bytes() {
    let text = "**日本語**";
    let block = present_plain(3, Revision(2), SourceRange::new(10, 10 + text.len()), text);
    assert_eq!(block.visual_text, text);
    for relative in [0, 2, 5, 8, text.len()] {
        assert_eq!(
            block
                .source_map
                .visual_to_source(VisualOffset(relative), Bias::After)
                .unwrap()
                .source_offset,
            SourceOffset(10 + relative)
        );
    }
}
#[test]
fn stale_non_overlapping_block_rebases() {
    let mut b = present_plain(0, Revision(0), SourceRange::new(4, 7), "two");
    let d = RevisionDelta {
        from_revision: Revision(0),
        to_revision: Revision(1),
        edited_source_range_before: SourceRange::empty(0),
        edited_source_range_after: SourceRange::new(0, 4),
        byte_delta: 4,
    };
    assert!(b.rebase(&[d], Revision(1)));
    assert_eq!(b.source_range, SourceRange::new(8, 11));
}

#[test]
fn external_link_target_carries_destination_and_absolute_text_range() {
    let source = "go [docs](https://example.com/) or [rel](./a.md)";
    let line = present_markdown(
        1,
        Revision(1),
        SourceRange::new(10, 10 + source.len()),
        source,
        26.0,
    );
    assert_eq!(line.links.len(), 1);
    let target = &line.links[0];
    assert_eq!(&*target.destination, "https://example.com/");
    assert_eq!(target.source_range, SourceRange::new(14, 18));
    assert!(target.contains(SourceOffset(14)));
    assert!(!target.contains(SourceOffset(18)));
}

#[test]
fn heading_reference_link_keeps_destination_defined_on_another_line() {
    let source = "# [docs][ref]\n\n[ref]: https://example.com\n";
    let index = BlockIndex::build(Revision(1), source);
    let block = index.block(0).expect("heading block");
    let projection = index.list_projection(&block).expect("formal link projection");
    let line_range = SourceRange::new(0, "# [docs][ref]\n".len());
    let lines = [BlockLine {
        line: 0,
        range: line_range,
        text: &source[..line_range.end.0],
        disclosure: None,
    }];
    let visual = present_block_with_list_projection(
        &block,
        Revision(1),
        &BlockWindow {
            span: 0..2,
            trailing_blank_lines: 1,
            lines: &lines,
            clipped_fence_lines: &[],
            zero_height_fence_rows_before: 0,
            zero_height_fence_rows_after: 0,
            table_delimiter_line: None,
            render: 0..1,
            joined: None,
            block_disclosure: None,
        },
        26.0,
        Some(projection),
    );

    let links = &visual.lines[0].links;
    assert_eq!(links.len(), 1);
    assert_eq!(&*links[0].destination, "https://example.com");
    let text_start = source.find("docs").expect("link text");
    assert_eq!(
        links[0].source_range,
        SourceRange::new(text_start, text_start + "docs".len())
    );
}

#[test]
fn table_row_reference_link_keeps_destination_defined_on_another_line() {
    let source = "| a | b |\n| --- | --- |\n| [docs][ref] | x |\n\n[ref]: https://example.com\n";
    let index = BlockIndex::build(Revision(1), source);
    let block = index.block(0).expect("table block");
    let list_projection = index.list_projection(&block);
    let table_projection = index.table_projection(&block).expect("table projection");
    let header_end = "| a | b |\n".len();
    let delimiter_end = header_end + "| --- | --- |\n".len();
    let row_end = delimiter_end + "| [docs][ref] | x |\n".len();
    let ranges = [
        SourceRange::new(0, header_end),
        SourceRange::new(header_end, delimiter_end),
        SourceRange::new(delimiter_end, row_end),
    ];
    let lines = ranges
        .iter()
        .enumerate()
        .map(|(line, range)| BlockLine {
            line,
            range: *range,
            text: &source[range.start.0..range.end.0],
            disclosure: None,
        })
        .collect::<Vec<_>>();
    let visual = present_block_with_table_projection(
        &block,
        Revision(1),
        &BlockWindow {
            span: 0..4,
            trailing_blank_lines: 1,
            lines: &lines,
            clipped_fence_lines: &[],
            zero_height_fence_rows_before: 0,
            zero_height_fence_rows_after: 0,
            table_delimiter_line: Some(1),
            render: 2..3,
            joined: None,
            block_disclosure: None,
        },
        26.0,
        list_projection,
        Some(table_projection),
    );

    let links = &visual.lines[0].links;
    assert_eq!(links.len(), 1);
    assert_eq!(&*links[0].destination, "https://example.com");
    let text_start = source.find("docs").expect("link text");
    assert_eq!(
        links[0].source_range,
        SourceRange::new(text_start, text_start + "docs".len())
    );
}

#[test]
fn rebase_moves_link_targets_with_the_source_map() {
    let source = "go [docs](https://example.com/)";
    let mut line = present_markdown(
        1,
        Revision(1),
        SourceRange::new(10, 10 + source.len()),
        source,
        26.0,
    );
    let delta = RevisionDelta {
        from_revision: Revision(1),
        to_revision: Revision(2),
        edited_source_range_before: SourceRange::empty(0),
        edited_source_range_after: SourceRange::new(0, 5),
        byte_delta: 5,
    };
    assert!(line.rebase(&[delta], Revision(2)));
    assert_eq!(line.links[0].source_range, SourceRange::new(19, 23));

    // A delta that overlaps the link text cannot be transformed, so the
    // cached presentation is rejected instead of keeping a stale target.
    let mut line = present_markdown(
        1,
        Revision(1),
        SourceRange::new(10, 10 + source.len()),
        source,
        26.0,
    );
    let overlapping = RevisionDelta {
        from_revision: Revision(1),
        to_revision: Revision(2),
        edited_source_range_before: SourceRange::new(15, 17),
        edited_source_range_after: SourceRange::new(15, 18),
        byte_delta: 1,
    };
    assert!(!line.rebase(&[overlapping], Revision(2)));
}

#[test]
fn phase3_presentation_hides_markers_without_changing_source() {
    let source = "## Hello **太字** and _italic_ with `code`";
    let block = present_markdown(
        5,
        Revision(3),
        SourceRange::new(40, 40 + source.len()),
        source,
        26.0,
    );
    assert_eq!(block.visual_text, "Hello 太字 and italic with code");
    assert_eq!(block.kind, BlockKind::Heading(2));
    for kind in [StyleKind::Bold, StyleKind::Italic, StyleKind::InlineCode] {
        assert!(block.style_runs.iter().any(|run| run.kind == kind));
    }
    assert_eq!(
        block
            .source_map
            .visual_to_source(VisualOffset(0), Bias::After)
            .unwrap()
            .source_offset,
        SourceOffset(43)
    );
    assert!(
        block
            .source_map
            .segments
            .iter()
            .any(|segment| segment.visibility == Visibility::HiddenMarkup)
    );
    assert!(block.height() > 26.0);
}

/// Whether some style run of `kind` fully covers `range`, the same rule
/// `hane_ui::inline_display_for` applies when combining runs for one
/// stretch of visual text.
fn style_run_covers(style_runs: &[StyleRun], kind: StyleKind, range: &Range<usize>) -> bool {
    style_runs.iter().any(|run| {
        run.kind == kind
            && range.start >= run.visual_range.start.0
            && range.end <= run.visual_range.end.0
    })
}

#[test]
fn italic_and_bold_style_runs_fully_cover_cjk_text() {
    // Both `*text*`/`_text_` delimiters and the `***text***` combination,
    // matching the ASCII coverage already asserted for Bold in
    // `phase3_presentation_hides_markers_without_changing_source`, but
    // with CJK-only emphasis content: the render policy must not depend
    // on the emphasized text being ASCII.
    let source = "*漢字ひらがな* and _漢字ひらがな_ and ***漢字ひらがな***";
    let block = present_markdown(
        0,
        Revision(0),
        SourceRange::new(0, source.len()),
        source,
        16.0,
    );
    assert_eq!(
        block.visual_text,
        "漢字ひらがな and 漢字ひらがな and 漢字ひらがな"
    );

    let cjk = "漢字ひらがな";
    let mut occurrences = block.visual_text.match_indices(cjk).map(|(i, _)| i);

    let star = occurrences.next().unwrap();
    let star_range = star..star + cjk.len();
    assert!(style_run_covers(
        &block.style_runs,
        StyleKind::Italic,
        &star_range
    ));
    assert!(!style_run_covers(
        &block.style_runs,
        StyleKind::Bold,
        &star_range
    ));

    let underscore = occurrences.next().unwrap();
    let underscore_range = underscore..underscore + cjk.len();
    assert!(style_run_covers(
        &block.style_runs,
        StyleKind::Italic,
        &underscore_range
    ));

    let triple = occurrences.next().unwrap();
    let triple_range = triple..triple + cjk.len();
    assert!(style_run_covers(
        &block.style_runs,
        StyleKind::Italic,
        &triple_range
    ));
    assert!(style_run_covers(
        &block.style_runs,
        StyleKind::Bold,
        &triple_range
    ));
}

#[test]
fn disclosure_expands_only_the_active_inline_construct() {
    let source = "**one** and _two_";
    let range = SourceRange::new(20, 20 + source.len());
    let block = present_markdown_with_disclosure(
        1,
        Revision(4),
        range,
        source,
        26.0,
        Some(SourceRange::empty(23)),
    );
    assert_eq!(block.visual_text, "**one** and two");
    assert!(block.source_map.segments.iter().any(|segment| {
        segment.visibility == Visibility::ExpandedMarkup
            && segment.source_range == SourceRange::new(20, 22)
    }));
    assert!(block.source_map.segments.iter().any(|segment| {
        segment.visibility == Visibility::HiddenMarkup
            && segment.source_range == SourceRange::new(32, 33)
    }));
}






#[test]
fn code_padding_inside_containers_preserves_shared_projection_and_source() {
    for (source, expected) in [
        ("> `\n> x\n> `", vec!["", "x", ""]),
        ("> `\r\n> x\r\n> `", vec!["", "x", ""]),
        ("> > `\n> > x\n> > `", vec!["", "x", ""]),
        ("> ` \n>  `", vec![" ", " "]),
        ("> ` x\n> y `", vec!["x", "y"]),
        // The list item's opening line carries the synthesized bullet
        // marker even while inactive (#126 contract), unlike the quote
        // fixtures above whose `>` markup is real source text instead.
        ("- `\n  x\n  `", vec!["• ", "x", ""]),
    ] {
        let mut offset = 50;
        let lines: Vec<_> = source
            .split_inclusive('\n')
            .enumerate()
            .map(|(line, text)| {
                let start = offset;
                offset += text.len();
                BlockLine {
                    line,
                    range: SourceRange::new(start, offset),
                    text,
                    disclosure: None,
                }
            })
            .collect();
        let block = IndexedBlock {
            ordinal: 0,
            id: BlockId(0),
            kind: if source.starts_with('>') {
                NodeKind::Quote
            } else {
                NodeKind::List { start: None }
            },
            source_range: SourceRange::new(50, offset),
            revision: Revision(1),
            confidence: Confidence::Formal,
            line_count: lines.len(),
            leading_content_lines: 0,
        };
        let joined = parse_joined_block(&lines, Revision(1));
        let window = BlockWindow {
            span: 0..lines.len(),
            trailing_blank_lines: 0,
            lines: &lines,
            clipped_fence_lines: &[],
            zero_height_fence_rows_before: 0,
            zero_height_fence_rows_after: 0,
            table_delimiter_line: None,
            render: 0..lines.len(),
            joined: Some(&joined),
            block_disclosure: None,
        };
        let wide = present_block(&block, Revision(1), &window, 26.0);
        for (index, line) in wide.lines.iter().enumerate() {
            assert_eq!(
                line.visual_text, expected[index],
                "{source:?}, line {index}"
            );
            assert_ne!(line.kind, BlockKind::Unsupported);
            assert!(segments_tile_range(
                lines[index].range,
                &line.source_map.segments
            ));
            let narrow_window = BlockWindow {
                lines: &lines[index..index + 1],
                render: index..index + 1,
                ..window.clone()
            };
            let narrow = present_block(&block, Revision(1), &narrow_window, 26.0);
            assert_eq!(
                narrow.lines[0].source_map.segments,
                line.source_map.segments
            );
        }
        let active_window = BlockWindow {
            block_disclosure: Some(SourceRange::empty(50 + source.find('`').unwrap() + 1)),
            ..window
        };
        let active = present_block(&block, Revision(1), &active_window, 26.0);
        for (index, line) in active.lines.iter().enumerate() {
            assert_eq!(
                line.visual_text,
                lines[index].text.trim_end_matches(['\r', '\n'])
            );
        }
    }
}

#[test]
fn average_line_height_ignores_collapsed_fence_rows() {
    let opening = present_fenced_code_opening_line(
        0,
        Revision(1),
        SourceRange::new(0, 4),
        "```\n",
        26.0,
        None,
    );
    let body = present_fenced_code_content_line(
        1,
        Revision(1),
        SourceRange::new(4, 9),
        "code\n",
        26.0,
    );
    let closing = present_fenced_code_closing_line(
        2,
        Revision(1),
        SourceRange::new(9, 13),
        "```\n",
        26.0,
        None,
    );
    assert_eq!(opening.height(), 0.0);
    assert!(body.height() > 0.0);
    assert_eq!(closing.height(), 0.0);

    let block = VisualBlock {
        id: BlockId(0),
        kind: BlockKind::CodeBlock,
        source_range: SourceRange::new(0, 13),
        revision: Revision(1),
        confidence: Confidence::Formal,
        span: 0..3,
        lines: vec![opening, body, closing],
        table_projection: None,
        lines_before: 0,
        lines_after: 0,
        zero_height_lines_before: 0,
        zero_height_lines_after: 0,
        line_height: 26.0,
    };
    let layout = layout_block(&block, 400.0, &testing::FixedAdvanceShaper::default());

    assert_eq!(
        layout.average_line_height(),
        Some(code_line_height(26.0)),
        "collapsed fence rows must not lower the visual-row estimate"
    );
}

#[test]
fn quote_disclosure_does_not_expand_inactive_nested_quote_or_inline_markers() {
    let source = "> outer\n> > **nested**";
    let start = 40;
    let visual = present_markdown_with_disclosure(
        0,
        Revision(1),
        SourceRange::new(start, start + source.len()),
        source,
        26.0,
        Some(SourceRange::empty(start + 4)),
    );
    assert_eq!(visual.visual_text, "> outer\n> nested");
}

#[test]
fn list_marker_discloses_for_every_ancestor_whose_source_range_the_caret_enters() {
    // A nested/mixed fixture (#126): an outer item whose own directly-owned
    // content is interrupted by a nested child list with its own second
    // paragraph, followed by the outer item's own second paragraph, then
    // an unrelated top-level sibling.
    let source = "- outer\n\n  - inner\n\n    second\n\n  outer second\n\n- sibling";
    let start = 50;
    let range = SourceRange::new(start, start + source.len());
    let outer_line = 0;
    let inner_line = 2;
    let sibling_line = 8;
    let line = |visual_text: &str, index: usize| -> String {
        visual_text.split('\n').nth(index).unwrap().to_string()
    };
    let present = |disclosure: Option<SourceRange>| {
        present_markdown_with_disclosure(0, Revision(1), range, source, 26.0, disclosure)
    };

    // Nothing disclosed: both markers collapse to their synthesized bullet.
    let collapsed = present(None);
    assert_eq!(line(&collapsed.visual_text, outer_line), "\u{2022} outer");
    assert_eq!(line(&collapsed.visual_text, inner_line), "\u{2022} inner");
    assert_eq!(
        line(&collapsed.visual_text, sibling_line),
        "\u{2022} sibling"
    );

    // A caret inside the nested child's own first line discloses both the
    // nested item's own marker and its ancestor's, since the caret's
    // source range intersects both owners' source ranges.
    let at_inner = start + source.find("inner").unwrap();
    let inner_caret = present(Some(SourceRange::empty(at_inner)));
    assert_eq!(line(&inner_caret.visual_text, outer_line), "- outer");
    assert_eq!(line(&inner_caret.visual_text, inner_line), "  - inner");
    assert_eq!(
        line(&inner_caret.visual_text, sibling_line),
        "\u{2022} sibling"
    );

    // A selection inside the nested child's own second paragraph — not on
    // the marker's own line — still discloses the nested item's marker and
    // every ancestor whose source range contains that paragraph.
    let second_start = start + source.find("second").unwrap();
    let second_selection = present(Some(SourceRange::new(
        second_start,
        second_start + "second".len(),
    )));
    assert_eq!(line(&second_selection.visual_text, outer_line), "- outer");
    assert_eq!(line(&second_selection.visual_text, inner_line), "  - inner");
    assert_eq!(
        line(&second_selection.visual_text, sibling_line),
        "\u{2022} sibling"
    );

    // A caret back in the outer item's own paragraph, past the nested
    // child, discloses only the outer marker: the caret's source range no
    // longer intersects the nested item's own source range.
    let at_outer_second = start + source.find("outer second").unwrap();
    let outer_second_caret = present(Some(SourceRange::empty(at_outer_second)));
    assert_eq!(line(&outer_second_caret.visual_text, outer_line), "- outer");
    assert_eq!(
        line(&outer_second_caret.visual_text, inner_line),
        "  \u{2022} inner"
    );
    assert_eq!(
        line(&outer_second_caret.visual_text, sibling_line),
        "\u{2022} sibling"
    );

    // A caret in the unrelated sibling item discloses only its own
    // marker, never the outer or nested ancestors of a different subtree.
    let at_sibling = start + source.find("sibling").unwrap();
    let sibling_caret = present(Some(SourceRange::empty(at_sibling)));
    assert_eq!(
        line(&sibling_caret.visual_text, outer_line),
        "\u{2022} outer"
    );
    assert_eq!(
        line(&sibling_caret.visual_text, inner_line),
        "\u{2022} inner"
    );
    assert_eq!(line(&sibling_caret.visual_text, sibling_line), "- sibling");
}

#[test]
fn list_item_boundary_discloses_only_the_item_that_starts_there() {
    let source = "- first\n- second";
    let range = SourceRange::new(0, source.len());
    let second_start = source.find("- second").expect("second list item");
    let presented = present_markdown_with_disclosure(
        0,
        Revision(1),
        range,
        source,
        26.0,
        Some(SourceRange::empty(second_start)),
    );
    let lines = presented.visual_text.split('\n').collect::<Vec<_>>();

    assert_eq!(lines, &["• first", "- second"]);
}

#[test]
fn list_structural_prefix_synthesizes_columns_and_discloses_the_owning_items_raw_bytes() {
    // A two-digit ordered marker ("10. ") is four columns wide; the
    // continuation line reaches that width with a single leading tab, so
    // the inactive synthesized replacement (four plain spaces) and the
    // disclosed original byte (one tab character) are visibly different,
    // unlike the common all-spaces case.
    let source = "10. item\n\tcontinued\n";
    let start = 40;
    let range = SourceRange::new(start, start + source.len());
    let line = |visual_text: &str, index: usize| -> String {
        visual_text.split('\n').nth(index).unwrap().to_string()
    };
    let present = |disclosure: Option<SourceRange>| {
        present_markdown_with_disclosure(0, Revision(1), range, source, 26.0, disclosure)
    };

    let collapsed = present(None);
    assert_eq!(line(&collapsed.visual_text, 1), "continued");

    let at_continued = start + source.find("continued").unwrap();
    let disclosed = present(Some(SourceRange::empty(at_continued)));
    assert_eq!(line(&disclosed.visual_text, 1), "\tcontinued");
}

#[test]
fn opening_list_structural_prefix_uses_source_mapping_without_fake_text() {
    let source = "  - item\n";
    let start = 40;
    let range = SourceRange::new(start, start + source.len());
    let collapsed = present_markdown_with_disclosure(0, Revision(1), range, source, 26.0, None);
    assert_eq!(collapsed.visual_text, "• item\n");
    let prefix = collapsed
        .list
        .as_ref()
        .and_then(|list| list.structural_prefixes.first())
        .expect("opening indentation is owned by the item");
    assert_eq!(prefix.source_range, SourceRange::new(start, start + 2));
    assert_eq!(prefix.columns, 2);

    let item = start + source.find("item").expect("item in source");
    let disclosed = present_markdown_with_disclosure(
        0,
        Revision(1),
        range,
        source,
        26.0,
        Some(SourceRange::empty(item)),
    );
    assert_eq!(disclosed.visual_text, source);
}

#[test]
fn opening_list_marker_padding_is_hidden_but_disclosed_with_the_item() {
    for (source, expected) in [("-   item\n", "• item\n"), ("1.    item\n", "1. item\n")] {
        let start = 40;
        let range = SourceRange::new(start, start + source.len());
        let collapsed =
            present_markdown_with_disclosure(0, Revision(1), range, source, 26.0, None);
        assert_eq!(collapsed.visual_text, expected, "source: {source:?}");

        let item = start + source.find("item").expect("item in source");
        let disclosed = present_markdown_with_disclosure(
            0,
            Revision(1),
            range,
            source,
            26.0,
            Some(SourceRange::empty(item)),
        );
        assert_eq!(disclosed.visual_text, source, "source: {source:?}");
    }
}

#[test]
fn ordered_list_labels_use_owner_start_plus_sibling_position() {
    for (source, expected_labels) in [
        // Loose/irregular source digits never leak into the synthesized
        // label; only the first item's marker sets the owner's `start`,
        // and later items number sequentially from it regardless of
        // their own written digits.
        ("3. a\n1. b\n1. c", vec!["3. a", "4. b", "5. c"]),
        // `0` is a valid ordered-list start.
        ("0. a\n0. b", vec!["0. a", "1. b"]),
    ] {
        let start = 40;
        let range = SourceRange::new(start, start + source.len());
        let presented =
            present_markdown_with_disclosure(0, Revision(1), range, source, 26.0, None);
        let lines: Vec<_> = presented.visual_text.split('\n').collect();
        assert_eq!(lines, expected_labels, "source: {source:?}");
    }
}

#[test]
fn nested_lists_number_independently_of_their_ancestors_and_siblings() {
    // A mixed fixture: an ordered outer list whose first item contains a
    // nested bullet list, followed by the outer list's second item. The
    // nested items are siblings of each other only, and the outer
    // second item's ordinal must not count the nested items at all.
    let source = "3. outer-a\n   - nested-a\n   - nested-b\n1. outer-b";
    let start = 40;
    let range = SourceRange::new(start, start + source.len());
    let presented = present_markdown_with_disclosure(0, Revision(1), range, source, 26.0, None);
    let lines: Vec<_> = presented.visual_text.split('\n').collect();
    assert_eq!(
        lines,
        vec![
            "3. outer-a",
            "\u{2022} nested-a",
            "\u{2022} nested-b",
            "4. outer-b",
        ]
    );
}


// The old transient list-editing geometry tests were superseded by the
// source-first planner and UI contract tests.




#[test]
fn list_item_label_returns_none_when_the_ordinal_table_omits_the_item() {
    // `list_item_label` must read only the precomputed ordinal table, not
    // fall back to `MarkdownTree::list_item_ordinal`'s preceding-sibling
    // scan when an entry is missing — a fallback here would mask a
    // construction gap and silently reintroduce a per-line sibling walk.
    let source = "1. a\n2. b\n3. c\n";
    let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
    let (item, _) = parsed
        .tree
        .blocks()
        .find(|(_, node)| matches!(node.kind, NodeKind::ListItem { .. }))
        .expect("a list item");
    let populated = list_item_ordinals(&parsed.tree);
    assert_eq!(
        list_item_label(&parsed, &populated, item),
        Some("1. ".to_owned())
    );
    let empty = vec![None; parsed.tree.len()];
    assert_eq!(list_item_label(&parsed, &empty, item), None);
}

#[test]
fn large_ordered_list_viewport_labels_reuse_one_shared_parse_at_middle_and_tail() {
    // 100,000 flat items in one ordered list. If a visible item's label
    // still re-derived its ordinal via `MarkdownTree::list_item_ordinal`
    // per line (a scan of every preceding sibling), presenting only the
    // handful of lines drawn from the middle and the tail below would
    // still be correct but would cost proportional to each item's
    // position rather than to the lines actually presented; this test
    // fixes correctness at that position, and
    // `list_item_label_returns_none_when_the_ordinal_table_omits_the_item`
    // fixes structurally that no such fallback scan exists at all.
    const ITEMS: usize = 100_000;
    let start_value = 3u64;
    let source = (0..ITEMS)
        .map(|i| format!("{start_value}. item {i}\n"))
        .collect::<String>();
    let mut cursor = 40;
    let lines = source
        .split_inclusive('\n')
        .enumerate()
        .map(|(line, text)| {
            let range = SourceRange::new(cursor, cursor + text.len());
            cursor = range.end.0;
            BlockLine {
                line,
                range,
                text,
                disclosure: None,
            }
        })
        .collect::<Vec<_>>();
    let joined = parse_joined_block(&lines, Revision(9));
    let shared = SharedParse {
        parsed: &joined.parsed,
        projection: &joined.projection,
        list_projection: None,
    };
    let middle = ITEMS / 2;
    for &index in &[
        middle - 2,
        middle - 1,
        middle,
        ITEMS - 3,
        ITEMS - 2,
        ITEMS - 1,
    ] {
        let line = &lines[index];
        let presented = present_markdown_from_parse(
            line.line as u64,
            Revision(9),
            line.range,
            line.text,
            26.0,
            None,
            &shared,
        );
        assert_eq!(
            presented.visual_text,
            format!("{}. item {index}\n", start_value + index as u64),
            "item {index}"
        );
        assert!(segments_tile_range(
            line.range,
            &presented.source_map.segments
        ));
        assert!(
            presented.source_map.segments.capacity() <= 5,
            "mapping storage must depend on this line's own markers, not item {index}'s position"
        );
    }
}

#[test]
fn quote_disclosure_uses_prefix_ownership_across_viewports() {
    let texts = ["> **first**\n", "> second"];
    let start = 50;
    let split = start + texts[0].len();
    let lines = [
        BlockLine {
            line: 0,
            range: SourceRange::new(start, split),
            text: texts[0],
            disclosure: None,
        },
        BlockLine {
            line: 1,
            range: SourceRange::new(split, split + texts[1].len()),
            text: texts[1],
            disclosure: None,
        },
    ];
    let block = IndexedBlock {
        ordinal: 0,
        id: BlockId(0),
        kind: NodeKind::Quote,
        source_range: SourceRange::new(start, lines[1].range.end.0),
        revision: Revision(1),
        confidence: Confidence::Formal,
        line_count: 2,
        leading_content_lines: 0,
    };
    let joined = parse_joined_block(&lines, Revision(1));
    // Empty ranges represent carets; non-empty ranges also cover selection
    // and IME disclosure. Neither should disclose the inactive strong span.
    for disclosure in [
        SourceRange::empty(split + 4),
        SourceRange::new(split + 3, split + 6),
    ] {
        let wide_window = BlockWindow {
            span: 0..2,
            trailing_blank_lines: 0,
            lines: &lines,
            clipped_fence_lines: &[],
            zero_height_fence_rows_before: 0,
            zero_height_fence_rows_after: 0,
            table_delimiter_line: None,
            render: 0..2,
            joined: Some(&joined),
            block_disclosure: Some(disclosure),
        };
        let wide = present_block(&block, Revision(1), &wide_window, 26.0);
        assert_eq!(wide.lines[0].visual_text, "> first");
        assert_eq!(wide.lines[1].visual_text, "> second");
        for index in 0..2 {
            let narrow_window = BlockWindow {
                lines: &lines[index..index + 1],
                render: index..index + 1,
                ..wide_window.clone()
            };
            let narrow = present_block(&block, Revision(1), &narrow_window, 26.0);
            assert_eq!(narrow.lines[0].visual_text, wide.lines[index].visual_text);
            assert_eq!(
                narrow.lines[0].source_map.segments,
                wide.lines[index].source_map.segments
            );
        }
    }
}


#[test]
fn disclosure_on_one_line_of_a_shared_construct_reaches_every_line_that_carries_it() {
    // A caret on the line that owns a shared Strong's opening `**` must also
    // expand the closing `**` on the other physical line: the two markers
    // belong to one active construct, so a partial disclosure would show one
    // marker and hide the other for the same bold run.
    let line0 = "This is **bold\n";
    let line1 = "across lines** ok";
    let start = 50;
    let range0 = SourceRange::new(start, start + line0.len());
    let range1 = SourceRange::new(range0.end.0, range0.end.0 + line1.len());
    let caret = range0.start.0 + line0.find("bold").unwrap();
    let lines = [
        BlockLine {
            line: 0,
            range: range0,
            text: line0,
            disclosure: Some(SourceRange::empty(caret)),
        },
        BlockLine {
            line: 1,
            range: range1,
            text: line1,
            disclosure: None,
        },
    ];
    let block = IndexedBlock {
        ordinal: 0,
        id: BlockId(0),
        kind: NodeKind::Paragraph,
        source_range: SourceRange::new(range0.start.0, range1.end.0),
        revision: Revision(1),
        confidence: Confidence::Formal,
        line_count: 2,
        leading_content_lines: 0,
    };
    let window = BlockWindow {
        span: 0..2,
        trailing_blank_lines: 0,
        lines: &lines,
        clipped_fence_lines: &[],
        zero_height_fence_rows_before: 0,
        zero_height_fence_rows_after: 0,
        table_delimiter_line: None,
        render: 0..2,
        joined: None,
        block_disclosure: None,
    };
    let visual = present_block(&block, Revision(1), &window, 26.0);
    let closing_marker = range1.start.0 + line1.find("**").unwrap();
    assert!(
        visual.lines[1].source_map.segments.iter().any(|segment| {
            segment.visibility == Visibility::ExpandedMarkup
                && segment.source_range.start.0 == closing_marker
        }),
        "the closing marker on the line without its own disclosure must still expand"
    );

    // `expected_disclosures` must agree with what `present_block` itself
    // just assigned to every line of the run, including the one without
    // its own `BlockLine::disclosure` — a caller comparing a cached
    // presentation's disclosures against this instead of each line's own,
    // unmerged disclosure is what lets it recognize the cache as current
    // while the caret sits inside a joined multi-line run (see
    // `EditorView::disclosures_are_current`).
    let expected = expected_disclosures(NodeKind::Paragraph, &window);
    assert_eq!(
        expected,
        visual
            .lines
            .iter()
            .map(|line| (line.line_id as usize, line.disclosure))
            .collect::<Vec<_>>(),
    );
}

#[test]
fn standalone_image_line_inside_a_run_does_not_split_the_shared_parse() {
    // A `**`/`_`/`` ` `` pair distant enough to sit on either side of a
    // standalone image line's own physical line must still resolve as one
    // shared construct: the image line is its own presentation (rendered
    // through `present_image`, not the joined parse), but it must not act
    // as a run boundary that forces the paragraph's two halves into
    // separate, self-contained parses.
    let line0 = "This is **bold\n";
    let image_line = "![alt](dest)\n";
    let line2 = "across lines** ok";
    let start = 50;
    let range0 = SourceRange::new(start, start + line0.len());
    let range1 = SourceRange::new(range0.end.0, range0.end.0 + image_line.len());
    let range2 = SourceRange::new(range1.end.0, range1.end.0 + line2.len());
    let caret = range0.start.0 + line0.find("bold").unwrap();
    let lines = [
        BlockLine {
            line: 0,
            range: range0,
            text: line0,
            disclosure: Some(SourceRange::empty(caret)),
        },
        BlockLine {
            line: 1,
            range: range1,
            text: image_line,
            disclosure: None,
        },
        BlockLine {
            line: 2,
            range: range2,
            text: line2,
            disclosure: None,
        },
    ];
    let block = IndexedBlock {
        ordinal: 0,
        id: BlockId(0),
        kind: NodeKind::Paragraph,
        source_range: SourceRange::new(range0.start.0, range2.end.0),
        revision: Revision(1),
        confidence: Confidence::Formal,
        line_count: 3,
        leading_content_lines: 0,
    };
    let window = BlockWindow {
        span: 0..3,
        trailing_blank_lines: 0,
        lines: &lines,
        clipped_fence_lines: &[],
        zero_height_fence_rows_before: 0,
        zero_height_fence_rows_after: 0,
        table_delimiter_line: None,
        render: 0..3,
        joined: None,
        block_disclosure: None,
    };
    let visual = present_block(&block, Revision(1), &window, 26.0);
    assert_eq!(visual.lines.len(), 3);

    // The closing `**` on the far side of the image line must still be
    // recognized as the same Strong construct's marker and expand,
    // exactly as if the image line were not there.
    let closing_marker = range2.start.0 + line2.find("**").unwrap();
    assert!(
        visual.lines[2].source_map.segments.iter().any(|segment| {
            segment.visibility == Visibility::ExpandedMarkup
                && segment.source_range.start.0 == closing_marker
        }),
        "the closing marker across the image line must still expand"
    );

    // The image line itself must still go through Hane's dedicated image
    // presentation path, not the shared markdown parse.
    assert_eq!(visual.lines[1].kind, BlockKind::Image);
    assert_eq!(
        visual.lines[1].image,
        Some(ImagePresentation {
            alt: "alt".to_owned(),
            destination: "dest".to_owned(),
        })
    );
    assert_eq!(visual.lines[1].disclosure, None);

    // `expected_disclosures` must still agree with what `present_block`
    // actually assigned, including the image line's own hardcoded `None`
    // even though the run's merged disclosure (from the caret on line 0)
    // is `Some`.
    let expected = expected_disclosures(NodeKind::Paragraph, &window);
    assert_eq!(
        expected,
        visual
            .lines
            .iter()
            .map(|line| (line.line_id as usize, line.disclosure))
            .collect::<Vec<_>>(),
    );
}

#[test]
fn one_physical_line_render_window_with_cached_joined_parse_matches_a_wider_window() {
    // `presented_block` (in `hane-ui`) trims `window.lines` down to just
    // `window.render` once a cached `JoinedParse` is available (see
    // `JOIN_SYNC_LINE_BUDGET`), which can leave `disclosure_runs` a run
    // only one physical line long even though the block's own construct —
    // a `**` pair here — spans a second, off-screen line. `present_block`
    // must still resolve it against the cached whole-block parse instead
    // of falling back to a lone-line, self-contained reparse that never
    // sees the closing marker: the rendered line's marker visibility,
    // style runs and source map must come out identical whether the
    // render window is one line or both.
    let line0 = "This is **bold\n";
    let line1 = "across lines** ok";
    let start = 50;
    let range0 = SourceRange::new(start, start + line0.len());
    let range1 = SourceRange::new(range0.end.0, range0.end.0 + line1.len());
    let lines = [
        BlockLine {
            line: 0,
            range: range0,
            text: line0,
            disclosure: None,
        },
        BlockLine {
            line: 1,
            range: range1,
            text: line1,
            disclosure: None,
        },
    ];
    let block = IndexedBlock {
        ordinal: 0,
        id: BlockId(0),
        kind: NodeKind::Paragraph,
        source_range: SourceRange::new(range0.start.0, range1.end.0),
        revision: Revision(1),
        confidence: Confidence::Formal,
        line_count: 2,
        leading_content_lines: 0,
    };
    let joined = parse_joined_block(&lines, Revision(1));

    let wide_window = BlockWindow {
        span: 0..2,
        trailing_blank_lines: 0,
        lines: &lines,
        clipped_fence_lines: &[],
        zero_height_fence_rows_before: 0,
        zero_height_fence_rows_after: 0,
        table_delimiter_line: None,
        render: 0..2,
        joined: Some(&joined),
        block_disclosure: None,
    };
    let wide = present_block(&block, Revision(1), &wide_window, 26.0);

    let narrow_lines = &lines[0..1];
    let narrow_window = BlockWindow {
        span: 0..2,
        trailing_blank_lines: 0,
        lines: narrow_lines,
        clipped_fence_lines: &[],
        zero_height_fence_rows_before: 0,
        zero_height_fence_rows_after: 0,
        table_delimiter_line: None,
        render: 0..1,
        joined: Some(&joined),
        block_disclosure: None,
    };
    let narrow = present_block(&block, Revision(1), &narrow_window, 26.0);

    assert_eq!(narrow.lines.len(), 1);
    assert_eq!(narrow.lines[0].visual_text, wide.lines[0].visual_text);
    assert_eq!(narrow.lines[0].style_runs, wide.lines[0].style_runs);
    assert_eq!(
        narrow.lines[0].source_map.segments,
        wide.lines[0].source_map.segments
    );
    // Without the shared parse, line 0's lone `**` would be unmatched and
    // stay literal instead of hiding as an opening Strong marker.
    assert!(
        narrow.lines[0]
            .source_map
            .segments
            .iter()
            .any(|segment| segment.visibility == Visibility::HiddenMarkup),
        "the opening marker must resolve against the cached whole-block parse, not a lone-line reparse"
    );

    // `expected_disclosures` must agree with what `present_block` itself
    // just assigned, for the same trimmed window.
    let expected = expected_disclosures(NodeKind::Paragraph, &narrow_window);
    assert_eq!(
        expected,
        narrow
            .lines
            .iter()
            .map(|line| (line.line_id as usize, line.disclosure))
            .collect::<Vec<_>>(),
    );
}

#[test]
fn standalone_image_line_does_not_split_a_run_with_distant_delimiters() {
    // Same guarantee as `standalone_image_line_inside_a_run_does_not_split_the_shared_parse`,
    // but with several plain lines separating the image line from each
    // delimiter instead of the image sitting immediately next to both —
    // the run's shared parse must not narrow to a self-contained window
    // around the image line.
    let lines_text = [
        "This text has **bold that continues\n",
        "through several\n",
        "![alt](dest)\n",
        "more plain lines\n",
        "until it finally ends** here",
    ];
    let mut ranges = Vec::new();
    let mut cursor = 50;
    for text in &lines_text {
        let range = SourceRange::new(cursor, cursor + text.len());
        cursor = range.end.0;
        ranges.push(range);
    }
    let caret = ranges[0].start.0 + lines_text[0].find("bold").unwrap();
    let lines: Vec<BlockLine<'_>> = lines_text
        .iter()
        .zip(ranges.iter())
        .enumerate()
        .map(|(index, (text, range))| BlockLine {
            line: index,
            range: *range,
            text,
            disclosure: (index == 0).then(|| SourceRange::empty(caret)),
        })
        .collect();
    let block = IndexedBlock {
        ordinal: 0,
        id: BlockId(0),
        kind: NodeKind::Paragraph,
        source_range: SourceRange::new(ranges[0].start.0, ranges[4].end.0),
        revision: Revision(1),
        confidence: Confidence::Formal,
        line_count: lines.len(),
        leading_content_lines: 0,
    };
    let window = BlockWindow {
        span: 0..lines.len(),
        trailing_blank_lines: 0,
        lines: &lines,
        clipped_fence_lines: &[],
        zero_height_fence_rows_before: 0,
        zero_height_fence_rows_after: 0,
        table_delimiter_line: None,
        render: 0..lines.len(),
        joined: None,
        block_disclosure: None,
    };
    let visual = present_block(&block, Revision(1), &window, 26.0);
    assert_eq!(visual.lines.len(), lines.len());

    // The opening `**` (line 0) and the closing `**` (line 4) are five
    // physical lines apart with an image line between them; both must
    // still resolve as the same Strong construct's markers.
    let opening_marker = ranges[0].start.0 + lines_text[0].find("**").unwrap();
    assert!(
        visual.lines[0].source_map.segments.iter().any(|segment| {
            segment.visibility == Visibility::ExpandedMarkup
                && segment.source_range.start.0 == opening_marker
        }),
        "the opening marker before the image line must still expand"
    );
    let closing_marker = ranges[4].start.0 + lines_text[4].find("**").unwrap();
    assert!(
        visual.lines[4].source_map.segments.iter().any(|segment| {
            segment.visibility == Visibility::ExpandedMarkup
                && segment.source_range.start.0 == closing_marker
        }),
        "the closing marker after the image line must still expand"
    );

    // The image line itself still renders through the dedicated image
    // path rather than the shared markdown parse.
    assert_eq!(visual.lines[2].kind, BlockKind::Image);
    assert_eq!(
        visual.lines[2].image,
        Some(ImagePresentation {
            alt: "alt".to_owned(),
            destination: "dest".to_owned(),
        })
    );
}

#[test]
fn nested_delimiter_runs_do_not_duplicate_visual_or_source_segments() {
    let source = "***nested***";
    let block = present_markdown(
        0,
        Revision(1),
        SourceRange::new(0, source.len()),
        source,
        26.0,
    );
    assert_eq!(block.visual_text, "nested");
    assert_eq!(
        block.source_map.segments[0].source_range,
        SourceRange::new(0, 3)
    );
    assert_eq!(
        block.source_map.segments.last().unwrap().source_range,
        SourceRange::new(9, 12)
    );
}

#[test]
fn phase4_image_keeps_source_and_exposes_lazy_destination() {
    let source = "![羽のロゴ](assets/phase4-feather.svg)\n";
    let range = SourceRange::new(100, 100 + source.len());
    let block = present_polished_line(
        7,
        Revision(2),
        range,
        source,
        26.0,
        None,
        LineContext::Normal,
    );
    assert_eq!(block.kind, BlockKind::Image);
    assert_eq!(block.visual_text, "羽のロゴ");
    assert_eq!(
        block.image,
        Some(ImagePresentation {
            alt: "羽のロゴ".to_owned(),
            destination: "assets/phase4-feather.svg".to_owned(),
        })
    );
    assert!(block.source_map.segments.iter().any(|segment| {
        segment.visibility == Visibility::HiddenMarkup
            && segment.source_range.start == range.start
    }));
    let active = present_polished_line(
        7,
        Revision(2),
        range,
        source,
        26.0,
        Some(SourceRange::empty(105)),
        LineContext::Normal,
    );
    assert_eq!(active.kind, BlockKind::Paragraph);
    assert!(active.image.is_none());
    assert!(active.visual_text.contains("assets/phase4-feather.svg"));
}

#[test]
fn image_estimated_height_scales_with_the_zoomed_line_height() {
    let source = "![alt](dest)";
    let range = SourceRange::new(0, source.len());
    let normal = present_polished_line(
        0,
        Revision(1),
        range,
        source,
        26.0,
        None,
        LineContext::Normal,
    );
    let zoomed = present_polished_line(
        0,
        Revision(1),
        range,
        source,
        52.0,
        None,
        LineContext::Normal,
    );

    assert_eq!(normal.estimated_height, 190.0);
    assert_eq!(zoomed.estimated_height, 380.0);
}

#[test]
fn table_rows_expose_cells_without_fake_separator_offsets() {
    let source = "| 名前 | 値 |\n";
    let range = SourceRange::new(50, 50 + source.len());
    let block = present_polished_line(
        1,
        Revision(3),
        range,
        source,
        26.0,
        None,
        LineContext::Table,
    );
    assert_eq!(block.kind, BlockKind::TableRow);
    assert_eq!(block.visual_text, " 名前  値 \n");
    let table = block.table_row.as_ref().expect("table metadata");
    assert_eq!(table.cells.len(), 2);
    assert_eq!(table.cells[0].source_range, SourceRange::new(51, 59));
    assert_eq!(table.cells[1].source_range, SourceRange::new(60, 65));
    assert!(
        !block
            .source_map
            .segments
            .iter()
            .any(|segment| segment.visibility == Visibility::Synthesized)
    );
    let empty = present_polished_line(
        1,
        Revision(3),
        SourceRange::new(0, "| x || z |\n".len()),
        "| x || z |\n",
        26.0,
        None,
        LineContext::Table,
    );
    let empty_cells = empty
        .table_row
        .as_ref()
        .expect("empty table row")
        .cells
        .as_slice();
    assert_eq!(
        empty_cells
            .iter()
            .map(|cell| cell.column)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert!(empty_cells[1].visual_range.start == empty_cells[1].visual_range.end);
    let indented = present_table_line(
        1,
        Revision(3),
        SourceRange::new(0, "  | a | b |\n".len()),
        "  | a | b |\n",
        26.0,
        false,
        &[TableAlignment::Left, TableAlignment::Right],
    );
    let indented_cells = indented
        .table_row
        .as_ref()
        .expect("indented table row")
        .cells
        .as_slice();
    assert_eq!(indented_cells.len(), 2);
    assert_eq!(indented_cells[0].source_range.start, SourceOffset(3));
    let one_column_delimiter = present_polished_line(
        1,
        Revision(3),
        SourceRange::new(0, "| --- |\n".len()),
        "| --- |\n",
        26.0,
        None,
        LineContext::Table,
    );
    assert_eq!(one_column_delimiter.kind, BlockKind::TableDelimiter);
    let sparse = present_table_line(
        1,
        Revision(3),
        SourceRange::new(0, "| only |\n".len()),
        "| only |\n",
        26.0,
        false,
        &[TableAlignment::Default, TableAlignment::Right],
    );
    assert_eq!(
        sparse
            .table_row
            .as_ref()
            .expect("sparse table row")
            .column_count,
        2
    );
    let sparse_cells = sparse
        .table_row
        .as_ref()
        .expect("sparse table row cells")
        .cells
        .as_slice();
    assert_eq!(
        sparse_cells
            .iter()
            .map(|cell| cell.column)
            .collect::<Vec<_>>(),
        vec![0, 1]
    );
    assert!(sparse_cells[1].visual_range.start == sparse_cells[1].visual_range.end);
    assert_eq!(
        sparse_cells[1].visual_range.start.0,
        sparse.visual_text.trim_end_matches(['\r', '\n']).len()
    );
    let excess = present_table_line(
        1,
        Revision(3),
        SourceRange::new(0, "| a | 2 |\n".len()),
        "| a | 2 |\n",
        26.0,
        false,
        &[TableAlignment::Default],
    );
    let excess_table = excess.table_row.as_ref().expect("excess table row");
    assert_eq!(excess_table.column_count, 1);
    assert_eq!(excess_table.cells.len(), 1);
    assert_eq!(
        &excess.visual_text[excess_table.cells[0].visual_range.start.0
            ..excess_table.cells[0].visual_range.end.0],
        " a "
    );
    let active = present_polished_line(
        1,
        Revision(3),
        range,
        source,
        26.0,
        Some(SourceRange::empty(54)),
        LineContext::Table,
    );
    assert_eq!(active.visual_text, source);
    assert_eq!(active.kind, BlockKind::Paragraph);
}

#[test]
fn raw_source_fallback_preserves_every_byte_as_plain_text() {
    let source = "<div class=\"note\">未対応 &amp; raw</div>";
    let range = SourceRange::new(12, 12 + source.len());
    let block = present_raw_source(9, Revision(3), range, source, 26.0);
    assert_eq!(block.kind, BlockKind::Unsupported);
    assert_eq!(block.visual_text, source);
    assert!(block.style_runs.is_empty());
    assert_eq!(block.source_map.segments.len(), 1);
    assert_eq!(block.source_map.segments[0].visibility, Visibility::Visible);
    for relative in 0..=source.len() {
        if !source.is_char_boundary(relative) {
            continue;
        }
        let offset = SourceOffset(12 + relative);
        let visual = block
            .source_map
            .source_to_visual(offset, Bias::After)
            .unwrap()
            .visual_offset;
        assert_eq!(
            block
                .source_map
                .visual_to_source(visual, Bias::After)
                .unwrap()
                .source_offset,
            offset
        );
    }
}

#[test]
fn fenced_code_context_styles_the_line_as_code_without_hiding_markup() {
    let source = "let answer = **42**;\n";
    let range = SourceRange::new(30, 30 + source.len());
    let block = present_polished_line(
        2,
        Revision(1),
        range,
        source,
        26.0,
        None,
        LineContext::FencedCode,
    );
    assert_eq!(block.kind, BlockKind::CodeBlock);
    // Source is literal inside a fence: emphasis markers stay visible.
    assert_eq!(block.visual_text, source);
    assert_eq!(block.style_runs.len(), 1);
    let run = block.style_runs[0];
    assert_eq!(run.kind, StyleKind::CodeBlock);
    // The style run covers the content but stops before the trailing newline.
    assert_eq!(
        run.visual_range,
        VisualRange::new(0, source.trim_end_matches('\n').len())
    );
    assert_eq!(block.height(), 26.0);
    assert!(
        block
            .source_map
            .segments
            .iter()
            .all(|segment| segment.visibility == Visibility::Visible)
    );
}

fn fence_block_lines(
    source: &'static str,
    base: usize,
) -> (IndexedBlock, Vec<BlockLine<'static>>) {
    let mut offset = base;
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
    let block = IndexedBlock {
        ordinal: 0,
        id: BlockId(0),
        kind: NodeKind::CodeBlock,
        source_range: SourceRange::new(base, base + source.len()),
        revision: Revision(1),
        confidence: Confidence::Formal,
        line_count: lines.len(),
        leading_content_lines: 0,
    };
    (block, lines)
}

#[test]
fn present_block_hides_fence_delimiters_and_info_string_when_inactive() {
    let (block, lines) = fence_block_lines("```rust\nlet answer = 42;\n```", 40);
    let window = BlockWindow {
        trailing_blank_lines: 0,
        span: 0..lines.len(),
        render: 0..lines.len(),
        lines: &lines,
        clipped_fence_lines: &[],
        zero_height_fence_rows_before: 0,
        zero_height_fence_rows_after: 0,
        table_delimiter_line: None,
        joined: None,
        block_disclosure: None,
    };
    let visual = present_block(&block, Revision(1), &window, 26.0);
    assert_eq!(visual.lines.len(), 3);

    let opening = &visual.lines[0];
    assert_eq!(opening.kind, BlockKind::CodeBlock);
    assert_eq!(opening.visual_text, "");
    let hidden = opening
        .source_map
        .segments
        .iter()
        .find(|segment| segment.visibility == Visibility::HiddenMarkup)
        .expect("opening fence hides its own delimiter run");
    assert_eq!(hidden.source_range, SourceRange::new(40, 43));
    assert_eq!(hidden.marker_edge, Some(MarkerEdge::Opening));
    let hidden_info = opening
        .source_map
        .segments
        .iter()
        .find(|segment| segment.source_range.start == SourceOffset(43))
        .expect("the info string and trailing newline have their own mapping segment");
    assert_eq!(hidden_info.visibility, Visibility::HiddenMarkup);
    assert_eq!(hidden_info.marker_edge, None);
    assert_eq!(hidden_info.source_range, SourceRange::new(43, 48));
    assert_eq!(opening.height(), 0.0);

    let mut disclosed_lines = lines.clone();
    disclosed_lines[0].disclosure = Some(SourceRange::empty(44));
    let disclosed = present_block(
        &block,
        Revision(1),
        &BlockWindow {
            lines: &disclosed_lines,
            block_disclosure: Some(SourceRange::empty(44)),
            ..window.clone()
        },
        26.0,
    );
    assert_eq!(disclosed.lines[0].visual_text, "```rust");
    assert_eq!(disclosed.lines[0].height(), 26.0);
    assert!(
        disclosed.lines[0]
            .source_map
            .segments
            .iter()
            .all(|segment| { segment.visibility == Visibility::ExpandedMarkup })
    );

    let content = &visual.lines[1];
    assert_eq!(content.visual_text, "let answer = 42;");
    assert!(
        content
            .source_map
            .segments
            .iter()
            .all(|segment| segment.visibility == Visibility::Visible),
        "code content is never treated as markup"
    );

    let closing = &visual.lines[2];
    assert_eq!(closing.kind, BlockKind::CodeBlock);
    assert_eq!(closing.visual_text, "");
    assert_eq!(closing.source_map.segments.len(), 1);
    let closing_segment = closing.source_map.segments[0];
    assert_eq!(closing_segment.visibility, Visibility::HiddenMarkup);
    assert_eq!(closing_segment.marker_edge, Some(MarkerEdge::Closing));
    assert_eq!(closing_segment.source_range, lines[2].range);
}

#[test]
fn present_block_keeps_an_unterminated_fences_closing_lookalike_line_literal() {
    // A 3-backtick line cannot close a 4-backtick fence (CommonMark:
    // matching character, run at least as long). The fence never closes,
    // so its closing-shaped last line must stay literal code content.
    let (block, lines) = fence_block_lines("````rust\ncode\n```", 0);
    let window = BlockWindow {
        trailing_blank_lines: 0,
        span: 0..lines.len(),
        render: 0..lines.len(),
        lines: &lines,
        clipped_fence_lines: &[],
        zero_height_fence_rows_before: 0,
        zero_height_fence_rows_after: 0,
        table_delimiter_line: None,
        joined: None,
        block_disclosure: None,
    };
    let visual = present_block(&block, Revision(1), &window, 26.0);
    assert_eq!(
        visual.lines[0].visual_text, "",
        "the real opening collapses"
    );
    assert_eq!(visual.lines[1].visual_text, "code");
    let last = &visual.lines[2];
    assert_eq!(last.visual_text, "```");
    assert!(
        last.source_map
            .segments
            .iter()
            .all(|segment| segment.visibility == Visibility::Visible),
        "an unterminated fence's closing-lookalike last line is not markup"
    );
}
