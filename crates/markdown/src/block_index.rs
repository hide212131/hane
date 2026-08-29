//! Revision-tracked Markdown block boundaries.
//!
//! [`BlockIndex`] answers the two questions the editor asks constantly — which
//! block owns a byte offset, and which source range a block ordinal covers — and
//! it survives edits without re-parsing the document: an edit is absorbed by the
//! block that contains it, only the affected window is re-parsed, and the result
//! is spliced back once the parse has re-synchronized with the untouched blocks
//! that follow it.
//!
//! Invariants:
//!
//! - Blocks *tile* the document. Block `i` runs from its own parsed start to the
//!   next block's start, so the blank lines between two blocks belong to the
//!   block above, block 0 starts at offset 0, and the last block ends at the
//!   document end. Every byte therefore belongs to exactly one block.
//! - The index holds *top-level* blocks only (the children of the parse tree
//!   root). Nesting stays in [`crate::MarkdownTree`].
//! - The index is empty exactly when the document contains no Markdown block at
//!   all, which for CommonMark means the document is blank.
//! - Block byte *lengths*, not absolute offsets, are what the index stores (see
//!   [`crate::block_store`]). An edit inside one block updates one length, and
//!   every later block moves implicitly, so typing never writes to blocks it did
//!   not touch.

use crate::block_store::BlockStore;
use crate::{MarkdownTree, NodeKind, parse_document};
use hane_document::{Revision, RevisionDelta, RopeBuffer, SourceOffset, SourceRange, TextBuffer};
use std::ops::Range;
use std::time::{Duration, Instant};

/// Bytes a single incremental update may re-parse while hunting for a
/// re-synchronization boundary before it gives up and invalidates the tail.
/// Bounds the input path: an edit that cannot re-synchronize costs a fixed
/// amount of work rather than an amount proportional to the document.
pub const RESYNC_BYTE_BUDGET: usize = 256 * 1024;

/// Blocks a single incremental update may pull into the re-parse window. Guards
/// documents made of very many tiny blocks, where the byte budget alone would
/// still allow a long walk.
pub const RESYNC_BLOCK_BUDGET: usize = 512;

/// Identifies a block across edits. A block keeps its id while its kind survives
/// a re-parse of the window it sits in, so caches keyed by block id stay warm
/// through ordinary typing.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BlockId(pub u64);

/// How much the index knows about a block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Confidence {
    /// Parsed, and re-synchronized with the blocks around it. Its kind and range
    /// are what a full parse of the document would produce.
    Formal,
    /// Its range was rebased through the edits, but the parse that produced its
    /// kind could not be re-synchronized. Usable for display, and replaced as
    /// soon as a formal parse arrives.
    Provisional,
}

/// One block as seen by a caller. Ranges are computed on lookup, so this is a
/// value rather than a borrow into the index.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IndexedBlock {
    pub ordinal: usize,
    pub id: BlockId,
    pub kind: NodeKind,
    pub source_range: SourceRange,
    /// Document revision this block's kind was parsed at. Older than the index
    /// revision means the block's text has not changed since.
    pub revision: Revision,
    pub confidence: Confidence,
    /// Physical source lines the block covers.
    ///
    /// Counted as the newlines in the block's bytes, plus one when the block does
    /// not end in a newline. Defined that way the counts are additive under
    /// concatenation, so merging blocks needs no re-count — and the empty last
    /// line of a document that ends in a newline belongs to no block, which is
    /// what `hane_presentation::block_heights` accounts for.
    pub line_count: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Entry {
    id: BlockId,
    kind: NodeKind,
    revision: Revision,
    lines: usize,
}

/// What one incremental update did. Reported so the caller can measure update
/// time, re-parsed bytes, and invalidated blocks without instrumenting the
/// index itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlockIndexUpdate {
    pub revision: Revision,
    pub reparsed_bytes: usize,
    /// First ordinal removed from the index by the window re-parse.
    pub first_replaced_block: usize,
    /// Blocks removed from the index and replaced by the window re-parse.
    pub replaced_blocks: usize,
    /// Blocks inserted at [`Self::first_replaced_block`].
    pub inserted_blocks: usize,
    /// Blocks after the window that were conservatively marked
    /// [`Confidence::Provisional`] because the parse could not re-synchronize.
    pub invalidated_blocks: usize,
    pub resynchronized: bool,
    pub elapsed: Duration,
}

/// One tiled block: its kind, its byte length, and the physical lines it covers.
pub(crate) type TiledBlock = (NodeKind, usize, usize);

/// Tiles one parsed slice into block spans covering `range` exactly: each
/// top-level block runs from its own start to the next block's start, the first
/// starts at `range.start`, and the last ends at `range.end`. Returns no block
/// when the slice parses to nothing, which is the caller's signal that the slice
/// is blank.
///
/// Line counts are taken here because this is the one place that already holds
/// the block's bytes; resolving them from the rope later costs a traversal per
/// block, which the input path cannot afford.
pub(crate) fn tiled_blocks(
    tree: &MarkdownTree,
    range: SourceRange,
    source: &str,
) -> Vec<TiledBlock> {
    let starts = tree
        .children(MarkdownTree::ROOT)
        .iter()
        .filter_map(|id| tree.node(*id))
        .map(|node| (node.kind, node.source_range.start.0))
        .collect::<Vec<_>>();
    starts
        .iter()
        .enumerate()
        .map(|(index, (kind, block_start))| {
            // Leading bytes before the first block belong to block 0.
            let start = if index == 0 {
                range.start.0
            } else {
                *block_start
            };
            let end = starts
                .get(index + 1)
                .map_or(range.end.0, |(_, next)| *next)
                .max(start);
            let slice = &source[start - range.start.0..end - range.start.0];
            let newlines = slice.bytes().filter(|byte| *byte == b'\n').count();
            let lines = newlines + usize::from(!slice.ends_with('\n'));
            (*kind, end - start, lines)
        })
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockIndex {
    revision: Revision,
    store: BlockStore<Entry>,
    next_id: u64,
    /// First ordinal of the conservatively invalidated tail, if any. Invalidation
    /// always covers a suffix, so one ordinal answers "is this block provisional"
    /// in constant time instead of writing a flag into every affected block.
    provisional_from: Option<usize>,
}

impl BlockIndex {
    /// Full parse. This is the formal path and is meant for a background job:
    /// cost is proportional to the document.
    pub fn build(revision: Revision, source: &str) -> Self {
        let range = SourceRange::new(0, source.len());
        let parsed = parse_document(revision, range, source);
        let blocks = tiled_blocks(&parsed.tree, range, source);
        let next_id = blocks.len() as u64;
        let store = BlockStore::new(blocks.into_iter().enumerate().map(
            |(index, (kind, length, lines))| {
                (
                    Entry {
                        id: BlockId(index as u64),
                        kind,
                        revision,
                        lines,
                    },
                    length,
                )
            },
        ));
        Self {
            revision,
            store,
            next_id,
            provisional_from: None,
        }
    }

    pub fn from_buffer(buffer: &RopeBuffer) -> Self {
        Self::build(buffer.revision(), &buffer.full_text())
    }

    /// Document revision the block spans are expressed in.
    pub fn revision(&self) -> Revision {
        self.revision
    }

    pub fn len(&self) -> usize {
        self.store.len()
    }

    pub fn is_empty(&self) -> bool {
        self.store.is_empty()
    }

    /// Total bytes owned by blocks. Equals the document length whenever the
    /// document holds at least one block.
    pub fn covered_bytes(&self) -> usize {
        self.store.total_bytes()
    }

    /// True when some block's kind could not be re-synchronized and only a full
    /// parse can restore it.
    pub fn has_provisional_blocks(&self) -> bool {
        self.provisional_from.is_some()
    }

    fn confidence(&self, ordinal: usize) -> Confidence {
        match self.provisional_from {
            Some(from) if ordinal >= from => Confidence::Provisional,
            _ => Confidence::Formal,
        }
    }

    fn span(&self, ordinal: usize) -> SourceRange {
        let start = self.store.start(ordinal);
        SourceRange::new(start, start + self.store.length(ordinal))
    }

    /// Source range of a block ordinal.
    pub fn block(&self, ordinal: usize) -> Option<IndexedBlock> {
        let (entry, length) = self.store.get(ordinal)?;
        let start = self.store.start(ordinal);
        Some(self.indexed(ordinal, entry, SourceRange::new(start, start + length)))
    }

    fn indexed(&self, ordinal: usize, entry: Entry, source_range: SourceRange) -> IndexedBlock {
        IndexedBlock {
            ordinal,
            id: entry.id,
            kind: entry.kind,
            source_range,
            revision: entry.revision,
            confidence: self.confidence(ordinal),
            line_count: entry.lines,
        }
    }

    /// Ordinal of the block owning `offset`: a Fenwick search plus a scan
    /// bounded by the chunk size. The document end belongs to the last block.
    pub fn ordinal_at(&self, offset: SourceOffset) -> Option<usize> {
        if offset.0 > self.covered_bytes() {
            return None;
        }
        self.store.ordinal_at(offset.0)
    }

    /// Block owning `offset`. See [`BlockIndex::ordinal_at`] for the cost.
    pub fn block_at(&self, offset: SourceOffset) -> Option<IndexedBlock> {
        self.block(self.ordinal_at(offset)?)
    }

    /// Every block in document order.
    pub fn blocks(&self) -> impl Iterator<Item = IndexedBlock> + '_ {
        self.blocks_from(0)
    }

    fn blocks_from(&self, ordinal: usize) -> impl Iterator<Item = IndexedBlock> + '_ {
        self.store
            .iter_from(ordinal)
            .map(move |(ordinal, entry, start, length)| {
                self.indexed(ordinal, entry, SourceRange::new(start, start + length))
            })
    }

    /// Blocks overlapping `range`, after a seek to the first one. An empty range
    /// yields the single block that owns the offset.
    pub fn blocks_in(&self, range: SourceRange) -> impl Iterator<Item = IndexedBlock> + '_ {
        let first = self.ordinal_at(range.start).unwrap_or(0);
        self.blocks_from(first).take_while(move |block| {
            block.ordinal == first || block.source_range.start.0 < range.end.0
        })
    }

    /// Rebases the index onto `buffer` and re-parses only what the edits could
    /// have changed. `deltas` must chain from this index's revision to the
    /// buffer's, as returned by [`RopeBuffer::deltas_since`].
    ///
    /// Non-intersecting blocks are rebased implicitly: an edit changes the byte
    /// length of the blocks it touches, and every later block's start moves with
    /// it. Only blocks the edits intersect are replaced.
    pub fn update(&mut self, buffer: &RopeBuffer, deltas: &[RevisionDelta]) -> BlockIndexUpdate {
        let started = Instant::now();
        let revision = buffer.revision();
        let finish = |index: &mut Self,
                      reparsed_bytes,
                      first_replaced_block,
                      replaced_blocks,
                      inserted_blocks,
                      invalidated_blocks,
                      resynchronized| {
            index.revision = revision;
            BlockIndexUpdate {
                revision,
                reparsed_bytes,
                first_replaced_block,
                replaced_blocks,
                inserted_blocks,
                invalidated_blocks,
                resynchronized,
                elapsed: started.elapsed(),
            }
        };
        if deltas.is_empty() {
            return finish(self, 0, 0, 0, 0, 0, true);
        }
        // Only a document with no block at all indexes to nothing, so this
        // rebuild parses a blank (hence tiny) document.
        if self.is_empty() {
            let rebuilt = Self::build(revision, &buffer.full_text());
            let bytes = rebuilt.covered_bytes();
            let blocks = rebuilt.len();
            *self = rebuilt;
            return finish(self, bytes, 0, 0, blocks, 0, true);
        }

        let Some((dirty_first, dirty_last)) = self.absorb_edits(deltas, revision) else {
            let replaced = self.len();
            let rebuilt = Self::build(revision, &buffer.full_text());
            let bytes = rebuilt.covered_bytes();
            let inserted = rebuilt.len();
            *self = rebuilt;
            return finish(self, bytes, 0, replaced, inserted, 0, true);
        };

        // The block before the dirty run joins the window: an edit can merge its
        // block with the one above (a deleted blank line, a new setext
        // underline), and that merge reaches at most one block back.
        let window_first = dirty_first.saturating_sub(1);
        let mut window_last = (dirty_last + 1).min(self.len() - 1);
        let mut reparsed_bytes = 0;
        loop {
            let window = SourceRange::new(
                self.span(window_first).start.0,
                self.span(window_last).end.0,
            );
            let Ok(text) = buffer.text(window) else {
                // The window is inside the buffer by construction; if reading it
                // ever fails, say so rather than leaving stale kinds marked
                // formal.
                self.provisional_from = Some(
                    self.provisional_from
                        .map_or(window_first, |at| at.min(window_first)),
                );
                let invalidated = self.len() - window_first;
                return finish(
                    self,
                    reparsed_bytes,
                    window_first,
                    0,
                    0,
                    invalidated,
                    false,
                );
            };
            reparsed_bytes += window.len_bytes();
            let parsed = parse_document(revision, window, &text);
            let blocks = tiled_blocks(&parsed.tree, window, &text);
            // Re-synchronized when the window's last parsed block lands exactly
            // on the boundary and kind the index already has for the untouched
            // block that closes the window. Everything after that boundary is
            // then still valid, because its text did not change. A window that
            // reaches the document end has nothing after it to disagree with.
            let tail_start = self.span(window_last).start.0;
            let tail_kind = self.store.get(window_last).map(|(entry, _)| entry.kind);
            let resynchronized = window.end.0 == buffer.len_bytes().0
                || blocks.last().is_some_and(|(kind, length, _)| {
                    Some(*kind) == tail_kind && window.end.0 - *length == tail_start
                });
            let can_grow = window_last + 1 < self.len()
                && reparsed_bytes < RESYNC_BYTE_BUDGET
                && window_last - window_first < RESYNC_BLOCK_BUDGET;
            if !resynchronized && can_grow {
                window_last += 1;
                continue;
            }
            let replaced = window_last + 1 - window_first;
            let inserted = self.splice_window(window_first..window_last + 1, &blocks, revision);
            let invalidated = if resynchronized {
                0
            } else {
                // The parse could not prove where the window ends, so every
                // block after it may have moved or changed kind. Keep their
                // rebased spans (offsets stay usable) but stop claiming their
                // kinds are current.
                let from = window_first + inserted;
                self.provisional_from = Some(self.provisional_from.map_or(from, |at| at.min(from)));
                self.len() - from
            };
            return finish(
                self,
                reparsed_bytes,
                window_first,
                replaced,
                inserted,
                invalidated,
                resynchronized,
            );
        }
    }

    /// Applies each edit's byte delta to the blocks it intersects and returns the
    /// ordinal range that needs re-parsing. Returns `None` when an edit falls
    /// outside the indexed bytes, which means the index no longer describes this
    /// document and the caller must rebuild.
    fn absorb_edits(
        &mut self,
        deltas: &[RevisionDelta],
        revision: Revision,
    ) -> Option<(usize, usize)> {
        let mut dirty: Option<(usize, usize)> = None;
        for delta in deltas {
            let edit = delta.edited_source_range_before;
            if edit.end.0 > self.covered_bytes() {
                return None;
            }
            let first = self.store.ordinal_at(edit.start.0)?;
            let last = self.store.ordinal_at(edit.end.0)?;
            let start = self.span(first).start.0;
            let end = self.span(last).end.0;
            let combined = (end - start).checked_add_signed(delta.byte_delta)?;
            self.merge_run(first, last, combined, revision);
            // Merging collapsed `first..=last` into `first`, so ordinals recorded
            // by earlier deltas move down by the blocks that disappeared.
            let remap = |ordinal: usize| {
                if ordinal <= first {
                    ordinal
                } else if ordinal <= last {
                    first
                } else {
                    ordinal - (last - first)
                }
            };
            dirty = Some(match dirty {
                None => (first, first),
                Some((low, high)) => (remap(low).min(first), remap(high).max(first)),
            });
        }
        dirty
    }

    /// Collapses `first..=last` into one entry of `length` bytes. The common case
    /// (an edit inside a single block) touches one length and nothing else.
    fn merge_run(&mut self, first: usize, last: usize, length: usize, revision: Revision) {
        let Some((mut entry, _)) = self.store.get(first) else {
            return;
        };
        entry.revision = revision;
        if first == last {
            self.store.set_payload(first, entry);
            self.store.set_length(first, length);
            return;
        }
        // Line counts are additive under concatenation, so a merged run carries
        // the sum. The edit itself may have added or removed newlines; the window
        // re-parse that follows in the same update replaces the count exactly.
        entry.lines = (first..=last)
            .filter_map(|ordinal| self.store.get(ordinal))
            .map(|(entry, _)| entry.lines)
            .sum();
        self.store.set_payload(first, entry);
        self.store.splice(first..last + 1, &[(entry, length)]);
        if let Some(from) = self.provisional_from {
            self.provisional_from = Some(if from <= first {
                from
            } else if from <= last {
                first
            } else {
                from - (last - first)
            });
        }
    }

    /// Replaces the entries covering `window` with freshly parsed blocks and
    /// returns how many were inserted. Ids are carried over from the front and
    /// the back while kinds still line up, so an edit inside a block leaves that
    /// block's identity — and any cache keyed by it — intact.
    fn splice_window(
        &mut self,
        window: Range<usize>,
        blocks: &[TiledBlock],
        revision: Revision,
    ) -> usize {
        let window_start = self.store.start(window.start);
        let existing = self
            .store
            .iter_from(window.start)
            .take(window.len())
            .collect::<Vec<_>>();
        let previous = existing
            .iter()
            .map(|(_, entry, _, _)| *entry)
            .collect::<Vec<_>>();
        // Old and new blocks tile the same bytes, so their offsets within the
        // window are directly comparable. Match from both ends while a block
        // still starts (or ends) where it did and kept its kind: inserting a
        // block in the middle then leaves the blocks around it untouched.
        let old_offsets = existing
            .iter()
            .map(|(_, _, start, length)| (start - window_start, start + length - window_start))
            .collect::<Vec<_>>();
        let new_offsets = blocks
            .iter()
            .scan(0, |start, (_, length, _)| {
                let span = (*start, *start + length);
                *start = span.1;
                Some(span)
            })
            .collect::<Vec<_>>();
        let mut ids = vec![None; blocks.len()];
        let mut front = 0;
        while front < blocks.len()
            && front < previous.len()
            && blocks[front].0 == previous[front].kind
            && new_offsets[front].0 == old_offsets[front].0
        {
            ids[front] = Some(previous[front].id);
            front += 1;
        }
        let mut back = 0;
        while back + front < blocks.len()
            && back + front < previous.len()
            && blocks[blocks.len() - 1 - back].0 == previous[previous.len() - 1 - back].kind
            && new_offsets[blocks.len() - 1 - back].1 == old_offsets[previous.len() - 1 - back].1
        {
            ids[blocks.len() - 1 - back] = Some(previous[previous.len() - 1 - back].id);
            back += 1;
        }
        let entries = blocks
            .iter()
            .zip(&ids)
            .map(|((kind, _, lines), id)| Entry {
                id: id.unwrap_or_else(|| {
                    let id = BlockId(self.next_id);
                    self.next_id += 1;
                    id
                }),
                kind: *kind,
                revision,
                lines: *lines,
            })
            .collect::<Vec<_>>();
        if blocks.is_empty() {
            // A window that parses to nothing is blank. Its bytes join the block
            // above, keeping the tiling intact; with no block above, the document
            // holds no block at all.
            let bytes: usize = existing.iter().map(|(_, _, _, length)| length).sum();
            let lines: usize = existing.iter().map(|(_, entry, _, _)| entry.lines).sum();
            self.store.splice(window.clone(), &[]);
            if window.start > 0 {
                let above = window.start - 1;
                self.store
                    .set_length(above, self.store.length(above) + bytes);
                if let Some((mut entry, _)) = self.store.get(above) {
                    entry.lines += lines;
                    self.store.set_payload(above, entry);
                }
            }
        } else if blocks.len() == window.len() {
            // Same block count: rewrite the slots in place, so an edit that does
            // not change the window's structure never re-chunks the store.
            for (offset, (entry, (_, length, _))) in entries.iter().zip(blocks).enumerate() {
                self.store.set_payload(window.start + offset, *entry);
                self.store.set_length(window.start + offset, *length);
            }
        } else {
            let items = entries
                .into_iter()
                .zip(blocks.iter().map(|(_, length, _)| *length))
                .collect::<Vec<_>>();
            self.store.splice(window.clone(), &items);
        }
        if let Some(from) = self.provisional_from
            && from >= window.start
        {
            self.provisional_from = Some(if from < window.end {
                window.start
            } else {
                from + blocks.len() - window.len()
            });
        }
        blocks.len()
    }
}

/// Where a published [`BlockIndex`] came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexSource {
    /// A full parse of one document revision: authoritative for that revision.
    Formal,
    /// Incremental updates around the edit site, or a formal parse that has been
    /// rebased onto later edits. Correct to display, but a formal parse of the
    /// same revision supersedes it.
    Provisional,
}

/// What [`BlockIndexState::publish`] did with a candidate index.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublishOutcome {
    /// Accepted; the candidate already described the buffer's revision.
    Published,
    /// Accepted after re-parsing the edits made since the candidate started.
    Rebased(BlockIndexUpdate),
    /// Rejected: older than what is already published, so publishing it would
    /// overwrite newer knowledge with a stale parse.
    Stale,
    /// Rejected: same revision, but no more authoritative than what is published.
    NotMoreAuthoritative,
    /// Rejected: the buffer no longer remembers the edits since the candidate's
    /// revision, so it cannot be rebased onto the current document.
    HistoryUnavailable,
}

/// Owns the published [`BlockIndex`] and the rule for which result may replace
/// it. Both the background full parse and the incremental updates on the input
/// path go through here, so "a stale result never overwrites the display" is one
/// rule in one place rather than a revision check at each call site.
///
/// Priority, highest first:
///
/// 1. a newer document revision beats an older one;
/// 2. at the same revision, [`IndexSource::Formal`] beats
///    [`IndexSource::Provisional`], and a provisional result never replaces a
///    formal one;
/// 3. a candidate older than what is published is rejected outright;
/// 4. an accepted candidate is first brought to the buffer's current revision;
///    one that cannot be (its edits are no longer in the buffer's history) is
///    rejected rather than published stale.
#[derive(Clone, Debug, Default)]
pub struct BlockIndexState {
    published: Option<BlockIndex>,
    source: Option<IndexSource>,
}

impl BlockIndexState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn index(&self) -> Option<&BlockIndex> {
        self.published.as_ref()
    }

    pub fn source(&self) -> Option<IndexSource> {
        self.source
    }

    /// True while the published index is missing, behind the buffer, or carries
    /// blocks a full parse still has to confirm.
    pub fn needs_formal_parse(&self, buffer: &RopeBuffer) -> bool {
        self.published.as_ref().is_none_or(|index| {
            index.revision() != buffer.revision()
                || index.has_provisional_blocks()
                || self.source != Some(IndexSource::Formal)
        })
    }

    /// Brings the published index up to the buffer's revision by re-parsing only
    /// the edited window. Intended for the input path. Drops the index if the
    /// buffer no longer remembers the edits, leaving the caller to re-parse.
    pub fn apply_edits(&mut self, buffer: &RopeBuffer) -> Option<BlockIndexUpdate> {
        let index = self.published.as_mut()?;
        if index.revision() == buffer.revision() {
            return None;
        }
        let Ok(deltas) = buffer.deltas_since(index.revision()) else {
            self.published = None;
            self.source = None;
            return None;
        };
        let update = index.update(buffer, &deltas);
        self.source = Some(IndexSource::Provisional);
        Some(update)
    }

    /// Applies the publish priority to a finished parse.
    pub fn publish(
        &mut self,
        mut candidate: BlockIndex,
        source: IndexSource,
        buffer: &RopeBuffer,
    ) -> PublishOutcome {
        if let Some(published) = &self.published {
            if candidate.revision() < published.revision() {
                return PublishOutcome::Stale;
            }
            if candidate.revision() == published.revision()
                && !(source == IndexSource::Formal && self.source == Some(IndexSource::Provisional))
            {
                return PublishOutcome::NotMoreAuthoritative;
            }
        }
        if candidate.revision() == buffer.revision() {
            self.published = Some(candidate);
            self.source = Some(source);
            return PublishOutcome::Published;
        }
        let Ok(deltas) = buffer.deltas_since(candidate.revision()) else {
            return PublishOutcome::HistoryUnavailable;
        };
        let update = candidate.update(buffer, &deltas);
        self.published = Some(candidate);
        // Rebasing re-parses only the edited windows, so the result is no longer
        // a full parse of the current revision.
        self.source = Some(IndexSource::Provisional);
        PublishOutcome::Rebased(update)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hane_document::TextBuffer;

    /// Kind and range of every block, for comparing an incrementally updated
    /// index against a full parse of the same text.
    fn structure(index: &BlockIndex) -> Vec<(NodeKind, Range<usize>)> {
        index
            .blocks()
            .map(|block| (block.kind, block.source_range.as_usize()))
            .collect()
    }

    fn edit(buffer: &mut RopeBuffer, range: SourceRange, replacement: &str) -> Vec<RevisionDelta> {
        let base = buffer.revision();
        buffer.edit(range, replacement).unwrap();
        buffer.deltas_since(base).unwrap()
    }

    fn apply(index: &mut BlockIndex, buffer: &mut RopeBuffer, at: usize, replacement: &str) -> BlockIndexUpdate {
        let deltas = edit(buffer, SourceRange::empty(at), replacement);
        index.update(buffer, &deltas)
    }

    #[test]
    fn blocks_tile_the_document_and_resolve_by_offset() {
        let source = "# head\n\npara one\ncont\n\n\n- a\n- b\n\n```rust\nx\n```\n\n> q\n\ntail";
        let index = BlockIndex::build(Revision(3), source);
        assert_eq!(index.covered_bytes(), source.len());
        let mut expected_start = 0;
        for block in index.blocks() {
            assert_eq!(block.source_range.start.0, expected_start);
            expected_start = block.source_range.end.0;
            assert_eq!(block.revision, Revision(3));
            assert_eq!(block.confidence, Confidence::Formal);
        }
        assert_eq!(expected_start, source.len());
        // The blank lines between two blocks belong to the block above.
        assert_eq!(
            index.block_at(SourceOffset(7)).map(|block| block.kind),
            Some(NodeKind::Heading(1))
        );
        assert_eq!(
            index.block_at(SourceOffset(9)).map(|block| block.kind),
            Some(NodeKind::Paragraph)
        );
        assert_eq!(
            index.block_at(SourceOffset(34)).map(|block| block.kind),
            Some(NodeKind::CodeBlock)
        );
        // The document end belongs to the last block; past it is out of range.
        assert_eq!(
            index.block_at(SourceOffset(source.len())).map(|b| b.ordinal),
            Some(index.len() - 1)
        );
        assert_eq!(index.block_at(SourceOffset(source.len() + 1)), None);
        // Ordinal to range agrees with the iteration order.
        for (ordinal, block) in index.blocks().enumerate() {
            assert_eq!(index.block(ordinal), Some(block));
        }
        assert_eq!(index.block(index.len()), None);
    }

    #[test]
    fn typing_inside_a_block_reparses_only_its_neighborhood() {
        let mut source = String::new();
        for line in 0..20_000 {
            source.push_str(&format!("paragraph {line}\n\n"));
        }
        let mut buffer = RopeBuffer::from_text(&source);
        let mut index = BlockIndex::from_buffer(&buffer);
        assert_eq!(index.len(), 20_000);
        let ids = index.blocks().map(|block| block.id).collect::<Vec<_>>();
        let target = index.block(10_000).unwrap();
        let update = apply(&mut index, &mut buffer, target.source_range.start.0 + 4, "!");

        assert!(update.resynchronized);
        assert_eq!(update.invalidated_blocks, 0);
        assert_eq!(update.first_replaced_block, 9_999);
        assert_eq!(update.replaced_blocks, 3);
        assert_eq!(update.inserted_blocks, 3);
        assert!(
            update.reparsed_bytes < 200,
            "one keystroke re-parsed {} bytes",
            update.reparsed_bytes
        );
        assert_eq!(index.len(), 20_000);
        assert_eq!(index.covered_bytes(), buffer.len_bytes().0);
        // Untouched blocks keep both their identity and their parse revision.
        assert_eq!(
            index.blocks().map(|block| block.id).collect::<Vec<_>>(),
            ids
        );
        assert_eq!(index.block(0).unwrap().revision, Revision(0));
        assert_eq!(index.block(10_000).unwrap().revision, buffer.revision());
        assert_eq!(index.revision(), buffer.revision());
        assert_eq!(structure(&index), structure(&BlockIndex::from_buffer(&buffer)));
    }

    #[test]
    fn splitting_and_merging_blocks_keeps_the_index_equal_to_a_full_parse() {
        let mut buffer = RopeBuffer::from_text("alpha\n\nbravo\n\ncharlie\n");
        let mut index = BlockIndex::from_buffer(&buffer);
        let last_id = index.block(2).unwrap().id;

        // Split "bravo" into two paragraphs.
        let update = apply(&mut index, &mut buffer, 12, "\n\nsplit");
        assert!(update.resynchronized);
        assert_eq!(update.first_replaced_block, 0);
        assert_eq!(update.replaced_blocks, 3);
        assert_eq!(update.inserted_blocks, 4);
        assert_eq!(index.len(), 4);
        assert_eq!(structure(&index), structure(&BlockIndex::from_buffer(&buffer)));
        // The block after the split is untouched, so it keeps its id.
        assert_eq!(index.block(3).unwrap().id, last_id);

        // Delete the blank line that separates the first two paragraphs.
        let deltas = edit(&mut buffer, SourceRange::new(5, 7), "\n");
        let update = index.update(&buffer, &deltas);
        assert!(update.resynchronized);
        assert_eq!(index.len(), 3);
        assert_eq!(structure(&index), structure(&BlockIndex::from_buffer(&buffer)));
        assert_eq!(index.block(2).unwrap().id, last_id);
    }

    #[test]
    fn an_unterminated_fence_invalidates_the_tail_conservatively() {
        let mut source = String::from("intro\n\n");
        for line in 0..2_000 {
            source.push_str(&format!("paragraph {line}\n\n"));
        }
        let mut buffer = RopeBuffer::from_text(&source);
        let mut index = BlockIndex::from_buffer(&buffer);
        let blocks_before = index.len();

        // An opening fence with no closing fence swallows everything after it,
        // so the parse can never re-synchronize with the blocks that follow.
        let update = apply(&mut index, &mut buffer, 5, "\n\n```");
        assert!(!update.resynchronized);
        assert!(update.invalidated_blocks > 0);
        assert!(
            update.reparsed_bytes <= RESYNC_BYTE_BUDGET + buffer.len_bytes().0 / 4,
            "gave up after {} bytes",
            update.reparsed_bytes
        );
        assert!(index.has_provisional_blocks());
        // Offsets still resolve, and the tail is honest about being provisional.
        assert_eq!(index.covered_bytes(), buffer.len_bytes().0);
        assert!(index.len() <= blocks_before + 1);
        assert_eq!(
            index.block(index.len() - 1).unwrap().confidence,
            Confidence::Provisional
        );
        assert_eq!(index.block(0).unwrap().confidence, Confidence::Formal);

        // A full parse of the same revision restores formal knowledge.
        let formal = BlockIndex::from_buffer(&buffer);
        assert!(!formal.has_provisional_blocks());
    }

    #[test]
    fn closing_a_fence_resynchronizes_the_following_blocks() {
        let mut buffer = RopeBuffer::from_text("```\ncode\n\nalpha\n\nbravo\n");
        let mut index = BlockIndex::from_buffer(&buffer);
        assert_eq!(index.len(), 1, "the unterminated fence owns the document");

        let update = apply(&mut index, &mut buffer, 9, "```\n");
        assert!(update.resynchronized);
        assert_eq!(structure(&index), structure(&BlockIndex::from_buffer(&buffer)));
        assert!(!index.has_provisional_blocks());
    }

    #[test]
    fn a_blank_document_holds_no_block_and_recovers_when_typed_into() {
        let mut buffer = RopeBuffer::from_text("\n\n");
        let mut index = BlockIndex::from_buffer(&buffer);
        assert!(index.is_empty());
        assert_eq!(index.block_at(SourceOffset(0)), None);

        let update = apply(&mut index, &mut buffer, 0, "# heading\n");
        assert!(update.resynchronized);
        assert_eq!(index.len(), 1);
        assert_eq!(index.covered_bytes(), buffer.len_bytes().0);
        assert_eq!(index.block(0).unwrap().kind, NodeKind::Heading(1));

        // Deleting it all returns the index to empty without losing the tiling.
        let whole = SourceRange::new(0, buffer.len_bytes().0);
        let deltas = edit(&mut buffer, whole, "");
        index.update(&buffer, &deltas);
        assert!(index.is_empty());
        assert_eq!(index.covered_bytes(), 0);
    }

    #[test]
    fn incremental_updates_match_a_full_parse_across_mixed_edits() {
        let source = "# title\n\nintro paragraph\n\n- one\n- two\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n> quote\n\nlast\n";
        let mut buffer = RopeBuffer::from_text(source);
        let mut index = BlockIndex::from_buffer(&buffer);
        let edits: [(usize, usize, &str); 7] = [
            (9, 9, "**bold** "),
            (0, 1, "###"),
            (30, 30, "\n- three"),
            (5, 5, "日本語"),
            (2, 4, ""),
            (60, 60, "\n\nnew paragraph\n"),
            (0, 0, "front matter\n\n"),
        ];
        for (start, end, replacement) in edits {
            let start = start.min(buffer.len_bytes().0);
            let end = end.clamp(start, buffer.len_bytes().0);
            let deltas = edit(&mut buffer, SourceRange::new(start, end), replacement);
            let update = index.update(&buffer, &deltas);
            assert_eq!(index.revision(), buffer.revision());
            assert_eq!(index.covered_bytes(), buffer.len_bytes().0);
            if update.resynchronized && !index.has_provisional_blocks() {
                assert_eq!(
                    structure(&index),
                    structure(&BlockIndex::from_buffer(&buffer)),
                    "diverged after replacing {start}..{end} with {replacement:?}"
                );
            }
        }
    }

    #[test]
    fn batched_edits_are_absorbed_in_one_update() {
        let mut buffer = RopeBuffer::from_text("alpha\n\nbravo\n\ncharlie\n");
        let mut index = BlockIndex::from_buffer(&buffer);
        let base = buffer.revision();
        buffer.edit(SourceRange::empty(5), "!").unwrap();
        buffer.edit(SourceRange::empty(6), "?").unwrap();
        let deltas = buffer.deltas_since(base).unwrap();
        assert_eq!(deltas.len(), 2);
        let update = index.update(&buffer, &deltas);
        assert!(update.resynchronized);
        assert_eq!(index.revision(), buffer.revision());
        assert_eq!(structure(&index), structure(&BlockIndex::from_buffer(&buffer)));
    }

    #[test]
    fn blocks_in_range_returns_only_overlapping_blocks() {
        let source = "alpha\n\nbravo\n\ncharlie\n";
        let index = BlockIndex::build(Revision(0), source);
        let middle = index.block(1).unwrap().source_range;
        let overlapping = index
            .blocks_in(middle)
            .map(|block| block.ordinal)
            .collect::<Vec<_>>();
        assert_eq!(overlapping, vec![1]);
        assert_eq!(
            index
                .blocks_in(SourceRange::new(0, source.len()))
                .count(),
            index.len()
        );
        assert_eq!(
            index
                .blocks_in(SourceRange::empty(middle.start.0 + 1))
                .map(|block| block.ordinal)
                .collect::<Vec<_>>(),
            vec![1]
        );
    }

    #[test]
    fn publish_prefers_newer_revisions_and_formal_parses() {
        let mut buffer = RopeBuffer::from_text("alpha\n\nbravo\n");
        let mut state = BlockIndexState::new();
        assert!(state.needs_formal_parse(&buffer));

        let formal = BlockIndex::from_buffer(&buffer);
        assert_eq!(
            state.publish(formal.clone(), IndexSource::Formal, &buffer),
            PublishOutcome::Published
        );
        assert!(!state.needs_formal_parse(&buffer));

        // Same revision, no more authoritative: rejected either way round.
        assert_eq!(
            state.publish(formal.clone(), IndexSource::Provisional, &buffer),
            PublishOutcome::NotMoreAuthoritative
        );
        assert_eq!(
            state.publish(formal.clone(), IndexSource::Formal, &buffer),
            PublishOutcome::NotMoreAuthoritative
        );

        // An edit makes the published index provisional, and a formal parse of
        // that same revision may then replace it.
        buffer.edit(SourceRange::empty(0), "# ").unwrap();
        let update = state.apply_edits(&buffer).expect("edits are applied");
        assert!(update.resynchronized);
        assert_eq!(state.source(), Some(IndexSource::Provisional));
        assert!(state.needs_formal_parse(&buffer));
        assert_eq!(
            state.index().unwrap().block(0).unwrap().kind,
            NodeKind::Heading(1)
        );
        assert_eq!(
            state.publish(BlockIndex::from_buffer(&buffer), IndexSource::Formal, &buffer),
            PublishOutcome::Published
        );
        assert_eq!(state.source(), Some(IndexSource::Formal));

        // A parse that started before the last edit is stale and never publishes.
        assert_eq!(
            state.publish(formal, IndexSource::Formal, &buffer),
            PublishOutcome::Stale
        );
    }

    #[test]
    fn a_background_parse_that_finished_late_is_rebased_onto_the_edits() {
        let mut buffer = RopeBuffer::from_text("alpha\n\nbravo\n\ncharlie\n");
        let mut state = BlockIndexState::new();
        // The background job starts here and finishes after two more edits.
        let candidate = BlockIndex::from_buffer(&buffer);
        buffer.edit(SourceRange::empty(0), "# ").unwrap();
        buffer.edit(SourceRange::empty(9), "!").unwrap();

        let outcome = state.publish(candidate, IndexSource::Formal, &buffer);
        assert!(matches!(outcome, PublishOutcome::Rebased(_)));
        let index = state.index().unwrap();
        assert_eq!(index.revision(), buffer.revision());
        assert_eq!(index.covered_bytes(), buffer.len_bytes().0);
        assert_eq!(structure(index), structure(&BlockIndex::from_buffer(&buffer)));
        // Rebasing re-parsed only the edited windows, so it is not a formal
        // parse of the current revision.
        assert_eq!(state.source(), Some(IndexSource::Provisional));
    }
}
