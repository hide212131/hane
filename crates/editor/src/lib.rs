//! GPUI-independent editor commands, cursor/selection, and IME transactions.

mod ime;
mod movement;
mod selection;

pub use ime::{ImeCancelOutcome, ImeState, utf16_range_to_byte};
pub use selection::Selection;

use hane_document::{BufferError, EditSummary, RopeBuffer, SourceOffset, TextBuffer};
use hane_metrics::RollingWindow;
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
    SelectAll,
}

pub struct Editor {
    pub(crate) document: RopeBuffer,
    pub(crate) selection: Selection,
    pub(crate) preferred_grapheme_column: Option<usize>,
    pub(crate) ime: Option<ImeState>,
    pub(crate) next_transaction: u64,
    next_input_sequence: u64,
    pending_measurements: RollingWindow<InputMeasurement>,
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
            pending_measurements: RollingWindow::new(2_048),
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
}
