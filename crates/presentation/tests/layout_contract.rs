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
    BlockKind, BlockLayout, BlockLine, BlockWindow, LineShaper, LineWrap, VerticalMove,
    VisualBlock, VisualOffset, block_line_span, layout_block,
    present_block_with_table_projection, table_delimiter_is_collapsed, trailing_blank_lines,
};
use std::cell::Cell;
use std::ops::Range;

const LINE_HEIGHT: f32 = 26.0;
/// Ten columns wide with the test shaper's 8 px advance.
const WIDTH: f32 = 80.0;

fn shaper() -> FixedAdvanceShaper {
    FixedAdvanceShaper::new(8.0)
}

struct CountingShaper {
    inner: FixedAdvanceShaper,
    wrap_calls: Cell<usize>,
    width_calls: Cell<usize>,
}

impl CountingShaper {
    fn new(advance: f32) -> Self {
        Self {
            inner: FixedAdvanceShaper::new(advance),
            wrap_calls: Cell::new(0),
            width_calls: Cell::new(0),
        }
    }
}

impl LineShaper for CountingShaper {
    fn wrap_boundaries(
        &self,
        line: &hane_presentation::VisualLine,
        fragment: Range<usize>,
        width: f32,
    ) -> Vec<usize> {
        self.wrap_calls.set(self.wrap_calls.get() + 1);
        self.inner.wrap_boundaries(line, fragment, width)
    }

    fn x_for_offset(
        &self,
        line: &hane_presentation::VisualLine,
        fragment: Range<usize>,
        offset: usize,
    ) -> f32 {
        self.inner.x_for_offset(line, fragment, offset)
    }

    fn offset_for_x(
        &self,
        line: &hane_presentation::VisualLine,
        fragment: Range<usize>,
        x: f32,
    ) -> usize {
        self.inner.offset_for_x(line, fragment, x)
    }

    fn width_for_text(&self, line: &hane_presentation::VisualLine, text: &str) -> f32 {
        self.width_calls.set(self.width_calls.get() + 1);
        self.inner.width_for_text(line, text)
    }
}

struct VariableLabelShaper {
    inner: FixedAdvanceShaper,
}

impl VariableLabelShaper {
    fn new(advance: f32) -> Self {
        Self {
            inner: FixedAdvanceShaper::new(advance),
        }
    }
}

impl LineShaper for VariableLabelShaper {
    fn wrap_boundaries(
        &self,
        line: &hane_presentation::VisualLine,
        fragment: Range<usize>,
        width: f32,
    ) -> Vec<usize> {
        self.inner.wrap_boundaries(line, fragment, width)
    }

    fn x_for_offset(
        &self,
        line: &hane_presentation::VisualLine,
        fragment: Range<usize>,
        offset: usize,
    ) -> f32 {
        self.inner.x_for_offset(line, fragment, offset)
    }

    fn offset_for_x(
        &self,
        line: &hane_presentation::VisualLine,
        fragment: Range<usize>,
        x: f32,
    ) -> usize {
        self.inner.offset_for_x(line, fragment, x)
    }

    fn width_for_text(&self, line: &hane_presentation::VisualLine, text: &str) -> f32 {
        if text == "12. " {
            48.0
        } else {
            self.inner.width_for_text(line, text)
        }
    }
}

/// A variable-width fixture where the first body glyph is narrow enough to fit
/// after an aligned marker gap. This makes double-counting that gap observable.
struct NarrowBodyShaper;

impl NarrowBodyShaper {
    fn advance(line: &hane_presentation::VisualLine, offset: usize) -> f32 {
        if line.visual_text[..offset].chars().count() >= 5 {
            4.0
        } else {
            8.0
        }
    }
}

impl LineShaper for NarrowBodyShaper {
    fn wrap_boundaries(
        &self,
        line: &hane_presentation::VisualLine,
        fragment: Range<usize>,
        width: f32,
    ) -> Vec<usize> {
        let mut boundaries = Vec::new();
        let mut row_start = fragment.start;
        let mut row_width = 0.0;
        for (relative, _) in line.visual_text[fragment.clone()].char_indices() {
            let offset = fragment.start + relative;
            let advance = Self::advance(line, offset);
            if row_width + advance > width && offset > row_start {
                boundaries.push(offset);
                row_start = offset;
                row_width = advance;
            } else {
                row_width += advance;
            }
        }
        boundaries
    }

    fn x_for_offset(
        &self,
        line: &hane_presentation::VisualLine,
        fragment: Range<usize>,
        offset: usize,
    ) -> f32 {
        let offset = offset.clamp(fragment.start, fragment.end);
        line.visual_text[fragment.clone()]
            .char_indices()
            .take_while(|(relative, _)| fragment.start + *relative < offset)
            .map(|(relative, _)| Self::advance(line, fragment.start + relative))
            .sum()
    }

    fn offset_for_x(
        &self,
        line: &hane_presentation::VisualLine,
        fragment: Range<usize>,
        x: f32,
    ) -> usize {
        let mut width = 0.0;
        for (relative, _) in line.visual_text[fragment.clone()].char_indices() {
            let offset = fragment.start + relative;
            let advance = Self::advance(line, offset);
            if x < width + advance / 2.0 {
                return offset;
            }
            width += advance;
        }
        fragment.end
    }

    fn width_for_text(&self, _line: &hane_presentation::VisualLine, text: &str) -> f32 {
        text.chars().count() as f32 * 8.0
    }
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
            present_block_with_table_projection(
                &block,
                buffer.revision(),
                &BlockWindow {
                    trailing_blank_lines: trailing_blank_lines(&buffer, &span),
                    render: span.clone(),
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
                index.list_projection(&block),
                index.table_projection(&block),
            )
        })
        .collect()
}

fn present_table_window(source: &str, render: Range<usize>) -> VisualBlock {
    let buffer = RopeBuffer::from_text(source);
    let index = BlockIndex::from_buffer(&buffer);
    let block = index.blocks().next().expect("table block");
    let span = block_line_span(&buffer, &block).expect("table span");
    let ranges = render
        .clone()
        .map(|line| buffer.line_range(LineId(line)).expect("line in range"))
        .collect::<Vec<_>>();
    let texts = ranges
        .iter()
        .map(|range| buffer.text(*range).expect("line text"))
        .collect::<Vec<_>>();
    let lines = render
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
    let table_delimiter_line = index
        .table_projection(&block)
        .and_then(|projection| projection.delimiter_range)
        .and_then(|range| buffer.line_for_offset(range.start).ok())
        .map(|line| line.0);
    present_block_with_table_projection(
        &block,
        buffer.revision(),
        &BlockWindow {
            trailing_blank_lines: trailing_blank_lines(&buffer, &span),
            span,
            lines: &lines,
            clipped_fence_lines: &[],
            zero_height_fence_rows_before: 0,
            zero_height_fence_rows_after: 0,
            table_delimiter_line,
            render,
            joined: None,
            block_disclosure: None,
        },
        LINE_HEIGHT,
        index.list_projection(&block),
        index.table_projection(&block),
    )
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
fn table_layout_shares_cell_geometry_and_keeps_cell_hit_testing_local() {
    let source = "| Name | Count |\n|:-----|------:|\n| Hane | 3 |";
    let (block, layout) = laid_out(source)
        .into_iter()
        .find(|(block, _)| block.kind == BlockKind::TableRow)
        .expect("table block");
    let rows = layout
        .lines
        .iter()
        .filter(|row| !row.table_cells.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].table_cells.len(), 2);
    assert_eq!(rows[1].table_cells.len(), 2);
    assert_eq!(rows[0].table_cells[0].x, rows[1].table_cells[0].x);
    assert_eq!(rows[0].table_cells[0].width, rows[1].table_cells[0].width);
    assert!(rows[0].table_cells[1].x > rows[0].table_cells[0].x);

    let count_offset = SourceOffset(source.find("Count").expect("Count cell"));
    let point = layout
        .point_for_source(&block, count_offset, &shaper())
        .expect("cell source maps to a point");
    assert_eq!(point.row, 0);
    let count_cell = &rows[0].table_cells[1];
    assert!(
        count_cell
            .fragments
            .iter()
            .all(|fragment| fragment.text_x >= count_cell.text_x),
        "cell text origin must be no farther right than any fragment origin"
    );
    assert!(point.x >= count_cell.text_x);

    let (aligned_block, aligned_layout) = laid_out("|a|b|\n|---|---:|\n|c|d|")
        .into_iter()
        .find(|(block, _)| block.kind == BlockKind::TableRow)
        .expect("aligned table block");
    let aligned_row = aligned_layout
        .lines
        .iter()
        .find(|row| row.line_id == 2)
        .expect("aligned body row");
    assert!(aligned_row.table_cells[1].text_x > aligned_row.table_cells[0].text_x);
    let right_offset = SourceOffset("|a|b|\n|---|---:|\n|c|d|".rfind('d').expect("right cell"));
    let right_point = aligned_layout
        .point_for_source(&aligned_block, right_offset, &shaper())
        .expect("right-aligned cell source maps to a point");
    assert!(
        right_point.x >= aligned_row.table_cells[1].text_x,
        "point {:?} should be inside right cell text origin {}",
        right_point,
        aligned_row.table_cells[1].text_x
    );
}

#[test]
fn issue_14_table_example_reaches_every_visible_grid_row() {
    let source = "| Name | Count | Status |\n|:-----|------:|:------:|\n| Hane | 3 | Ready |\n| Long value | 120 | Working |";
    let blocks = present(source, None);
    let block = blocks
        .into_iter()
        .find(|block| block.kind == BlockKind::TableRow)
        .expect("Issue #14 example must produce a table block");

    assert_eq!(
        block.lines.iter().map(|line| line.kind).collect::<Vec<_>>(),
        vec![
            BlockKind::TableRow,
            BlockKind::TableDelimiter,
            BlockKind::TableRow,
            BlockKind::TableRow,
        ]
    );
    assert_eq!(block.lines[0].visual_text.trim(), "Name  Count  Status");
    assert_eq!(block.lines[2].visual_text.trim(), "Hane  3  Ready");
    assert_eq!(
        block.lines[3].visual_text.trim(),
        "Long value  120  Working"
    );

    let layout = layout_block(&block, 640.0, &shaper());
    let rows = layout
        .lines
        .iter()
        .filter(|row| !row.table_cells.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 3, "header and both body rows must be drawable");
    assert!(rows.iter().all(|row| row.table_cells.len() == 3));
    for column in 0..3 {
        assert_eq!(rows[0].table_cells[column].x, rows[1].table_cells[column].x);
        assert_eq!(rows[0].table_cells[column].x, rows[2].table_cells[column].x);
        assert_eq!(
            rows[0].table_cells[column].width,
            rows[1].table_cells[column].width
        );
        assert_eq!(
            rows[0].table_cells[column].width,
            rows[2].table_cells[column].width
        );
    }
}

#[test]
fn readme_shortcut_table_keeps_text_in_all_three_columns() {
    let source = "| 操作 | macOS | Windows / Linux |\n| --- | --- | --- |\n| カーソル移動 | 矢印キー / Home / End | 矢印キー / Home / End |\n| 選択 | Shift + 矢印キー | Shift + 矢印キー |\n| 文頭・文末へ移動 | Command + ↑ / ↓ | Ctrl + Home / End（Ctrl + ↑ / ↓ も可） |\n| 全選択 | Command + A | Ctrl + A |\n| コピー / 切り取り / 貼り付け | Command + C / X / V | Ctrl + C / X / V |\n| 元に戻す / やり直す | Command + Z / Command + Shift + Z | Ctrl + Z / Ctrl + Y（Ctrl + Shift + Z も可） |\n| 開く | Command + O | Ctrl + O |\n| フォルダを開く（work folder mode） | Command + Shift + O | Ctrl + Shift + O |\n| 保存 / 名前を付けて保存 | Command + S / Command + Shift + S | Ctrl + S / Ctrl + Shift + S |\n| 自動保存の切り替え | Command + Option + A | Ctrl + Alt + A |";
    let block = present(source, None)
        .into_iter()
        .find(|block| block.kind == BlockKind::TableRow)
        .expect("README shortcut table must produce a table block");
    let layout = layout_block(&block, 920.0, &shaper());
    let rows = layout
        .lines
        .iter()
        .filter(|row| !row.table_cells.is_empty())
        .collect::<Vec<_>>();

    assert_eq!(rows.len(), 11);
    assert!(rows.iter().all(|row| row.table_cells.len() == 3));
    for row in rows {
        for cell in &row.table_cells {
            let text = &block.lines[row.line].visual_text[cell.visual_range.clone()];
            assert!(
                !text.trim().is_empty(),
                "README shortcut table column {} must retain visible text",
                cell.column
            );
            assert!(
                !cell.fragments.is_empty(),
                "README shortcut table column {} must have drawable fragments",
                cell.column
            );
        }
    }
}

#[test]
fn empty_table_cells_keep_a_visible_row_for_editing() {
    let source = "|||\n|---|---|\n|||";
    let (block, layout) = laid_out(source)
        .into_iter()
        .find(|(block, _)| block.kind == BlockKind::TableRow)
        .expect("table block");
    let row = layout
        .lines
        .iter()
        .find(|row| row.line_id == 2)
        .expect("empty body row");

    assert_eq!(row.table_cells.len(), 2);
    assert!(row.table_cells.iter().all(|cell| cell.fragments.is_empty()));
    assert_eq!(row.height, block.lines[row.line].height());
    assert!(
        row.height > 0.0,
        "empty cells must still own an editable row"
    );
}

#[test]
fn aligned_table_hit_testing_uses_the_selected_fragment_origin() {
    let source = "| a reallylongword |\n| ---: |\n| x |";
    let (block, layout) = laid_out(source)
        .into_iter()
        .find(|(block, _)| block.kind == BlockKind::TableRow)
        .expect("table block");
    let row_index = layout
        .lines
        .iter()
        .position(|row| row.line_id == 0)
        .expect("aligned header row");
    let row = &layout.lines[row_index];
    let cell = &row.table_cells[0];
    let first = cell.fragments.first().expect("wrapped first fragment");

    assert!(first.text_x > cell.text_x);
    assert_eq!(
        layout.visual_at_x(&block, row_index, first.text_x, &shaper()),
        Some(VisualOffset(first.visual_range.start)),
        "hit testing must measure from the selected fragment's aligned origin"
    );
}

#[test]
fn disclosed_table_headers_keep_the_projected_header_state() {
    let source = "| Header | body |\n| --- | --- |\n| value | cell |";
    let active = present(source, Some(source.find("Header").expect("header cell")));
    let block = active
        .into_iter()
        .find(|block| block.kind == BlockKind::TableRow)
        .expect("table block");
    let header = block.lines.first().expect("header line");

    assert!(header.table_row.is_none(), "the disclosed row stays raw");
    assert!(
        header.table_header,
        "the projected header state must survive disclosure"
    );
}

#[test]
fn table_layout_uses_intrinsic_column_widths_without_filler_space() {
    let (block, layout) = present("| a | wide |\n| --- | --- |\n| bb | x |", None)
        .into_iter()
        .find(|block| block.kind == BlockKind::TableRow)
        .map(|block| {
            let layout = layout_block(&block, 400.0, &shaper());
            (block, layout)
        })
        .expect("table block");
    let rows = layout
        .lines
        .iter()
        .filter(|row| !row.table_cells.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].table_cells[0].width, 48.0);
    assert_eq!(rows[0].table_cells[1].width, 64.0);
    assert_eq!(rows[0].table_cells[1].x, 48.0);
    assert_eq!(rows[1].table_cells[0].width, 48.0);
    assert_eq!(rows[1].table_cells[1].x, 48.0);
    assert_eq!(
        rows[0].table_cells[1].x + rows[0].table_cells[1].width,
        112.0
    );
    assert!(
        rows[0].table_cells[1].x + rows[0].table_cells[1].width < 400.0,
        "preferred columns must not absorb unused table width"
    );
    assert_eq!(block.lines[0].table_row.as_ref().unwrap().column_count, 2);
    assert_eq!(block.lines[2].table_row.as_ref().unwrap().column_count, 2);
}

#[test]
fn table_layout_does_not_restore_cells_for_hidden_delimiter_rows() {
    let source = "| h | short |\n| --- | --- |\n| a | x |";
    let block = present(source, None)
        .into_iter()
        .find(|block| block.kind == BlockKind::TableRow)
        .expect("table block");
    let delimiter = block
        .lines
        .iter()
        .find(|line| line.kind == BlockKind::TableDelimiter)
        .expect("table delimiter line");
    assert!(delimiter.visual_text.is_empty());

    let layout = layout_block(&block, 160.0, &shaper());
    assert!(
        layout
            .lines
            .iter()
            .all(|row| row.line_id != delimiter.line_id),
        "hidden delimiter rows must not become layout rows"
    );
}

#[test]
fn clipped_table_delimiter_does_not_restore_virtual_space() {
    let source = "| header | value |\n| --- | --- |\n| body | cell |";
    let header = present_table_window(source, 0..1);
    let body = present_table_window(source, 2..3);

    assert_eq!(header.trailing_space(), LINE_HEIGHT);
    assert_eq!(body.leading_space(), LINE_HEIGHT);
}

#[test]
fn table_delimiter_ownership_is_half_open_at_the_first_body_byte() {
    let source = "| header | value |\n| --- | --- |\n| body | cell |";
    let buffer = RopeBuffer::from_text(source);
    let index = BlockIndex::from_buffer(&buffer);
    let block = index.blocks().next().expect("table block");
    let projection = index
        .table_projection(&block)
        .expect("table projection");
    let delimiter = projection.delimiter_range.expect("table delimiter");
    let body_start = SourceOffset(source.find("| body").expect("body row"));

    assert_eq!(delimiter.end, body_start);
    assert!(
        table_delimiter_is_collapsed(projection, Some(SourceRange::empty(body_start.0))),
        "the caret at the body line start must not disclose the delimiter"
    );
    assert!(!table_delimiter_is_collapsed(
        projection,
        Some(SourceRange::empty(delimiter.start.0 + 2))
    ));
}

#[test]
fn table_layout_moves_directly_between_header_and_first_body_row() {
    let source = "| header | value |\n| --- | --- |\n| body | cell |";
    // Keep this contract focused on skipping the hidden delimiter. Wrapped
    // table-cell fragments are visual rows in their own right and are covered
    // by `table_rows_use_fragment_y_for_caret_hit_testing_and_vertical_movement`.
    let (block, layout) = present(source, None)
        .into_iter()
        .find(|block| block.kind == BlockKind::TableRow)
        .map(|block| {
            let layout = layout_block(&block, 200.0, &shaper());
            (block, layout)
        })
        .expect("table block");
    assert_eq!(
        layout.lines.iter().map(|row| row.line_id).collect::<Vec<_>>(),
        vec![0, 2]
    );
    assert_eq!(layout.lines[1].y, layout.lines[0].bottom());

    let header_offset = SourceOffset(source.find("header").expect("header cell"));
    let body_offset = SourceOffset(source.find("body").expect("body cell"));
    let header_point = layout
        .point_for_source(&block, header_offset, &shaper())
        .expect("header point");
    let body_point = layout
        .point_for_source(&block, body_offset, &shaper())
        .expect("body point");
    let down = layout.vertical_target(&block, header_offset, true, header_point.x, &shaper());
    let VerticalMove::To(down_offset) = down else {
        panic!("down from the header must enter the first body row: {down:?}");
    };
    let down_point = layout
        .point_for_source(&block, down_offset, &shaper())
        .expect("down target point");
    assert_eq!(down_point.row, body_point.row);
    assert_eq!(down_point.x, header_point.x);

    let up = layout.vertical_target(&block, body_offset, false, body_point.x, &shaper());
    let VerticalMove::To(up_offset) = up else {
        panic!("up from the first body row must enter the header: {up:?}");
    };
    let up_point = layout
        .point_for_source(&block, up_offset, &shaper())
        .expect("up target point");
    assert_eq!(up_point.row, header_point.row);
    assert_eq!(up_point.x, body_point.x);
}

#[test]
fn table_delimiter_stays_editable_and_restores_a_layout_row_when_disclosed() {
    let source = "| header | value |\n| --- | --- |\n| body | cell |";
    let delimiter_offset = source.find("---").expect("delimiter");
    let block = present(source, Some(delimiter_offset))
        .into_iter()
        .find(|block| block.kind == BlockKind::TableRow)
        .expect("table block");
    let delimiter = block.lines.iter().find(|line| line.line_id == 1).unwrap();
    assert_ne!(delimiter.kind, BlockKind::TableDelimiter);
    assert!(delimiter.visual_text.contains("---"));
    assert!(delimiter.height() > 0.0);

    let layout = layout_block(&block, WIDTH, &shaper());
    assert!(layout.lines.iter().any(|row| row.line_id == 1));
}

#[test]
fn table_layout_keeps_shared_columns_when_the_widest_row_is_being_edited() {
    let source = "| h | short |\n| --- | --- |\n| a | the widest cell |\n| b | x |";
    let inactive = present(source, None)
        .into_iter()
        .find(|block| block.kind == BlockKind::TableRow)
        .expect("table block");
    let active_offset = source.find("the widest").expect("active cell");
    let active = present(source, Some(active_offset));
    let active = active
        .iter()
        .find(|block| block.kind == BlockKind::TableRow)
        .expect("active table block");
    let inactive_layout = layout_block(&inactive, 120.0, &shaper());
    let active_layout = layout_block(active, 120.0, &shaper());

    let inactive_header = inactive_layout
        .lines
        .iter()
        .find(|row| row.line_id == 0)
        .expect("inactive header row");
    let active_header = active_layout
        .lines
        .iter()
        .find(|row| row.line_id == 0)
        .expect("active header row");
    let active_widest = active
        .lines
        .iter()
        .find(|line| line.line_id == 2)
        .expect("active widest row");

    assert!(
        active_widest.table_row.is_none(),
        "the editing row remains the raw Markdown presentation"
    );
    assert_eq!(
        active_header.table_cells[1].x, inactive_header.table_cells[1].x,
        "editing a cell must not move the shared column boundary"
    );
    assert_eq!(
        active_header.table_cells[1].width, inactive_header.table_cells[1].width,
        "editing a cell must not change the shared column width"
    );
}

#[test]
fn table_layout_restores_formal_cell_boundaries_for_shortened_inline_markup() {
    let source = "| h | short |\n| --- | --- |\n| **the widest** | x |\n| b | y |";
    let inactive = present(source, None)
        .into_iter()
        .find(|block| block.kind == BlockKind::TableRow)
        .expect("inactive table block");
    let active = present(source, Some(source.find('x').expect("active cell")))
        .into_iter()
        .find(|block| block.kind == BlockKind::TableRow)
        .expect("active table block");
    let inactive_line = inactive
        .lines
        .iter()
        .find(|line| line.line_id == 2)
        .expect("inactive widest row");
    let active_line = active
        .lines
        .iter()
        .find(|line| line.line_id == 2)
        .expect("active widest row");
    assert_eq!(
        active_line.visual_text, "| the widest | x |",
        "the active row keeps the inline marker collapsed in the visual text"
    );
    assert_eq!(
        active_line.table_row, None,
        "the active row remains raw while layout receives its shared cells"
    );

    let inactive_layout = layout_block(&inactive, 160.0, &shaper());
    let active_layout = layout_block(&active, 160.0, &shaper());
    let inactive_row = inactive_layout
        .lines
        .iter()
        .find(|row| row.line_id == 2)
        .expect("inactive layout row");
    let active_row = active_layout
        .lines
        .iter()
        .find(|row| row.line_id == 2)
        .expect("active layout row");
    assert_eq!(active_row.table_cells.len(), 2);
    assert_eq!(
        active_row.table_cells[0].source_range,
        inactive_line.table_row.as_ref().unwrap().cells[0].source_range,
        "layout keeps the formal source cell, including hidden markers"
    );
    assert!(
        active_row.table_cells[0].source_range.end.0
            - active_row.table_cells[0].source_range.start.0
            > active_row.table_cells[0].visual_range.end
                - active_row.table_cells[0].visual_range.start,
        "the source cell retains hidden inline markers while its visual range is shortened"
    );
    assert_eq!(active_row.table_cells[1].x, inactive_row.table_cells[1].x);
    assert_eq!(
        active_row.table_cells[1].width,
        inactive_row.table_cells[1].width
    );

    let widest = SourceOffset(source.find("the widest").expect("widest text"));
    let point = active_layout
        .point_for_source(&active, widest, &shaper())
        .expect("active cell caret point");
    assert!(point.x >= active_row.table_cells[0].text_x);
    let selected_layout_row = active_layout
        .visual_range_on_row(
            &active,
            active_layout
                .lines
                .iter()
                .position(|row| row.line_id == 2)
                .expect("active layout row index"),
            SourceRange::new(widest.0, widest.0 + "the widest".len()),
        )
        .expect("selection/IME cell range");
    let ime_layout_row = active_layout
        .visual_range_on_row(
            &active,
            active_layout
                .lines
                .iter()
                .position(|row| row.line_id == 2)
                .expect("active layout row index"),
            SourceRange::new(widest.0, widest.0 + "the widest".len()),
        )
        .expect("IME cell range");
    assert_eq!(selected_layout_row, ime_layout_row);
    assert!(
        selected_layout_row.start >= active_row.table_cells[0].visual_range.start
            && selected_layout_row.end <= active_row.table_cells[0].visual_range.end,
        "selection and IME ranges stay inside the same formal cell"
    );
}

#[test]
fn table_layout_uses_formal_rows_outside_each_render_window() {
    let source =
        "| h | short |\n| --- | --- |\n| a | x |\n| b | the widest cell outside the first window |";
    let top = present_table_window(source, 0..1);
    let bottom = present_table_window(source, 3..4);
    let top_layout = layout_block(&top, 160.0, &shaper());
    let bottom_layout = layout_block(&bottom, 160.0, &shaper());
    let top_row = &top_layout.lines[0];
    let bottom_row = &bottom_layout.lines[0];
    assert_eq!(top_row.table_cells[1].x, bottom_row.table_cells[1].x);
    assert_eq!(
        top_row.table_cells[1].width,
        bottom_row.table_cells[1].width
    );
    assert_eq!(
        top.table_projection.as_ref().unwrap().rows.len(),
        3,
        "the formal projection retains header and every body row"
    );
}

#[test]
fn table_layout_distributes_available_width_between_minimum_and_preferred() {
    let (_block, layout) = present("| abc def | x |\n| --- | --- |\n| a | b |", None)
        .into_iter()
        .find(|block| block.kind == BlockKind::TableRow)
        .map(|block| {
            let layout = layout_block(&block, 100.0, &shaper());
            (block, layout)
        })
        .expect("table block");
    let row = layout
        .lines
        .iter()
        .find(|row| row.line_id == 0)
        .expect("header row");
    assert_eq!(row.table_cells[0].width, 67.0);
    assert_eq!(row.table_cells[1].width, 33.0);
    assert_eq!(row.table_cells[1].x, 67.0);
    assert_eq!(row.table_cells[1].x + row.table_cells[1].width, 100.0);
}

#[test]
fn table_layout_compresses_overflowing_minimums_without_widening_the_panel() {
    let (_, layout) = present("| abcdef | ghijkl |\n| --- | --- |\n| 1 | 2 |", None)
        .into_iter()
        .find(|block| block.kind == BlockKind::TableRow)
        .map(|block| {
            let layout = layout_block(&block, 100.0, &shaper());
            (block, layout)
        })
        .expect("table block");
    let row = layout
        .lines
        .iter()
        .find(|row| row.line_id == 0)
        .expect("header row");
    assert_eq!(row.table_cells[0].width, 50.0);
    assert_eq!(row.table_cells[1].x, 50.0);
    assert!(
        row.table_cells
            .iter()
            .all(|cell| cell.x + cell.width <= 100.0),
        "compressed table geometry must stay inside the available column"
    );
}

#[test]
fn table_cell_fragments_wrap_visible_text_within_shared_columns() {
    let source = "| URL | 日本語 |\n| --- | --- |\n| https://example.com/a/very/long/identifier | これは空白のない長い日本語のセルです |\n| short | 別の行 |";
    let (block, layout) = present(source, None)
        .into_iter()
        .find(|block| block.kind == BlockKind::TableRow)
        .map(|block| {
            let layout = layout_block(&block, 120.0, &shaper());
            (block, layout)
        })
        .expect("table block");
    let shaper = shaper();
    let rows = layout
        .lines
        .iter()
        .filter(|row| !row.table_cells.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 3);
    assert!(rows[1].table_cells[1].fragments.len() > 1);
    assert!(rows[1].table_cells[0].fragments.len() > 1);

    for row in &rows {
        for cell in &row.table_cells {
            if cell.fragments.is_empty() {
                assert!(cell.visual_range.is_empty());
                continue;
            }
            assert_eq!(
                cell.fragments.first().unwrap().visual_range.start,
                cell.visual_range.start
            );
            assert_eq!(
                cell.fragments.last().unwrap().visual_range.end,
                cell.visual_range.end
            );
            for pair in cell.fragments.windows(2) {
                assert_eq!(
                    pair[0].visual_range.end, pair[1].visual_range.start,
                    "cell fragments must tile visible text without a gap"
                );
            }
            for fragment in &cell.fragments {
                let text = &block.lines[row.line].visual_text;
                assert!(text.is_char_boundary(fragment.visual_range.start));
                assert!(text.is_char_boundary(fragment.visual_range.end));
                let fragment_width = shaper.x_for_offset(
                    &block.lines[row.line],
                    fragment.visual_range.clone(),
                    fragment.visual_range.end,
                );
                assert!(
                    fragment_width <= cell.width - 16.0 + f32::EPSILON,
                    "fragment {:?} exceeds its cell content width {}",
                    fragment.visual_range,
                    cell.width - 16.0
                );
            }
        }
    }

    assert_eq!(
        rows[0].table_cells[0].x, rows[1].table_cells[0].x,
        "wrapping must not move a shared column boundary"
    );
    assert_eq!(
        rows[0].table_cells[0].width, rows[1].table_cells[0].width,
        "wrapping must not change a shared column width"
    );

    let identifier = SourceOffset(source.find("identifier").expect("identifier"));
    let point = layout
        .point_for_source(&block, identifier, &shaper)
        .expect("wrapped cell source maps to a point");
    let row = &rows[1];
    assert!(
        point.x >= row.table_cells[0].x + 8.0
            && point.x <= row.table_cells[0].x + row.table_cells[0].width - 8.0,
        "caret x must stay inside the wrapped cell"
    );
}

#[test]
fn table_rows_use_fragment_y_for_caret_hit_testing_and_vertical_movement() {
    let source = "| URL | 日本語 |\n| --- | --- |\n| https://example.com/a/very/long/identifier | これは空白のない長い日本語のセルです |\n| short | 別の行 |\n| tail | end |";
    let (block, layout) = present(source, None)
        .into_iter()
        .find(|block| block.kind == BlockKind::TableRow)
        .map(|block| {
            let layout = layout_block(&block, 120.0, &shaper());
            (block, layout)
        })
        .expect("table block");
    let wrapped_index = layout
        .lines
        .iter()
        .position(|row| row.line_id == 2)
        .expect("wrapped body row");
    let wrapped = &layout.lines[wrapped_index];
    let max_fragments = wrapped
        .table_cells
        .iter()
        .map(|cell| cell.fragments.len())
        .max()
        .unwrap_or(1);
    assert!(max_fragments > 1, "the body row must contain wrapped cells");
    let fragment_height = wrapped
        .table_cells
        .iter()
        .flat_map(|cell| cell.fragments.first())
        .map(|fragment| fragment.height)
        .next()
        .expect("wrapped row has a fragment");
    assert_eq!(wrapped.height, fragment_height * max_fragments as f32);
    assert_eq!(
        layout.height(),
        layout.leading_space
            + layout.trailing_space
            + layout.lines.iter().map(|row| row.height).sum::<f32>(),
        "table block height must include every variable-height physical row"
    );

    let cell = &wrapped.table_cells[0];
    let second = &cell.fragments[1];
    let second_visual = second.visual_range.start + 1;
    let second_source = block.lines[wrapped.line]
        .source_map
        .visual_to_source(VisualOffset(second_visual), Bias::After)
        .expect("second fragment has a source boundary")
        .source_offset;
    let point = layout
        .point_for_source(&block, second_source, &shaper())
        .expect("wrapped source has a point");
    assert_eq!(point.row, wrapped_index);
    assert_eq!(point.y, wrapped.y + second.y);
    assert_eq!(point.height, second.height);
    let back = layout
        .source_for_point(&block, point.x, point.y, &shaper())
        .expect("point has a source boundary");
    let back_visual = block.lines[wrapped.line]
        .source_map
        .source_to_visual(back, Bias::After)
        .expect("hit-test source maps back to visual text")
        .visual_offset
        .0;
    assert!(
        second.visual_range.contains(&back_visual),
        "source and point conversion must use the same wrapped fragment"
    );

    let first_source = layout
        .source_at_xy(
            &block,
            wrapped_index,
            cell.text_x,
            cell.fragments[0].y + fragment_height / 2.0,
            &shaper(),
        )
        .expect("first fragment has a hit-test boundary");
    let VerticalMove::To(next_source) =
        layout.vertical_target(&block, first_source, true, cell.text_x, &shaper())
    else {
        panic!("vertical movement must enter the next fragment in the same physical row");
    };
    let next_point = layout
        .point_for_source(&block, next_source, &shaper())
        .expect("vertical target has a point");
    assert_eq!(next_point.row, wrapped_index);
    assert_eq!(next_point.y, wrapped.y + second.y);

    let third = &cell.fragments[2];
    let VerticalMove::To(third_target) =
        layout.vertical_target(&block, next_source, true, cell.text_x, &shaper())
    else {
        panic!("vertical movement must advance one fragment at a time");
    };
    let third_point = layout
        .point_for_source(&block, third_target, &shaper())
        .expect("third fragment target has a point");
    assert_eq!(third_point.y, wrapped.y + third.y);

    let last = cell
        .fragments
        .last()
        .expect("wrapped cell has a last fragment");
    let last_source = block.lines[wrapped.line]
        .source_map
        .visual_to_source(VisualOffset(last.visual_range.start + 1), Bias::After)
        .expect("last fragment has a source boundary")
        .source_offset;
    let VerticalMove::To(next_row_source) =
        layout.vertical_target(&block, last_source, true, cell.text_x, &shaper())
    else {
        panic!("vertical movement must leave the wrapped physical row");
    };
    assert_eq!(
        layout
            .point_for_source(&block, next_row_source, &shaper())
            .map(|point| point.row),
        Some(wrapped_index + 1)
    );
    let VerticalMove::To(up_source) = layout.vertical_target(
        &block,
        next_row_source,
        false,
        cell.text_x,
        &shaper(),
    ) else {
        panic!("moving up must enter the previous row's last fragment");
    };
    let up_point = layout
        .point_for_source(&block, up_source, &shaper())
        .expect("upward target has a point");
    assert_eq!(up_point.row, wrapped_index);
    assert_eq!(up_point.y, wrapped.y + last.y);
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
fn list_body_columns_are_shared_by_inactive_ordered_labels() {
    let source = "9. foo\n1. bar\n";
    let block = present(source, None)
        .into_iter()
        .find(|block| block.lines.iter().any(|line| line.list.is_some()))
        .expect("the ordered list block is presented");
    let layout = layout_block(&block, 160.0, &shaper());
    let rows = layout
        .lines
        .iter()
        .filter(|row| row.line < 2)
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row.marker_x_origin == Some(0.0)));
    assert!(rows.iter().all(|row| row.body_x_origin == 32.0));
    assert_eq!(
        rows.iter()
            .map(|row| row.marker_body_gap)
            .collect::<Vec<_>>(),
        vec![8.0, 0.0]
    );
    assert_eq!(rows[0].effective_width, 128.0);

    let foo = SourceOffset(source.find("foo").expect("foo in source"));
    let bar = SourceOffset(source.find("bar").expect("bar in source"));
    let foo_point = layout
        .point_for_source(&block, foo, &shaper())
        .expect("foo has a point");
    let bar_point = layout
        .point_for_source(&block, bar, &shaper())
        .expect("bar has a point");
    assert_eq!(foo_point.x, bar_point.x);
    assert_eq!(foo_point.x, 32.0);
}

#[test]
fn list_body_columns_use_the_widest_actual_marker_label() {
    let source = "10. first\n11. second\n12. third\n";
    let block = present(source, None)
        .into_iter()
        .find(|block| block.lines.iter().any(|line| line.list.is_some()))
        .expect("the ordered list block is presented");
    let shaper = VariableLabelShaper::new(8.0);
    let layout = layout_block(&block, 160.0, &shaper);
    let rows = layout
        .lines
        .iter()
        .filter(|row| row.line < 3)
        .collect::<Vec<_>>();

    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|row| row.body_x_origin == 48.0));
    for word in ["first", "second", "third"] {
        let source_offset = SourceOffset(source.find(word).expect("list item body"));
        assert_eq!(
            layout
                .point_for_source(&block, source_offset, &shaper)
                .expect("body has a point")
                .x,
            48.0
        );
    }
}

#[test]
fn disclosed_markers_use_their_source_visible_width_without_a_virtual_gap() {
    let cases = [
        ("- body", "body", 16.0),
        ("+ body", "body", 16.0),
        ("* body", "body", 16.0),
        ("1. body", "body", 24.0),
        ("1) body", "body", 24.0),
        ("003) body", "body", 40.0),
    ];

    for (source, body, expected_x) in cases {
        let cursor = source.find(body).expect("list body");
        let block = present(source, Some(cursor))
            .into_iter()
            .find(|block| block.lines.iter().any(|line| line.list.is_some()))
            .expect("the list block is presented");
        let marker = block.lines[0]
            .list
            .as_ref()
            .and_then(|list| list.marker.as_ref())
            .expect("the disclosed marker is present");
        assert!(
            !marker.synthesized,
            "{source:?} must expose its source marker"
        );

        let layout = layout_block(&block, 160.0, &shaper());
        let row = layout
            .lines
            .iter()
            .find(|row| row.line == 0)
            .expect("the list row is laid out");
        assert_eq!(row.body_x_origin, expected_x, "{source:?}");
        assert_eq!(row.marker_body_gap, 0.0, "{source:?}");

        let point = layout
            .point_for_source(&block, SourceOffset(cursor), &shaper())
            .expect("the body has a point");
        assert_eq!(point.x, expected_x, "{source:?}");
        assert_eq!(
            layout.source_at_x(&block, point.row, point.x, &shaper()),
            Some(SourceOffset(cursor)),
            "source → point → source must preserve the body caret for {source:?}"
        );
    }
}

#[test]
fn disclosed_empty_markers_put_the_terminal_caret_after_raw_source() {
    let cases = [("-\n", 1, 8.0), ("003)\n", 4, 32.0)];

    for (source, marker_end, expected_x) in cases {
        let block = present(source, Some(0))
            .into_iter()
            .find(|block| block.lines.iter().any(|line| line.list.is_some()))
            .expect("the empty list block is presented");
        let marker = block.lines[0]
            .list
            .as_ref()
            .and_then(|list| list.marker.as_ref())
            .expect("the empty marker is present");
        assert!(
            !marker.synthesized,
            "{source:?} must not add a bullet or number"
        );
        assert_eq!(marker.label, source[..marker_end], "{source:?}");

        let layout = layout_block(&block, 160.0, &shaper());
        let row = &layout.lines[0];
        assert_eq!(row.body_x_origin, expected_x, "{source:?}");
        assert_eq!(row.marker_body_gap, 0.0, "{source:?}");
        let point = layout
            .point_for_source(&block, SourceOffset(marker_end), &shaper())
            .expect("the terminal caret has a point");
        assert_eq!(point.x, expected_x, "{source:?}");
        assert_eq!(
            layout.source_at_x(&block, point.row, point.x, &shaper()),
            Some(SourceOffset(marker_end)),
            "source → point → source must preserve the terminal caret for {source:?}"
        );
    }
}

#[test]
fn disclosing_a_short_ordered_marker_drops_only_the_inactive_alignment_gap() {
    let source = "9. foo\n10. bar\n";
    let cursor = source.find("foo").expect("first list body");
    let block = present(source, Some(cursor))
        .into_iter()
        .find(|block| block.lines.iter().any(|line| line.list.is_some()))
        .expect("the ordered list block is presented");
    let layout = layout_block(&block, 160.0, &shaper());
    let opening = layout
        .lines
        .iter()
        .find(|row| row.line == 0)
        .expect("the disclosed opening row is laid out");
    let inactive = layout
        .lines
        .iter()
        .find(|row| row.line == 1)
        .expect("the inactive sibling row is laid out");

    assert_eq!(opening.body_x_origin, 24.0);
    assert_eq!(opening.marker_body_gap, 0.0);
    assert_eq!(inactive.body_x_origin, 32.0);
    assert_eq!(inactive.marker_body_gap, 0.0);
}

#[test]
fn a_non_list_dash_prefix_keeps_plain_text_coordinates() {
    let source = "-a\n";
    let cursor = source.find('a').expect("plain text");
    let block = present(source, Some(cursor))
        .into_iter()
        .next()
        .expect("the paragraph is presented");
    assert!(block.lines[0].list.is_none());

    let layout = layout_block(&block, 160.0, &shaper());
    let row = &layout.lines[0];
    assert_eq!(row.body_x_origin, 0.0);
    assert_eq!(row.marker_x_origin, None);
    assert_eq!(row.marker_body_gap, 0.0);
    let point = layout
        .point_for_source(&block, SourceOffset(cursor), &shaper())
        .expect("plain text has a point");
    assert_eq!(point.x, 8.0);
    assert_eq!(
        layout.source_at_x(&block, point.row, point.x, &shaper()),
        Some(SourceOffset(cursor))
    );
}

#[test]
fn list_marker_widths_cache_each_font_scale_once() {
    let source = "- # heading one\n- body two\n- # heading three\n- body four\n";
    let block = present(source, None)
        .into_iter()
        .find(|block| block.lines.iter().any(|line| line.list.is_some()))
        .expect("the mixed-style list block is presented");
    let scales = block
        .lines
        .iter()
        .filter_map(|line| {
            line.list
                .as_ref()
                .map(|_| line.display().font_scale.to_bits())
        })
        .collect::<Vec<_>>();
    assert_eq!(scales.len(), 4);
    assert_eq!(scales[0], scales[2]);
    assert_eq!(scales[1], scales[3]);
    assert_ne!(scales[0], scales[1]);

    let marker_labels = block
        .lines
        .first()
        .and_then(|line| line.list.as_ref())
        .expect("the first list row has metadata")
        .owner
        .alignment
        .marker_labels
        .len();
    let shaper = CountingShaper::new(8.0);
    let _layout = layout_block(&block, 240.0, &shaper);

    assert_eq!(shaper.width_calls.get(), marker_labels * 2);
}

#[test]
fn list_marker_body_gap_clicks_clamp_to_the_body_boundary() {
    let source = "9. foo\n1. bar\n";
    let block = present(source, None)
        .into_iter()
        .find(|block| block.lines.iter().any(|line| line.list.is_some()))
        .expect("the ordered list block is presented");
    let layout = layout_block(&block, 160.0, &shaper());
    let row = layout
        .lines
        .iter()
        .find(|row| row.line == 0)
        .expect("the short ordered row is laid out");
    let foo = SourceOffset(source.find("foo").expect("foo in source"));

    assert_eq!(row.body_x_origin, 32.0);
    assert_eq!(row.marker_body_gap, 8.0);
    assert_eq!(layout.source_at_x(&block, 0, 28.0, &shaper()), Some(foo));
}

#[test]
fn list_marker_body_gap_counts_against_the_opening_wrap_budget() {
    let source = "9. foo\n1. bar\n";
    let block = present(source, None)
        .into_iter()
        .find(|block| block.lines.iter().any(|line| line.list.is_some()))
        .expect("the ordered list block is presented");
    let layout = layout_block(&block, 48.0, &shaper());
    let body_start = block.lines[0]
        .list
        .as_ref()
        .expect("the first line has list metadata")
        .body_visual_start
        .0;
    let first_rows = layout
        .lines
        .iter()
        .filter(|row| row.line == 0)
        .collect::<Vec<_>>();

    assert!(first_rows.len() > 1, "the marker and body must not overrun");
    assert_eq!(first_rows[0].line_visual_range.end, body_start);
}

#[test]
fn long_wrapped_lines_reuse_all_boundaries_from_one_shape() {
    let source = "word ".repeat(400);
    let block = present(&source, None)
        .into_iter()
        .next()
        .expect("the paragraph is presented");
    let shaper = CountingShaper::new(8.0);
    let layout = layout_block(&block, 40.0, &shaper);

    assert!(layout.lines.len() > 10, "the long line must wrap");
    assert_eq!(shaper.wrap_calls.get(), 1);
}

#[test]
fn list_continuations_and_wrapped_fragments_start_at_the_body_column() {
    let source = "- first\n  continuation\n\n- one two three four five six\n";
    let list = present(source, None)
        .into_iter()
        .find(|block| block.lines.iter().any(|line| line.list.is_some()))
        .expect("the list block is presented");
    let layout = layout_block(&list, WIDTH, &shaper());
    let continuation = layout
        .lines
        .iter()
        .find(|row| row.line == 1)
        .expect("the continuation row is laid out");
    assert_eq!(continuation.text_x_origin, 16.0);
    assert_eq!(continuation.body_x_origin, 16.0);

    let wrapped = layout
        .lines
        .iter()
        .filter(|row| row.line == 3)
        .collect::<Vec<_>>();
    assert!(wrapped.len() > 1, "the final item wraps");
    assert_eq!(wrapped[0].text_x_origin, 0.0);
    assert!(wrapped[1..].iter().all(|row| row.text_x_origin == 16.0));
    assert!(wrapped.iter().all(|row| row.effective_width == 64.0));
}

#[test]
fn opening_list_wrap_counts_the_marker_only_in_the_first_row_budget() {
    let source = "- one two three four five six\n";
    let block = present(source, None)
        .into_iter()
        .find(|block| block.lines.iter().any(|line| line.list.is_some()))
        .expect("the list block is presented");
    let layout = layout_block(&block, WIDTH, &shaper());
    let rows = layout
        .lines
        .iter()
        .filter(|row| row.line == 0)
        .collect::<Vec<_>>();
    assert!(rows.len() > 1, "the item must wrap");

    let line = &block.lines[0];
    let body_start = line
        .list
        .as_ref()
        .expect("the line has list metadata")
        .body_visual_start
        .0;
    let first_body_text = line.visual_text[body_start..]
        .find("three")
        .map(|offset| body_start + offset)
        .expect("the first body row reaches the word after the marker budget");
    assert_eq!(rows[0].line_visual_range.end, first_body_text);
    assert_eq!(rows[0].text_x_origin, 0.0);
    assert_eq!(rows[0].body_x_origin, 16.0);
}

#[test]
fn a_long_marker_stays_intact_before_the_body_when_the_column_is_narrow() {
    let source = "999999999. value\n";
    let block = present(source, None)
        .into_iter()
        .find(|block| block.lines.iter().any(|line| line.list.is_some()))
        .expect("the ordered list block is presented");
    let layout = layout_block(&block, 40.0, &shaper());
    let body = block.lines[0]
        .list
        .as_ref()
        .expect("the line has list metadata")
        .body_visual_start
        .0;
    let rows = layout
        .lines
        .iter()
        .filter(|row| row.line == 0)
        .collect::<Vec<_>>();

    assert!(rows.len() >= 2, "the body must continue on a later row");
    assert_eq!(rows[0].line_visual_range, 0..body);
    assert_eq!(rows[0].text_x_origin, 0.0);
    assert_eq!(rows[1].line_visual_range.start, body);
    assert_eq!(rows[1].text_x_origin, rows[1].body_x_origin);
}

#[test]
fn disclosed_prefix_and_marker_stay_intact_before_the_body_when_the_column_is_narrow() {
    let source = "> 1. one two three four five six\n";
    let cursor = source.find("one").expect("list body");
    let block = present(source, Some(cursor))
        .into_iter()
        .find(|block| block.lines.iter().any(|line| line.list.is_some()))
        .expect("the quoted list block is presented");
    let line = &block.lines[0];
    let body = line
        .list
        .as_ref()
        .expect("the line has list metadata")
        .body_visual_start
        .0;
    let layout = layout_block(&block, 32.0, &shaper());
    let rows = layout
        .lines
        .iter()
        .filter(|row| row.line == 0)
        .collect::<Vec<_>>();

    assert!(rows.len() >= 2, "the body must continue on a later row");
    assert_eq!(rows[0].line_visual_range, 0..body);
    assert_eq!(rows[0].text_x_origin, 0.0);
    assert_eq!(rows[1].line_visual_range.start, body);
    assert_eq!(rows[1].text_x_origin, rows[1].body_x_origin);
}

#[test]
fn disclosed_prefix_marker_uses_raw_width_in_the_opening_budget() {
    let source = "> 9. first character\n> 10. second\n";
    let cursor = source.find("first").expect("list body");
    let block = present(source, Some(cursor))
        .into_iter()
        .find(|block| block.lines.iter().any(|line| line.list.is_some()))
        .expect("the quoted ordered list block is presented");
    let line = &block.lines[0];
    let body = line
        .list
        .as_ref()
        .expect("the line has list metadata")
        .body_visual_start
        .0;
    let layout = layout_block(&block, 52.0, &NarrowBodyShaper);
    let opening = layout
        .lines
        .iter()
        .find(|row| row.line == 0)
        .expect("opening row is laid out");

    assert_eq!(opening.body_x_origin, 40.0);
    assert_eq!(opening.marker_body_gap, 0.0);
    assert_eq!(opening.line_visual_range, 0..body + 3);
}

#[test]
fn disclosed_list_prefix_is_subtracted_only_on_the_first_wrap_fragment() {
    let source = "  - one two three four five six seven eight\n";
    let cursor = source.find("one").expect("list body");
    let block = present(source, Some(cursor))
        .into_iter()
        .find(|block| block.lines.iter().any(|line| line.list.is_some()))
        .expect("the list block is presented");
    let layout = layout_block(&block, 64.0, &shaper());
    let rows = layout
        .lines
        .iter()
        .filter(|row| row.line == 0)
        .collect::<Vec<_>>();

    assert!(rows.len() > 1, "the disclosed list line must wrap");
    assert_eq!(rows[0].text_x_origin, 0.0);
    assert!(
        rows[1..]
            .iter()
            .all(|row| row.text_x_origin == row.body_x_origin)
    );
}

#[test]
fn disclosed_empty_long_marker_stays_in_one_opening_row() {
    let source = "999999999.\n";
    let block = present(source, Some(0))
        .into_iter()
        .find(|block| block.lines.iter().any(|line| line.list.is_some()))
        .expect("the ordered list block is presented");
    let line = &block.lines[0];
    let body = line
        .list
        .as_ref()
        .expect("the line has list metadata")
        .body_visual_start
        .0;
    assert_eq!(body, line.visual_text.len());

    let layout = layout_block(&block, 40.0, &shaper());
    let rows = layout
        .lines
        .iter()
        .filter(|row| row.line == 0)
        .collect::<Vec<_>>();

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].line_visual_range, 0..line.visual_text.len());
    assert_eq!(rows[0].wrap, LineWrap::Hard);
}

#[test]
fn disclosed_structural_prefix_width_is_included_in_list_body_geometry() {
    let source = "  - item\n    continued\n";
    let cursor = source.find("continued").expect("continuation in source");
    let block = present(source, Some(cursor))
        .into_iter()
        .find(|block| block.lines.iter().any(|line| line.list.is_some()))
        .expect("the list block is presented");
    let layout = layout_block(&block, 160.0, &shaper());

    let opening = layout
        .lines
        .iter()
        .find(|row| row.line == 0)
        .expect("opening row is laid out");
    assert_eq!(opening.text_x_origin, 0.0);
    assert_eq!(opening.body_x_origin, 32.0);

    let continuation = layout
        .lines
        .iter()
        .find(|row| row.line == 1)
        .expect("continuation row is laid out");
    assert_eq!(continuation.text_x_origin, 0.0);
    assert_eq!(continuation.body_x_origin, 32.0);

    let item = SourceOffset(source.find("item").expect("item in source"));
    let continued = SourceOffset(source.find("continued").expect("continued in source"));
    assert_eq!(
        layout.point_for_source(&block, item, &shaper()).unwrap().x,
        32.0
    );
    assert_eq!(
        layout
            .point_for_source(&block, continued, &shaper())
            .unwrap()
            .x,
        32.0
    );
}

#[test]
fn disclosed_continuation_prefix_does_not_readd_marker_width() {
    let source = "- item\n  continued\n";
    let cursor = source.find("continued").expect("continuation in source");
    let block = present(source, Some(cursor))
        .into_iter()
        .find(|block| block.lines.iter().any(|line| line.list.is_some()))
        .expect("the list block is presented");
    let layout = layout_block(&block, 160.0, &shaper());

    let opening = layout
        .lines
        .iter()
        .find(|row| row.line == 0)
        .expect("opening row is laid out");
    let continuation = layout
        .lines
        .iter()
        .find(|row| row.line == 1)
        .expect("continuation row is laid out");
    assert_eq!(opening.body_x_origin, 16.0);
    assert_eq!(continuation.body_x_origin, 16.0);
}

#[test]
fn disclosed_quote_prefix_width_is_included_in_list_body_geometry() {
    let source = "> 1. item\n";
    let cursor = source.find("item").expect("item in source");
    let block = present(source, Some(cursor))
        .into_iter()
        .find(|block| block.lines.iter().any(|line| line.list.is_some()))
        .expect("the list block is presented");
    let layout = layout_block(&block, 160.0, &shaper());
    let row = layout
        .lines
        .iter()
        .find(|row| row.line == 0)
        .expect("the quoted list row is laid out");

    assert_eq!(row.text_x_origin, 0.0);
    assert_eq!(row.body_x_origin, 40.0);
    assert_eq!(
        layout
            .point_for_source(&block, SourceOffset(cursor), &shaper())
            .expect("item has a point")
            .x,
        40.0
    );
}

#[test]
fn disclosed_quote_prefix_uses_source_width_without_semantic_inset() {
    let source = "> item\n";
    let cursor = source.find("item").expect("item in source");
    let block = present(source, Some(cursor))
        .into_iter()
        .find(|block| block.lines.iter().any(|line| line.quote.is_some()))
        .expect("the quoted block is presented");
    let layout = layout_block(&block, 160.0, &shaper());
    let row = layout
        .lines
        .iter()
        .find(|row| row.line == 0)
        .expect("the quoted row is laid out");

    assert_eq!(row.text_x_origin, 0.0);
    assert_eq!(row.body_x_origin, 0.0);
    assert_eq!(
        layout
            .point_for_source(&block, SourceOffset(cursor), &shaper())
            .expect("item has a point")
            .x,
        16.0
    );
}

#[test]
fn inactive_quote_wrap_width_reserves_semantic_inset() {
    let quoted_source = "> quoted text that wraps in a narrow column\n";
    let quoted_block = present(quoted_source, None)
        .into_iter()
        .find(|block| block.lines.iter().any(|line| line.quote.is_some()))
        .expect("the quoted block is presented");
    let quoted_layout = layout_block(&quoted_block, 80.0, &shaper());
    let quoted_rows = quoted_layout
        .lines
        .iter()
        .filter(|row| row.line == 0)
        .collect::<Vec<_>>();

    assert!(quoted_rows.len() > 1, "the quoted body must wrap");
    assert!(quoted_rows.iter().all(|row| row.effective_width == 56.0));

    let plain_source = "plain text that wraps in a narrow column\n";
    let plain_block = present(plain_source, None)
        .into_iter()
        .next()
        .expect("the plain block is presented");
    let plain_layout = layout_block(&plain_block, 80.0, &shaper());
    assert!(
        plain_layout
            .lines
            .iter()
            .all(|row| row.effective_width == 80.0)
    );
}

#[test]
fn inactive_rule_discloses_a_quote_prefix_owned_by_the_active_quote() {
    let source = "> text\n>\n> ---\n";
    let cursor = source.find("text").expect("text in source");
    let rule_start = source.find("---").expect("rule source");
    let rule_line_start = source[..rule_start]
        .rfind('\n')
        .map_or(0, |newline| newline + 1);
    let block = present(source, Some(cursor))
        .into_iter()
        .find(|block| {
            block.lines.iter().any(|line| {
                line.source_range.start.0 == rule_line_start && line.kind == BlockKind::Rule
            })
        })
        .expect("the quoted rule block is presented");
    let line = block
        .lines
        .iter()
        .find(|line| line.source_range.start.0 == rule_line_start)
        .expect("the quoted rule line is presented");

    assert_eq!(line.visual_text, "> ");
    assert_eq!(
        line.quote
            .map(|quote| (quote.depth, quote.disclosed_depth)),
        Some((1, 1))
    );
    assert!(line.source_map.segments.iter().any(|segment| {
        segment.source_range == SourceRange::new(rule_line_start, rule_line_start + 2)
            && segment.visibility == hane_presentation::Visibility::ExpandedMarkup
    }));
    assert!(line.source_map.segments.iter().any(|segment| {
        segment.source_range == SourceRange::new(rule_start, line.source_range.end.0)
            && segment.visibility == hane_presentation::Visibility::HiddenMarkup
    }));
    assert!(line.rule_body_is_collapsed());

    let layout = layout_block(&block, 160.0, &shaper());
    let row = layout
        .lines
        .iter()
        .find(|row| row.line == 2)
        .expect("the quoted rule row is laid out");
    assert_eq!(row.body_x_origin, 0.0);
    assert_eq!(row.quote_bar_x_origin, None);
}

#[test]
fn caret_on_the_following_line_does_not_disclose_the_previous_rule_body() {
    let source = "> ---\n> text\n";
    let cursor = source.find("text").expect("text in source");
    let block = present(source, Some(cursor))
        .into_iter()
        .find(|block| block.lines.iter().any(|line| line.kind == BlockKind::Rule))
        .expect("the quoted rule block is presented");
    let line = block
        .lines
        .iter()
        .find(|line| line.kind == BlockKind::Rule)
        .expect("the quoted rule line is presented");

    assert_eq!(line.visual_text, "> ");
    assert!(line.rule_body_is_collapsed());
    assert!(line.source_map.segments.iter().any(|segment| {
        segment.source_range == SourceRange::new(2, 6)
            && segment.visibility == hane_presentation::Visibility::HiddenMarkup
    }));
}

#[test]
fn collapsed_nested_rule_keeps_the_disclosed_quote_body_gap() {
    let source = "> outer\n> > ---\n";
    let cursor = source.find("outer").expect("outer in source");
    let block = present(source, Some(cursor))
        .into_iter()
        .find(|block| block.lines.iter().any(|line| line.kind == BlockKind::Rule))
        .expect("the nested rule block is presented");
    let line = block
        .lines
        .iter()
        .find(|line| line.kind == BlockKind::Rule)
        .expect("the nested rule line is presented");
    assert!(line.rule_body_is_collapsed());

    let layout = layout_block(&block, 160.0, &shaper());
    let row = layout
        .lines
        .iter()
        .find(|row| row.line == line.line_id as usize)
        .expect("the nested rule row is laid out");
    assert_eq!(row.body_gap, 24.0);
    assert_eq!(row.body_x_origin, 40.0);
}

#[test]
fn nested_rule_keeps_list_and_quote_geometry_when_collapsed_or_disclosed() {
    for source in ["- item\n\n  ---\n", "> - item\n>\n>   ---\n"] {
        let rule_start = source.find("---").expect("rule source");
        let line_start = source[..rule_start]
            .rfind('\n')
            .map_or(0, |newline| newline + 1);
        let rule_line = source[..line_start]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count();
        let quoted = source.starts_with('>');
        let inactive_block = present(source, None)
            .into_iter()
            .find(|block| {
                block
                    .lines
                    .iter()
                    .any(|line| line.line_id as usize == rule_line && line.kind == BlockKind::Rule)
            })
            .expect("the nested rule block is presented");
        let inactive_layout = layout_block(&inactive_block, 160.0, &shaper());
        let inactive_row = inactive_layout
            .lines
            .iter()
            .find(|row| row.line == rule_line)
            .expect("collapsed rule row is laid out");
        let expected_collapsed_body = if quoted { 40.0 } else { 16.0 };
        assert_eq!(inactive_row.text_x_origin, expected_collapsed_body);
        assert_eq!(inactive_row.body_x_origin, expected_collapsed_body);
        assert_eq!(
            inactive_row.quote_bar_x_origin,
            quoted.then_some(14.0),
            "source: {source:?}"
        );

        let active_block = present(source, Some(rule_start + 1))
            .into_iter()
            .find(|block| {
                block
                    .lines
                    .iter()
                    .any(|line| line.line_id as usize == rule_line && line.kind == BlockKind::Rule)
            })
            .expect("the active nested rule block is presented");
        let active_line = active_block
            .lines
            .iter()
            .find(|line| line.line_id as usize == rule_line)
            .expect("active rule line");
        assert_eq!(
            active_line
                .list
                .as_ref()
                .expect("active rule keeps list metadata")
                .body_visual_start,
            VisualOffset(rule_start - line_start)
        );
        let active_layout = layout_block(&active_block, 160.0, &shaper());
        let active_row = active_layout
            .lines
            .iter()
            .find(|row| row.line == rule_line)
            .expect("active rule row is laid out");
        let expected_active_body = (rule_start - line_start) as f32 * 8.0;
        assert_eq!(active_row.text_x_origin, 0.0, "source: {source:?}");
        assert_eq!(
            active_row.body_x_origin, expected_active_body,
            "source: {source:?}"
        );
        assert_eq!(active_row.quote_bar_x_origin, None);
    }
}

#[test]
fn disclosed_outer_quote_prefix_keeps_hidden_nested_quote_inset() {
    let source = "> outer\n> > nested\n";
    let cursor = source.find("outer").expect("outer in source");
    let block = present(source, Some(cursor))
        .into_iter()
        .find(|block| block.lines.iter().any(|line| line.quote.is_some()))
        .expect("the quoted block is presented");

    assert_eq!(
        block.lines[0]
            .quote
            .map(|quote| (quote.depth, quote.disclosed_depth)),
        Some((1, 1))
    );
    assert_eq!(
        block.lines[1]
            .quote
            .map(|quote| (quote.depth, quote.disclosed_depth)),
        Some((2, 1))
    );

    let layout = layout_block(&block, 160.0, &shaper());
    let nested = SourceOffset(source.find("nested").expect("nested in source"));
    let point = layout
        .point_for_source(&block, nested, &shaper())
        .expect("nested text has a point");
    assert_eq!(point.x, 40.0);
    let nested_row = layout
        .lines
        .iter()
        .find(|row| row.line == 1)
        .expect("nested quote row is laid out");
    let nested_row_index = layout
        .lines
        .iter()
        .position(|row| row.line == 1)
        .expect("nested quote row has a layout index");
    assert_eq!(nested_row.text_x_origin, 0.0);
    assert_eq!(nested_row.body_x_origin, 40.0);
    assert_eq!(nested_row.body_gap, 24.0);
    assert_eq!(nested_row.quote_bar_x_origin, Some(30.0));
    assert_eq!(
        layout.source_at_x(&block, nested_row_index, 24.0, &shaper()),
        Some(nested)
    );
}

#[test]
fn disclosed_quote_prefix_uses_quote_marker_after_leading_indent() {
    let source = "  > outer\n  > > inner\n";
    let cursor = source.find("outer").expect("outer in source");
    let block = present(source, Some(cursor))
        .into_iter()
        .find(|block| block.lines.iter().any(|line| line.quote.is_some()))
        .expect("the indented quoted block is presented");
    let line = block
        .lines
        .iter()
        .find(|line| line.line_id == 1)
        .expect("the nested quote line is presented");
    assert_eq!(line.quote_marker_visual_ranges.len(), 2);

    let layout = layout_block(&block, 160.0, &shaper());
    let row = layout
        .lines
        .iter()
        .find(|row| row.line == 1)
        .expect("the nested quote row is laid out");
    assert_eq!(row.text_x_origin, 0.0);
    assert_eq!(row.body_x_origin, 56.0);
    assert_eq!(row.body_gap, 24.0);
    assert_eq!(row.quote_bar_x_origin, Some(46.0));
}

#[test]
fn disclosed_quote_prefix_does_not_consume_list_prefix_as_quote() {
    let source = "- item\n  > outer\n  > > inner\n";
    let cursor = source.find("outer").expect("outer in source");
    let block = present(source, Some(cursor))
        .into_iter()
        .find(|block| block.lines.iter().any(|line| line.line_id == 2))
        .expect("the list block is presented");
    let layout = layout_block(&block, 160.0, &shaper());
    let row = layout
        .lines
        .iter()
        .find(|row| row.line == 2)
        .expect("the nested quoted continuation is laid out");
    assert_eq!(row.text_x_origin, 0.0);
    assert_eq!(row.body_x_origin, 56.0);
    assert_eq!(row.body_gap, 24.0);
    assert_eq!(row.quote_bar_x_origin, Some(46.0));
}

#[test]
fn inactive_list_quote_places_inset_after_the_list_prefix() {
    for (source, line_id, expected_text_x, expected_marker_x) in [
        ("- > item\n", 0, 0.0, Some(0.0)),
        ("- item\n  > quote\n", 1, 40.0, None),
    ] {
        let block = present(source, None)
            .into_iter()
            .find(|block| {
                block
                    .lines
                    .iter()
                    .any(|line| line.line_id == line_id && line.quote.is_some())
            })
            .expect("the list quote block is presented");
        let layout = layout_block(&block, 160.0, &shaper());
        let row = layout
            .lines
            .iter()
            .find(|row| row.line_id == line_id)
            .expect("the list quote row is laid out");
        assert_eq!(row.text_x_origin, expected_text_x, "source: {source:?}");
        assert_eq!(row.marker_x_origin, expected_marker_x, "source: {source:?}");
        assert_eq!(row.body_x_origin, 40.0, "source: {source:?}");
        assert_eq!(row.quote_bar_x_origin, Some(30.0), "source: {source:?}");
    }
}

#[test]
fn lazy_continuation_image_keeps_quote_geometry() {
    let source = r#"> quote
![alt](dest)
"#;
    let block = present(source, None)
        .into_iter()
        .find(|block| block.lines.iter().any(|line| line.image.is_some()))
        .expect("the quoted image block is presented");
    let image = block
        .lines
        .iter()
        .find(|line| line.image.is_some())
        .expect("the lazy continuation image is presented");

    assert_eq!(image.kind, BlockKind::Image);
    assert_eq!(
        image.quote
            .map(|quote| (quote.depth, quote.disclosed_depth)),
        Some((1, 0))
    );
    let layout = layout_block(&block, 160.0, &shaper());
    let row = layout
        .lines
        .iter()
        .find(|row| row.line == image.line_id as usize)
        .expect("the image row is laid out");
    assert_eq!(row.body_x_origin, 24.0);
    assert_eq!(row.quote_bar_x_origin, Some(14.0));
}

#[test]
fn disclosed_outer_quote_prefix_precedes_nested_list_geometry() {
    let source = r#"> outer
> > - item
"#;
    let cursor = source.find("outer").expect("outer in source");
    let block = present(source, Some(cursor))
        .into_iter()
        .find(|block| block.lines.iter().any(|line| line.list.is_some()))
        .expect("the nested list block is presented");
    let line = block
        .lines
        .iter()
        .find(|line| line.list.is_some())
        .expect("the nested list line is presented");
    let layout = layout_block(&block, 160.0, &shaper());
    let row = layout
        .lines
        .iter()
        .find(|row| row.line == line.line_id as usize)
        .expect("the nested list row is laid out");
    assert_eq!(row.text_x_origin, 0.0);
    assert_eq!(row.marker_x_origin, Some(40.0));
    assert_eq!(row.body_x_origin, 56.0);
    assert_eq!(row.body_gap, 24.0);
    assert_eq!(row.quote_bar_x_origin, Some(30.0));
}

#[test]
fn nested_list_rows_use_semantic_depth_after_opening_prefixes_are_hidden() {
    let source = "- outer\n\n  - inner\n";
    let block = present(source, None)
        .into_iter()
        .find(|block| {
            block
                .lines
                .iter()
                .any(|line| line.list.as_ref().is_some_and(|list| list.owner.depth == 2))
        })
        .expect("the nested list block is presented");
    let inner_line = block
        .lines
        .iter()
        .position(|line| line.list.as_ref().is_some_and(|list| list.owner.depth == 2))
        .expect("the nested row is presented");
    let layout = layout_block(&block, 160.0, &shaper());
    let row = layout
        .lines
        .iter()
        .find(|row| row.line == inner_line)
        .expect("the nested row is laid out");
    assert_eq!(row.marker_x_origin, Some(24.0));
    assert_eq!(row.text_x_origin, 24.0);
    assert_eq!(row.body_x_origin, 40.0);

    let inner = SourceOffset(source.find("inner").expect("inner in source"));
    let point = layout
        .point_for_source(&block, inner, &shaper())
        .expect("inner has a point");
    assert_eq!(point.x, 40.0);
}

#[test]
fn deeply_nested_or_narrow_lists_keep_a_positive_effective_width() {
    let source = "999999999. value\n";
    let block = present(source, None)
        .into_iter()
        .find(|block| block.lines.iter().any(|line| line.list.is_some()))
        .expect("the ordered list block is presented");
    let layout = layout_block(&block, 1.0, &shaper());
    assert!(!layout.lines.is_empty());
    assert!(layout.lines.iter().all(|row| row.effective_width >= 1.0));
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
