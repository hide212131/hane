//! CommonMark 0.31.2 §4.2 examples 62–79, reused rather than transcribed.
//! Fixture source: https://spec.commonmark.org/0.31.2/spec.json
//! Copyright John MacFarlane, CC BY-SA 4.0:
//! https://creativecommons.org/licenses/by-sa/4.0/
use hane_document::{Revision, SourceRange};
use hane_markdown::{NodeKind, parse_document};
use pulldown_cmark::{Parser, html};

#[test]
fn commonmark_0312_atx_examples_match_html_and_hane_tree() {
    let fixtures: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/atx-commonmark.json")).unwrap();
    let fixtures = fixtures.as_array().unwrap();
    assert_eq!(fixtures.len(), 18);
    for fixture in fixtures {
        let source = fixture["markdown"].as_str().unwrap();
        let expected = fixture["html"].as_str().unwrap();
        let mut actual = String::new();
        html::push_html(&mut actual, Parser::new(source));
        assert_eq!(actual, expected, "example {}", fixture["example"]);
        let parsed = parse_document(
            Revision(1),
            SourceRange::new(100, 100 + source.len()),
            source,
        );
        let headings = parsed
            .tree
            .blocks()
            .filter_map(|(_, node)| match node.kind {
                NodeKind::Heading(level) => Some(level),
                _ => None,
            })
            .collect::<Vec<_>>();
        let expected_levels = expected
            .as_bytes()
            .windows(4)
            .filter_map(|w| {
                (w[0] == b'<' && w[1] == b'h' && (b'1'..=b'6').contains(&w[2]) && w[3] == b'>')
                    .then_some(w[2].wrapping_sub(b'0'))
            })
            .collect::<Vec<_>>();
        assert_eq!(
            headings, expected_levels,
            "Hane tree example {}",
            fixture["example"]
        );
        for (_, node) in parsed.tree.iter() {
            assert!(node.source_range.start.0 >= 100);
            assert!(node.source_range.end.0 <= 100 + source.len());
        }
    }
}

#[test]
fn atx_tab_closers_use_parser_semantics() {
    for (source, expected) in [
        ("#\t羽\t###\t\n", "<h1>羽</h1>\n"),
        ("## 羽 ###\t\n", "<h2>羽</h2>\n"),
        ("###\t###\t\n", "<h3></h3>\n"),
        ("# 羽\t\n", "<h1>羽</h1>\n"),
        ("# 羽###\t\n", "<h1>羽###</h1>\n"),
        ("# 羽 \\###\t\n", "<h1>羽 ###</h1>\n"),
        (
            "> # 羽\t###\n",
            "<blockquote>\n<h1>羽</h1>\n</blockquote>\n",
        ),
        ("- # 羽\t###\n", "<ul>\n<li>\n<h1>羽</h1>\n</li>\n</ul>\n"),
        ("# `a\tb`\t###\n", "<h1><code>a\tb</code></h1>\n"),
    ] {
        let mut actual = String::new();
        html::push_html(&mut actual, Parser::new(source));
        assert_eq!(actual, expected, "{source:?}");
    }
}

#[test]
fn setext_content_beginning_with_hashes_is_not_an_atx_marker() {
    for source in ["#foo\n====\n", "####### title\n----\n"] {
        let parsed = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        assert!(
            parsed
                .tree
                .blocks()
                .any(|(_, node)| matches!(node.kind, NodeKind::Heading(_)))
        );
        assert!(parsed.markers.is_empty());
    }
}
