//! GPUI-independent editor commands, cursor/selection, and IME transactions.

use hane_document::{
    Anchor, Bias, BufferError, EditSummary, Revision, RopeBuffer, SourceOffset, SourceRange,
    TextBuffer, TransactionId,
};
use std::ops::Range;
use std::time::{Duration, Instant};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Selection {
    pub anchor: SourceOffset,
    pub active: SourceOffset,
}

impl Selection {
    pub fn caret(offset: SourceOffset) -> Self {
        Self {
            anchor: offset,
            active: offset,
        }
    }
    pub fn range(self) -> SourceRange {
        SourceRange {
            start: self.anchor.min(self.active),
            end: self.anchor.max(self.active),
        }
    }
    pub fn is_reversed(self) -> bool {
        self.active < self.anchor
    }
}

#[derive(Clone, Debug)]
pub struct ImeState {
    pub active: bool,
    pub base_revision: Revision,
    pub transaction_id: TransactionId,
    pub original_range: SourceRange,
    pub original_text: String,
    pub current_range: SourceRange,
    pub marked_text: String,
    pub selected_utf16_range: Range<usize>,
    pub cursor_affinity: Bias,
    pub start_anchor: Anchor,
    pub end_anchor: Anchor,
    pub original_selection: Selection,
    expected_revision: Revision,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImeCancelOutcome {
    Restored,
    Conflict,
    Inactive,
}

#[derive(Clone, Debug)]
pub struct InputMeasurement {
    pub sequence: u64,
    pub received_at: Instant,
    pub model_updated_at: Instant,
    pub frame_painted_at: Option<Instant>,
}

impl InputMeasurement {
    pub fn keystroke_to_model(&self) -> Duration {
        self.model_updated_at.duration_since(self.received_at)
    }
    pub fn keystroke_to_frame(&self) -> Option<Duration> {
        self.frame_painted_at
            .map(|t| t.duration_since(self.received_at))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EditorCommand<'a> {
    Insert(&'a str),
    Backspace,
    Delete,
    MoveLeft { extend: bool },
    MoveRight { extend: bool },
    MoveUp { extend: bool },
    MoveDown { extend: bool },
    MoveToStart { extend: bool },
    MoveToEnd { extend: bool },
    SelectAll,
}

pub struct Editor {
    document: RopeBuffer,
    selection: Selection,
    preferred_grapheme_column: Option<usize>,
    ime: Option<ImeState>,
    next_transaction: u64,
    next_input_sequence: u64,
    pending_measurements: Vec<InputMeasurement>,
}

impl Editor {
    pub fn new(text: &str) -> Self {
        Self {
            document: RopeBuffer::from_text(text),
            selection: Selection::caret(SourceOffset(0)),
            preferred_grapheme_column: None,
            ime: None,
            next_transaction: 1,
            next_input_sequence: 1,
            pending_measurements: Vec::new(),
        }
    }
    pub fn document(&self) -> &RopeBuffer {
        &self.document
    }
    pub fn selection(&self) -> Selection {
        self.selection
    }
    pub fn ime(&self) -> Option<&ImeState> {
        self.ime.as_ref()
    }
    pub fn set_selection(&mut self, selection: Selection) -> Result<(), BufferError> {
        self.commit_composition();
        self.document.validate_offset(selection.anchor)?;
        self.document.validate_offset(selection.active)?;
        self.selection = selection;
        self.preferred_grapheme_column = None;
        Ok(())
    }

    pub fn dispatch(
        &mut self,
        command: EditorCommand<'_>,
    ) -> Result<Option<EditSummary>, BufferError> {
        let received = Instant::now();
        self.commit_composition();
        if !matches!(
            command,
            EditorCommand::MoveUp { .. } | EditorCommand::MoveDown { .. }
        ) {
            self.preferred_grapheme_column = None;
        }
        let edit = match command {
            EditorCommand::Insert(text) => Some(self.replace_selection(text)?),
            EditorCommand::Backspace => self.backspace()?,
            EditorCommand::Delete => self.delete()?,
            EditorCommand::MoveLeft { extend } => {
                self.move_grapheme(false, extend)?;
                None
            }
            EditorCommand::MoveRight { extend } => {
                self.move_grapheme(true, extend)?;
                None
            }
            EditorCommand::MoveUp { extend } => {
                self.move_vertical(false, extend)?;
                None
            }
            EditorCommand::MoveDown { extend } => {
                self.move_vertical(true, extend)?;
                None
            }
            EditorCommand::MoveToStart { extend } => {
                self.move_to(SourceOffset(0), extend);
                None
            }
            EditorCommand::MoveToEnd { extend } => {
                self.move_to(SourceOffset(self.document.len_bytes().0), extend);
                None
            }
            EditorCommand::SelectAll => {
                self.selection = Selection {
                    anchor: SourceOffset(0),
                    active: SourceOffset(self.document.len_bytes().0),
                };
                None
            }
        };
        self.record_model_update(received);
        Ok(edit)
    }

    /// Inserts text through the same command path used by interactive editing.
    ///
    /// This is intentionally public so regression tests can describe input as a
    /// sequence of cursor commands followed by text insertion.
    pub fn insert_text(&mut self, text: &str) -> Result<EditSummary, BufferError> {
        Ok(self
            .dispatch(EditorCommand::Insert(text))?
            .expect("inserting text always produces an edit"))
    }

    fn record_model_update(&mut self, received_at: Instant) {
        self.pending_measurements.push(InputMeasurement {
            sequence: self.next_input_sequence,
            received_at,
            model_updated_at: Instant::now(),
            frame_painted_at: None,
        });
        self.next_input_sequence += 1;
        if self.pending_measurements.len() > 2_048 {
            self.pending_measurements.drain(..1_024);
        }
    }

    pub fn mark_frame_painted(&mut self) -> Vec<InputMeasurement> {
        let now = Instant::now();
        let mut completed = Vec::new();
        for measurement in &mut self.pending_measurements {
            if measurement.frame_painted_at.is_none() {
                measurement.frame_painted_at = Some(now);
                completed.push(measurement.clone());
            }
        }
        self.pending_measurements.clear();
        completed
    }

    fn replace_selection(&mut self, text: &str) -> Result<EditSummary, BufferError> {
        let summary = self.document.edit(self.selection.range(), text)?;
        self.selection = Selection::caret(summary.range_after.end);
        Ok(summary)
    }

    fn previous_grapheme(&self, offset: SourceOffset) -> Result<SourceOffset, BufferError> {
        self.document.validate_offset(offset)?;
        if offset.0 == 0 {
            return Ok(offset);
        }
        let line = self.document.line_for_offset(offset)?;
        let line_start = self
            .document
            .offset_for_line_col(line, hane_document::LineCol(0))?
            .0;
        if offset.0 == line_start && line.0 > 0 {
            return Ok(self
                .line_content_range(hane_document::LineId(line.0 - 1))?
                .end);
        }
        let text = self.document.text(SourceRange::new(line_start, offset.0))?;
        Ok(SourceOffset(
            line_start
                + text
                    .grapheme_indices(true)
                    .next_back()
                    .map(|(i, _)| i)
                    .unwrap_or(0),
        ))
    }

    fn next_grapheme(&self, offset: SourceOffset) -> Result<SourceOffset, BufferError> {
        self.document.validate_offset(offset)?;
        let len = self.document.len_bytes().0;
        if offset.0 == len {
            return Ok(offset);
        }
        let line = self.document.line_for_offset(offset)?;
        let next_line = line.0 + 1;
        let line_end = if next_line < self.document.line_count() {
            self.document
                .offset_for_line_col(hane_document::LineId(next_line), hane_document::LineCol(0))?
                .0
        } else {
            len
        };
        let text = self.document.text(SourceRange::new(offset.0, line_end))?;
        let next = text
            .grapheme_indices(true)
            .nth(1)
            .map(|(i, _)| i)
            .unwrap_or(text.len());
        Ok(SourceOffset(offset.0 + next))
    }

    fn move_grapheme(&mut self, right: bool, extend: bool) -> Result<(), BufferError> {
        let target = if !extend && !self.selection.range().is_empty() {
            if right {
                self.selection.range().end
            } else {
                self.selection.range().start
            }
        } else if right {
            self.next_grapheme(self.selection.active)?
        } else {
            self.previous_grapheme(self.selection.active)?
        };
        self.move_to(target, extend);
        Ok(())
    }

    fn move_vertical(&mut self, down: bool, extend: bool) -> Result<(), BufferError> {
        let current_line = self.document.line_for_offset(self.selection.active)?;
        let current_range = self.line_content_range(current_line)?;
        let current_prefix = self.document.text(SourceRange {
            start: current_range.start,
            end: self.selection.active.min(current_range.end),
        })?;
        let preferred_column = *self
            .preferred_grapheme_column
            .get_or_insert_with(|| current_prefix.graphemes(true).count());

        let target_line = if down {
            let next = current_line.0 + 1;
            if next >= self.document.line_count() {
                return Ok(());
            }
            hane_document::LineId(next)
        } else {
            let Some(previous) = current_line.0.checked_sub(1) else {
                return Ok(());
            };
            hane_document::LineId(previous)
        };
        let target_range = self.line_content_range(target_line)?;
        let target_text = self.document.text(target_range)?;
        let relative_offset = target_text
            .grapheme_indices(true)
            .nth(preferred_column)
            .map(|(offset, _)| offset)
            .unwrap_or(target_text.len());
        self.move_to(SourceOffset(target_range.start.0 + relative_offset), extend);
        Ok(())
    }

    fn line_content_range(&self, line: hane_document::LineId) -> Result<SourceRange, BufferError> {
        let start = self
            .document
            .offset_for_line_col(line, hane_document::LineCol(0))?;
        let raw_end = if line.0 + 1 < self.document.line_count() {
            self.document
                .offset_for_line_col(hane_document::LineId(line.0 + 1), hane_document::LineCol(0))?
        } else {
            SourceOffset(self.document.len_bytes().0)
        };
        let raw_text = self.document.text(SourceRange {
            start,
            end: raw_end,
        })?;
        let content = if let Some(without_lf) = raw_text.strip_suffix('\n') {
            without_lf.strip_suffix('\r').unwrap_or(without_lf)
        } else {
            &raw_text
        };
        let content_len = content.len();
        Ok(SourceRange::new(start.0, start.0 + content_len))
    }
    fn move_to(&mut self, target: SourceOffset, extend: bool) {
        if extend {
            self.selection.active = target;
        } else {
            self.selection = Selection::caret(target);
        }
    }

    fn backspace(&mut self) -> Result<Option<EditSummary>, BufferError> {
        if self.selection.range().is_empty() {
            let previous = self.previous_grapheme(self.selection.active)?;
            if previous == self.selection.active {
                return Ok(None);
            }
            self.selection.anchor = previous;
        }
        Ok(Some(self.replace_selection("")?))
    }
    fn delete(&mut self) -> Result<Option<EditSummary>, BufferError> {
        if self.selection.range().is_empty() {
            let next = self.next_grapheme(self.selection.active)?;
            if next == self.selection.active {
                return Ok(None);
            }
            self.selection.active = next;
        }
        Ok(Some(self.replace_selection("")?))
    }

    pub fn replace_and_mark_text(
        &mut self,
        replacement_utf16: Option<Range<usize>>,
        text: &str,
        selected_utf16: Option<Range<usize>>,
    ) -> Result<EditSummary, BufferError> {
        let received = Instant::now();
        self.preferred_grapheme_column = None;
        if self.ime.is_none() {
            let original_range = if let Some(range) = replacement_utf16 {
                self.utf16_range_to_source(range)?
            } else {
                self.selection.range()
            };
            let original_text = self.document.text(original_range)?;
            let id = TransactionId(self.next_transaction);
            self.next_transaction += 1;
            self.ime = Some(ImeState {
                active: true,
                base_revision: self.document.revision(),
                transaction_id: id,
                original_range,
                original_text,
                current_range: original_range,
                marked_text: String::new(),
                selected_utf16_range: 0..0,
                cursor_affinity: Bias::After,
                start_anchor: self.document.anchor(original_range.start, Bias::Before)?,
                end_anchor: self.document.anchor(original_range.end, Bias::After)?,
                original_selection: self.selection,
                expected_revision: self.document.revision(),
            });
        }
        let current = self.ime.as_ref().unwrap().current_range;
        let summary = self.document.edit(current, text)?;
        let selected = selected_utf16
            .unwrap_or_else(|| text.encode_utf16().count()..text.encode_utf16().count());
        let relative = utf16_range_to_byte(text, selected.clone());
        let ime = self.ime.as_mut().unwrap();
        ime.current_range = summary.range_after;
        ime.marked_text = text.to_owned();
        ime.selected_utf16_range = selected;
        ime.expected_revision = summary.revision_after;
        self.selection = Selection {
            anchor: SourceOffset(summary.range_after.start.0 + relative.start),
            active: SourceOffset(summary.range_after.start.0 + relative.end),
        };
        self.record_model_update(received);
        Ok(summary)
    }

    pub fn commit_text(
        &mut self,
        replacement_utf16: Option<Range<usize>>,
        text: &str,
    ) -> Result<EditSummary, BufferError> {
        let received = Instant::now();
        self.preferred_grapheme_column = None;
        let range = if let Some(range) = replacement_utf16 {
            self.utf16_range_to_source(range)?
        } else {
            self.ime
                .as_ref()
                .map(|i| i.current_range)
                .unwrap_or(self.selection.range())
        };
        let summary = self.document.edit(range, text)?;
        self.selection = Selection::caret(summary.range_after.end);
        self.ime = None;
        self.record_model_update(received);
        Ok(summary)
    }

    pub fn commit_composition(&mut self) {
        self.ime = None;
    }

    pub fn cancel_composition(&mut self) -> Result<ImeCancelOutcome, BufferError> {
        let Some(ime) = self.ime.take() else {
            return Ok(ImeCancelOutcome::Inactive);
        };
        if self.document.revision() != ime.expected_revision {
            return Ok(ImeCancelOutcome::Conflict);
        }
        let start = self
            .document
            .resolve_anchor(ime.start_anchor, ime.base_revision)?;
        let end = self
            .document
            .resolve_anchor(ime.end_anchor, ime.base_revision)?;
        let current = ime.current_range;
        if start != current.start || end != current.end {
            return Ok(ImeCancelOutcome::Conflict);
        }
        self.document.edit(current, &ime.original_text)?;
        self.selection = ime.original_selection;
        self.preferred_grapheme_column = None;
        Ok(ImeCancelOutcome::Restored)
    }

    pub fn source_range_to_utf16(&self, range: SourceRange) -> Result<Range<usize>, BufferError> {
        self.document.validate_range(range)?;
        let prefix = self.document.text(SourceRange::new(0, range.end.0))?;
        let start = prefix[..range.start.0].encode_utf16().count();
        Ok(start..prefix.encode_utf16().count())
    }
    pub fn utf16_range_to_source(&self, range: Range<usize>) -> Result<SourceRange, BufferError> {
        let text = self.document.full_text();
        let bytes = utf16_range_to_byte(&text, range);
        let source = SourceRange::new(bytes.start, bytes.end);
        self.document.validate_range(source)?;
        Ok(source)
    }
    pub fn text_for_utf16_range(
        &self,
        range: Range<usize>,
    ) -> Result<(String, Range<usize>), BufferError> {
        let source = self.utf16_range_to_source(range)?;
        Ok((
            self.document.text(source)?,
            self.source_range_to_utf16(source)?,
        ))
    }
}

pub fn utf16_range_to_byte(text: &str, range: Range<usize>) -> Range<usize> {
    fn one(text: &str, target: usize) -> usize {
        let mut utf16 = 0;
        for (byte, ch) in text.char_indices() {
            if utf16 >= target {
                return byte;
            }
            let next = utf16 + ch.len_utf16();
            if target < next {
                return byte;
            }
            utf16 = next;
        }
        text.len()
    }
    one(text, range.start)..one(text, range.end)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn grapheme_movement_keeps_combining_sequence_together() {
        let mut e = Editor::new("a\u{301}🙂羽");
        e.dispatch(EditorCommand::MoveRight { extend: false })
            .unwrap();
        assert_eq!(e.selection().active, SourceOffset(3));
        e.dispatch(EditorCommand::MoveRight { extend: false })
            .unwrap();
        assert_eq!(e.selection().active, SourceOffset(7));
    }
    #[test]
    fn ime_updates_replace_previous_marked_text_and_commit() {
        let mut e = Editor::new("abc");
        e.set_selection(Selection::caret(SourceOffset(1))).unwrap();
        e.replace_and_mark_text(None, "に", Some(1..1)).unwrap();
        e.replace_and_mark_text(None, "日本", Some(2..2)).unwrap();
        assert_eq!(e.document().full_text(), "a日本bc");
        e.commit_text(None, "日本").unwrap();
        assert_eq!(e.document().full_text(), "a日本bc");
        assert!(e.ime().is_none());
    }
    #[test]
    fn ime_cancel_restores_original_selection_and_text() {
        let mut e = Editor::new("a旧b");
        e.set_selection(Selection {
            anchor: SourceOffset(1),
            active: SourceOffset(4),
        })
        .unwrap();
        e.replace_and_mark_text(None, "新しい", Some(3..3)).unwrap();
        assert_eq!(e.cancel_composition().unwrap(), ImeCancelOutcome::Restored);
        assert_eq!(e.document().full_text(), "a旧b");
        assert_eq!(
            e.selection(),
            Selection {
                anchor: SourceOffset(1),
                active: SourceOffset(4)
            }
        );
    }
    #[test]
    fn utf16_conversion_handles_surrogate_pairs() {
        let e = Editor::new("a🙂羽");
        assert_eq!(
            e.utf16_range_to_source(1..3).unwrap(),
            SourceRange::new(1, 5)
        );
        assert_eq!(
            e.source_range_to_utf16(SourceRange::new(1, 8)).unwrap(),
            1..4
        );
    }
}
