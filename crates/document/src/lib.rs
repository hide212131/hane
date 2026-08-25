//! UI-independent UTF-8 source buffer and edit primitives.

use ropey::{Rope, RopeSlice};
use std::collections::VecDeque;
use std::fmt;
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
    pub const fn new(start: usize, end: usize) -> Self {
        Self {
            start: SourceOffset(start),
            end: SourceOffset(end),
        }
    }

    pub const fn empty(offset: usize) -> Self {
        Self::new(offset, offset)
    }

    pub const fn len_bytes(self) -> usize {
        self.end.0 - self.start.0
    }

    pub const fn is_empty(self) -> bool {
        self.start.0 == self.end.0
    }

    pub const fn as_usize(self) -> Range<usize> {
        self.start.0..self.end.0
    }

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

#[derive(Clone)]
pub struct RopeBuffer {
    rope: Rope,
    revision: Revision,
    deltas: VecDeque<RevisionDelta>,
    delta_capacity: usize,
}

impl Default for RopeBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl RopeBuffer {
    pub fn new() -> Self {
        Self::from_text("")
    }

    pub fn from_text(text: &str) -> Self {
        Self {
            rope: Rope::from_str(text),
            revision: Revision(0),
            deltas: VecDeque::new(),
            delta_capacity: 4_096,
        }
    }

    pub fn full_text(&self) -> String {
        self.rope.to_string()
    }

    pub fn line_count(&self) -> usize {
        self.rope.len_lines()
    }

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

    pub fn deltas_since(&self, revision: Revision) -> Result<Vec<RevisionDelta>, BufferError> {
        if revision == self.revision {
            return Ok(Vec::new());
        }
        let first = self.deltas.front().map(|d| d.from_revision);
        if first.is_none() || revision < first.unwrap() || revision > self.revision {
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
        let before = self.revision;
        self.rope.remove(start_char..end_char);
        self.rope.insert(start_char, replacement);
        self.revision = Revision(self.revision.0.checked_add(1).expect("revision overflow"));
        let range_after = SourceRange::new(range.start.0, range.start.0 + replacement.len());
        let delta = RevisionDelta {
            from_revision: before,
            to_revision: self.revision,
            edited_source_range_before: range,
            edited_source_range_after: range_after,
            byte_delta: replacement.len() as isize - range.len_bytes() as isize,
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
        Ok(LineId(self.rope.char_to_line(char_idx)))
    }

    fn offset_for_line_col(&self, line: LineId, col: LineCol) -> Result<SourceOffset, BufferError> {
        if line.0 >= self.rope.len_lines() {
            return Err(BufferError::InvalidLineColumn { line, col });
        }
        let line_slice = self.rope.line(line.0);
        let content_chars = match line_slice.len_chars() {
            0 => 0,
            n if line_slice.char(n - 1) == '\n' => {
                if n >= 2 && line_slice.char(n - 2) == '\r' {
                    n - 2
                } else {
                    n - 1
                }
            }
            n => n,
        };
        if col.0 > content_chars {
            return Err(BufferError::InvalidLineColumn { line, col });
        }
        let char_idx = self.rope.line_to_char(line.0) + col.0;
        Ok(SourceOffset(self.rope.char_to_byte(char_idx)))
    }

    fn line_range(&self, line: LineId) -> Result<SourceRange, BufferError> {
        if line.0 >= self.rope.len_lines() {
            return Err(BufferError::InvalidLineColumn {
                line,
                col: LineCol(0),
            });
        }
        let start = self.rope.line_to_byte(line.0);
        let end = if line.0 + 1 < self.rope.len_lines() {
            self.rope.line_to_byte(line.0 + 1)
        } else {
            self.rope.len_bytes()
        };
        Ok(SourceRange::new(start, end))
    }

    fn line_content_range(&self, line: LineId) -> Result<SourceRange, BufferError> {
        let range = self.line_range(line)?;
        let line_slice = self.rope.line(line.0);
        let mut content_chars = line_slice.len_chars();
        if content_chars > 0 && line_slice.char(content_chars - 1) == '\n' {
            content_chars -= 1;
            if content_chars > 0 && line_slice.char(content_chars - 1) == '\r' {
                content_chars -= 1;
            }
        }
        Ok(SourceRange {
            start: range.start,
            end: SourceOffset(range.start.0 + line_slice.char_to_byte(content_chars)),
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
    fn only_lf_and_crlf_are_line_breaks() {
        let buffer = RopeBuffer::from_text("a\rb\u{2028}c\nd");
        assert_eq!(buffer.line_count(), 2);
        assert_eq!(
            buffer.offset_for_line_col(LineId(0), LineCol(5)).unwrap(),
            SourceOffset(7)
        );
        assert_eq!(buffer.line_for_offset(SourceOffset(3)).unwrap(), LineId(0));
        assert_eq!(buffer.line_for_offset(SourceOffset(8)).unwrap(), LineId(1));
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
}
