//! GPUI-independent replace planning for the document find bar (Issue #589).
//!
//! A plan is computed from an already-verified, source-ordered, non-overlapping
//! match list over an immutable [`RopeBuffer`] snapshot, so it can be prepared
//! on a background thread and published later in one short step.
//!
//! The internal representation is a single covering edit: the plan holds one
//! replacement string for the span from the first *changed* match's start to
//! the last changed match's end (the untouched text between matches is copied
//! verbatim from the snapshot). Publishing it as one
//! [`crate::Editor::replace_range_recorded`] transaction gives, with no new
//! history or document machinery: one Undo/Redo entry that is separate from
//! the surrounding typing, an all-or-nothing document edit (the range is
//! validated before anything is mutated), and the existing revision / delta /
//! line-mirror contract. The cost is that the span between the first and last
//! changed match is copied once while planning and held by the history entry.

use hane_document::{BufferError, RopeBuffer, SourceRange, TextBuffer};

/// Why a replace plan could not be built. Nothing has been changed when one of
/// these is returned: planning only reads the snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReplacePlanError {
    /// A match was out of order, overlapping a previous match, or inverted.
    InvalidMatches,
    /// The resulting document size does not fit the platform's address space.
    SizeOverflow,
    /// The planning was cancelled before it finished.
    Cancelled,
    Buffer(BufferError),
}

impl From<BufferError> for ReplacePlanError {
    fn from(error: BufferError) -> Self {
        Self::Buffer(error)
    }
}

/// A prepared, not yet applied replacement of every changed match.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplacePlan {
    /// Matches in the plan's input (including ones already equal to the
    /// replacement).
    pub matched: usize,
    /// Matches whose bytes actually differ from the replacement.
    pub changed: usize,
    /// Document length (bytes) the plan was computed against.
    pub source_len: usize,
    /// The source span replaced, from the first changed match's start to the
    /// last changed match's end. `None` when nothing changes.
    pub range: Option<SourceRange>,
    /// Text that replaces `range`.
    pub replacement: String,
    /// Total bytes inserted / deleted over the changed matches; the caller
    /// uses their difference to shift a selection scope's end.
    pub inserted_bytes: usize,
    pub deleted_bytes: usize,
}

impl ReplacePlan {
    /// The document length after the plan is applied.
    #[must_use]
    pub fn result_len(&self) -> usize {
        self.source_len - self.deleted_bytes + self.inserted_bytes
    }
}

const CANCEL_POLL_INTERVAL: usize = 1_024;

/// Plans replacing every range of `matches` with `replacement`.
///
/// Each match is processed once, in source order, against the original
/// `buffer`; inserted text is never searched again. Matches whose current text
/// already equals `replacement` are left alone and not counted as changes.
///
/// # Errors
///
/// Returns [`ReplacePlanError`] without side effects when the matches are not
/// strictly ordered and non-overlapping, a range is invalid for `buffer`, the
/// size arithmetic overflows, or `is_cancelled` reports true.
pub fn plan_replace_all(
    buffer: &RopeBuffer,
    matches: &[SourceRange],
    replacement: &str,
    mut is_cancelled: impl FnMut() -> bool,
) -> Result<ReplacePlan, ReplacePlanError> {
    let source_len = buffer.len_bytes().0;
    let mut previous_end = 0usize;
    let mut changed_ranges: Vec<SourceRange> = Vec::new();
    for (index, &range) in matches.iter().enumerate() {
        if index & (CANCEL_POLL_INTERVAL - 1) == 0 && is_cancelled() {
            return Err(ReplacePlanError::Cancelled);
        }
        if range.start.0 > range.end.0 || range.start.0 < previous_end {
            return Err(ReplacePlanError::InvalidMatches);
        }
        buffer.validate_range(range)?;
        buffer.validate_offset(range.start)?;
        buffer.validate_offset(range.end)?;
        previous_end = range.end.0;
        // A different length is a different text; only equal lengths need the
        // bytes themselves compared.
        let unchanged = range.len_bytes() == replacement.len()
            && buffer.text(range)? == replacement;
        if !unchanged {
            changed_ranges.push(range);
        }
    }
    let (Some(first), Some(last)) = (changed_ranges.first(), changed_ranges.last()) else {
        return Ok(ReplacePlan {
            matched: matches.len(),
            changed: 0,
            source_len,
            range: None,
            replacement: String::new(),
            inserted_bytes: 0,
            deleted_bytes: 0,
        });
    };
    let span = SourceRange::new(first.start.0, last.end.0);
    let mut deleted_bytes = 0usize;
    let mut inserted_bytes = 0usize;
    for range in &changed_ranges {
        deleted_bytes = deleted_bytes
            .checked_add(range.len_bytes())
            .ok_or(ReplacePlanError::SizeOverflow)?;
        inserted_bytes = inserted_bytes
            .checked_add(replacement.len())
            .ok_or(ReplacePlanError::SizeOverflow)?;
    }
    // Gaps between changed matches are source text that stays; their total is
    // the span minus what is deleted.
    let gap_bytes = span
        .len_bytes()
        .checked_sub(deleted_bytes)
        .ok_or(ReplacePlanError::InvalidMatches)?;
    let capacity = gap_bytes
        .checked_add(inserted_bytes)
        .ok_or(ReplacePlanError::SizeOverflow)?;
    source_len
        .checked_sub(deleted_bytes)
        .and_then(|len| len.checked_add(inserted_bytes))
        .ok_or(ReplacePlanError::SizeOverflow)?;

    let mut output = String::with_capacity(capacity);
    let mut cursor = span.start.0;
    for (index, range) in changed_ranges.iter().enumerate() {
        if index & (CANCEL_POLL_INTERVAL - 1) == 0 && is_cancelled() {
            return Err(ReplacePlanError::Cancelled);
        }
        if range.start.0 > cursor {
            output.push_str(&buffer.text(SourceRange::new(cursor, range.start.0))?);
        }
        output.push_str(replacement);
        cursor = range.end.0;
    }
    debug_assert_eq!(output.len(), capacity);
    Ok(ReplacePlan {
        matched: matches.len(),
        changed: changed_ranges.len(),
        source_len,
        range: Some(span),
        replacement: output,
        inserted_bytes,
        deleted_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FindOptions, FindQuery, FindScan, find_scan};

    fn find_all(text: &str, query: &str, case_sensitive: bool) -> (RopeBuffer, Vec<SourceRange>) {
        let buffer = RopeBuffer::from_text(text);
        let query = FindQuery::parse(query).unwrap();
        let FindScan::Completed(results) =
            find_scan(&buffer, &query, FindOptions { case_sensitive }, || false)
        else {
            panic!("never cancelled");
        };
        (buffer, results.matches)
    }

    fn apply(text: &str, query: &str, replacement: &str, case_sensitive: bool) -> String {
        let (buffer, matches) = find_all(text, query, case_sensitive);
        let plan = plan_replace_all(&buffer, &matches, replacement, || false).unwrap();
        let mut editor = crate::Editor::new(text);
        if let Some(range) = plan.range {
            let end = SourceRange::empty(range.start.0 + plan.replacement.len());
            editor
                .replace_range_recorded(
                    range,
                    &plan.replacement,
                    crate::Selection::caret(end.start),
                )
                .unwrap();
        }
        editor.document().full_text()
    }

    #[test]
    fn replaces_every_match_once_without_rescanning_inserted_text() {
        assert_eq!(apply("a a a", "a", "aa", true), "aa aa aa");
        assert_eq!(apply("abab", "ab", "abab", true), "abababab");
    }

    #[test]
    fn empty_replacement_deletes_matches() {
        assert_eq!(apply("x-y-z", "-", "", true), "xyz");
        assert_eq!(apply("aaaa", "aa", "", true), "");
    }

    #[test]
    fn length_changing_and_unicode_replacements_match_expected_text() {
        assert_eq!(
            apply("日本語 と 日本語", "日本語", "にほんご🙂", true),
            "にほんご🙂 と にほんご🙂"
        );
        assert_eq!(apply("e\u{301}x e\u{301}", "e\u{301}", "E", true), "Ex E");
    }

    #[test]
    fn case_insensitive_matches_of_different_byte_length_are_replaced_by_match_range() {
        // "İ" (2 bytes) and "i̇"-style lowercasings may differ in byte length
        // from the query; ranges come from the match, not the query length.
        assert_eq!(apply("Hello HELLO hello", "hello", "x", false), "x x x");
        assert_eq!(apply("Straße STRASSE", "straße", "S", false), "S STRASSE");
    }

    #[test]
    fn adjacent_matches_are_all_replaced() {
        assert_eq!(apply("aaa", "a", "bb", true), "bbbbbb");
    }

    #[test]
    fn matches_equal_to_the_replacement_are_not_counted_as_changes() {
        let (buffer, matches) = find_all("Foo foo FOO", "foo", false);
        let plan = plan_replace_all(&buffer, &matches, "foo", || false).unwrap();
        assert_eq!(plan.matched, 3);
        assert_eq!(plan.changed, 2);
        assert_eq!(apply("Foo foo FOO", "foo", "foo", false), "foo foo foo");
    }

    #[test]
    fn identical_or_absent_matches_produce_an_empty_plan() {
        let (buffer, matches) = find_all("same same", "same", true);
        let plan = plan_replace_all(&buffer, &matches, "same", || false).unwrap();
        assert_eq!((plan.matched, plan.changed, plan.range), (2, 0, None));
        let (buffer, matches) = find_all("nothing", "zzz", true);
        let plan = plan_replace_all(&buffer, &matches, "q", || false).unwrap();
        assert_eq!((plan.matched, plan.changed, plan.range), (0, 0, None));
    }

    #[test]
    fn text_outside_the_matches_is_preserved_byte_for_byte() {
        assert_eq!(
            apply("p1\r\nfoo\rbar foo\nend", "foo", "X", true),
            "p1\r\nX\rbar X\nend"
        );
    }

    #[test]
    fn plan_reports_byte_accounting() {
        let (buffer, matches) = find_all("ab ab", "ab", true);
        let plan = plan_replace_all(&buffer, &matches, "xyz", || false).unwrap();
        assert_eq!(plan.deleted_bytes, 4);
        assert_eq!(plan.inserted_bytes, 6);
        assert_eq!(plan.result_len(), 5 - 4 + 6);
    }

    #[test]
    fn unordered_or_overlapping_matches_are_rejected() {
        let buffer = RopeBuffer::from_text("abcdef");
        let overlapping = [SourceRange::new(0, 3), SourceRange::new(2, 4)];
        assert_eq!(
            plan_replace_all(&buffer, &overlapping, "x", || false),
            Err(ReplacePlanError::InvalidMatches)
        );
        let unordered = [SourceRange::new(3, 4), SourceRange::new(0, 1)];
        assert_eq!(
            plan_replace_all(&buffer, &unordered, "x", || false),
            Err(ReplacePlanError::InvalidMatches)
        );
    }

    #[test]
    fn out_of_bounds_or_non_boundary_matches_are_rejected() {
        let buffer = RopeBuffer::from_text("日本");
        assert!(matches!(
            plan_replace_all(&buffer, &[SourceRange::new(0, 99)], "x", || false),
            Err(ReplacePlanError::Buffer(_))
        ));
        assert!(matches!(
            plan_replace_all(&buffer, &[SourceRange::new(1, 3)], "x", || false),
            Err(ReplacePlanError::Buffer(_))
        ));
    }

    #[test]
    fn cancellation_returns_an_error_and_changes_nothing() {
        let (buffer, matches) = find_all("a a a", "a", true);
        assert_eq!(
            plan_replace_all(&buffer, &matches, "b", || true),
            Err(ReplacePlanError::Cancelled)
        );
        assert_eq!(buffer.full_text(), "a a a");
    }

    #[test]
    fn one_undo_and_redo_restore_the_whole_replacement_with_selection() {
        use crate::{EditorCommand, Selection};
        use hane_document::SourceOffset;
        let text = "foo bar foo";
        let mut editor = crate::Editor::new(text);
        let before = Selection {
            anchor: SourceOffset(7),
            active: SourceOffset(2),
        };
        editor.set_selection(before).unwrap();
        let (buffer, matches) = find_all(text, "foo", true);
        let plan = plan_replace_all(&buffer, &matches, "quux", || false).unwrap();
        let range = plan.range.unwrap();
        let after = Selection::caret(SourceOffset(range.start.0 + plan.replacement.len()));
        editor
            .replace_range_recorded(range, &plan.replacement, after)
            .unwrap();
        assert_eq!(editor.document().full_text(), "quux bar quux");
        editor.dispatch(EditorCommand::Undo).unwrap();
        assert_eq!(editor.document().full_text(), text);
        assert_eq!(editor.selection(), before);
        editor.dispatch(EditorCommand::Redo).unwrap();
        assert_eq!(editor.document().full_text(), "quux bar quux");
        assert_eq!(editor.selection(), after);
    }

    #[test]
    fn replacement_is_independent_from_surrounding_typing_in_history() {
        use crate::EditorCommand;
        let mut editor = crate::Editor::new("foo foo");
        editor.insert_text("1").unwrap();
        let text = editor.document().full_text();
        let (buffer, matches) = find_all(&text, "foo", true);
        let plan = plan_replace_all(&buffer, &matches, "b", || false).unwrap();
        let range = plan.range.unwrap();
        editor
            .replace_range_recorded(
                range,
                &plan.replacement,
                crate::Selection::caret(hane_document::SourceOffset(
                    range.start.0 + plan.replacement.len(),
                )),
            )
            .unwrap();
        editor.insert_text("2").unwrap();
        let typed_then_replaced_then_typed = editor.document().full_text();
        assert!(typed_then_replaced_then_typed.contains('b'));
        editor.dispatch(EditorCommand::Undo).unwrap();
        editor.dispatch(EditorCommand::Undo).unwrap();
        assert_eq!(editor.document().full_text(), text);
        editor.dispatch(EditorCommand::Undo).unwrap();
        assert_eq!(editor.document().full_text(), "foo foo");
        for _ in 0..3 {
            editor.dispatch(EditorCommand::Redo).unwrap();
        }
        assert_eq!(editor.document().full_text(), typed_then_replaced_then_typed);
    }
}
