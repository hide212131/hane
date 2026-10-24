//! Builds the `{{document_context}}` text for the selection AI popup
//! (Issue #594): the whole document when it fits the Hane-side budget,
//! otherwise one contiguous excerpt around the selection. Only the needed
//! source range is ever turned into a `String`.

use super::*;
use hane_ai::{DocumentContext, DocumentContextScope, MAX_SELECTED_TEXT_BYTES};

/// How far a cut may be moved to land on a line break. Bounded so that
/// finding a paragraph boundary never scans far beyond the budget.
const BOUNDARY_SNAP_BYTES: usize = 512;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DocumentContextError {
    /// The selection leaves no room (or too little to contain itself) in the
    /// document-derived input budget.
    SelectionTooLarge,
    /// The captured range no longer maps onto the document.
    Unavailable,
}

/// The source range to send as document context for `selection`, given the
/// total budget of document-derived bytes. The budget left for the context is
/// `budget - selection bytes`; a context that has to be an excerpt must still
/// contain the whole selection.
pub(super) fn document_context_range(
    document: &RopeBuffer,
    selection: SourceRange,
    budget: usize,
) -> Result<(SourceRange, DocumentContextScope), DocumentContextError> {
    let len = document.len_bytes().0;
    if selection.end.0 > len || selection.start.0 > selection.end.0 {
        return Err(DocumentContextError::Unavailable);
    }
    let selection_len = selection.len_bytes();
    let context_max = budget.saturating_sub(selection_len);
    if context_max == 0 {
        return Err(DocumentContextError::SelectionTooLarge);
    }
    if len <= context_max {
        return Ok((SourceRange::new(0, len), DocumentContextScope::Full));
    }
    if selection_len > context_max {
        return Err(DocumentContextError::SelectionTooLarge);
    }

    // Spread the remaining bytes around the selection; whatever one side
    // cannot use (document edge) goes to the other side.
    let extra = context_max - selection_len;
    let mut before = extra / 2;
    let mut after = extra - before;
    let available_before = selection.start.0;
    let available_after = len - selection.end.0;
    if available_before < before {
        after += before - available_before;
        before = available_before;
    }
    if available_after < after {
        before = (before + (after - available_after)).min(available_before);
        after = available_after;
    }
    let mut start = selection.start.0 - before;
    let mut end = selection.end.0 + after;

    // Never cut a UTF-8 character: shrink toward the selection.
    while start < selection.start.0 && document.validate_offset(SourceOffset(start)).is_err() {
        start += 1;
    }
    while end > selection.end.0 && document.validate_offset(SourceOffset(end)).is_err() {
        end -= 1;
    }

    // Prefer cutting right after a line break close to each end, shrinking
    // toward the selection so the budget is still respected.
    if start > 0 && start < selection.start.0 {
        let limit = (start + BOUNDARY_SNAP_BYTES).min(selection.start.0);
        if let Ok(window) = document.text(SourceRange::new(start, limit))
            && let Some(newline) = window.as_bytes().iter().position(|byte| *byte == b'\n')
        {
            start += newline + 1;
        }
    }
    if end < len && end > selection.end.0 {
        let floor = end.saturating_sub(BOUNDARY_SNAP_BYTES).max(selection.end.0);
        if let Ok(window) = document.text(SourceRange::new(floor, end))
            && let Some(newline) = window.as_bytes().iter().rposition(|byte| *byte == b'\n')
        {
            end = floor + newline + 1;
        }
    }
    Ok((
        SourceRange::new(start, end),
        DocumentContextScope::Surrounding,
    ))
}

/// Builds the document context for `selection` from `document` (the same
/// revision the caller validated as fresh).
pub(super) fn build_document_context(
    document: &RopeBuffer,
    selection: SourceRange,
) -> Result<DocumentContext, DocumentContextError> {
    let (range, scope) = document_context_range(document, selection, MAX_SELECTED_TEXT_BYTES)?;
    let text = document
        .text(range)
        .map_err(|_| DocumentContextError::Unavailable)?;
    Ok(DocumentContext { text, scope })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range_of(text: &str, selected: &str) -> SourceRange {
        let start = text.find(selected).unwrap();
        SourceRange::new(start, start + selected.len())
    }

    /// T04: a document that fits the remaining budget is sent whole,
    /// including unsaved content (the buffer is the live one).
    #[test]
    fn small_document_is_sent_in_full() {
        let text = "前の段落\n\n選択部分\n\n後の段落";
        let document = RopeBuffer::from_text(text);
        let selection = range_of(text, "選択部分");
        let context = build_document_context(&document, selection).unwrap();
        assert_eq!(context.scope, DocumentContextScope::Full);
        assert_eq!(context.text, text);
    }

    /// T05/T06: an oversized document yields one contiguous excerpt that
    /// contains the selection and keeps selection + excerpt within budget.
    #[test]
    fn large_document_yields_a_contiguous_excerpt_containing_the_selection() {
        let line = "あいうえお\n";
        let text = line.repeat(40_000);
        let document = RopeBuffer::from_text(&text);
        // Character-aligned: every line is the same 16 bytes.
        let start = line.len() * 20_000;
        let selection = SourceRange::new(start, start + 3 * line.len());
        let budget = 4096;
        let (range, scope) = document_context_range(&document, selection, budget).unwrap();
        assert_eq!(scope, DocumentContextScope::Surrounding);
        assert!(range.start.0 <= selection.start.0 && selection.end.0 <= range.end.0);
        assert!(range.len_bytes() <= budget - selection.len_bytes());
        document.validate_range(range).unwrap();
    }

    #[test]
    fn excerpt_at_document_start_and_end_gives_the_spare_to_the_other_side() {
        let text = "a".repeat(10_000);
        let document = RopeBuffer::from_text(&text);
        let budget = 1000;

        let head = SourceRange::new(0, 10);
        let (range, _) = document_context_range(&document, head, budget).unwrap();
        assert_eq!(range.start.0, 0);
        assert_eq!(range.len_bytes(), budget - 10);

        let tail = SourceRange::new(9_990, 10_000);
        let (range, _) = document_context_range(&document, tail, budget).unwrap();
        assert_eq!(range.end.0, 10_000);
        assert_eq!(range.len_bytes(), budget - 10);
    }

    /// A huge single paragraph (no line breaks) still gets an excerpt that
    /// respects UTF-8 boundaries.
    #[test]
    fn excerpt_never_splits_a_utf8_character() {
        let text = "あ".repeat(5_000);
        let document = RopeBuffer::from_text(&text);
        let selection = SourceRange::new(3 * 2_500, 3 * 2_500 + 3);
        for budget in [100, 101, 102, 1001, 4000] {
            let (range, scope) = document_context_range(&document, selection, budget).unwrap();
            assert_eq!(scope, DocumentContextScope::Surrounding);
            document.validate_range(range).unwrap();
            assert!(range.start.0 <= selection.start.0 && selection.end.0 <= range.end.0);
            assert!(range.len_bytes() <= budget - selection.len_bytes());
        }
    }

    #[test]
    fn excerpt_prefers_a_line_break_cut_within_the_budget() {
        let mut text = String::new();
        for index in 0..2_000 {
            text.push_str(&format!("line {index:04}\n"));
        }
        let document = RopeBuffer::from_text(&text);
        let selection = range_of(&text, "line 1000");
        let (range, scope) = document_context_range(&document, selection, 1000).unwrap();
        assert_eq!(scope, DocumentContextScope::Surrounding);
        assert_eq!(text.as_bytes()[range.start.0 - 1], b'\n');
        assert_eq!(text.as_bytes()[range.end.0 - 1], b'\n');
        assert!(range.len_bytes() <= 1000 - selection.len_bytes());
    }

    /// T06: a selection that leaves no room, or cannot contain itself in the
    /// remaining budget, is an explicit error rather than an empty context.
    #[test]
    fn selection_that_leaves_no_room_is_an_explicit_error() {
        let text = "a".repeat(5_000);
        let document = RopeBuffer::from_text(&text);
        assert_eq!(
            document_context_range(&document, SourceRange::new(0, 1000), 1000),
            Err(DocumentContextError::SelectionTooLarge)
        );
        assert_eq!(
            document_context_range(&document, SourceRange::new(0, 600), 1000),
            Err(DocumentContextError::SelectionTooLarge)
        );
    }
}
