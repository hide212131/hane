//! Editing state shared by the sidebar filter and the inline rename field.
//!
//! The model owns the text, the selection (with its direction), the marked
//! (in-composition) range and the pre-composition snapshot used to cancel an
//! IME composition. Callers keep every outer condition (focus, rename
//! pending, clipboard, notifications, list updates); this module only keeps
//! the text and the ranges consistent.
//!
//! Ranges are UTF-8 byte ranges on scalar boundaries internally. Positions
//! exchanged with the platform input handler are UTF-16 offsets.

use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Debug, Default)]
pub(crate) struct ShortTextInput {
    text: String,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    composition: Option<CompositionSnapshot>,
}

#[derive(Clone, Debug)]
struct CompositionSnapshot {
    text: String,
    selected_range: Range<usize>,
    selection_reversed: bool,
}

impl ShortTextInput {
    pub(crate) fn new(text: String) -> Self {
        let end = text.len();
        Self {
            text,
            selected_range: end..end,
            selection_reversed: false,
            marked_range: None,
            composition: None,
        }
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn selected_range(&self) -> Range<usize> {
        self.selected_range.clone()
    }

    pub(crate) fn selection_reversed(&self) -> bool {
        self.selection_reversed
    }

    pub(crate) fn marked_range(&self) -> Option<Range<usize>> {
        self.marked_range.clone()
    }

    pub(crate) fn has_composition(&self) -> bool {
        self.composition.is_some()
    }

    pub(crate) fn selected_text(&self) -> Option<&str> {
        (!self.selected_range.is_empty()).then(|| &self.text[self.selected_range.clone()])
    }

    pub(crate) fn text_for_utf16_range(&self, range: Range<usize>) -> (String, Range<usize>) {
        let range = byte_range_from_utf16(&self.text, &range);
        (
            self.text[range.clone()].to_owned(),
            range_to_utf16(&self.text, &range),
        )
    }

    pub(crate) fn selection_utf16(&self) -> (Range<usize>, bool) {
        (
            range_to_utf16(&self.text, &self.selected_range),
            self.selection_reversed,
        )
    }

    pub(crate) fn marked_range_utf16(&self) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| range_to_utf16(&self.text, range))
    }

    pub(crate) fn replace(&mut self, range_utf16: Option<Range<usize>>, text: &str) {
        let range = self.edit_range(range_utf16);
        let replacement = strip_line_breaks(text);
        self.text.replace_range(range.clone(), &replacement);
        let next = range.start + replacement.len();
        self.selected_range = next..next;
        self.selection_reversed = false;
        self.marked_range = None;
        self.composition = None;
    }

    pub(crate) fn replace_and_mark(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        selected_utf16: Option<Range<usize>>,
    ) {
        if self.composition.is_none() {
            self.composition = Some(CompositionSnapshot {
                text: self.text.clone(),
                selected_range: self.selected_range.clone(),
                selection_reversed: self.selection_reversed,
            });
        }
        let range = self.edit_range(range_utf16);
        let replacement = strip_line_breaks(text);
        self.text.replace_range(range.clone(), &replacement);
        let marked_end = range.start + replacement.len();
        self.marked_range = (!replacement.is_empty()).then_some(range.start..marked_end);
        self.selected_range = selected_range_in_replacement(
            range.start,
            &replacement,
            selected_utf16,
            marked_end..marked_end,
        );
        self.selection_reversed = false;
    }

    pub(crate) fn move_horizontal(&mut self, right: bool, extend: bool) {
        let target = if right {
            next_boundary(&self.text, self.cursor())
        } else {
            previous_boundary(&self.text, self.cursor())
        };
        if extend {
            self.select_to(target);
        } else if self.selected_range.is_empty() {
            self.selected_range = target..target;
            self.selection_reversed = false;
        } else {
            let target = if right {
                self.selected_range.end
            } else {
                self.selected_range.start
            };
            self.selected_range = target..target;
            self.selection_reversed = false;
        }
    }

    pub(crate) fn move_home(&mut self, extend: bool) {
        self.move_to(0, extend);
    }

    pub(crate) fn move_end(&mut self, extend: bool) {
        let end = self.text.len();
        self.move_to(end, extend);
    }

    pub(crate) fn select_all(&mut self) {
        self.selected_range = 0..self.text.len();
        self.selection_reversed = false;
    }

    pub(crate) fn set_caret_utf16(&mut self, offset: usize) {
        let byte = byte_offset_from_utf16(&self.text, offset);
        self.selected_range = byte..byte;
        self.selection_reversed = false;
    }

    pub(crate) fn delete(&mut self, backwards: bool) {
        let range = if self.selected_range.is_empty() {
            let cursor = self.cursor();
            if backwards {
                previous_boundary(&self.text, cursor)..cursor
            } else {
                cursor..next_boundary(&self.text, cursor)
            }
        } else {
            self.selected_range.clone()
        };
        self.text.replace_range(range.clone(), "");
        self.selected_range = range.start..range.start;
        self.selection_reversed = false;
        self.marked_range = None;
        self.composition = None;
    }

    pub(crate) fn commit_composition(&mut self) -> bool {
        if self.composition.take().is_some() {
            self.marked_range = None;
            true
        } else {
            false
        }
    }

    pub(crate) fn cancel_composition(&mut self) -> bool {
        let Some(composition) = self.composition.take() else {
            return false;
        };
        self.text = composition.text;
        self.selected_range = composition.selected_range;
        self.selection_reversed = composition.selection_reversed;
        self.marked_range = None;
        true
    }

    pub(crate) fn discard_composition(&mut self) {
        self.marked_range = None;
        self.composition = None;
    }

    fn edit_range(&self, range_utf16: Option<Range<usize>>) -> Range<usize> {
        range_utf16
            .as_ref()
            .map(|range| byte_range_from_utf16(&self.text, range))
            .or_else(|| self.marked_range.clone())
            .unwrap_or_else(|| self.selected_range.clone())
    }

    fn cursor(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn move_to(&mut self, target: usize, extend: bool) {
        if extend {
            self.select_to(target);
        } else {
            self.selected_range = target..target;
            self.selection_reversed = false;
        }
    }

    fn select_to(&mut self, offset: usize) {
        if self.selection_reversed {
            self.selected_range.start = offset;
        } else {
            self.selected_range.end = offset;
        }
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
    }
}

fn strip_line_breaks(text: &str) -> String {
    text.chars()
        .filter(|character| *character != '\n' && *character != '\r')
        .collect()
}

fn byte_offset_from_utf16(text: &str, offset: usize) -> usize {
    let mut utf16 = 0;
    for (byte, character) in text.char_indices() {
        if utf16 >= offset {
            return byte;
        }
        utf16 += character.len_utf16();
    }
    text.len()
}

pub(crate) fn utf16_offset_from_byte(text: &str, offset: usize) -> usize {
    let mut utf16 = 0;
    for (byte, character) in text.char_indices() {
        if byte >= offset {
            break;
        }
        utf16 += character.len_utf16();
    }
    utf16
}

fn byte_range_from_utf16(text: &str, range: &Range<usize>) -> Range<usize> {
    byte_offset_from_utf16(text, range.start)..byte_offset_from_utf16(text, range.end)
}

fn range_to_utf16(text: &str, range: &Range<usize>) -> Range<usize> {
    utf16_offset_from_byte(text, range.start)..utf16_offset_from_byte(text, range.end)
}

fn selected_range_in_replacement(
    replacement_start: usize,
    replacement: &str,
    selected_range_utf16: Option<Range<usize>>,
    fallback: Range<usize>,
) -> Range<usize> {
    selected_range_utf16
        .map(|selected| {
            let selected_start = byte_offset_from_utf16(replacement, selected.start);
            let selected_end = byte_offset_from_utf16(replacement, selected.end);
            (replacement_start + selected_start)..(replacement_start + selected_end)
        })
        .unwrap_or(fallback)
}

fn previous_boundary(text: &str, offset: usize) -> usize {
    text[..offset]
        .grapheme_indices(true)
        .next_back()
        .map_or(0, |(index, _)| index)
}

fn next_boundary(text: &str, offset: usize) -> usize {
    text[offset..]
        .grapheme_indices(true)
        .nth(1)
        .map_or(text.len(), |(index, _)| offset + index)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIXED: &str = "A😀e\u{301}Z";

    type State = (String, Range<usize>, bool, Option<Range<usize>>, bool);

    fn state(input: &ShortTextInput) -> State {
        (
            input.text().to_owned(),
            input.selected_range(),
            input.selection_reversed(),
            input.marked_range(),
            input.has_composition(),
        )
    }

    /// Builds `text` with an explicit (possibly reversed) selection through
    /// the public operations only.
    fn with_selection(text: &str, range: Range<usize>, reversed: bool) -> ShortTextInput {
        let mut input = ShortTextInput::new(text.to_owned());
        if reversed {
            // Fix the anchor at the end, then extend backwards past it.
            input.set_caret_utf16(utf16_offset_from_byte(text, range.end));
            input.select_to(range.start);
        } else {
            input.set_caret_utf16(utf16_offset_from_byte(text, range.start));
            input.select_to(range.end);
        }
        input
    }

    #[test]
    fn m01_default_and_new_keep_text_unmodified() {
        let default = ShortTextInput::default();
        assert_eq!(state(&default), (String::new(), 0..0, false, None, false));
        let empty = ShortTextInput::new(String::new());
        assert_eq!(state(&empty), (String::new(), 0..0, false, None, false));
        let named = ShortTextInput::new("a\r\nb".to_owned());
        assert_eq!(
            state(&named),
            ("a\r\nb".to_owned(), 4..4, false, None, false)
        );
    }

    #[test]
    fn m02_utf16_positions_round_to_the_following_scalar_boundary() {
        let expected = [(0, 0), (1, 1), (2, 5), (3, 5), (4, 6), (5, 8), (6, 9), (99, 9)];
        for (utf16, byte) in expected {
            assert_eq!(byte_offset_from_utf16(MIXED, utf16), byte, "utf16 {utf16}");
        }
        let input = ShortTextInput::new(MIXED.to_owned());
        assert_eq!(input.text_for_utf16_range(1..3), ("😀".to_owned(), 1..3));
        assert_eq!(input.text_for_utf16_range(2..3), (String::new(), 3..3));
    }

    #[test]
    fn m03_explicit_range_replaces_whole_emoji() {
        let mut input = ShortTextInput::new(MIXED.to_owned());
        input.replace(Some(1..3), "羽");
        assert_eq!(
            state(&input),
            ("A羽e\u{301}Z".to_owned(), 4..4, false, None, false)
        );
    }

    #[test]
    fn m04_range_inside_surrogate_pair_does_not_split_the_emoji() {
        let mut input = ShortTextInput::new(MIXED.to_owned());
        input.replace(Some(2..3), "羽");
        assert_eq!(input.text(), "A😀羽e\u{301}Z");
        assert_eq!(input.selected_range(), 8..8);
    }

    #[test]
    fn m05_range_priority_is_explicit_then_marked_then_selection() {
        let mut input = ShortTextInput::new("abcdef".to_owned());
        input.replace_and_mark(Some(1..3), "XY", None);
        assert_eq!(input.marked_range(), Some(1..3));
        input.set_caret_utf16(5);
        // Selection (5..5) and marked (1..3) differ: marked wins.
        let mut marked = input.clone();
        marked.replace(None, "_");
        assert_eq!(marked.text(), "a_def");
        // An explicit range, even empty, wins over marked.
        let mut explicit = input.clone();
        explicit.replace(Some(0..0), "_");
        assert_eq!(explicit.text(), "_aXYdef");
        // Without marked, the selection is used.
        let mut selection = ShortTextInput::new("abcdef".to_owned());
        selection.set_caret_utf16(2);
        selection.replace(None, "_");
        assert_eq!(selection.text(), "ab_cdef");
    }

    #[test]
    fn m06_mark_strips_line_breaks_and_maps_selection_relative_to_replacement() {
        let mut input = ShortTextInput::default();
        input.replace_and_mark(None, "a\r\n😀b", Some(1..3));
        assert_eq!(input.text(), "a😀b");
        assert_eq!(input.marked_range(), Some(0..6));
        assert_eq!(input.selected_range(), 1..5);
        assert!(!input.selection_reversed());
        let mut beyond = ShortTextInput::default();
        beyond.replace_and_mark(None, "ab", Some(1..99));
        assert_eq!(beyond.selected_range(), 1..2);
    }

    #[test]
    fn m07_extending_across_the_anchor_flips_direction() {
        let mut input = with_selection("ab", 0..1, true);
        assert_eq!((input.selected_range(), input.selection_reversed()), (0..1, true));
        input.move_horizontal(true, true);
        assert_eq!((input.selected_range(), input.selection_reversed()), (1..1, true));
        input.move_horizontal(true, true);
        assert_eq!((input.selected_range(), input.selection_reversed()), (1..2, false));

        let mut forward = with_selection("ab", 1..2, false);
        forward.move_horizontal(false, true);
        assert_eq!((forward.selected_range(), forward.selection_reversed()), (1..1, false));
        forward.move_horizontal(false, true);
        assert_eq!((forward.selected_range(), forward.selection_reversed()), (0..1, true));
    }

    #[test]
    fn m08_collapse_home_end_and_select_all() {
        let mut input = with_selection("abcd", 1..3, false);
        input.move_horizontal(true, false);
        assert_eq!(input.selected_range(), 3..3);
        let mut input = with_selection("abcd", 1..3, false);
        input.move_horizontal(false, false);
        assert_eq!(input.selected_range(), 1..1);

        let mut input = with_selection("abcd", 1..3, false);
        input.move_home(true);
        assert_eq!((input.selected_range(), input.selection_reversed()), (0..1, true));
        let mut input = with_selection("abcd", 1..3, false);
        input.move_end(true);
        assert_eq!((input.selected_range(), input.selection_reversed()), (1..4, false));
        input.move_home(false);
        assert_eq!((input.selected_range(), input.selection_reversed()), (0..0, false));
        input.move_end(false);
        assert_eq!(input.selected_range(), 4..4);
        input.select_all();
        assert_eq!((input.selected_range(), input.selection_reversed()), (0..4, false));

        let mut marking = ShortTextInput::default();
        marking.replace_and_mark(None, "ab", None);
        marking.select_all();
        marking.move_home(false);
        assert!(marking.has_composition());
        assert_eq!(marking.marked_range(), Some(0..2));
    }

    #[test]
    fn m09_boundaries_follow_graphemes_and_empty_deletes_reset_state() {
        let mut input = ShortTextInput::new(MIXED.to_owned());
        input.move_horizontal(false, false);
        assert_eq!(input.selected_range(), 8..8);
        input.move_horizontal(false, false);
        assert_eq!(input.selected_range(), 5..5);
        input.move_horizontal(false, false);
        assert_eq!(input.selected_range(), 1..1);
        input.move_horizontal(true, false);
        assert_eq!(input.selected_range(), 5..5);
        input.move_horizontal(true, false);
        assert_eq!(input.selected_range(), 8..8);

        let mut backwards = ShortTextInput::new(MIXED.to_owned());
        backwards.move_horizontal(false, false);
        backwards.delete(true);
        assert_eq!(backwards.text(), "A😀Z");
        assert_eq!(backwards.selected_range(), 5..5);
        let mut forwards = ShortTextInput::new(MIXED.to_owned());
        forwards.move_home(false);
        forwards.move_horizontal(true, false);
        forwards.delete(false);
        assert_eq!(forwards.text(), "Ae\u{301}Z");

        for text in ["👩\u{200d}💻z", "🇯🇵z"] {
            let mut input = ShortTextInput::new(text.to_owned());
            input.move_home(false);
            input.delete(false);
            assert_eq!(input.text(), "z", "{text}");
            let mut input = ShortTextInput::new(text.to_owned());
            input.move_horizontal(false, false);
            input.move_horizontal(false, false);
            assert_eq!(input.selected_range(), 0..0, "{text}");
        }

        let mut marked = ShortTextInput::default();
        marked.replace_and_mark(None, "ab", None);
        marked.move_home(false);
        marked.delete(true);
        assert_eq!(state(&marked), ("ab".to_owned(), 0..0, false, None, false));
        marked.move_end(false);
        marked.delete(false);
        assert_eq!(state(&marked), ("ab".to_owned(), 2..2, false, None, false));
    }

    #[test]
    fn m10_utf16_caret_keeps_mid_cluster_scalar_boundary() {
        let mut input = ShortTextInput::new(MIXED.to_owned());
        input.set_caret_utf16(4);
        assert_eq!(input.selected_range(), 6..6);
        assert!(!input.selection_reversed());
        input.move_horizontal(false, false);
        assert_eq!(input.selected_range(), 5..5);
        input.set_caret_utf16(4);
        input.move_horizontal(true, false);
        assert_eq!(input.selected_range(), 8..8);
    }

    #[test]
    fn m11_cancel_restores_first_snapshot_after_several_marks() {
        let mut input = with_selection("abcd", 1..3, true);
        input.replace_and_mark(None, "か", Some(0..1));
        input.replace_and_mark(None, "かな", None);
        input.replace_and_mark(None, "かなX", None);
        assert!(input.cancel_composition());
        assert_eq!(state(&input), ("abcd".to_owned(), 1..3, true, None, false));
    }

    #[test]
    fn m12_commit_reports_only_the_first_time() {
        let mut input = ShortTextInput::new("ab".to_owned());
        input.replace_and_mark(None, "か", None);
        let committed = state(&input);
        assert!(input.commit_composition());
        assert_eq!(input.text(), committed.0);
        assert_eq!(input.selected_range(), committed.1);
        assert_eq!(input.marked_range(), None);
        assert!(!input.commit_composition());
        assert!(!input.cancel_composition());
        assert_eq!(input.text(), "abか");
        assert_eq!(input.selected_range(), 5..5);
    }

    #[test]
    fn m13_empty_marks_leave_a_snapshot_without_a_marked_range() {
        let mut input = ShortTextInput::new("ab".to_owned());
        input.replace_and_mark(None, "", None);
        assert_eq!(state(&input), ("ab".to_owned(), 2..2, false, None, true));
        input.replace_and_mark(None, "\r\n", None);
        assert_eq!(state(&input), ("ab".to_owned(), 2..2, false, None, true));
        let mut with_text = ShortTextInput::new("ab".to_owned());
        with_text.replace_and_mark(None, "か", None);
        with_text.replace_and_mark(None, "\n", None);
        assert_eq!(with_text.marked_range(), None);
        assert!(with_text.cancel_composition());
        assert_eq!(state(&with_text), ("ab".to_owned(), 2..2, false, None, false));
    }

    #[test]
    fn m14_discard_keeps_text_and_selection() {
        let mut input = ShortTextInput::new("ab".to_owned());
        input.replace_and_mark(None, "か", Some(0..1));
        let selected = input.selected_range();
        input.discard_composition();
        assert_eq!(input.text(), "abか");
        assert_eq!(input.selected_range(), selected);
        assert_eq!(input.marked_range(), None);
        assert!(!input.has_composition());
        assert!(!input.cancel_composition());
    }

    #[test]
    fn m15_two_values_are_independent() {
        let mut filter = ShortTextInput::default();
        let mut rename = ShortTextInput::new("note".to_owned());
        filter.replace_and_mark(None, "か", None);
        rename.select_all();
        rename.replace(None, "x");
        assert_eq!(state(&filter), ("か".to_owned(), 3..3, false, Some(0..3), true));
        assert_eq!(state(&rename), ("x".to_owned(), 1..1, false, None, false));
        assert!(filter.cancel_composition());
        assert_eq!(rename.text(), "x");
        assert_eq!(filter.text(), "");
    }

    // Moved from `view.rs`: the former `inline_rename_*` helper tests.

    #[test]
    fn movement_uses_grapheme_boundaries() {
        let text = "a\u{301}👩‍💻z";
        let combining_end = "a\u{301}".len();
        let emoji_end = "a\u{301}👩‍💻".len();

        assert_eq!(next_boundary(text, 0), combining_end);
        assert_eq!(previous_boundary(text, combining_end), 0);
        assert_eq!(next_boundary(text, combining_end), emoji_end);
        assert_eq!(previous_boundary(text, emoji_end), combining_end);
        assert_eq!(next_boundary(text, emoji_end), text.len());
    }

    /// Pinned byte boundaries, written out by hand rather than asked of the
    /// segmentation library, so a library update that moves a boundary fails
    /// here (Issue #587 updates `unicode-segmentation` to 1.13.3).
    const PINNED_BOUNDARIES: &[(&str, &[usize])] = &[
        ("日本語", &[0, 3, 6, 9]),
        ("e\u{301}x", &[0, 3, 4]),
        ("👨\u{200d}👩\u{200d}👧\u{200d}👦", &[0, 25]),
        ("🇯🇵🇺🇸", &[0, 8, 16]),
        ("👍🏽", &[0, 8]),
        ("か\u{3099}き", &[0, 6, 9]),
        ("☺\u{fe0f}!", &[0, 6, 7]),
        ("1\u{fe0f}\u{20e3}", &[0, 7]),
        ("\u{1112}\u{1161}\u{11ab}", &[0, 9]),
        ("a\r\nb", &[0, 1, 3, 4]),
    ];

    #[test]
    fn grapheme_boundaries_match_the_pinned_unicode_expectations() {
        for (text, boundaries) in PINNED_BOUNDARIES {
            let mut forward = vec![0];
            while *forward.last().unwrap() < text.len() {
                forward.push(next_boundary(text, *forward.last().unwrap()));
            }
            assert_eq!(&forward, boundaries, "forward over {text:?}");

            let mut backward = vec![text.len()];
            while *backward.last().unwrap() > 0 {
                backward.push(previous_boundary(text, *backward.last().unwrap()));
            }
            backward.reverse();
            assert_eq!(&backward, boundaries, "backward over {text:?}");
        }
    }

    #[test]
    fn deleting_and_moving_remove_whole_pinned_clusters() {
        for (text, boundaries) in PINNED_BOUNDARIES {
            let mut backspace = ShortTextInput::new((*text).to_owned());
            backspace.move_end(false);
            backspace.delete(true);
            assert_eq!(
                backspace.text(),
                &text[..boundaries[boundaries.len() - 2]],
                "backspace at the end of {text:?}"
            );

            let mut delete = ShortTextInput::new((*text).to_owned());
            delete.move_home(false);
            delete.delete(false);
            assert_eq!(
                delete.text(),
                &text[boundaries[1]..],
                "delete at the start of {text:?}"
            );
        }
    }

    #[test]
    fn shift_home_and_end_extend_from_the_active_caret() {
        let mut forward = with_selection("abcdef", 2..4, false);
        forward.move_home(true);
        assert_eq!(forward.selected_range(), 0..2);
        assert!(forward.selection_reversed());

        let mut reversed = with_selection("abcdef", 2..4, true);
        reversed.move_end(true);
        assert_eq!(reversed.selected_range(), 4..6);
        assert!(!reversed.selection_reversed());
    }

    #[test]
    fn ime_selection_is_relative_to_the_inserted_text() {
        assert_eq!(
            selected_range_in_replacement(6, "かな", Some(1..2), 12..12),
            9..12
        );
        assert_eq!(
            selected_range_in_replacement(6, "かな", None, 12..12),
            12..12
        );
    }

    #[test]
    fn selected_text_and_utf16_views_follow_the_state() {
        let mut input = ShortTextInput::new(MIXED.to_owned());
        assert_eq!(input.selected_text(), None);
        input.select_all();
        assert_eq!(input.selected_text(), Some(MIXED));
        assert_eq!(input.selection_utf16(), (0..6, false));
        input.replace_and_mark(Some(1..3), "か", None);
        assert_eq!(input.marked_range_utf16(), Some(1..2));
    }
}
