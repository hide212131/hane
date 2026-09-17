//! UI-independent UTF-8 source buffer and edit primitives.

use ropey::{Rope, RopeBuilder, RopeSlice};
use std::collections::VecDeque;
use std::fmt;
use std::io;
use std::ops::Range;

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub struct SourceOffset(pub usize);

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub struct ByteLen(pub usize);

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub struct CharLen(pub usize);

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub struct Revision(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct TransactionId(pub u64);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub struct LineId(pub usize);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub struct LineCol(pub usize);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub struct SourceRange {
    pub start: SourceOffset,
    pub end: SourceOffset,
}

impl SourceRange {
    #[must_use]
    pub const fn new(start: usize, end: usize) -> Self {
        Self {
            start: SourceOffset(start),
            end: SourceOffset(end),
        }
    }

    #[must_use]
    pub const fn empty(offset: usize) -> Self {
        Self::new(offset, offset)
    }

    #[must_use]
    pub const fn len_bytes(self) -> usize {
        self.end.0 - self.start.0
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start.0 == self.end.0
    }

    #[must_use]
    pub const fn as_usize(self) -> Range<usize> {
        self.start.0..self.end.0
    }

    #[must_use]
    pub fn intersects(self, other: Self) -> bool {
        self.start.0 < other.end.0 && other.start.0 < self.end.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Bias {
    Before,
    After,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct Anchor {
    pub offset: SourceOffset,
    pub bias: Bias,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BufferError {
    OffsetOutOfBounds { offset: SourceOffset, len: ByteLen },
    RangeOutOfBounds { range: SourceRange, len: ByteLen },
    InvalidRange { range: SourceRange },
    NotCharBoundary { offset: SourceOffset },
    InvalidLineColumn { line: LineId, col: LineCol },
    RevisionHistoryUnavailable { from: Revision, to: Revision },
}

impl fmt::Display for BufferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for BufferError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InverseEdit {
    pub range: SourceRange,
    pub replacement: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RevisionDelta {
    pub from_revision: Revision,
    pub to_revision: Revision,
    pub edited_source_range_before: SourceRange,
    pub edited_source_range_after: SourceRange,
    pub byte_delta: isize,
}

impl RevisionDelta {
    #[must_use]
    pub fn transform_offset(self, offset: SourceOffset, bias: Bias) -> Option<SourceOffset> {
        let before = self.edited_source_range_before;
        if offset.0 < before.start.0 || (offset.0 == before.start.0 && bias == Bias::Before) {
            return Some(offset);
        }
        if offset.0 > before.end.0 || (offset.0 == before.end.0 && !before.is_empty()) {
            return Some(SourceOffset(offset.0.checked_add_signed(self.byte_delta)?));
        }
        Some(match bias {
            Bias::Before => self.edited_source_range_after.start,
            Bias::After => self.edited_source_range_after.end,
        })
    }

    #[must_use]
    pub fn transform_range(self, range: SourceRange) -> Option<SourceRange> {
        if range.intersects(self.edited_source_range_before) {
            return None;
        }
        Some(SourceRange {
            start: self.transform_offset(range.start, Bias::Before)?,
            end: self.transform_offset(range.end, Bias::After)?,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EditSummary {
    pub revision_before: Revision,
    pub revision_after: Revision,
    pub range_before: SourceRange,
    pub range_after: SourceRange,
    pub inserted_bytes: ByteLen,
    pub deleted_bytes: ByteLen,
    pub inverse: InverseEdit,
    pub delta: RevisionDelta,
}

pub enum BufferSlice<'a> {
    Rope(RopeSlice<'a>),
}

impl fmt::Display for BufferSlice<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rope(slice) => write!(f, "{slice}"),
        }
    }
}

/// Common operations over the revision-tracked UTF-8 source buffer.
///
/// # Errors
///
/// Methods returning [`BufferError`] reject offsets or ranges outside the buffer,
/// invalid UTF-8 boundaries, and unavailable revision history as applicable.
#[allow(
    clippy::missing_errors_doc,
    reason = "the shared TextBuffer error contract is documented on the trait"
)]
pub trait TextBuffer {
    fn len_bytes(&self) -> ByteLen;
    fn len_chars(&self) -> CharLen;
    fn revision(&self) -> Revision;
    fn validate_offset(&self, offset: SourceOffset) -> Result<(), BufferError>;
    fn validate_range(&self, range: SourceRange) -> Result<(), BufferError>;
    fn slice(&self, range: SourceRange) -> Result<BufferSlice<'_>, BufferError>;
    fn text(&self, range: SourceRange) -> Result<String, BufferError>;
    fn edit(&mut self, range: SourceRange, replacement: &str) -> Result<EditSummary, BufferError>;
    fn line_for_offset(&self, offset: SourceOffset) -> Result<LineId, BufferError>;
    fn offset_for_line_col(&self, line: LineId, col: LineCol) -> Result<SourceOffset, BufferError>;
    fn line_range(&self, line: LineId) -> Result<SourceRange, BufferError>;
    fn line_content_range(&self, line: LineId) -> Result<SourceRange, BufferError>;
    fn byte_to_utf16(&self, offset: SourceOffset) -> Result<usize, BufferError>;
    fn utf16_to_byte(&self, offset: usize) -> SourceOffset;
    fn anchor(&self, offset: SourceOffset, bias: Bias) -> Result<Anchor, BufferError>;
}

/// CommonMark 0.31.2 treats LF, CRLF and a bare CR as the three source line
/// endings. Ropey's own line index (built with `unicode_lines` disabled, see
/// `Cargo.toml`) only recognizes `\n`, so a bare CR is invisible to it. Rather
/// than reimplement Ropey's incremental line-break bookkeeping, `line_mirror`
/// is a second rope kept byte-for-byte and char-for-char aligned with `rope`,
/// where every bare CR (a `\r` not immediately followed by `\n`) is replaced
/// with `\n`. A CRLF's `\r` is left untouched, so the pair still contributes
/// exactly one break, at the `\n`. Because the substitution is always one
/// single-byte ASCII character for another, byte and char offsets never shift
/// between the two ropes: any position valid in one is the same position in
/// the other. `rope` alone remains the source of truth for saved bytes and
/// text content; `line_mirror` is consulted only for line boundaries.
#[derive(Clone)]
pub struct RopeBuffer {
    rope: Rope,
    line_mirror: Rope,
    revision: Revision,
    deltas: VecDeque<RevisionDelta>,
    delta_capacity: usize,
}

/// Rewrites every bare CR in `s` to `\n` so a line-break-only-on-`\n` index can
/// see it. A CR immediately followed by `\n` (within `s`, or by
/// `trailing_is_lf` when the CR is `s`'s last byte) is a CRLF pair and is left
/// as `\r`. `\r` and `\n` are both single-byte ASCII, so this never changes
/// `s`'s length or its UTF-8 char boundaries.
fn mirror_bare_cr(s: &str, trailing_is_lf: bool) -> String {
    let bytes = s.as_bytes();
    let mut out = bytes.to_vec();
    for (i, byte) in bytes.iter().enumerate() {
        if *byte != b'\r' {
            continue;
        }
        let followed_by_lf = match bytes.get(i + 1) {
            Some(next) => *next == b'\n',
            None => trailing_is_lf,
        };
        if !followed_by_lf {
            out[i] = b'\n';
        }
    }
    String::from_utf8(out).expect("ASCII CR/LF substitution preserves UTF-8 validity")
}

/// Reads the char at `char_idx`, or `None` past the rope's end. Goes through
/// `slice` because `Rope` (unlike `RopeSlice`) has no direct single-char
/// accessor.
fn char_at(rope: &Rope, char_idx: usize) -> Option<char> {
    (char_idx < rope.len_chars()).then(|| rope.slice(char_idx..char_idx + 1).char(0))
}

/// Builds `line_mirror` for `rope` chunk by chunk, so constructing it never
/// materializes the whole document as one `String`.
fn build_line_mirror(rope: &Rope) -> Rope {
    let chunks: Vec<&str> = rope.chunks().collect();
    let mut builder = RopeBuilder::new();
    for (index, chunk) in chunks.iter().enumerate() {
        let trailing_is_lf = chunks[index + 1..]
            .iter()
            .find_map(|later| later.chars().next())
            == Some('\n');
        builder.append(&mirror_bare_cr(chunk, trailing_is_lf));
    }
    builder.finish()
}

impl Default for RopeBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl RopeBuffer {
    #[must_use]
    pub fn new() -> Self {
        Self::from_text("")
    }

    #[must_use]
    pub fn from_text(text: &str) -> Self {
        Self::from_rope(Rope::from_str(text))
    }

    /// Builds a buffer from a UTF-8 reader.
    ///
    /// # Errors
    ///
    /// Returns the I/O error produced while reading the source.
    pub fn from_reader(reader: impl io::Read) -> io::Result<Self> {
        Ok(Self::from_rope(Rope::from_reader(reader)?))
    }

    fn from_rope(rope: Rope) -> Self {
        let line_mirror = build_line_mirror(&rope);
        Self {
            rope,
            line_mirror,
            revision: Revision(0),
            deltas: VecDeque::new(),
            delta_capacity: 4_096,
        }
    }

    #[must_use]
    pub fn full_text(&self) -> String {
        self.rope.to_string()
    }

    /// Writes the current Rope without first materializing the full document as
    /// one contiguous `String`.
    ///
    /// # Errors
    ///
    /// Returns the I/O error produced while writing a source chunk.
    pub fn write_to(&self, mut writer: impl io::Write) -> io::Result<()> {
        for chunk in self.rope.chunks() {
            writer.write_all(chunk.as_bytes())?;
        }
        Ok(())
    }

    #[must_use]
    pub fn line_count(&self) -> usize {
        self.line_mirror.len_lines()
    }

    /// Resolves an anchor created at `from` to the current revision.
    ///
    /// # Errors
    ///
    /// Returns [`BufferError`] when the anchor is invalid or revision history is unavailable.
    pub fn resolve_anchor(
        &self,
        anchor: Anchor,
        from: Revision,
    ) -> Result<SourceOffset, BufferError> {
        if from == self.revision {
            self.validate_offset(anchor.offset)?;
            return Ok(anchor.offset);
        }
        let deltas = self.deltas_since(from)?;
        let mut offset = anchor.offset;
        for delta in deltas {
            offset = delta.transform_offset(offset, anchor.bias).ok_or(
                BufferError::RevisionHistoryUnavailable {
                    from,
                    to: self.revision,
                },
            )?;
        }
        self.validate_offset(offset)?;
        Ok(offset)
    }

    /// Returns the edits after `revision` in chronological order.
    ///
    /// # Errors
    ///
    /// Returns [`BufferError::RevisionHistoryUnavailable`] when the requested revision is absent.
    pub fn deltas_since(&self, revision: Revision) -> Result<Vec<RevisionDelta>, BufferError> {
        if revision == self.revision {
            return Ok(Vec::new());
        }
        let Some(first) = self.deltas.front().map(|delta| delta.from_revision) else {
            return Err(BufferError::RevisionHistoryUnavailable {
                from: revision,
                to: self.revision,
            });
        };
        if revision < first || revision > self.revision {
            return Err(BufferError::RevisionHistoryUnavailable {
                from: revision,
                to: self.revision,
            });
        }
        let result: Vec<_> = self
            .deltas
            .iter()
            .copied()
            .filter(|d| d.from_revision >= revision)
            .collect();
        if result.first().map(|d| d.from_revision) != Some(revision)
            || result.last().map(|d| d.to_revision) != Some(self.revision)
        {
            return Err(BufferError::RevisionHistoryUnavailable {
                from: revision,
                to: self.revision,
            });
        }
        Ok(result)
    }

    fn byte_to_char(&self, offset: SourceOffset) -> Result<usize, BufferError> {
        self.validate_offset(offset)?;
        Ok(self.rope.byte_to_char(offset.0))
    }

    fn is_char_boundary(&self, byte: usize) -> bool {
        if byte == self.rope.len_bytes() {
            return true;
        }
        let char_idx = self.rope.byte_to_char(byte);
        self.rope.char_to_byte(char_idx) == byte
    }
}

impl TextBuffer for RopeBuffer {
    fn len_bytes(&self) -> ByteLen {
        ByteLen(self.rope.len_bytes())
    }
    fn len_chars(&self) -> CharLen {
        CharLen(self.rope.len_chars())
    }
    fn revision(&self) -> Revision {
        self.revision
    }

    fn validate_offset(&self, offset: SourceOffset) -> Result<(), BufferError> {
        if offset.0 > self.rope.len_bytes() {
            return Err(BufferError::OffsetOutOfBounds {
                offset,
                len: self.len_bytes(),
            });
        }
        if !self.is_char_boundary(offset.0) {
            return Err(BufferError::NotCharBoundary { offset });
        }
        Ok(())
    }

    fn validate_range(&self, range: SourceRange) -> Result<(), BufferError> {
        if range.start.0 > range.end.0 {
            return Err(BufferError::InvalidRange { range });
        }
        if range.end.0 > self.rope.len_bytes() {
            return Err(BufferError::RangeOutOfBounds {
                range,
                len: self.len_bytes(),
            });
        }
        self.validate_offset(range.start)?;
        self.validate_offset(range.end)
    }

    fn slice(&self, range: SourceRange) -> Result<BufferSlice<'_>, BufferError> {
        let start = self.byte_to_char(range.start)?;
        let end = self.byte_to_char(range.end)?;
        if start > end {
            return Err(BufferError::InvalidRange { range });
        }
        Ok(BufferSlice::Rope(self.rope.slice(start..end)))
    }

    fn text(&self, range: SourceRange) -> Result<String, BufferError> {
        Ok(self.slice(range)?.to_string())
    }

    fn edit(&mut self, range: SourceRange, replacement: &str) -> Result<EditSummary, BufferError> {
        self.validate_range(range)?;
        let start_char = self.rope.byte_to_char(range.start.0);
        let end_char = self.rope.byte_to_char(range.end.0);
        let deleted = self.rope.slice(start_char..end_char).to_string();

        // Whether the character right after the edit (unaffected by it) is a
        // `\n`, needed both to mirror a `\r` trailing `replacement` and to
        // decide whether a `\r` immediately before the edit stays paired.
        let trailing_is_lf = char_at(&self.rope, end_char) == Some('\n');
        let mirrored_replacement = mirror_bare_cr(replacement, trailing_is_lf);
        // A `\r` immediately before the edit may change classification: the
        // edit can insert, remove, or change the character that used to
        // follow it. Every other `\r` in the document keeps its classification,
        // since nothing about its own neighbors changed.
        let left_reclassification = start_char
            .checked_sub(1)
            .filter(|&left| char_at(&self.rope, left) == Some('\r'))
            .map(|left| {
                let now_paired = if replacement.is_empty() {
                    trailing_is_lf
                } else {
                    replacement.starts_with('\n')
                };
                (left, if now_paired { '\r' } else { '\n' })
            })
            .filter(|(left, desired)| char_at(&self.line_mirror, *left) != Some(*desired));

        let before = self.revision;
        self.rope.remove(start_char..end_char);
        self.rope.insert(start_char, replacement);
        self.line_mirror.remove(start_char..end_char);
        self.line_mirror.insert(start_char, &mirrored_replacement);
        if let Some((left, desired)) = left_reclassification {
            self.line_mirror.remove(left..left + 1);
            self.line_mirror.insert(left, &desired.to_string());
        }
        debug_assert_eq!(self.rope.len_chars(), self.line_mirror.len_chars());
        self.revision = Revision(self.revision.0.checked_add(1).expect("revision overflow"));
        let range_after = SourceRange::new(range.start.0, range.start.0 + replacement.len());
        let delta = RevisionDelta {
            from_revision: before,
            to_revision: self.revision,
            edited_source_range_before: range,
            edited_source_range_after: range_after,
            byte_delta: replacement.len().cast_signed() - range.len_bytes().cast_signed(),
        };
        self.deltas.push_back(delta);
        if self.deltas.len() > self.delta_capacity {
            self.deltas.pop_front();
        }
        Ok(EditSummary {
            revision_before: before,
            revision_after: self.revision,
            range_before: range,
            range_after,
            inserted_bytes: ByteLen(replacement.len()),
            deleted_bytes: ByteLen(deleted.len()),
            inverse: InverseEdit {
                range: range_after,
                replacement: deleted,
            },
            delta,
        })
    }

    fn line_for_offset(&self, offset: SourceOffset) -> Result<LineId, BufferError> {
        let char_idx = self.byte_to_char(offset)?;
        Ok(LineId(self.line_mirror.char_to_line(char_idx)))
    }

    fn offset_for_line_col(&self, line: LineId, col: LineCol) -> Result<SourceOffset, BufferError> {
        let content_range = self.line_content_range(line)?;
        let start_char = self.rope.byte_to_char(content_range.start.0);
        let content_chars = self.rope.byte_to_char(content_range.end.0) - start_char;
        if col.0 > content_chars {
            return Err(BufferError::InvalidLineColumn { line, col });
        }
        Ok(SourceOffset(self.rope.char_to_byte(start_char + col.0)))
    }

    fn line_range(&self, line: LineId) -> Result<SourceRange, BufferError> {
        if line.0 >= self.line_mirror.len_lines() {
            return Err(BufferError::InvalidLineColumn {
                line,
                col: LineCol(0),
            });
        }
        let start_char = self.line_mirror.line_to_char(line.0);
        let end_char = if line.0 + 1 < self.line_mirror.len_lines() {
            self.line_mirror.line_to_char(line.0 + 1)
        } else {
            self.line_mirror.len_chars()
        };
        Ok(SourceRange::new(
            self.rope.char_to_byte(start_char),
            self.rope.char_to_byte(end_char),
        ))
    }

    /// `line_range` trimmed of its trailing line ending. The ending's bytes
    /// come from `rope`, the real source, never from `line_mirror`: a bare CR
    /// is real content there, not the synthetic `\n` `line_mirror` substitutes
    /// to make Ropey see the break.
    fn line_content_range(&self, line: LineId) -> Result<SourceRange, BufferError> {
        let range = self.line_range(line)?;
        let start_char = self.rope.byte_to_char(range.start.0);
        let mut content_chars = self.rope.byte_to_char(range.end.0) - start_char;
        if content_chars > 0 {
            match char_at(&self.rope, start_char + content_chars - 1) {
                Some('\n') => {
                    content_chars -= 1;
                    if content_chars > 0
                        && char_at(&self.rope, start_char + content_chars - 1) == Some('\r')
                    {
                        content_chars -= 1;
                    }
                }
                Some('\r') => content_chars -= 1,
                _ => {}
            }
        }
        Ok(SourceRange {
            start: range.start,
            end: SourceOffset(self.rope.char_to_byte(start_char + content_chars)),
        })
    }

    fn byte_to_utf16(&self, offset: SourceOffset) -> Result<usize, BufferError> {
        let char_offset = self.byte_to_char(offset)?;
        Ok(self
            .rope
            .slice(..char_offset)
            .chars()
            .map(char::len_utf16)
            .sum())
    }

    fn utf16_to_byte(&self, offset: usize) -> SourceOffset {
        let mut utf16 = 0;
        let mut bytes = 0;
        for ch in self.rope.chars() {
            if utf16 >= offset || offset < utf16 + ch.len_utf16() {
                break;
            }
            utf16 += ch.len_utf16();
            bytes += ch.len_utf8();
        }
        SourceOffset(bytes)
    }

    fn anchor(&self, offset: SourceOffset, bias: Bias) -> Result<Anchor, BufferError> {
        self.validate_offset(offset)?;
        Ok(Anchor { offset, bias })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_edit_and_inverse_are_byte_based() {
        let mut buffer = RopeBuffer::from_text("a日本語🙂z");
        let summary = buffer.edit(SourceRange::new(1, 10), "羽").unwrap();
        assert_eq!(buffer.full_text(), "a羽🙂z");
        assert_eq!(summary.inverse.replacement, "日本語");
        buffer
            .edit(summary.inverse.range, &summary.inverse.replacement)
            .unwrap();
        assert_eq!(buffer.full_text(), "a日本語🙂z");
        assert_eq!(buffer.revision(), Revision(2));
    }

    #[test]
    fn rejects_non_character_boundaries() {
        let buffer = RopeBuffer::from_text("羽");
        assert!(matches!(
            buffer.validate_offset(SourceOffset(1)),
            Err(BufferError::NotCharBoundary { .. })
        ));
    }

    #[test]
    fn lines_preserve_crlf_but_exclude_it_from_columns() {
        let buffer = RopeBuffer::from_text("ab\r\n日本\n");
        assert_eq!(
            buffer.offset_for_line_col(LineId(0), LineCol(2)).unwrap(),
            SourceOffset(2)
        );
        assert_eq!(
            buffer.offset_for_line_col(LineId(1), LineCol(2)).unwrap(),
            SourceOffset(10)
        );
        assert!(buffer.offset_for_line_col(LineId(0), LineCol(3)).is_err());
        assert_eq!(
            buffer.line_range(LineId(0)).unwrap(),
            SourceRange::new(0, 4)
        );
        assert_eq!(
            buffer.line_content_range(LineId(0)).unwrap(),
            SourceRange::new(0, 2)
        );
    }

    #[test]
    fn utf16_offsets_convert_without_flattening_the_rope() {
        let buffer = RopeBuffer::from_text("a🙂羽");
        assert_eq!(buffer.byte_to_utf16(SourceOffset(5)).unwrap(), 3);
        assert_eq!(buffer.utf16_to_byte(3), SourceOffset(5));
        assert_eq!(buffer.utf16_to_byte(2), SourceOffset(1));
        assert_eq!(buffer.utf16_to_byte(99), SourceOffset(8));
    }

    #[test]
    fn bare_cr_is_a_line_break_but_unicode_line_separators_are_not() {
        // CommonMark 0.31.2 recognizes only LF, CR and CRLF as line endings;
        // U+2028 LINE SEPARATOR is ordinary text.
        let buffer = RopeBuffer::from_text("a\rb\u{2028}c\nd");
        assert_eq!(buffer.line_count(), 3);
        assert_eq!(buffer.line_for_offset(SourceOffset(0)).unwrap(), LineId(0));
        assert_eq!(buffer.line_for_offset(SourceOffset(2)).unwrap(), LineId(1));
        assert_eq!(buffer.line_for_offset(SourceOffset(8)).unwrap(), LineId(2));
        assert_eq!(
            buffer.line_range(LineId(0)).unwrap(),
            SourceRange::new(0, 2)
        );
        assert_eq!(
            buffer.line_content_range(LineId(0)).unwrap(),
            SourceRange::new(0, 1)
        );
        assert_eq!(
            buffer.line_range(LineId(1)).unwrap(),
            SourceRange::new(2, 8)
        );
        assert_eq!(
            buffer.offset_for_line_col(LineId(0), LineCol(1)).unwrap(),
            SourceOffset(1)
        );
        assert!(buffer.offset_for_line_col(LineId(0), LineCol(2)).is_err());
    }

    #[test]
    fn two_bare_crs_in_a_row_make_an_empty_line_between_them() {
        // Mirrors the blank line a markdown parser reads between "a" and "b"
        // when the separator is `\r\r` instead of `\n\n`.
        let buffer = RopeBuffer::from_text("a\r\rb");
        assert_eq!(buffer.line_count(), 3);
        assert_eq!(
            buffer.line_content_range(LineId(0)).unwrap(),
            SourceRange::new(0, 1)
        );
        assert_eq!(
            buffer.line_content_range(LineId(1)).unwrap(),
            SourceRange::new(2, 2)
        );
        assert_eq!(
            buffer.line_content_range(LineId(2)).unwrap(),
            SourceRange::new(3, 4)
        );
    }

    #[test]
    fn mixed_lf_cr_and_crlf_line_endings_each_count_as_one_break() {
        let buffer = RopeBuffer::from_text("a\rb\r\nc\nd");
        assert_eq!(buffer.line_count(), 4);
        let ranges = (0..4)
            .map(|line| buffer.line_content_range(LineId(line)).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            ranges,
            vec![
                SourceRange::new(0, 1),
                SourceRange::new(2, 3),
                SourceRange::new(5, 6),
                SourceRange::new(7, 8),
            ]
        );
    }

    #[test]
    fn source_bytes_stay_exact_across_a_bare_cr_edit() {
        // The mirror rope used for line lookup never leaks into saved bytes.
        let mut buffer = RopeBuffer::from_text("a\rb");
        buffer.edit(SourceRange::empty(1), "X").unwrap();
        assert_eq!(buffer.full_text(), "aX\rb");
        let mut written = Vec::new();
        buffer.write_to(&mut written).unwrap();
        assert_eq!(written, b"aX\rb");
    }

    #[test]
    fn inserting_lf_after_a_bare_cr_turns_it_into_one_crlf_break() {
        let mut buffer = RopeBuffer::from_text("a\rb");
        assert_eq!(buffer.line_count(), 2);
        buffer.edit(SourceRange::empty(2), "\n").unwrap();
        assert_eq!(buffer.full_text(), "a\r\nb");
        assert_eq!(buffer.line_count(), 2);
        assert_eq!(
            buffer.line_range(LineId(0)).unwrap(),
            SourceRange::new(0, 3)
        );
    }

    #[test]
    fn deleting_the_lf_of_a_crlf_leaves_a_bare_cr_break() {
        let mut buffer = RopeBuffer::from_text("a\r\nb");
        assert_eq!(buffer.line_count(), 2);
        buffer.edit(SourceRange::new(2, 3), "").unwrap();
        assert_eq!(buffer.full_text(), "a\rb");
        assert_eq!(buffer.line_count(), 2);
        assert_eq!(
            buffer.line_range(LineId(0)).unwrap(),
            SourceRange::new(0, 2)
        );
    }

    #[test]
    fn anchors_follow_insert_bias_and_deletion() {
        let mut buffer = RopeBuffer::from_text("abcd");
        let before = buffer.anchor(SourceOffset(2), Bias::Before).unwrap();
        let after = buffer.anchor(SourceOffset(2), Bias::After).unwrap();
        let base = buffer.revision();
        buffer.edit(SourceRange::empty(2), "日").unwrap();
        assert_eq!(
            buffer.resolve_anchor(before, base).unwrap(),
            SourceOffset(2)
        );
        assert_eq!(buffer.resolve_anchor(after, base).unwrap(), SourceOffset(5));
    }

    #[test]
    fn non_overlapping_range_can_rebase() {
        let mut buffer = RopeBuffer::from_text("one two");
        let base = buffer.revision();
        buffer.edit(SourceRange::empty(0), "big ").unwrap();
        let delta = buffer.deltas_since(base).unwrap()[0];
        assert_eq!(
            delta.transform_range(SourceRange::new(4, 7)),
            Some(SourceRange::new(8, 11))
        );
    }

    #[test]
    fn reader_builds_rope_without_intermediate_document_string() {
        let input = std::io::Cursor::new("日本語\nplain text".as_bytes());
        let buffer = RopeBuffer::from_reader(input).unwrap();
        assert_eq!(buffer.full_text(), "日本語\nplain text");
        assert_eq!(buffer.line_count(), 2);
    }
}
