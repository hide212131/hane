//! Keeping the user's selection meaningful across a replacement.

use hane_document::SourceRange;
use unicode_segmentation::UnicodeSegmentation;

/// The largest grapheme boundary of `text` that is at or before `offset`.
fn snap_down(text: &str, offset: usize) -> usize {
    if offset >= text.len() {
        return text.len();
    }
    text.grapheme_indices(true)
        .map(|(start, _)| start)
        .take_while(|start| *start <= offset)
        .last()
        .unwrap_or(0)
}

/// Where one selection endpoint goes when `old` is replaced by `new_text`:
///
/// * at or before the start of the range: unchanged;
/// * at or after its end (the caret an insertion leaves at its end included):
///   moved by the change in length, i.e. to the new end for the old end;
/// * inside it: the same distance from the start in the new text, or the new
///   end if that is past it, moved to a cursor boundary of the new text.
pub fn map_endpoint(offset: usize, old: SourceRange, new_text: &str) -> usize {
    let (start, end) = (old.start.0, old.end.0);
    if offset <= start {
        offset
    } else if offset >= end {
        offset - old.len_bytes() + new_text.len()
    } else {
        start + snap_down(new_text, offset - start)
    }
}

/// Both endpoints of a selection through the same replacement.
pub fn map_selection(
    (anchor, active): (usize, usize),
    old: SourceRange,
    new_text: &str,
) -> (usize, usize) {
    (
        map_endpoint(anchor, old, new_text),
        map_endpoint(active, old, new_text),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_outside_the_range_move_by_the_length_change() {
        let old = SourceRange::new(10, 20);
        assert_eq!(map_endpoint(3, old, "xx"), 3);
        assert_eq!(map_endpoint(10, old, "xx"), 10);
        assert_eq!(
            map_endpoint(20, old, "xx"),
            12,
            "the end follows the new end"
        );
        assert_eq!(map_endpoint(25, old, "xx"), 17);
        assert_eq!(map_endpoint(25, old, &"y".repeat(15)), 30);
    }

    #[test]
    fn an_endpoint_inside_keeps_its_distance_or_clamps_to_the_new_end() {
        let old = SourceRange::new(10, 20);
        assert_eq!(map_endpoint(13, old, "abcdef"), 13);
        assert_eq!(
            map_endpoint(19, old, "abc"),
            13,
            "past the new end → the end"
        );
        assert_eq!(map_endpoint(15, old, ""), 10);
    }

    #[test]
    fn an_endpoint_inside_never_lands_inside_a_character_or_a_cluster() {
        let old = SourceRange::new(0, 30);
        // Byte 1 is inside 'あ' (3 bytes).
        assert_eq!(map_endpoint(1, old, "あい"), 0);
        assert_eq!(map_endpoint(4, old, "あい"), 3);
        // 'e' + combining acute is one cluster of three bytes.
        let text = "e\u{301}x";
        assert_eq!(map_endpoint(2, old, text), 0);
        assert_eq!(map_endpoint(3, old, text), 3);
        // A flag is one cluster of eight bytes.
        assert_eq!(map_endpoint(5, old, "🇯🇵z"), 0);
    }

    #[test]
    fn the_caret_at_the_end_of_an_insertion_follows_a_replacement() {
        // Inserted [3, 9) with the caret at 9; replaced by 4 bytes.
        let old = SourceRange::new(3, 9);
        assert_eq!(map_selection((9, 9), old, "wxyz"), (7, 7));
    }

    #[test]
    fn a_selection_spanning_the_range_keeps_both_outer_ends() {
        let old = SourceRange::new(5, 8);
        assert_eq!(map_selection((2, 12), old, "abcdef"), (2, 15));
        // Reversed selections keep their direction.
        assert_eq!(map_selection((12, 2), old, "abcdef"), (15, 2));
    }
}
