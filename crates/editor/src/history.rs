use crate::Selection;
use hane_document::{BufferError, EditSummary, RopeBuffer, SourceRange, TextBuffer};
use std::time::{Duration, Instant};

const GROUP_TIMEOUT: Duration = Duration::from_millis(750);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EditKind {
    Insert,
    Backspace,
    Delete,
    Replace,
    Ime,
}

#[derive(Clone, Debug)]
struct HistoryEntry {
    start: usize,
    deleted: String,
    inserted: String,
    selection_before: Selection,
    selection_after: Selection,
    kind: EditKind,
    last_edit_at: Instant,
}

impl HistoryEntry {
    fn from_summary(
        summary: &EditSummary,
        inserted: &str,
        selection_before: Selection,
        selection_after: Selection,
        kind: EditKind,
        now: Instant,
    ) -> Self {
        Self {
            start: summary.range_before.start.0,
            deleted: summary.inverse.replacement.clone(),
            inserted: inserted.to_owned(),
            selection_before,
            selection_after,
            kind,
            last_edit_at: now,
        }
    }

    fn try_merge(&mut self, next: &Self) -> bool {
        if self.kind != next.kind
            || next.last_edit_at.duration_since(self.last_edit_at) > GROUP_TIMEOUT
            || self.selection_after != next.selection_before
        {
            return false;
        }
        match self.kind {
            EditKind::Insert
                if self.deleted.is_empty()
                    && next.deleted.is_empty()
                    && next.start == self.start + self.inserted.len()
                    && !self.inserted.ends_with('\n')
                    && !next.inserted.contains('\n') =>
            {
                self.inserted.push_str(&next.inserted);
            }
            EditKind::Backspace
                if self.inserted.is_empty()
                    && next.inserted.is_empty()
                    && next.start + next.deleted.len() == self.start =>
            {
                self.start = next.start;
                self.deleted.insert_str(0, &next.deleted);
            }
            EditKind::Delete
                if self.inserted.is_empty()
                    && next.inserted.is_empty()
                    && next.start == self.start =>
            {
                self.deleted.push_str(&next.deleted);
            }
            _ => return false,
        }
        self.selection_after = next.selection_after;
        self.last_edit_at = next.last_edit_at;
        true
    }

    fn undo(&self, document: &mut RopeBuffer) -> Result<EditSummary, BufferError> {
        document.edit(
            SourceRange::new(self.start, self.start + self.inserted.len()),
            &self.deleted,
        )
    }

    fn redo(&self, document: &mut RopeBuffer) -> Result<EditSummary, BufferError> {
        document.edit(
            SourceRange::new(self.start, self.start + self.deleted.len()),
            &self.inserted,
        )
    }
}

#[derive(Default)]
pub(crate) struct History {
    undo: Vec<HistoryEntry>,
    redo: Vec<HistoryEntry>,
}

impl History {
    pub(crate) fn record(
        &mut self,
        summary: &EditSummary,
        inserted: &str,
        selection_before: Selection,
        selection_after: Selection,
        kind: EditKind,
        now: Instant,
    ) {
        let entry = HistoryEntry::from_summary(
            summary,
            inserted,
            selection_before,
            selection_after,
            kind,
            now,
        );
        if !self
            .undo
            .last_mut()
            .is_some_and(|previous| previous.try_merge(&entry))
        {
            self.undo.push(entry);
        }
        self.redo.clear();
    }

    pub(crate) fn record_replacement(
        &mut self,
        start: usize,
        deleted: String,
        inserted: String,
        selection_before: Selection,
        selection_after: Selection,
        kind: EditKind,
    ) {
        self.undo.push(HistoryEntry {
            start,
            deleted,
            inserted,
            selection_before,
            selection_after,
            kind,
            last_edit_at: Instant::now(),
        });
        self.redo.clear();
    }

    pub(crate) fn undo(
        &mut self,
        document: &mut RopeBuffer,
    ) -> Result<Option<(EditSummary, Selection)>, BufferError> {
        let Some(entry) = self.undo.pop() else {
            return Ok(None);
        };
        let summary = entry.undo(document)?;
        let selection = entry.selection_before;
        self.redo.push(entry);
        Ok(Some((summary, selection)))
    }

    pub(crate) fn redo(
        &mut self,
        document: &mut RopeBuffer,
    ) -> Result<Option<(EditSummary, Selection)>, BufferError> {
        let Some(entry) = self.redo.pop() else {
            return Ok(None);
        };
        let summary = entry.redo(document)?;
        let selection = entry.selection_after;
        self.undo.push(entry);
        Ok(Some((summary, selection)))
    }

    pub(crate) fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub(crate) fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}
