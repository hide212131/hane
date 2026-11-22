//! The pieces of a document `context.get` returns, cut without copying more of
//! the document than the answer needs.

use crate::auth::sha256_hex;
use crate::frontmatter::{self, FrontMatter};
use hane_document::{SourceOffset, SourceRange, TextBuffer};
use serde_json::{Value, json};

/// The longest title returned, in Unicode scalar values.
pub const MAX_TITLE_CHARS: usize = 120;

/// `text` cut to at most `max` Unicode scalar values.
pub fn truncate_chars(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((end, _)) => text[..end].to_owned(),
        None => text.to_owned(),
    }
}

/// An opaque document id. It names one document of one window of one service
/// instance: it changes when the service restarts, when the tab is closed and
/// reopened, and when the document is replaced (reloaded or adopted); it stays
/// when an unsaved tab is first saved or renamed.
pub fn document_id(instance_id: &str, window: u64, session: u64, generation: u64) -> String {
    let digest = sha256_hex(format!("{instance_id}|{window}|{session}|{generation}").as_bytes());
    digest[..32].to_owned()
}

/// The `frontMatter` member of a `context.get` result.
pub fn front_matter_json(front_matter: &FrontMatter) -> Value {
    match front_matter {
        FrontMatter::Absent => json!({ "status": "absent" }),
        FrontMatter::Unavailable => json!({ "status": "unavailable" }),
        FrontMatter::Present { inner, range } => json!({
            "status": "present",
            "text": inner,
            "range": { "startByte": range.start.0, "endByte": range.end.0 },
        }),
    }
}

/// A scalar value is at most four UTF-8 bytes, so this many bytes always cover
/// `n` scalars.
const fn max_bytes(chars: usize) -> usize {
    chars * 4
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Slices {
    pub before: String,
    /// The byte offset `before` starts at.
    pub before_start: usize,
    /// More text precedes `before` in the document.
    pub before_truncated: bool,
    pub after: String,
    pub after_start: usize,
    pub after_truncated: bool,
}

/// The first byte at or after `offset` that starts a character.
fn boundary_up(buffer: &impl TextBuffer, mut offset: usize) -> usize {
    while buffer.validate_offset(SourceOffset(offset)).is_err() {
        offset += 1;
    }
    offset
}

/// The last byte at or before `offset` that starts a character.
fn boundary_down(buffer: &impl TextBuffer, mut offset: usize) -> usize {
    while buffer.validate_offset(SourceOffset(offset)).is_err() {
        offset -= 1;
    }
    offset
}

/// Up to `before_chars` scalar values before `cursor` and `after_chars` after
/// it, never splitting a UTF-8 sequence. `cursor` must be a valid offset.
pub fn slices(
    buffer: &impl TextBuffer,
    cursor: usize,
    before_chars: usize,
    after_chars: usize,
) -> Slices {
    let len = buffer.len_bytes().0;
    let window_start = boundary_up(buffer, cursor.saturating_sub(max_bytes(before_chars)));
    let head = buffer
        .text(SourceRange::new(window_start, cursor))
        .unwrap_or_default();
    let skip = head.chars().count().saturating_sub(before_chars);
    let kept_from = head
        .char_indices()
        .nth(skip)
        .map_or(head.len(), |(at, _)| at);
    let before = head[kept_from..].to_owned();
    let before_start = cursor - before.len();

    let window_end = boundary_down(buffer, len.min(cursor + max_bytes(after_chars)));
    let tail = buffer
        .text(SourceRange::new(cursor, window_end))
        .unwrap_or_default();
    let after_len = tail
        .char_indices()
        .nth(after_chars)
        .map_or(tail.len(), |(at, _)| at);
    let after = tail[..after_len].to_owned();
    Slices {
        before,
        before_truncated: before_start > 0,
        before_start,
        after_truncated: cursor + after.len() < len,
        after_start: cursor,
        after,
    }
}

/// The document's Front Matter, reading only the head of the buffer.
pub fn front_matter(buffer: &impl TextBuffer) -> FrontMatter {
    let len = buffer.len_bytes().0;
    let end = boundary_down(buffer, len.min(max_bytes(frontmatter::SEARCH_CHARS)));
    let head = buffer.text(SourceRange::new(0, end)).unwrap_or_default();
    frontmatter::extract(&head)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hane_document::RopeBuffer;

    #[test]
    fn cuts_by_scalar_values_around_the_cursor() {
        let buffer = RopeBuffer::from_text("あいうえお|かきくけこ");
        let cursor = "あいうえお|".len();
        let slices = slices(&buffer, cursor, 3, 2);
        assert_eq!(slices.before, "えお|");
        assert_eq!(slices.before_start, "あいう".len());
        assert!(slices.before_truncated);
        assert_eq!(slices.after, "かき");
        assert_eq!(slices.after_start, cursor);
        assert!(slices.after_truncated);
    }

    #[test]
    fn whole_document_is_not_truncated() {
        let buffer = RopeBuffer::from_text("abc");
        let slices = slices(&buffer, 1, 1_200, 200);
        assert_eq!((slices.before.as_str(), slices.after.as_str()), ("a", "bc"));
        assert!(!slices.before_truncated && !slices.after_truncated);
        let slices = super::slices(&buffer, 0, 10, 10);
        assert_eq!(slices.before, "");
        assert_eq!(slices.after, "abc");
    }

    #[test]
    fn four_byte_and_combining_sequences_are_never_split() {
        let text = "🙂🙂🙂a\u{301}🙂|🙂🙂";
        let buffer = RopeBuffer::from_text(text);
        let cursor = text.find('|').unwrap() + 1;
        // Scalars, not graphemes: 'a' and U+0301 count as two.
        let slices = slices(&buffer, cursor, 4, 1);
        assert_eq!(slices.before, "a\u{301}🙂|");
        assert_eq!(slices.after, "🙂");
        assert!(slices.before.is_char_boundary(0));
        let slices = super::slices(&buffer, cursor, 1, 1);
        assert_eq!(slices.before, "|");
    }

    #[test]
    fn zero_length_windows_are_empty() {
        let buffer = RopeBuffer::from_text("abc");
        let slices = slices(&buffer, 1, 0, 0);
        assert_eq!((slices.before.as_str(), slices.after.as_str()), ("", ""));
        assert!(slices.before_truncated && slices.after_truncated);
    }

    #[test]
    fn crlf_is_returned_as_is() {
        let buffer = RopeBuffer::from_text("a\r\nb\r\nc");
        let slices = slices(&buffer, 3, 10, 10);
        assert_eq!(slices.before, "a\r\n");
        assert_eq!(slices.after, "b\r\nc");
    }

    #[test]
    fn document_ids_are_stable_per_document_and_differ_otherwise() {
        let base = document_id("inst", 1, 2, 0);
        assert_eq!(base, document_id("inst", 1, 2, 0));
        assert_eq!(base.len(), 32);
        assert_ne!(base, document_id("inst", 1, 2, 1), "a replaced document");
        assert_ne!(base, document_id("inst", 1, 3, 0), "another tab");
        assert_ne!(base, document_id("inst", 2, 2, 0), "another window");
        assert_ne!(base, document_id("other", 1, 2, 0), "another instance");
    }

    #[test]
    fn titles_are_cut_by_scalar_values() {
        assert_eq!(truncate_chars("abc", 120), "abc");
        assert_eq!(truncate_chars(&"あ".repeat(130), 120).chars().count(), 120);
        assert_eq!(truncate_chars("🙂🙂🙂", 2), "🙂🙂");
    }

    #[test]
    fn front_matter_reads_through_the_buffer() {
        let buffer = RopeBuffer::from_text("---\ntitle: T\n---\n本文");
        assert!(matches!(front_matter(&buffer), FrontMatter::Present { .. }));
        assert_eq!(
            front_matter(&RopeBuffer::from_text("本文")),
            FrontMatter::Absent
        );
    }
}
