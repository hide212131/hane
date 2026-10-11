use hane_document::{SourceOffset, SourceRange};
use std::collections::HashMap;
use std::ops::Range;

/// Block-local geometry metadata for fenced-code rows that collapse to zero
/// height while inactive. This is deliberately separate from [`crate::ListProjection`]:
/// incremental block parsing can keep this geometry current even while formal
/// list numbering/container semantics are temporarily unavailable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FenceHeightProjection {
    rows: Vec<FenceHeightRow>,
    /// Fence rows grouped by the quote owner whose prefix is present on that
    /// row. The groups are disjoint: nested quote prefixes use the outermost
    /// owner on a line, because touching an inner owner also touches that
    /// owner's enclosing quote. Keeping this reverse index makes disclosure
    /// queries proportional to quote nesting, not to every fence in a block.
    quote_owner_rows: Vec<FenceQuoteOwnerRows>,
    /// Prefix count for rows that carry a quote prefix. It lets the direct
    /// fence-range query subtract rows already counted through the quote index
    /// without scanning the rows themselves.
    quote_row_prefix: Vec<usize>,
    /// Prefix maximum of quote-owner ends, aligned with `quote_owner_rows`.
    /// It skips owners that cannot contain a point disclosure before scanning
    /// the small set of overlapping/nested owners.
    quote_owner_max_end: Vec<SourceOffset>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FenceHeightRow {
    range: SourceRange,
    /// Zero-based physical line within the owning block. Keeping this beside
    /// the source range lets render-time geometry answer prefix queries without
    /// walking the block's source lines.
    line: usize,
    owns_end: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FenceQuoteOwnerRows {
    owner: SourceRange,
    rows: Vec<usize>,
}

impl FenceHeightProjection {
    pub(crate) fn from_absolute_rows(
        block_range: SourceRange,
        rows: Vec<(SourceRange, bool, Vec<SourceRange>, usize)>,
    ) -> Self {
        let mut normalized = rows
            .into_iter()
            .map(|(range, owns_end, quote_owners, line)| {
                let range = SourceRange::new(
                    range.start.0.saturating_sub(block_range.start.0),
                    range.end.0.saturating_sub(block_range.start.0),
                );
                let mut quote_owners = quote_owners
                    .into_iter()
                    .map(|owner| {
                        SourceRange::new(
                            owner.start.0.saturating_sub(block_range.start.0),
                            owner.end.0.saturating_sub(block_range.start.0),
                        )
                    })
                    .collect::<Vec<_>>();
                quote_owners.sort_by_key(|owner| (owner.start, owner.end));
                quote_owners.dedup();
                (range, owns_end, quote_owners, line)
            })
            .collect::<Vec<_>>();
        normalized.sort_by_key(|(range, _, _, line)| (range.start, range.end, *line));
        let mut deduplicated: Vec<(SourceRange, bool, Vec<SourceRange>, usize)> =
            Vec::with_capacity(normalized.len());
        for (range, owns_end, quote_owners, line) in normalized {
            if let Some((last_range, last_owns_end, last_quote_owners, _last_line)) =
                deduplicated.last_mut()
                && *last_range == range
            {
                *last_owns_end |= owns_end;
                last_quote_owners.extend(quote_owners);
                last_quote_owners.sort_by_key(|owner| (owner.start, owner.end));
                last_quote_owners.dedup();
            } else {
                deduplicated.push((range, owns_end, quote_owners, line));
            }
        }
        let rows = deduplicated
            .iter()
            .map(|(range, owns_end, _, line)| FenceHeightRow {
                range: *range,
                line: *line,
                owns_end: *owns_end,
            })
            .collect::<Vec<_>>();
        let mut owner_rows = HashMap::<SourceRange, Vec<usize>>::new();
        let mut quote_row_prefix = Vec::with_capacity(rows.len() + 1);
        quote_row_prefix.push(0);
        for (index, (_, _, quote_owners, _)) in deduplicated.iter().enumerate() {
            // Nested quote owners are nested source ranges. Choosing the
            // outermost actual prefix makes the groups disjoint while keeping
            // the disclosure rule exact: an inner disclosure also touches its
            // enclosing quote owner.
            let owner = quote_owners
                .iter()
                .min_by_key(|owner| (owner.start, std::cmp::Reverse(owner.end)))
                .copied();
            if let Some(owner) = owner {
                owner_rows.entry(owner).or_default().push(index);
                quote_row_prefix.push(quote_row_prefix[index] + 1);
            } else {
                quote_row_prefix.push(quote_row_prefix[index]);
            }
        }
        let mut quote_owner_rows = owner_rows
            .into_iter()
            .map(|(owner, rows)| FenceQuoteOwnerRows { owner, rows })
            .collect::<Vec<_>>();
        quote_owner_rows.sort_by_key(|entry| (entry.owner.start, entry.owner.end));
        let mut quote_owner_max_end = Vec::with_capacity(quote_owner_rows.len());
        let mut max_end = SourceOffset(0);
        for entry in &quote_owner_rows {
            max_end = max_end.max(entry.owner.end);
            quote_owner_max_end.push(max_end);
        }
        Self {
            rows,
            quote_owner_rows,
            quote_row_prefix,
            quote_owner_max_end,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Counts rows in `range` that remain collapsed under the current
    /// disclosure. Ranges stored here are block-relative, so byte edits in
    /// earlier blocks do not require rebasing this projection. `block_is_final`
    /// supplies the current block topology for the document-end quote-owner
    /// exception; it is intentionally not cached in the projection because
    /// incremental edits can change which block is final without reparsing it.
    pub fn inactive_rows_in(
        &self,
        block_range: SourceRange,
        range: SourceRange,
        disclosure: Option<SourceRange>,
        block_is_final: bool,
    ) -> usize {
        if range.is_empty() || self.rows.is_empty() {
            return 0;
        }
        let relative = SourceRange::new(
            range.start.0.saturating_sub(block_range.start.0),
            range.end.0.saturating_sub(block_range.start.0),
        );
        let start = self
            .rows
            .partition_point(|row| row.range.end <= relative.start);
        let end = self
            .rows
            .partition_point(|row| row.range.start < relative.end);
        self.inactive_rows_in_indices(block_range, start..end, disclosure, block_is_final)
    }

    /// Counts rows before a physical line boundary that remain collapsed under
    /// `disclosure`. This is the render-time prefix query used to translate a
    /// visual y position back to a source-line window. Its cost is bounded by
    /// binary searches over the projection, independent of the total block
    /// length or fence count. See [`Self::inactive_rows_in`] for the meaning
    /// of `block_is_final`.
    pub fn inactive_rows_before_line(
        &self,
        block_range: SourceRange,
        line: usize,
        disclosure: Option<SourceRange>,
        block_is_final: bool,
    ) -> usize {
        let end = self.rows.partition_point(|row| row.line < line);
        self.inactive_rows_in_indices(block_range, 0..end, disclosure, block_is_final)
    }

    /// Whether a physical block-relative line is one of the fence rows this
    /// projection can collapse. Keeping this as a binary-search query lets the
    /// UI use a cached wrapped layout to recover the measured height of a
    /// visible fence row without enumerating the block's off-screen fences.
    pub fn is_fence_row_line(&self, line: usize) -> bool {
        let start = self.rows.partition_point(|row| row.line < line);
        self.rows.get(start).is_some_and(|row| row.line == line)
    }

    fn inactive_rows_in_indices(
        &self,
        block_range: SourceRange,
        indices: Range<usize>,
        disclosure: Option<SourceRange>,
        block_is_final: bool,
    ) -> usize {
        let rows = &self.rows[indices.clone()];
        let Some(disclosure) = disclosure else {
            return rows.len();
        };
        if disclosure.is_empty() {
            if disclosure.start < block_range.start || disclosure.start > block_range.end {
                return rows.len();
            }
            let caret = SourceOffset(disclosure.start.0 - block_range.start.0);
            let relative_active = rows.partition_point(|row| {
                row.range.end < caret || (row.range.end == caret && !row.owns_end)
            });
            let active_row = rows
                .get(relative_active)
                .filter(|row| {
                    row.range.start <= caret
                        && (caret < row.range.end || (row.owns_end && caret == row.range.end))
                })
                .map(|_| indices.start + relative_active);
            let caret_absolute = block_range.start.0 + caret.0;
            let quote_active = self.active_quote_rows_in(
                indices.clone(),
                SourceRange::empty(caret.0),
                caret_absolute < block_range.end.0
                    || (block_is_final && caret_absolute == block_range.end.0),
            );
            let direct_active = usize::from(active_row.is_some());
            let direct_already_counted = active_row
                .is_some_and(|row| self.quote_row_prefix[row + 1] > self.quote_row_prefix[row]);
            return rows.len().saturating_sub(
                quote_active + direct_active - usize::from(direct_already_counted),
            );
        }
        // A non-empty selection only discloses the part of a block that it
        // actually intersects. Do this test before converting to block-
        // relative offsets: `saturating_sub` would otherwise turn a range
        // wholly before the block into `0..0`, which can be mistaken for a
        // caret on the block's first row by the quote-owner index.
        if !block_range.intersects(disclosure) {
            return rows.len();
        }
        let disclosure = SourceRange::new(
            disclosure
                .start
                .0
                .max(block_range.start.0)
                .min(block_range.end.0)
                - block_range.start.0,
            disclosure
                .end
                .0
                .min(block_range.end.0)
                .max(block_range.start.0)
                - block_range.start.0,
        );
        let active_start = rows.partition_point(|row| row.range.end <= disclosure.start);
        let active_end = rows.partition_point(|row| row.range.start < disclosure.end);
        let direct_active = active_end.saturating_sub(active_start);
        let direct_already_counted = self.quote_row_prefix[indices.start + active_end]
            .saturating_sub(self.quote_row_prefix[indices.start + active_start]);
        let quote_active = self.active_quote_rows_in(indices, disclosure, false);
        rows.len()
            .saturating_sub(quote_active + direct_active - direct_already_counted)
    }

    fn active_quote_rows_in(
        &self,
        rows: Range<usize>,
        disclosure: SourceRange,
        allow_owner_end: bool,
    ) -> usize {
        if self.quote_owner_rows.is_empty() {
            return 0;
        }
        let owner_end = if disclosure.is_empty() {
            self.quote_owner_rows
                .partition_point(|entry| entry.owner.start <= disclosure.start)
        } else {
            self.quote_owner_rows
                .partition_point(|entry| entry.owner.start < disclosure.end)
        };
        let owner_start = if disclosure.is_empty() {
            self.quote_owner_max_end[..owner_end].partition_point(|end| *end < disclosure.start)
        } else {
            self.quote_owner_max_end[..owner_end].partition_point(|end| *end <= disclosure.start)
        };
        self.quote_owner_rows[owner_start..owner_end]
            .iter()
            .filter(|entry| {
                if disclosure.is_empty() {
                    entry.owner.end > disclosure.start
                        || (allow_owner_end && entry.owner.end == disclosure.start)
                } else {
                    entry.owner.end > disclosure.start
                }
            })
            .map(|entry| {
                let start = entry.rows.partition_point(|row| *row < rows.start);
                let end = entry.rows.partition_point(|row| *row < rows.end);
                end.saturating_sub(start)
            })
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fence_height_projection_answers_physical_line_prefixes() {
        let block_range = SourceRange::new(100, 500);
        let projection = FenceHeightProjection::from_absolute_rows(
            block_range,
            vec![
                (SourceRange::new(100, 104), true, Vec::new(), 0),
                (SourceRange::new(300, 304), true, Vec::new(), 200),
            ],
        );

        assert_eq!(
            projection.inactive_rows_before_line(block_range, 0, None, true),
            0
        );
        assert_eq!(
            projection.inactive_rows_before_line(block_range, 1, None, true),
            1
        );
        assert_eq!(
            projection.inactive_rows_before_line(block_range, 200, None, true),
            1
        );
        assert_eq!(
            projection.inactive_rows_before_line(block_range, 201, None, true),
            2
        );
    }
}
