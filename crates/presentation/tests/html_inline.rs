//! Issue #757 phase 1: allow-listed inline HTML presents exactly like its
//! Markdown equivalent, hides and discloses its tags like Markdown markers, and
//! stays literal source wherever it is not a recognized top-level pair.

use hane_document::{LineId, Revision, RopeBuffer, SourceRange, TextBuffer};
use hane_markdown::BlockIndex;
use hane_presentation::{
    BlockLine, BlockWindow, MarkerEdge, StyleKind, VisualBlock, VisualLine, Visibility,
    block_line_span, present_block, present_block_plain, present_markdown_with_disclosure,
    trailing_blank_lines,
};

const LINE_HEIGHT: f32 = 24.0;

fn line(source: &str, disclosure: Option<SourceRange>) -> VisualLine {
    present_markdown_with_disclosure(
        0,
        Revision(1),
        SourceRange::new(0, source.len()),
        source,
        LINE_HEIGHT,
        disclosure,
    )
}

fn runs(line: &VisualLine) -> Vec<(StyleKind, usize, usize)> {
    line.style_runs
        .iter()
        .map(|run| (run.kind, run.visual_range.start.0, run.visual_range.end.0))
        .collect()
}

fn find(source: &str, needle: &str) -> SourceRange {
    let start = source.find(needle).unwrap_or_else(|| panic!("{needle:?} in {source:?}"));
    SourceRange::new(start, start + needle.len())
}

fn hidden_tags(line: &VisualLine, source: &str) -> Vec<(String, Option<MarkerEdge>)> {
    line.source_map
        .segments
        .iter()
        .filter(|segment| segment.visibility == Visibility::HiddenMarkup)
        .map(|segment| {
            (
                source[segment.source_range.start.0..segment.source_range.end.0].to_owned(),
                segment.marker_edge,
            )
        })
        .collect()
}

#[test]
fn html_inline_styles_match_markdown_equivalents() {
    for (html, markdown, style) in [
        ("<b>bold</b> tail", "**bold** tail", StyleKind::Bold),
        ("<strong>bold</strong> tail", "**bold** tail", StyleKind::Bold),
        ("<i>bold</i> tail", "*bold* tail", StyleKind::Italic),
        ("<EM>bold</EM> tail", "*bold* tail", StyleKind::Italic),
        ("<s>bold</s> tail", "~~bold~~ tail", StyleKind::Strikethrough),
        ("<del>bold</del> tail", "~~bold~~ tail", StyleKind::Strikethrough),
        ("<strike>bold</strike> tail", "~~bold~~ tail", StyleKind::Strikethrough),
        ("<code>bold</code> tail", "`bold` tail", StyleKind::InlineCode),
    ] {
        let from_html = line(html, None);
        let from_markdown = line(markdown, None);
        assert_eq!(from_html.visual_text, "bold tail", "{html:?}");
        assert_eq!(from_html.visual_text, from_markdown.visual_text, "{html:?}");
        assert_eq!(runs(&from_html), runs(&from_markdown), "{html:?}");
        assert_eq!(runs(&from_html), [(style, 0, 4)], "{html:?}");
        assert_eq!(from_html.kind, from_markdown.kind, "{html:?}");
        assert_eq!(from_html.estimated_height, from_markdown.estimated_height, "{html:?}");
    }

    // A link takes the same style and openable target as `[label](url)`.
    let from_html = line("<a href=\"https://example.com/\">Hane</a> end", None);
    let from_markdown = line("[Hane](https://example.com/) end", None);
    assert_eq!(from_html.visual_text, "Hane end");
    assert_eq!(runs(&from_html), runs(&from_markdown));
    assert_eq!(runs(&from_html), [(StyleKind::Link, 0, 4)]);
    assert_eq!(from_html.links.len(), 1);
    assert_eq!(&*from_html.links[0].destination, "https://example.com/");
    assert_eq!(
        from_html.links[0].destination,
        from_markdown.links[0].destination
    );
    assert!(!from_html.links[0].uses_reference);

    // Markdown composes with the tags in both directions.
    let composed = line("<b>a *b* c</b> and *<s>d</s>*", None);
    assert_eq!(composed.visual_text, "a b c and d");
    assert!(runs(&composed).contains(&(StyleKind::Bold, 0, 5)));
    assert!(runs(&composed).contains(&(StyleKind::Italic, 2, 3)));
    assert!(runs(&composed).contains(&(StyleKind::Strikethrough, 10, 11)));
    assert!(runs(&composed).contains(&(StyleKind::Italic, 10, 11)));

    // `<code>` content is literal: no Markdown styling, hiding or links.
    let literal = line("<code>**x** [l](https://example.com/)</code>", None);
    assert_eq!(literal.visual_text, "**x** [l](https://example.com/)");
    assert_eq!(runs(&literal), [(StyleKind::InlineCode, 0, literal.visual_text.len())]);
    assert!(literal.links.is_empty());

    // Anything else is the source, unchanged.
    for source in [
        "<u>x</u> <span>y</span>",
        "<b>unclosed",
        "<a href=\"/relative\">x</a>",
        "<a href=\"javascript:alert(1)\">x</a>",
        "<b>x</strong>",
    ] {
        let raw = line(source, None);
        assert_eq!(raw.visual_text, source);
        assert!(raw.style_runs.is_empty(), "{source:?}");
        assert!(raw.links.is_empty(), "{source:?}");
    }
}

#[test]
fn html_markers_keep_source_positions_and_disclose_by_owner() {
    let source = "pre <b>bold</b> mid <i>it</i> end";
    let closed = line(source, None);
    assert_eq!(closed.visual_text, "pre bold mid it end");
    assert_eq!(
        hidden_tags(&closed, source),
        [
            ("<b>".to_owned(), Some(MarkerEdge::Opening)),
            ("</b>".to_owned(), Some(MarkerEdge::Closing)),
            ("<i>".to_owned(), Some(MarkerEdge::Opening)),
            ("</i>".to_owned(), Some(MarkerEdge::Closing)),
        ]
    );
    // The segments tile the whole source without a gap or an overlap.
    let mut cursor = 0;
    for segment in &closed.source_map.segments {
        assert_eq!(segment.source_range.start.0, cursor);
        cursor = segment.source_range.end.0;
    }
    assert_eq!(cursor, source.len());

    let open_b = "pre <b>bold</b> mid it end";
    let open_i = "pre bold mid <i>it</i> end";
    for (disclosure, expected) in [
        // Inside the content, and on either boundary of the element.
        (SourceRange::empty(9), open_b),
        (SourceRange::empty(4), open_b),
        (SourceRange::empty(15), open_b),
        (SourceRange::empty(20), open_i),
        // A selection opens every element it reaches, and nothing else.
        (SourceRange::new(9, 24), source),
        (SourceRange::new(13, 22), source),
        // Away from every tag.
        (SourceRange::empty(1), "pre bold mid it end"),
        (SourceRange::empty(source.len()), "pre bold mid it end"),
    ] {
        let line = line(source, Some(disclosure));
        assert_eq!(line.visual_text, expected, "{disclosure:?}");
        // Disclosure changes the picture, never the mapped source bytes.
        let covered = line
            .source_map
            .segments
            .iter()
            .map(|segment| segment.source_range.end.0 - segment.source_range.start.0)
            .sum::<usize>();
        assert_eq!(covered, source.len(), "{disclosure:?}");
    }

    // Nested elements: a touched element and its ancestors open together.
    let nested = "<b>x <i>y</i></b> z";
    assert_eq!(line(nested, Some(SourceRange::empty(8))).visual_text, nested);
    assert_eq!(line(nested, Some(SourceRange::empty(3))).visual_text, "<b>x y</b> z");
    assert_eq!(line(nested, None).visual_text, "x y z");

    // A find match that lands on an attribute only still opens its element.
    let link = "see <a href=\"https://example.com/x\">t</a> end";
    assert_eq!(line(link, None).visual_text, "see t end");
    assert_eq!(line(link, Some(find(link, "example"))).visual_text, link);

    // Expanded tags are source: the link target stays on the label only.
    let label = find(link, "t</a>").start.0;
    let expanded = line(link, Some(SourceRange::empty(label)));
    assert_eq!(expanded.visual_text, link);
    assert_eq!(expanded.links.len(), 1);
    assert_eq!(expanded.links[0].source_range, SourceRange::new(label, label + 1));
}

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

fn texts(block: &VisualBlock) -> Vec<String> {
    block.lines.iter().map(|line| line.visual_text.clone()).collect()
}

#[test]
fn html_pairs_present_through_the_block_path_only_at_top_level() {
    // A top-level paragraph, one line and several lines joined.
    let single = present("pre <b>bold</b> end\n", 0, None, false);
    assert_eq!(texts(&single)[0], "pre bold end");
    assert!(runs(&single.lines[0]).contains(&(StyleKind::Bold, 4, 8)));

    let joined = present("<b>one</b>\n<i>two</i>\n", 0, None, false);
    assert_eq!(texts(&joined)[..2], ["one", "two"]);
    // Opening the first line's element leaves the second line's closed.
    let joined = present("<b>one</b>\n<i>two</i>\n", 0, Some(SourceRange::empty(4)), false);
    assert_eq!(texts(&joined)[..2], ["<b>one</b>", "two"]);

    // A pair spanning two physical lines is source on both.
    let split = present("<b>one\ntwo</b>\n", 0, None, false);
    assert_eq!(texts(&split)[..2], ["<b>one", "two</b>"]);

    // Lists and quotes never recognize HTML pairs.
    for (source, needle) in [
        ("- a\n  <b>y</b>\n", "<b>y</b>"),
        ("- <b>x</b>\n", "<b>x</b>"),
        ("> a\n> <b>y</b>\n", "<b>y</b>"),
    ] {
        let block = present(source, 0, None, false);
        assert!(
            block.lines.iter().any(|line| line.visual_text.contains(needle)),
            "{source:?}: {:?}",
            texts(&block)
        );
        assert!(
            block
                .lines
                .iter()
                .all(|line| line.style_runs.iter().all(|run| run.kind != StyleKind::Bold)),
            "{source:?}"
        );
    }

    // Plain mode shows the source unchanged, with no HTML-derived style or link.
    let plain = present("pre <b>bold</b> <a href=\"https://example.com/\">x</a>\n", 0, None, true);
    assert_eq!(
        texts(&plain)[0],
        "pre <b>bold</b> <a href=\"https://example.com/\">x</a>"
    );
    assert!(plain.lines[0].style_runs.is_empty());
    assert!(plain.lines[0].links.is_empty());
}
