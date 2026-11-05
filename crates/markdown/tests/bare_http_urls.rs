use hane_document::{Revision, RopeBuffer, SourceRange, TextBuffer};
use hane_markdown::{
    BlockIndex, LinkParse, MarkdownParse, NodeKind, links_intersecting, parse_document,
};

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
        (
            "<span>https://example.com</span> https://outside.example",
            &[],
        ),
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
fn scan_resumes_after_the_trimmed_candidate() {
    let cases: &[(&str, &[&str])] = &[
        (
            "https://a.example),https://b.example",
            &["https://a.example", "https://b.example"],
        ),
        ("me@https://a.example),https://b.example", &["https://b.example"]),
        (
            "日本語 https://a.example),https://b.example/path?q=1&x=2#end。",
            &["https://a.example", "https://b.example/path?q=1&x=2#end"],
        ),
        (
            "https://a.example),https://b.example),https://c.example",
            &["https://a.example", "https://b.example", "https://c.example"],
        ),
        (
            "me@https://a.example),HTTPS://b.example/path",
            &["HTTPS://b.example/path"],
        ),
        (
            "me@https://a.example plain https://b.example",
            &["https://b.example"],
        ),
        (
            "https://a.example/(ok) https://b.example",
            &["https://a.example/(ok)", "https://b.example"],
        ),
    ];
    for (source, expected) in cases {
        for offset in [0, 1_000] {
            let parsed = parse_document(
                Revision(1),
                SourceRange::new(offset, offset + source.len()),
                source,
            );
            let destinations: Vec<&str> = parsed
                .links
                .iter()
                .map(|link| &*link.destination)
                .collect();
            assert_eq!(&destinations, expected, "{source:?} @{offset}");
            let mut previous_end = 0;
            for link in &parsed.links {
                let start = link.text_range.start.0 - offset;
                let end = link.text_range.end.0 - offset;
                assert_eq!(&source[start..end], &*link.destination, "{source:?}");
                assert!(!link.uses_reference);
                let node = parsed.tree.node(link.node).expect("owner node");
                assert_eq!(node.kind, NodeKind::Text);
                assert!(previous_end <= start, "{source:?}");
                previous_end = end;
            }
        }
    }
}

#[test]
fn repeated_unmatched_closers_keep_every_url_and_range() {
    let cases: &[(&str, &str)] = &[
        ("https://a)", "https://a"),
        ("https://a]", "https://a"),
        ("https://a}", "https://a"),
        ("https://a/(b))", "https://a/(b)"),
    ];
    for (unit, expected) in cases {
        let count = 500;
        let source = unit.repeat(count);
        let parsed = parse(&source);
        assert_eq!(parsed.links.len(), count, "{unit:?}");
        for (index, link) in parsed.links.iter().enumerate() {
            let start = index * unit.len();
            assert_eq!(link.text_range.start.0, start, "{unit:?} #{index}");
            assert_eq!(link.text_range.end.0, start + expected.len(), "{unit:?} #{index}");
            assert_eq!(&*link.destination, *expected);
        }
    }

    // A blocked prefix still skips only its own candidate, then resumes.
    let source = format!("me@{}", "https://a)".repeat(100));
    let parsed = parse(&source);
    assert_eq!(parsed.links.len(), 99);
    assert_eq!(parsed.links[0].text_range.start.0, 3 + "https://a)".len());
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

fn structure(index: &BlockIndex) -> Vec<(NodeKind, SourceRange)> {
    index
        .blocks()
        .map(|block| (block.kind, block.source_range))
        .collect()
}

/// Links the given index's formal projections hand to presentation, in document
/// order. A projection the index withholds contributes nothing.
fn projected_links(index: &BlockIndex) -> Vec<(SourceRange, String)> {
    let mut found = Vec::new();
    for block in index.blocks() {
        if let Some(projection) = index.list_projection(&block) {
            found.extend(
                projection
                    .links_in(block.source_range)
                    .map(|link| (link.text_range, link.destination.to_string())),
            );
        }
    }
    found
}

fn edit_and_update(
    buffer: &mut RopeBuffer,
    index: &mut BlockIndex,
    range: SourceRange,
    text: &str,
) {
    let base = buffer.revision();
    buffer.edit(range, text).expect("edit");
    let deltas = buffer.deltas_since(base).expect("deltas");
    let _update = index.update(buffer, &deltas);
}

/// `reference` is the destination the fixture's existing `[doc][r]` link must
/// resolve to; every other link must be a bare URL owned by a `Text` node.
fn assert_incremental_matches_full(
    buffer: &RopeBuffer,
    index: &BlockIndex,
    expected: &[&str],
    reference: &str,
    label: &str,
) {
    let source = buffer.full_text();
    let full = BlockIndex::from_buffer(buffer);
    assert_eq!(structure(index), structure(&full), "{label}: block tiling");
    let parsed = parse(&source);
    let destinations: Vec<_> = parsed
        .links
        .iter()
        .map(|link| link.destination.to_string())
        .collect();
    assert_eq!(destinations, expected, "{label}: destinations");
    let from_parse: Vec<_> = parsed
        .links
        .iter()
        .map(|link| (link.text_range, link.destination.to_string()))
        .collect();
    assert_eq!(projected_links(&full), from_parse, "{label}: full index links");
    // `update` withholds projections until a formal rebuild, so the incremental
    // index may expose none; whatever it does expose must be the full result.
    let incremental = projected_links(index);
    assert!(
        incremental.is_empty() || incremental == from_parse,
        "{label}: incremental index links {incremental:?} != {from_parse:?}"
    );
    for pair in parsed.links.windows(2) {
        assert!(
            pair[0].text_range.end <= pair[1].text_range.start,
            "{label}: links overlap"
        );
    }
    let mut references = 0;
    for link in &parsed.links {
        let owner = parsed.tree.node(link.node).expect("owner node");
        if owner.kind == NodeKind::Link {
            // Existing Markdown link: the range covers its label, not the URL.
            references += 1;
            assert_eq!(&*link.destination, reference, "{label}: reference");
            assert!(link.uses_reference, "{label}: uses_reference");
            assert_eq!(slice(&source, link), "doc", "{label}: reference label");
        } else {
            // Bare URL: the Text owner's source is the destination verbatim.
            assert_eq!(owner.kind, NodeKind::Text, "{label}: bare owner");
            assert!(!link.uses_reference, "{label}: bare uses_reference");
            assert_eq!(slice(&source, link), &*link.destination, "{label}");
            assert!(owner.source_range.start <= link.text_range.start);
            assert!(link.text_range.end <= owner.source_range.end);
        }
        let end = link.text_range.end.0;
        let last_glyph = SourceRange::new(end - 1, end);
        assert!(
            links_intersecting(&parsed.links, last_glyph)
                .iter()
                .any(|found| found.text_range == link.text_range),
            "{label}: the final byte still belongs to the link"
        );
        let after = SourceRange::new(end, end + 1);
        assert!(
            !links_intersecting(&parsed.links, after)
                .iter()
                .any(|found| found.text_range == link.text_range),
            "{label}: the half-open end must not reach the next byte"
        );
    }
    assert_eq!(references, 1, "{label}: the existing reference link");
}

#[test]
fn incremental_updates_follow_the_url_through_typing_deleting_and_retyping() {
    let source = "# 見出し\n\n参照 https://\n\nsee [doc][r] and https://ex.example/p.\n\n[r]: https://ref.example/a\n";
    let mut buffer = RopeBuffer::from_text(source);
    let mut index = BlockIndex::from_buffer(&buffer);
    let without_typed = ["https://ref.example/a", "https://ex.example/p"];
    let with_typed = [
        "https://example.com/a",
        "https://ref.example/a",
        "https://ex.example/p",
    ];
    let reference = "https://ref.example/a";
    assert_incremental_matches_full(
        &buffer,
        &index,
        &without_typed,
        reference,
        "unfinished https://",
    );

    let tail = "example.com/a";
    let at = source.find("https://\n").expect("unfinished url") + "https://".len();
    edit_and_update(&mut buffer, &mut index, SourceRange::empty(at), tail);
    assert_incremental_matches_full(&buffer, &index, &with_typed, reference, "completed url");

    edit_and_update(
        &mut buffer,
        &mut index,
        SourceRange::new(at, at + tail.len()),
        "",
    );
    assert_incremental_matches_full(
        &buffer,
        &index,
        &without_typed,
        reference,
        "destination removed",
    );

    edit_and_update(&mut buffer, &mut index, SourceRange::empty(at), tail);
    assert_incremental_matches_full(&buffer, &index, &with_typed, reference, "retyped");

    // A multi-byte prefix shifts every later offset by a non-ASCII byte count.
    edit_and_update(&mut buffer, &mut index, SourceRange::empty(0), "🙂あ");
    assert_incremental_matches_full(&buffer, &index, &with_typed, reference, "UTF-8 front edit");
    let shifted = buffer.full_text();
    let parsed = parse(&shifted);
    let first = &parsed.links[0];
    assert_eq!(
        first.text_range.start.0,
        shifted.find("https://example.com/a").expect("typed url")
    );

    // The definition changes while the bare URLs around it keep their ranges.
    let definition = shifted.find("ref.example").expect("definition");
    edit_and_update(
        &mut buffer,
        &mut index,
        SourceRange::new(definition, definition + "ref.example".len()),
        "ref2.example",
    );
    assert_incremental_matches_full(
        &buffer,
        &index,
        &[
            "https://example.com/a",
            "https://ref2.example/a",
            "https://ex.example/p",
        ],
        "https://ref2.example/a",
        "reference definition updated",
    );
}

#[test]
fn bare_urls_merge_with_markdown_links_in_source_order() {
    let source = "https://a.example [b](https://b.example) https://c.example";
    let parsed = parse(source);
    let destinations: Vec<_> = parsed
        .links
        .iter()
        .map(|l| l.destination.to_string())
        .collect();
    assert_eq!(
        destinations,
        [
            "https://a.example",
            "https://b.example",
            "https://c.example"
        ]
    );
    let node = parsed.tree.node(parsed.links[1].node).expect("link node");
    assert_eq!(node.kind, NodeKind::Link);
    for pair in parsed.links.windows(2) {
        assert!(pair[0].text_range.end <= pair[1].text_range.start);
    }
}
