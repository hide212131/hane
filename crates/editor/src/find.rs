//! GPUI-independent literal full-document find: query/options validation, a
//! chunk-crossing byte-windowed scanner over [`RopeBuffer`], and match
//! navigation. `SessionId` and any request-generation bookkeeping belong to
//! the UI layer that calls this module, not here, so this module never
//! depends on `hane-session` or GPUI.

use hane_document::{RopeBuffer, SourceOffset, SourceRange, TextBuffer};

/// A query longer than this is rejected outright rather than silently
/// truncated.
pub const MAX_QUERY_BYTES: usize = 4096;

/// Matches kept per scan. The 100,001st match (if confirmed) marks the result
/// `truncated` instead of growing this list further.
pub const MAX_MATCHES: usize = 100_000;

/// Byte size of the "core" region a single scan window is responsible for.
/// A window's fetched text extends past its core by [`WINDOW_SLACK_BYTES`] so
/// a match that starts near (but before) the core's end still has enough
/// haystack to complete without being mistaken for a shorter, unrelated
/// match. Keeping this bounded (rather than scanning line-by-line) is what
/// lets one arbitrarily large single line be scanned without materializing it
/// whole.
const WINDOW_CORE_BYTES: usize = 256 * 1024;

/// Every matched span consumes exactly `query.chars().count()` haystack
/// chars (see [`chars_match`]), each at most 4 UTF-8 bytes, so a query at the
/// [`MAX_QUERY_BYTES`] limit can never match a haystack span longer than
/// `MAX_QUERY_BYTES * 4` bytes. The slack is padded a little further to
/// absorb the handful of bytes [`floor_char_boundary`] may trim off a window
/// edge.
const WINDOW_SLACK_BYTES: usize = MAX_QUERY_BYTES * 4 + 8;

/// A window between cancellation checks, in scanned haystack characters. A
/// single failed match attempt costs at most `query.chars().count()`
/// comparisons, so this bounds how long a cancellation request can be
/// delayed regardless of how large the current line or window is.
const CANCEL_CHECK_INTERVAL: u32 = 2_048;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum FindQueryError {
    Empty,
    MultiLine,
    TooLong { bytes: usize, max: usize },
}

/// A validated, single-line, non-empty find query no larger than
/// [`MAX_QUERY_BYTES`]. Always matched literally: regex metacharacters in the
/// query have no special meaning.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct FindQuery(String);

impl FindQuery {
    /// # Errors
    ///
    /// Returns [`FindQueryError`] for an empty query, a query containing a
    /// line break, or one over [`MAX_QUERY_BYTES`].
    pub fn parse(raw: &str) -> Result<Self, FindQueryError> {
        if raw.is_empty() {
            return Err(FindQueryError::Empty);
        }
        if raw.contains(['\n', '\r']) {
            return Err(FindQueryError::MultiLine);
        }
        if raw.len() > MAX_QUERY_BYTES {
            return Err(FindQueryError::TooLong {
                bytes: raw.len(),
                max: MAX_QUERY_BYTES,
            });
        }
        Ok(Self(raw.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub struct FindOptions {
    pub case_sensitive: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FindResults {
    pub matches: Vec<SourceRange>,
    /// True only once a confirmed match beyond [`MAX_MATCHES`] was seen; a
    /// document with exactly `MAX_MATCHES` matches and no more is not
    /// truncated.
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FindScan {
    Completed(FindResults),
    Cancelled,
}

/// Scans `buffer` for every non-overlapping literal occurrence of `query` in
/// source order, in windows of bounded byte size so neither a huge document
/// nor a single huge line is ever copied whole. `is_cancelled` is polled
/// periodically (roughly every [`CANCEL_CHECK_INTERVAL`] match attempts) and
/// mid-line, never only at a line or window's end.
pub fn scan(
    buffer: &RopeBuffer,
    query: &FindQuery,
    options: FindOptions,
    mut is_cancelled: impl FnMut() -> bool,
) -> FindScan {
    let query_chars: Vec<char> = query.as_str().chars().collect();
    let total = buffer.len_bytes().0;
    let mut matches: Vec<SourceRange> = Vec::new();
    let mut truncated = false;
    let mut window_search_start = 0usize;
    let mut checks_since_cancel_poll = 0u32;

    'windows: while window_search_start < total {
        if is_cancelled() {
            return FindScan::Cancelled;
        }
        let window_search_end =
            floor_char_boundary(buffer, (window_search_start + WINDOW_CORE_BYTES).min(total));
        let fetch_end = floor_char_boundary(
            buffer,
            (window_search_end + WINDOW_SLACK_BYTES).min(total),
        );
        let text = buffer
            .text(SourceRange::new(window_search_start, fetch_end))
            .expect("window bounds are validated char boundaries within the buffer");
        let chars: Vec<(usize, char)> = text.char_indices().collect();

        // A match accepted in this window (its start is before
        // `window_search_end`) may still extend past `window_search_end`
        // into the slack region. The next window must resume after such a
        // match's end, not at `window_search_end` itself, or the tail of
        // that match would be rescanned as fresh candidate starts and could
        // spuriously overlap with what was already matched.
        let mut resume_from = window_search_end;
        let mut idx = 0usize;
        while idx < chars.len() {
            let (rel_byte, _) = chars[idx];
            let abs_byte = window_search_start + rel_byte;
            if abs_byte >= window_search_end {
                break;
            }
            checks_since_cancel_poll += 1;
            if checks_since_cancel_poll >= CANCEL_CHECK_INTERVAL {
                checks_since_cancel_poll = 0;
                if is_cancelled() {
                    return FindScan::Cancelled;
                }
            }
            if let Some(match_end_idx) =
                try_match(&chars, idx, &query_chars, options.case_sensitive)
            {
                let end_rel = chars
                    .get(match_end_idx)
                    .map_or(text.len(), |(byte, _)| *byte);
                let end_abs = window_search_start + end_rel;
                if matches.len() >= MAX_MATCHES {
                    truncated = true;
                    break 'windows;
                }
                matches.push(SourceRange::new(abs_byte, end_abs));
                resume_from = resume_from.max(end_abs);
                idx = match_end_idx;
            } else {
                idx += 1;
            }
        }
        window_search_start = resume_from;
    }

    FindScan::Completed(FindResults { matches, truncated })
}

fn try_match(
    chars: &[(usize, char)],
    start: usize,
    query: &[char],
    case_sensitive: bool,
) -> Option<usize> {
    if start + query.len() > chars.len() {
        return None;
    }
    for (offset, &q) in query.iter().enumerate() {
        let (_, h) = chars[start + offset];
        if !chars_match(h, q, case_sensitive) {
            return None;
        }
    }
    Some(start + query.len())
}

/// Unicode case-insensitive per character: two characters match when equal,
/// or when their (possibly multi-character) lowercasings are equal. This is
/// deliberately not full Unicode case folding or NFC normalization; matching
/// stays a simple, predictable one-haystack-character-per-query-character
/// walk with no realignment across a lowercasing that changes character
/// count, so byte offsets always come directly from the original text.
fn chars_match(haystack: char, query: char, case_sensitive: bool) -> bool {
    if case_sensitive {
        haystack == query
    } else {
        haystack == query || haystack.to_lowercase().eq(query.to_lowercase())
    }
}

/// The largest byte offset `<= byte` that is a valid character boundary in
/// `buffer`. UTF-8 guarantees a boundary within 3 bytes of any position, so
/// this always terminates quickly.
fn floor_char_boundary(buffer: &RopeBuffer, byte: usize) -> usize {
    let total = buffer.len_bytes().0;
    let mut candidate = byte.min(total);
    while candidate > 0 && buffer.validate_offset(SourceOffset(candidate)).is_err() {
        candidate -= 1;
    }
    candidate
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NavigationStep {
    pub index: usize,
    /// True when this step crossed from the last match back to the first
    /// (`next`) or from the first back to the last (`previous`), so the
    /// caller can show a short wrap notification.
    pub wrapped: bool,
}

/// Match navigation over an already-computed, source-ordered, non-overlapping
/// match list. Holds no state of its own: the caller (the UI's request
/// envelope) is the source of truth for which index is current.
pub struct FindNavigation;

impl FindNavigation {
    /// The match to land on when a find bar first opens (or its query
    /// changes): `seeded_selection`, if it names exactly one of `matches`,
    /// wins outright (requirement: starting from a selected occurrence keeps
    /// that same occurrence current); otherwise the first match at or after
    /// `anchor`, wrapping to the first match in the document if none is at or
    /// after it.
    #[must_use]
    pub fn initial_index(
        matches: &[SourceRange],
        anchor: SourceOffset,
        seeded_selection: Option<SourceRange>,
    ) -> Option<usize> {
        if let Some(seed) = seeded_selection {
            if let Some(index) = matches.iter().position(|m| *m == seed) {
                return Some(index);
            }
        }
        matches
            .iter()
            .position(|m| m.start >= anchor)
            .or(if matches.is_empty() { None } else { Some(0) })
    }

    #[must_use]
    pub fn next(matches_len: usize, current: Option<usize>) -> Option<NavigationStep> {
        if matches_len == 0 {
            return None;
        }
        Some(match current {
            Some(index) if index + 1 < matches_len => NavigationStep {
                index: index + 1,
                wrapped: false,
            },
            current => NavigationStep {
                index: 0,
                wrapped: current.is_some(),
            },
        })
    }

    #[must_use]
    pub fn previous(matches_len: usize, current: Option<usize>) -> Option<NavigationStep> {
        if matches_len == 0 {
            return None;
        }
        Some(match current {
            Some(index) if index > 0 => NavigationStep {
                index: index - 1,
                wrapped: false,
            },
            current => NavigationStep {
                index: matches_len - 1,
                wrapped: current.is_some(),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(text: &str, query: &str, case_sensitive: bool) -> FindResults {
        let buffer = RopeBuffer::from_text(text);
        let query = FindQuery::parse(query).unwrap();
        match scan(&buffer, &query, FindOptions { case_sensitive }, || false) {
            FindScan::Completed(results) => results,
            FindScan::Cancelled => panic!("scan reported cancelled with a never-cancelling check"),
        }
    }

    #[test]
    fn empty_document_has_no_matches() {
        let result = matches("", "needle", true);
        assert_eq!(result.matches, Vec::new());
        assert!(!result.truncated);
    }

    #[test]
    fn no_occurrence_has_no_matches() {
        let result = matches("hello world", "xyz", true);
        assert_eq!(result.matches, Vec::new());
    }

    #[test]
    fn single_occurrence_is_found() {
        let result = matches("hello world", "world", true);
        assert_eq!(result.matches, vec![SourceRange::new(6, 11)]);
    }

    #[test]
    fn multiple_occurrences_are_found_in_source_order_and_non_overlapping() {
        let result = matches("aa aa aa", "aa", true);
        assert_eq!(
            result.matches,
            vec![
                SourceRange::new(0, 2),
                SourceRange::new(3, 5),
                SourceRange::new(6, 8),
            ]
        );
    }

    #[test]
    fn overlapping_candidates_are_not_double_counted() {
        // "aaaa" contains two non-overlapping "aa" matches, not three.
        let result = matches("aaaa", "aa", true);
        assert_eq!(
            result.matches,
            vec![SourceRange::new(0, 2), SourceRange::new(2, 4)]
        );
    }

    #[test]
    fn case_sensitive_by_default_rejects_different_case() {
        let result = matches("Hello hello HELLO", "hello", true);
        assert_eq!(result.matches, vec![SourceRange::new(6, 11)]);
    }

    #[test]
    fn case_insensitive_matches_ascii_case_variants() {
        let result = matches("Hello hello HELLO", "hello", false);
        assert_eq!(
            result.matches,
            vec![
                SourceRange::new(0, 5),
                SourceRange::new(6, 11),
                SourceRange::new(12, 17),
            ]
        );
    }

    #[test]
    fn case_insensitive_matches_unicode_case_variants() {
        // German ß has no uppercase pair under simple lowercasing, but Latin
        // diacritics do: É lowercases to é.
        let result = matches("café CAFÉ Café", "café", false);
        assert_eq!(result.matches.len(), 3);
    }

    #[test]
    fn japanese_text_is_matched_by_byte_exact_ranges() {
        let text = "日本語のテキストを検索します";
        let result = matches(text, "テキスト", true);
        let start = text.find("テキスト").unwrap();
        let end = start + "テキスト".len();
        assert_eq!(result.matches, vec![SourceRange::new(start, end)]);
    }

    #[test]
    fn emoji_and_combining_characters_do_not_corrupt_byte_offsets() {
        let text = "a🙂b e\u{301}f 🙂";
        let result = matches(text, "🙂", true);
        assert_eq!(result.matches.len(), 2);
        for range in &result.matches {
            assert_eq!(&text[range.as_usize()], "🙂");
        }
        let combining = matches(text, "e\u{301}", true);
        assert_eq!(combining.matches.len(), 1);
        assert_eq!(&text[combining.matches[0].as_usize()], "e\u{301}");
    }

    #[test]
    fn regex_metacharacters_are_matched_literally() {
        let result = matches("a.b*c a.b*c literal", "a.b*c", true);
        assert_eq!(result.matches.len(), 2);
    }

    #[test]
    fn crlf_and_bare_cr_line_endings_do_not_shift_byte_offsets() {
        let text = "foo\r\nbar\rbaz\nbar";
        let result = matches(text, "bar", true);
        let expected: Vec<SourceRange> = text
            .match_indices("bar")
            .map(|(start, matched)| SourceRange::new(start, start + matched.len()))
            .collect();
        assert_eq!(result.matches, expected);
    }

    #[test]
    fn a_match_straddling_a_scan_window_boundary_is_found_once() {
        let boundary = WINDOW_CORE_BYTES;
        let mut text = "b".repeat(boundary - 2);
        text.push_str("needle");
        text.push_str(&"b".repeat(1024));
        let result = matches(&text, "needle", true);
        assert_eq!(
            result.matches,
            vec![SourceRange::new(boundary - 2, boundary - 2 + "needle".len())]
        );
    }

    #[test]
    fn a_match_starting_exactly_at_a_scan_window_boundary_is_found() {
        let boundary = WINDOW_CORE_BYTES;
        let mut text = "b".repeat(boundary);
        text.push_str("needle");
        text.push_str(&"b".repeat(1024));
        let result = matches(&text, "needle", true);
        assert_eq!(
            result.matches,
            vec![SourceRange::new(boundary, boundary + "needle".len())]
        );
    }

    #[test]
    fn results_below_the_cap_are_not_marked_truncated() {
        let text = "a".repeat(MAX_MATCHES);
        let result = matches(&text, "a", true);
        assert_eq!(result.matches.len(), MAX_MATCHES);
        assert!(!result.truncated);
    }

    #[test]
    fn a_confirmed_match_beyond_the_cap_marks_results_truncated() {
        let text = "a".repeat(MAX_MATCHES + 1);
        let result = matches(&text, "a", true);
        assert_eq!(result.matches.len(), MAX_MATCHES);
        assert!(result.truncated);
    }

    #[test]
    fn scan_reports_cancelled_when_the_check_returns_true() {
        let buffer = RopeBuffer::from_text(&"needle ".repeat(10_000));
        let query = FindQuery::parse("needle").unwrap();
        let mut calls = 0u32;
        let outcome = scan(&buffer, &query, FindOptions::default(), || {
            calls += 1;
            calls > 1
        });
        assert_eq!(outcome, FindScan::Cancelled);
    }

    #[test]
    fn query_validation_rejects_empty_multiline_and_oversized_queries() {
        assert_eq!(FindQuery::parse(""), Err(FindQueryError::Empty));
        assert_eq!(FindQuery::parse("a\nb"), Err(FindQueryError::MultiLine));
        assert_eq!(FindQuery::parse("a\rb"), Err(FindQueryError::MultiLine));
        let oversized = "a".repeat(MAX_QUERY_BYTES + 1);
        assert_eq!(
            FindQuery::parse(&oversized),
            Err(FindQueryError::TooLong {
                bytes: MAX_QUERY_BYTES + 1,
                max: MAX_QUERY_BYTES,
            })
        );
        assert!(FindQuery::parse(&"a".repeat(MAX_QUERY_BYTES)).is_ok());
    }

    #[test]
    fn initial_index_prefers_the_seeded_selection_when_it_is_a_match() {
        let ranges = vec![
            SourceRange::new(0, 3),
            SourceRange::new(10, 13),
            SourceRange::new(20, 23),
        ];
        let index = FindNavigation::initial_index(
            &ranges,
            SourceOffset(0),
            Some(SourceRange::new(10, 13)),
        );
        assert_eq!(index, Some(1));
    }

    #[test]
    fn initial_index_falls_back_to_first_match_at_or_after_anchor() {
        let ranges = vec![SourceRange::new(0, 3), SourceRange::new(10, 13)];
        assert_eq!(
            FindNavigation::initial_index(&ranges, SourceOffset(5), None),
            Some(1)
        );
        assert_eq!(
            FindNavigation::initial_index(&ranges, SourceOffset(50), None),
            Some(0)
        );
        assert_eq!(FindNavigation::initial_index(&[], SourceOffset(0), None), None);
    }

    #[test]
    fn next_and_previous_wrap_at_the_ends_and_are_safe_for_zero_or_one_matches() {
        assert_eq!(FindNavigation::next(0, None), None);
        assert_eq!(
            FindNavigation::next(1, Some(0)),
            Some(NavigationStep {
                index: 0,
                wrapped: true
            })
        );
        assert_eq!(
            FindNavigation::next(3, None),
            Some(NavigationStep {
                index: 0,
                wrapped: false
            })
        );
        assert_eq!(
            FindNavigation::next(3, Some(2)),
            Some(NavigationStep {
                index: 0,
                wrapped: true
            })
        );
        assert_eq!(
            FindNavigation::previous(3, Some(0)),
            Some(NavigationStep {
                index: 2,
                wrapped: true
            })
        );
        assert_eq!(
            FindNavigation::previous(3, None),
            Some(NavigationStep {
                index: 2,
                wrapped: false
            })
        );
    }
}
