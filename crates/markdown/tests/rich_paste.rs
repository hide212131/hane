//! Issue #596: external HTML → Markdown fixtures for rich Paste.

use hane_document::{Revision, SourceRange};
use hane_markdown::{
    HtmlImportError, HtmlLimits, NodeKind, html_to_markdown, html_to_markdown_with_limits,
    html_to_plain_text, parse_document,
};

fn md(html: &str) -> String {
    html_to_markdown(html).unwrap_or_else(|error| panic!("{html:?} failed: {error:?}"))
}

fn count_kind(markdown: &str, wanted: NodeKind) -> usize {
    parse_document(
        Revision(1),
        SourceRange::new(0, markdown.len()),
        markdown,
    )
    .tree
    .iter()
    .filter(|(_, node)| node.kind == wanted)
    .count()
}

/// The user's required condition: ordinary text and several links in one
/// paragraph keep every label, destination and the surrounding sentences.
#[test]
fn mixed_text_and_multiple_links_keep_labels_destinations_and_text() {
    let html = r#"<p>前 <a href="https://example.com/a_(b)?x=1&amp;y=2">日本語リンク</a> と <strong>太字</strong>、<a href="https://example.org/">別リンク</a>。後</p>"#;
    let markdown = md(html);
    assert_eq!(
        markdown,
        "前 [日本語リンク](https://example.com/a_(b)?x=1&y=2) と **太字**、[別リンク](https://example.org/)。後"
    );
    // The parser reads the result back as two links and one strong run.
    assert_eq!(count_kind(&markdown, NodeKind::Link), 2);
    assert_eq!(count_kind(&markdown, NodeKind::Strong), 1);
}

/// An implied paragraph close must not truncate the stack past a dropped
/// element, or the dropped subtree's text would leak into the output.
#[test]
fn paragraph_close_does_not_cross_a_dropped_subtree_boundary() {
    let markdown = md("<p>before<object><p>secret</p></object>after</p>");
    assert!(!markdown.contains("secret"), "{markdown:?}");
    assert!(markdown.contains("before"), "{markdown:?}");
    assert!(markdown.contains("after"), "{markdown:?}");
}

/// An end tag outside a dropped element must not close it, or the text that
/// follows would leak out of the hidden subtree.
#[test]
fn end_tag_does_not_cross_a_dropped_subtree_boundary() {
    let markdown = md("<div>before<template>x</div>secret</template>after</div>");
    assert!(markdown.contains("before"), "{markdown:?}");
    assert!(markdown.contains("after"), "{markdown:?}");
    assert!(!markdown.contains("secret"), "{markdown:?}");
}

#[test]
fn links_nest_with_and_sit_next_to_emphasis() {
    assert_eq!(
        md(r#"<p><a href="https://example.com/"><strong>太</strong>字</a></p>"#),
        "[**太**字](https://example.com/)"
    );
    assert_eq!(
        md(r#"<p><strong><a href="https://example.com/">太字</a></strong></p>"#),
        "**[太字](https://example.com/)**"
    );
    assert_eq!(
        md(r#"<p><strong>a</strong><a href="https://example.com/">b</a><em>c</em></p>"#),
        "**a**[b](https://example.com/)*c*"
    );
    assert_eq!(
        md(r#"<p>見て、<a href="https://example.com/">ここ</a>。次の<a href="https://example.org/x">リンク</a>（注）</p>"#),
        "見て、[ここ](https://example.com/)。次の[リンク](https://example.org/x)（注）"
    );
}

#[test]
fn unsafe_and_relative_hrefs_keep_only_the_label() {
    assert_eq!(md(r#"<a href="javascript:alert(1)">文字</a>"#), "文字");
    assert_eq!(
        md(r#"<p><a href="/rel">相対</a> <a href="https://ok.example/">安全</a> <a href="data:text/html,x">d</a> <a href="//host/x">e</a> <a href="mailto:a@b.c">f</a></p>"#),
        "相対 [安全](https://ok.example/) d e f"
    );
    // Control characters inside the href refuse the whole destination.
    assert_eq!(
        md("<p><a href=\"https://exa\nmple.com/\">x</a></p>"),
        "x"
    );
    assert_eq!(md(r#"<p><a>no href</a></p>"#), "no href");
}

#[test]
fn link_destinations_with_parentheses_and_queries_are_kept() {
    assert_eq!(
        md(r#"<a href="https://example.com/a_(b)?x=1&amp;y=2">x</a>"#),
        "[x](https://example.com/a_(b)?x=1&y=2)"
    );
    // An unbalanced parenthesis is percent-encoded instead of breaking the link.
    assert_eq!(
        md(r#"<a href="https://example.com/a_(b">x</a>"#),
        "[x](https://example.com/a_%28b)"
    );
    // A destination that reads as a character reference keeps its `&` literal.
    assert_eq!(
        md(r#"<a href="https://example.com/?a=&amp;amp;b">x</a>"#),
        "[x](https://example.com/?a=\\&amp;b)"
    );
}

#[test]
fn href_keeps_semicolonless_legacy_references_before_equals_or_alphanumerics() {
    assert_eq!(
        md(r#"<a href="https://x.example/?a=1&copy=2">x</a>"#),
        "[x](https://x.example/?a=1&copy=2)"
    );
    assert_eq!(
        md(r#"<a href="https://x.example/?regionId=5">x</a>"#),
        "[x](https://x.example/?regionId=5)"
    );
    // Unquoted attribute values take the same path.
    assert_eq!(
        md(r#"<a href=https://x.example/?a=1&copy=2>x</a>"#),
        "[x](https://x.example/?a=1&copy=2)"
    );
    assert_eq!(
        md(r#"<a href=https://x.example/?regionId=5>x</a>"#),
        "[x](https://x.example/?regionId=5)"
    );
    // Body text still decodes the semicolon-less legacy reference.
    assert_eq!(md("<p>a&copy=2</p>"), "a©=2");
}

#[test]
fn a_link_around_blocks_keeps_the_link_on_each_block() {
    assert_eq!(
        md(r#"<a href="https://example.com/"><div>カード</div></a>"#),
        "[カード](https://example.com/)"
    );
}

#[test]
fn golden_blocks_and_inline_marks() {
    assert_eq!(
        md(r#"<h2>羽</h2><p><strong>太字</strong>と<a href="https://example.com/">リンク</a></p>"#),
        "## 羽\n\n**太字**と[リンク](https://example.com/)"
    );
    assert_eq!(md("<p>a<br>b</p>"), "a\\\nb");
    assert_eq!(md("<p>a<br></p>"), "a");
    assert_eq!(md("<p>&lt;script&gt; *x*</p>"), "\\<script\\> \\*x\\*");
    assert_eq!(md("<h1>見出し <em>斜体</em></h1>"), "# 見出し *斜体*");
    assert_eq!(md("<p><del>消す</del> <s>x</s></p>"), "~~消す~~ ~~x~~");
    assert_eq!(md("<p><b>a </b>b</p>"), "**a** b");
    assert_eq!(md("<p><b> </b>x</p>"), "x");
    assert_eq!(md("<h2>C#</h2>"), "## C\\#");
    for level in 1..=6 {
        assert_eq!(
            md(&format!("<h{level}>見出し</h{level}>")),
            format!("{} 見出し", "#".repeat(level))
        );
    }
}

#[test]
fn text_is_unicode_safe_and_whitespace_collapses() {
    assert_eq!(md("<p>  a \n\t b  </p>"), "a b");
    assert_eq!(md("<p>a&nbsp;b</p>"), "a\u{a0}b");
    assert_eq!(md("<p>&#26085;&#x672c; &amp; 👨‍👩‍👧 é</p>"), "日本 & 👨‍👩‍👧 é");
    assert_eq!(md("<p>AT&T</p>"), "AT&T");
    assert_eq!(md("<p>snake_case _lead trail_</p>"), "snake_case \\_lead trail\\_");
    assert_eq!(md("<p>前<span style=\"color:red\">色</span>後</p>"), "前色後");
}

#[test]
fn emphasis_edges_with_multibyte_characters_do_not_panic() {
    assert_eq!(md("<p>前<strong>日本語</strong>後</p>"), "前**日本語**後");
    assert_eq!(md("<p>前<em>日本語</em>後</p>"), "前*日本語*後");
    assert_eq!(md("<p>前<strong>「日本」</strong>後</p>"), "前<strong>「日本」</strong>後");
    assert_eq!(md("<p><strong>日本語。</strong></p>"), "<strong>日本語。</strong>");
}

#[test]
fn line_starts_that_would_become_blocks_are_escaped() {
    assert_eq!(md("<p>1. not list</p>"), "1\\. not list");
    assert_eq!(md("<p># not heading</p>"), "\\# not heading");
    assert_eq!(md("<p>- not item</p>"), "\\- not item");
    assert_eq!(md("<p>+ not item</p>"), "\\+ not item");
    assert_eq!(md("<p>---</p>"), "\\---");
    assert_eq!(md("<p>2) x</p>"), "2\\) x");
    assert_eq!(md("<p>a<br>1. b</p>"), "a\\\n1\\. b");
}

/// Hyphens separated by spaces still form a CommonMark thematic break, which
/// would drop the pasted text on a Markdown round trip.
#[test]
fn spaced_hyphen_lines_do_not_become_thematic_breaks() {
    for text in ["-- -", "--- -"] {
        let markdown = md(&format!("<p>{text}</p>"));
        assert_eq!(markdown, format!("\\{text}"));
        assert_eq!(count_kind(&markdown, NodeKind::Rule), 0, "{markdown:?}");
    }
}

#[test]
fn inline_and_fenced_code() {
    assert_eq!(md("<p>使う <code>a`b</code> と</p>"), "使う ``a`b`` と");
    assert_eq!(md("<p><code>`x`</code></p>"), "`` `x` ``");
    assert_eq!(md("<p><code>*not* <b>bold</b></code></p>"), "`*not* bold`");
    assert_eq!(
        md("<pre><code class=\"language-rust\">fn main() {}\nlet x = `1`;</code></pre>"),
        "```rust\nfn main() {}\nlet x = `1`;\n```"
    );
    assert_eq!(
        md("<pre>a\n```\nb</pre>"),
        "````\na\n```\nb\n````"
    );
    // An arbitrary class is not an info string.
    assert_eq!(
        md("<pre class=\"x y\"><code class=\"hl evil;\">a</code></pre>"),
        "```\na\n```"
    );
    // The leading newline of `pre` and indentation survive.
    assert_eq!(md("<pre>\n  a\n\n  b\n</pre>"), "```\n  a\n\n  b\n```");
}

#[test]
fn code_keeps_edge_whitespace_and_trailing_blank_lines() {
    assert_eq!(md("<p>x<code> a </code>y</p>"), "x`  a  `y");
    assert_eq!(md("<p>x<code> a</code>y</p>"), "x` a`y");
    assert_eq!(md("<pre>a\n\n</pre>"), "```\na\n\n```");
    assert_eq!(md("<pre>a\n</pre>"), "```\na\n```");
    assert_eq!(md("<pre>a</pre>"), "```\na\n```");
}

#[test]
fn a_heading_inside_a_link_keeps_the_destination() {
    assert_eq!(
        md(r#"<a href="https://example.com/"><h2>題</h2></a>"#),
        "## [題](https://example.com/)"
    );
}

#[test]
fn ordered_lists_that_markdown_cannot_number_faithfully_are_unsupported() {
    for html in [
        "<ol start=\"x\"><li>a</li></ol>",
        "<ol start=\"-1\"><li>a</li></ol>",
        "<ol start=\"1000000000\"><li>a</li></ol>",
        "<ol reversed><li>a</li><li>b</li></ol>",
        "<ol start=\"999999999\"><li>a</li><li>b</li></ol>",
    ] {
        assert_eq!(
            html_to_markdown(html),
            Err(HtmlImportError::Unsupported),
            "{html}"
        );
    }
    assert_eq!(
        md("<ol start=\"999999999\"><li>a</li></ol>"),
        "999999999. a"
    );
}

#[test]
fn lists_and_quotes_keep_their_structure() {
    assert_eq!(
        md("<ul><li>一</li><li>二<ul><li>入れ子</li></ul></li></ul>"),
        "- 一\n- 二\n  - 入れ子"
    );
    assert_eq!(md("<ol><li>a</li></ol>"), "1. a");
    assert_eq!(md("<ol start=\"3\"><li>a</li><li>b</li></ol>"), "3. a\n4. b");
    assert_eq!(md("<ol start=\"0\"><li>a</li></ol>"), "0. a");
    assert_eq!(
        md("<ul><li><p>a</p><p>b</p></li></ul>"),
        "- a\n\n  b"
    );
    assert_eq!(
        md("<ul><li>x<pre>c</pre></li></ul>"),
        "- x\n\n  ```\n  c\n  ```"
    );
    assert_eq!(
        md("<blockquote><p>引用</p><p>二段落</p></blockquote>"),
        "> 引用\n>\n> 二段落"
    );
    assert_eq!(
        md("<blockquote><blockquote>深い</blockquote></blockquote>"),
        "> > 深い"
    );
    assert_eq!(md("<ul><li>a<li>b</ul>"), "- a\n- b");
    assert_eq!(md("<p>a<p>b"), "a\n\nb");
    assert_eq!(md("<div>前<div>中</div>後</div>"), "前\n\n中\n\n後");
    assert_eq!(md("<p>x</p><hr><p>y</p>"), "x\n\n---\n\ny");
}

#[test]
fn adjacent_ordered_lists_keep_their_own_boundary_and_start() {
    let markdown = md("<ol><li>a</li><li>b</li></ol><ol start=\"5\"><li>c</li></ol>");
    assert_eq!(markdown, "1. a\n2. b\n\n<!-- -->\n\n5. c");
    assert_eq!(count_kind(&markdown, NodeKind::List { start: Some(1) }), 1);
    assert_eq!(count_kind(&markdown, NodeKind::List { start: Some(5) }), 1);
    // Unordered neighbours and ordinary items are unchanged.
    assert_eq!(
        md("<ol><li>a</li></ol><ul><li>b</li></ul>"),
        "1. a\n\n- b"
    );
}

#[test]
fn nested_ordered_lists_keep_paragraph_boundaries_and_starts() {
    let markdown = md("<ul><li>x<ol start=\"3\"><li>y</li></ol></li></ul>");
    assert_eq!(markdown, "- x\n\n  3. y");
    assert_eq!(count_kind(&markdown, NodeKind::List { start: Some(3) }), 1);

    let markdown = md("<ul><li>x<ol><li>y</li></ol></li></ul>");
    assert_eq!(markdown, "- x\n  1. y");

    let markdown =
        md("<ul><li><ol><li>a</li></ol><ol start=\"4\"><li>b</li></ol></li></ul>");
    assert_eq!(markdown, "- 1. a\n\n  <!-- -->\n\n  4. b");
    assert_eq!(count_kind(&markdown, NodeKind::List { start: Some(1) }), 1);
    assert_eq!(count_kind(&markdown, NodeKind::List { start: Some(4) }), 1);
}

#[test]
fn unsupported_constructs_fall_back_instead_of_being_half_converted() {
    assert_eq!(
        html_to_markdown("<table><tr><td>a</td></tr></table>"),
        Err(HtmlImportError::Unsupported)
    );
    assert_eq!(
        html_to_markdown("<ol type=\"a\"><li>x</li></ol>"),
        Err(HtmlImportError::Unsupported)
    );
    assert_eq!(
        html_to_markdown("<ol><li value=\"7\">x</li></ol>"),
        Err(HtmlImportError::Unsupported)
    );
    // The plain extraction of a table is tab / newline separated.
    assert_eq!(
        html_to_plain_text("<table><tr><td>a</td><td>b</td></tr><tr><td>c</td></tr></table>")
            .as_deref(),
        Ok("a\tb\nc")
    );
}

#[test]
fn dangerous_content_is_removed() {
    assert_eq!(md("<p>前<script>alert(1)</script><style>p{}</style>後</p>"), "前後");
    assert_eq!(md("<p>a<iframe src=\"https://x\">fallback</iframe>b</p>"), "ab");
    assert_eq!(md("<p>a<svg><text>x</text></svg>b</p>"), "ab");
    assert_eq!(md("<p>a<math><mi>x</mi></math>b</p>"), "ab");
    assert_eq!(md("<p>a<object data=\"x\">fb</object>b</p>"), "ab");
    assert_eq!(md("<p>a<template><p>t</p></template>b</p>"), "ab");
    assert_eq!(md("<p>a<!-- <script> -->b</p>"), "ab");
    assert_eq!(md("<p onclick=\"alert(1)\" style=\"x\">a</p>"), "a");
    assert_eq!(
        md("<head><title>t</title></head><body><p>a</p></body>"),
        "a"
    );
    // The image is neither fetched nor kept: only its alt text remains.
    assert_eq!(md("<p><img src=\"https://x.example/y.png\" alt=\"説明\">後</p>"), "説明後");
    assert_eq!(md("<p><img src=\"https://x.example/y.png\">後</p>"), "後");
    // No raw HTML ever reaches the output.
    let hostile = md("<p>&lt;img src=x onerror=alert(1)&gt;</p>");
    assert!(hostile.starts_with("\\<img"), "{hostile}");
    assert_eq!(count_kind(&hostile, NodeKind::InlineHtml), 0);
}

#[test]
fn browser_clipboard_wrappers_are_transparent() {
    assert_eq!(
        md("<html><body><!--StartFragment--><p>a</p><!--EndFragment--></body></html>"),
        "a"
    );
    assert_eq!(md("<meta charset='utf-8'><p>a</p>"), "a");
    assert_eq!(md("<meta charset='utf-8'><span style=\"x\">a</span>"), "a");
    assert_eq!(md("<p>未閉じ<b>太字"), "未閉じ**太字**");
}

#[test]
fn empty_fragments_are_reported() {
    assert_eq!(html_to_markdown("<p> </p>"), Err(HtmlImportError::Empty));
    assert_eq!(html_to_markdown("<script>x</script>"), Err(HtmlImportError::Empty));
    assert_eq!(html_to_markdown(""), Err(HtmlImportError::Empty));
}

#[test]
fn plain_text_extraction_adds_no_markdown() {
    assert_eq!(
        html_to_plain_text(r#"<h1>見出し</h1><p><b>太</b> <a href="https://example.com/">リンク</a>*</p><ul><li>a</li><li>b</li></ul>"#)
            .as_deref(),
        Ok("見出し\n太 リンク*\na\nb")
    );
    assert_eq!(html_to_plain_text("<script>x</script>"), Err(HtmlImportError::Empty));
}

#[test]
fn limits_are_enforced_while_building() {
    let defaults = HtmlLimits::default();
    let tiny_input = HtmlLimits {
        input_bytes: 3,
        ..defaults
    };
    assert_eq!(
        html_to_markdown_with_limits("<p>abc</p>", &tiny_input),
        Err(HtmlImportError::TooLarge)
    );

    // Depth: the 129th open element is refused (and dropped without recursion).
    let deep = format!("{}x", "<div>".repeat(200));
    assert_eq!(html_to_markdown(&deep), Err(HtmlImportError::TooLarge));
    let deeper = format!("{}x", "<b>".repeat(100_000));
    assert_eq!(html_to_markdown(&deeper), Err(HtmlImportError::TooLarge));
    let shallow = format!("{}x{}", "<div>".repeat(100), "</div>".repeat(100));
    assert_eq!(md(&shallow), "x");

    let few_nodes = HtmlLimits {
        nodes: 10,
        ..defaults
    };
    assert_eq!(
        html_to_markdown_with_limits(&"<b>x</b>".repeat(20), &few_nodes),
        Err(HtmlImportError::TooLarge)
    );
    // Text that merges into one node does not count per run.
    assert!(html_to_markdown_with_limits(&"a".repeat(1000), &few_nodes).is_ok());

    let small_output = HtmlLimits {
        output_bytes: 10,
        ..defaults
    };
    assert_eq!(
        html_to_markdown_with_limits(&format!("<p>{}</p>", "あ".repeat(20)), &small_output),
        Err(HtmlImportError::TooLarge)
    );
    // Nested lists re-indent every line; the output limit still bounds them.
    let nested = format!(
        "{}x{}",
        "<ul><li>".repeat(60),
        "</li></ul>".repeat(60)
    );
    let nested_limit = HtmlLimits {
        output_bytes: 100,
        ..defaults
    };
    assert_eq!(
        html_to_markdown_with_limits(&nested, &nested_limit),
        Err(HtmlImportError::TooLarge)
    );

    // A long run of bare ampersands stays linear and is kept as text.
    let ampersands = "&".repeat(200_000);
    assert_eq!(md(&ampersands).len(), 200_000);
}

#[test]
fn void_elements_respect_the_depth_limit() {
    let two = HtmlLimits {
        depth: 2,
        ..HtmlLimits::default()
    };
    assert_eq!(
        html_to_markdown_with_limits("<div><div><br>x</div></div>", &two),
        Err(HtmlImportError::TooLarge)
    );
    assert_eq!(
        html_to_markdown_with_limits("<div><img alt=\"x\"></div>", &two),
        Ok("x".to_owned())
    );
}

#[test]
fn character_references_follow_html_rules() {
    // Numeric references, with and without the terminating semicolon.
    assert_eq!(md("<p>&#65;&#x42;&#67 &#x44</p>"), "ABC D");
    // Legacy named references work without a semicolon.
    assert_eq!(md("<p>a &amp b &copy c</p>"), "a & b \u{a9} c");
    // C1 numeric references follow windows-1252; invalid ones become U+FFFD.
    assert_eq!(md("<p>&#150;&#0;&#xD800;&#99999999999;</p>"), "\u{2013}\u{fffd}\u{fffd}\u{fffd}");
    // Unknown names stay text.
    assert_eq!(md("<p>&unknown; &</p>"), "\\&unknown; &");
}

#[test]
fn named_references_use_the_complete_html5_table() {
    assert_eq!(md("<p>caf&eacute;</p>"), "café");
    assert_eq!(html_to_plain_text("<p>caf&eacute;</p>").as_deref(), Ok("café"));
    // Names that expand to several code points.
    assert_eq!(md("<p>&NotEqualTilde;</p>"), "\u{2242}\u{338}");
    assert_eq!(html_to_plain_text("<p>&NotEqualTilde;</p>").as_deref(), Ok("\u{2242}\u{338}"));
    // Attribute rules for legacy references are unchanged.
    assert_eq!(
        md(r#"<a href="https://x.example/?a=1&regionId=5&copy=2">x</a>"#),
        "[x](https://x.example/?a=1&regionId=5&copy=2)"
    );
}

/// Delimiter flanking: adjacent runs of one mark must not leave a literal
/// delimiter, and punctuation-only runs between words must stay emphasised.
#[test]
fn adjacent_and_punctuation_only_marks_keep_their_emphasis() {
    let adjacent = md("<p><em>a</em><em>b</em></p>");
    assert_eq!(adjacent, "*ab*");
    assert_eq!(count_kind(&adjacent, NodeKind::Emphasis), 1);

    let strong = md("<p><strong>a</strong><b>b</b></p>");
    assert_eq!(strong, "**ab**");
    assert_eq!(count_kind(&strong, NodeKind::Strong), 1);

    let spaced = md("<p><em>a</em> <em>b</em></p>");
    assert_eq!(spaced, "*a* *b*");
    assert_eq!(count_kind(&spaced, NodeKind::Emphasis), 2);

    let punctuation = md("<p>a<em>!</em>b</p>");
    assert_eq!(punctuation, "a<em>!</em>b");
    assert_eq!(count_kind(&punctuation, NodeKind::InlineHtml), 2);

    let linked = md(r#"<p><em>a</em><a href="https://example.com/"><em>b</em></a></p>"#);
    assert_eq!(linked, "*a*[*b*](https://example.com/)");
    assert_eq!(count_kind(&linked, NodeKind::Link), 1);
}

/// A span whose first or last character is a Unicode punctuation / symbol is
/// not flanked by `**` next to a word, so it uses the inline HTML tag.
#[test]
fn unicode_punctuation_edges_keep_their_emphasis() {
    let both = md("<p>本文<strong>「重要」</strong>です</p>");
    assert_eq!(both, "本文<strong>「重要」</strong>です");
    assert_eq!(count_kind(&both, NodeKind::InlineHtml), 2);

    let leading = md("<p>本文<strong>「重要</strong>です</p>");
    assert_eq!(leading, "本文<strong>「重要</strong>です");
    assert_eq!(count_kind(&leading, NodeKind::InlineHtml), 2);

    let trailing = md("<p>本文<em>重要。</em>です</p>");
    assert_eq!(trailing, "本文<em>重要。</em>です");
    assert_eq!(count_kind(&trailing, NodeKind::InlineHtml), 2);

    let symbols = md("<p>本文<del>★重要☆</del>です</p>");
    assert_eq!(symbols, "本文<del>★重要☆</del>です");
    assert_eq!(count_kind(&symbols, NodeKind::InlineHtml), 2);
}

/// ASCII punctuation at the edge of a mark next to a word is not flanked by
/// `**`, so the inline HTML tag keeps the emphasis after a re-parse.
#[test]
fn ascii_punctuation_edges_keep_their_emphasis() {
    let strong = md("<p>foo<strong>!a!</strong>bar</p>");
    assert_eq!(strong, "foo<strong>!a!</strong>bar");
    assert_eq!(count_kind(&strong, NodeKind::InlineHtml), 2);

    let emphasis = md("<p>foo<em>(a)</em>bar</p>");
    assert_eq!(emphasis, "foo<em>(a)</em>bar");
    assert_eq!(count_kind(&emphasis, NodeKind::InlineHtml), 2);

    let trailing = md("<p>foo<em>a)</em>bar</p>");
    assert_eq!(trailing, "foo<em>a)</em>bar");
    assert_eq!(count_kind(&trailing, NodeKind::InlineHtml), 2);

    let mixed = md("<p>foo<strong>a (b)</strong>bar</p>");
    assert_eq!(mixed, "foo<strong>a (b)</strong>bar");
    assert_eq!(count_kind(&mixed, NodeKind::InlineHtml), 2);

    let unicode = md("<p>本文<strong>!重要。</strong>です</p>");
    assert_eq!(unicode, "本文<strong>!重要。</strong>です");

    let linked = md(r#"<p><strong><a href="https://example.com/">太字</a></strong></p>"#);
    assert_eq!(linked, "**[太字](https://example.com/)**");

    let plain = md("<p>foo<strong>a</strong>bar</p>");
    assert_eq!(plain, "foo**a**bar");
    assert_eq!(count_kind(&plain, NodeKind::Strong), 1);
}

/// Adjacent code spans must not fuse their backtick runs into foreign content.
#[test]
fn adjacent_code_spans_do_not_gain_backticks() {
    let adjacent = md("<p><code>a</code><code>b</code></p>");
    assert_eq!(adjacent, "`ab`");
    assert_eq!(count_kind(&adjacent, NodeKind::InlineCode), 1);

    let ticks = md("<p><code>a`</code><code>`b</code></p>");
    assert_eq!(count_kind(&ticks, NodeKind::InlineCode), 1);
    assert!(ticks.starts_with("``"), "{ticks}");

    let spaced = md("<p><code>a</code> <code>b</code></p>");
    assert_eq!(spaced, "`a` `b`");
    assert_eq!(count_kind(&spaced, NodeKind::InlineCode), 2);
}

#[test]
fn output_is_valid_for_the_markdown_parser() {
    let markdown = md("<h1>題</h1><p>本文 <a href=\"https://example.com/\">リンク</a></p><ul><li>a</li><li>b</li></ul><blockquote><p>q</p></blockquote><pre><code class=\"language-rs\">x</code></pre>");
    assert_eq!(count_kind(&markdown, NodeKind::Heading(1)), 1);
    assert_eq!(count_kind(&markdown, NodeKind::Link), 1);
    assert_eq!(count_kind(&markdown, NodeKind::Quote), 1);
    assert_eq!(count_kind(&markdown, NodeKind::CodeBlock), 1);
    assert_eq!(count_kind(&markdown, NodeKind::InlineHtml), 0);
    assert_eq!(count_kind(&markdown, NodeKind::HtmlBlock), 0);
}
