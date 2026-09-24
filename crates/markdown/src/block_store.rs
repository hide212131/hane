//! Two-level storage for a tiling sequence of blocks.
//!
//! Blocks live in chunks of a few hundred, and only the per-chunk byte and block
//! totals go into Fenwick trees. That keeps all three costs the index needs off
//! the document's size:
//!
//! - an edit inside one block updates one length and one chunk total, which is
//!   `O(log chunks)`;
//! - an edit that adds or removes blocks rewrites one or two chunks and the
//!   chunk totals, so it moves at most `CHUNK_TARGET` blocks rather than every
//!   block after the edit;
//! - offset and ordinal lookups take a Fenwick search plus a scan bounded by the
//!   chunk size.
//!
//! A flat `Vec` with one Fenwick node per block answers lookups just as fast but
//! pays a document-length memmove and tree rebuild every time the block count
//! changes, which is exactly what pressing Return does.

/// Blocks per chunk when (re)chunking. Chunks only ever change size through the
/// re-chunking a structural edit performs, so there is no rebalancing pass. The
/// size trades the in-chunk scan that lookups pay against the number of chunk
/// totals a structural edit rebuilds; measured on a 500k-block document, 128
/// keeps both in the tens of microseconds.
const CHUNK_TARGET: usize = 128;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct SumTree {
    tree: Vec<usize>,
}

impl SumTree {
    fn build(values: impl Iterator<Item = usize>) -> Self {
        let mut tree = Vec::new();
        tree.push(0);
        tree.extend(values);
        for index in 1..tree.len() {
            let parent = index + (index & index.wrapping_neg());
            if parent < tree.len() {
                tree[parent] += tree[index];
            }
        }
        Self { tree }
    }

    fn prefix(&self, exclusive_end: usize) -> usize {
        let mut index = exclusive_end.min(self.tree.len().saturating_sub(1));
        let mut sum = 0;
        while index > 0 {
            sum += self.tree[index];
            index &= index - 1;
        }
        sum
    }

    fn total(&self) -> usize {
        self.prefix(self.tree.len().saturating_sub(1))
    }

    fn add(&mut self, index: usize, delta: isize) {
        let mut node = index + 1;
        while node < self.tree.len() {
            self.tree[node] = self.tree[node].wrapping_add_signed(delta);
            node += node & node.wrapping_neg();
        }
    }

    /// Highest index whose prefix sum is at most `target`, with that prefix sum.
    fn search(&self, target: usize) -> (usize, usize) {
        let mut index = 0usize;
        let mut sum = 0usize;
        let mut step = 1usize;
        while step << 1 < self.tree.len() {
            step <<= 1;
        }
        while step > 0 {
            let next = index + step;
            if next < self.tree.len() && sum + self.tree[next] <= target {
                index = next;
                sum += self.tree[next];
            }
            step >>= 1;
        }
        (index, sum)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Chunk<T> {
    items: Vec<(T, usize)>,
    bytes: usize,
}

impl<T: Copy> Chunk<T> {
    fn new(items: &[(T, usize)]) -> Self {
        Self {
            items: items.to_vec(),
            bytes: items.iter().map(|(_, length)| *length).sum(),
        }
    }
}

/// A sequence of `(payload, byte length)` blocks that tile a document, indexed
/// by ordinal and by byte offset.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BlockStore<T> {
    chunks: Vec<Chunk<T>>,
    bytes: SumTree,
    counts: SumTree,
}

impl<T: Copy> BlockStore<T> {
    pub(crate) fn new(items: impl IntoIterator<Item = (T, usize)>) -> Self {
        let items = items.into_iter().collect::<Vec<_>>();
        Self::from_chunks(items.chunks(CHUNK_TARGET).map(Chunk::new).collect())
    }

    fn from_chunks(chunks: Vec<Chunk<T>>) -> Self {
        let bytes = SumTree::build(chunks.iter().map(|chunk| chunk.bytes));
        let counts = SumTree::build(chunks.iter().map(|chunk| chunk.items.len()));
        Self {
            chunks,
            bytes,
            counts,
        }
    }

    fn retree(&mut self) {
        self.bytes = SumTree::build(self.chunks.iter().map(|chunk| chunk.bytes));
        self.counts = SumTree::build(self.chunks.iter().map(|chunk| chunk.items.len()));
    }

    pub(crate) fn len(&self) -> usize {
        self.counts.total()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub(crate) fn total_bytes(&self) -> usize {
        self.bytes.total()
    }

    /// Chunk and slot holding `ordinal`, or `None` past the last block.
    fn locate(&self, ordinal: usize) -> Option<(usize, usize)> {
        if ordinal >= self.len() {
            return None;
        }
        let (chunk, before) = self.counts.search(ordinal);
        Some((chunk, ordinal - before))
    }

    /// Chunk and slot to insert at, where the position just past the last block
    /// is the end slot of the last chunk.
    fn locate_insert(&self, ordinal: usize) -> (usize, usize) {
        self.locate(ordinal).unwrap_or_else(|| {
            self.chunks
                .last()
                .map_or((0, 0), |chunk| (self.chunks.len() - 1, chunk.items.len()))
        })
    }

    pub(crate) fn get(&self, ordinal: usize) -> Option<(T, usize)> {
        let (chunk, slot) = self.locate(ordinal)?;
        Some(self.chunks[chunk].items[slot])
    }

    pub(crate) fn length(&self, ordinal: usize) -> usize {
        self.get(ordinal).map_or(0, |(_, length)| length)
    }

    /// Byte offset the block starts at: chunk totals from the tree, then a scan
    /// bounded by the chunk size.
    pub(crate) fn start(&self, ordinal: usize) -> usize {
        let Some((chunk, slot)) = self.locate(ordinal) else {
            return self.total_bytes();
        };
        self.bytes.prefix(chunk)
            + self.chunks[chunk].items[..slot]
                .iter()
                .map(|(_, length)| *length)
                .sum::<usize>()
    }

    /// Ordinal of the block owning `offset`; the document end belongs to the
    /// last block.
    pub(crate) fn ordinal_at(&self, offset: usize) -> Option<usize> {
        if self.is_empty() {
            return None;
        }
        let target = offset.min(self.total_bytes());
        let (chunk, _) = self.bytes.search(target);
        let chunk = chunk.min(self.chunks.len() - 1);
        let ordinal = self.counts.prefix(chunk);
        let mut sum = self.bytes.prefix(chunk);
        let items = &self.chunks[chunk].items;
        for (slot, (_, length)) in items.iter().enumerate() {
            if target < sum + length {
                return Some(ordinal + slot);
            }
            sum += length;
        }
        Some(ordinal + items.len().saturating_sub(1))
    }

    pub(crate) fn set_length(&mut self, ordinal: usize, length: usize) {
        let Some((chunk, slot)) = self.locate(ordinal) else {
            return;
        };
        let delta = length as isize - self.chunks[chunk].items[slot].1 as isize;
        self.chunks[chunk].items[slot].1 = length;
        self.chunks[chunk].bytes = self.chunks[chunk].bytes.wrapping_add_signed(delta);
        self.bytes.add(chunk, delta);
    }

    pub(crate) fn set_payload(&mut self, ordinal: usize, payload: T) {
        if let Some((chunk, slot)) = self.locate(ordinal) {
            self.chunks[chunk].items[slot].0 = payload;
        }
    }

    /// Replaces an ordinal range. Only the chunks the range touches are rewritten
    /// and re-chunked, so a structural edit moves a bounded number of blocks.
    pub(crate) fn splice(&mut self, ordinals: std::ops::Range<usize>, items: &[(T, usize)]) {
        let (first_chunk, first_slot) = self.locate_insert(ordinals.start);
        let (last_chunk, last_slot) = self.locate_insert(ordinals.end);
        let mut merged = Vec::with_capacity(items.len() + 2 * CHUNK_TARGET);
        if let Some(chunk) = self.chunks.get(first_chunk) {
            merged.extend_from_slice(&chunk.items[..first_slot.min(chunk.items.len())]);
        }
        merged.extend_from_slice(items);
        if let Some(chunk) = self.chunks.get(last_chunk) {
            merged.extend_from_slice(&chunk.items[last_slot.min(chunk.items.len())..]);
        }
        let replacement = merged
            .chunks(CHUNK_TARGET)
            .map(Chunk::new)
            .collect::<Vec<_>>();
        let end_chunk = (last_chunk + 1).min(self.chunks.len());
        self.chunks
            .splice(first_chunk.min(end_chunk)..end_chunk, replacement);
        self.retree();
    }

    /// Blocks from `ordinal` on, as `(ordinal, payload, start offset, length)`.
    pub(crate) fn iter_from(
        &self,
        ordinal: usize,
    ) -> impl Iterator<Item = (usize, T, usize, usize)> + '_ {
        let mut position = self.locate_insert(ordinal);
        let mut offset = self.start(ordinal);
        let mut next = ordinal;
        std::iter::from_fn(move || {
            let (mut chunk, mut slot) = position;
            while chunk < self.chunks.len() && slot >= self.chunks[chunk].items.len() {
                chunk += 1;
                slot = 0;
            }
            let (payload, length) = *self.chunks.get(chunk)?.items.get(slot)?;
            position = (chunk, slot + 1);
            let item = (next, payload, offset, length);
            offset += length;
            next += 1;
            Some(item)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Naive model the chunked store must agree with.
    fn model(items: &[(u32, usize)]) -> Vec<(u32, usize, usize)> {
        let mut start = 0;
        items
            .iter()
            .enumerate()
            .map(|(ordinal, (payload, length))| {
                let entry = (*payload, ordinal, start);
                start += length;
                entry
            })
            .collect()
    }

    fn items(count: usize) -> Vec<(u32, usize)> {
        (0..count)
            .map(|value| (value as u32, value % 7 + 1))
            .collect()
    }

    #[test]
    fn lookups_agree_with_a_flat_model_across_chunk_boundaries() {
        let items = items(3_000);
        let store = BlockStore::new(items.clone());
        assert_eq!(store.len(), items.len());
        assert_eq!(
            store.total_bytes(),
            items.iter().map(|(_, length)| length).sum::<usize>()
        );
        for (payload, ordinal, start) in model(&items) {
            assert_eq!(store.get(ordinal).map(|item| item.0), Some(payload));
            assert_eq!(store.start(ordinal), start);
            assert_eq!(store.ordinal_at(start), Some(ordinal));
            assert_eq!(
                store.ordinal_at(start + store.length(ordinal) - 1),
                Some(ordinal)
            );
        }
        assert_eq!(store.get(items.len()), None);
        assert_eq!(
            store.ordinal_at(store.total_bytes()),
            Some(items.len() - 1),
            "the document end belongs to the last block"
        );
        assert_eq!(
            store
                .iter_from(0)
                .map(|(_, payload, _, _)| payload)
                .collect::<Vec<_>>(),
            items
                .iter()
                .map(|(payload, _)| *payload)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            store
                .iter_from(1_500)
                .map(|(ordinal, _, _, _)| ordinal)
                .next(),
            Some(1_500)
        );
        assert_eq!(store.iter_from(items.len()).next(), None);
    }

    #[test]
    fn splices_and_length_changes_keep_the_store_consistent() {
        let mut items = items(2_000);
        let mut store = BlockStore::new(items.clone());

        // Insert in the middle, delete a run, replace across a chunk boundary.
        let inserted = [(9_001, 4), (9_002, 5)];
        store.splice(700..700, &inserted);
        items.splice(700..700, inserted);
        store.splice(100..150, &[]);
        items.splice(100..150, []);
        let replacement = [(9_100, 3)];
        store.splice(500..520, &replacement);
        items.splice(500..520, replacement);
        store.set_length(300, 42);
        items[300].1 = 42;
        store.set_payload(301, 7_777);
        items[301].0 = 7_777;

        assert_eq!(store.len(), items.len());
        assert_eq!(
            store.total_bytes(),
            items.iter().map(|(_, length)| length).sum::<usize>()
        );
        for (payload, ordinal, start) in model(&items) {
            assert_eq!(store.get(ordinal), Some(items[ordinal]));
            assert_eq!(store.get(ordinal).map(|item| item.0), Some(payload));
            assert_eq!(store.start(ordinal), start);
            assert_eq!(store.ordinal_at(start), Some(ordinal));
        }
    }

    #[test]
    fn an_emptied_store_reports_nothing() {
        let mut store = BlockStore::new(items(10));
        store.splice(0..10, &[]);
        assert!(store.is_empty());
        assert_eq!(store.total_bytes(), 0);
        assert_eq!(store.ordinal_at(0), None);
        assert_eq!(store.iter_from(0).next(), None);
        store.splice(0..0, &[(1, 5)]);
        assert_eq!(store.len(), 1);
        assert_eq!(store.ordinal_at(5), Some(0));
    }
}
