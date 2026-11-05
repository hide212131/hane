use crate::history::{TextEdit, checked_edits, map_offset, map_selection};
use crate::{Editor, InputMeasurementKind, RewritePlan, Selection};
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
    /// The range containing only the marked text.
    pub marked_range: SourceRange,
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
            marked_range: range,
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
        self.marked_range = summary.range_after;
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
        let range = if let Some(ime) = &self.ime {
            ime.current_range
        } else if let Some(range) = replacement_utf16 {
            self.utf16_range_to_source(range)?
        } else {
            self.selection.range()
        };
        let ime = self.ime.take();
        let selection_before = self.selection;
        let replacement = text.to_owned();
        let summary = self.document.edit(range, text)?;
        self.selection = Selection::caret(summary.range_after.end);
        if let Some(ime) = ime {
            self.history.record_replacement(
                ime.original_range.start.0,
                ime.original_text,
                replacement,
                ime.original_selection,
                self.selection,
                crate::history::EditKind::Ime,
            );
        } else {
            let kind = if selection_before.range().is_empty() {
                crate::history::EditKind::Insert
            } else {
                crate::history::EditKind::Replace
            };
            self.history.record(
                &summary,
                &replacement,
                selection_before,
                self.selection,
                kind,
                received,
            );
        }
        self.record_model_update(received, InputMeasurementKind::ImeCommit);
        self.run_deferred_rewrites();
        Ok(summary)
    }

    /// Rewrites the document by `plan` while an IME composition stays open.
    /// The plan is evaluated on the text the history holds (the document with
    /// the original text in place of the marked text), and the replacements
    /// are carried to the live document around the marked range. The
    /// composition, its anchors and its pending Undo entry move with the text,
    /// so later marked-text updates, commit and cancel land at the right
    /// place without being forced to commit.
    ///
    /// A replacement that touches the composed region cannot be carried
    /// safely; then nothing is changed and an error is returned so the caller
    /// treats the rewrite as not followed.
    pub(crate) fn rewrite_with_composition(
        &mut self,
        plan: &RewritePlan,
    ) -> Result<(), BufferError> {
        let Some(ime) = self.ime.clone() else {
            return Ok(());
        };
        let current = ime.current_range;
        let text = self.document.full_text();
        let base = format!(
            "{}{}{}",
            &text[..current.start.0],
            ime.original_text,
            &text[current.end.0..]
        );
        // The marked text is absent from `base`, so a replacement that lives
        // inside it only shows up on the live text. Such a rewrite cannot be
        // carried around the composition; refuse it so it waits for the end.
        if let Some(live) = checked_edits(&text, plan(&text)) {
            let touches_marked = live.iter().any(|(range, _)| {
                if range.start == range.end {
                    range.start >= current.start.0 && range.start <= current.end.0
                } else {
                    range.start < current.end.0 && range.end > current.start.0
                }
            });
            if touches_marked && !current.is_empty() {
                return Err(BufferError::InvalidRange { range: current });
            }
        }
        let Some(edits) = checked_edits(&base, plan(&base)) else {
            return Ok(());
        };
        let start = current.start.0;
        let original_len = ime.original_text.len();
        let original_end = start + original_len;
        let marked_len = current.end.0 - start;
        let mut doc_edits: Vec<TextEdit> = Vec::with_capacity(edits.len());
        for (range, replacement) in &edits {
            let empty = range.start == range.end;
            // An insertion exactly at either edge of the composed region has
            // no safe side, so it is refused like an overlap.
            let before =
                range.end <= start && !(empty && original_len == 0 && range.start == start);
            let after = range.start >= original_end && !(empty && range.start == original_end);
            let shifted = if before {
                range.clone()
            } else if after {
                range.start + marked_len - original_len..range.end + marked_len - original_len
            } else {
                return Err(BufferError::InvalidRange { range: current });
            };
            doc_edits.push((shifted, replacement.clone()));
        }
        let history = self.history.rewritten(&base, plan);
        self.adopt_rewrite(&doc_edits, history)?;

        let new_start = map_offset(&doc_edits, current.start.0);
        let new_current = SourceRange::new(new_start, new_start + marked_len);
        let original_range = SourceRange::new(new_start, new_start + original_len);
        let start_anchor = self.document.anchor(new_current.start, Bias::Before)?;
        let end_anchor = self.document.anchor(new_current.end, Bias::After)?;
        let revision = self.document.revision();
        if let Some(ime) = &mut self.ime {
            ime.current_range = new_current;
            ime.marked_range = new_current;
            ime.original_range = original_range;
            ime.original_selection = map_selection(&edits, ime.original_selection);
            ime.start_anchor = start_anchor;
            ime.end_anchor = end_anchor;
            ime.base_revision = revision;
            ime.expected_revision = revision;
        }
        Ok(())
    }

    pub fn commit_composition(&mut self) {
        let Some(ime) = self.ime.take() else {
            return;
        };
        self.history.record_replacement(
            ime.original_range.start.0,
            ime.original_text,
            ime.marked_text,
            ime.original_selection,
            self.selection,
            crate::history::EditKind::Ime,
        );
        self.run_deferred_rewrites();
    }

    pub fn cancel_composition(&mut self) -> Result<ImeCancelOutcome, BufferError> {
        let outcome = self.cancel_composition_inner();
        self.run_deferred_rewrites();
        outcome
    }

    fn cancel_composition_inner(&mut self) -> Result<ImeCancelOutcome, BufferError> {
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
