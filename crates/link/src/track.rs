//! Following an inserted range through later edits.

use hane_document::{RevisionDelta, SourceRange};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrackError {
    /// An edit touched the range: inside it, over it, or an undo of it.
    Changed,
}

/// Moves `range` through `deltas` in order. Rules, per edit `[bs, be)`:
///
/// * ending at or before the range start (including a pure insertion exactly
///   at its start): the range shifts right or left by the edit's size;
/// * starting at or after the range end (including a pure insertion exactly at
///   its end): the range is unaffected;
/// * anything else overlaps the range, or sits strictly inside it:
///   [`TrackError::Changed`], even when a later edit would restore the text.
///
/// The simple `RevisionDelta::transform_range` cannot be used as is: it treats
/// an empty edit range as intersecting nothing, so an insertion inside the
/// range would slip through, and it ties a boundary edit to its bias.
pub fn track_range(
    range: SourceRange,
    deltas: &[RevisionDelta],
) -> Result<SourceRange, TrackError> {
    let (mut start, mut end) = (range.start.0, range.end.0);
    for delta in deltas {
        let before = delta.edited_source_range_before;
        let (edit_start, edit_end) = (before.start.0, before.end.0);
        if edit_end <= start {
            // Entirely before (or touching the start from the left, or a pure
            // insertion at the start): the whole range moves.
            start = shift(start, delta.byte_delta)?;
            end = shift(end, delta.byte_delta)?;
        } else if edit_start >= end {
            // Entirely after, or touching the end from the right.
        } else {
            return Err(TrackError::Changed);
        }
    }
    Ok(SourceRange::new(start, end))
}

fn shift(offset: usize, delta: isize) -> Result<usize, TrackError> {
    offset.checked_add_signed(delta).ok_or(TrackError::Changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hane_document::{Revision, SourceOffset};

    /// `replace [from, to) with a text of `inserted` bytes`.
    fn delta(from: usize, to: usize, inserted: usize) -> RevisionDelta {
        RevisionDelta {
            from_revision: Revision(0),
            to_revision: Revision(1),
            edited_source_range_before: SourceRange::new(from, to),
            edited_source_range_after: SourceRange::new(from, from + inserted),
            byte_delta: inserted as isize - (to - from) as isize,
        }
    }

    fn track(
        range: (usize, usize),
        deltas: &[RevisionDelta],
    ) -> Result<(usize, usize), TrackError> {
        track_range(SourceRange::new(range.0, range.1), deltas)
            .map(|range| (range.start.0, range.end.0))
    }

    #[test]
    fn an_edit_before_the_range_shifts_it() {
        assert_eq!(track((10, 20), &[delta(2, 2, 3)]), Ok((13, 23)));
        assert_eq!(track((10, 20), &[delta(2, 5, 0)]), Ok((7, 17)));
        assert_eq!(track((10, 20), &[delta(2, 5, 1)]), Ok((8, 18)));
    }

    #[test]
    fn an_edit_that_ends_exactly_at_the_start_is_outside() {
        assert_eq!(track((10, 20), &[delta(6, 10, 1)]), Ok((7, 17)));
    }

    #[test]
    fn typing_at_the_start_moves_the_range_right() {
        assert_eq!(track((10, 20), &[delta(10, 10, 4)]), Ok((14, 24)));
        // A second keystroke lands after the first one, still at the new start.
        assert_eq!(
            track((10, 20), &[delta(10, 10, 1), delta(10, 10, 1)]),
            Ok((12, 22))
        );
    }

    #[test]
    fn typing_at_the_end_is_outside_the_range() {
        assert_eq!(track((10, 20), &[delta(20, 20, 4)]), Ok((10, 20)));
        assert_eq!(track((10, 20), &[delta(20, 25, 0)]), Ok((10, 20)));
    }

    #[test]
    fn an_edit_after_the_range_is_ignored() {
        assert_eq!(track((10, 20), &[delta(30, 31, 5)]), Ok((10, 20)));
    }

    #[test]
    fn an_insertion_strictly_inside_the_range_changes_it() {
        assert_eq!(
            track((10, 20), &[delta(15, 15, 1)]),
            Err(TrackError::Changed)
        );
        assert_eq!(
            track((10, 20), &[delta(11, 11, 1)]),
            Err(TrackError::Changed)
        );
        assert_eq!(
            track((10, 20), &[delta(19, 19, 1)]),
            Err(TrackError::Changed)
        );
    }

    #[test]
    fn a_deletion_or_replacement_overlapping_the_range_changes_it() {
        for (from, to) in [(8, 12), (15, 18), (18, 25), (5, 30), (10, 20), (10, 11)] {
            assert_eq!(
                track((10, 20), &[delta(from, to, 0)]),
                Err(TrackError::Changed),
                "{from}..{to}"
            );
            assert_eq!(
                track((10, 20), &[delta(from, to, 2)]),
                Err(TrackError::Changed),
                "{from}..{to}"
            );
        }
    }

    #[test]
    fn undoing_the_insertion_is_a_change_even_if_the_text_would_come_back() {
        // Undo removes exactly the range; a redo would insert it again.
        assert_eq!(
            track((10, 20), &[delta(10, 20, 0), delta(10, 10, 10)]),
            Err(TrackError::Changed)
        );
    }

    #[test]
    fn deltas_apply_in_order_against_the_moved_range() {
        // A 5-byte insertion before, then an edit that only overlaps the
        // shifted range, not the original one.
        assert_eq!(
            track((10, 20), &[delta(0, 0, 5), delta(16, 17, 1)]),
            Err(TrackError::Changed)
        );
        assert_eq!(
            track((10, 20), &[delta(0, 0, 5), delta(8, 9, 1)]),
            Ok((15, 25))
        );
    }

    #[test]
    fn a_shift_below_zero_is_a_change() {
        let weird = RevisionDelta {
            from_revision: Revision(0),
            to_revision: Revision(1),
            edited_source_range_before: SourceRange::new(0, 1),
            edited_source_range_after: SourceRange::new(0, 0),
            byte_delta: -100,
        };
        assert_eq!(track((10, 20), &[weird]), Err(TrackError::Changed));
        let _ = SourceOffset(0);
    }
}
