//! Byte-range partitioning shared by painting and shaping.

use std::ops::Range;

/// Splits `bounds` at every supplied boundary. Boundaries outside the range,
/// repeated boundaries, and empty intervals are discarded. Callers retain
/// ownership of their semantic state (style, selection, IME, caret); this only
/// establishes the UTF-8 byte ranges on which that state is constant.
pub(crate) fn partition(
    bounds: Range<usize>,
    boundaries: impl IntoIterator<Item = usize>,
) -> Vec<Range<usize>> {
    let mut points = vec![bounds.start, bounds.end];
    points.extend(
        boundaries
            .into_iter()
            .map(|point| point.clamp(bounds.start, bounds.end)),
    );
    points.sort_unstable();
    points.dedup();
    points
        .windows(2)
        .filter_map(|pair| {
            let range = pair[0]..pair[1];
            (!range.is_empty()).then_some(range)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::partition;

    #[test]
    fn clamps_deduplicates_and_discards_empty_ranges() {
        assert_eq!(partition(3..9, [0, 3, 5, 5, 9, 12]), vec![3..5, 5..9]);
    }

    #[test]
    fn partitions_soft_wrapped_style_ranges_on_utf8_byte_boundaries() {
        // `é` occupies bytes 1..3. The helper deliberately operates in the
        // visual byte coordinate system supplied by presentation.
        assert_eq!(partition(0..5, [1, 3, 4]), vec![0..1, 1..3, 3..4, 4..5]);
    }
}
