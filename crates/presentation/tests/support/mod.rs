//! Shared Markdown feature fixture format (R3.25).
//!
//! A fixture describes one Markdown construct once, and [`verify`] checks every
//! contract that construct has to satisfy across the whole pipeline: the parse
//! tree shape, the derived marker ranges, the presented display kind and visual
//! text, SourceMap round-tripping, disclosure, and the bytes a save would write.
//!
//! Adding a Markdown feature means adding a fixture here rather than a bespoke
//! test per layer. The harness itself makes exactly the two calls `EditorView`
//! makes — a [`BlockIndex`] for the block boundaries, then [`present_block`] per
//! block — and has no per-feature branch, so a fixture that passes is also
//! evidence that the feature needs nothing from the UI crate.

use hane_document::{Bias, LineId, RopeBuffer, SourceOffset, SourceRange, TextBuffer};
use hane_markdown::{BlockIndex, MarkdownTree, NodeKind, parse_document};
use hane_presentation::{
    BlockKind, BlockLine, BlockWindow, VisualLine, block_line_context, block_line_span,
    present_block, trailing_blank_lines,
};

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

/// Presents the whole document the way `EditorView` does: block boundaries from
/// the index, then one [`present_block`] call per block. `cursor`, when given,
/// discloses markers on the line that owns it, using the editor's ownership rule
/// — a boundary belongs to the following line, and the document end to the last.
fn present_document(buffer: &RopeBuffer, cursor: Option<usize>) -> Vec<VisualLine> {
    let index = BlockIndex::from_buffer(buffer);
    let count = buffer.line_count();
    let ranges = (0..count)
        .map(|line| buffer.line_range(LineId(line)).expect("line in range"))
        .collect::<Vec<_>>();
    let texts = ranges
        .iter()
        .map(|range| buffer.text(*range).expect("line text"))
        .collect::<Vec<_>>();
    let mut presented = Vec::with_capacity(count);
    let mut line = 0;
    while line < count {
        let block = index
            .block_at(ranges[line].start)
            .expect("every line belongs to a block");
        let mut end = line + 1;
        while end < count && index.ordinal_at(ranges[end].start) == Some(block.ordinal) {
            end += 1;
        }
        let block_lines = (line..end)
            .map(|at| BlockLine {
                line: at,
                range: ranges[at],
                text: &texts[at],
                disclosure: cursor
                    .filter(|cursor| {
                        ranges[at].start.0 <= *cursor
                            && (*cursor < ranges[at].end.0
                                || (at + 1 == count && *cursor == ranges[at].end.0))
                    })
                    .map(SourceRange::empty),
            })
            .collect::<Vec<_>>();
        let span = block_line_span(buffer, &block).expect("block spans lines");
        let visual = present_block(
            &block,
            buffer.revision(),
            &BlockWindow {
                trailing_blank_lines: trailing_blank_lines(buffer, &span),
                span,
                lines: &block_lines,
            },
            LINE_HEIGHT,
        );
        assert_eq!(
            visual.lines.len(),
            block_lines.len(),
            "presented block covers every line it was given"
        );
        // Trailing blank lines tiling folded into the block are presented as
        // normal text; every other line carries the block's own context.
        assert!(
            visual.lines.iter().zip(&block_lines).all(|(visual, line)| {
                visual.context == block_line_context(block.kind) || line.text.trim().is_empty()
            }),
            "presented context follows the block kind"
        );
        presented.extend(visual.lines);
        line = end;
    }
    presented
}

fn verify_lines(fixture: &MarkdownFixture, buffer: &RopeBuffer) {
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
    let presented = present_document(buffer, None);
    for (line, block) in presented.iter().enumerate() {
        let range = buffer.line_range(LineId(line)).expect("line in range");
        let source = buffer.text(range).expect("line text");
        assert_eq!(
            block.kind, fixture.block_kinds[line],
            "{}: line {line} display kind",
            fixture.name
        );
        assert_eq!(
            block.visual_text, fixture.visual_lines[line],
            "{}: line {line} visual text",
            fixture.name
        );
        assert_eq!(
            segment_source(block, fixture.source),
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
fn segment_source(block: &VisualLine, document: &str) -> String {
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
    let lines = buffer.line_count();
    for cursor in (0..=fixture.source.len()).filter(|at| fixture.source.is_char_boundary(*at)) {
        let presented = present_document(buffer, Some(cursor));
        let mut saved = String::with_capacity(fixture.source.len());
        for (line, block) in presented.iter().enumerate() {
            let range = buffer.line_range(LineId(line)).expect("line in range");
            let owns_cursor = range.start.0 <= cursor
                && (cursor < range.end.0 || (line + 1 == lines && cursor == range.end.0));
            saved.push_str(&segment_source(block, fixture.source));

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
