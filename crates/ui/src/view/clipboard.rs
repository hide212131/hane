//! Body Paste and "プレーンテキストとして貼り付け" (Issue #596).
//!
//! The clipboard is read once, on the UI thread, as one snapshot (plain text
//! and HTML of the same clipboard generation). `crate::clipboard::decide` picks
//! what to do with it; only HTML conversion leaves the UI thread. A background
//! result is applied only if, right before applying, the same session and
//! document instance are active with the same revision and the same directional
//! selection, the body still has the focus and may be edited, no IME
//! composition is running and no newer Paste, Copy or Cut started. A stale
//! result is dropped; it is never rebased onto the new state.
//!
//! The text is applied with `Editor::replace_range_recorded` and
//! `EditorView::after_input`, once each, so a Paste is one Undo step that is
//! independent of the typing around it. This module adds no editing API.

use super::*;
use crate::clipboard::{
    COMPOSING_MESSAGE, CONVERTING_MESSAGE, ConvertJob, PasteDecision, PasteMode, PasteOutcome,
    convert, decide, read_snapshot,
};
use crate::view::input_routing::InputTarget;

/// Shown when a finished conversion is not applied because the document, the
/// selection or the focus changed in the meantime.
pub(super) const PASTE_STALE_MESSAGE: &str = "文書が変わったため、貼り付けを中止しました";

/// The state a background Paste conversion was started against.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PasteJob {
    generation: u64,
    id: SessionId,
    instance: DocumentInstance,
    revision: Revision,
    selection: Selection,
}

impl EditorView {
    /// Whether the document body may take a paste now: editable, and no
    /// settings screen, comparison, AI popup, find input, sidebar filter or
    /// rename owns the keyboard.
    fn body_paste_ready(&self) -> bool {
        self.body_editable()
            && self.tab_close_confirm.is_none()
            && !self.settings_open()
            && !self.external_comparison_blocks_body()
            && self.other_input_capture_allows_body()
            && !self.document_find_input_is_focused()
            && matches!(self.input_target(), InputTarget::Body)
    }

    /// Drops the running Paste conversion, if any. Called by a newer Paste and
    /// by Copy and Cut, so an older job can never finish after them.
    pub(crate) fn invalidate_clipboard_job(&mut self) {
        self.clipboard_generation = self.clipboard_generation.wrapping_add(1);
        if self.clipboard_job.take().is_some()
            && self.status.as_deref() == Some(CONVERTING_MESSAGE)
        {
            self.status = None;
        }
    }

    /// Drops the running Paste conversion when the selection of the document it
    /// started on is no longer the one it started with. Comparing only the
    /// selection leaves events that do not move it (scroll, focus-neutral
    /// input, status updates) free to leave the job running.
    pub(super) fn drop_clipboard_job_if_selection_moved(&mut self) {
        let Some(job) = self.clipboard_job else {
            return;
        };
        let moved = self
            .sessions
            .get(job.id)
            .is_some_and(|session| session.editor().selection() != job.selection);
        if moved {
            self.clipboard_generation = self.clipboard_generation.wrapping_add(1);
            self.clipboard_job = None;
            self.status = Some(PASTE_STALE_MESSAGE.to_owned());
        }
    }

    /// Paste / PastePlainText into the document body.
    ///
    /// A read-only body is refused before the clipboard is read. An unfinished
    /// IME composition is refused with a short guidance and nothing (text,
    /// selection, clipboard) is touched.
    pub(crate) fn paste_into_body(
        &mut self,
        mode: PasteMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.body_paste_ready() {
            return;
        }
        self.invalidate_clipboard_job();
        if self.editor().ime().is_some() {
            self.status = Some(COMPOSING_MESSAGE.to_owned());
            cx.notify();
            return;
        }
        let snapshot = match read_snapshot(cx) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.status = Some(error.message().to_owned());
                cx.notify();
                return;
            }
        };
        match decide(mode, snapshot) {
            PasteDecision::Nothing => {}
            PasteDecision::InsertPlain(text) => {
                self.replace_selection_with(&text, None, cx);
            }
            PasteDecision::Replace { text, notice } => {
                self.replace_selection_with(&text, notice, cx);
            }
            PasteDecision::Refuse(message) => {
                self.status = Some(message.to_owned());
                cx.notify();
            }
            PasteDecision::Convert(job) => self.start_paste_conversion(job, window, cx),
        }
    }

    /// Replaces the current selection with `text` as one undoable transaction.
    /// An empty text never deletes the selection; a failure changes nothing.
    fn replace_selection_with(
        &mut self,
        text: &str,
        notice: Option<&'static str>,
        cx: &mut Context<Self>,
    ) {
        if text.is_empty() {
            return;
        }
        let range = self.editor().selection().range();
        let caret = SourceOffset(range.start.0 + text.len());
        match self
            .editor_mut()
            .replace_range_recorded(range, text, Selection::caret(caret))
        {
            Ok(_) => {
                self.status = notice.map(str::to_owned);
                self.after_input(cx);
            }
            Err(error) => self.report_error("paste", error),
        }
    }

    fn start_paste_conversion(
        &mut self,
        job: ConvertJob,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let id = self.sessions.active_id();
        let Some(instance) = self.document_instance(id) else {
            return;
        };
        let session = self.sessions.active();
        let pending = PasteJob {
            generation: self.clipboard_generation,
            id,
            instance,
            revision: session.revision(),
            selection: session.editor().selection(),
        };
        self.clipboard_job = Some(pending);
        self.status = Some(CONVERTING_MESSAGE.to_owned());
        cx.notify();
        let executor = cx.background_executor().clone();
        cx.spawn_in(window, async move |view, cx| {
            let outcome = executor.spawn(async move { convert(job) }).await;
            let _ = view.update_in(cx, |view, window, cx| {
                view.finish_paste_conversion(pending, outcome, window, cx);
            });
        })
        .detach();
    }

    /// Whether the state `job` was started against is still the live state.
    fn paste_job_is_fresh(&self, job: &PasteJob, window: &Window) -> bool {
        self.sessions.active_id() == job.id
            && self.document_instance(job.id) == Some(job.instance)
            && self.sessions.get(job.id).is_some_and(|session| {
                session.revision() == job.revision
                    && session.editor().selection() == job.selection
                    && session.editor().ime().is_none()
            })
            && window.is_window_active()
            && self.tab_close_confirm.is_none()
            && self.focus_handle.is_focused(window)
            && self.body_paste_ready()
    }

    fn finish_paste_conversion(
        &mut self,
        job: PasteJob,
        outcome: PasteOutcome,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        // A newer Paste, Copy or Cut owns the status line now.
        if self.clipboard_job != Some(job) {
            return;
        }
        self.clipboard_job = None;
        if self.status.as_deref() == Some(CONVERTING_MESSAGE) {
            self.status = None;
        }
        if !self.paste_job_is_fresh(&job, window) {
            self.status = Some(PASTE_STALE_MESSAGE.to_owned());
            cx.notify();
            return;
        }
        match outcome {
            PasteOutcome::Insert { text, notice } => {
                self.replace_selection_with(&text, notice, cx);
            }
            PasteOutcome::Refuse(message) => {
                self.status = Some(message.to_owned());
                cx.notify();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::{
        CLIPBOARD_BUSY_MESSAGE, HTML_FALLBACK_NOTICE, HTML_UNSUPPORTED_MESSAGE, SnapshotError,
        set_test_snapshot,
    };
    use gpui::ClipboardItem;

    const HTML: &str = r#"<p>前 <a href="https://example.com/a_(b)?x=1&amp;y=2">日本語リンク</a> と <strong>太字</strong>、<a href="https://example.org/">別リンク</a>。後</p>"#;
    const MARKDOWN: &str = "前 [日本語リンク](https://example.com/a_(b)?x=1&y=2) と **太字**、[別リンク](https://example.org/)。後";

    fn open_view<'a>(
        cx: &'a mut gpui::TestAppContext,
        text: &str,
    ) -> (gpui::Entity<EditorView>, &'a mut gpui::VisualTestContext) {
        let text = text.to_owned();
        let (view, cx) = cx.add_window_view(move |_, cx| EditorView::new(&text, "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        cx.update(|window, app| {
            let handle = view.read(app).focus_handle.clone();
            window.focus(&handle, app);
        });
        cx.run_until_parked();
        set_test_snapshot(None, false, None);
        (view, cx)
    }

    fn select(
        view: &gpui::Entity<EditorView>,
        cx: &mut gpui::VisualTestContext,
        selection: Selection,
    ) {
        view.update(cx, |view, cx| {
            view.editor_mut().set_selection(selection).unwrap();
            cx.notify();
        });
        cx.run_until_parked();
    }

    fn forward(start: usize, end: usize) -> Selection {
        Selection {
            anchor: SourceOffset(start),
            active: SourceOffset(end),
        }
    }

    fn text_of(view: &gpui::Entity<EditorView>, cx: &gpui::VisualTestContext) -> String {
        view.read_with(cx, |view, _| view.editor().document().full_text())
    }

    fn status_of(view: &gpui::Entity<EditorView>, cx: &gpui::VisualTestContext) -> Option<String> {
        view.read_with(cx, |view, _| view.status.clone())
    }

    fn paste(
        view: &gpui::Entity<EditorView>,
        cx: &mut gpui::VisualTestContext,
        mode: PasteMode,
    ) {
        cx.update(|window, app| {
            view.update(app, |view, cx| view.paste_into_body(mode, window, cx));
        });
    }

    fn dispatch(cx: &mut gpui::VisualTestContext, action: Box<dyn gpui::Action>) {
        cx.update(|window, app| window.dispatch_action(action, app));
        cx.run_until_parked();
    }

    fn set_clipboard(cx: &mut gpui::VisualTestContext, plain: &str) {
        cx.write_to_clipboard(ClipboardItem::new_string(plain.to_owned()));
    }

    /// The user's required condition end to end: mixed text and links paste as
    /// Markdown, replace only the selection, and are one Undo / Redo step that
    /// restores the original text and the reversed selection.
    #[gpui::test]
    fn rich_paste_replaces_the_selection_with_markdown_as_one_undo_step(
        cx: &mut gpui::TestAppContext,
    ) {
        let text = "前置き [選択] 後置き\n";
        let (view, cx) = open_view(cx, text);
        let start = text.find("[選択]").unwrap();
        let end = start + "[選択]".len();
        let reversed = Selection {
            anchor: SourceOffset(end),
            active: SourceOffset(start),
        };
        select(&view, cx, reversed);
        set_clipboard(cx, "前 日本語リンク と 太字、別リンク。後");
        set_test_snapshot(Some(HTML), false, None);

        paste(&view, cx, PasteMode::Rich);
        // Converting: nothing changed yet, and the status says so.
        assert_eq!(text_of(&view, cx), text);
        assert_eq!(status_of(&view, cx).as_deref(), Some(CONVERTING_MESSAGE));
        cx.run_until_parked();

        let pasted = format!("前置き {MARKDOWN} 後置き\n");
        assert_eq!(text_of(&view, cx), pasted);
        view.read_with(cx, |view, _| {
            assert_eq!(
                view.editor().selection(),
                Selection::caret(SourceOffset(start + MARKDOWN.len()))
            );
            assert_eq!(view.status, None);
            assert!(view.clipboard_job.is_none());
            assert!(view.editor().can_undo());
        });

        view.update(cx, |view, cx| view.dispatch(EditorCommand::Undo, cx));
        assert_eq!(text_of(&view, cx), text);
        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().selection(), reversed);
            assert!(!view.editor().can_undo());
        });
        view.update(cx, |view, cx| view.dispatch(EditorCommand::Redo, cx));
        assert_eq!(text_of(&view, cx), pasted);
    }

    /// The Paste is its own Undo step: typing before and after it is undone
    /// separately.
    #[gpui::test]
    fn paste_is_independent_of_the_typing_around_it(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "ab\n");
        select(&view, cx, Selection::caret(SourceOffset(2)));
        view.update(cx, |view, cx| view.insert_text("X", cx));
        set_clipboard(cx, "y");
        set_test_snapshot(Some("<b>y</b>"), false, None);
        paste(&view, cx, PasteMode::Rich);
        cx.run_until_parked();
        view.update(cx, |view, cx| view.insert_text("Z", cx));
        assert_eq!(text_of(&view, cx), "abX**y**Z\n");

        for expected in ["abX**y**\n", "abX\n", "ab\n"] {
            view.update(cx, |view, cx| view.dispatch(EditorCommand::Undo, cx));
            assert_eq!(text_of(&view, cx), expected);
        }
    }

    /// A plain-text paste is its own Undo step too.
    #[gpui::test]
    fn plain_paste_is_independent_of_the_typing_around_it(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "ab\n");
        select(&view, cx, Selection::caret(SourceOffset(2)));
        view.update(cx, |view, cx| view.insert_text("X", cx));
        set_clipboard(cx, "y");
        paste(&view, cx, PasteMode::PlainText);
        view.update(cx, |view, cx| view.insert_text("Z", cx));
        assert_eq!(text_of(&view, cx), "abXyZ\n");

        for expected in ["abXy\n", "abX\n", "ab\n"] {
            view.update(cx, |view, cx| view.dispatch(EditorCommand::Undo, cx));
            assert_eq!(text_of(&view, cx), expected);
        }
    }

    #[gpui::test]
    fn plain_text_paste_keeps_the_original_and_html_only_extracts_the_text(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = open_view(cx, "x\n");
        select(&view, cx, forward(0, 1));
        set_clipboard(cx, "原文 *そのまま*");
        set_test_snapshot(Some("<b>bold</b>"), false, None);
        dispatch(cx, Box::new(crate::actions::PastePlainText));
        assert_eq!(text_of(&view, cx), "原文 *そのまま*\n");

        // HTML-only: the visible text, with no Markdown syntax.
        let (view, cx) = open_view_again(cx, &view, "x\n");
        select(&view, cx, forward(0, 1));
        set_clipboard(cx, "");
        set_test_snapshot(Some("<p><b>太</b> *x*</p>"), false, None);
        dispatch(cx, Box::new(crate::actions::PastePlainText));
        assert_eq!(text_of(&view, cx), "太 *x*\n");
    }

    /// Resets a view to `text` so one test can run several snapshots.
    fn open_view_again<'a>(
        cx: &'a mut gpui::VisualTestContext,
        view: &gpui::Entity<EditorView>,
        text: &str,
    ) -> (gpui::Entity<EditorView>, &'a mut gpui::VisualTestContext) {
        let text = text.to_owned();
        view.update(cx, |view, cx| {
            view.dispatch(EditorCommand::SelectAll, cx);
            view.insert_text(&text, cx);
            view.status = None;
            cx.notify();
        });
        cx.run_until_parked();
        (view.clone(), cx)
    }

    /// Plain-only clipboards are never read as HTML, whatever they look like.
    #[gpui::test]
    fn a_plain_only_clipboard_is_pasted_as_it_is(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "x\n");
        select(&view, cx, forward(0, 1));
        let html_like = "<p>前 <a href=\"https://example.com/\">x</a> *y*</p>";
        set_clipboard(cx, html_like);
        dispatch(cx, Box::new(crate::actions::Paste));
        assert_eq!(text_of(&view, cx), format!("{html_like}\n"));
        view.read_with(cx, |view, _| assert!(view.clipboard_job.is_none()));
    }

    /// A table cannot be converted: the same snapshot's plain text is pasted
    /// with a short notice. HTML-only content that cannot be converted changes
    /// nothing, and the selection is not deleted.
    #[gpui::test]
    fn unconvertible_html_falls_back_to_plain_or_refuses_without_deleting(
        cx: &mut gpui::TestAppContext,
    ) {
        let table = "<table><tr><td>a</td></tr></table>";
        let (view, cx) = open_view(cx, "xy\n");
        select(&view, cx, forward(0, 1));
        set_clipboard(cx, "表のplain");
        set_test_snapshot(Some(table), false, None);
        paste(&view, cx, PasteMode::Rich);
        cx.run_until_parked();
        assert_eq!(text_of(&view, cx), "表のplainy\n");
        assert_eq!(status_of(&view, cx).as_deref(), Some(HTML_FALLBACK_NOTICE));

        // HTML-only and unconvertible.
        let (view, cx) = open_view_again(cx, &view, "xy\n");
        select(&view, cx, forward(0, 1));
        set_clipboard(cx, "");
        set_test_snapshot(Some(table), false, None);
        paste(&view, cx, PasteMode::Rich);
        cx.run_until_parked();
        assert_eq!(text_of(&view, cx), "xy\n");
        assert_eq!(status_of(&view, cx).as_deref(), Some(HTML_UNSUPPORTED_MESSAGE));
        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().selection(), forward(0, 1));
        });

        // HTML that the OS layer could not read: the plain text is pasted.
        let (view, cx) = open_view_again(cx, &view, "xy\n");
        select(&view, cx, forward(0, 1));
        set_clipboard(cx, "plain");
        set_test_snapshot(None, true, None);
        paste(&view, cx, PasteMode::Rich);
        assert_eq!(text_of(&view, cx), "plainy\n");
        assert_eq!(status_of(&view, cx).as_deref(), Some(HTML_FALLBACK_NOTICE));

        // ... and without plain text nothing changes.
        let (view, cx) = open_view_again(cx, &view, "xy\n");
        select(&view, cx, forward(0, 1));
        set_clipboard(cx, "");
        paste(&view, cx, PasteMode::Rich);
        assert_eq!(text_of(&view, cx), "xy\n");
        assert_eq!(status_of(&view, cx).as_deref(), Some(HTML_UNSUPPORTED_MESSAGE));
    }

    /// An unfinished IME composition is neither committed nor replaced.
    #[gpui::test]
    fn paste_during_an_ime_composition_changes_nothing(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "ab\n");
        select(&view, cx, Selection::caret(SourceOffset(2)));
        view.update(cx, |view, cx| {
            view.editor_mut()
                .replace_and_mark_text(None, "か", None)
                .unwrap();
            view.after_input(cx);
        });
        let before = text_of(&view, cx);
        set_clipboard(cx, "sentinel");
        set_test_snapshot(Some(HTML), false, None);

        paste(&view, cx, PasteMode::Rich);
        cx.run_until_parked();
        assert_eq!(text_of(&view, cx), before);
        view.read_with(cx, |view, _| {
            assert!(view.editor().ime().is_some());
            assert!(view.clipboard_job.is_none());
            assert_eq!(view.status.as_deref(), Some(COMPOSING_MESSAGE));
        });
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("sentinel".to_owned())
        );
    }

    /// A reading-mode body is refused before the clipboard is read; a busy
    /// clipboard changes nothing and says so.
    #[gpui::test]
    fn reading_mode_and_a_busy_clipboard_change_nothing(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "ab\n");
        select(&view, cx, forward(0, 1));
        set_test_snapshot(Some(HTML), false, Some(SnapshotError::Busy));
        view.update(cx, |view, cx| {
            view.editor_mode = EditorMode::Reading;
            cx.notify();
        });
        cx.run_until_parked();
        paste(&view, cx, PasteMode::Rich);
        cx.run_until_parked();
        assert_eq!(text_of(&view, cx), "ab\n");
        // The read was never attempted, so no clipboard error was reported.
        assert_eq!(status_of(&view, cx), None);

        view.update(cx, |view, cx| {
            view.editor_mode = EditorMode::Editing;
            cx.notify();
        });
        cx.run_until_parked();
        paste(&view, cx, PasteMode::Rich);
        cx.run_until_parked();
        assert_eq!(text_of(&view, cx), "ab\n");
        assert_eq!(status_of(&view, cx).as_deref(), Some(CLIPBOARD_BUSY_MESSAGE));
    }

    /// A finished conversion is dropped when the selection (including its
    /// direction), the revision, or the running job changed.
    #[gpui::test]
    fn stale_conversions_are_never_applied(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "ab cd\n");
        set_clipboard(cx, "plain");
        set_test_snapshot(Some("<b>B</b>"), false, None);

        // Caret moved.
        select(&view, cx, forward(0, 2));
        paste(&view, cx, PasteMode::Rich);
        select(&view, cx, Selection::caret(SourceOffset(3)));
        cx.run_until_parked();
        assert_eq!(text_of(&view, cx), "ab cd\n");
        assert_eq!(status_of(&view, cx).as_deref(), Some(PASTE_STALE_MESSAGE));

        // Same range, opposite direction.
        select(&view, cx, forward(0, 2));
        paste(&view, cx, PasteMode::Rich);
        select(
            &view,
            cx,
            Selection {
                anchor: SourceOffset(2),
                active: SourceOffset(0),
            },
        );
        cx.run_until_parked();
        assert_eq!(text_of(&view, cx), "ab cd\n");

        // The document was edited.
        select(&view, cx, forward(0, 2));
        paste(&view, cx, PasteMode::Rich);
        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset(5)))
                .unwrap();
            view.insert_text("!", cx);
            view.editor_mut().set_selection(forward(0, 2)).unwrap();
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(text_of(&view, cx), "ab cd!\n");
        view.read_with(cx, |view, _| assert!(view.clipboard_job.is_none()));
    }

    /// Moving the selection away and back before the conversion finishes still
    /// drops the result.
    #[gpui::test]
    fn moving_the_selection_away_and_back_drops_a_running_conversion(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = open_view(cx, "ab cd\n");
        select(&view, cx, forward(0, 2));
        set_clipboard(cx, "plain");
        set_test_snapshot(Some("<b>B</b>"), false, None);
        paste(&view, cx, PasteMode::Rich);
        view.read_with(cx, |view, _| assert!(view.clipboard_job.is_some()));
        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset(3)))
                .unwrap();
            view.editor_mut().set_selection(forward(0, 2)).unwrap();
            cx.notify();
        });
        view.read_with(cx, |view, _| assert!(view.clipboard_job.is_none()));
        cx.run_until_parked();
        assert_eq!(text_of(&view, cx), "ab cd\n");
        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().selection(), forward(0, 2));
            assert!(view.clipboard_job.is_none());
        });
    }

    /// A newer Paste supersedes the older one: the result is applied once.
    #[gpui::test]
    fn a_newer_paste_supersedes_the_older_conversion(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "ab\n");
        select(&view, cx, forward(0, 2));
        set_clipboard(cx, "");
        set_test_snapshot(Some("<b>first</b>"), false, None);
        paste(&view, cx, PasteMode::Rich);
        set_test_snapshot(Some("<i>second</i>"), false, None);
        paste(&view, cx, PasteMode::Rich);
        cx.run_until_parked();
        assert_eq!(text_of(&view, cx), "*second*\n");
        view.read_with(cx, |view, _| {
            assert_eq!(view.status, None);
            assert!(view.clipboard_job.is_none());
        });
    }

    /// A close confirmation opened while converting stops the result from
    /// being applied.
    #[gpui::test]
    fn a_close_confirmation_drops_a_finished_conversion(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "ab\n");
        select(&view, cx, forward(0, 2));
        set_clipboard(cx, "");
        set_test_snapshot(Some("<b>x</b>"), false, None);
        paste(&view, cx, PasteMode::Rich);
        view.update(cx, |view, cx| {
            let id = view.sessions.active_id();
            let instance = view.document_instance(id).unwrap();
            view.tab_close_confirm = Some(TabCloseConfirm { id, instance });
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(text_of(&view, cx), "ab\n");
        assert_eq!(status_of(&view, cx).as_deref(), Some(PASTE_STALE_MESSAGE));
    }

    /// While the close confirmation is open, the synchronous Paste paths (plain
    /// text, the Replace fallback) and the start of an HTML conversion change
    /// neither the document, the selection nor the Undo history.
    #[gpui::test]
    fn paste_while_a_close_confirmation_is_open_changes_nothing(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "ab\n");
        select(&view, cx, forward(0, 1));
        view.update(cx, |view, cx| {
            let id = view.sessions.active_id();
            let instance = view.document_instance(id).unwrap();
            view.tab_close_confirm = Some(TabCloseConfirm { id, instance });
            cx.notify();
        });
        cx.run_until_parked();
        let can_undo = view.read_with(cx, |view, _| view.editor().can_undo());

        let table = "<table><tr><td>a</td></tr></table>";
        let cases = [
            (PasteMode::PlainText, "plain", None),
            (PasteMode::Rich, "plain", None),
            (PasteMode::Rich, "表のplain", Some(table)),
            (PasteMode::Rich, "plain", Some("<b>x</b>")),
        ];
        for (mode, plain, html) in cases {
            set_clipboard(cx, plain);
            set_test_snapshot(html, false, None);
            paste(&view, cx, mode);
            cx.run_until_parked();
            assert_eq!(text_of(&view, cx), "ab\n");
            view.read_with(cx, |view, _| {
                assert_eq!(view.editor().selection(), forward(0, 1));
                assert_eq!(view.editor().can_undo(), can_undo);
                assert!(view.clipboard_job.is_none());
                assert_eq!(view.status, None);
            });
        }
    }

    /// Leaving the tab a conversion started on invalidates it, even when the
    /// user comes back to the same session, instance, revision and selection.
    #[gpui::test]
    fn a_tab_round_trip_drops_a_running_conversion(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "ab\n");
        select(&view, cx, forward(0, 2));
        set_clipboard(cx, "");
        set_test_snapshot(Some("<b>x</b>"), false, None);
        let first = view.read_with(cx, |view, _| view.sessions.active_id());
        let second = view.update(cx, |view, _| {
            let second = view.sessions.open_untitled("other\n", "Other");
            // Whether or not opening activates it, start on the first one.
            assert!(view.sessions.activate(first));
            second
        });
        paste(&view, cx, PasteMode::Rich);
        view.read_with(cx, |view, _| assert!(view.clipboard_job.is_some()));
        view.update(cx, |view, cx| {
            assert!(view.activate_session(second, cx));
            assert!(view.activate_session(first, cx));
        });
        view.read_with(cx, |view, _| {
            assert!(view.clipboard_job.is_none());
            assert_eq!(view.status, None);
        });
        cx.run_until_parked();
        assert_eq!(text_of(&view, cx), "ab\n");
        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().selection(), forward(0, 2));
            assert_eq!(view.status, None);
        });
    }

    /// Copy and Cut invalidate a running conversion too.
    #[gpui::test]
    fn copy_invalidates_a_running_conversion(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "ab\n");
        select(&view, cx, forward(0, 2));
        set_clipboard(cx, "");
        set_test_snapshot(Some("<b>x</b>"), false, None);
        paste(&view, cx, PasteMode::Rich);
        dispatch(cx, Box::new(crate::actions::Copy));
        cx.run_until_parked();
        assert_eq!(text_of(&view, cx), "ab\n");
        view.read_with(cx, |view, _| {
            assert!(view.clipboard_job.is_none());
            assert_eq!(view.status, None);
        });
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("ab".to_owned())
        );
    }
}
