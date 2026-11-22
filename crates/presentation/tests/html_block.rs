//! Issue #757 phase 2: a single-line `p`, `h1`–`h6` or `hr` HTML block presents
//! like its Markdown equivalent, discloses like Markdown markers and Front
//! Matter, keeps its block height in step with that, and stays source otherwise.

use hane_document::{LineId, RopeBuffer, SourceRange, TextBuffer};
use hane_markdown::BlockIndex;
use hane_presentation::{
    BlockKind, BlockLine, BlockWindow, StyleKind, VisualBlock, block_heights_with_disclosure,
    block_line_span, present_block, present_block_plain, trailing_blank_lines,
};

const LINE_HEIGHT: f32 = 24.0;

fn line_disclosure(range: SourceRange, disclosure: Option<SourceRange>) -> Option<SourceRange> {
    let disclosure = disclosure?;
    if disclosure.is_empty() {
        (range.start <= disclosure.start && disclosure.start <= range.end).then_some(disclosure)
    } else {
        disclosure.intersects(range).then(|| SourceRange {
            start: disclosure.start.max(range.start),
            end: disclosure.end.min(range.end),
        })
    }
}

/// Presents block `ordinal` of `source` the way the editor does: one
/// `present_block` call over the block's whole span.
fn present(source: &str, ordinal: usize, disclosure: Option<SourceRange>, plain: bool) -> VisualBlock {
    let buffer = RopeBuffer::from_text(source);
    let index = BlockIndex::from_buffer(&buffer);
    let block = index.block(ordinal).expect("block");
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
            disclosure: line_disclosure(*range, disclosure),
        })
        .collect::<Vec<_>>();
    let window = BlockWindow {
        trailing_blank_lines: trailing_blank_lines(&buffer, &span),
        render: span.clone(),
        span,
        lines: &lines,
        clipped_fence_lines: &[],
        zero_height_fence_rows_before: 0,
        zero_height_fence_rows_after: 0,
        table_delimiter_line: None,
        joined: None,
        block_disclosure: disclosure,
    };
    if plain {
        present_block_plain(&block, buffer.revision(), &window, LINE_HEIGHT, None)
    } else {
        present_block(&block, buffer.revision(), &window, LINE_HEIGHT)
    }
}

fn first_text(block: &VisualBlock) -> &str {
    &block.lines[0].visual_text
}

#[test]
fn html_block_presents_like_its_markdown_equivalent() {
    for (html, markdown) in [
        ("<p>para</p>\n", "para\n"),
        ("  <P>para</P>  \n", "para\n"),
        ("<h1>Title</h1>\n", "# Title\n"),
        ("<h2>Title</h2>\n", "## Title\n"),
        ("<h3>Title</h3>\n", "### Title\n"),
        ("<h6>Title</h6>\n", "###### Title\n"),
        ("<hr>\n", "---\n"),
        ("<hr />\n", "---\n"),
    ] {
        let from_html = present(html, 0, None, false);
        let from_markdown = present(markdown, 0, None, false);
        assert_eq!(from_html.kind, from_markdown.kind, "{html:?}");
        assert_eq!(from_html.lines[0].kind, from_markdown.lines[0].kind, "{html:?}");
        assert_eq!(first_text(&from_html), first_text(&from_markdown), "{html:?}");
        assert_eq!(
            from_html.lines[0].estimated_height,
            from_markdown.lines[0].estimated_height,
            "{html:?}"
        );
    }

    // Phase-1 tags inside the content, with their style and link target.
    let source = "<p>a <b>b</b> <a href=\"https://example.com/\">l</a></p>\n";
    let block = present(source, 0, None, false);
    assert_eq!(first_text(&block), "a b l");
    let runs = block.lines[0]
        .style_runs
        .iter()
        .map(|run| (run.kind, run.visual_range.start.0, run.visual_range.end.0))
        .collect::<Vec<_>>();
    assert!(runs.contains(&(StyleKind::Bold, 2, 3)));
    assert!(runs.contains(&(StyleKind::Link, 4, 5)));
    assert_eq!(block.lines[0].links.len(), 1);
    assert_eq!(&*block.lines[0].links[0].destination, "https://example.com/");

    // The segments tile the whole line.
    let mut cursor = 0;
    for segment in &block.lines[0].source_map.segments {
        assert_eq!(segment.source_range.start.0, cursor);
        cursor = segment.source_range.end.0;
    }
    assert_eq!(cursor, source.len());

    // The content is not re-parsed as Markdown.
    let raw = present("<p>**raw** [l](https://example.com/)</p>\n", 0, None, false);
    assert_eq!(first_text(&raw), "**raw** [l](https://example.com/)");
    assert!(raw.lines[0].style_runs.is_empty());
    assert!(raw.lines[0].links.is_empty());
}

#[test]
fn html_block_other_shapes_stay_source() {
    for (source, first_line) in [
        ("<div>x</div>\n", "<div>x</div>"),
        ("<p>a</p><p>b</p>\n", "<p>a</p><p>b</p>"),
        ("<p>a</p> tail\n", "<p>a</p> tail"),
        ("<p>a\nb</p>\n", "<p>a"),
        ("<p>unclosed\n", "<p>unclosed"),
        ("<p></p>\n", "<p></p>"),
        ("<hr> tail\n", "<hr> tail"),
    ] {
        let block = present(source, 0, None, false);
        assert_eq!(first_text(&block), first_line, "{source:?}");
        assert!(block.lines[0].style_runs.is_empty(), "{source:?}");
        assert_eq!(block.kind, BlockKind::Unsupported, "{source:?}");
    }
}

#[test]
fn html_block_discloses_its_source_line_and_plain_mode_shows_source() {
    let source = "<h1>Title</h1>\n\nafter\n";
    let closed = present(source, 0, None, false);
    assert_eq!(closed.kind, BlockKind::Heading(1));
    assert_eq!(first_text(&closed), "Title");

    // The caret anywhere in the element line shows the whole line.
    for caret in [0, 4, 9, 14] {
        let open = present(source, 0, Some(SourceRange::empty(caret)), false);
        assert_eq!(first_text(&open), "<h1>Title</h1>", "caret {caret}");
        assert_eq!(open.kind, BlockKind::Unsupported, "caret {caret}");
    }
    // The blank line the index folds into the block does not disclose it.
    let blank = present(source, 0, Some(SourceRange::empty(15)), false);
    assert_eq!(first_text(&blank), "Title");

    let plain = present(source, 0, None, true);
    assert_eq!(first_text(&plain), "<h1>Title</h1>");
    assert_eq!(plain.kind, BlockKind::Unsupported);
}

#[test]
fn html_block_height_matches_disclosure_and_measured_rows() {
    let source = "<h1>Title</h1>\n\n<p>para</p>\n\n<hr>\n\ntail\n";
    let buffer = RopeBuffer::from_text(source);
    let index = BlockIndex::from_buffer(&buffer);
    let close = |a: f32, b: f32| (a - b).abs() < 1e-3;

    // The index keeps counting physical lines: the element and its blank line.
    for ordinal in 0..3 {
        assert_eq!(index.block(ordinal).unwrap().line_count, 2);
    }

    let collapsed = block_heights_with_disclosure(&buffer, &index, LINE_HEIGHT, None);
    assert!(close(collapsed[0], LINE_HEIGHT * 1.65 + LINE_HEIGHT), "{collapsed:?}");
    assert!(close(collapsed[1], 2.0 * LINE_HEIGHT), "{collapsed:?}");
    assert!(close(collapsed[2], 2.0 * LINE_HEIGHT), "{collapsed:?}");

    // The initial estimate is the measured presentation's first row plus the
    // remaining physical lines, collapsed and disclosed alike.
    for (ordinal, caret) in [(0usize, None), (0, Some(4)), (1, None), (2, None)] {
        let disclosure = caret.map(SourceRange::empty);
        let heights = block_heights_with_disclosure(&buffer, &index, LINE_HEIGHT, disclosure);
        let presented = present(source, ordinal, disclosure, false);
        let expected = presented.lines[0].estimated_height
            + LINE_HEIGHT * (index.block(ordinal).unwrap().line_count - 1) as f32;
        assert!(
            close(heights[ordinal], expected),
            "block {ordinal} caret {caret:?}: {} vs {expected}",
            heights[ordinal]
        );
    }

    // Disclosed, the heading is an ordinary row; neighbours are unchanged.
    let disclosed =
        block_heights_with_disclosure(&buffer, &index, LINE_HEIGHT, Some(SourceRange::empty(4)));
    assert!(close(disclosed[0], 2.0 * LINE_HEIGHT), "{disclosed:?}");
    assert!(close(disclosed[1], collapsed[1]));
    assert!(close(disclosed[2], collapsed[2]));
}

#[test]
fn raw_html_fallback_does_not_create_resource_targets() {
    let image = "![i](https://example.com/a.png)";
    let link = "[l](https://example.com/)";
    for source in [
        format!("<div><img src=\"https://example.com/a.png\"><a href=\"https://example.com/\">x</a></div>\n"),
        format!("<script>\n{image}\n{link}\n</script>\n"),
        format!("<p>{image} {link}</p>\n"),
        format!("<p>{image}\n{link}</p>\n"),
        format!("<h1>{link}</h1>\n"),
        format!("<p><code>{link}</code></p>\n"),
    ] {
        for disclosure in [None, Some(SourceRange::empty(1)), Some(SourceRange::empty(8))] {
            let block = present(&source, 0, disclosure, false);
            for line in &block.lines {
                assert!(line.image.is_none(), "{source:?} {disclosure:?}: {line:?}");
                assert!(line.links.is_empty(), "{source:?} {disclosure:?}: {line:?}");
            }
        }
    }
}
