//! Drying ink (Issue #779, phase P2): text that was just confirmed is drawn a
//! little stronger and settles back to the normal color as it dries.
//!
//! The view owns one [`InkLedger`] for the document on screen and one timer.
//! Neither touches the document, the selection, the IME state or the history:
//! the ledger only follows edits through `RopeBuffer::deltas_since`, and the
//! renderer reads it once per frame. Time always comes from the executor's
//! clock, so tests drive it with `advance_clock`. There is a timer only while
//! some range is still wet, and it sleeps until the next level change; nothing
//! here asks for animation frames.

use super::*;
use crate::ink::{InkFrame, InkLedger, InkOwner, InkParams, rebase_range};

/// The ledger and the single wake-up timer of the visible document.
pub(super) struct DryingInkState {
    ledger: InkLedger,
    /// When the pending wake-up fires, and the task that will fire it. Dropping
    /// the task cancels it.
    timer: Option<(Instant, Task<()>)>,
}

impl DryingInkState {
    pub(super) fn new(epoch: Instant) -> Self {
        Self {
            ledger: InkLedger::new(epoch),
            timer: None,
        }
    }

    /// Forgets everything and stops the timer.
    pub(super) fn reset(&mut self) {
        self.ledger.reset();
        self.timer = None;
    }
}

/// Parameters in effect. Speed and strength are fixed for now, and the band
/// is not drawn yet, so it is never created.
fn ink_params() -> InkParams {
    InkParams {
        band_enabled: false,
        ..InkParams::default()
    }
}

fn hull(pieces: [Option<SourceRange>; 2]) -> Option<SourceRange> {
    pieces
        .into_iter()
        .flatten()
        .reduce(|a, b| SourceRange::new(a.start.0.min(b.start.0), a.end.0.max(b.end.0)))
}

/// Carries `range`, valid at `revision`, to the document's current revision.
/// `None` when the history is no longer available or nothing of it survives.
fn carry_range(document: &RopeBuffer, range: SourceRange, revision: Revision) -> Option<SourceRange> {
    let deltas = document.deltas_since(revision).ok()?;
    deltas
        .iter()
        .try_fold(range, |range, delta| hull(rebase_range(range, delta)))
}

impl EditorView {
    /// Whether the drying-ink master switch is on.
    pub(crate) fn drying_ink_enabled(&self) -> bool {
        self.settings.drying_ink
    }

    /// Turns the master switch on or off. Turning it off drops every range and
    /// stops the timer. Only the display depends on it, so it is allowed
    /// during a composition. Returns whether the value is now the requested one.
    pub(crate) fn set_drying_ink(&mut self, on: bool, cx: &mut Context<Self>) -> bool {
        if self.settings.drying_ink == on {
            return true;
        }
        self.settings.drying_ink = on;
        if !on {
            self.drying_ink.reset();
        }
        // A failed write keeps the change for this run, like the theme.
        self.store_settings();
        cx.notify();
        true
    }

    fn ink_owner(&self) -> InkOwner {
        InkOwner {
            session: self.sessions.active_id(),
            generation: self.sessions.active().generation(),
            workspace: self.work_folder_generation,
        }
    }

    /// Lets the ledger follow the edits made since it last looked.
    pub(crate) fn sync_drying_ink(&mut self) {
        if !self.settings.drying_ink {
            return;
        }
        let owner = self.ink_owner();
        let document = self.sessions.active().editor().document();
        self.drying_ink.ledger.sync(owner, document);
    }

    /// Records text confirmed into `range`, a range valid at `revision` (the
    /// revision the confirming edit produced). Call it after `after_input`, or
    /// after the ledger has otherwise been synced, so a rewrite that ran
    /// right after the edit has already been followed.
    pub(crate) fn note_ink_commit(
        &mut self,
        range: SourceRange,
        revision: Revision,
        text: &str,
        cx: &mut Context<Self>,
    ) {
        if !self.settings.drying_ink || !self.body_editable() {
            return;
        }
        self.sync_drying_ink();
        let Some(range) = carry_range(self.sessions.active().editor().document(), range, revision)
        else {
            return;
        };
        let now = cx.background_executor().now();
        self.drying_ink
            .ledger
            .record_commit(range, text, now, &ink_params());
        self.schedule_drying_ink_wakeup(cx);
    }

    /// Types `text` at the caret the way the platform delivers a plain
    /// keystroke, and marks what it inserted as wet.
    pub(crate) fn insert_typed_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if !self.body_editable() {
            return;
        }
        let summary = match self.editor_mut().insert_text(text) {
            Ok(summary) => {
                self.status = None;
                Some(summary)
            }
            Err(error) => {
                self.report_error("text input", error);
                None
            }
        };
        self.after_input(cx);
        if let Some(summary) = summary {
            self.note_ink_commit(summary.range_after, summary.revision_after, text, cx);
        }
    }

    /// What the renderer needs for this frame. Empty while reading and while
    /// the switch is off. Also keeps the timer pointed at the next change.
    pub(crate) fn drying_ink_frame(&mut self, cx: &mut Context<Self>) -> InkFrame {
        let params = ink_params();
        if !self.settings.drying_ink {
            self.drying_ink.timer = None;
            return InkFrame {
                wet: Vec::new(),
                band: None,
                params,
            };
        }
        self.sync_drying_ink();
        let now = cx.background_executor().now();
        let frame = self.drying_ink.ledger.frame(now, &params);
        self.schedule_drying_ink_wakeup(cx);
        if self.body_editable() {
            frame
        } else {
            // Time keeps running while reading, but nothing is drawn.
            InkFrame {
                wet: Vec::new(),
                band: None,
                params,
            }
        }
    }

    /// Sleeps until the next level change, or stops the timer when nothing is
    /// wet. One timer at most: an unchanged target keeps the pending one.
    pub(crate) fn schedule_drying_ink_wakeup(&mut self, cx: &mut Context<Self>) {
        let now = cx.background_executor().now();
        let next = (self.settings.drying_ink && self.body_editable())
            .then(|| self.drying_ink.ledger.next_wakeup(now, &ink_params()))
            .flatten();
        let Some(at) = next else {
            self.drying_ink.timer = None;
            return;
        };
        if self
            .drying_ink
            .timer
            .as_ref()
            .is_some_and(|(scheduled, _)| *scheduled == at)
        {
            return;
        }
        let delay = at.saturating_duration_since(now);
        let task = cx.spawn(async move |view, cx| {
            cx.background_executor().timer(delay).await;
            // Only asks for a redraw; the frame decides what changed and
            // whether another wake-up is needed.
            let _ = view.update(cx, |_, cx| cx.notify());
        });
        self.drying_ink.timer = Some((at, task));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::EntityInputHandler;

    type TestView = gpui::Entity<EditorView>;
    type TestCx = gpui::VisualTestContext;

    fn settle(cx: &mut TestCx) {
        cx.run_until_parked();
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(100));
        cx.run_until_parked();
    }

    fn open_view<'a>(
        cx: &'a mut gpui::TestAppContext,
        source: &str,
        stores: StateStores,
    ) -> (TestView, &'a mut TestCx) {
        let source = source.to_owned();
        let (view, cx) = cx.add_window_view(move |_, cx| {
            EditorView::from_sessions(
                SessionSet::with_untitled(&source, "Untitled"),
                Arc::new(OsFileService),
                stores,
                cx,
            )
        });
        cx.simulate_resize(gpui::size(px(800.0), px(600.0)));
        settle(cx);
        (view, cx)
    }

    fn replace(view: &TestView, cx: &mut TestCx, text: &str) {
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.replace_text_in_range(None, text, window, cx);
            });
        });
        cx.run_until_parked();
    }

    fn mark(view: &TestView, cx: &mut TestCx, text: &str) {
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.replace_and_mark_text_in_range(None, text, None, window, cx);
            });
        });
        cx.run_until_parked();
    }

    fn unmark(view: &TestView, cx: &mut TestCx) {
        cx.update(|window, app| {
            view.update(app, |view, cx| view.unmark_text(window, cx));
        });
        cx.run_until_parked();
    }

    /// Wet ranges as `(start, end, level)`, read the way the renderer reads them.
    fn wet(view: &TestView, cx: &mut TestCx) -> Vec<(usize, usize, u8)> {
        view.update(cx, |view, cx| {
            view.drying_ink_frame(cx)
                .wet
                .iter()
                .map(|(range, level)| (range.start.0, range.end.0, *level))
                .collect()
        })
    }

    fn timer_active(view: &TestView, cx: &mut TestCx) -> bool {
        view.read_with(cx, |view, _| view.drying_ink.timer.is_some())
    }

    fn text_of(view: &TestView, cx: &mut TestCx) -> String {
        view.read_with(cx, |view, _| view.editor().document().full_text())
    }

    fn doc_state(view: &TestView, cx: &mut TestCx) -> (Revision, String, bool, bool, bool) {
        view.read_with(cx, |view, _| {
            (
                view.editor().document().revision(),
                view.editor().document().full_text(),
                view.sessions.active().is_dirty(),
                view.editor().can_undo(),
                view.editor().can_redo(),
            )
        })
    }

    fn count_notifications(
        view: &TestView,
        cx: &mut TestCx,
    ) -> std::rc::Rc<std::cell::Cell<usize>> {
        let count = std::rc::Rc::new(std::cell::Cell::new(0));
        let observed = std::rc::Rc::clone(&count);
        cx.update(|_, app| {
            app.observe(view, move |_, _| observed.set(observed.get() + 1))
                .detach();
        });
        count
    }

    fn advance(cx: &mut TestCx, duration: Duration) {
        cx.executor().advance_clock(duration);
        cx.run_until_parked();
    }

    #[gpui::test]
    fn the_switch_defaults_to_on(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "", StateStores::memory());
        view.read_with(cx, |view, _| assert!(view.drying_ink_enabled()));
        assert!(!timer_active(&view, cx));
    }

    #[gpui::test]
    fn direct_input_gets_wet(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "", StateStores::memory());
        replace(&view, cx, "abc");
        assert_eq!(wet(&view, cx), vec![(0, 3, 4)]);
        assert!(timer_active(&view, cx));
    }

    #[gpui::test]
    fn committed_composition_gets_wet_but_marking_does_not(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "", StateStores::memory());
        mark(&view, cx, "か");
        assert!(wet(&view, cx).is_empty());
        mark(&view, cx, "漢");
        assert!(wet(&view, cx).is_empty());
        assert!(!timer_active(&view, cx));
        replace(&view, cx, "漢字");
        assert_eq!(wet(&view, cx), vec![(0, 6, 4)]);
    }

    #[gpui::test]
    fn unmark_text_makes_the_composed_text_wet(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "", StateStores::memory());
        mark(&view, cx, "かな");
        assert!(wet(&view, cx).is_empty());
        unmark(&view, cx);
        assert_eq!(text_of(&view, cx), "かな");
        assert_eq!(wet(&view, cx), vec![(0, 6, 4)]);
        assert!(timer_active(&view, cx));
    }

    #[gpui::test]
    fn cancelled_composition_does_not_get_wet(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "", StateStores::memory());
        mark(&view, cx, "か");
        replace(&view, cx, "");
        assert_eq!(text_of(&view, cx), "");
        assert!(wet(&view, cx).is_empty());
        assert!(!timer_active(&view, cx));
    }

    #[gpui::test]
    fn paste_bulk_replace_and_ai_insertions_stay_dry(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "xyz", StateStores::memory());
        // A paste goes through `replace_range_recorded`.
        view.update(cx, |view, cx| {
            view.editor_mut()
                .replace_range_recorded(
                    SourceRange::new(0, 1),
                    "PQ",
                    Selection::caret(SourceOffset(2)),
                )
                .unwrap();
            view.after_input(cx);
        });
        assert!(wet(&view, cx).is_empty());
        // Replace-all and AI insertions use the plain `insert_text`.
        view.update(cx, |view, cx| view.insert_text("zz", cx));
        assert!(wet(&view, cx).is_empty());
        assert!(!timer_active(&view, cx));
    }

    #[gpui::test]
    fn undo_removes_the_ink_and_redo_does_not_bring_it_back(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "", StateStores::memory());
        replace(&view, cx, "abc");
        assert_eq!(wet(&view, cx), vec![(0, 3, 4)]);
        view.update(cx, |view, cx| view.dispatch(EditorCommand::Undo, cx));
        assert_eq!(text_of(&view, cx), "");
        assert!(wet(&view, cx).is_empty());
        view.update(cx, |view, cx| view.dispatch(EditorCommand::Redo, cx));
        assert_eq!(text_of(&view, cx), "abc");
        assert!(wet(&view, cx).is_empty());
        cx.run_until_parked();
        assert!(!timer_active(&view, cx));
    }

    #[gpui::test]
    fn later_edits_move_the_wet_range(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "", StateStores::memory());
        replace(&view, cx, "abc");
        // A plain insertion before the wet text shifts it without wetting itself.
        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset(0)))
                .unwrap();
            view.insert_text("ZZ", cx);
        });
        assert_eq!(wet(&view, cx), vec![(2, 5, 4)]);
    }

    #[gpui::test]
    fn replacing_the_document_clears_everything(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "", StateStores::memory());
        replace(&view, cx, "abc");
        assert!(timer_active(&view, cx));
        view.update(cx, |view, _| view.on_document_replaced());
        assert!(!timer_active(&view, cx));
        assert!(wet(&view, cx).is_empty());
    }

    #[gpui::test]
    fn nothing_is_drawn_while_reading(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "", StateStores::memory());
        replace(&view, cx, "abc");
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                assert!(view.set_editor_mode(EditorMode::Reading, window, cx));
            });
        });
        cx.run_until_parked();
        assert!(wet(&view, cx).is_empty());
    }

    #[gpui::test]
    fn turning_the_switch_off_clears_the_ink_and_stops_the_timer(cx: &mut gpui::TestAppContext) {
        let stores = StateStores::memory();
        let (view, cx) = open_view(cx, "", stores.clone());
        replace(&view, cx, "abc");
        assert!(timer_active(&view, cx));
        view.update(cx, |view, cx| assert!(view.set_drying_ink(false, cx)));
        cx.run_until_parked();
        assert!(!timer_active(&view, cx));
        assert!(wet(&view, cx).is_empty());
        assert!(!stores.settings().load().drying_ink);
        replace(&view, cx, "def");
        assert!(wet(&view, cx).is_empty());
        assert!(!timer_active(&view, cx));
        view.update(cx, |view, cx| assert!(view.set_drying_ink(true, cx)));
        replace(&view, cx, "ghi");
        assert_eq!(wet(&view, cx).len(), 1);
        assert!(stores.settings().load().drying_ink);
    }

    #[gpui::test]
    fn ink_dries_in_steps_then_the_timer_and_notifications_stop(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, "", StateStores::memory());
        replace(&view, cx, "abc");
        advance(cx, Duration::from_secs(10));
        assert_eq!(wet(&view, cx), vec![(0, 3, 4)]);
        advance(cx, Duration::from_secs(25));
        let level = wet(&view, cx)[0].2;
        assert!((1..4).contains(&level), "level {level}");
        assert!(timer_active(&view, cx));
        advance(cx, Duration::from_secs(40));
        assert!(wet(&view, cx).is_empty());
        cx.run_until_parked();
        assert!(!timer_active(&view, cx));
        let notifications = count_notifications(&view, cx);
        advance(cx, Duration::from_secs(30));
        assert_eq!(notifications.get(), 0);
        assert!(!timer_active(&view, cx));
    }

    type Rendered = (
        Vec<VisualBlock>,
        Option<(f32, f32, f32)>,
        (Revision, String, bool, bool, bool),
        (usize, usize),
    );

    fn rendered(view: &TestView, cx: &mut TestCx) -> Rendered {
        cx.run_until_parked();
        let state = doc_state(view, cx);
        view.read_with(cx, |view, _| {
            let mut blocks = view.block_cache.values().cloned().collect::<Vec<_>>();
            blocks.sort_by_key(|block| block.source_range.start.0);
            let selection = view.editor().selection();
            (
                blocks,
                view.caret_geometry()
                    .map(|caret| (caret.x, caret.y, caret.height)),
                state,
                (selection.anchor.0, selection.active.0),
            )
        })
    }

    fn typed_in(cx: &mut gpui::TestAppContext, ink: bool) -> (Rendered, Rendered) {
        let (view, cx) = open_view(cx, "# Title\n\nbody\n", StateStores::memory());
        view.update(cx, |view, cx| assert!(view.set_drying_ink(ink, cx)));
        view.update(cx, |view, _| {
            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset(14)))
                .unwrap();
        });
        replace(&view, cx, " added");
        let wet_state = rendered(&view, cx);
        advance(cx, Duration::from_secs(90));
        let dry_state = rendered(&view, cx);
        (wet_state, dry_state)
    }

    #[gpui::test]
    fn ink_changes_neither_layout_nor_document_state(cx: &mut gpui::TestAppContext) {
        let (with_wet, with_dry) = typed_in(cx, true);
        let (without_wet, without_dry) = typed_in(cx, false);
        // Wet, dried and switched off: the presentation, the caret, the
        // document state and the selection are the same.
        assert_eq!(with_wet, without_wet);
        assert_eq!(with_wet, with_dry);
        assert_eq!(without_wet, without_dry);
    }
}
