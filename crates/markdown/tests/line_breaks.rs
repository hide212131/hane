use hane_document::{Revision, SourceRange};
use hane_markdown::{NodeKind, parse_document};

#[test]
fn hard_break_syntax_is_derived_without_consuming_the_line_ending() {
    for newline in ["\n", "\r\n", "\r"] {
        let source = format!("日本  {newline}emoji🙂\\{newline}終");
        let base = 37;
        let parsed = parse_document(
            Revision(1),
            SourceRange::new(base, base + source.len()),
            &source,
        );

        let marker_text = parsed
            .markers
            .iter()
            .map(|marker| {
                &source[marker.start.0 - base..marker.end.0 - base]
            })
            .collect::<Vec<_>>();

        assert_eq!(marker_text, ["  ", "\\"], "newline={newline:?}");
        assert!(parsed.markers.iter().all(|marker| {
            source.is_char_boundary(marker.start.0 - base)
                && source.is_char_boundary(marker.end.0 - base)
        }));
    }
}

/// A blank line is a block boundary regardless of hard/soft breaks, while an
/// ordinary (non-blank-separated) line ending inside one paragraph is a soft
/// break, not a hard break — the two must stay distinguishable in the tree.
#[test]
fn blank_lines_split_paragraphs_and_ordinary_line_endings_stay_soft_breaks() {
    let separated = parse_document(Revision(1), SourceRange::new(0, 13), "first\n\nsecond");
    assert_eq!(
        separated
            .tree
            .blocks()
            .filter(|(_, node)| node.kind == NodeKind::Paragraph)
            .count(),
        2,
        "a blank line splits into two paragraphs"
    );

    let source = "first\nsecond";
    let joined = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
    assert_eq!(
        joined
            .tree
            .blocks()
            .filter(|(_, node)| node.kind == NodeKind::Paragraph)
            .count(),
        1,
        "an ordinary line ending stays inside one paragraph"
    );
    assert!(
        joined
            .tree
            .iter()
            .any(|(_, node)| node.kind == NodeKind::SoftBreak)
    );
    assert!(
        !joined
            .tree
            .iter()
            .any(|(_, node)| node.kind == NodeKind::HardBreak),
        "an ordinary line ending is never a hard break"
    );
    assert!(joined.markers.is_empty(), "a soft break has no markup to hide");
}

#[test]
fn hard_break_needs_two_or_more_trailing_spaces_or_a_backslash() {
    for (source, expect_hard) in [
        ("a \nb", false),
        ("a  \nb", true),
        ("a     \nb", true),
        ("a\\\nb", true),
    ] {
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        let has_hard = parsed
            .tree
            .iter()
            .any(|(_, node)| node.kind == NodeKind::HardBreak);
        assert_eq!(has_hard, expect_hard, "source: {source:?}");
    }
}

/// A hard break's syntax is only ever derived from an `Event::HardBreak` the
/// parser itself emitted, so it can never appear where CommonMark forbids a
/// break: inside a code span, inside an HTML tag's own markup, or at the very
/// end of a block with no following line to break to.
#[test]
fn hard_break_never_forms_inside_code_spans_html_tags_or_at_block_end() {
    for source in [
        "`code  \nspan`",
        "<a  \nhref=\"x\">hi</a>",
        "no following line\\",
        "no following line  ",
    ] {
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        assert!(
            !parsed
                .tree
                .iter()
                .any(|(_, node)| node.kind == NodeKind::HardBreak),
            "source: {source:?}"
        );
    }
}

/// CommonMark drops the leading indentation of the physical line a break
/// continues onto (regardless of break kind), and the single trailing
/// space/tab a soft break folds away, so neither should remain as visible
/// whitespace in rendered presentation. Both stay real source bytes —
/// addressable and disclosable — via `line_break_padding`, not `markers`,
/// since a soft break itself carries no markup (see `NodeKind::SoftBreak`).
#[test]
fn line_break_padding_hides_insignificant_spaces_around_breaks_only() {
    for (source, expected) in [
        ("foo  \n     bar", vec![(6, 11)]),
        ("foo\\\n     bar", vec![(5, 10)]),
        ("foo \n baz", vec![(3, 4), (5, 6)]),
        // No padding to hide when neither side has insignificant whitespace.
        ("foo\nbar", vec![]),
    ] {
        let base = 37;
        let parsed = parse_document(
            Revision(1),
            SourceRange::new(base, base + source.len()),
            source,
        );
        let mut actual = parsed.line_break_padding.clone();
        actual.sort_by_key(|range| range.start);
        let expected: Vec<_> = expected
            .into_iter()
            .map(|(start, end)| SourceRange::new(base + start, base + end))
            .collect();
        assert_eq!(actual, expected, "source: {source:?}");
        for padding in &parsed.line_break_padding {
            assert!(
                parsed
                    .markers
                    .iter()
                    .all(|marker| !padding.intersects(*marker)),
                "line-break padding must not overlap derived markers: {padding:?}"
            );
        }
    }
}

/// A lazy continuation line omits some (or all) ancestor container prefixes,
/// but pulldown-cmark still folds it into the same paragraph as a SoftBreak,
/// so the leading whitespace it drops from semantic content is exactly as
/// insignificant as an ordinary continuation's — even though `markers` has
/// nothing to hide there (`quote_markers_follow_list_ancestors_without_hiding_literal_greater_than`
/// covers the marker side of the same lazy lines).
#[test]
fn line_break_padding_hides_insignificant_spaces_on_lazy_continuation_lines() {
    for (source, expected) in [
        // Quote lazy continuation: line 2 has no `>` at all.
        ("> foo\n  bar", vec![(6, 8)]),
        // List lazy continuation: line 2 is indented less than the marker
        // requires ("- " is 2 columns), so it lazily continues the paragraph.
        ("- foo\n bar", vec![(6, 7)]),
        ("- foo\nbar", vec![]),
        // Nested quotes: line 2 keeps only the outer `>`, line 3 keeps none.
        ("> > foo\n> bar\nbaz", vec![]),
    ] {
        let base = 37;
        let parsed = parse_document(
            Revision(1),
            SourceRange::new(base, base + source.len()),
            source,
        );
        let mut actual = parsed.line_break_padding.clone();
        actual.sort_by_key(|range| range.start);
        let expected: Vec<_> = expected
            .into_iter()
            .map(|(start, end)| SourceRange::new(base + start, base + end))
            .collect();
        assert_eq!(actual, expected, "source: {source:?}");
        for padding in &parsed.line_break_padding {
            assert!(
                parsed
                    .markers
                    .iter()
                    .all(|marker| !padding.intersects(*marker)),
                "line-break padding must not overlap derived markers: {padding:?}"
            );
        }
    }
}

/// CommonMark resolves emphasis across a hard break the same way it does
/// across a soft break: the delimiter pair spans both physical lines.
#[test]
fn hard_break_can_sit_inside_an_emphasis_span() {
    let source = "**bold  \nacross**";
    let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
    assert!(
        parsed
            .tree
            .iter()
            .any(|(_, node)| node.kind == NodeKind::Strong)
    );
    let marker_text = parsed
        .markers
        .iter()
        .map(|marker| &source[marker.start.0..marker.end.0])
        .collect::<Vec<_>>();
    assert_eq!(marker_text, ["**", "  ", "**"]);
}
