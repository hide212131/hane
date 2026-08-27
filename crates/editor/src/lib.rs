//! GPUI-independent editor commands, cursor/selection, and IME transactions.

mod history;
mod ime;
mod movement;
mod selection;

pub use ime::{ImeCancelOutcome, ImeState, utf16_range_to_byte};
pub use selection::Selection;

use hane_document::{BufferError, EditSummary, RopeBuffer, SourceOffset, TextBuffer};
use hane_metrics::RollingWindow;
use history::{EditKind, History};
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
}
