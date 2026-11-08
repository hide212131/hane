use crate::Selection;
use hane_document::{BufferError, EditSummary, RopeBuffer, SourceOffset, SourceRange, TextBuffer};
use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const GROUP_TIMEOUT: Duration = Duration::from_millis(750);

/// An identity for the content of one history entry. Process-wide so that an
/// entry a rewrite builds can never be mistaken for one of any other history.
fn fresh_entry_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Matches RopeBuffer's source line endings (see its `line_mirror` docs):
/// LF, CRLF and a bare CR all end a line. A CRLF's `\r` is always followed by
/// `\n`, so checking either byte alone is enough to catch the pair too.
fn is_line_ending(c: char) -> bool {
    c == '\n' || c == '\r'
}

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
    /// Changes whenever the entry's content does (it is merged into, or
    /// rebuilt by a rewrite), so equal ids mean an identical step.
    id: u64,
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
            id: fresh_entry_id(),
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
            // A selection replacement starts as an Insert entry whose
            // `deleted` text is the selected range. Subsequent characters
            // typed at the resulting caret are still part of that same user
            // transaction, so keep appending them even though the first
            // entry was not an insertion into an empty range.
            EditKind::Insert
                if next.deleted.is_empty()
                    && next.start == self.start + self.inserted.len()
                    && !self.inserted.ends_with(is_line_ending)
                    && !next.inserted.contains(is_line_ending) =>
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
        self.id = fresh_entry_id();
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

#[derive(Clone, Default)]
pub(crate) struct History {
    undo: Vec<HistoryEntry>,
    redo: Vec<HistoryEntry>,
    can_merge_last: bool,
}

impl History {
    /// How many entries Undo and Redo hold; a cheap fingerprint of the shape.
    pub(crate) fn depth(&self) -> (usize, usize) {
        (self.undo.len(), self.redo.len())
    }

    pub(crate) fn break_group(&mut self) {
        self.can_merge_last = false;
    }

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
        if !(self.can_merge_last
            && self
                .undo
                .last_mut()
                .is_some_and(|previous| previous.try_merge(&entry)))
        {
            self.undo.push(entry);
        }
        self.redo.clear();
        self.can_merge_last = true;
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
            id: fresh_entry_id(),
            start,
            deleted,
            inserted,
            selection_before,
            selection_after,
            kind,
            last_edit_at: Instant::now(),
        });
        self.redo.clear();
        self.can_merge_last = false;
    }

    pub(crate) fn undo(
        &mut self,
        document: &mut RopeBuffer,
    ) -> Result<Option<(EditSummary, Selection)>, BufferError> {
        self.break_group();
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
        self.break_group();
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

    /// The same history with every state the document went through (and will
    /// go through on redo) mapped by `plan`, so Undo and Redo land on the
    /// rewritten form of each state. `current` is the document text now.
    /// `None` means the history could not be followed; nothing is changed.
    pub(crate) fn rewritten(&self, current: &str, plan: &RewritePlan) -> Option<Self> {
        let now = RewriteState::of(current.to_owned(), plan)?;
        self.rewritten_from(now, plan)
    }

    /// The document text of every state Undo and Redo can still reach, other
    /// than `current`. `None` means the history could not be followed.
    pub(crate) fn reachable_texts(&self, current: &str) -> Option<Vec<String>> {
        let mut texts = Vec::with_capacity(self.undo.len() + self.redo.len());
        let mut text = current.to_owned();
        for entry in self.undo.iter().rev() {
            text = entry.reverted(&text)?;
            texts.push(text.clone());
        }
        let mut text = current.to_owned();
        for entry in self.redo.iter().rev() {
            text = entry.applied(&text)?;
            texts.push(text.clone());
        }
        Some(texts)
    }

    /// The identities of every step in time order: Undo oldest first, then
    /// Redo in the order it would be redone.
    pub(crate) fn timeline_ids(&self) -> Vec<u64> {
        self.undo
            .iter()
            .chain(self.redo.iter().rev())
            .map(|entry| entry.id)
            .collect()
    }

    /// [`Self::rewritten`] for a current state that is already worked out.
    pub(crate) fn rewritten_from(&self, now: RewriteState, plan: &RewritePlan) -> Option<Self> {
        let mut undo = Vec::with_capacity(self.undo.len());
        let mut after = now.clone();
        for entry in self.undo.iter().rev() {
            let before = RewriteState::of(entry.reverted(&after.text)?, plan)?;
            undo.push(entry.rebuilt(&before, &after));
            after = before;
        }
        undo.reverse();
        let mut redo = Vec::with_capacity(self.redo.len());
        let mut before = now;
        for entry in self.redo.iter().rev() {
            let after = RewriteState::of(entry.applied(&before.text)?, plan)?;
            redo.push(entry.rebuilt(&before, &after));
            before = after;
        }
        redo.reverse();
        Some(Self {
            undo,
            redo,
            can_merge_last: false,
        })
    }

    /// This history rewritten by taking `rewritten` (the rewrite of an older
    /// history whose steps had `source_ids`) as far as the two still agree,
    /// and working out only the steps that came after. The steps in common
    /// are not evaluated again, so the plan runs once per state the editor
    /// gained or lost since, not once per state of the whole history.
    ///
    /// `now` is the rewrite state of the current document text. `None` means
    /// the history could not be followed.
    pub(crate) fn rebased(
        &self,
        mut now: RewriteState,
        source_ids: &[u64],
        rewritten: Self,
        plan: &RewritePlan,
    ) -> Option<Self> {
        let entries: Vec<&HistoryEntry> = self.undo.iter().chain(self.redo.iter().rev()).collect();
        let n = entries.len();
        let cursor = self.undo.len();
        let shared = entries
            .iter()
            .zip(source_ids)
            .take_while(|(entry, id)| entry.id == **id)
            .count();
        let mut timeline = rewritten.undo;
        timeline.extend(rewritten.redo.into_iter().rev());
        if timeline.len() != source_ids.len() {
            return None;
        }
        timeline.truncate(shared);
        if shared < n {
            // Texts of the states from the earlier of the cursor and the
            // first new step, found by walking the steps from the current text.
            let low = cursor.min(shared);
            let mut texts = vec![String::new(); n + 1 - low];
            texts[cursor - low] = std::mem::take(&mut now.text);
            for i in cursor..n {
                let next = entries[i].applied(&texts[i - low])?;
                texts[i + 1 - low] = next;
            }
            for i in (low..cursor).rev() {
                let previous = entries[i].reverted(&texts[i + 1 - low])?;
                texts[i - low] = previous;
            }
            let mut now = Some(now);
            let mut states = Vec::with_capacity(n + 1 - shared);
            for i in shared..=n {
                let state = if i == cursor {
                    now.take()?
                } else {
                    RewriteState::of(std::mem::take(&mut texts[i - low]), plan)?
                };
                states.push(state);
            }
            for i in shared..n {
                timeline.push(entries[i].rebuilt(&states[i - shared], &states[i + 1 - shared]));
            }
        }
        let mut redo = timeline.split_off(cursor);
        redo.reverse();
        Some(Self {
            undo: timeline,
            redo,
            can_merge_last: false,
        })
    }
}

/// One non-overlapping source replacement of a rewrite.
pub type TextEdit = (Range<usize>, String);

/// Decides, for a whole document text, the replacements that rewrite it. It
/// is evaluated on every state of the history, so it must depend on the text
/// alone.
pub type RewritePlan = dyn Fn(&str) -> Vec<TextEdit> + Send + Sync;

/// Sorts `edits` and checks that they are ordered, disjoint and on character
/// boundaries of `text`.
pub(crate) fn checked_edits(text: &str, mut edits: Vec<TextEdit>) -> Option<Vec<TextEdit>> {
    edits.sort_by_key(|(range, _)| (range.start, range.end));
    let mut end = 0;
    for (range, _) in &edits {
        if range.start < end
            || range.start > range.end
            || !text.is_char_boundary(range.start)
            || !text.is_char_boundary(range.end)
        {
            return None;
        }
        end = range.end;
    }
    Some(edits)
}

fn apply_edits(text: &str, edits: &[TextEdit]) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (range, replacement) in edits {
        out.push_str(&text[last..range.start]);
        out.push_str(replacement);
        last = range.end;
    }
    out.push_str(&text[last..]);
    out
}

/// Where `offset` of the old text lands in the rewritten text. An offset
/// inside a replaced range moves to the end of its replacement.
pub(crate) fn map_offset(edits: &[TextEdit], offset: usize) -> usize {
    let (mut grown, mut shrunk) = (0usize, 0usize);
    for (range, replacement) in edits {
        if range.end <= offset {
            grown += replacement.len();
            shrunk += range.len();
        } else {
            if range.start < offset {
                return range.start + grown - shrunk + replacement.len();
            }
            break;
        }
    }
    offset + grown - shrunk
}

pub(crate) fn map_selection(edits: &[TextEdit], selection: Selection) -> Selection {
    Selection {
        anchor: SourceOffset(map_offset(edits, selection.anchor.0)),
        active: SourceOffset(map_offset(edits, selection.active.0)),
    }
}

/// A document text of the history, the rewrite planned for it and the result.
#[derive(Clone)]
pub(crate) struct RewriteState {
    text: String,
    pub(crate) edits: Vec<TextEdit>,
    rewritten: String,
}

impl RewriteState {
    pub(crate) fn of(text: String, plan: &RewritePlan) -> Option<Self> {
        let edits = checked_edits(&text, plan(&text))?;
        let rewritten = apply_edits(&text, &edits);
        Some(Self {
            text,
            edits,
            rewritten,
        })
    }
}

fn splice(text: &str, start: usize, len: usize, with: &str) -> Option<String> {
    let end = start.checked_add(len)?;
    if !text.is_char_boundary(start) || !text.is_char_boundary(end) {
        return None;
    }
    let mut out = String::with_capacity(text.len() - len + with.len());
    out.push_str(&text[..start]);
    out.push_str(with);
    out.push_str(&text[end..]);
    Some(out)
}

fn common_prefix_len(a: &str, b: &str) -> usize {
    a.char_indices()
        .zip(b.chars())
        .find(|((_, x), y)| x != y)
        .map_or(a.len().min(b.len()), |((index, _), _)| index)
}

fn common_suffix_len(a: &str, b: &str) -> usize {
    a.chars()
        .rev()
        .zip(b.chars().rev())
        .take_while(|(x, y)| x == y)
        .map(|(x, _)| x.len_utf8())
        .sum()
}

impl HistoryEntry {
    fn reverted(&self, after: &str) -> Option<String> {
        splice(after, self.start, self.inserted.len(), &self.deleted)
    }

    fn applied(&self, before: &str) -> Option<String> {
        splice(before, self.start, self.deleted.len(), &self.inserted)
    }

    /// This entry between the rewritten `before` and `after` states. The
    /// entry keeps its own shape (so a typing group stays what it was) when
    /// it still describes the rewritten change; otherwise the change is
    /// re-derived from the two rewritten texts.
    fn rebuilt(&self, before: &RewriteState, after: &RewriteState) -> Self {
        let start = map_offset(&before.edits, self.start);
        let deleted_end = start + self.deleted.len();
        let inserted_end = start + self.inserted.len();
        let verbatim = before.rewritten.get(start..deleted_end) == Some(self.deleted.as_str())
            && after.rewritten.get(start..inserted_end) == Some(self.inserted.as_str())
            && before.rewritten.get(..start) == after.rewritten.get(..start)
            && before.rewritten.get(deleted_end..) == after.rewritten.get(inserted_end..)
            && before.rewritten.get(..start).is_some();
        let (start, deleted, inserted) = if verbatim {
            (start, self.deleted.clone(), self.inserted.clone())
        } else {
            let prefix = common_prefix_len(&before.rewritten, &after.rewritten);
            let suffix = common_suffix_len(
                &before.rewritten[prefix..],
                &after.rewritten[prefix..],
            );
            (
                prefix,
                before.rewritten[prefix..before.rewritten.len() - suffix].to_owned(),
                after.rewritten[prefix..after.rewritten.len() - suffix].to_owned(),
            )
        };
        Self {
            id: fresh_entry_id(),
            start,
            deleted,
            inserted,
            selection_before: map_selection(&before.edits, self.selection_before),
            selection_after: map_selection(&after.edits, self.selection_after),
            kind: self.kind,
            last_edit_at: self.last_edit_at,
        }
    }
}
