use hane_document::{Bias, LineId, RopeBuffer, SourceOffset, SourceRange, TextBuffer};
use hane_markdown::{BlockIndex, NodeKind};
use hane_presentation::testing::FixedAdvanceShaper;
use hane_presentation::{
    BlockKind, BlockLine, BlockWindow, VisualBlock, VisualOffset, block_height_with_disclosure,
    block_line_span, front_matter_line_disclosure, front_matter_lines, front_matter_row_lines,
    layout_block, present_front_matter_block,
};
use std::ops::Range;

const LINE_HEIGHT: f32 = 26.0;

/// Presents block 0 (which must be Front Matter) for `render`, expanded or
/// collapsed exactly as the given disclosure decides.
fn present(
    source: &str,
    render: Option<Range<usize>>,
    disclosure: Option<SourceRange>,
) -> (RopeBuffer, BlockIndex, VisualBlock) {
    let buffer = RopeBuffer::from_text(source);
    let index = BlockIndex::from_buffer(&buffer);
    let block = index.block(0).expect("block 0");
    assert_eq!(block.kind, NodeKind::FrontMatter, "{source:?}");
    let front_matter = index.front_matter().expect("front matter");
    let span = block_line_span(&buffer, &block).expect("span");
    let lines = front_matter_lines(&buffer, front_matter.span()).expect("front matter lines");
    let render = render.unwrap_or_else(|| span.clone());
    let expanded = disclosure.is_some_and(|active| front_matter.span().is_disclosed_by(active));
    let rows = front_matter_row_lines(&lines, &span, &render, expanded);
    let texts = rows
        .iter()
        .map(|line| {
            let range = buffer.line_range(LineId(*line)).expect("line range");
            (*line, range, buffer.text(range).expect("line text"))
        })
        .collect::<Vec<_>>();
    let block_lines = texts
        .iter()
        .map(|(line, range, text)| BlockLine {
            line: *line,
            range: *range,
            text,
            disclosure: None,
        })
        .collect::<Vec<_>>();
    let visual = present_front_matter_block(
        &block,
        buffer.revision(),
        &BlockWindow {
            trailing_blank_lines: 0,
            span,
            lines: &block_lines,
            clipped_fence_lines: &[],
            zero_height_fence_rows_before: 0,
            zero_height_fence_rows_after: 0,
            table_delimiter_line: None,
            render,
            joined: None,
            block_disclosure: disclosure.filter(|_| expanded),
        },
        LINE_HEIGHT,
        front_matter,
        lines,
    );
    (buffer, index, visual)
}

fn summary_of(source: &str) -> (String, usize) {
    let (_, index, visual) = present(source, None, None);
    let anchor = index.front_matter().expect("front matter").anchor().0;
    assert_eq!(visual.lines[0].kind, BlockKind::FrontMatter, "{source:?}");
    (visual.lines[0].visual_text.clone(), anchor)
}

#[test]
fn front_matter_summary_has_one_row_and_stable_source_anchor() {
    // The date is declared after the other items; it still leads the summary.
    let long_title = "長い日本語のタイトルがとても長く続いて二十四文字を超える文章です";
    let author = "佐藤 🧑\u{200d}💻";
    let source = format!(
        "---\ntitle: {long_title}\nauthor: {author}\ndate: \"2026-11-21\"\nproject: Hane\n---\n\n# 本文\n"
    );
    let (buffer, _, visual) = present(&source, None, None);
    let expected_title = format!("{}…", long_title.chars().take(23).collect::<String>());
    assert_eq!(
        visual.lines[0].visual_text,
        format!("2026-11-21 · title: {expected_title} · author: {author} · +1")
    );

    // One summary row plus the blank line after the closing delimiter, which
    // keeps its normal height. The omitted lines add no height.
    assert_eq!(visual.lines.len(), 2);
    assert_eq!(visual.lines[0].kind, BlockKind::FrontMatter);
    assert_eq!(visual.lines[1].line_id, 6);
    assert!(visual.lines[1].visual_text.is_empty());
    assert_eq!(visual.omitted_lines, 1..6);
    assert_eq!(visual.height(), 2.0 * LINE_HEIGHT);
    assert!(visual.covers(&(0..7)));

    // Every visual position of the row, glyphs and trailing space alike,
    // lands on the date value's start with either bias.
    let anchor = source.find("2026-11-21").expect("date value");
    let row = &visual.lines[0];
    for offset in [0, 1, row.visual_text.len() / 2, row.visual_text.len()] {
        for bias in [Bias::Before, Bias::After] {
            assert_eq!(
                row.source_map
                    .visual_to_source(VisualOffset(offset), bias)
                    .map(|candidate| candidate.source_offset),
                Some(SourceOffset(anchor)),
                "visual {offset} {bias:?}"
            );
        }
    }

    // Narrow widths still produce one row for the summary; clicks on a glyph and
    // in the blank space to the right resolve to the same anchor.
    let shaper = FixedAdvanceShaper::default();
    for width in [40.0, 160.0, 4_000.0] {
        let layout = layout_block(&visual, width, &shaper);
        assert_eq!(layout.lines.len(), 2, "width {width}");
        assert_eq!(layout.lines[0].line_id, 0);
        assert_eq!(layout.lines[0].height, LINE_HEIGHT);
        for x in [0.0, 12.0, 100_000.0] {
            for bias in [Bias::Before, Bias::After] {
                assert_eq!(
                    layout.source_at_x_with_bias(&visual, 0, x, &shaper, bias),
                    Some(SourceOffset(anchor)),
                    "width {width} x {x} {bias:?}"
                );
            }
        }
    }
    let _ = buffer;

    // Anchors and fallbacks.
    let author_only = "---\nauthor: 佐藤\n---\n";
    let (summary, anchor) = summary_of(author_only);
    assert_eq!(summary, "メタデータ · author: 佐藤");
    assert_eq!(anchor, author_only.find("佐藤").expect("value"));

    let invalid = "---\ndate: 2026-02-29\nauthor: a\n---\n";
    let (summary, anchor) = summary_of(invalid);
    assert_eq!(summary, "メタデータ · 日付無効 · author: a");
    assert_eq!(anchor, invalid.find("a\n---").expect("value"));

    let duplicate = "---\ndate: 2026-11-21\ndate: 2026-11-22\n---\n";
    assert_eq!(summary_of(duplicate), ("メタデータ · 日付無効".to_owned(), 0));

    assert_eq!(summary_of("---\n---\n"), ("メタデータなし".to_owned(), 0));
    assert_eq!(
        summary_of("---\n\n# コメント\n---\n"),
        ("メタデータなし".to_owned(), 0)
    );
    assert_eq!(summary_of("---\n- item\n---\n"), ("メタデータ".to_owned(), 0));
    assert_eq!(
        summary_of("---\ndate: 2026-11-21\n---\n").0,
        "2026-11-21".to_owned()
    );
    let many = "---\na: 1\nb: 2\nc: 3\nd: 4\n---\n";
    assert_eq!(summary_of(many).0, "メタデータ · a: 1 · b: 2 · +2");
    assert_eq!(summary_of(many).1, many.find("1\n").expect("value"));
}

#[test]
fn front_matter_disclosure_respects_exact_boundaries() {
    let caret = |offset: usize| Some(SourceRange::empty(offset));
    let span = |start: usize, end: usize| Some(SourceRange::new(start, end));
    // (source, disclosure, expanded, lines when expanded)
    let closed = "---\ndate: 2026-11-21\n---\n\n# 本文\n";
    let unterminated = "---\na: b\n---";
    let terminated = "---\na: b\n---\n";
    let cases: Vec<(&str, Option<SourceRange>, bool)> = vec![
        (closed, None, false),
        (closed, caret(0), true),
        (closed, caret(4), true),
        // Inside the closing line, before its line ending.
        (closed, caret(24), true),
        // The start of the blank line after the block is body, not metadata.
        (closed, caret(25), false),
        (closed, caret(26), false),
        // Selections and composition ranges with length intersect.
        (closed, span(25, 26), false),
        (closed, span(24, 26), true),
        (closed, span(0, 3), true),
        (closed, span(5, 9), true),
        // A zero-length composition follows the caret rule.
        (closed, caret(24), true),
        // The unterminated closing line owns the caret at the document end.
        (unterminated, caret(12), true),
        (unterminated, caret(11), true),
        (terminated, caret(13), false),
        (terminated, caret(12), true),
    ];
    for (source, disclosure, expanded) in cases {
        let label = format!("{source:?} {disclosure:?}");
        let (buffer, index, visual) = present(source, None, disclosure);
        let block = index.block(0).expect("block 0");
        let span = block_line_span(&buffer, &block).expect("span");
        assert_eq!(visual.omitted_lines.is_empty(), expanded, "{label}");
        assert!(visual.covers(&span), "{label}");

        // Presented rows, coverage and the height estimate all agree.
        let estimated =
            block_height_with_disclosure(&buffer, &index, &block, LINE_HEIGHT, disclosure);
        assert!(
            (estimated - visual.height()).abs() < f32::EPSILON,
            "{label}: estimated {estimated} presented {}",
            visual.height()
        );
        let lines = front_matter_lines(&buffer, index.front_matter().expect("fm").span())
            .expect("front matter lines");
        for line in &visual.lines {
            let expected = front_matter_line_disclosure(
                line.line_id as usize,
                &lines,
                disclosure.filter(|_| expanded),
            );
            assert_eq!(line.disclosure, expected, "{label} line {}", line.line_id);
        }
        if expanded {
            // Every line of the block is its own raw source, identity mapped.
            assert!(visual.lines.len() >= lines.len(), "{label}");
            for line in visual.lines.iter().filter(|line| lines.contains(&(line.line_id as usize))) {
                assert_eq!(line.kind, BlockKind::Unsupported, "{label}");
                for offset in 0..=line.visual_text.len() {
                    let at = line
                        .source_map
                        .visual_to_source(VisualOffset(offset), Bias::After)
                        .map(|candidate| candidate.source_offset);
                    assert_eq!(
                        at,
                        Some(SourceOffset(line.source_range.start.0 + offset)),
                        "{label} line {} offset {offset}",
                        line.line_id
                    );
                }
            }
        } else {
            assert_eq!(visual.lines[0].kind, BlockKind::FrontMatter, "{label}");
        }
    }
}
