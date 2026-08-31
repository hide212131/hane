//! R4B layout contract: rows, not source lines, are what the caret moves through.
//!
//! Every assertion here is made without a window: the shaper advances a fixed
//! width per character, so "column 4" is a number a test can write down. What is
//! being fixed is the coordinate system — which row owns a source offset, that
//! rows tile their line, that a point maps back to the offset it came from, and
//! that vertical movement aims at an x rather than at a grapheme column.

#![allow(
    clippy::float_cmp,
    clippy::cast_precision_loss,
    reason = "layout fixtures assert exact deterministic pixel geometry"
)]

use hane_document::{Bias, LineId, RopeBuffer, SourceOffset, SourceRange, TextBuffer};
use hane_markdown::BlockIndex;
use hane_presentation::testing::FixedAdvanceShaper;
use hane_presentation::{
    BlockLayout, BlockLine, BlockWindow, LineShaper, LineWrap, VerticalMove, VisualBlock,
    block_line_span, layout_block, present_block, trailing_blank_lines,
};

const LINE_HEIGHT: f32 = 26.0;
/// Ten columns wide with the test shaper's 8 px advance.
const WIDTH: f32 = 80.0;

fn shaper() -> FixedAdvanceShaper {
    FixedAdvanceShaper::new(8.0)
}

/// Presents a whole document into blocks the way `EditorView` does: block
/// boundaries from the index, then one `present_block` call per block with all
/// of its lines.
fn present(source: &str, cursor: Option<usize>) -> Vec<VisualBlock> {
    let buffer = RopeBuffer::from_text(source);
    let index = BlockIndex::from_buffer(&buffer);
    index
        .blocks()
        .map(|block| {
            let span = block_line_span(&buffer, &block).expect("block spans lines");
            let ranges = span
                .clone()
                .map(|line| buffer.line_range(LineId(line)).expect("line in range"))
                .collect::<Vec<_>>();
            let texts = ranges
                .iter()
                .map(|range| buffer.text(*range).expect("line text"))
                .collect::<Vec<_>>();
            let lines = span
                .clone()
                .zip(&ranges)
                .zip(&texts)
                .map(|((line, range), text)| BlockLine {
                    line,
                    range: *range,
                    text,
                    disclosure: cursor
                        .filter(|cursor| range.start.0 <= *cursor && *cursor < range.end.0)
                        .map(SourceRange::empty),
                })
                .collect::<Vec<_>>();
            present_block(
                &block,
                buffer.revision(),
                &BlockWindow {
                    trailing_blank_lines: trailing_blank_lines(&buffer, &span),
                    span,
                    lines: &lines,
                },
                LINE_HEIGHT,
            )
        })
        .collect()
}

fn laid_out(source: &str) -> Vec<(VisualBlock, BlockLayout)> {
    present(source, None)
        .into_iter()
        .map(|block| {
            let layout = layout_block(&block, WIDTH, &shaper());
            (block, layout)
        })
        .collect()
}

const WRAPPED: &str = "the quick brown fox jumps over the lazy dog again and again\n";

#[test]
fn every_editable_source_offset_round_trips_through_the_layout() {
    let source = format!(
        "{WRAPPED}\n> quoted text that also wraps past the column\n\n- item one\n- item two\n\n\
         ```rust\nlet answer = 42;\n```\n\n| a | b |\n| --- | --- |\n| 1 | 2 |\n"
    );
    let shaper = shaper();
    let blocks = laid_out(&source);
    let last = blocks.len() - 1;
    for (index, (block, layout)) in blocks.iter().enumerate() {
        // A block's end offset is the next block's start; only the document end
        // belongs to the block that owns it.
        let end = block.source_range.end.0 + usize::from(index == last);
        for offset in block.source_range.start.0..end {
            if !source.is_char_boundary(offset) {
                continue;
            }
            let offset = SourceOffset(offset);
            let Some(point) = layout.point_for_source(block, offset, &shaper) else {
                continue;
            };
            let row = &layout.lines[point.row];
            assert!(
                row.source_range.start <= offset && offset <= row.source_range.end,
                "offset {offset:?} rendered on a row that does not cover it: {row:?}"
            );
            let back = layout
                .source_for_point(block, point.x, point.y, &shaper)
                .expect("a point inside the block resolves to source");
            // An offset the source map hides — every byte of a table delimiter
            // row, the inside of a collapsed marker — has no position of its
            // own; it renders where the offset it normalizes to renders. Those
            // are not editable positions, so the round trip is asserted for the
            // offsets that are.
            let line = &block.lines[row.line];
            if line.source_map.normalize_source(offset, Bias::After) != Some(offset) {
                continue;
            }
            assert_eq!(
                back, offset,
                "point at {offset:?} in {:?} resolved back to another offset",
                block.kind
            );
        }
    }
}

#[test]
fn soft_wrapped_rows_tile_their_line_and_keep_the_break_kind() {
    let (block, layout) = laid_out(WRAPPED).into_iter().next().expect("one block");
    let rows: Vec<_> = layout
        .lines
        .iter()
        .filter(|row| row.line == 0)
        .cloned()
        .collect();
    assert!(rows.len() > 1, "the line is wider than the column");

    let line = &block.lines[0];
    assert_eq!(rows[0].source_range.start, line.source_range.start);
    assert_eq!(
        rows.last().unwrap().source_range.end,
        line.source_range.end,
        "the last row carries the line's newline"
    );
    for pair in rows.windows(2) {
        assert_eq!(
            pair[0].source_range.end, pair[1].source_range.start,
            "rows tile the line's source range"
        );
        assert_eq!(
            pair[0].line_visual_range.end, pair[1].line_visual_range.start,
            "rows tile the line's visual text"
        );
        assert!(
            pair[0].visual_range.end.0 < pair[1].visual_range.end.0,
            "block-local visual offsets are ordered"
        );
    }
    // A break that no source byte stands for is soft; the one the newline makes
    // is hard, and only that one owns the trailing caret position.
    assert!(
        rows[..rows.len() - 1]
            .iter()
            .all(|row| row.wrap == LineWrap::Soft)
    );
    assert_eq!(rows.last().unwrap().wrap, LineWrap::Hard);
    assert_eq!(layout.height(), layout.lines.len() as f32 * LINE_HEIGHT);
}

#[test]
fn a_caret_at_a_wrap_point_renders_at_the_start_of_the_next_row() {
    let (block, layout) = laid_out(WRAPPED).into_iter().next().expect("one block");
    let boundary = layout.lines[0].source_range.end;
    let point = layout
        .point_for_source(&block, boundary, &shaper())
        .expect("the wrap boundary has a position");
    assert_eq!(point.row, 1, "the caret continues on the next row");
    assert_eq!(point.x, 0.0);
}

#[test]
fn multi_line_constructs_lay_out_one_row_per_source_line_when_they_fit() {
    let source = "> one\n> two\n\n- a\n- b\n\n```rs\nlet x=1;\n```\n\n| a | b |\n| - | - |\n";
    for (block, layout) in laid_out(source) {
        assert_eq!(
            layout.lines.len(),
            block.lines.len(),
            "{:?} should not wrap at this width",
            block.kind
        );
        assert!(layout.lines.iter().all(|row| row.wrap == LineWrap::Hard));
        assert!(
            layout
                .lines
                .iter()
                .enumerate()
                .all(|(index, row)| row.line == index && row.fragment == 0)
        );
        // Rows tile the block: every source byte belongs to exactly one row.
        assert_eq!(
            layout.lines.first().map(|row| row.source_range.start),
            Some(block.source_range.start)
        );
        assert_eq!(
            layout.lines.last().map(|row| row.source_range.end),
            Some(block.source_range.end)
        );
    }
}

#[test]
fn vertical_movement_follows_rows_and_keeps_the_preferred_x() {
    let (block, layout) = laid_out(WRAPPED).into_iter().next().expect("one block");
    let shaper = shaper();
    // Four columns into the first row, which is inside the first source line.
    let start = layout
        .source_at_x(&block, 0, 4.0 * 8.0, &shaper)
        .expect("a column on the first row");
    let down = layout.vertical_target(&block, start, true, 4.0 * 8.0, &shaper);
    let VerticalMove::To(next) = down else {
        panic!("moving down inside a wrapped line stays in the block: {down:?}");
    };
    assert_eq!(
        layout
            .point_for_source(&block, next, &shaper)
            .map(|point| (point.row, point.x)),
        Some((1, 4.0 * 8.0)),
        "the caret lands on the next row at the same x"
    );
    // Both offsets are on the same physical source line: the row moved, the
    // line did not.
    assert_eq!(layout.lines[0].line, layout.lines[1].line);
    // And back up returns to where it started.
    assert_eq!(
        layout.vertical_target(&block, next, false, 4.0 * 8.0, &shaper),
        VerticalMove::To(start)
    );
}

#[test]
fn a_short_row_clamps_the_caret_without_losing_the_preferred_x() {
    // Second line is shorter than the first: passing through it must not pull
    // the caret to its end permanently.
    let source = "aaaa bbbb cccc dddd\nxy\nzzzz wwww vvvv uuuu\n";
    let (block, layout) = laid_out(source).into_iter().next().expect("one block");
    let shaper = shaper();
    let x = 5.0 * 8.0;
    let first = layout.source_at_x(&block, 0, x, &shaper).unwrap();
    let mut caret = first;
    let mut rows = vec![layout.row_for_source(caret).unwrap()];
    for _ in 0..3 {
        let VerticalMove::To(next) = layout.vertical_target(&block, caret, true, x, &shaper) else {
            break;
        };
        caret = next;
        rows.push(layout.row_for_source(caret).unwrap());
    }
    assert_eq!(rows, vec![0, 1, 2, 3], "each move advances exactly one row");
    // The short middle line clamped the caret, but the x the moves aim at is the
    // caller's, so the row after it is reached at the original column again.
    let landed = layout.point_for_source(&block, caret, &shaper).unwrap();
    assert_eq!(landed.x, x);
}

#[test]
fn the_first_and_last_row_report_the_block_edge() {
    let source = "one\n\ntwo\n";
    let blocks = laid_out(source);
    let (block, layout) = &blocks[0];
    let shaper = shaper();
    assert_eq!(
        layout.vertical_target(block, block.source_range.start, false, 0.0, &shaper),
        VerticalMove::PastEdge,
        "moving up from the first row leaves the block"
    );
    let (block, layout) = blocks.last().unwrap();
    assert_eq!(
        layout.vertical_target(block, block.source_range.end, true, 0.0, &shaper),
        VerticalMove::PastEdge,
        "moving down from the last row leaves the block"
    );
}

/// A row's painted text never runs past the column it was laid out against;
/// otherwise a soft-wrap "row" is a lie and the viewport would need a
/// horizontal scrollbar to read it.
fn assert_rows_fit_the_column(
    block: &VisualBlock,
    layout: &BlockLayout,
    shaper: &FixedAdvanceShaper,
) {
    for row in &layout.lines {
        let line = &block.lines[row.line];
        let width = shaper.x_for_offset(
            line,
            row.line_visual_range.clone(),
            row.line_visual_range.end,
        );
        assert!(
            width <= WIDTH + f32::EPSILON,
            "row {row:?} measures {width} wide, past the {WIDTH} column"
        );
    }
}

#[test]
fn a_long_unbroken_ascii_token_wraps_instead_of_widening_the_column() {
    // No whitespace anywhere: a URL or a long identifier is one "word" as far
    // as ordinary wrapping is concerned.
    let source = "https://example.com/very/long/path/segment/that/keeps/going/and/going\n";
    let (block, layout) = laid_out(source).into_iter().next().expect("one block");
    let shaper = shaper();
    assert_rows_fit_the_column(&block, &layout, &shaper);
    let rows: Vec<_> = layout.lines.iter().filter(|row| row.line == 0).collect();
    assert!(
        rows.len() > 1,
        "an unbroken token wider than the column must still split across rows"
    );
    // Rows still tile the physical line's source range, wherever the split fell.
    assert_eq!(
        rows[0].source_range.start,
        block.lines[0].source_range.start
    );
    assert_eq!(
        rows.last().unwrap().source_range.end,
        block.lines[0].source_range.end
    );
}

#[test]
fn long_japanese_text_without_spaces_wraps_at_the_column() {
    let source = "これはとても長い日本語の文章であり空白がまったくないので通常の折り返しでは一語として扱われてしまう可能性がある\n";
    let (block, layout) = laid_out(source).into_iter().next().expect("one block");
    let shaper = shaper();
    assert_rows_fit_the_column(&block, &layout, &shaper);
    let rows: Vec<_> = layout.lines.iter().filter(|row| row.line == 0).collect();
    assert!(
        rows.len() > 1,
        "space-less Japanese prose wider than the column must still wrap"
    );
    assert_eq!(
        rows[0].source_range.start,
        block.lines[0].source_range.start
    );
    assert_eq!(
        rows.last().unwrap().source_range.end,
        block.lines[0].source_range.end
    );
}

#[test]
fn mixed_script_and_emoji_wrap_without_panicking_or_splitting_a_char_boundary() {
    let source = "helloこんにちは🎉world混在テキストsegment🚀another\n";
    let (block, layout) = laid_out(source).into_iter().next().expect("one block");
    let shaper = shaper();
    assert_rows_fit_the_column(&block, &layout, &shaper);
    let rows: Vec<_> = layout.lines.iter().filter(|row| row.line == 0).collect();
    // Rows tile the line's source range with no gap or overlap, and every
    // boundary the layout picked lands on a char boundary (a byte-split emoji
    // or kana would already have panicked the string slicing above).
    for pair in rows.windows(2) {
        assert_eq!(pair[0].source_range.end, pair[1].source_range.start);
    }
    assert_eq!(
        rows[0].source_range.start,
        block.lines[0].source_range.start
    );
    assert_eq!(
        rows.last().unwrap().source_range.end,
        block.lines[0].source_range.end
    );
}

#[test]
fn narrowing_the_column_rewraps_without_changing_the_source_it_covers() {
    let shaper = shaper();
    let blocks = present(WRAPPED, None);
    let block = &blocks[0];
    let wide = layout_block(block, WIDTH, &shaper);
    let narrow = layout_block(block, WIDTH / 2.0, &shaper);
    assert!(
        narrow.lines.len() > wide.lines.len(),
        "a narrower column must produce at least as many rows, strictly more here"
    );
    // Both layouts cover exactly the block's source range: narrowing rewraps
    // rows, it does not drop or duplicate source.
    for layout in [&wide, &narrow] {
        assert_eq!(
            layout.lines.first().map(|row| row.source_range.start),
            Some(block.source_range.start)
        );
        assert_eq!(
            layout.lines.last().map(|row| row.source_range.end),
            Some(block.source_range.end)
        );
        for pair in layout.lines.windows(2) {
            if pair[0].line == pair[1].line {
                assert_eq!(pair[0].source_range.end, pair[1].source_range.start);
            }
        }
    }
}

#[test]
fn selection_is_painted_per_row() {
    let (block, layout) = laid_out(WRAPPED).into_iter().next().expect("one block");
    let whole = block.source_range;
    let painted = (0..layout.lines.len())
        .filter_map(|row| {
            layout
                .visual_range_on_row(&block, row, whole)
                .map(|range| (row, range))
        })
        .collect::<Vec<_>>();
    let with_text = layout
        .lines
        .iter()
        .filter(|row| !row.line_visual_range.is_empty())
        .count();
    assert_eq!(
        painted.len(),
        with_text,
        "a selection over the block reaches every row that has text"
    );
    for (row, range) in painted {
        let line = &layout.lines[row];
        assert!(
            line.line_visual_range.start <= range.start && range.end <= line.line_visual_range.end,
            "row {row} painted outside its own text"
        );
    }
}
