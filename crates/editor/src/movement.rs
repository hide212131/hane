use crate::{Editor, Selection};
use hane_document::{
    BufferError, EditSummary, LineCol, LineId, SourceOffset, SourceRange, TextBuffer,
};
use unicode_segmentation::UnicodeSegmentation;

/// Where a forward word/paragraph move stops. Backward moves ignore it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForwardStop {
    /// Stop at the end of the current/next word or paragraph (macOS).
    End,
    /// Stop at the start of the next word or paragraph (Windows).
    NextStart,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WordClass {
    Word,
    Separator,
}

impl WordClass {
    fn of(segment: &str) -> Self {
        if segment.chars().any(char::is_alphanumeric) {
            Self::Word
        } else {
            Self::Separator
        }
    }
}

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

    /// The edge a non-extending move collapses a non-empty selection to.
    fn collapsed_edge(&self, forward: bool) -> Option<SourceOffset> {
        let range = self.selection.range();
        if range.is_empty() {
            None
        } else if forward {
            Some(range.end)
        } else {
            Some(range.start)
        }
    }

    /// Walks word/separator runs from `from` in one direction.
    ///
    /// `phases` lists the run classes consumed in order. A run of the wrong
    /// class for the current phase advances to the next phase without
    /// consuming; once the last phase meets a run of another class the walk
    /// stops. Text is read one line at a time, so the cost beyond the line
    /// holding the caret is bounded by the lines actually crossed; a single
    /// very long line is still copied once per step.
    fn scan_words(
        &self,
        from: SourceOffset,
        forward: bool,
        phases: [WordClass; 2],
    ) -> Result<SourceOffset, BufferError> {
        self.document.validate_offset(from)?;
        let len = self.document.len_bytes().0;
        let mut pos = from.0;
        let mut phase = 0;
        loop {
            // The whole line is segmented, not just the part on one side of
            // `pos`: a cursor inside a word ("can|'t") must keep the word's
            // context, or the half it cuts off is classified on its own.
            let range = if forward {
                if pos >= len {
                    return Ok(SourceOffset(pos));
                }
                let line = self.document.line_for_offset(SourceOffset(pos))?;
                self.document.line_range(line)?
            } else {
                if pos == 0 {
                    return Ok(SourceOffset(pos));
                }
                let mut line = self.document.line_for_offset(SourceOffset(pos))?;
                if pos == self.document.line_range(line)?.start.0 {
                    line = LineId(line.0 - 1);
                }
                self.document.line_range(line)?
            };
            let start = range.start.0;
            let relative = pos - start;
            let text = self.document.text(range)?;
            // Each segment with the number of its bytes on the walk's side of
            // `pos`; only the segment holding `pos` is partial.
            let mut segments: Vec<(&str, usize)> = Vec::new();
            for (index, segment) in text.split_word_bound_indices() {
                let segment_end = index + segment.len();
                if forward {
                    if segment_end > relative {
                        segments.push((segment, segment_end - index.max(relative)));
                    }
                } else if index < relative {
                    segments.push((segment, segment_end.min(relative) - index));
                }
            }
            if !forward {
                segments.reverse();
            }
            for (segment, walked) in segments {
                let class = WordClass::of(segment);
                if class != phases[phase] {
                    if phase == 0 {
                        phase = 1;
                    }
                    if class != phases[phase] {
                        return Ok(SourceOffset(pos));
                    }
                }
                if forward {
                    pos += walked;
                } else {
                    pos -= walked;
                }
            }
        }
    }

    fn word_target(
        &self,
        from: SourceOffset,
        forward: bool,
        stop: ForwardStop,
    ) -> Result<SourceOffset, BufferError> {
        let phases = match (forward, stop) {
            (true, ForwardStop::NextStart) => [WordClass::Word, WordClass::Separator],
            _ => [WordClass::Separator, WordClass::Word],
        };
        self.scan_words(from, forward, phases)
    }

    pub(crate) fn move_word(
        &mut self,
        forward: bool,
        stop: ForwardStop,
        extend: bool,
    ) -> Result<(), BufferError> {
        let target = match self.collapsed_edge(forward).filter(|_| !extend) {
            Some(edge) => edge,
            None => self.word_target(self.selection.active, forward, stop)?,
        };
        self.move_to(target, extend);
        Ok(())
    }

    /// Paragraphs are source lines separated by LF/CRLF/CR, not Markdown
    /// blank-line blocks.
    fn paragraph_target(
        &self,
        from: SourceOffset,
        forward: bool,
        stop: ForwardStop,
    ) -> Result<SourceOffset, BufferError> {
        let line = self.document.line_for_offset(from)?;
        let content = self.document.line_content_range(line)?;
        let has_next = line.0 + 1 < self.document.line_count();
        if !forward {
            if from.0 > content.start.0 {
                return Ok(content.start);
            }
            return match line.0.checked_sub(1) {
                Some(previous) => Ok(self.document.line_content_range(LineId(previous))?.start),
                None => Ok(content.start),
            };
        }
        match stop {
            ForwardStop::NextStart => {
                if has_next {
                    Ok(self.document.line_range(LineId(line.0 + 1))?.start)
                } else {
                    Ok(self.document.line_content_range(line)?.end)
                }
            }
            ForwardStop::End => {
                if from.0 < content.end.0 {
                    Ok(content.end)
                } else if has_next {
                    Ok(self.document.line_content_range(LineId(line.0 + 1))?.end)
                } else {
                    Ok(from)
                }
            }
        }
    }

    pub(crate) fn move_paragraph(
        &mut self,
        forward: bool,
        stop: ForwardStop,
        extend: bool,
    ) -> Result<(), BufferError> {
        let target = match self.collapsed_edge(forward).filter(|_| !extend) {
            Some(edge) => edge,
            None => self.paragraph_target(self.selection.active, forward, stop)?,
        };
        self.move_to(target, extend);
        Ok(())
    }

    /// Deletes the selection if there is one, otherwise up to the word
    /// boundary the matching move would reach. One call is one undo step.
    pub(crate) fn delete_word(
        &mut self,
        forward: bool,
        stop: ForwardStop,
        now: std::time::Instant,
    ) -> Result<Option<EditSummary>, BufferError> {
        let selection_before = self.selection;
        if self.selection.range().is_empty() {
            let target = self.word_target(self.selection.active, forward, stop)?;
            if target == self.selection.active {
                return Ok(None);
            }
            self.selection = Selection {
                anchor: self.selection.active,
                active: target,
            };
        }
        Ok(Some(self.replace_selection_recorded_from(
            "",
            selection_before,
            crate::history::EditKind::Replace,
            now,
        )?))
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
