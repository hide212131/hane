//! Issue #757 phase 2: a top-level HTML block that is one complete `p`, `h1`–`h6`
//! or `hr` element on a single line is published by the block index as a display
//! kind. Full and incremental parses must agree on it.

use hane_document::{RopeBuffer, SourceRange, TextBuffer};
use hane_markdown::{
    BlockIndex, HtmlBlockKind, HtmlKind, NodeKind, recognize_html_block_line,
};

fn kind_of(line: &str) -> Option<HtmlBlockKind> {
    recognize_html_block_line(line).map(|recognized| recognized.kind)
}

#[test]
fn html_block_line_recognizes_only_one_complete_element() {
    let line = "<p>hi</p>\n";
    let recognized = recognize_html_block_line(line).expect("paragraph");
    assert_eq!(recognized.kind, HtmlBlockKind::Paragraph);
    assert_eq!(recognized.open, 0..3);
    assert_eq!(recognized.content, 3..5);
    assert_eq!(recognized.close, 5..9);

    // Surrounding whitespace belongs to the hidden tags; CRLF stays outside.
    let line = "  <H2 class=\"x\">a <b>b</b></H2>  \r\n";
    let recognized = recognize_html_block_line(line).expect("heading");
    assert_eq!(recognized.kind, HtmlBlockKind::Heading(2));
    assert_eq!(recognized.open.start, 0);
    assert_eq!(recognized.close.end, line.len() - 2);
    assert_eq!(recognized.inline.len(), 1);
    assert_eq!(recognized.inline[0].kind, HtmlKind::Bold);

    assert_eq!(kind_of("<hr>\n"), Some(HtmlBlockKind::Rule));
    assert_eq!(kind_of("<hr />"), Some(HtmlBlockKind::Rule));
    assert_eq!(kind_of("<h6>x</h6>"), Some(HtmlBlockKind::Heading(6)));

    // Content is not Markdown: `**` stays source and nothing inside is a tag.
    let raw = recognize_html_block_line("<p>**raw**</p>").expect("paragraph");
    assert_eq!(raw.content, 3..10);
    assert!(raw.inline.is_empty());

    // An unclosed or invalid inline tag is literal, the block still applies.
    assert!(recognize_html_block_line("<p>a <b>b</p>").is_some_and(|r| r.inline.is_empty()));
    assert!(
        recognize_html_block_line("<p><a href=\"javascript:x\">l</a></p>")
            .is_some_and(|r| r.inline.is_empty())
    );
    let link = recognize_html_block_line("<p><a href=\"https://example.com/\">l</a></p>")
        .expect("link paragraph");
    assert_eq!(link.inline.len(), 1);
    assert_eq!(
        link.inline[0].destination.as_deref(),
        Some("https://example.com/")
    );

    for source in [
        "<p>a</p><p>b</p>",
        "<p>a</p> tail",
        "tail <p>a</p>",
        "<p></p>",
        "<p>  </p>",
        "<p>a",
        "a</p>",
        "<div>a</div>",
        "<p/>",
        "<h7>x</h7>",
        "<h1>x</h2>",
        "<hr>x",
        "<p>a<h1>b</h1></p>",
        "",
        "   ",
    ] {
        assert_eq!(kind_of(source), None, "{source:?}");
    }
}

#[test]
fn html_block_line_validates_tags_strictly() {
    for source in ["<hr>", "<hr/>", "<hr />"] {
        assert_eq!(kind_of(source), Some(HtmlBlockKind::Rule), "{source:?}");
    }
    for source in ["<hr /foo>", "<hr/foo>", "<p>x</ p>", "<p>x< /p>"] {
        assert_eq!(kind_of(source), None, "{source:?}");
    }
    assert_eq!(kind_of("<p>x</p >"), Some(HtmlBlockKind::Paragraph));

    // A comment is one literal region: tags inside it are never recognized.
    let commented =
        recognize_html_block_line("<p><!-- <a href=\"https://example.com/\">x</a> --></p>")
            .expect("paragraph");
    assert!(commented.inline.is_empty());
    let mixed = recognize_html_block_line(
        "<p><!-- <b>y</b> --><a href=\"https://example.com/\">x</a></p>",
    )
    .expect("paragraph");
    assert_eq!(mixed.inline.len(), 1);
    assert_eq!(mixed.inline[0].kind, HtmlKind::Link);
}

type Shape =Vec<(NodeKind, SourceRange, usize, usize)>;

fn shape(index: &BlockIndex) -> Shape {
    index
        .blocks()
        .map(|block| {
            (
                block.kind,
                block.source_range,
                block.line_count,
                block.leading_content_lines,
            )
        })
        .collect()
}

fn replace(buffer: &mut RopeBuffer, index: &mut BlockIndex, needle: &str, replacement: &str) {
    let text = buffer.full_text();
    let start = text.find(needle).unwrap_or_else(|| panic!("{needle:?} in {text:?}"));
    let base = buffer.revision();
    buffer
        .edit(SourceRange::new(start, start + needle.len()), replacement)
        .unwrap();
    let deltas = buffer.deltas_since(base).unwrap();
    index.update(buffer, &deltas);
    assert_eq!(
        shape(index),
        shape(&BlockIndex::from_buffer(buffer)),
        "after replacing {needle:?} with {replacement:?}"
    );
}

#[test]
fn html_block_index_update_matches_full_parse() {
    let mut buffer = RopeBuffer::from_text("<h1>T</h1>\n\n<p>a</p>\n\n<hr>\n\nbody\n");
    let mut index = BlockIndex::from_buffer(&buffer);
    let kinds = index.blocks().map(|block| block.kind).collect::<Vec<_>>();
    assert_eq!(
        kinds,
        [
            NodeKind::HtmlBlockElement(HtmlBlockKind::Heading(1)),
            NodeKind::HtmlBlockElement(HtmlBlockKind::Paragraph),
            NodeKind::HtmlBlockElement(HtmlBlockKind::Rule),
            NodeKind::Paragraph,
        ]
    );
    // The index keeps counting physical lines, blank lines included.
    assert_eq!(index.block(0).unwrap().line_count, 2);

    // Inline tags and text inside the element.
    replace(&mut buffer, &mut index, "T</h1>", "T <b>x</b></h1>");
    // Tag added and removed: the element becomes ordinary HTML and back.
    replace(&mut buffer, &mut index, "</p>", "");
    assert_eq!(index.block(1).unwrap().kind, NodeKind::HtmlBlock);
    replace(&mut buffer, &mut index, "<p>a", "<p>a</p>");
    assert_eq!(
        index.block(1).unwrap().kind,
        NodeKind::HtmlBlockElement(HtmlBlockKind::Paragraph)
    );
    // A second content line makes the block multi-line.
    replace(&mut buffer, &mut index, "<p>a</p>", "<p>a\nb</p>");
    assert_eq!(index.block(1).unwrap().kind, NodeKind::HtmlBlock);
    replace(&mut buffer, &mut index, "<p>a\nb</p>", "<p>a b</p>");
    // Blank-line changes around the element.
    replace(&mut buffer, &mut index, "<p>a b</p>\n\n", "<p>a b</p>\n\n\n");
    replace(&mut buffer, &mut index, "<hr>\n\n", "<hr>\n");
    // An edit at the document start shifts every offset.
    replace(&mut buffer, &mut index, "<h1>", "intro\n\n<h1>");
    // Other tags and text after the element.
    replace(&mut buffer, &mut index, "<hr>", "<hr> tail");
    replace(&mut buffer, &mut index, "<hr> tail", "<hr/>");
}
