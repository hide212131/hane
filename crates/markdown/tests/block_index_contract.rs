//! Contract for the revision-tracked block index: what edits cost, what survives
//! them, and which parse result is allowed to reach the display.

use hane_document::{RopeBuffer, SourceOffset, SourceRange, TextBuffer};
use hane_markdown::{
    BlockIndex, BlockIndexState, BlockIndexUpdate, Confidence, IndexSource, NodeKind,
    PublishOutcome,
};

fn document_of(paragraphs: usize) -> RopeBuffer {
    let mut source = String::new();
    for paragraph in 0..paragraphs {
        source.push_str(&format!("paragraph {paragraph} with some words\n\n"));
    }
    RopeBuffer::from_text(&source)
}

fn type_at(buffer: &mut RopeBuffer, index: &mut BlockIndex, at: usize, text: &str) -> BlockIndexUpdate {
    let base = buffer.revision();
    buffer.edit(SourceRange::empty(at), text).unwrap();
    let deltas = buffer.deltas_since(base).unwrap();
    index.update(buffer, &deltas)
}

#[test]
fn local_editing_costs_the_same_in_a_small_and_a_large_document() {
    let mut costs = Vec::new();
    for paragraphs in [1_000, 20_000] {
        let mut buffer = document_of(paragraphs);
        let mut index = BlockIndex::from_buffer(&buffer);
        assert_eq!(index.len(), paragraphs);
        let middle = index.block(paragraphs / 2).unwrap().source_range.start.0;
        let mut reparsed = 0;
        for character in "hello".chars() {
            let update = type_at(&mut buffer, &mut index, middle + 10, &character.to_string());
            assert!(update.resynchronized);
            assert_eq!(update.invalidated_blocks, 0);
            reparsed += update.reparsed_bytes;
        }
        assert_eq!(index.len(), paragraphs, "typing did not change the structure");
        assert_eq!(index.covered_bytes(), buffer.len_bytes().0);
        costs.push(reparsed);
    }
    // Bytes re-parsed depend on the edited block's neighborhood, never on how
    // much document surrounds it.
    for cost in &costs {
        assert!(*cost < 1_000, "five keystrokes re-parsed {cost} bytes");
    }
    assert!(
        costs[1] < costs[0] * 2,
        "re-parsed bytes grew with the document: {costs:?}"
    );
}

#[test]
fn an_edit_whose_effect_reaches_far_marks_the_tail_provisional_instead_of_guessing() {
    let mut buffer = document_of(4_000);
    let mut index = BlockIndex::from_buffer(&buffer);

    // An opening fence at the top changes what every following line means.
    let update = type_at(&mut buffer, &mut index, 0, "```\n");
    assert!(!update.resynchronized);
    assert!(update.invalidated_blocks > 0);

    // The index keeps answering — offsets still resolve, and every byte is still
    // owned — but it reports the tail as provisional rather than claiming the
    // stale kinds are current.
    assert_eq!(index.covered_bytes(), buffer.len_bytes().0);
    let last = index.block(index.len() - 1).unwrap();
    assert_eq!(last.confidence, Confidence::Provisional);
    assert_eq!(last.source_range.end.0, buffer.len_bytes().0);
    assert!(index.block_at(SourceOffset(buffer.len_bytes().0 / 2)).is_some());
    assert!(index.has_provisional_blocks());

    // The formal parse that follows sees one code block swallowing the document.
    let formal = BlockIndex::from_buffer(&buffer);
    assert!(!formal.has_provisional_blocks());
    assert_eq!(formal.block(0).unwrap().kind, NodeKind::CodeBlock);
    assert_eq!(formal.block(0).unwrap().source_range.end.0, buffer.len_bytes().0);
}

#[test]
fn a_stale_parse_never_replaces_what_is_already_published() {
    let mut buffer = RopeBuffer::from_text("# title\n\nbody\n");
    let mut state = BlockIndexState::new();
    let started_early = BlockIndex::from_buffer(&buffer);

    buffer.edit(SourceRange::empty(9), "more ").unwrap();
    state.apply_edits(&buffer);
    assert_eq!(
        state.publish(
            BlockIndex::from_buffer(&buffer),
            IndexSource::Formal,
            &buffer
        ),
        PublishOutcome::Published
    );
    let published = state.index().unwrap().clone();

    // The job that started before the edit finishes last, and is refused.
    assert_eq!(
        state.publish(started_early, IndexSource::Formal, &buffer),
        PublishOutcome::Stale
    );
    assert_eq!(state.index(), Some(&published));
    assert_eq!(state.source(), Some(IndexSource::Formal));
    assert!(!state.needs_formal_parse(&buffer));
}

#[test]
fn block_identity_survives_editing_so_caches_stay_warm() {
    let mut buffer = document_of(200);
    let mut index = BlockIndex::from_buffer(&buffer);
    let before = index.blocks().map(|block| block.id).collect::<Vec<_>>();
    let target = index.block(100).unwrap();

    type_at(&mut buffer, &mut index, target.source_range.start.0 + 5, "日本語");

    let after = index.blocks().map(|block| block.id).collect::<Vec<_>>();
    assert_eq!(before, after, "editing inside a block keeps every block id");
    assert_eq!(index.block(100).unwrap().id, target.id);
    // Only the re-parsed window records the new revision; the rest of the
    // document keeps the revision it was last parsed at.
    assert_eq!(index.block(100).unwrap().revision, buffer.revision());
    assert_ne!(index.block(150).unwrap().revision, buffer.revision());
}

/// Line counts are what the block-driven height index is built from, so they
/// have to keep tiling the document through incremental updates — otherwise the
/// scroll range drifts away from the text.
#[test]
fn block_line_counts_tile_the_document_through_edits() {
    let mut buffer = RopeBuffer::from_text(
        "# title\n\npara one\ncontinued\n\n```rust\nlet x = 1;\nlet y = 2;\n```\n\ntail\n",
    );
    let mut index = BlockIndex::from_buffer(&buffer);

    let check = |index: &BlockIndex, buffer: &RopeBuffer, what: &str| {
        for block in index.blocks() {
            let first = buffer.line_for_offset(block.source_range.start).unwrap();
            let last = if block.source_range.end.0 >= buffer.len_bytes().0 {
                buffer.line_count() - 1
            } else {
                buffer
                    .line_for_offset(SourceOffset(block.source_range.end.0 - 1))
                    .unwrap()
                    .0
            };
            let span = last + 1 - first.0;
            // The block owning the document end also owns the empty last line a
            // trailing newline creates, which no block counts.
            let expected = if block.ordinal + 1 == index.len() {
                span - 1
            } else {
                span
            };
            assert_eq!(
                block.line_count, expected,
                "{what}: block {} covers {expected} lines",
                block.ordinal
            );
        }
        let counted: usize = index.blocks().map(|block| block.line_count).sum();
        assert_eq!(
            counted + 1,
            buffer.line_count(),
            "{what}: counts tile every line but the empty last one"
        );
    };
    check(&index, &buffer, "full parse");

    // Typing inside a block, then splitting one in two, then joining two back.
    let at = index.block(2).unwrap().source_range.start.0 + 4;
    type_at(&mut buffer, &mut index, at, "x");
    check(&index, &buffer, "after typing");
    type_at(&mut buffer, &mut index, at, "\n\n");
    check(&index, &buffer, "after splitting a block");
    let base = buffer.revision();
    buffer.edit(SourceRange::new(at, at + 2), "").unwrap();
    let deltas = buffer.deltas_since(base).unwrap();
    index.update(&buffer, &deltas);
    check(&index, &buffer, "after joining the blocks back");
}
