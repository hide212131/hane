//! Boundary contract for `HeightIndex` and `anchored_scroll_y` through the
//! public entry points only. The existing unit tests exercise ordinary
//! updates, end-of-document viewports and chunk boundaries; these cover the
//! empty index, zero-height entries, negative-height clamping and the
//! distinction between a block's visual height and its height-index entry
//! that `anchored_scroll_y` relies on.

use hane_document::{Revision, SourceRange};
use hane_markdown::{BlockId, Confidence};
use hane_presentation::{
    BlockKind, HeightIndex, ScrollAnchor, VisualBlock, VisualOffset, anchored_scroll_y,
    present_markdown,
};

#[test]
fn empty_height_index_round_trips_through_splice() {
    let mut heights = HeightIndex::new(Vec::<f32>::new());
    assert_eq!(heights.len(), 0);
    assert!(heights.is_empty());
    assert_eq!(heights.height(0), None);
    assert_eq!(heights.prefix_sum(0), 0.0);
    assert_eq!(heights.total_height(), 0.0);
    assert_eq!(heights.block_at_y(0.0), 0);
    assert_eq!(heights.visible_range(0.0, 0.0, 0.0), 0..0);

    heights.splice(0..0, [5.0, 7.0]);
    assert_eq!(heights.len(), 2);
    assert_eq!(heights.total_height(), 12.0);

    heights.splice(0..2, []);
    assert_eq!(heights.len(), 0);
    assert!(heights.is_empty());
}

#[test]
fn zero_height_entries_preserve_boundary_and_eof_selection() {
    let heights = HeightIndex::new([10.0, 0.0, 20.0, 0.0]);
    assert_eq!(
        (0..=4)
            .map(|end| heights.prefix_sum(end))
            .collect::<Vec<_>>(),
        vec![0.0, 10.0, 10.0, 30.0, 30.0]
    );
    assert_eq!(heights.block_at_y(10.0), 2);
    assert_eq!(heights.block_at_y(30.0), 3);
    assert_eq!(heights.block_at_y(100.0), 3);
    assert_eq!(heights.visible_range(0.0, 30.0, 0.0), 0..4);

    let trailing_zeros = HeightIndex::new([0.0, 0.0]);
    assert_eq!(trailing_zeros.block_at_y(0.0), 1);
    assert_eq!(trailing_zeros.visible_range(0.0, 0.0, 0.0), 1..2);
}

#[test]
fn negative_height_inputs_keep_existing_clamping() {
    let mut heights = HeightIndex::new([-2.0, 4.0]);
    assert_eq!(heights.height(0), Some(0.0));
    assert_eq!(heights.height(1), Some(4.0));

    heights.update(1, -3.0);
    assert_eq!(heights.height(0), Some(0.0));
    assert_eq!(heights.height(1), Some(0.0));

    heights.splice(1..2, [-1.0, 6.0]);
    assert_eq!(heights.height(0), Some(0.0));
    assert_eq!(heights.height(1), Some(0.0));
    assert_eq!(heights.height(2), Some(6.0));
    assert_eq!(heights.total_height(), 6.0);
}

fn single_line_block(
    id: u64,
    line_id: u64,
    span: std::ops::Range<usize>,
    height: f32,
) -> VisualBlock {
    VisualBlock {
        id: BlockId(id),
        kind: BlockKind::Paragraph,
        source_range: SourceRange::empty(0),
        revision: Revision(1),
        confidence: Confidence::Formal,
        span,
        lines: vec![present_markdown(
            line_id,
            Revision(1),
            SourceRange::empty(0),
            "",
            height,
        )],
        table_projection: None,
        lines_before: 0,
        lines_after: 0,
        zero_height_lines_before: 0,
        zero_height_lines_after: 0,
        line_height: height,
    }
}

#[test]
fn scroll_anchor_keeps_identity_and_visual_height_clamping() {
    let block_a = single_line_block(1, 0, 0..1, 10.0);
    let block_b = single_line_block(2, 1, 1..2, 20.0);
    assert_eq!(block_a.height(), 10.0);
    assert_eq!(block_b.height(), 20.0);

    // The index's own entry for block B (30.0) deliberately differs from the
    // block's visual height (20.0): `anchored_scroll_y` must clamp against the
    // visual height, not the index's stored measurement.
    let blocks = [block_a, block_b];
    let heights = HeightIndex::new([10.0, 30.0]);

    let anchor = ScrollAnchor {
        block: BlockId(2),
        intra_block_y: 3.0,
        visual_position_hint: None,
    };
    assert_eq!(anchored_scroll_y(anchor, &blocks, &heights), Some(13.0));

    let negative = ScrollAnchor {
        block: BlockId(2),
        intra_block_y: -5.0,
        visual_position_hint: None,
    };
    assert_eq!(anchored_scroll_y(negative, &blocks, &heights), Some(10.0));

    let overflow = ScrollAnchor {
        block: BlockId(2),
        intra_block_y: 100.0,
        visual_position_hint: None,
    };
    assert_eq!(anchored_scroll_y(overflow, &blocks, &heights), Some(30.0));

    let hinted = ScrollAnchor {
        block: BlockId(2),
        intra_block_y: 3.0,
        visual_position_hint: Some(VisualOffset(42)),
    };
    assert_eq!(
        anchored_scroll_y(hinted, &blocks, &heights),
        anchored_scroll_y(anchor, &blocks, &heights),
        "visual_position_hint must not change the anchored scroll position"
    );

    let missing = ScrollAnchor {
        block: BlockId(99),
        intra_block_y: 0.0,
        visual_position_hint: None,
    };
    assert_eq!(anchored_scroll_y(missing, &blocks, &heights), None);
    assert_eq!(anchored_scroll_y(missing, &[], &heights), None);
}
