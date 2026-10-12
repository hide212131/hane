use crate::{VisualBlock, VisualOffset};
use hane_markdown::BlockId;
use std::ops::Range;

/// Heights per leaf chunk. Structural edits rewrite at most the two boundary
/// chunks plus their replacement instead of moving every following block.
const HEIGHT_CHUNK_TARGET: usize = 128;

#[derive(Clone, Debug)]
struct HeightChunk {
    heights: Vec<f32>,
    total: f32,
}

impl HeightChunk {
    fn new(heights: &[f32]) -> Self {
        Self {
            heights: heights.to_vec(),
            total: heights.iter().sum(),
        }
    }
}

/// Two-level Fenwick tree over non-negative block heights. Per-block values
/// live in bounded chunks; the trees index only chunk totals and item counts.
#[derive(Clone, Debug)]
pub struct HeightIndex {
    chunks: Vec<HeightChunk>,
    sums: Vec<f32>,
    counts: Vec<usize>,
    len: usize,
}

impl HeightIndex {
    pub fn new(heights: impl IntoIterator<Item = f32>) -> Self {
        let heights: Vec<_> = heights.into_iter().map(|h| h.max(0.0)).collect();
        let chunks = heights
            .chunks(HEIGHT_CHUNK_TARGET)
            .map(HeightChunk::new)
            .collect();
        let mut this = Self {
            chunks,
            sums: Vec::new(),
            counts: Vec::new(),
            len: heights.len(),
        };
        this.retree();
        this
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[expect(clippy::manual_isolate_lowest_one, reason = "unstable on Rust 1.98.1")]
    fn retree(&mut self) {
        self.sums.clear();
        self.counts.clear();
        self.sums.push(0.0);
        self.counts.push(0);
        self.sums
            .extend(self.chunks.iter().map(|chunk| chunk.total));
        self.counts
            .extend(self.chunks.iter().map(|chunk| chunk.heights.len()));
        for index in 1..self.sums.len() {
            let parent = index + (index & index.wrapping_neg());
            if parent < self.sums.len() {
                self.sums[parent] += self.sums[index];
                self.counts[parent] += self.counts[index];
            }
        }
    }

    fn tree_prefix<T>(tree: &[T], exclusive_end: usize) -> T
    where
        T: Copy + Default + std::ops::AddAssign,
    {
        let mut index = exclusive_end.min(tree.len().saturating_sub(1));
        let mut sum = T::default();
        while index > 0 {
            sum += tree[index];
            index &= index - 1;
        }
        sum
    }

    fn search_counts(&self, ordinal: usize) -> (usize, usize) {
        let mut index = 0;
        let mut count = 0;
        let mut step = 1;
        while step << 1 < self.counts.len() {
            step <<= 1;
        }
        while step > 0 {
            let next = index + step;
            if next < self.counts.len() && count + self.counts[next] <= ordinal {
                index = next;
                count += self.counts[next];
            }
            step >>= 1;
        }
        (index, count)
    }

    fn locate(&self, ordinal: usize) -> Option<(usize, usize)> {
        if ordinal >= self.len {
            return None;
        }
        let (chunk, before) = self.search_counts(ordinal);
        Some((chunk, ordinal - before))
    }

    fn locate_insert(&self, ordinal: usize) -> (usize, usize) {
        self.locate(ordinal).unwrap_or_else(|| {
            self.chunks
                .last()
                .map_or((0, 0), |chunk| (self.chunks.len() - 1, chunk.heights.len()))
        })
    }

    #[expect(clippy::manual_isolate_lowest_one, reason = "unstable on Rust 1.98.1")]
    fn add_sum(&mut self, chunk: usize, delta: f32) {
        let mut node = chunk + 1;
        while node < self.sums.len() {
            self.sums[node] += delta;
            node += node & node.wrapping_neg();
        }
    }

    pub fn update(&mut self, index: usize, height: f32) {
        let next = height.max(0.0);
        let (chunk, slot) = self.locate(index).expect("height index out of bounds");
        let delta = next - self.chunks[chunk].heights[slot];
        self.chunks[chunk].heights[slot] = next;
        self.chunks[chunk].total += delta;
        self.add_sum(chunk, delta);
    }
    pub fn height(&self, index: usize) -> Option<f32> {
        let (chunk, slot) = self.locate(index)?;
        Some(self.chunks[chunk].heights[slot])
    }
    /// Replaces one ordinal range while preserving the measured heights outside
    /// it. Only boundary chunks are copied; the Fenwick trees are rebuilt over
    /// chunks, not over every block.
    pub fn splice(&mut self, range: Range<usize>, heights: impl IntoIterator<Item = f32>) {
        assert!(range.start <= range.end && range.end <= self.len());
        let inserted = heights
            .into_iter()
            .map(|height| height.max(0.0))
            .collect::<Vec<_>>();
        let (first_chunk, first_slot) = self.locate_insert(range.start);
        let (last_chunk, last_slot) = self.locate_insert(range.end);
        let mut merged = Vec::with_capacity(inserted.len() + 2 * HEIGHT_CHUNK_TARGET);
        if let Some(chunk) = self.chunks.get(first_chunk) {
            merged.extend_from_slice(&chunk.heights[..first_slot.min(chunk.heights.len())]);
        }
        merged.extend_from_slice(&inserted);
        if let Some(chunk) = self.chunks.get(last_chunk) {
            merged.extend_from_slice(&chunk.heights[last_slot.min(chunk.heights.len())..]);
        }
        let replacement = merged
            .chunks(HEIGHT_CHUNK_TARGET)
            .map(HeightChunk::new)
            .collect::<Vec<_>>();
        let end_chunk = (last_chunk + 1).min(self.chunks.len());
        self.chunks
            .splice(first_chunk.min(end_chunk)..end_chunk, replacement);
        self.len = self.len - range.len() + inserted.len();
        self.retree();
    }
    pub fn prefix_sum(&self, exclusive_end: usize) -> f32 {
        let end = exclusive_end.min(self.len);
        let Some((chunk, slot)) = self.locate(end) else {
            return Self::tree_prefix(&self.sums, self.chunks.len());
        };
        Self::tree_prefix(&self.sums, chunk)
            + self.chunks[chunk].heights[..slot].iter().sum::<f32>()
    }
    pub fn total_height(&self) -> f32 {
        self.prefix_sum(self.len())
    }
    pub fn block_at_y(&self, y: f32) -> usize {
        if self.is_empty() {
            return 0;
        }
        let target = y.clamp(0.0, self.total_height());
        let mut chunk = 0usize;
        let mut sum = 0.0;
        let mut bit = 1usize;
        while bit << 1 < self.sums.len() {
            bit <<= 1;
        }
        while bit > 0 {
            let next = chunk + bit;
            if next < self.sums.len() && sum + self.sums[next] <= target {
                chunk = next;
                sum += self.sums[next];
            }
            bit >>= 1;
        }
        // A `target` equal to the whole height — where a viewport at the end of
        // the document lands, since `y` is clamped to it — lets the descent walk
        // past the last chunk. Step back onto that chunk and take its total back
        // out of `sum`, or the scan below starts from a running height that
        // already covers the chunk and returns its first entry.
        if chunk >= self.chunks.len() {
            chunk = self.chunks.len() - 1;
            sum -= self.chunks[chunk].total;
        }
        let ordinal = Self::tree_prefix(&self.counts, chunk);
        for (slot, height) in self.chunks[chunk].heights.iter().enumerate() {
            if target < sum + height {
                return ordinal + slot;
            }
            sum += height;
        }
        (ordinal + self.chunks[chunk].heights.len().saturating_sub(1)).min(self.len - 1)
    }
    pub fn visible_range(&self, scroll_y: f32, viewport: f32, overscan: f32) -> Range<usize> {
        if self.is_empty() {
            return 0..0;
        }
        let start = self.block_at_y((scroll_y - overscan).max(0.0));
        let end = (self.block_at_y(scroll_y + viewport + overscan) + 1).min(self.len());
        start..end
    }
}

/// A scroll position expressed against a block rather than a pixel offset, so it
/// survives a rebuild of the height index.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollAnchor {
    pub block: BlockId,
    pub intra_block_y: f32,
    pub visual_position_hint: Option<VisualOffset>,
}

pub fn anchored_scroll_y(
    anchor: ScrollAnchor,
    blocks: &[VisualBlock],
    heights: &HeightIndex,
) -> Option<f32> {
    let index = blocks.iter().position(|block| block.id == anchor.block)?;
    Some(heights.prefix_sum(index) + anchor.intra_block_y.clamp(0.0, blocks[index].height()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fenwick_updates_and_finds_visible_blocks() {
        let mut h = HeightIndex::new([10.0, 20.0, 30.0]);
        assert_eq!(h.total_height(), 60.0);
        assert_eq!(h.block_at_y(10.0), 1);
        assert_eq!(h.visible_range(12.0, 10.0, 0.0), 1..2);
        h.update(0, 20.0);
        assert_eq!(h.total_height(), 70.0);
        assert_eq!(h.block_at_y(15.0), 0);
    }
    #[test]
    fn the_last_block_stays_visible_when_the_viewport_reaches_the_end() {
        // Measured heights of the README's blocks: the viewport plus overscan
        // reaches past the total height well before the scroll limit, so the
        // whole tail of the document depends on this.
        let heights = HeightIndex::new([
            68.9, 216.0, 104.0, 63.7, 208.0, 63.7, 78.0, 52.0, 63.7, 52.0, 235.3, 52.0, 115.7,
            63.7, 52.0, 314.8, 52.0, 63.7, 52.0, 63.7, 52.0, 52.0, 145.6, 130.0,
        ]);
        let total = heights.total_height();
        assert_eq!(heights.block_at_y(total), 23);
        assert_eq!(heights.block_at_y(total + 500.0), 23);
        let visible = heights.visible_range(1480.6, 693.0, 260.0);
        assert_eq!(visible.end, 24);
        assert!(visible.start < visible.end);
    }

    #[test]
    fn the_last_block_stays_visible_across_a_chunk_boundary() {
        // More entries than one chunk holds, so the descent ends on a chunk the
        // running height has already been carried past.
        let heights = HeightIndex::new(vec![10.0; 300]);
        assert_eq!(heights.block_at_y(3000.0), 299);
        assert_eq!(heights.visible_range(2900.0, 100.0, 0.0), 290..300);
    }

    #[test]
    fn height_splice_preserves_measurements_outside_the_changed_blocks() {
        let mut h = HeightIndex::new([11.0, 22.0, 33.0, 44.0]);
        h.splice(1..3, [7.0, 8.0, 9.0]);
        assert_eq!(h.len(), 5);
        assert_eq!(h.height(0), Some(11.0));
        assert_eq!(h.height(1), Some(7.0));
        assert_eq!(h.height(3), Some(9.0));
        assert_eq!(h.height(4), Some(44.0));
        assert_eq!(h.total_height(), 79.0);
        assert_eq!(h.block_at_y(26.0), 3);
    }
    #[test]
    fn chunked_height_index_matches_a_flat_model_across_boundaries() {
        let mut flat = (0..300)
            .map(|index| (index % 9 + 1) as f32)
            .collect::<Vec<_>>();
        let mut heights = HeightIndex::new(flat.iter().copied());
        for (range, inserted) in [
            (127..130, vec![41.0, 42.0, 43.0, 44.0]),
            (0..1, vec![]),
            (299..299, vec![51.0, 52.0]),
            (100..250, vec![61.0, 62.0, 63.0]),
        ] {
            flat.splice(range.clone(), inserted.iter().copied());
            heights.splice(range, inserted);
            assert_eq!(heights.len(), flat.len());
            for index in 0..flat.len() {
                assert_eq!(heights.height(index), Some(flat[index]));
                assert_eq!(heights.prefix_sum(index), flat[..index].iter().sum());
            }
            assert_eq!(heights.total_height(), flat.iter().sum());
        }
        heights.update(128, 75.0);
        flat[128] = 75.0;
        let mut top = 0.0;
        for (index, height) in flat.iter().copied().enumerate() {
            assert_eq!(heights.block_at_y(top), index);
            top += height;
        }
        assert_eq!(heights.total_height(), top);
    }
}
