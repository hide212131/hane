use hane_document::{Revision, RevisionDelta, RopeBuffer, SourceRange, TextBuffer};
use hane_markdown::{
    BlockIndex, FrontMatterDateState, NodeKind, local_block_index, parse_document,
    parse_full_document, recognize_front_matter,
};

fn first_child_kind(source: &str) -> Option<NodeKind> {
    let parsed = parse_full_document(Revision(1), source);
    let first = *parsed
        .tree
        .children(hane_markdown::MarkdownTree::ROOT)
        .first()?;
    parsed.tree.node(first).map(|node| node.kind)
}

#[test]
fn front_matter_recognizes_only_completed_document_prefix() {
    // (source, expected end of the Front Matter range)
    for (source, end) in [
        ("---\ndate: 2026-11-21\n---\nbody\n", 25),
        ("---\r\ndate: 2026-11-21\r\n---\r\nbody\r\n", 28),
        ("---\rdate: 2026-11-21\r---\rbody\r", 25),
        // The closing line may be the unterminated end of the document.
        ("---\na: b\n---", 12),
        // Spaces and tabs may follow either delimiter.
        ("--- \t\na: b\n---\t \nbody", 17),
        // Empty blocks.
        ("---\n---", 7),
        ("---\n\n---\n", 9),
    ] {
        let front_matter =
            recognize_front_matter(source).unwrap_or_else(|| panic!("{source:?} should match"));
        assert_eq!(front_matter.range, SourceRange::new(0, end), "{source:?}");
        assert_eq!(front_matter.opening_line.start.0, 0, "{source:?}");
        assert_eq!(front_matter.closing_line.end.0, end, "{source:?}");
        assert_eq!(
            front_matter.closing_has_line_ending,
            source[..end].ends_with(['\r', '\n']),
            "{source:?}"
        );
        assert_eq!(
            first_child_kind(source),
            Some(NodeKind::FrontMatter),
            "{source:?}"
        );
    }

    // Anything else keeps the parse of a document without Front Matter.
    for source in [
        "\u{feff}---\na: b\n---\nbody\n",
        "\n---\na: b\n---\nbody\n",
        " ---\na: b\n---\nbody\n",
        "----\na: b\n---\nbody\n",
        "---x\na: b\n---\nbody\n",
        "---\na: b\n",
        "---\na: b\n----\nbody\n",
        "---\na: b\n --- \nbody\n",
        "text\n---\na: b\n---\nbody\n",
        "---",
    ] {
        assert!(recognize_front_matter(source).is_none(), "{source:?}");
        let full = parse_full_document(Revision(1), source);
        let plain = parse_document(Revision(1), SourceRange::new(0, source.len()), source);
        assert_eq!(full.tree, plain.tree, "{source:?}");
        assert_eq!(full.markers, plain.markers, "{source:?}");
        assert!(
            full.tree.iter().all(|(_, node)| node.kind != NodeKind::FrontMatter),
            "{source:?}"
        );
    }

    // After one completed pair, a later `---` is an ordinary thematic break.
    let source = "---\na: b\n---\n\n---\nbody\n";
    let parsed = parse_full_document(Revision(1), source);
    assert_eq!(
        parsed
            .tree
            .iter()
            .filter(|(_, node)| node.kind == NodeKind::FrontMatter)
            .count(),
        1
    );
    assert!(
        parsed
            .tree
            .iter()
            .any(|(_, node)| node.kind == NodeKind::Rule)
    );
}

#[test]
fn front_matter_keeps_body_event_offsets_and_isolates_metadata() {
    let header = "---\n# コメント\n[ref]: https://example.com\n```rust\n---\n";
    let body = "\n# 見出し\n\n[link][ref] と [x](https://example.org)\n\n---\n\n末尾\n";
    let source = format!("{header}{body}");
    let parsed = parse_full_document(Revision(3), &source);
    let tree = &parsed.tree;

    let first = *tree
        .children(hane_markdown::MarkdownTree::ROOT)
        .first()
        .expect("root has a first child");
    let front_matter = tree.node(first).expect("front matter node");
    assert_eq!(front_matter.kind, NodeKind::FrontMatter);
    assert_eq!(front_matter.source_range, SourceRange::new(0, header.len()));
    assert!(front_matter.children.is_empty());
    assert_eq!(tree.root().source_range, SourceRange::new(0, source.len()));

    for (id, node) in tree.iter().filter(|(id, _)| *id != first) {
        assert!(
            node.source_range.start.0 >= header.len(),
            "{id:?} {:?} starts inside the header",
            node.kind
        );
        assert!(source.is_char_boundary(node.source_range.start.0));
        assert!(source.is_char_boundary(node.source_range.end.0));
    }
    let slice_of = |kind: NodeKind| {
        tree.iter()
            .find(|(_, node)| node.kind == kind)
            .map(|(_, node)| &source[node.source_range.start.0..node.source_range.end.0])
    };
    assert!(slice_of(NodeKind::Heading(1)).is_some_and(|text| text.starts_with("# 見出し")));
    assert!(slice_of(NodeKind::Rule).is_some_and(|text| text.starts_with("---")));
    // The definition in the header is not visible to the body, while the
    // inline link keeps its absolute range.
    let links = tree
        .iter()
        .filter(|(_, node)| node.kind == NodeKind::Link)
        .map(|(_, node)| &source[node.source_range.start.0..node.source_range.end.0])
        .collect::<Vec<_>>();
    assert_eq!(links, vec!["[x](https://example.org)"]);
    // The header's own heading-like and fence-like lines produced nothing.
    assert!(!tree.iter().any(|(_, node)| node.kind == NodeKind::CodeBlock));
    assert_eq!(
        tree.iter()
            .filter(|(_, node)| matches!(node.kind, NodeKind::Heading(_)))
            .count(),
        1
    );
}

fn date_state(content: &str) -> (FrontMatterDateState, String) {
    let source = format!("---\n{content}---\n");
    let front_matter = recognize_front_matter(&source).expect("front matter");
    (front_matter.metadata.date, source)
}

#[test]
fn front_matter_metadata_uses_one_valid_date() {
    for (content, expected) in [
        ("date: 2026-11-21\n", "2026-11-21"),
        ("date : \"2026-11-21\"\n", "2026-11-21"),
        ("date: '2026-11-21'\n", "2026-11-21"),
        ("date:\t2028-02-29  \n", "2028-02-29"),
        ("タイトル: 日本語\ntitle: 日本語のタイトル\ndate: 2000-02-29\n", "2000-02-29"),
        ("# 日本語のコメント\ndate: \"0001-01-01\"\n", "0001-01-01"),
    ] {
        let (state, source) = date_state(content);
        let FrontMatterDateState::Valid(date) = state else {
            panic!("{content:?} should hold a valid date, got {state:?}");
        };
        assert_eq!(date.text(), expected, "{content:?}");
        // The range is the digits alone and sits on UTF-8 boundaries.
        assert!(source.is_char_boundary(date.value_range.start.0));
        assert!(source.is_char_boundary(date.value_range.end.0));
        assert_eq!(
            &source[date.value_range.start.0..date.value_range.end.0],
            expected,
            "{content:?}"
        );
    }

    for content in [
        "date: 2026-02-29\n",
        "date: 2100-02-29\n",
        "date: 2026-1-2\n",
        "date: 2026-13-01\n",
        "date: 2026-04-31\n",
        "date: 0000-01-01\n",
        "date: \" 2026-11-21 \"\n",
        "date:\n",
        "date: 2026-11-21 # memo\n",
        "date: \"2026-11-21\n",
        "date: 2026-11-21\ndate: 2026-11-22\n",
        "date: 2026-11-21\ndate:\n",
        "date: 二〇二六-11-21\n",
    ] {
        assert_eq!(date_state(content).0, FrontMatterDateState::Invalid, "{content:?}");
    }

    // Keys are case-sensitive; only a lowercase `date` declares one.
    for content in ["Date: 2026-11-21\n", "dates: 2026-11-21\n", "title: x\n", "\n# c\n"] {
        assert_eq!(date_state(content).0, FrontMatterDateState::Absent, "{content:?}");
    }
}

fn edit(buffer: &mut RopeBuffer, range: SourceRange, replacement: &str) -> Vec<RevisionDelta> {
    let base = buffer.revision();
    buffer.edit(range, replacement).expect("edit");
    buffer.deltas_since(base).expect("deltas")
}

fn shape(index: &BlockIndex) -> Vec<(NodeKind, SourceRange, usize)> {
    index
        .blocks()
        .map(|block| (block.kind, block.source_range, block.line_count))
        .collect()
}

/// Applies the edit through the incremental path and checks the result against
/// a full build of the same revision.
fn apply_and_compare(
    index: &mut BlockIndex,
    buffer: &mut RopeBuffer,
    range: SourceRange,
    replacement: &str,
    label: &str,
) {
    let deltas = edit(buffer, range, replacement);
    index.update(buffer, &deltas);
    let fresh = BlockIndex::from_buffer(buffer);
    assert_eq!(shape(index), shape(&fresh), "{label}: {:?}", buffer.full_text());
    assert_eq!(
        index.front_matter().map(|front_matter| front_matter.range),
        fresh.front_matter().map(|front_matter| front_matter.range),
        "{label}"
    );
    assert_eq!(index.covered_bytes(), buffer.len_bytes().0, "{label}");
}

#[test]
fn front_matter_index_reclassifies_after_delimiter_edit() {
    let source = "---\ndate: 2026-11-21\n---\n\n# 本文\n\ntext\n";
    let mut buffer = RopeBuffer::from_text(source);
    let mut index = BlockIndex::from_buffer(&buffer);
    assert_eq!(index.block(0).map(|block| block.kind), Some(NodeKind::FrontMatter));
    assert_eq!(
        index.front_matter().map(|front_matter| front_matter.range),
        Some(SourceRange::new(0, 25))
    );

    // Deleting the opening line, then undoing it.
    apply_and_compare(&mut index, &mut buffer, SourceRange::new(0, 4), "", "drop opening");
    assert!(index.front_matter().is_none());
    apply_and_compare(&mut index, &mut buffer, SourceRange::empty(0), "---\n", "undo opening");
    assert!(index.front_matter().is_some());

    // Deleting the closing line, then undoing it.
    apply_and_compare(&mut index, &mut buffer, SourceRange::new(21, 25), "", "drop closing");
    assert!(index.front_matter().is_none());
    apply_and_compare(&mut index, &mut buffer, SourceRange::empty(21), "---\n", "undo closing");
    assert!(index.front_matter().is_some());

    // An earlier closing line shrinks the block; removing it restores it.
    apply_and_compare(&mut index, &mut buffer, SourceRange::empty(4), "---\n", "early close");
    assert_eq!(
        index.front_matter().map(|front_matter| front_matter.range),
        Some(SourceRange::new(0, 8))
    );
    apply_and_compare(&mut index, &mut buffer, SourceRange::new(4, 8), "", "redo late close");
    assert_eq!(
        index.front_matter().map(|front_matter| front_matter.range),
        Some(SourceRange::new(0, 25))
    );

    // Edits after the block use the incremental path and keep it.
    let end = buffer.len_bytes().0;
    apply_and_compare(&mut index, &mut buffer, SourceRange::empty(end), "more\n", "append");
    apply_and_compare(&mut index, &mut buffer, SourceRange::empty(28), "x", "body edit");
    assert_eq!(
        index.front_matter().map(|front_matter| front_matter.range),
        Some(SourceRange::new(0, 25))
    );

    // A closing line added far below an unclosed opening creates the block.
    let mut buffer = RopeBuffer::from_text("---\n# 見出し\n\ntext\n");
    let mut index = BlockIndex::from_buffer(&buffer);
    assert!(index.front_matter().is_none());
    let end = buffer.len_bytes().0;
    apply_and_compare(&mut index, &mut buffer, SourceRange::empty(end), "---\n", "late close");
    assert!(index.front_matter().is_some());
    assert_eq!(index.block(0).map(|block| block.kind), Some(NodeKind::FrontMatter));
}

#[test]
fn front_matter_local_windows_only_trust_the_real_document_start() {
    let mut source = String::from("---\ndate: 2026-11-21\n---\n\n");
    source.push_str(&"body\n\n".repeat(3_000));
    let buffer = RopeBuffer::from_text(&source);

    let top = local_block_index(&buffer, 0..4);
    assert_eq!(
        top.block_at(hane_document::SourceOffset(0)).map(|block| block.kind),
        Some(NodeKind::FrontMatter)
    );

    // A window that starts below the first line never sees a Front Matter.
    let below = local_block_index(&buffer, 2_900..2_910);
    assert!(
        below
            .blocks_in(SourceRange::new(0, buffer.len_bytes().0))
            .all(|block| block.kind != NodeKind::FrontMatter)
    );
}
