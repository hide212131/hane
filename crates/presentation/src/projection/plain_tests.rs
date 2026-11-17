use super::*;
use hane_document::LineId;

const LINE_HEIGHT: f32 = 26.0;

const MIXED: &str = "# Heading\n\n**bold** `code` [t](https://e.com)\n\n- [ ] task\n- item\n\n> quote\n\n| a | b |\n| --- | --- |\n| 1 | 2 |\n\n```rust\nlet x = 1;\n```\n\n![alt](a.png)\n";

/// Presents every block of `source` in full through [`present_block_plain`].
fn plain_blocks(source: &str) -> (RopeBuffer, BlockIndex, Vec<VisualBlock>) {
    let document = RopeBuffer::from_text(source);
    let index = BlockIndex::from_buffer(&document);
    let blocks = index
        .blocks()
        .map(|block| {
            let span = block_line_span(&document, &block).expect("block span");
            let rows = span
                .clone()
                .map(|line| {
                    let range = document.line_range(LineId(line)).expect("line range");
                    (line, range, document.text(range).expect("line text"))
                })
                .collect::<Vec<_>>();
            let lines = rows
                .iter()
                .map(|(line, range, text)| BlockLine {
                    line: *line,
                    range: *range,
                    text,
                    disclosure: None,
                })
                .collect::<Vec<_>>();
            present_block_plain(
                &block,
                Revision(1),
                &BlockWindow {
                    trailing_blank_lines: trailing_blank_lines(&document, &span),
                    span: span.clone(),
                    lines: &lines,
                    clipped_fence_lines: &[],
                    zero_height_fence_rows_before: 0,
                    zero_height_fence_rows_after: 0,
                    table_delimiter_line: None,
                    render: span,
                    joined: None,
                    block_disclosure: None,
                },
                LINE_HEIGHT,
            )
        })
        .collect();
    (document, index, blocks)
}

fn source_line(document: &RopeBuffer, line: &VisualLine) -> String {
    document
        .text(line.source_range)
        .expect("source line")
        .trim_end_matches(['\r', '\n'])
        .to_owned()
}

#[test]
fn plain_lines_are_identity_mapped() {
    for source in [MIXED, &MIXED.replace('\n', "\r\n")] {
        let (document, _, blocks) = plain_blocks(source);
        let mut seen = 0;
        for line in blocks.iter().flat_map(|block| &block.lines) {
            seen += 1;
            assert_eq!(line.visual_text, source_line(&document, line));
            assert_eq!(line.source_map.segments.len(), 1);
            let segment = line.source_map.segments[0];
            assert_eq!(segment.visibility, Visibility::Visible);
            assert_eq!(segment.source_range, line.source_range);
            assert_eq!(segment.visual_range.start, VisualOffset(0));
            for visual in 0..=line.visual_text.len() {
                let source = line
                    .source_map
                    .visual_to_source(VisualOffset(visual), Bias::After)
                    .expect("visual maps to source");
                assert_eq!(source.source_offset.0, line.source_range.start.0 + visual);
                let back = line
                    .source_map
                    .source_to_visual(source.source_offset, Bias::After)
                    .expect("source maps to visual");
                assert_eq!(back.visual_offset, VisualOffset(visual));
            }
        }
        assert!(seen > 0);
    }
}

#[test]
fn plain_lines_carry_no_markdown_decoration() {
    let (_, _, blocks) = plain_blocks(MIXED);
    for block in &blocks {
        assert_eq!(block.kind, BlockKind::Unsupported);
        for line in &block.lines {
            assert!(line.style_runs.is_empty(), "{:?}", line.visual_text);
            assert!(line.links.is_empty());
            assert!(line.image.is_none());
            assert!(line.list.is_none());
            assert!(line.quote.is_none());
            assert!(line.table_row.is_none());
            assert_eq!(line.kind, BlockKind::Unsupported);
        }
    }
}

#[test]
fn plain_block_has_no_zero_height_lines() {
    let (_, _, blocks) = plain_blocks(MIXED);
    for block in &blocks {
        assert_eq!(block.zero_height_lines_before, 0);
        assert_eq!(block.zero_height_lines_after, 0);
        assert!(block.table_projection.is_none());
        for line in &block.lines {
            assert_eq!(line.estimated_height, LINE_HEIGHT, "{:?}", line.visual_text);
        }
    }
}

#[test]
fn plain_block_strips_crlf_from_visual_text_only() {
    let (_, _, blocks) = plain_blocks("a\r\nb\r\n");
    let lines = blocks.iter().flat_map(|block| &block.lines).collect::<Vec<_>>();
    assert_eq!(lines[0].visual_text, "a");
    assert_eq!(lines[1].visual_text, "b");
    assert_eq!(lines[0].source_range, SourceRange::new(0, 3));
    assert_eq!(lines[1].source_range, SourceRange::new(3, 6));
}

#[test]
fn plain_block_height_matches_presented_block() {
    for source in [MIXED, "plain", "a\n\n\n", "```\ncode\n```\n\n", "| a |\n| - |\n"] {
        let (document, index, blocks) = plain_blocks(source);
        for (block, visual) in index.blocks().zip(&blocks) {
            assert_eq!(
                plain_block_height(&document, &index, &block, LINE_HEIGHT),
                visual.height(),
                "{source:?} block {}",
                block.ordinal
            );
        }
    }
}

#[test]
fn plain_block_heights_sum_to_line_count() {
    for (source, rows) in [
        ("plain", 1),
        ("a\n", 2),
        ("a\n\n\n\nb", 5),
        ("a\r\nb\r\n", 3),
        (MIXED, MIXED.matches('\n').count() + 1),
    ] {
        let document = RopeBuffer::from_text(source);
        let index = BlockIndex::from_buffer(&document);
        let total = plain_block_heights(&document, &index, LINE_HEIGHT)
            .into_iter()
            .sum::<f32>();
        assert_eq!(total, rows as f32 * LINE_HEIGHT, "{source:?}");
    }
}

#[test]
fn plain_window_reports_lines_before_and_after() {
    let source = "- a\n- b\n- c\n- d\n- e\n";
    let document = RopeBuffer::from_text(source);
    let index = BlockIndex::from_buffer(&document);
    let block = index.blocks().next().expect("list block");
    let span = block_line_span(&document, &block).expect("span");
    let render = 1..3;
    let rows = span
        .clone()
        .map(|line| {
            let range = document.line_range(LineId(line)).expect("line range");
            (line, range, document.text(range).expect("line text"))
        })
        .collect::<Vec<_>>();
    let lines = rows
        .iter()
        .map(|(line, range, text)| BlockLine {
            line: *line,
            range: *range,
            text,
            disclosure: None,
        })
        .collect::<Vec<_>>();
    let visual = present_block_plain(
        &block,
        Revision(1),
        &BlockWindow {
            trailing_blank_lines: 0,
            span: span.clone(),
            lines: &lines,
            clipped_fence_lines: &[],
            zero_height_fence_rows_before: 0,
            zero_height_fence_rows_after: 0,
            table_delimiter_line: None,
            render,
            joined: None,
            block_disclosure: None,
        },
        LINE_HEIGHT,
    );
    assert_eq!(visual.lines.len(), 2);
    assert_eq!(visual.lines_before, 1);
    assert_eq!(
        visual.lines_before + visual.lines.len() + visual.lines_after,
        span.len()
    );
}

#[test]
fn matches_plain_ignores_kind() {
    let source = "| a | b |\n| --- | --- |\n| 1 | 2 |\n";
    let (_, index, blocks) = plain_blocks(source);
    let block = index.blocks().next().expect("table block");
    assert!(blocks[0].matches_plain(&block));
    assert!(!blocks[0].matches(&block));
}
