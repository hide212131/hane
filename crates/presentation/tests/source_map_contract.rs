use hane_document::{Bias, Revision, SourceOffset, SourceRange};
use hane_presentation::{
    LineContext, Visibility, VisualOffset, present_markdown_with_disclosure, present_polished_line,
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
    let synthesized = table_block
        .source_map
        .segments
        .iter()
        .find(|segment| segment.visibility == Visibility::Synthesized)
        .expect("table presentation must expose synthesized separators");
    for affinity in [Bias::Before, Bias::After] {
        let normalized = table_block
            .source_map
            .normalize_visual(synthesized.visual_range.start, affinity)
            .unwrap();
        assert_eq!(
            table_block
                .source_map
                .normalize_visual(normalized, affinity),
            Some(normalized)
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
