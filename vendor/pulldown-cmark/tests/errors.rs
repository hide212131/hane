use pulldown_cmark::{Event, Options, Parser};

#[test]
fn hard_break_needs_two_trailing_spaces_not_tabs() {
    // CommonMark 0.31.2 section 6.7: the space-type hard break requires two
    // or more U+0020 SPACE immediately before the line ending. Tab is not
    // `space`, so it must never count toward a hard break, nor be hidden
    // inside one; it stays ordinary text.
    for (source, expect_hard, expect_text) in [
        ("foo\t\t\nbar", false, "foo\t\t"),
        ("foo \t\nbar", false, "foo \t"),
        ("foo\t \nbar", false, "foo\t"),
        ("foo\t  \nbar", true, "foo\t"),
    ] {
        let events = Parser::new(source).collect::<Vec<_>>();
        let has_hard = events.iter().any(|event| *event == Event::HardBreak);
        let has_soft = events.iter().any(|event| *event == Event::SoftBreak);
        assert_eq!(has_hard, expect_hard, "{source:?}: {events:?}");
        assert_eq!(has_soft, !expect_hard, "{source:?}: {events:?}");
        let text = events
            .iter()
            .find_map(|event| match event {
                Event::Text(text) => Some(text.as_ref()),
                _ => None,
            })
            .unwrap();
        assert_eq!(text, expect_text, "{source:?}: {events:?}");
    }
}

#[test]
fn code_span_line_endings_normalize_once_and_preserve_source_offsets() {
    // CommonMark 0.31.2 sections 2.1 and 6.1: CRLF is one line ending,
    // and each line ending becomes one space before optional edge trimming.
    for ending in ["\n", "\r\n", "\r"] {
        for (source, expected) in [
            (format!("before `{ending}x{ending}` after"), "x"),
            (format!("before `羽{ending}x` after"), "羽 x"),
            (format!("before ` {ending} ` after"), "   "),
            (format!("before `{ending}` after"), " "),
            (format!("> before `{ending}> x{ending}> ` after"), "x"),
            (format!("- before `{ending}  x{ending}  ` after"), "x"),
            (format!("> before ` {ending}>  ` after"), "   "),
        ] {
            let code_events = Parser::new(&source)
                .into_offset_iter()
                .filter_map(|(event, range)| match event {
                    Event::Code(text) => Some((text, range)),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(code_events.len(), 1, "{source:?}");
            let (text, range) = &code_events[0];
            assert_eq!(text.as_ref(), expected, "{source:?}");
            let original_range = source.find('`').unwrap()..source.rfind('`').unwrap() + 1;
            assert_eq!(*range, original_range, "{source:?}");
            assert_eq!(&source[range.clone()], &source[original_range]);
        }
    }
}

fn parse(md: &str) {
    let parser = Parser::new(md);

    for _ in parser {}
}

fn parse_all_options(md: &str) {
    let parser = Parser::new_ext(md, Options::all());

    for _ in parser {}
}

#[test]
fn test_lists_inside_code_spans() {
    parse(
        r"- `
x
**
  *
  `",
    );
}

#[test]
fn test_fuzzer_input_1() {
    parse(">\n >>><N\n");
}

#[test]
fn test_fuzzer_input_2() {
    parse(" \u{b}\\\r- ");
}

#[test]
fn test_fuzzer_input_3() {
    parse_all_options("\n # #\r\u{1c} ");
}

#[test]
fn test_fuzzer_input_4() {
    parse_all_options("\u{0}{\tϐ}\n-");
}

#[test]
fn test_fuzzer_input_5() {
    parse_all_options(" \u{c}{}\n-\n");
}

#[test]
fn test_fuzzer_input_6() {
    parse("*\t[][\n\t<p]>\n\t[]");
}

#[test]
fn test_fuzzer_input_7() {
    parse_all_options("[][{]}\n-");
}

#[test]
fn test_fuzzer_input_8() {
    parse_all_options("a\n \u{c}{}\n-");
}

#[test]
fn test_fuzzer_input_9() {
    parse_all_options("a\n \u{c}{}\\\n-");
}

#[test]
fn test_fuzzer_input_10() {
    parse_all_options("[[    \t\n   \u{c}\u{c}\u{c}\u{c}\u{c}    {}\n-\r\u{e}\u{0}\u{0}{# }\n\u{b}\u{b}\u{b}\u{b}\u{b}\u{b}\u{b}\u{b}\u{b}\u{b}\u{0}\u{0}");
}

#[test]
fn test_fuzzer_input_11() {
    parse_all_options(
        "[[\u{c}\u{c}   \t\n   \u{c}\u{c}\u{c}\u{c}\u{c}\u{c}\u{c}\u{c}\u{c}       {}\n-\r\u{e}",
    );
}

#[test]
fn test_fuzzer_input_12() {
    parse_all_options("\u{c}-\n\u{c}\n-");
}

#[test]
fn test_fuzzer_input_13() {
    // Does not fail with Options::all():
    Parser::new_ext(
        ".\r> ^](\r\u{c}\r\0\0\r.\r[^\0\0\\\0\0\0^^^^^]",
        Options::ENABLE_FOOTNOTES,
    );
}

#[test]
fn test_wrong_code_block() {
    parse(
        r##"```
 * ```
 "##,
    );
}

#[test]
fn test_unterminated_link() {
    parse("[](\\");
}

#[test]
fn test_unterminated_autolink() {
    parse("<a");
}

#[test]
fn test_infinite_loop() {
    parse("[<!W\n\\\n");
}

#[test]
fn test_html_tag() {
    parse("<script\u{feff}");
}

// all of test_bad_slice_* were found in https://github.com/raphlinus/pulldown-cmark/issues/521
#[test]
fn test_bad_slice_a() {
    parse("><a\n");
}

#[test]
fn test_bad_slice_b() {
    parse("><a a\n");
}

#[test]
fn test_bad_slice_unicode() {
    parse("><a a=\n毿>")
}

#[test]
fn test_simd_wrapping_shr_issue_651() {
    parse("`````````````````````````````````x`");
}
