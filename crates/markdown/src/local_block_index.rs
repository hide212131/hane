use crate::block_index::{self, BlockId, Confidence, IndexedBlock};
use crate::parser::parse_document;
use crate::syntax::NodeKind;
use hane_document::{LineId, Revision, RopeBuffer, SourceOffset, SourceRange, TextBuffer};

/// Lines scanned before the viewport when recovering block boundaries without a
/// published [`crate::BlockIndex`]. Bounds the fallback so visible parsing never depends
/// on total document size.
const LOCAL_BLOCK_LOOKBACK: usize = 2_048;

/// Block boundaries for one viewport, parsed from a bounded window.
///
/// Used only while no document-wide [`crate::BlockIndex`] is published — during the
/// first frames after a document is opened, and after an edit history gap drops
/// the published index. The window starts a fixed number of lines above the
/// viewport, so a construct that opens further above (a very long fenced block)
/// is not seen; that is the approximation the bound buys, and it is why every
/// block reported here is [`Confidence::Provisional`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalBlockIndex {
    revision: Revision,
    window: SourceRange,
    /// Block kinds with their absolute source ranges, line counts, and leading
    /// blank-line offsets, tiling `window`.
    blocks: Vec<(NodeKind, SourceRange, usize, usize)>,
}

impl LocalBlockIndex {
    fn indexed(&self, ordinal: usize) -> IndexedBlock {
        let (kind, source_range, line_count, leading_content_lines) = self.blocks[ordinal];
        IndexedBlock {
            ordinal,
            // Keyed by start offset rather than a counter: the window is
            // re-parsed on every scroll, and an offset keeps a block's cache
            // entry addressable across those re-parses.
            id: BlockId(source_range.start.0 as u64),
            kind,
            source_range,
            revision: self.revision,
            confidence: Confidence::Provisional,
            line_count,
            leading_content_lines,
        }
    }

    /// Block owning `offset`, or `None` when the offset lies outside the window.
    pub fn block_at(&self, offset: SourceOffset) -> Option<IndexedBlock> {
        let ordinal = self
            .blocks
            .partition_point(|(_, range, _, _)| range.end.0 <= offset.0)
            .min(self.blocks.len().checked_sub(1)?);
        let block = self.indexed(ordinal);
        (block.source_range.start <= offset && offset <= block.source_range.end).then_some(block)
    }

    /// Blocks overlapping `range`, in document order.
    pub fn blocks_in(&self, range: SourceRange) -> impl Iterator<Item = IndexedBlock> + '_ {
        let first = self
            .blocks
            .partition_point(|(_, span, _, _)| span.end.0 <= range.start.0);
        (first..self.blocks.len())
            .take_while(move |ordinal| {
                *ordinal == first || self.blocks[*ordinal].1.start.0 < range.end.0
            })
            .map(|ordinal| self.indexed(ordinal))
    }
}

/// Single bounded synchronous parse used only while no [`crate::BlockIndex`] is
/// published. Reads a window that starts `LOCAL_BLOCK_LOOKBACK` lines above
/// `visible` and ends one line below it, and tiles that window into blocks the
/// same way the formal index tiles the document. Never reads the whole document,
/// so it is safe on the visible-render path.
pub fn local_block_index(buffer: &RopeBuffer, visible: std::ops::Range<usize>) -> LocalBlockIndex {
    let line_count = buffer.line_count();
    let revision = buffer.revision();
    let empty = |window| LocalBlockIndex {
        revision,
        window,
        blocks: Vec::new(),
    };
    if line_count == 0 {
        return empty(SourceRange::empty(0));
    }
    let first_line = visible.start.saturating_sub(LOCAL_BLOCK_LOOKBACK);
    // One line past the viewport, so a construct whose closing line sits just
    // below the last visible line is still parsed as part of the same block.
    let last_line = visible.end.min(line_count - 1);
    let (Ok(start), Ok(end)) = (
        buffer.line_range(LineId(first_line)),
        buffer.line_range(LineId(last_line)),
    ) else {
        return empty(SourceRange::empty(0));
    };
    let window = SourceRange::new(start.start.0, end.end.0);
    let Ok(text) = buffer.text(window) else {
        return empty(window);
    };
    let parsed = parse_document(revision, window, &text);
    let mut offset = window.start.0;
    let blocks = block_index::tiled_blocks(&parsed.tree, window, &text)
        .into_iter()
        .map(|(kind, length, lines, leading_content_lines)| {
            let range = SourceRange::new(offset, offset + length);
            offset += length;
            (kind, range, lines, leading_content_lines)
        })
        .collect();
    LocalBlockIndex {
        revision,
        window,
        blocks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_index::BlockIndex;

    #[test]
    fn local_index_sees_a_fence_that_opens_inside_the_lookback_window() {
        let mut source = String::from("intro\n```rust\n");
        source.push_str(&"inside\n".repeat(2_000));
        source.push_str("```\nafter\n");
        let buffer = RopeBuffer::from_text(&source);
        let local = local_block_index(&buffer, 1_900..1_910);
        let line = buffer.line_range(LineId(1_905)).unwrap();
        assert_eq!(
            local.block_at(line.start).map(|block| block.kind),
            Some(NodeKind::CodeBlock),
            "a line inside the fence resolves to the code block"
        );
        assert!(
            local
                .block_at(line.start)
                .is_some_and(|block| block.confidence == Confidence::Provisional),
            "every locally parsed block is provisional"
        );
    }

    #[test]
    fn local_index_remembers_leading_blank_lines_for_the_first_fenced_block() {
        let buffer = RopeBuffer::from_text("\n\n```rust\ncode\n```\n");
        let local = local_block_index(&buffer, 2..3);
        let block = local
            .block_at(buffer.line_range(LineId(2)).unwrap().start)
            .expect("fenced block");
        assert_eq!(block.kind, NodeKind::CodeBlock);
        assert_eq!(block.leading_content_lines, 2);
    }

    #[test]
    fn local_index_resolves_gfm_pipe_tables() {
        let buffer = RopeBuffer::from_text(
            "before\n| Name | 値 |\n|:---|---:|\n| 羽 | 3 |\n| 鳥 | 4 |\n\nafter\n",
        );
        let local = local_block_index(&buffer, 0..buffer.line_count());
        let kind = |line: usize| {
            local
                .block_at(buffer.line_range(LineId(line)).unwrap().start)
                .map(|block| block.kind)
        };
        assert_eq!(kind(0), Some(NodeKind::Paragraph));
        for line in 1..=4 {
            assert_eq!(kind(line), Some(NodeKind::Table), "line {line} is table");
        }
        // The blank line closing the table belongs to the table block, the way
        // tiling assigns every blank run to the block above it.
        assert_eq!(kind(5), Some(NodeKind::Table));
        assert_eq!(kind(6), Some(NodeKind::Paragraph));
    }

    #[test]
    fn local_index_agrees_with_the_formal_index_on_the_visible_window() {
        let buffer = RopeBuffer::from_text(
            "intro\n```rust\ncode\n```\n| Name | 値 |\n|:---|---:|\n| 羽 | 3 |\ntail\n",
        );
        let formal = BlockIndex::from_buffer(&buffer);
        let local = local_block_index(&buffer, 0..buffer.line_count());
        for line in 0..buffer.line_count() {
            let at = buffer.line_range(LineId(line)).unwrap().start;
            assert_eq!(
                local.block_at(at).map(|block| block.kind),
                formal.block_at(at).map(|block| block.kind),
                "line {line} resolves to the same block kind"
            );
        }
    }

    #[test]
    fn local_index_covers_only_the_scanned_window() {
        let mut source = String::from("head\n");
        source.push_str(&"body\n".repeat(4_000));
        let buffer = RopeBuffer::from_text(&source);
        let local = local_block_index(&buffer, 3_500..3_510);
        let visible = buffer.line_range(LineId(3_505)).unwrap();
        assert_eq!(
            local.block_at(visible.start).map(|block| block.kind),
            Some(NodeKind::Paragraph)
        );
        // Far above the lookback window is not parsed at all.
        assert!(local.window.start.0 > 0);
        assert_eq!(local.block_at(SourceOffset(0)), None);
    }
}
