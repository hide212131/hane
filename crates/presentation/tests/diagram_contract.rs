//! A fenced block shown as a rendered diagram keeps every physical line.
//!
//! The first construct line owns the diagram's height and the other construct
//! lines fold to zero height, so layout, source↔point mapping and coverage all
//! come from the block's ordinary rows. Blank lines around the construct are not
//! part of it.

#![allow(
    clippy::float_cmp,
    reason = "layout fixtures assert exact deterministic pixel geometry"
)]

use hane_document::{LineId, RopeBuffer, SourceOffset, SourceRange, TextBuffer};
use hane_markdown::BlockIndex;
use hane_presentation::testing::FixedAdvanceShaper;
use hane_presentation::{
    BlockKind, BlockLine, BlockWindow, DiagramPlan, VisualBlock, apply_diagram, block_line_span,
    diagram_height, diagram_owner_line, layout_block, present_block, trailing_blank_lines,
};
use std::ops::Range;

const LINE_HEIGHT: f32 = 26.0;
const WIDTH: f32 = 400.0;
/// Intro, blank, fence, two body lines, closing fence, blank, tail.
const SOURCE: &str = "intro\n\n```mermaid\nflowchart TD\n  A --> B\n```\n\ntail\n";

fn present_code_block(render: Option<Range<usize>>) -> (RopeBuffer, VisualBlock) {
    let buffer = RopeBuffer::from_text(SOURCE);
    let index = BlockIndex::from_buffer(&buffer);
    let block = index
        .blocks()
        .find(|block| block.source_range.start.0 == "intro\n\n".len())
        .expect("the fenced block");
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
            disclosure: None,
        })
        .collect::<Vec<_>>();
    let visual = present_block(
        &block,
        buffer.revision(),
        &BlockWindow {
            trailing_blank_lines: trailing_blank_lines(&buffer, &span),
            render: render.unwrap_or_else(|| span.clone()),
            span,
            lines: &lines,
            clipped_fence_lines: &[],
            zero_height_fence_rows_before: 0,
            zero_height_fence_rows_after: 0,
            table_delimiter_line: None,
            joined: None,
            block_disclosure: None,
        },
        LINE_HEIGHT,
    );
    (buffer, visual)
}

fn plan() -> DiagramPlan {
    DiagramPlan {
        lines: 2..6,
        height: 150.0,
    }
}

#[test]
fn the_owner_line_takes_the_height_and_the_other_construct_lines_fold() {
    let (_, mut block) = present_code_block(None);
    let source_ranges = block
        .lines
        .iter()
        .map(|line| line.source_range)
        .collect::<Vec<_>>();
    assert_eq!(block.lines.len(), 5);
    let blank = block.lines[4].height();
    assert!(blank > 0.0);
    assert!(apply_diagram(&mut block, &plan()));

    let heights = block.lines.iter().map(|line| line.height()).collect::<Vec<_>>();
    assert_eq!(heights, vec![150.0, 0.0, 0.0, 0.0, blank]);
    assert_eq!(block.lines[0].kind, BlockKind::Diagram);
    assert!(block.lines[1..4].iter().all(|line| line.kind == BlockKind::CodeBlock));
    assert_eq!(diagram_height(&block), Some(150.0));
    assert_eq!(diagram_owner_line(&block), Some(2));
    // The blank line closing the block is not part of the construct.
    assert_ne!(block.lines[4].kind, BlockKind::Diagram);
    assert!(block.lines[4].estimated_height > 0.0);
    // Every physical line keeps its source range and the ranges still tile the block.
    assert_eq!(
        block.lines.iter().map(|line| line.source_range).collect::<Vec<_>>(),
        source_ranges
    );
    assert_eq!(block.lines.first().unwrap().source_range.start, block.source_range.start);
    assert_eq!(block.lines.last().unwrap().source_range.end, block.source_range.end);
    assert_eq!(block.height(), 150.0 + blank);
}

#[test]
fn layout_gives_the_diagram_height_to_one_row_and_keeps_every_row() {
    let (_, mut block) = present_code_block(None);
    assert!(apply_diagram(&mut block, &plan()));
    let shaper = FixedAdvanceShaper::new(8.0);
    let layout = layout_block(&block, WIDTH, &shaper);

    assert_eq!(layout.lines.len(), block.lines.len());
    assert_eq!(layout.height(), block.height());
    let tops = layout.lines.iter().map(|row| row.y).collect::<Vec<_>>();
    assert_eq!(tops, vec![0.0, 150.0, 150.0, 150.0, 150.0]);
    let heights = layout.lines.iter().map(|row| row.height).collect::<Vec<_>>();
    assert_eq!(&heights[..4], &[150.0, 0.0, 0.0, 0.0]);

    // A point inside the picture resolves to the owner line, deterministically.
    let owner = block.lines[0].source_range;
    for y in [0.0, 40.0, 149.0] {
        let offset = layout
            .source_for_point(&block, 20.0, y, &shaper)
            .expect("point inside the diagram");
        assert!(owner.start <= offset && offset <= owner.end, "y = {y}: {offset:?}");
    }
    // A point in the blank line after the construct is not swallowed by it.
    let after = layout
        .source_for_point(&block, 0.0, 150.0 + 5.0, &shaper)
        .expect("point in the closing blank line");
    let blank = block.lines[4].source_range;
    assert!(blank.start <= after && after <= blank.end, "{after:?}");
}

#[test]
fn every_source_offset_of_the_construct_still_has_a_row() {
    let (_, mut block) = present_code_block(None);
    assert!(apply_diagram(&mut block, &plan()));
    let layout = layout_block(&block, WIDTH, &FixedAdvanceShaper::new(8.0));
    for offset in block.source_range.start.0..block.source_range.end.0 {
        let row = layout
            .row_for_source(SourceOffset(offset))
            .unwrap_or_else(|| panic!("no row owns {offset}"));
        assert!(layout.lines[row].source_range.start.0 <= offset);
    }
}

#[test]
fn a_clipped_or_foreign_presentation_stays_ordinary_source() {
    let (_, clipped) = present_code_block(Some(3..6));
    assert!(clipped.lines_before > 0);
    let mut unchanged = clipped.clone();
    assert!(!apply_diagram(&mut unchanged, &plan()));
    assert_eq!(unchanged, clipped);

    let (_, whole) = present_code_block(None);
    for bad in [
        DiagramPlan {
            lines: 2..9,
            height: 150.0,
        },
        DiagramPlan {
            lines: 4..4,
            height: 150.0,
        },
        DiagramPlan {
            lines: 2..6,
            height: 0.0,
        },
        DiagramPlan {
            lines: 2..6,
            height: f32::NAN,
        },
    ] {
        let mut candidate = whole.clone();
        assert!(!apply_diagram(&mut candidate, &bad), "{bad:?}");
        assert_eq!(candidate, whole);
    }
}

#[test]
fn folding_does_not_change_the_text_a_save_would_write() {
    let (buffer, mut block) = present_code_block(None);
    assert!(apply_diagram(&mut block, &plan()));
    let mut written = String::new();
    for line in &block.lines {
        let segments_cover_line = line
            .source_map
            .segments
            .iter()
            .map(|segment| segment.source_range)
            .collect::<Vec<SourceRange>>();
        assert_eq!(segments_cover_line, vec![line.source_range]);
        written.push_str(&buffer.text(line.source_range).expect("line text"));
    }
    assert_eq!(
        written,
        SOURCE["intro\n\n".len()..SOURCE.len() - "tail\n".len()]
    );
}
