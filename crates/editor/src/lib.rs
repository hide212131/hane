//! GPUI-independent editor commands, cursor/selection, and IME transactions.

// The editor exposes lightweight commands and query methods whose results are
// intentionally optional to retain ergonomic event-handler call sites.
#![allow(
    clippy::must_use_candidate,
    reason = "editor commands and queries are intentionally discardable at event boundaries"
)]
// Error semantics are documented by each public type/trait contract instead of
// repeating the same prose on delegated editor operations.
#![allow(
    clippy::missing_errors_doc,
    reason = "editor error contracts are defined on their enclosing abstractions"
)]
#![allow(
    clippy::missing_panics_doc,
    reason = "the panic precondition is described with the relevant editor invariant"
)]
#![allow(
    clippy::map_unwrap_or,
    reason = "the explicit map followed by fallback is clearer for cursor transformation"
)]
#![allow(
    clippy::assigning_clones,
    reason = "replacement of IME marked text intentionally makes ownership explicit"
)]

mod find;
mod history;
mod ime;
mod movement;
mod replace;
mod selection;

pub use find::{
    FindNavigation, FindOptions, FindQuery, FindQueryError, FindResults, FindScan, MAX_MATCHES,
    MAX_QUERY_BYTES, NavigationStep, matches_in_range, scan as find_scan,
    scan_range as find_scan_range,
};
pub use replace::{ReplacePlan, ReplacePlanError, plan_replace_all};
pub use ime::{ImeCancelOutcome, ImeState, utf16_range_to_byte};
pub use movement::ForwardStop;
pub use selection::Selection;

use hane_document::{BufferError, EditSummary, RopeBuffer, SourceOffset, SourceRange, TextBuffer};
use hane_metrics::RollingWindow;
pub use history::{RewritePlan, TextEdit};
use history::{EditKind, History, RewriteState, checked_edits, map_selection};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct InputMeasurement {
    pub sequence: u64,
    pub kind: InputMeasurementKind,
    pub received_at: Instant,
    pub model_updated_at: Instant,
    pub frame_painted_at: Option<Instant>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputMeasurementKind {
    Command,
    ImeComposition,
    ImeCommit,
}

impl InputMeasurementKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::ImeComposition => "ime_composition",
            Self::ImeCommit => "ime_commit",
        }
    }
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
    MoveToLineStart { extend: bool },
    MoveToLineEnd { extend: bool },
    /// Moves by word. `stop` only matters going forward.
    MoveWord {
        forward: bool,
        stop: ForwardStop,
        extend: bool,
    },
    /// Moves by source-line paragraph (LF/CRLF/CR separated), not by Markdown block.
    MoveParagraph {
        forward: bool,
        stop: ForwardStop,
        extend: bool,
    },
    /// Deletes the selection, or up to the word boundary the matching
    /// [`EditorCommand::MoveWord`] reaches, as one undo step.
    DeleteWord { forward: bool, stop: ForwardStop },
    SelectAll,
    Undo,
    Redo,
}

pub struct Editor {
    pub(crate) document: RopeBuffer,
    pub(crate) selection: Selection,
    pub(crate) preferred_grapheme_column: Option<usize>,
    /// The x a layout-driven vertical move aims at, in the text column's own
    /// coordinates. A wrapped row has no source line, so a grapheme column
    /// cannot describe where the caret should land; the view resolves the x
    /// against the layout and hands the result back through
    /// [`Editor::move_vertical_to`].
    pub(crate) preferred_visual_x: Option<f32>,
    pub(crate) ime: Option<ImeState>,
    pub(crate) next_transaction: u64,
    next_input_sequence: u64,
    pending_measurements: RollingWindow<InputMeasurement>,
    history: History,
    /// Rewrites that touched the composed region and so wait for the
    /// composition to end; see [`Editor::defer_rewrite`].
    deferred_rewrites: Vec<Arc<RewritePlan>>,
}

/// The document and history a rewrite is prepared from; see
/// [`Editor::rewrite_snapshot`].
pub struct RewriteSnapshot {
    document: RopeBuffer,
    history: History,
}

impl RewriteSnapshot {
    /// Evaluates `plan` on the document and on every state of the history.
    /// This is the heavy part of a rewrite and touches no editor, so it can
    /// run on any thread. `None` when the plan is not a valid set of
    /// replacements for the document.
    pub fn prepare(&self, plan: &RewritePlan) -> Option<PreparedRewrite> {
        let text = self.document.full_text();
        let edits = checked_edits(&text, plan(&text))?;
        let mut document = self.document.clone();
        for (range, replacement) in edits.iter().rev() {
            document
                .edit(SourceRange::new(range.start, range.end), replacement)
                .ok()?;
        }
        let history = self.history.rewritten(&text, plan);
        Some(PreparedRewrite {
            revision: self.document.revision(),
            depth: self.history.depth(),
            source_ids: self.history.timeline_ids(),
            edits,
            document,
            history,
        })
    }
}

/// A rewrite worked out ahead of time for one exact editor state. It is only
/// applied while the editor still is that state; see
/// [`Editor::rewrite_is_current`].
pub struct PreparedRewrite {
    revision: hane_document::Revision,
    depth: (usize, usize),
    /// The identities of the history steps it was prepared from, so a later
    /// editor can tell how much of that history it still shares.
    source_ids: Vec<u64>,
    edits: Vec<TextEdit>,
    document: RopeBuffer,
    history: Option<History>,
}

impl Editor {
    pub fn new(text: &str) -> Self {
        Self {
            document: RopeBuffer::from_text(text),
            selection: Selection::caret(SourceOffset(0)),
            preferred_grapheme_column: None,
            preferred_visual_x: None,
            ime: None,
            next_transaction: 1,
            next_input_sequence: 1,
            pending_measurements: RollingWindow::new(2_048),
            history: History::default(),
            deferred_rewrites: Vec::new(),
        }
    }

    pub fn from_document(document: RopeBuffer) -> Self {
        let mut editor = Self::new("");
        editor.document = document;
        editor
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
        self.history.break_group();
        self.commit_composition();
        self.document.validate_offset(selection.anchor)?;
        self.document.validate_offset(selection.active)?;
        self.selection = selection;
        self.preferred_grapheme_column = None;
        self.preferred_visual_x = None;
        Ok(())
    }

    /// The x later vertical moves should aim at, if one has been established.
    pub fn preferred_visual_x(&self) -> Option<f32> {
        self.preferred_visual_x
    }

    /// Moves the caret to a target the layout resolved, remembering the x it was
    /// aiming at so a run of vertical moves keeps its column across rows of
    /// different lengths.
    ///
    /// Vertical movement lives here rather than in [`Editor::dispatch`] because
    /// only the layout knows where a row is: with soft wrap, "the line below" is
    /// not the next source line, and the same source line can hold several rows.
    pub fn move_vertical_to(
        &mut self,
        target: SourceOffset,
        extend: bool,
        preferred_x: f32,
    ) -> Result<(), BufferError> {
        self.history.break_group();
        self.commit_composition();
        self.document.validate_offset(target)?;
        self.move_to(target, extend);
        self.preferred_grapheme_column = None;
        self.preferred_visual_x = Some(preferred_x);
        Ok(())
    }

    pub fn dispatch(
        &mut self,
        command: EditorCommand<'_>,
    ) -> Result<Option<EditSummary>, BufferError> {
        let received = Instant::now();
        if !matches!(
            command,
            EditorCommand::Insert(_) | EditorCommand::Backspace | EditorCommand::Delete
        ) {
            self.history.break_group();
        }
        self.commit_composition();
        if !matches!(
            command,
            EditorCommand::MoveUp { .. } | EditorCommand::MoveDown { .. }
        ) {
            self.preferred_grapheme_column = None;
            self.preferred_visual_x = None;
        }
        let edit = match command {
            EditorCommand::Insert(text) => {
                Some(self.replace_selection_recorded(text, EditKind::Insert, received)?)
            }
            EditorCommand::Backspace => self.backspace(received)?,
            EditorCommand::Delete => self.delete(received)?,
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
            EditorCommand::MoveToLineStart { extend } => {
                self.move_to_line_boundary(false, extend)?;
                None
            }
            EditorCommand::MoveToLineEnd { extend } => {
                self.move_to_line_boundary(true, extend)?;
                None
            }
            EditorCommand::MoveWord {
                forward,
                stop,
                extend,
            } => {
                self.move_word(forward, stop, extend)?;
                None
            }
            EditorCommand::MoveParagraph {
                forward,
                stop,
                extend,
            } => {
                self.move_paragraph(forward, stop, extend)?;
                None
            }
            EditorCommand::DeleteWord { forward, stop } => {
                self.delete_word(forward, stop, received)?
            }
            EditorCommand::SelectAll => {
                self.selection = Selection {
                    anchor: SourceOffset(0),
                    active: SourceOffset(self.document.len_bytes().0),
                };
                None
            }
            EditorCommand::Undo => self.undo()?,
            EditorCommand::Redo => self.redo()?,
        };
        self.record_model_update(received, InputMeasurementKind::Command);
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

    /// Applies one caller-planned source replacement as one undoable
    /// transaction. The editor remains unaware of Markdown: callers provide a
    /// range, replacement text, and the post-edit selection only.
    pub fn replace_range_recorded(
        &mut self,
        range: SourceRange,
        replacement: &str,
        selection_after: Selection,
    ) -> Result<EditSummary, BufferError> {
        self.history.break_group();
        self.commit_composition();
        self.document.validate_range(range)?;
        let next_len = self
            .document
            .len_bytes()
            .0
            .saturating_sub(range.len_bytes())
            .saturating_add(replacement.len());
        if selection_after.anchor.0 > next_len {
            return Err(BufferError::OffsetOutOfBounds {
                offset: selection_after.anchor,
                len: hane_document::ByteLen(next_len),
            });
        }
        if selection_after.active.0 > next_len {
            return Err(BufferError::OffsetOutOfBounds {
                offset: selection_after.active,
                len: hane_document::ByteLen(next_len),
            });
        }
        let received = Instant::now();
        let selection_before = self.selection;
        let summary = self.document.edit(range, replacement)?;
        self.selection = selection_after;
        self.preferred_grapheme_column = None;
        self.preferred_visual_x = None;
        self.history.record_replacement(
            summary.range_before.start.0,
            summary.inverse.replacement.clone(),
            replacement.to_owned(),
            selection_before,
            selection_after,
            EditKind::Replace,
        );
        self.record_model_update(received, InputMeasurementKind::Command);
        Ok(summary)
    }

    /// A copy of the current document with `plan` applied, without touching
    /// the editor. `None` when the plan is not a valid set of replacements.
    pub fn rewritten_document(&self, plan: &RewritePlan) -> Option<RopeBuffer> {
        let text = self.document.full_text();
        let edits = checked_edits(&text, plan(&text))?;
        let mut document = self.document.clone();
        for (range, replacement) in edits.iter().rev() {
            document
                .edit(SourceRange::new(range.start, range.end), replacement)
                .ok()?;
        }
        Some(document)
    }

    /// Rewrites the document by `plan` and carries the whole edit history
    /// along: every state Undo and Redo reach is the rewritten form of the
    /// state it was, with its selections moved accordingly, so no step goes
    /// back to the text as it was before the rewrite. The rewrite is not an
    /// edit of its own and adds no history entry. The editor stays unaware
    /// of what the plan means. An invalid plan changes nothing.
    ///
    /// This evaluates `plan` on the text and on every state of the history
    /// (document size times history depth), so a caller on an interactive
    /// path prepares with [`Editor::rewrite_snapshot`] off-thread and applies
    /// with [`Editor::apply_prepared_rewrite`], keeping this as the fallback
    /// for an editor that moved on since.
    pub fn apply_rewrite(&mut self, plan: &RewritePlan) -> Result<(), BufferError> {
        self.history.break_group();
        if self.ime.is_some() {
            return self.rewrite_with_composition(plan);
        }
        let text = self.document.full_text();
        let Some(edits) = checked_edits(&text, plan(&text)) else {
            return Ok(());
        };
        let history = self.history.rewritten(&text, plan);
        self.adopt_rewrite(&edits, history)
    }

    /// The text of every document state Undo and Redo can still reach, other
    /// than the current one. `None` when the history cannot be followed.
    pub fn history_texts(&self) -> Option<Vec<String>> {
        let text = self.document.full_text();
        // History steps are recorded against the text without the marked
        // composition, so follow them from the original text.
        let Some(ime) = &self.ime else {
            return self.history.reachable_texts(&text);
        };
        let range = ime.current_range;
        let base = format!(
            "{}{}{}",
            text.get(..range.start.0)?,
            ime.original_text,
            text.get(range.end.0..)?
        );
        self.history.reachable_texts(&base)
    }

    /// Whether an IME composition is open.
    pub fn is_composing(&self) -> bool {
        self.ime.is_some()
    }

    /// Holds `plan` back until the open composition ends (commit, cancel or
    /// forced commit), then applies it to the settled text and history. For a
    /// rewrite that touches the composed region, which cannot be carried
    /// around the marked text. Without an open composition it applies at once.
    pub fn defer_rewrite(&mut self, plan: Arc<RewritePlan>) {
        if self.ime.is_some() {
            self.deferred_rewrites.push(plan);
        } else {
            let _ = self.apply_rewrite(&*plan);
        }
    }

    /// Applies the rewrites held back during a composition, now that it ended.
    pub(crate) fn run_deferred_rewrites(&mut self) {
        if self.ime.is_some() {
            return;
        }
        for plan in std::mem::take(&mut self.deferred_rewrites) {
            let _ = self.apply_rewrite(&*plan);
        }
    }

    /// Writes the already planned `edits` into the document and takes `history`
    /// as the rewritten history. A history that cannot be followed (it never
    /// is for one this editor recorded) is dropped rather than left pointing
    /// at the old text.
    fn adopt_rewrite(
        &mut self,
        edits: &[TextEdit],
        history: Option<History>,
    ) -> Result<(), BufferError> {
        let selection = map_selection(edits, self.selection);
        for (range, replacement) in edits.iter().rev() {
            self.document
                .edit(SourceRange::new(range.start, range.end), replacement)?;
        }
        self.history = history.unwrap_or_default();
        self.selection = selection;
        self.preferred_grapheme_column = None;
        self.preferred_visual_x = None;
        Ok(())
    }

    /// The state a rewrite is prepared from: a cheap copy (the document is a
    /// persistent rope) that can be moved to another thread. `None` while an
    /// IME composition is open, whose pending history entry is not in it.
    pub fn rewrite_snapshot(&self) -> Option<RewriteSnapshot> {
        if self.ime.is_some() {
            return None;
        }
        Some(RewriteSnapshot {
            document: self.document.clone(),
            history: self.history.clone(),
        })
    }

    /// Whether `prepared` still describes this editor exactly: same document
    /// revision, same history shape and no composition open. Selection is not
    /// part of it; it is mapped when the rewrite is applied.
    pub fn rewrite_is_current(&self, prepared: &PreparedRewrite) -> bool {
        self.ime.is_none()
            && self.document.revision() == prepared.revision
            && self.history.depth() == prepared.depth
    }

    /// The document as the prepared rewrite makes it.
    pub fn prepared_rewrite_document(&self, prepared: &PreparedRewrite) -> Option<RopeBuffer> {
        self.rewrite_is_current(prepared)
            .then(|| prepared.document.clone())
    }

    /// Applies a rewrite prepared by [`RewriteSnapshot::prepare`] without
    /// evaluating the plan: the cost is the number of replacements plus the
    /// size of the history, not the document size times the history depth.
    /// `Ok(false)` means the editor is not what the rewrite was prepared from
    /// and nothing was changed; the caller falls back to [`Editor::apply_rewrite`].
    pub fn apply_prepared_rewrite(
        &mut self,
        prepared: PreparedRewrite,
    ) -> Result<bool, BufferError> {
        if !self.rewrite_is_current(&prepared) {
            return Ok(false);
        }
        self.history.break_group();
        self.adopt_rewrite(&prepared.edits, prepared.history)?;
        Ok(true)
    }

    /// Makes the editor follow `plan`, using `prepared` as far as it still
    /// describes this editor. While it is exact this is
    /// [`Editor::apply_prepared_rewrite`]. When the editor moved on (typing,
    /// Undo/Redo or a composition since the preparation), the history steps
    /// it still shares with the preparation are taken as prepared and the plan
    /// is evaluated only on the current text and on the states of the steps
    /// gained since: the work follows what changed, not the document size
    /// times the whole history depth that [`Editor::apply_rewrite`] costs.
    /// An invalid plan changes nothing.
    pub fn follow_prepared_rewrite(
        &mut self,
        prepared: PreparedRewrite,
        plan: &RewritePlan,
    ) -> Result<(), BufferError> {
        if self.rewrite_is_current(&prepared) {
            self.history.break_group();
            return self.adopt_rewrite(&prepared.edits, prepared.history);
        }
        self.history.break_group();
        if self.ime.is_some() {
            return self.rewrite_with_composition(plan);
        }
        let Some(now) = RewriteState::of(self.document.full_text(), plan) else {
            return Ok(());
        };
        let edits = now.edits.clone();
        let history = match prepared.history {
            Some(rewritten) => self
                .history
                .rebased(now, &prepared.source_ids, rewritten, plan),
            None => self.history.rewritten_from(now, plan),
        };
        self.adopt_rewrite(&edits, history)
    }

    pub(crate) fn record_model_update(&mut self, received_at: Instant, kind: InputMeasurementKind) {
        self.pending_measurements.push(InputMeasurement {
            sequence: self.next_input_sequence,
            kind,
            received_at,
            model_updated_at: Instant::now(),
            frame_painted_at: None,
        });
        self.next_input_sequence += 1;
    }

    pub fn mark_frame_painted(&mut self) -> Vec<InputMeasurement> {
        let now = Instant::now();
        let mut completed = self.pending_measurements.take_all();
        for measurement in &mut completed {
            measurement.frame_painted_at = Some(now);
        }
        completed
    }

    pub(crate) fn replace_selection(&mut self, text: &str) -> Result<EditSummary, BufferError> {
        let summary = self.document.edit(self.selection.range(), text)?;
        self.selection = Selection::caret(summary.range_after.end);
        Ok(summary)
    }

    pub(crate) fn replace_selection_recorded(
        &mut self,
        text: &str,
        kind: EditKind,
        now: Instant,
    ) -> Result<EditSummary, BufferError> {
        let before = self.selection;
        let summary = self.replace_selection(text)?;
        self.history
            .record(&summary, text, before, self.selection, kind, now);
        Ok(summary)
    }

    pub(crate) fn replace_selection_recorded_from(
        &mut self,
        text: &str,
        selection_before: Selection,
        kind: EditKind,
        now: Instant,
    ) -> Result<EditSummary, BufferError> {
        let summary = self.replace_selection(text)?;
        self.history
            .record(&summary, text, selection_before, self.selection, kind, now);
        Ok(summary)
    }

    pub fn selected_text(&self) -> Result<String, BufferError> {
        self.document.text(self.selection.range())
    }

    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    fn undo(&mut self) -> Result<Option<EditSummary>, BufferError> {
        let Some((summary, selection)) = self.history.undo(&mut self.document)? else {
            return Ok(None);
        };
        self.selection = selection;
        Ok(Some(summary))
    }

    fn redo(&mut self) -> Result<Option<EditSummary>, BufferError> {
        let Some((summary, selection)) = self.history.redo(&mut self.document)? else {
            return Ok(None);
        };
        self.selection = selection;
        Ok(Some(summary))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hane_document::SourceRange;
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

    #[test]
    fn consecutive_typing_is_one_undo_transaction() {
        let mut e = Editor::new("");
        for text in ["h", "e", "l", "l", "o"] {
            e.dispatch(EditorCommand::Insert(text)).unwrap();
        }
        assert_eq!(e.document().full_text(), "hello");
        assert!(e.can_undo());
        e.dispatch(EditorCommand::Undo).unwrap();
        assert_eq!(e.document().full_text(), "");
        assert_eq!(e.selection(), Selection::caret(SourceOffset(0)));
        e.dispatch(EditorCommand::Redo).unwrap();
        assert_eq!(e.document().full_text(), "hello");
        assert_eq!(e.selection(), Selection::caret(SourceOffset(5)));
    }

    #[test]
    fn recorded_range_replacement_is_one_undoable_structural_transaction() {
        let mut editor = Editor::new("- A");
        editor
            .set_selection(Selection::caret(SourceOffset(3)))
            .unwrap();
        editor
            .replace_range_recorded(
                SourceRange::empty(3),
                "\n- ",
                Selection::caret(SourceOffset(6)),
            )
            .unwrap();
        assert_eq!(editor.document().full_text(), "- A\n- ");
        editor.dispatch(EditorCommand::Undo).unwrap();
        assert_eq!(editor.document().full_text(), "- A");
        assert_eq!(editor.selection(), Selection::caret(SourceOffset(3)));
        editor.dispatch(EditorCommand::Redo).unwrap();
        assert_eq!(editor.document().full_text(), "- A\n- ");
        assert_eq!(editor.selection(), Selection::caret(SourceOffset(6)));
    }

    /// Issue #460: mirrors the paragraph-boundary replacement that
    /// `hane_markdown::list_editing::plan_list_edit` now plans when exiting
    /// a trailing empty top-level list item (hane-editor does not depend on
    /// hane-markdown, so the plan's range/replacement/selection are given
    /// literally, as in `recorded_range_replacement_is_one_undoable_structural_transaction`).
    #[test]
    fn trailing_list_exit_and_next_typed_paragraph_undo_redo_independently() {
        let mut editor = Editor::new("- abc\n- def\n- ");
        editor
            .set_selection(Selection::caret(SourceOffset(14)))
            .unwrap();
        editor
            .replace_range_recorded(
                SourceRange::new(12, 14),
                "\n",
                Selection::caret(SourceOffset(13)),
            )
            .unwrap();
        assert_eq!(editor.document().full_text(), "- abc\n- def\n\n");

        editor.dispatch(EditorCommand::Insert("ghi")).unwrap();
        assert_eq!(editor.document().full_text(), "- abc\n- def\n\nghi");

        editor.dispatch(EditorCommand::Undo).unwrap();
        assert_eq!(editor.document().full_text(), "- abc\n- def\n\n");
        assert_eq!(editor.selection(), Selection::caret(SourceOffset(13)));

        editor.dispatch(EditorCommand::Undo).unwrap();
        assert_eq!(editor.document().full_text(), "- abc\n- def\n- ");
        assert_eq!(editor.selection(), Selection::caret(SourceOffset(14)));

        editor.dispatch(EditorCommand::Redo).unwrap();
        assert_eq!(editor.document().full_text(), "- abc\n- def\n\n");
        editor.dispatch(EditorCommand::Redo).unwrap();
        assert_eq!(editor.document().full_text(), "- abc\n- def\n\nghi");
    }

    #[test]
    fn consecutive_backspace_and_delete_are_grouped() {
        let mut backspace = Editor::new("日本語");
        backspace
            .dispatch(EditorCommand::MoveToEnd { extend: false })
            .unwrap();
        backspace.dispatch(EditorCommand::Backspace).unwrap();
        backspace.dispatch(EditorCommand::Backspace).unwrap();
        assert_eq!(backspace.document().full_text(), "日");
        backspace.dispatch(EditorCommand::Undo).unwrap();
        assert_eq!(backspace.document().full_text(), "日本語");

        let mut delete = Editor::new("a🙂羽");
        delete.dispatch(EditorCommand::Delete).unwrap();
        delete.dispatch(EditorCommand::Delete).unwrap();
        delete.dispatch(EditorCommand::Undo).unwrap();
        assert_eq!(delete.document().full_text(), "a🙂羽");
    }

    #[test]
    fn ime_updates_form_one_undo_transaction() {
        let mut e = Editor::new("a旧b");
        e.set_selection(Selection {
            anchor: SourceOffset(1),
            active: SourceOffset(4),
        })
        .unwrap();
        e.replace_and_mark_text(None, "に", None).unwrap();
        e.replace_and_mark_text(None, "日本", None).unwrap();
        e.commit_text(None, "日本語").unwrap();
        assert_eq!(e.document().full_text(), "a日本語b");
        e.dispatch(EditorCommand::Undo).unwrap();
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
    fn selection_replacement_restores_text_and_selection() {
        let mut e = Editor::new("hello 日本語");
        let original = Selection {
            anchor: SourceOffset(6),
            active: SourceOffset(15),
        };
        e.set_selection(original).unwrap();
        e.dispatch(EditorCommand::Insert("world")).unwrap();
        assert_eq!(e.document().full_text(), "hello world");
        e.dispatch(EditorCommand::Undo).unwrap();
        assert_eq!(e.document().full_text(), "hello 日本語");
        assert_eq!(e.selection(), original);
        e.dispatch(EditorCommand::Redo).unwrap();
        assert_eq!(e.document().full_text(), "hello world");
    }

    #[test]
    fn characterwise_selection_replacement_is_one_undo_transaction() {
        let original_text =
            "# Normal List Editing Fixture\n\n3. Alpha row\n41. Beta row\n100. Gamma row\n";
        let mut e = Editor::new(original_text);
        let start = original_text.find("Gamma").unwrap();
        let original = Selection {
            anchor: SourceOffset(start),
            active: SourceOffset(start + "Gamma".len()),
        };
        e.set_selection(original).unwrap();

        for character in ["D", "e", "l", "t", "a"] {
            e.dispatch(EditorCommand::Insert(character)).unwrap();
        }
        assert_eq!(
            e.document().full_text(),
            "# Normal List Editing Fixture\n\n3. Alpha row\n41. Beta row\n100. Delta row\n"
        );

        e.dispatch(EditorCommand::Undo).unwrap();
        assert_eq!(e.document().full_text(), original_text);
        assert_eq!(e.selection(), original);

        e.dispatch(EditorCommand::Redo).unwrap();
        assert_eq!(
            e.document().full_text(),
            "# Normal List Editing Fixture\n\n3. Alpha row\n41. Beta row\n100. Delta row\n"
        );
        assert_eq!(
            e.selection(),
            Selection::caret(SourceOffset(start + "Delta".len()))
        );
    }

    #[test]
    fn selection_replacement_does_not_remerge_after_undo_redo() {
        let mut e = Editor::new("hello world");
        let original = Selection {
            anchor: SourceOffset(6),
            active: SourceOffset(11),
        };
        e.set_selection(original).unwrap();
        for character in ["n", "e", "w"] {
            e.dispatch(EditorCommand::Insert(character)).unwrap();
        }
        e.dispatch(EditorCommand::Undo).unwrap();
        e.dispatch(EditorCommand::Redo).unwrap();
        e.dispatch(EditorCommand::Insert("!")).unwrap();

        e.dispatch(EditorCommand::Undo).unwrap();
        assert_eq!(e.document().full_text(), "hello new");
        e.dispatch(EditorCommand::Undo).unwrap();
        assert_eq!(e.document().full_text(), "hello world");
    }

    #[test]
    fn selection_replacement_does_not_remerge_after_caret_round_trip() {
        let mut e = Editor::new("hello world");
        let original = Selection {
            anchor: SourceOffset(6),
            active: SourceOffset(11),
        };
        e.set_selection(original).unwrap();
        for character in ["n", "e", "w"] {
            e.dispatch(EditorCommand::Insert(character)).unwrap();
        }
        e.dispatch(EditorCommand::MoveLeft { extend: false })
            .unwrap();
        e.dispatch(EditorCommand::MoveRight { extend: false })
            .unwrap();
        e.dispatch(EditorCommand::Insert("!")).unwrap();

        e.dispatch(EditorCommand::Undo).unwrap();
        assert_eq!(e.document().full_text(), "hello new");
        e.dispatch(EditorCommand::Undo).unwrap();
        assert_eq!(e.document().full_text(), "hello world");
    }

    #[test]
    fn new_edit_after_undo_discards_redo() {
        let mut e = Editor::new("");
        e.dispatch(EditorCommand::Insert("a")).unwrap();
        e.dispatch(EditorCommand::Undo).unwrap();
        e.dispatch(EditorCommand::Insert("b")).unwrap();
        assert!(!e.can_redo());
        e.dispatch(EditorCommand::Redo).unwrap();
        assert_eq!(e.document().full_text(), "b");
    }

    #[test]
    fn home_and_end_are_line_boundaries() {
        let mut e = Editor::new("first\n日本語\nlast");
        e.set_selection(Selection::caret(SourceOffset(12))).unwrap();
        e.dispatch(EditorCommand::MoveToLineStart { extend: false })
            .unwrap();
        assert_eq!(e.selection(), Selection::caret(SourceOffset(6)));
        e.dispatch(EditorCommand::MoveToLineEnd { extend: true })
            .unwrap();
        assert_eq!(
            e.selection(),
            Selection {
                anchor: SourceOffset(6),
                active: SourceOffset(15)
            }
        );
    }

    #[test]
    fn home_and_end_treat_a_bare_cr_as_a_line_boundary_like_lf_and_crlf() {
        let mut e = Editor::new("first\r日本語\rlast");
        e.set_selection(Selection::caret(SourceOffset(12))).unwrap();
        e.dispatch(EditorCommand::MoveToLineStart { extend: false })
            .unwrap();
        assert_eq!(e.selection(), Selection::caret(SourceOffset(6)));
        e.dispatch(EditorCommand::MoveToLineEnd { extend: true })
            .unwrap();
        assert_eq!(
            e.selection(),
            Selection {
                anchor: SourceOffset(6),
                active: SourceOffset(15)
            }
        );
    }

    #[test]
    fn newline_replaces_selection_and_is_its_own_undo_transaction() {
        let mut e = Editor::new("ab");
        e.set_selection(Selection::caret(SourceOffset(1))).unwrap();
        e.dispatch(EditorCommand::Insert("x")).unwrap();
        e.dispatch(EditorCommand::Insert("\n")).unwrap();
        e.dispatch(EditorCommand::Insert("y")).unwrap();
        assert_eq!(e.document().full_text(), "ax\nyb");

        e.dispatch(EditorCommand::Undo).unwrap();
        assert_eq!(e.document().full_text(), "ax\nb");
        e.dispatch(EditorCommand::Undo).unwrap();
        assert_eq!(e.document().full_text(), "axb");
        e.dispatch(EditorCommand::Undo).unwrap();
        assert_eq!(e.document().full_text(), "ab");
    }

    #[test]
    fn bare_cr_is_its_own_undo_transaction_like_lf() {
        let mut e = Editor::new("ab");
        e.set_selection(Selection::caret(SourceOffset(1))).unwrap();
        e.dispatch(EditorCommand::Insert("x")).unwrap();
        e.dispatch(EditorCommand::Insert("\r")).unwrap();
        e.dispatch(EditorCommand::Insert("y")).unwrap();
        assert_eq!(e.document().full_text(), "ax\ryb");

        e.dispatch(EditorCommand::Undo).unwrap();
        assert_eq!(e.document().full_text(), "ax\rb");
        e.dispatch(EditorCommand::Undo).unwrap();
        assert_eq!(e.document().full_text(), "axb");
        e.dispatch(EditorCommand::Undo).unwrap();
        assert_eq!(e.document().full_text(), "ab");
    }

    #[test]
    fn crlf_inserted_as_one_string_is_its_own_undo_transaction() {
        let mut e = Editor::new("ab");
        e.set_selection(Selection::caret(SourceOffset(1))).unwrap();
        e.dispatch(EditorCommand::Insert("x")).unwrap();
        e.dispatch(EditorCommand::Insert("\r\n")).unwrap();
        e.dispatch(EditorCommand::Insert("y")).unwrap();
        assert_eq!(e.document().full_text(), "ax\r\nyb");

        e.dispatch(EditorCommand::Undo).unwrap();
        assert_eq!(e.document().full_text(), "ax\r\nb");
        e.dispatch(EditorCommand::Undo).unwrap();
        assert_eq!(e.document().full_text(), "axb");
        e.dispatch(EditorCommand::Undo).unwrap();
        assert_eq!(e.document().full_text(), "ab");
    }
}
