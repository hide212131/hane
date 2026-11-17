use super::*;
use hane_document::LineId;

const LINE_HEIGHT: f32 = 26.0;

const MIXED: &str = "# Heading\n\n**bold** `code` [t](https://e.com)\n\n- [ ] task\n- item\n\n> quote\n\n| a | b |\n| --- | --- |\n| 1 | 2 |\n\n```rust\nlet x = 1;\n```\n\n![alt](a.png)\n";

/// Presents every block of `source` in full through [`present_block_plain`].
fn plain_blocks(source: &str) -> (RopeBuffer, BlockIndex, Vec<VisualBlock>) {
    plain_blocks_with(source, false)
}

/// [`plain_blocks`], optionally with the whole block handed to the highlighter.
fn plain_blocks_with(
    source: &str,
    highlight: bool,
) -> (RopeBuffer, BlockIndex, Vec<VisualBlock>) {
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
            let trailing = trailing_blank_lines(&document, &span);
            let content_lines = &lines[..lines.len() - trailing.min(lines.len())];
            let input = PlainHighlightInput::Parse {
                lines: content_lines,
            };
            present_block_plain(
                &block,
                Revision(1),
                &BlockWindow {
                    trailing_blank_lines: trailing,
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
                highlight.then_some(&input),
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
        None,
    );
    assert_eq!(visual.lines.len(), 2);
    assert_eq!(visual.lines_before, 1);
    assert_eq!(
        visual.lines_before + visual.lines.len() + visual.lines_after,
        span.len()
    );
}

const HIGHLIGHT_SOURCES: [&str; 6] = [
    MIXED,
    "# Title\r\n\r\n- a\r\n  - b\r\n\r\n> q\r\n> r\r\n",
    "Title\n===\n\ntext **b** `c`\n",
    "| a | b |\n| --- | --- |\n| 1 | 2 |\n",
    "```rust\nlet x = 1;\n```\n\n    indented\n",
    "- [x] done\n- [ ] todo\n\n<div>\nraw\n</div>\n\n---\n",
];

#[test]
fn plain_highlight_changes_only_style_runs() {
    let mut colored = 0;
    for source in HIGHLIGHT_SOURCES {
        let (_, _, off) = plain_blocks_with(source, false);
        let (_, _, on) = plain_blocks_with(source, true);
        assert_eq!(off.len(), on.len());
        for (off_block, on_block) in off.iter().zip(&on) {
            colored += on_block
                .lines
                .iter()
                .filter(|line| !line.style_runs.is_empty())
                .count();
            let mut stripped = on_block.clone();
            for line in &mut stripped.lines {
                line.style_runs.clear();
            }
            assert_eq!(&stripped, off_block, "{source:?}");
            assert_eq!(on_block.kind, off_block.kind);
            assert_eq!(on_block.source_range, off_block.source_range);
            for (on_line, off_line) in on_block.lines.iter().zip(&off_block.lines) {
                assert_eq!(on_line.visual_text, off_line.visual_text);
                assert_eq!(on_line.source_map, off_line.source_map);
                assert_eq!(on_line.estimated_height, off_line.estimated_height);
            }
        }
    }
    assert!(colored > 0, "highlighting produced no runs at all");
}

#[test]
fn plain_highlight_keeps_block_height_invariants() {
    for source in HIGHLIGHT_SOURCES {
        let (document, index, blocks) = plain_blocks_with(source, true);
        for (block, visual) in index.blocks().zip(&blocks) {
            assert_eq!(
                plain_block_height(&document, &index, &block, LINE_HEIGHT),
                visual.height(),
                "{source:?} block {}",
                block.ordinal
            );
            assert_eq!(visual.kind, BlockKind::Unsupported);
        }
    }
}

#[test]
fn plain_highlight_none_matches_issue_737_output() {
    for source in HIGHLIGHT_SOURCES {
        let (_, _, blocks) = plain_blocks_with(source, false);
        for line in blocks.iter().flat_map(|block| &block.lines) {
            assert!(line.style_runs.is_empty(), "{:?}", line.visual_text);
        }
    }
}

#[test]
fn plain_highlight_window_only_parse_does_not_panic() {
    let source = "**a\nb\nc**\nd\n";
    let document = RopeBuffer::from_text(source);
    let index = BlockIndex::from_buffer(&document);
    let block = index.blocks().next().expect("paragraph block");
    let span = block_line_span(&document, &block).expect("span");
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
    // The window starts inside the strong span, so its opener is out of view.
    let input = PlainHighlightInput::Parse { lines: &lines[1..] };
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
            render: 1..span.end,
            joined: None,
            block_disclosure: None,
        },
        LINE_HEIGHT,
        Some(&input),
    );
    assert!(!visual.lines.is_empty());
    for line in &visual.lines {
        for run in &line.style_runs {
            assert!(run.visual_range.end.0 <= line.visual_text.len());
        }
    }
}

#[test]
fn matches_plain_ignores_kind() {
    let source = "| a | b |\n| --- | --- |\n| 1 | 2 |\n";
    let (_, index, blocks) = plain_blocks(source);
    let block = index.blocks().next().expect("table block");
    assert!(blocks[0].matches_plain(&block));
    assert!(!blocks[0].matches(&block));
}
