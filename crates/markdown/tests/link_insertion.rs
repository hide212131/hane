use hane_document::{Revision, SourceRange};
use hane_markdown::{LinkInsertionRefusal, clipboard_http_url, parse_document, plan_link_insertion};

const URL: &str = "https://example.com/hane";

/// Plans the link over the first occurrence of `selected` and applies it.
fn insert(source: &str, selected: &str, url: &str) -> Result<String, LinkInsertionRefusal> {
    let start = source.find(selected).expect("selected text is in the source");
    insert_range(source, start, start + selected.len(), url)
}

fn insert_range(
    source: &str,
    start: usize,
    end: usize,
    url: &str,
) -> Result<String, LinkInsertionRefusal> {
    let plan = plan_link_insertion(source, SourceRange::new(start, end), url)?;
    assert_eq!(plan.range, SourceRange::new(start, end));
    assert_eq!(plan.caret_after.0, start + plan.replacement.len());
    let mut edited = source.to_owned();
    edited.replace_range(start..end, &plan.replacement);
    Ok(edited)
}

fn destinations(source: &str) -> Vec<String> {
    parse_document(Revision(1), SourceRange::new(0, source.len()), source)
        .links
        .iter()
        .map(|link| link.destination.to_string())
        .collect()
}

#[test]
fn clipboard_text_is_accepted_only_as_one_http_url_with_a_host() {
    for accepted in [
        "https://example.com/path?q=1#part",
        "http://example.com",
        "HTTPS://Example.COM/A",
        "http://localhost:8080/",
        "http://127.0.0.1/x",
        "http://[::1]:3000/",
        "https://日本語.example/パス",
        "https://example.com/a_(b)?q=%E3%81%82&r=1#f",
    ] {
        assert_eq!(
            clipboard_http_url(accepted).as_deref(),
            Some(accepted),
            "{accepted:?}"
        );
    }
    // Surrounding whitespace is trimmed; the value is otherwise unchanged.
    assert_eq!(
        clipboard_http_url("  https://Example.com/A?z=1&a=2\r\n").as_deref(),
        Some("https://Example.com/A?z=1&a=2")
    );

    for rejected in [
        "",
        "   \n",
        "www.example.com",
        "example.com/path",
        "/relative/path",
        "./relative",
        "[label](https://example.com/)",
        "https://example.com/ and text",
        "https://a.example/ https://b.example/",
        "https://a.example/\nhttps://b.example/",
        "https://exa\tmple.com/",
        "https://example.com/\u{7}",
        "http:example.com",
        "http://",
        "https://",
        "http:///example.com",
        "https://@example.com/",
        "https://:80/",
        "javascript:alert(1)",
        "data:text/plain,hello",
        "file:///etc/hosts",
        "mailto:someone@example.com",
        "ftp://example.com/",
    ] {
        assert_eq!(clipboard_http_url(rejected), None, "{rejected:?}");
    }
}

#[test]
fn a_selection_becomes_a_link_and_the_rest_of_the_source_is_untouched() {
    let edited = insert("前 Haneの紹介 後\n", "Haneの紹介", URL).unwrap();
    assert_eq!(edited, "前 [Haneの紹介](https://example.com/hane) 後\n");
    assert_eq!(destinations(&edited), [URL]);
}

#[test]
fn the_first_and_last_bytes_of_a_document_can_be_linked() {
    assert_eq!(
        insert("abc", "abc", URL).unwrap(),
        "[abc](https://example.com/hane)"
    );
    assert_eq!(
        insert("abc def", "abc", URL).unwrap(),
        "[abc](https://example.com/hane) def"
    );
    assert_eq!(
        insert("abc def", "def", URL).unwrap(),
        "abc [def](https://example.com/hane)"
    );
}

#[test]
fn emoji_and_combining_characters_are_kept_whole() {
    let source = "x 👨‍👩‍👧 e\u{301} y";
    let edited = insert(source, "👨‍👩‍👧 e\u{301}", URL).unwrap();
    assert_eq!(
        edited,
        "x [👨‍👩‍👧 e\u{301}](https://example.com/hane) y"
    );
    // A range that cuts a character in the middle is refused, not repaired.
    assert_eq!(
        insert_range("あいう", 1, 3, URL),
        Err(LinkInsertionRefusal::OutOfRange)
    );
    assert_eq!(
        insert_range("abc", 1, 9, URL),
        Err(LinkInsertionRefusal::OutOfRange)
    );
}

#[test]
fn empty_and_blank_selections_are_refused() {
    assert_eq!(
        insert_range("abc def", 2, 2, URL),
        Err(LinkInsertionRefusal::EmptySelection)
    );
    assert_eq!(
        insert_range("abc   def", 3, 6, URL),
        Err(LinkInsertionRefusal::EmptySelection)
    );
}

#[test]
fn a_soft_wrapped_paragraph_and_crlf_endings_are_preserved() {
    assert_eq!(
        insert("foo\nbar baz", "foo\nbar", URL).unwrap(),
        "[foo\nbar](https://example.com/hane) baz"
    );
    assert_eq!(
        insert("foo\r\nbar baz", "bar", URL).unwrap(),
        "foo\r\n[bar](https://example.com/hane) baz"
    );
}

#[test]
fn destination_characters_are_escaped_without_changing_the_url() {
    for url in [
        "https://example.com/a_(b)?q=1&r=2#frag%20x",
        "https://example.com/a<b>c",
        "https://example.com/back\\slash",
        "https://example.com/?a=1&amp;b=2",
        "https://example.com/?a=1&#38;b=2",
        "https://example.com/日本語?q=あ#節",
        "https://xn--wgv71a119e.example/%E3%81%82",
        "https://example.com/((a)",
    ] {
        let edited = insert("see label here", "label", url).unwrap();
        assert!(edited.starts_with("see [label]("), "{edited:?}");
        assert!(edited.ends_with(") here"), "{edited:?}");
        assert_eq!(destinations(&edited), [url], "{edited:?}");
    }
    assert_eq!(
        insert("label", "label", "https://example.com/a_(b)").unwrap(),
        "[label](https://example.com/a_\\(b\\))"
    );
    assert_eq!(
        insert("label", "label", "https://example.com/?a=1&amp;b=2").unwrap(),
        "[label](https://example.com/?a=1\\&amp;b=2)"
    );
}

#[test]
fn literal_brackets_in_the_label_are_escaped_only_when_needed() {
    // Balanced brackets read back as one link, so they stay as written.
    let balanced = insert("x a [b] c y", "a [b] c", URL).unwrap();
    assert_eq!(destinations(&balanced), [URL]);

    // An unbalanced literal bracket is escaped so the label is not cut short.
    let edited = insert("x a ] b y", "a ] b", URL).unwrap();
    assert_eq!(edited, "x [a \\] b](https://example.com/hane) y");
    assert_eq!(destinations(&edited), [URL]);

    // An existing escape is left alone.
    let edited = insert("x a \\] b y", "a \\] b", URL).unwrap();
    assert_eq!(edited, "x [a \\] b](https://example.com/hane) y");
}

#[test]
fn a_selection_that_splits_an_escape_or_ends_in_a_lone_backslash_is_refused() {
    assert_eq!(
        insert_range("\\*a", 1, 3, URL),
        Err(LinkInsertionRefusal::UnsafeBoundary)
    );
    assert_eq!(
        insert_range("a\\* b", 0, 2, URL),
        Err(LinkInsertionRefusal::UnsafeBoundary)
    );
    assert_eq!(
        insert("a\\ b", "a\\", URL),
        Err(LinkInsertionRefusal::UnsafeBoundary)
    );
    // An escaped backslash pair is fine.
    assert_eq!(
        insert("x a\\\\ y", "a\\\\", URL).unwrap(),
        "x [a\\\\](https://example.com/hane) y"
    );
}

#[test]
fn decoration_wholly_inside_the_selection_is_kept_around_the_link() {
    assert_eq!(
        insert("a **太字** b", "**太字**", URL).unwrap(),
        "a [**太字**](https://example.com/hane) b"
    );
    assert_eq!(
        insert("a *強調* `code` b", "*強調* `code`", URL).unwrap(),
        "a [*強調* `code`](https://example.com/hane) b"
    );
    // A selection strictly inside the decoration links inside it.
    assert_eq!(
        insert("**太字**", "太字", URL).unwrap(),
        "**[太字](https://example.com/hane)**"
    );
    assert_eq!(
        insert("~~消す~~ x", "消す", URL).unwrap(),
        "~~[消す](https://example.com/hane)~~ x"
    );
}

#[test]
fn a_selection_that_cuts_through_syntax_is_refused() {
    assert_eq!(
        insert("**ab** cd", "b** c", URL),
        Err(LinkInsertionRefusal::PartialSyntax)
    );
    assert_eq!(
        insert("`ab` cd", "b` c", URL),
        Err(LinkInsertionRefusal::PartialSyntax)
    );
    assert_eq!(
        insert("`ab` cd", "a", URL),
        Err(LinkInsertionRefusal::PartialSyntax)
    );
    assert_eq!(
        insert("*ab* cd", "*a", URL),
        Err(LinkInsertionRefusal::PartialSyntax)
    );
}

#[test]
fn existing_links_images_and_html_are_not_wrapped() {
    for (source, selected) in [
        ("[a](https://x.example/) b", "a"),
        ("x [a](https://x.example/) b", "x [a](https://x.example/)"),
        ("x [a](https://x.example/) b", "a](https://x.example/) b"),
        ("x ![alt](img.png) y", "alt"),
        ("x <https://x.example/> y", "x <https://x.example/>"),
        ("x <b>y</b> z", "x <b>y"),
        ("[a](relative/path) b", "a"),
    ] {
        assert_eq!(
            insert(source, selected, URL),
            Err(LinkInsertionRefusal::ExistingSyntax),
            "{source:?} / {selected:?}"
        );
    }
}

#[test]
fn headings_list_items_quotes_and_table_cells_are_supported() {
    assert_eq!(
        insert("# 見出し\n\n本文", "見出し", URL).unwrap(),
        "# [見出し](https://example.com/hane)\n\n本文"
    );
    assert_eq!(
        insert("見出し\n====\n", "見出し", URL).unwrap(),
        "[見出し](https://example.com/hane)\n====\n"
    );
    assert_eq!(
        insert("- 項目\n- 次\n", "項目", URL).unwrap(),
        "- [項目](https://example.com/hane)\n- 次\n"
    );
    assert_eq!(
        insert("1. 項目\n2. 次\n", "次", URL).unwrap(),
        "1. 項目\n2. [次](https://example.com/hane)\n"
    );
    assert_eq!(
        insert("> 引用 文\n", "引用", URL).unwrap(),
        "> [引用](https://example.com/hane) 文\n"
    );
    assert_eq!(
        insert("> 一行目\n> 二行目\n", "一行目\n> 二行目", URL).unwrap(),
        "> [一行目\n> 二行目](https://example.com/hane)\n"
    );
    let table = "| a | b |\n|---|---|\n| 1 | 2 |\n";
    assert_eq!(
        insert(table, "b", URL).unwrap(),
        "| a | [b](https://example.com/hane) |\n|---|---|\n| 1 | 2 |\n"
    );
    assert_eq!(
        insert(table, "2", URL).unwrap(),
        "| a | b |\n|---|---|\n| 1 | [2](https://example.com/hane) |\n"
    );
}

#[test]
fn unsupported_contexts_are_refused_and_nothing_is_planned() {
    for (source, selected) in [
        ("```\ncode\n```\n", "code"),
        ("    indented code\n", "indented code"),
        ("a\n\nb\n", "a\n\nb"),
        ("- 項目\n- 次\n", "項目\n- 次"),
        ("# 見出し\n", "# 見出し"),
        ("> 引用\n", "> 引用"),
        ("- 項目\n", "- 項目"),
        ("| a | b |\n|---|---|\n| 1 | 2 |\n", "a | b"),
        ("| a | b |\n|---|---|\n| 1 | 2 |\n", "|---|"),
        ("- [ ] やること\n", "[ ] やること"),
    ] {
        assert!(
            matches!(
                insert(source, selected, URL),
                Err(LinkInsertionRefusal::UnsupportedContext | LinkInsertionRefusal::ExistingSyntax)
            ),
            "{source:?} / {selected:?}"
        );
    }
    // The task text itself is an ordinary list item text.
    assert_eq!(
        insert("- [ ] やること\n", "やること", URL).unwrap(),
        "- [ ] [やること](https://example.com/hane)\n"
    );
}

#[test]
fn text_outside_the_selection_keeps_its_syntax() {
    // A selection next to emphasis must not merge with its delimiters.
    let edited = insert("*foo*bar baz", "bar", URL).unwrap();
    assert_eq!(edited, "*foo*[bar](https://example.com/hane) baz");
    let parsed = parse_document(Revision(1), SourceRange::new(0, edited.len()), &edited);
    assert_eq!(parsed.links.len(), 1);
}

#[test]
fn a_link_that_would_break_surrounding_emphasis_is_refused_unchanged() {
    // `*` next to `[` stops opening emphasis, so the outer italic would be lost.
    let source = "x*a b*";
    assert_eq!(
        insert_range(source, 2, 3, URL),
        Err(LinkInsertionRefusal::Unverified)
    );
    // Emphasis that survives the link keeps being linked inside.
    assert_eq!(
        insert("*a b* c", "b", URL).unwrap(),
        "*a [b](https://example.com/hane)* c"
    );
    assert_eq!(
        insert("**a b** c", "a", URL).unwrap(),
        "**[a](https://example.com/hane) b** c"
    );
    // The whole decorated run can still be linked.
    assert_eq!(
        insert("x *a b* y", "*a b*", URL).unwrap(),
        "x [*a b*](https://example.com/hane) y"
    );
}

#[test]
fn a_selection_cutting_a_character_reference_is_refused() {
    for (source, start, end) in [
        ("&amp; tail", 1, 4),
        ("&amp; tail", 0, 3),
        ("&amp; tail", 2, 6),
        ("&#38; tail", 1, 3),
        ("&#x26; tail", 0, 4),
        ("a &amp; b", 3, 7),
    ] {
        assert_eq!(
            insert_range(source, start, end, URL),
            Err(LinkInsertionRefusal::UnsafeBoundary),
            "{source:?} {start}..{end}"
        );
    }
    // A whole reference, or text beside one, is still linked.
    assert_eq!(
        insert("&amp; tail", "&amp;", URL).unwrap(),
        "[&amp;](https://example.com/hane) tail"
    );
    assert_eq!(
        insert("a &#38; b", "&#38; b", URL).unwrap(),
        "a [&#38; b](https://example.com/hane)"
    );
    assert_eq!(
        insert("&#x26; tail", "tail", URL).unwrap(),
        "&#x26; [tail](https://example.com/hane)"
    );
    // A bare ampersand is ordinary text.
    assert_eq!(
        insert("a & b", "& b", URL).unwrap(),
        "a [& b](https://example.com/hane)"
    );
}

#[test]
fn every_refusal_has_a_status_message() {
    for refusal in [
        LinkInsertionRefusal::EmptySelection,
        LinkInsertionRefusal::OutOfRange,
        LinkInsertionRefusal::UnsupportedContext,
        LinkInsertionRefusal::ExistingSyntax,
        LinkInsertionRefusal::PartialSyntax,
        LinkInsertionRefusal::UnsafeBoundary,
        LinkInsertionRefusal::Unverified,
    ] {
        assert!(!refusal.message().is_empty());
    }
}
