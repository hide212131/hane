use hane_document::{Bias, Revision, SourceOffset, SourceRange};
use hane_presentation::{
    BlockKind, LineContext, Visibility, VisualOffset, present_markdown_with_disclosure,
    present_polished_line,
};

fn char_boundaries(text: &str) -> impl Iterator<Item = usize> + '_ {
    (0..=text.len()).filter(|offset| text.is_char_boundary(*offset))
}

#[test]
fn every_editable_source_boundary_round_trips_when_its_construct_is_disclosed() {
    let fixtures = [
        "plain ASCII",
        "日本語🙂e\u{301}",
        "**日本🙂**",
        "## Heading 羽",
        "before _italic_ after",
        "[`link`](https://example.com)",
        r"\*literal\*",
    ];

    for source in fixtures {
        let base = 100;
        let range = SourceRange::new(base, base + source.len());
        for relative in char_boundaries(source) {
            let source_offset = SourceOffset(base + relative);
            let block = present_markdown_with_disclosure(
                1,
                Revision(4),
                range,
                source,
                26.0,
                Some(SourceRange::empty(source_offset.0)),
            );
            let visual = block
                .source_map
                .source_to_visual(source_offset, Bias::After)
                .unwrap_or_else(|| panic!("missing source mapping at {relative} in {source:?}"))
                .visual_offset;
            let round_trip = block
                .source_map
                .visual_to_source(visual, Bias::After)
                .unwrap()
                .source_offset;
            assert_eq!(
                round_trip, source_offset,
                "source→visual→source mismatch at {relative} in {source:?}"
            );
        }
    }
}

#[test]
fn table_body_terminal_boundary_discloses_the_body_without_disclosing_the_delimiter() {
    let delimiter = "|:---|---:|\n";
    let delimiter_range = SourceRange::new(17, 17 + delimiter.len());
    let delimiter_line = present_polished_line(
        1,
        Revision(1),
        delimiter_range,
        delimiter,
        26.0,
        Some(SourceRange::empty(delimiter_range.end.0)),
        LineContext::Table,
    );
    assert_eq!(delimiter_line.kind, BlockKind::TableDelimiter);

    let body = "| 羽 | 3 |";
    let body_range = SourceRange::new(delimiter_range.end.0, delimiter_range.end.0 + body.len());
    let body_line = present_polished_line(
        2,
        Revision(1),
        body_range,
        body,
        26.0,
        Some(SourceRange::empty(body_range.end.0)),
        LineContext::Table,
    );
    assert_eq!(body_line.visual_text, body);

    let visual = body_line
        .source_map
        .source_to_visual(body_range.end, Bias::After)
        .expect("the body terminal boundary must remain mapped")
        .visual_offset;
    assert_eq!(
        body_line
            .source_map
            .visual_to_source(visual, Bias::After)
            .expect("the body terminal visual boundary must remain mapped")
            .source_offset,
        body_range.end,
    );
}

#[test]
fn code_span_padding_hides_on_display_and_discloses_with_the_span() {
    // CommonMark §6.1: content that both opens and closes on a space, and is
    // not all spaces, is displayed with one space trimmed from each end. The
    // source itself must be untouched, so the padding reappears once the
    // cursor discloses the code span, exactly like a hidden marker.
    let source = "pad ` code ` pad";
    let range = SourceRange::new(0, source.len());

    let collapsed = present_markdown_with_disclosure(1, Revision(1), range, source, 26.0, None);
    assert_eq!(collapsed.visual_text, "pad code pad");

    let disclosed = present_markdown_with_disclosure(
        1,
        Revision(1),
        range,
        source,
        26.0,
        Some(SourceRange::empty(7)),
    );
    assert_eq!(disclosed.visual_text, source);
}

#[test]
fn code_span_padding_is_left_alone_when_only_one_side_has_a_space() {
    // Trimming only applies when the content both opens and closes on a
    // space; one-sided padding, and content made only of spaces, is content.
    for source in ["`code `", "` code`", "` `"] {
        let range = SourceRange::new(0, source.len());
        let block = present_markdown_with_disclosure(1, Revision(1), range, source, 26.0, None);
        assert_eq!(
            block.visual_text,
            source[1..source.len() - 1],
            "{source:?} should show its content unchanged"
        );
    }
}

#[test]
fn unsupported_and_edge_constructs_never_lose_source() {
    // Constructs Hane does not specialize yet (raw HTML, autolinks, footnote and
    // reference-link references, task-list checkboxes, backslash escapes, HTML
    // entities). The display contract guarantees the presented block reproduces
    // the source verbatim: concatenating segment source ranges in order rebuilds
    // the original bytes, and every boundary round-trips.
    let fixtures = [
        "<div class=\"note\">raw html</div>",
        "see <https://example.com> now",
        "footnote ref[^1] here",
        "[label][ref] link",
        "- [ ] 未対応タスク",
        r"escaped \*not emphasis\* text",
        "A &amp; B entity",
        "pipes | without | delimiter",
    ];

    for source in fixtures {
        let base = 100;
        let range = SourceRange::new(base, base + source.len());
        let block = present_markdown_with_disclosure(1, Revision(4), range, source, 26.0, None);

        let reconstructed: String = block
            .source_map
            .segments
            .iter()
            .filter(|segment| !segment.source_range.is_empty())
            .map(|segment| {
                &source[segment.source_range.start.0 - base..segment.source_range.end.0 - base]
            })
            .collect();
        assert_eq!(
            reconstructed, source,
            "segments dropped source bytes for {source:?}"
        );

        for relative in char_boundaries(source) {
            let source_offset = SourceOffset(base + relative);
            let disclosed = present_markdown_with_disclosure(
                1,
                Revision(4),
                range,
                source,
                26.0,
                Some(SourceRange::empty(source_offset.0)),
            );
            let visual = disclosed
                .source_map
                .source_to_visual(source_offset, Bias::After)
                .unwrap_or_else(|| panic!("missing source mapping at {relative} in {source:?}"))
                .visual_offset;
            assert_eq!(
                disclosed
                    .source_map
                    .visual_to_source(visual, Bias::After)
                    .unwrap()
                    .source_offset,
                source_offset,
                "source→visual→source mismatch at {relative} in {source:?}"
            );
        }
    }
}

#[test]
fn hidden_and_synthesized_positions_normalize_idempotently() {
    let source = "**日本🙂** and [link](target)";
    let range = SourceRange::new(50, 50 + source.len());
    let block = present_markdown_with_disclosure(2, Revision(5), range, source, 26.0, None);

    for affinity in [Bias::Before, Bias::After] {
        for relative in char_boundaries(source) {
            let normalized = block
                .source_map
                .normalize_source(SourceOffset(50 + relative), affinity)
                .unwrap();
            assert!(source.is_char_boundary(normalized.0 - 50));
            assert_eq!(
                block.source_map.normalize_source(normalized, affinity),
                Some(normalized)
            );
        }
        for visual in char_boundaries(&block.visual_text) {
            let normalized = block
                .source_map
                .normalize_visual(VisualOffset(visual), affinity)
                .unwrap();
            assert!(block.visual_text.is_char_boundary(normalized.0));
            assert_eq!(
                block.source_map.normalize_visual(normalized, affinity),
                Some(normalized)
            );
        }
    }

    let table = "| 名前 | 値 |\n";
    let table_block = present_polished_line(
        3,
        Revision(5),
        SourceRange::new(200, 200 + table.len()),
        table,
        26.0,
        None,
        LineContext::Table,
    );
    let table = table_block.table_row.expect("table presentation metadata");
    assert_eq!(table.cells.len(), 2);
    assert!(
        !table_block
            .source_map
            .segments
            .iter()
            .any(|segment| segment.visibility == Visibility::Synthesized)
    );
    for cell in table.cells {
        for affinity in [Bias::Before, Bias::After] {
            let visual = cell.visual_range.start;
            let normalized = table_block
                .source_map
                .normalize_visual(visual, affinity)
                .unwrap();
            assert_eq!(
                table_block
                    .source_map
                    .normalize_visual(normalized, affinity),
                Some(normalized)
            );
        }
    }
}

#[test]
fn adjacent_hidden_list_and_heading_markers_normalize_stably() {
    // PR #189 / Issue #126: a list item's synthesized bullet sits directly
    // against its own nested ATX heading's hidden opening marker, with no
    // visible content between the list marker, the bullet and the heading
    // marker. A single source→visual→source round trip only walked from the
    // list marker to the heading marker's own boundary — itself not an
    // editable position — so `normalize_source` was not idempotent there.
    let source = "- ### item ###";
    let range = SourceRange::new(0, source.len());
    let block = present_markdown_with_disclosure(1, Revision(1), range, source, 26.0, None);
    assert_eq!(block.visual_text, "\u{2022} item");

    let list_marker_start = SourceOffset(0);
    let item_start = SourceOffset(source.find("item").unwrap());

    assert_eq!(
        block
            .source_map
            .normalize_source(list_marker_start, Bias::Before)
            .unwrap(),
        list_marker_start,
    );
    assert_eq!(
        block
            .source_map
            .normalize_source(list_marker_start, Bias::After)
            .unwrap(),
        item_start,
        "After affinity should walk past both hidden markers to the heading's visible text"
    );

    for affinity in [Bias::Before, Bias::After] {
        let normalized = block
            .source_map
            .normalize_source(list_marker_start, affinity)
            .unwrap();
        assert_eq!(
            block.source_map.normalize_source(normalized, affinity),
            Some(normalized),
            "normalize_source must be idempotent for {affinity:?}"
        );
    }
}

#[test]
fn visual_click_and_drag_endpoints_produce_a_source_selection() {
    let source = "before **日本🙂** after";
    let range = SourceRange::new(300, 300 + source.len());
    let block = present_markdown_with_disclosure(4, Revision(6), range, source, 26.0, None);
    assert_eq!(block.visual_text, "before 日本🙂 after");

    let visual_start = block.visual_text.find("日本").unwrap();
    let visual_end = visual_start + "日本🙂".len();
    let source_start = block
        .source_map
        .visual_to_source(VisualOffset(visual_start), Bias::After)
        .unwrap()
        .source_offset;
    let source_end = block
        .source_map
        .visual_to_source(VisualOffset(visual_end), Bias::Before)
        .unwrap()
        .source_offset;

    assert_eq!(
        &source[source_start.0 - range.start.0..source_end.0 - range.start.0],
        "日本🙂"
    );
    assert!(source_start < source_end);
}

#[test]
fn current_revision_local_presentation_wins_while_formal_context_is_stale() {
    let source = "**updated 日本語**";
    let block = present_markdown_with_disclosure(
        5,
        Revision(2),
        SourceRange::new(0, source.len()),
        source,
        26.0,
        None,
    );
    assert_eq!(block.revision, Revision(2));
    assert_eq!(block.visual_text, "updated 日本語");
    assert!(
        block
            .source_map
            .segments
            .iter()
            .any(|segment| { segment.visibility == Visibility::HiddenMarkup })
    );
}
