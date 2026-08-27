//! Shared Markdown feature fixture format (R3.25).
//!
//! A fixture describes one Markdown construct once, and [`verify`] checks every
//! contract that construct has to satisfy across the whole pipeline: the parse
//! tree shape, the derived marker ranges, the presented display kind and visual
//! text, SourceMap round-tripping, disclosure, and the bytes a save would write.
//!
//! Adding a Markdown feature means adding a fixture here rather than a bespoke
//! test per layer. The harness itself makes exactly the three calls `EditorView`
//! makes — `parse_block_context`, [`LineContext::from_document_context`], and
//! [`present_polished_line`] — and has no per-feature branch, so a fixture that
//! passes is also evidence that the feature needs nothing from the UI crate.

use hane_document::{Bias, LineId, RopeBuffer, SourceOffset, SourceRange, TextBuffer};
use hane_markdown::{MarkdownTree, NodeKind, parse_block_context, parse_document};
use hane_presentation::{BlockKind, LineContext, VisualBlock, present_polished_line};

const LINE_HEIGHT: f32 = 26.0;

pub struct MarkdownFixture {
    pub name: &'static str,
    /// Whole-document source, presented line by line exactly as the editor does.
    /// Written without a trailing newline so every line carries content.
    pub source: &'static str,
    /// Strict ancestor chains the parse tree must contain, root-most first and
    /// excluding the synthetic document root.
    pub tree_paths: &'static [&'static [NodeKind]],
    /// Every derived marker, in order, as the source text it covers.
    pub markers: &'static [&'static str],
    /// Display kind per source line with nothing disclosed.
    pub block_kinds: &'static [BlockKind],
    /// Visual text per source line with nothing disclosed, newline trimmed.
    pub visual_lines: &'static [&'static str],
}

/// Runs every fixture contract. Panics with the fixture name on the first
/// violation.
pub fn verify(fixture: &MarkdownFixture) {
    let buffer = RopeBuffer::from_text(fixture.source);
    let whole = SourceRange::new(0, fixture.source.len());
    let parsed = parse_document(buffer.revision(), whole, fixture.source);

    verify_tree(fixture, &parsed.tree);
    verify_markers(fixture, &parsed.markers);
    verify_lines(fixture, &buffer);
    verify_disclosure_and_saved_bytes(fixture, &buffer);
}

fn verify_tree(fixture: &MarkdownFixture, tree: &MarkdownTree) {
    for (id, node) in tree.iter() {
        let parent = tree
            .node(node.parent.expect("only the root has no parent"))
            .expect("parent must exist");
        assert!(
            parent.source_range.start <= node.source_range.start
                && node.source_range.end <= parent.source_range.end,
            "{}: {id:?} {:?} escapes its parent {:?}",
            fixture.name,
            node.kind,
            parent.kind
        );
        assert_eq!(
            node.depth,
            parent.depth + 1,
            "{}: {id:?} depth disagrees with its parent",
            fixture.name
        );
        assert!(
            fixture.source.is_char_boundary(node.source_range.start.0)
                && fixture.source.is_char_boundary(node.source_range.end.0),
            "{}: {id:?} {:?} splits a character",
            fixture.name,
            node.kind
        );
        assert_ne!(
            node.kind,
            NodeKind::Unsupported,
            "{}: {id:?} parsed to an unmodeled node; give it a NodeKind",
            fixture.name
        );
    }
    for path in fixture.tree_paths {
        assert!(
            contains_path(tree, path),
            "{}: parse tree is missing the chain {path:?}",
            fixture.name
        );
    }
}

/// True when some node's strict ancestor chain ends with `path`.
fn contains_path(tree: &MarkdownTree, path: &[NodeKind]) -> bool {
    let Some(leaf) = path.last() else {
        return true;
    };
    tree.iter().any(|(id, node)| {
        node.kind == *leaf
            && tree.ancestors(id).count() > path.len()
            && tree
                .ancestors(id)
                .zip(path.iter().rev())
                .all(|(ancestor, kind)| tree.node(ancestor).is_some_and(|node| node.kind == *kind))
    })
}

fn verify_markers(fixture: &MarkdownFixture, markers: &[SourceRange]) {
    assert!(
        markers.windows(2).all(|pair| pair[0].end <= pair[1].start),
        "{}: markers must be sorted and non-overlapping",
        fixture.name
    );
    let covered = markers
        .iter()
        .map(|marker| &fixture.source[marker.start.0..marker.end.0])
        .collect::<Vec<_>>();
    assert_eq!(
        covered, fixture.markers,
        "{}: derived markers changed",
        fixture.name
    );
}

/// Presents one line the way `EditorView` does: context from the document-wide
/// index, then a single presentation call.
fn present_line(
    buffer: &RopeBuffer,
    context: &hane_markdown::BlockContextIndex,
    line: usize,
    disclosure: Option<SourceRange>,
) -> (SourceRange, String, VisualBlock) {
    let range = buffer.line_range(LineId(line)).expect("line in range");
    let source = buffer.text(range).expect("line text");
    let line_context = LineContext::from_document_context(
        context.line_is_fenced(line).unwrap_or(false),
        context.line_is_table(line).unwrap_or(false),
    );
    let block = present_polished_line(
        line as u64,
        buffer.revision(),
        range,
        &source,
        LINE_HEIGHT,
        disclosure,
        line_context,
    );
    assert_eq!(block.context, line_context, "presented context is recorded");
    (range, source, block)
}

fn verify_lines(fixture: &MarkdownFixture, buffer: &RopeBuffer) {
    let context = parse_block_context(buffer);
    let lines = buffer.line_count();
    assert_eq!(
        fixture.block_kinds.len(),
        lines,
        "{}: expected block kinds must cover every line",
        fixture.name
    );
    assert_eq!(
        fixture.visual_lines.len(),
        lines,
        "{}: expected visual lines must cover every line",
        fixture.name
    );
    for line in 0..lines {
        let (range, source, block) = present_line(buffer, &context, line, None);
        assert_eq!(
            block.kind, fixture.block_kinds[line],
            "{}: line {line} display kind",
            fixture.name
        );
        assert_eq!(
            block.visual_text.trim_end_matches(['\r', '\n']),
            fixture.visual_lines[line],
            "{}: line {line} visual text",
            fixture.name
        );
        assert_eq!(
            segment_source(&block, fixture.source),
            source,
            "{}: line {line} segments dropped source bytes",
            fixture.name
        );
        assert_eq!(
            block.source_range, range,
            "{}: line {line} block range",
            fixture.name
        );
    }
}

/// Concatenates the source the block's mapping segments claim, in order. Equal
/// to the block's own source exactly when the segments tile it.
fn segment_source(block: &VisualBlock, document: &str) -> String {
    block
        .source_map
        .segments
        .iter()
        .filter(|segment| !segment.source_range.is_empty())
        .map(|segment| &document[segment.source_range.start.0..segment.source_range.end.0])
        .collect()
}

/// Walks the cursor over every character boundary in the document. At each
/// position the whole document must still be reproducible from the presented
/// segments (what a save writes), the cursor's own offset must round-trip so the
/// caret lands where it was put, and every other boundary must canonicalize to a
/// stable position under both affinities.
fn verify_disclosure_and_saved_bytes(fixture: &MarkdownFixture, buffer: &RopeBuffer) {
    let context = parse_block_context(buffer);
    let lines = buffer.line_count();
    for cursor in (0..=fixture.source.len()).filter(|at| fixture.source.is_char_boundary(*at)) {
        let mut saved = String::with_capacity(fixture.source.len());
        for line in 0..lines {
            let range = buffer.line_range(LineId(line)).expect("line in range");
            // Same ownership rule the editor uses: a boundary belongs to the
            // following line, and the document end belongs to the last line.
            let owns_cursor = range.start.0 <= cursor
                && (cursor < range.end.0 || (line + 1 == lines && cursor == range.end.0));
            let disclosure = owns_cursor.then(|| SourceRange::empty(cursor));
            let (_, _, block) = present_line(buffer, &context, line, disclosure);
            saved.push_str(&segment_source(&block, fixture.source));

            if owns_cursor {
                let offset = SourceOffset(cursor);
                let visual = block
                    .source_map
                    .source_to_visual(offset, Bias::After)
                    .unwrap_or_else(|| panic!("{}: no mapping at cursor {cursor}", fixture.name))
                    .visual_offset;
                assert_eq!(
                    block
                        .source_map
                        .visual_to_source(visual, Bias::After)
                        .unwrap()
                        .source_offset,
                    offset,
                    "{}: the disclosed cursor at {cursor} did not round-trip",
                    fixture.name
                );
            }

            for at in
                (range.start.0..=range.end.0).filter(|at| fixture.source.is_char_boundary(*at))
            {
                for affinity in [Bias::Before, Bias::After] {
                    let normalized = block
                        .source_map
                        .normalize_source(SourceOffset(at), affinity)
                        .unwrap_or_else(|| {
                            panic!("{}: no mapping at {at} with cursor {cursor}", fixture.name)
                        });
                    assert!(
                        fixture.source.is_char_boundary(normalized.0),
                        "{}: {at} normalized into a character at cursor {cursor}",
                        fixture.name
                    );
                    assert_eq!(
                        block.source_map.normalize_source(normalized, affinity),
                        Some(normalized),
                        "{}: {at} does not canonicalize stably at cursor {cursor}",
                        fixture.name
                    );
                }
            }
        }
        assert_eq!(
            saved, fixture.source,
            "{}: saved bytes changed with cursor at {cursor}",
            fixture.name
        );
    }
}
