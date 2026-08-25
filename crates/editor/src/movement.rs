use crate::{Editor, Selection};
use hane_document::{
    BufferError, EditSummary, LineCol, LineId, SourceOffset, SourceRange, TextBuffer,
};
use unicode_segmentation::UnicodeSegmentation;

impl Editor {
    fn previous_grapheme(&self, offset: SourceOffset) -> Result<SourceOffset, BufferError> {
        self.document.validate_offset(offset)?;
        if offset.0 == 0 {
            return Ok(offset);
        }
        let line = self.document.line_for_offset(offset)?;
        let line_start = self.document.offset_for_line_col(line, LineCol(0))?.0;
        if offset.0 == line_start && line.0 > 0 {
            return Ok(self.document.line_content_range(LineId(line.0 - 1))?.end);
        }
        let text = self.document.text(SourceRange::new(line_start, offset.0))?;
        Ok(SourceOffset(
            line_start
                + text
                    .grapheme_indices(true)
                    .next_back()
                    .map(|(index, _)| index)
                    .unwrap_or(0),
        ))
    }

    fn next_grapheme(&self, offset: SourceOffset) -> Result<SourceOffset, BufferError> {
        self.document.validate_offset(offset)?;
        if offset.0 == self.document.len_bytes().0 {
            return Ok(offset);
        }
        let line = self.document.line_for_offset(offset)?;
        let line_end = self.document.line_range(line)?.end.0;
        let text = self.document.text(SourceRange::new(offset.0, line_end))?;
        let next = text
            .grapheme_indices(true)
            .nth(1)
            .map(|(index, _)| index)
            .unwrap_or(text.len());
        Ok(SourceOffset(offset.0 + next))
    }

    pub(crate) fn move_grapheme(&mut self, right: bool, extend: bool) -> Result<(), BufferError> {
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

    pub(crate) fn move_vertical(&mut self, down: bool, extend: bool) -> Result<(), BufferError> {
        let current_line = self.document.line_for_offset(self.selection.active)?;
        let current_range = self.document.line_content_range(current_line)?;
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
            LineId(next)
        } else {
            let Some(previous) = current_line.0.checked_sub(1) else {
                return Ok(());
            };
            LineId(previous)
        };
        let target_range = self.document.line_content_range(target_line)?;
        let target_text = self.document.text(target_range)?;
        let relative_offset = target_text
            .grapheme_indices(true)
            .nth(preferred_column)
            .map(|(offset, _)| offset)
            .unwrap_or(target_text.len());
        self.move_to(SourceOffset(target_range.start.0 + relative_offset), extend);
        Ok(())
    }

    pub(crate) fn move_to(&mut self, target: SourceOffset, extend: bool) {
        if extend {
            self.selection.active = target;
        } else {
            self.selection = Selection::caret(target);
        }
    }

    pub(crate) fn move_to_line_boundary(
        &mut self,
        end: bool,
        extend: bool,
    ) -> Result<(), BufferError> {
        let line = self.document.line_for_offset(self.selection.active)?;
        let range = self.document.line_content_range(line)?;
        self.move_to(if end { range.end } else { range.start }, extend);
        Ok(())
    }

    pub(crate) fn backspace(
        &mut self,
        now: std::time::Instant,
    ) -> Result<Option<EditSummary>, BufferError> {
        let selection_before = self.selection;
        if self.selection.range().is_empty() {
            let previous = self.previous_grapheme(self.selection.active)?;
            if previous == self.selection.active {
                return Ok(None);
            }
            self.selection.anchor = previous;
        }
        Ok(Some(self.replace_selection_recorded_from(
            "",
            selection_before,
            crate::history::EditKind::Backspace,
            now,
        )?))
    }

    pub(crate) fn delete(
        &mut self,
        now: std::time::Instant,
    ) -> Result<Option<EditSummary>, BufferError> {
        let selection_before = self.selection;
        if self.selection.range().is_empty() {
            let next = self.next_grapheme(self.selection.active)?;
            if next == self.selection.active {
                return Ok(None);
            }
            self.selection.active = next;
        }
        Ok(Some(self.replace_selection_recorded_from(
            "",
            selection_before,
            crate::history::EditKind::Delete,
            now,
        )?))
    }
}
