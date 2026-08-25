use crate::{Editor, InputMeasurementKind, Selection};
use hane_document::{
    Anchor, Bias, BufferError, EditSummary, Revision, SourceOffset, SourceRange, TextBuffer,
    TransactionId,
};
use std::ops::Range;
use std::time::Instant;

#[derive(Clone, Debug)]
pub struct ImeState {
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

impl ImeState {
    fn begin(
        editor: &Editor,
        transaction_id: TransactionId,
        range: SourceRange,
    ) -> Result<Self, BufferError> {
        Ok(Self {
            base_revision: editor.document.revision(),
            transaction_id,
            original_range: range,
            original_text: editor.document.text(range)?,
            current_range: range,
            marked_text: String::new(),
            selected_utf16_range: 0..0,
            cursor_affinity: Bias::After,
            start_anchor: editor.document.anchor(range.start, Bias::Before)?,
            end_anchor: editor.document.anchor(range.end, Bias::After)?,
            original_selection: editor.selection,
            expected_revision: editor.document.revision(),
        })
    }

    fn update(&mut self, summary: &EditSummary, text: &str, selected_utf16: Range<usize>) {
        self.current_range = summary.range_after;
        self.marked_text = text.to_owned();
        self.selected_utf16_range = selected_utf16;
        self.expected_revision = summary.revision_after;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImeCancelOutcome {
    Restored,
    Conflict,
    Inactive,
}

impl Editor {
    pub fn replace_and_mark_text(
        &mut self,
        replacement_utf16: Option<Range<usize>>,
        text: &str,
        selected_utf16: Option<Range<usize>>,
    ) -> Result<EditSummary, BufferError> {
        let received = Instant::now();
        self.preferred_grapheme_column = None;
        let current = if let Some(ime) = &self.ime {
            ime.current_range
        } else {
            let original_range = if let Some(range) = replacement_utf16 {
                self.utf16_range_to_source(range)?
            } else {
                self.selection.range()
            };
            let transaction_id = TransactionId(self.next_transaction);
            self.next_transaction += 1;
            self.ime = Some(ImeState::begin(self, transaction_id, original_range)?);
            original_range
        };
        let summary = self.document.edit(current, text)?;
        let selected = selected_utf16.unwrap_or_else(|| {
            let end = text.encode_utf16().count();
            end..end
        });
        let relative = utf16_range_to_byte(text, selected.clone());
        if let Some(ime) = &mut self.ime {
            ime.update(&summary, text, selected);
        }
        self.selection = Selection {
            anchor: SourceOffset(summary.range_after.start.0 + relative.start),
            active: SourceOffset(summary.range_after.start.0 + relative.end),
        };
        self.record_model_update(received, InputMeasurementKind::ImeComposition);
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
                .map(|ime| ime.current_range)
                .unwrap_or(self.selection.range())
        };
        let summary = self.document.edit(range, text)?;
        self.selection = Selection::caret(summary.range_after.end);
        self.ime = None;
        self.record_model_update(received, InputMeasurementKind::ImeCommit);
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
        if start != ime.current_range.start || end != ime.current_range.end {
            return Ok(ImeCancelOutcome::Conflict);
        }
        self.document.edit(ime.current_range, &ime.original_text)?;
        self.selection = ime.original_selection;
        self.preferred_grapheme_column = None;
        Ok(ImeCancelOutcome::Restored)
    }

    pub fn source_range_to_utf16(&self, range: SourceRange) -> Result<Range<usize>, BufferError> {
        self.document.validate_range(range)?;
        Ok(self.document.byte_to_utf16(range.start)?..self.document.byte_to_utf16(range.end)?)
    }

    pub fn utf16_range_to_source(&self, range: Range<usize>) -> Result<SourceRange, BufferError> {
        let source = SourceRange {
            start: self.document.utf16_to_byte(range.start),
            end: self.document.utf16_to_byte(range.end),
        };
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
            if utf16 >= target || target < utf16 + ch.len_utf16() {
                return byte;
            }
            utf16 += ch.len_utf16();
        }
        text.len()
    }
    one(text, range.start)..one(text, range.end)
}
