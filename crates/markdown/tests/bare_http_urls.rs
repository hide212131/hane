use hane_document::{Revision, SourceRange};
use hane_markdown::{LinkParse, MarkdownParse, NodeKind, parse_document};

fn parse(source: &str) -> MarkdownParse {
    parse_document(Revision(1), SourceRange::new(0, source.len()), source)
}

fn bare(source: &str) -> Vec<String> {
    parse(source)
        .links
        .iter()
        .map(|link| link.destination.to_string())
        .collect()
}

fn slice<'a>(source: &'a str, link: &LinkParse) -> &'a str {
    &source[link.text_range.start.0..link.text_range.end.0]
}

#[test]
fn boundary_table_keeps_the_expected_destination() {
    let cases: &[(&str, &[&str])] = &[
        ("https://example.com", &["https://example.com"]),
        ("http://example.com", &["http://example.com"]),
        (
            "HTTPS://Example.com/A%2f?q=1&lang=ja#Top",
            &["HTTPS://Example.com/A%2f?q=1&lang=ja#Top"],
        ),
        (
            "https://example.com/a?q=1&b=two#part.",
            &["https://example.com/a?q=1&b=two#part"],
        ),
        (
            "(https://example.com/a_(b)).",
            &["https://example.com/a_(b)"],
        ),
        (
            "https://example.com/a_(b)))",
            &["https://example.com/a_(b)"],
        ),
        ("https://example.com/a(", &["https://example.com/a"]),
        (
            "https://example.com/a[1]{two}",
            &["https://example.com/a[1]{two}"],
        ),
        ("http://[::1]:8080/a", &["http://[::1]:8080/a"]),
        ("https://example.com/a%29", &["https://example.com/a%29"]),
        ("https://example.com/a?!", &["https://example.com/a"]),
        (
            "参照：https://example.com/資料?q=日本語#節。次",
            &["https://example.com/資料?q=日本語#節"],
        ),
        (
            "「https://example.com/a」、https://example.org/b",
            &["https://example.com/a", "https://example.org/b"],
        ),
        (
            "🙂 https://a.example/ https://b.example/終",
            &["https://a.example/", "https://b.example/終"],
        ),
        ("https://example.com/a next", &["https://example.com/a"]),
        ("https://example.com/a\tnext", &["https://example.com/a"]),
        ("https://example.com/a\r\nnext", &["https://example.com/a"]),
        ("xhttps://example.com", &[]),
        ("abchttps://example.com", &[]),
        ("me@https://example.com", &[]),
        ("https://", &[]),
        ("www.example.com", &[]),
        ("user@example.com", &[]),
        ("file:///a", &[]),
        ("ftp://example.com", &[]),
        ("relative.md", &[]),
        ("*https://example.com*", &["https://example.com"]),
        ("https://example.com/a*b*c", &["https://example.com/a"]),
        ("https://example.com/a_b_c", &["https://example.com/a_b_c"]),
        (
            "https://example.com/?a=1&amp;b=2",
            &["https://example.com/?a=1&amp;b=2"],
        ),
        ("`https://example.com`", &[]),
        ("```\nhttps://example.com\n```\n", &[]),
        ("    https://example.com\n", &[]),
        (
            "[https://label.example](https://dest.example)",
            &["https://dest.example"],
        ),
        ("![https://alt.example](https://img.example)", &[]),
        ("<https://example.com>", &[]),
        ("<span>https://example.com</span> https://outside.example", &[]),
    ];
    for (source, expected) in cases {
        assert_eq!(&bare(source), expected, "{source:?}");
    }
}

#[test]
fn html_block_exclusion_does_not_leak_into_the_next_paragraph() {
    let source = "<span>https://a.example</span> https://b.example\n\nhttps://c.example\n";
    assert_eq!(bare(source), ["https://c.example"]);
}

#[test]
fn ranges_are_absolute_utf8_and_match_the_destination() {
    let source = "🙂 https://a.example/ https://b.example/終 end";
    let parsed = parse(source);
    assert_eq!(parsed.links.len(), 2);
    for link in &parsed.links {
        assert_eq!(slice(source, link), &*link.destination);
        assert!(!link.uses_reference);
        let node = parsed.tree.node(link.node).expect("owner node");
        assert_eq!(node.kind, NodeKind::Text);
        assert!(node.source_range.start <= link.text_range.start);
        assert!(link.text_range.start < node.source_range.end);
    }
    assert!(parsed.links[0].text_range.end <= parsed.links[1].text_range.start);
}

#[test]
fn local_slice_offsets_stay_document_absolute() {
    let slice_source = "see https://example.com/x now";
    let offset = 1_000;
    let parsed = parse_document(
        Revision(1),
        SourceRange::new(offset, offset + slice_source.len()),
        slice_source,
    );
    assert_eq!(parsed.links.len(), 1);
    assert_eq!(
        parsed.links[0].text_range,
        SourceRange::new(offset + 4, offset + 4 + "https://example.com/x".len())
    );
}

#[test]
fn bare_urls_merge_with_markdown_links_in_source_order() {
    let source = "https://a.example [b](https://b.example) https://c.example";
    let parsed = parse(source);
    let destinations: Vec<_> = parsed.links.iter().map(|l| l.destination.to_string()).collect();
    assert_eq!(
        destinations,
        ["https://a.example", "https://b.example", "https://c.example"]
    );
    let node = parsed.tree.node(parsed.links[1].node).expect("link node");
    assert_eq!(node.kind, NodeKind::Link);
    for pair in parsed.links.windows(2) {
        assert!(pair[0].text_range.end <= pair[1].text_range.start);
    }
}
