use hane_document::{Revision, RopeBuffer, SourceRange, TextBuffer};
use hane_markdown::{BlockKind, InlineKind, parse_block_context, parse_document};

fn block_count(source: &str, kind: BlockKind) -> usize {
    parse_document(
        Revision(7),
        SourceRange::new(100, 100 + source.len()),
        source,
    )
    .blocks
    .iter()
    .filter(|block| block.kind == kind)
    .count()
}

#[test]
fn multiline_commonmark_fixtures_keep_structural_ranges() {
    let quote = "> first\n> second\n";
    assert_eq!(block_count(quote, BlockKind::Quote), 1);
    assert_eq!(block_count(quote, BlockKind::Paragraph), 1);

    let list = "1) first\n2) second\n";
    assert_eq!(block_count(list, BlockKind::ListItem), 2);

    let fenced = "```rust\nlet answer = 42;\n```\n";
    let parsed = parse_document(Revision(3), SourceRange::new(40, 40 + fenced.len()), fenced);
    assert_eq!(
        parsed
            .blocks
            .iter()
            .filter(|block| block.kind == BlockKind::CodeBlock)
            .count(),
        1
    );
    assert!(
        parsed
            .spans
            .iter()
            .any(|span| span.kind == InlineKind::CodeBlock)
    );

    let setext = "Heading 羽\n=========\n";
    let parsed = parse_document(Revision(9), SourceRange::new(12, 12 + setext.len()), setext);
    let heading = parsed
        .blocks
        .iter()
        .find(|block| block.kind == BlockKind::Heading(1))
        .expect("Setext heading must retain heading semantics");
    assert_eq!(
        heading.source_range,
        SourceRange::new(12, 12 + setext.len())
    );
}

#[test]
fn reference_links_and_escaped_markers_have_unambiguous_parse_contracts() {
    let reference = "[Hane][project]\n\n[project]: https://example.com/hane\n";
    let parsed = parse_document(Revision(1), SourceRange::new(0, reference.len()), reference);
    assert!(
        parsed
            .spans
            .iter()
            .any(|span| span.kind == InlineKind::Link)
    );

    let escaped = r"\*literal emphasis\* and \[literal link\]";
    let parsed = parse_document(Revision(2), SourceRange::new(0, escaped.len()), escaped);
    assert!(
        parsed
            .spans
            .iter()
            .all(|span| !matches!(span.kind, InlineKind::Italic | InlineKind::Link))
    );
}

#[test]
fn table_and_fence_context_follow_document_revisions() {
    let mut buffer =
        RopeBuffer::from_text("before\n| Name | 値 |\n|:---|---:|\n| 羽 | 3 |\nafter\n");
    let initial = parse_block_context(&buffer);
    assert_eq!(initial.revision, Revision(0));
    for line in 1..=3 {
        assert_eq!(initial.line_is_table(line), Some(true));
    }

    let insertion = buffer
        .line_content_range(hane_document::LineId(1))
        .unwrap()
        .start;
    buffer
        .edit(SourceRange::empty(insertion.0), "```\n")
        .unwrap();
    let after_open = parse_block_context(&buffer);
    assert_eq!(after_open.revision, Revision(1));
    assert_ne!(initial.revision, buffer.revision());
    assert_eq!(after_open.line_is_fenced(4), Some(true));

    buffer
        .edit(SourceRange::new(insertion.0, insertion.0 + 4), "")
        .unwrap();
    let after_close = parse_block_context(&buffer);
    assert_eq!(after_close.revision, Revision(2));
    assert_eq!(after_close.line_is_fenced(4), Some(false));
}

#[test]
fn stale_background_result_cannot_match_the_current_revision() {
    let mut buffer = RopeBuffer::from_text("before\nbody\nafter\n");
    let snapshot = buffer.clone();
    buffer
        .edit(SourceRange::empty("before\n".len()), "```\n")
        .unwrap();
    buffer
        .edit(SourceRange::empty(buffer.len_bytes().0), "```\n")
        .unwrap();

    let stale = parse_block_context(&snapshot);
    let current = parse_block_context(&buffer);
    assert_ne!(stale.revision, buffer.revision());
    assert_eq!(current.revision, buffer.revision());
    assert_eq!(stale.revision, Revision(0));
    assert_eq!(current.revision, Revision(2));
}

#[test]
fn opening_fence_edit_changes_far_context_and_removal_restores_it() {
    let mut source = String::from("before\n");
    source.push_str(&"body\n".repeat(2_100));
    source.push_str("after\n");
    let mut buffer = RopeBuffer::from_text(&source);
    assert_eq!(
        parse_block_context(&buffer).line_is_fenced(2_050),
        Some(false)
    );

    let insertion = "before\n".len();
    buffer.edit(SourceRange::empty(insertion), "```\n").unwrap();
    assert_eq!(
        parse_block_context(&buffer).line_is_fenced(2_050),
        Some(true)
    );

    buffer
        .edit(SourceRange::new(insertion, insertion + 4), "")
        .unwrap();
    assert_eq!(
        parse_block_context(&buffer).line_is_fenced(2_050),
        Some(false)
    );
}
