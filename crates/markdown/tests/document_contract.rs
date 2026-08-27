use hane_document::{Revision, RopeBuffer, SourceRange, TextBuffer};
use hane_markdown::{BlockIndex, NodeKind, parse_document};

fn node_count(source: &str, kind: NodeKind) -> usize {
    parse_document(
        Revision(7),
        SourceRange::new(100, 100 + source.len()),
        source,
    )
    .tree
    .iter()
    .filter(|(_, node)| node.kind == kind)
    .count()
}

#[test]
fn multiline_commonmark_fixtures_keep_structural_ranges() {
    let quote = "> first\n> second\n";
    assert_eq!(node_count(quote, NodeKind::Quote), 1);
    assert_eq!(node_count(quote, NodeKind::Paragraph), 1);

    let list = "1) first\n2) second\n";
    assert_eq!(node_count(list, NodeKind::ListItem { task: None }), 2);

    let fenced = "```rust\nlet answer = 42;\n```\n";
    assert_eq!(node_count(fenced, NodeKind::CodeBlock), 1);

    let setext = "Heading 羽\n=========\n";
    let parsed = parse_document(Revision(9), SourceRange::new(12, 12 + setext.len()), setext);
    let (_, heading) = parsed
        .tree
        .iter()
        .find(|(_, node)| node.kind == NodeKind::Heading(1))
        .expect("Setext heading must retain heading semantics");
    assert_eq!(
        heading.source_range,
        SourceRange::new(12, 12 + setext.len())
    );
}

#[test]
fn reference_links_and_escaped_markers_have_unambiguous_parse_contracts() {
    let reference = "[Hane][project]\n\n[project]: https://example.com/hane\n";
    assert_eq!(node_count(reference, NodeKind::Link), 1);

    let escaped = r"\*literal emphasis\* and \[literal link\]";
    let parsed = parse_document(Revision(2), SourceRange::new(0, escaped.len()), escaped);
    assert!(
        parsed
            .tree
            .iter()
            .all(|(_, node)| !matches!(node.kind, NodeKind::Emphasis | NodeKind::Link))
    );
}

/// Block kind of the block owning a physical line, the way the renderer asks.
fn kind_at_line(index: &BlockIndex, buffer: &RopeBuffer, line: usize) -> Option<NodeKind> {
    let range = buffer.line_range(hane_document::LineId(line)).ok()?;
    index.block_at(range.start).map(|block| block.kind)
}

#[test]
fn block_kinds_follow_document_revisions() {
    let mut buffer =
        RopeBuffer::from_text("before\n| Name | 値 |\n|:---|---:|\n| 羽 | 3 |\nafter\n");
    let initial = BlockIndex::from_buffer(&buffer);
    assert_eq!(initial.revision(), Revision(0));
    for line in 1..=3 {
        assert_eq!(kind_at_line(&initial, &buffer, line), Some(NodeKind::Table));
    }

    let insertion = buffer
        .line_content_range(hane_document::LineId(1))
        .unwrap()
        .start;
    buffer
        .edit(SourceRange::empty(insertion.0), "```\n")
        .unwrap();
    let after_open = BlockIndex::from_buffer(&buffer);
    assert_eq!(after_open.revision(), Revision(1));
    assert_ne!(initial.revision(), buffer.revision());
    assert_eq!(
        kind_at_line(&after_open, &buffer, 4),
        Some(NodeKind::CodeBlock),
        "the table rows now sit inside the opened fence"
    );

    buffer
        .edit(SourceRange::new(insertion.0, insertion.0 + 4), "")
        .unwrap();
    let after_close = BlockIndex::from_buffer(&buffer);
    assert_eq!(after_close.revision(), Revision(2));
    assert_eq!(
        kind_at_line(&after_close, &buffer, 4),
        Some(NodeKind::Table)
    );
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

    let stale = BlockIndex::from_buffer(&snapshot);
    let current = BlockIndex::from_buffer(&buffer);
    assert_ne!(stale.revision(), buffer.revision());
    assert_eq!(current.revision(), buffer.revision());
    assert_eq!(stale.revision(), Revision(0));
    assert_eq!(current.revision(), Revision(2));
}

#[test]
fn opening_fence_edit_changes_far_context_and_removal_restores_it() {
    let mut source = String::from("before\n");
    source.push_str(&"body\n".repeat(2_100));
    source.push_str("after\n");
    let mut buffer = RopeBuffer::from_text(&source);
    assert_eq!(
        kind_at_line(&BlockIndex::from_buffer(&buffer), &buffer, 2_050),
        Some(NodeKind::Paragraph)
    );

    let insertion = "before\n".len();
    buffer.edit(SourceRange::empty(insertion), "```\n").unwrap();
    assert_eq!(
        kind_at_line(&BlockIndex::from_buffer(&buffer), &buffer, 2_050),
        Some(NodeKind::CodeBlock)
    );

    buffer
        .edit(SourceRange::new(insertion, insertion + 4), "")
        .unwrap();
    assert_eq!(
        kind_at_line(&BlockIndex::from_buffer(&buffer), &buffer, 2_050),
        Some(NodeKind::Paragraph)
    );
}
