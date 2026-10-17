use hane_document::{Revision, SourceRange};
use hane_markdown::{LinkParse, NodeKind, parse_document};

fn links(source: &str) -> Vec<LinkParse> {
    parse_document(Revision(1), SourceRange::new(0, source.len()), source).links
}

fn text<'a>(source: &'a str, link: &LinkParse) -> &'a str {
    &source[link.text_range.start.0..link.text_range.end.0]
}

#[test]
fn inline_link_keeps_destination_and_text_range() {
    let source = "see [the docs](https://example.com/a?b=1#c) now";
    let found = links(source);
    assert_eq!(found.len(), 1);
    assert_eq!(&*found[0].destination, "https://example.com/a?b=1#c");
    assert_eq!(text(source, &found[0]), "the docs");
}

#[test]
fn reference_collapsed_and_shortcut_links_resolve_their_definition() {
    let source = "[one][a] [b][] [c]\n\n[a]: http://one.example/\n[b]: https://two.example/\n[c]: HTTPS://three.example/x\n";
    let found = links(source);
    let destinations = found
        .iter()
        .map(|link| &*link.destination)
        .collect::<Vec<_>>();
    assert_eq!(
        destinations,
        [
            "http://one.example/",
            "https://two.example/",
            "HTTPS://three.example/x"
        ]
    );
    assert_eq!(text(source, &found[0]), "one");
}

#[test]
fn soft_wrapped_link_text_spans_both_lines_with_one_destination() {
    let source = "[first\nsecond](https://example.com/)";
    let found = links(source);
    assert_eq!(found.len(), 1);
    assert_eq!(text(source, &found[0]), "first\nsecond");
    assert_eq!(&*found[0].destination, "https://example.com/");
}

#[test]
fn only_absolute_http_destinations_are_recorded() {
    for source in [
        "[a](/relative/path)",
        "[a](relative.md)",
        "[a](file:///etc/passwd)",
        "[a](mailto:me@example.com)",
        "[a](javascript:alert(1))",
        "[a](ftp://example.com/)",
        "[a](https://)",
        "[a](//example.com/)",
        "![img](https://example.com/a.png)",
        "<https://example.com/>",
        "[a][missing]",
    ] {
        assert!(links(source).is_empty(), "{source} must not be openable");
    }
}

#[test]
fn image_inside_link_text_still_records_the_link_only() {
    let source = "[![img](https://example.com/a.png)](https://example.com/page)";
    let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
    assert_eq!(parsed.links.len(), 1);
    assert_eq!(&*parsed.links[0].destination, "https://example.com/page");
    let node = parsed.tree.node(parsed.links[0].node).expect("link node");
    assert_eq!(node.kind, NodeKind::Link);
}
