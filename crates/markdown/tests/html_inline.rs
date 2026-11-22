//! Issue #757 phase 1: allow-listed inline HTML (`b`/`strong`, `i`/`em`,
//! `s`/`del`/`strike`, `code`, `a`) recognized from pulldown-cmark's ranges.

use hane_document::{Revision, RopeBuffer, SourceRange, TextBuffer};
use hane_markdown::{
    BlockIndex, HtmlElement, HtmlKind, MarkdownParse, NodeKind, parse_document, parse_full_document,
};

fn parse(source: &str) -> MarkdownParse {
    parse_document(Revision(1), SourceRange::new(0, source.len()), source)
}

fn at(base: usize, source: &str, needle: &str) -> SourceRange {
    let start = source.find(needle).unwrap_or_else(|| panic!("{needle:?} in {source:?}"));
    SourceRange::new(base + start, base + start + needle.len())
}

fn kinds(parsed: &MarkdownParse) -> Vec<HtmlKind> {
    parsed.html_elements.iter().map(|element| element.kind).collect()
}

#[test]
fn html_pairs_keep_absolute_utf8_ranges() {
    let base = 31;
    let open_link = "<A HREF=\"https://example.com/?a=1&amp;b=2\" class=x>";
    let source = format!("日本 <b>太字</b> と {open_link}リンク</A>\n");
    let parsed = parse_document(
        Revision(1),
        SourceRange::new(base, base + source.len()),
        &source,
    );
    assert_eq!(kinds(&parsed), [HtmlKind::Bold, HtmlKind::Link]);

    let bold = &parsed.html_elements[0];
    assert_eq!(bold.open_tag, at(base, &source, "<b>"));
    assert_eq!(bold.close_tag, at(base, &source, "</b>"));
    assert_eq!(bold.content_range, at(base, &source, "太字"));
    assert_eq!(
        bold.range,
        SourceRange {
            start: bold.open_tag.start,
            end: bold.close_tag.end
        }
    );
    assert_eq!(bold.destination, None);

    let link = &parsed.html_elements[1];
    assert_eq!(link.open_tag, at(base, &source, open_link));
    assert_eq!(link.close_tag, at(base, &source, "</A>"));
    assert_eq!(link.content_range, at(base, &source, "リンク"));
    assert_eq!(
        link.destination.as_deref(),
        Some("https://example.com/?a=1&b=2")
    );
    assert_eq!(
        parsed.tree.node(link.open_node).map(|node| node.kind),
        Some(NodeKind::InlineHtml)
    );

    // Every tag is a marker, and the `<a>` is the one openable link.
    for tag in [bold.open_tag, bold.close_tag, link.open_tag, link.close_tag] {
        assert!(parsed.markers.contains(&tag), "{tag:?}");
    }
    assert!(parsed.markers.windows(2).all(|pair| pair[0].end <= pair[1].start));
    assert_eq!(parsed.links.len(), 1);
    assert_eq!(parsed.links[0].text_range, link.content_range);
    assert_eq!(parsed.links[0].node, link.open_node);
    assert_eq!(&*parsed.links[0].destination, "https://example.com/?a=1&b=2");
    assert!(!parsed.links[0].uses_reference);
    assert!(parsed.html_elements.iter().all(|element| element.range.start.0 >= base));
}

#[test]
fn html_pairs_accept_nesting_aliases_and_crlf() {
    let source = "<b>a <i>b</i> c</b>\r\nnext <code>x</code> <del>d</del> <EM>e</EM>\r\n";
    let parsed = parse(source);
    assert_eq!(
        kinds(&parsed),
        [
            HtmlKind::Bold,
            HtmlKind::Italic,
            HtmlKind::Code,
            HtmlKind::Strikethrough,
            HtmlKind::Italic,
        ]
    );
    let outer = &parsed.html_elements[0];
    let inner = &parsed.html_elements[1];
    assert!(outer.range.start <= inner.range.start && inner.range.end <= outer.range.end);
    assert_eq!(outer.content_range, at(0, source, "a <i>b</i> c"));
    assert!(parsed.html_elements.iter().all(|element| {
        let text = &source[element.range.start.0..element.range.end.0];
        !text.contains(['\r', '\n'])
    }));

    // Markdown decorations compose with the tags in either order.
    let source = "<b>**x** and *y*</b> and **<i>z</i>**\n";
    assert_eq!(kinds(&parse(source)), [HtmlKind::Bold, HtmlKind::Italic]);

    // A heading is a recognition unit too.
    let source = "# 見出し <strong>強調</strong>\n";
    assert_eq!(kinds(&parse(source)), [HtmlKind::Bold]);
}

#[test]
fn html_malformed_or_over_budget_stays_literal() {
    let long_attribute = format!("<b class=\"{}\">x</b>\n", "a".repeat(5_000));
    let long_unit = format!("<b>x</b> {}\n", "y".repeat(70_000));
    let too_deep = format!("{}x{}\n", "<b>".repeat(17), "</b>".repeat(17));
    let too_many = format!("{}\n", "<b>y</b>".repeat(129));
    for source in [
        // Missing, mismatched, crossing and stray tags.
        "<b>unclosed\n",
        "x</b>\n",
        "<b>x</strong>\n",
        "<b><i>x</b></i>\n",
        "<b/>x</b>\n",
        // A pair across physical lines.
        "<b>multi\nline</b>\n",
        // A pair across a Markdown construct boundary.
        "<b>**x</b>**\n",
        "[<b>x](https://example.com/)</b>\n",
        // Outside a top-level paragraph or heading.
        "- <b>x</b>\n",
        "> <b>x</b>\n",
        "| a |\n|---|\n| <b>x</b> |\n",
        // Escaped, code-span and unsupported tags.
        "\\<b>x\\</b>\n",
        "`<b>x</b>`\n",
        "<u>x</u> <span>y</span> <kbd>z</kbd> <sub>s</sub>\n",
        // Empty content.
        "<b></b>\n",
        // Over the per-tag, per-unit, depth and tag-count budgets.
        &long_attribute,
        &long_unit,
        &too_deep,
        &too_many,
    ] {
        let parsed = parse(source);
        assert!(
            parsed.html_elements.is_empty(),
            "{:?} must stay literal",
            &source[..source.len().min(40)]
        );
        // A Markdown link may still exist; no link comes from HTML.
        assert!(parsed.links.iter().all(|link| {
            parsed
                .tree
                .node(link.node)
                .is_some_and(|node| node.kind != NodeKind::InlineHtml)
        }));
    }

    // The budget is inclusive: 128 pairs are 256 tags.
    let at_limit = format!("{}\n", "<b>y</b>".repeat(128));
    assert_eq!(parse(&at_limit).html_elements.len(), 128);

    // One malformed element does not take its valid siblings with it.
    let parsed = parse("<b>a</b> <i>unclosed <u>x</u> <s>c</s>\n");
    assert_eq!(kinds(&parsed), [HtmlKind::Bold, HtmlKind::Strikethrough]);
}

#[test]
fn html_code_content_is_literal() {
    let source = "<code>**x** <b>y</b> [l](https://example.com/)</code> `z`\n";
    let parsed = parse(source);
    assert_eq!(kinds(&parsed), [HtmlKind::Code]);
    let code = &parsed.html_elements[0];
    // Only the two tags are hidden; nothing inside the content is markup or a link.
    assert_eq!(parsed.markers.iter().filter(|m| code.content_range.start <= m.start && m.end <= code.content_range.end).count(), 0);
    assert!(parsed.markers.contains(&code.open_tag));
    assert!(parsed.markers.contains(&code.close_tag));
    assert!(parsed.links.is_empty());
    assert!(parsed.html_literal_contains(at(0, source, "**x**")));
    assert!(!parsed.html_literal_contains(at(0, source, "`z`")));
}

#[test]
fn html_inside_code_is_excluded_across_markdown_scopes() {
    let parsed = parse("<code>**<a href=\"https://example.com/\">x</a>**</code>\n");
    assert_eq!(kinds(&parsed), [HtmlKind::Code]);
    // Tags inside a backtick code span are never candidates.
    let parsed = parse("`<b>x</b>` and `<a href=\"https://example.com/\">y</a>`\n");
    assert!(parsed.html_elements.is_empty());
    // Outside `<code>`, Markdown and HTML still compose.
    let parsed = parse("**<b>x</b>** <code>y</code>\n");
    assert_eq!(kinds(&parsed), [HtmlKind::Bold, HtmlKind::Code]);
}

#[test]
fn html_nested_link_with_invalid_inner_href_excludes_both() {
    let parsed = parse("<a href=\"https://outer.example/\"><a href=\"/relative\">x</a></a>\n");
    assert!(parsed.html_elements.is_empty());
    // A valid sibling link is still recognized.
    let parsed = parse(
        "<a href=\"https://a.example/\">a</a> <a href=\"https://b.example/\">b</a>\n",
    );
    assert_eq!(kinds(&parsed), [HtmlKind::Link, HtmlKind::Link]);
}

#[test]
fn html_href_only_accepts_valid_absolute_http_urls() {
    for (attrs, expected) in [
        ("href=\"https://example.com/a\"", Some("https://example.com/a")),
        ("href='http://example.com'", Some("http://example.com")),
        ("HREF=https://example.com/x", Some("https://example.com/x")),
        (
            "class=\"c\" href=\"https://example.com/?a&amp;b\" id=i",
            Some("https://example.com/?a&b"),
        ),
        // Decoded exactly once.
        ("href=\"&#104;ttps://example.com/\"", Some("https://example.com/")),
        ("href=\"&amp;#104;ttps://example.com/\"", None),
        ("href=\"/relative\"", None),
        ("href=\"page.html\"", None),
        ("href=\"//example.com/\"", None),
        ("href=\"javascript:alert(1)\"", None),
        ("href=\"java&#115;cript:alert(1)\"", None),
        ("href=\"data:text/html,x\"", None),
        ("href=\"file:///etc/passwd\"", None),
        ("href=\"mailto:a@example.com\"", None),
        ("href=\"https://example.com/a b\"", None),
        ("href=\"https://exa&#9;mple.com/\"", None),
        ("href=\"https://a.example/\" href=\"https://b.example/\"", None),
        ("href=\"\"", None),
        ("href", None),
        ("class=\"c\"", None),
        ("", None),
    ] {
        let source = format!("<a {attrs}>label</a>\n");
        let parsed = parse(&source);
        match expected {
            Some(destination) => {
                assert_eq!(parsed.html_elements.len(), 1, "{attrs:?}");
                assert_eq!(
                    parsed.html_elements[0].destination.as_deref(),
                    Some(destination),
                    "{attrs:?}"
                );
                assert_eq!(parsed.links.len(), 1, "{attrs:?}");
                assert_eq!(&*parsed.links[0].destination, destination, "{attrs:?}");
            }
            None => {
                assert!(parsed.html_elements.is_empty(), "{attrs:?}");
                assert!(parsed.links.is_empty(), "{attrs:?}");
            }
        }
    }

    // Links never nest with another link or an image, and need a label.
    for source in [
        "<a href=\"https://a.example/\">x [y](https://b.example/) z</a>\n",
        "<a href=\"https://a.example/\">x <https://b.example/> z</a>\n",
        "<a href=\"https://a.example/\"><a href=\"https://b.example/\">x</a></a>\n",
        "[<a href=\"https://a.example/\">x</a>](https://b.example/)\n",
        "<a href=\"https://a.example/\">![i](x.png)</a>\n",
        "<a href=\"https://a.example/\"></a>\n",
    ] {
        let parsed = parse(source);
        assert!(
            !parsed
                .html_elements
                .iter()
                .any(|element| element.kind == HtmlKind::Link),
            "{source:?}"
        );
        assert!(
            parsed
                .links
                .iter()
                .all(|link| parsed.tree.node(link.node).is_some_and(|node| node.kind == NodeKind::Link)),
            "{source:?}"
        );
    }

    // A bare URL in a valid label does not become a second link.
    let parsed = parse("<a href=\"https://a.example/\">https://b.example/</a>\n");
    assert_eq!(parsed.links.len(), 1);
    assert_eq!(&*parsed.links[0].destination, "https://a.example/");
}

fn html_of(elements: &[HtmlElement]) -> Vec<(HtmlKind, SourceRange)> {
    elements
        .iter()
        .map(|element| (element.kind, element.range))
        .collect()
}

fn index_html(buffer: &RopeBuffer, index: &BlockIndex) -> Vec<(HtmlKind, SourceRange)> {
    index
        .blocks()
        .flat_map(|block| {
            let text = buffer.text(block.source_range).expect("block text");
            html_of(&parse_document(buffer.revision(), block.source_range, &text).html_elements)
        })
        .collect()
}

fn structure(index: &BlockIndex) -> Vec<(NodeKind, std::ops::Range<usize>, usize)> {
    index
        .blocks()
        .map(|block| (block.kind, block.source_range.as_usize(), block.line_count))
        .collect()
}

#[test]
fn html_block_index_update_matches_full_parse() {
    let mut buffer = RopeBuffer::from_text(
        "日本 <b>one</b>\n\ntwo <a href=\"https://example.com/\">x</a>\n\nthree\nfour\n",
    );
    let mut index = BlockIndex::from_buffer(&buffer);
    for (needle, replacement) in [
        ("日本", "<em>日本</em>"),
        ("</b>", ""),
        ("\n\n", "\n"),
        ("three", "<code>three</code>"),
        ("x</a>", "x</strong>"),
        ("one", "<s>one</s> </b>"),
        ("four", "\n\n<i>four</i>"),
        ("<em>", ""),
    ] {
        let text = buffer.full_text();
        let start = text
            .find(needle)
            .unwrap_or_else(|| panic!("{needle:?} in {text:?}"));
        let base = buffer.revision();
        buffer
            .edit(SourceRange::new(start, start + needle.len()), replacement)
            .expect("edit");
        let deltas = buffer.deltas_since(base).expect("deltas");
        let update = index.update(&buffer, &deltas);
        if update.resynchronized && !index.has_provisional_blocks() {
            let full = BlockIndex::from_buffer(&buffer);
            assert_eq!(structure(&index), structure(&full), "after {needle:?}");
            let whole = parse_full_document(buffer.revision(), &buffer.full_text());
            assert_eq!(
                index_html(&buffer, &index),
                html_of(&whole.html_elements),
                "after {needle:?}"
            );
            assert_eq!(index_html(&buffer, &full), html_of(&whole.html_elements));
        }
    }
}
