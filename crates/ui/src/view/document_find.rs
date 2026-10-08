//! Document-find entry bar: open/close, focus isolation, and (as of this
//! stage) running a literal search against the active session's own
//! in-memory `RopeBuffer` to show an exact match count, kept current with
//! the buffer's own revision while the bar is open — including across an
//! edit, Undo/Redo, or switching to a different open document — without
//! rescanning on a selection-only change or on every render frame.
//! Highlighting matches in the document body and previous/next navigation
//! are later stages (see Issue #413); this bar still never touches the
//! active document's text, selection, revision, dirty flag, undo history, or
//! IME composition, and does not move the caret when a search starts,
//! finishes, or the bar closes. Reuses `hane_editor::find`'s
//! `FindQuery`/`FindOptions`/`scan` rather than a second search engine or the
//! Work-folder `content_search`'s disk-backed one.
//!
//! Kept entirely separate from `content_search` (the Work-folder body search
//! living in the sidebar, opened with Cmd/Ctrl+Shift+F): different shortcut
//! (Cmd/Ctrl+F), different field, different focus flag, so the two can be
//! open at the same time without either stealing the other's state.

use super::*;
use gpui::{AppContext, Entity};
use gpui_component::input::{Input, InputEvent, InputState};
use hane_editor::{FindOptions, FindQuery, FindQueryError, FindScan, find_scan};
use std::sync::atomic::{AtomicBool, Ordering};

const DOCUMENT_FIND_BAR_HEIGHT: f32 = SIDEBAR_FILTER_HEIGHT;

/// Shorter than the Work-folder content search's debounce: this find only
/// scans the active session's own in-memory buffer (no disk I/O, no
/// multi-file fan-out), so a results refresh is cheap enough to coalesce
/// over a much shorter pause between keystrokes.
const DOCUMENT_FIND_DEBOUNCE: Duration = Duration::from_millis(120);

/// Identifies the document a scan was started against. A scan result whose
/// target no longer matches the active session's current target (different
/// session, the session's document was replaced, or it was edited) is
/// dropped instead of applied — including when only the session differs but
/// the revision coincidentally matches.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DocumentFindTarget {
    session: SessionId,
    generation: u64,
    revision: Revision,
}

/// What the find bar currently has to show, distinguishing every state the
/// bar's count display must tell apart.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) enum DocumentFindResult {
    /// No query entered yet.
    #[default]
    Empty,
    /// A non-empty query that cannot be searched as-is (spans multiple
    /// lines, or exceeds `MAX_QUERY_BYTES`).
    Invalid(FindQueryError),
    /// A valid query is debouncing or its scan has not completed yet.
    Pending,
    /// A scan for the current query/options/document completed.
    Ready { count: usize, truncated: bool },
}

#[derive(Default)]
pub(super) struct DocumentFindState {
    open: bool,
    input: Option<Entity<InputState>>,
    input_subscription: Option<Subscription>,
    /// Whether the find bar's own input currently holds keyboard focus, as
    /// opposed to the document body sharing the view's focus handle. This is
    /// the only state other actions (see `actions.rs`) check before routing
    /// document-editing keys, so it must be kept in lockstep with the
    /// input's real `Focus`/`Blur` events rather than inferred.
    input_focused: bool,
    case_sensitive: bool,
    query_text: String,
    /// Bumped on every query/options change (and on close), independently of
    /// the document's own revision, so a scan started for an earlier query
    /// or option is rejected even if it raced back after the current one.
    query_generation: u64,
    debounce_active: bool,
    debounce_task_id: u64,
    last_query_change: Option<Instant>,
    /// Whether a scan's background task is currently outstanding. Stays
    /// `true` for a cancelled scan until its task actually returns, so that
    /// slot still counts against the "one running job" cap.
    job_active: bool,
    job_cancel: Option<Arc<AtomicBool>>,
    /// A newer request arrived while `job_active` was still `true`; once
    /// that job's task returns, the latest query/options/document at that
    /// time (not whatever was current when this was set) is scanned next.
    restart_pending: bool,
    /// The target `restart_document_find` last (re)computed `result` for.
    /// Compared against the active document's current target after every
    /// edit (see `resync_document_find_after_edit`) so an edit that actually
    /// moved the revision triggers a fresh scan, while a selection-only
    /// change — which also runs through the same `after_input` hook but
    /// never bumps the revision — does not.
    synced_target: Option<DocumentFindTarget>,
    result: DocumentFindResult,
}

impl EditorView {
    fn initialize_document_find_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.document_find.input.is_some() {
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("検索"));
        let subscription =
            cx.subscribe(&input, |view, input, event: &InputEvent, cx| match event {
                InputEvent::Focus => {
                    view.document_find.input_focused = true;
                    view.sidebar_keyboard_focus = false;
                    cx.notify();
                }
                InputEvent::Blur => {
                    view.document_find.input_focused = false;
                    cx.notify();
                }
                InputEvent::Change => {
                    let text = input.read(cx).value().to_string();
                    view.document_find_query_changed(text, cx);
                }
                InputEvent::PressEnter { .. } => {}
            });
        self.document_find.input = Some(input);
        self.document_find.input_subscription = Some(subscription);
    }

    pub(crate) fn document_find_input_is_focused(&self) -> bool {
        self.document_find.input_focused
    }

    pub(crate) fn document_find_should_leave_on_escape(&self) -> bool {
        self.document_find.open && self.document_find.input_focused
    }

    /// A non-empty, single-line selection in the active document's body, or
    /// `None` when the selection is empty or spans more than one line. Reads
    /// the selection only; never mutates it.
    fn document_find_selection_seed(&self) -> Option<String> {
        let editor = self.sessions.active().editor();
        let range = editor.selection().range();
        if range.start == range.end {
            return None;
        }
        let text = editor.selected_text().ok()?;
        if text.is_empty() || text.contains(['\n', '\r']) {
            return None;
        }
        Some(text)
    }

    /// Opens the bar and focuses its input, seeding the query from the
    /// current selection the first time the bar opens. Invoking this again
    /// while the bar is already open selects the current query text instead
    /// of reseeding it, matching the platform convention for re-pressing
    /// Cmd/Ctrl+F. Either way, the query currently shown is (re)searched
    /// against the active document.
    pub(crate) fn open_document_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Only the filename filter (and inline rename) should block opening
        // this bar. `sidebar_filter_is_focused()` also reports focus for the
        // Work-folder content search's own input/results, but Cmd/Ctrl+F
        // from content search must be able to hand focus over to this bar
        // (see `blur_content_search_focus` below), not be swallowed here.
        if self.inline_rename_active() || self.sidebar_filter_focused {
            return;
        }
        self.initialize_document_find_input(window, cx);
        let already_open = self.document_find.open;
        if !already_open {
            self.document_find.open = true;
            if let Some(seed) = self.document_find_selection_seed()
                && let Some(input) = self.document_find.input.as_ref()
            {
                input.update(cx, |state, cx| state.set_value(seed, window, cx));
            }
        }
        if let Some(input) = self.document_find.input.as_ref() {
            input.update(cx, |state, cx| {
                state.select_all(window, cx);
                state.focus(window, cx);
            });
        }
        // Opening the find bar hides neither the Work-folder content search
        // sidebar nor the document body, but only one field should ever
        // claim `*_input_is_focused()` at a time, or the guards in
        // `actions.rs` could let a document-editing key reach the body while
        // this bar's input visibly holds focus.
        self.blur_content_search_focus(cx);
        self.document_find.input_focused = true;
        self.sidebar_keyboard_focus = false;
        let text = self
            .document_find
            .input
            .as_ref()
            .map_or_else(String::new, |input| input.read(cx).value().to_string());
        self.document_find.query_text = text;
        self.restart_document_find(cx, false);
    }

    /// Hides the bar and returns GPUI focus to the document body. Returns
    /// `false` without changing anything when the bar was not open, so
    /// callers (see the `CancelComposition`/Escape handler) can fall through
    /// to other Escape behavior.
    pub(crate) fn leave_document_find(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.document_find.open {
            return false;
        }
        self.document_find.open = false;
        self.document_find.input_focused = false;
        self.cancel_document_find_work();
        window.focus(&self.focus_handle, cx);
        cx.notify();
        true
    }

    /// Re-runs the current query against whatever document is now active,
    /// so a tab switch (or the active tab closing) while the bar is open
    /// never leaves a match count attributed to a document that is no
    /// longer showing. A no-op while the bar is closed.
    pub(super) fn resync_document_find_for_active_document(&mut self, cx: &mut Context<Self>) {
        if !self.document_find.open {
            return;
        }
        self.restart_document_find(cx, false);
    }

    /// Called after every edit to the active document (see `after_input`),
    /// which also fires for selection-only changes (a mouse click or caret
    /// move) that never move the revision. Only an edit that actually
    /// advances `document_find_target()` past what `result` was last
    /// computed for restarts the scan — and does so debounced, the same as
    /// a keystroke in the query field, so a run of edits coalesces into one
    /// rescan instead of one per edit. A no-op while the bar is closed.
    pub(super) fn resync_document_find_after_edit(&mut self, cx: &mut Context<Self>) {
        if !self.document_find.open {
            return;
        }
        if self.document_find.synced_target == Some(self.document_find_target()) {
            return;
        }
        self.restart_document_find(cx, true);
    }

    fn document_find_query_changed(&mut self, text: String, cx: &mut Context<Self>) {
        self.document_find.query_text = text;
        self.restart_document_find(cx, true);
    }

    pub(super) fn toggle_document_find_case(&mut self, cx: &mut Context<Self>) {
        self.document_find.case_sensitive = !self.document_find.case_sensitive;
        self.restart_document_find(cx, false);
    }

    /// Cancels whatever scan is in flight or about to start, and bumps
    /// `query_generation` so a result that nonetheless lands afterward is
    /// rejected. Leaves `result` untouched; callers that are about to
    /// recompute it (or close the bar, which stops rendering it) overwrite
    /// or ignore it on their own.
    fn cancel_document_find_work(&mut self) {
        self.document_find.query_generation = self.document_find.query_generation.wrapping_add(1);
        self.document_find.debounce_active = false;
        self.document_find.debounce_task_id = self.document_find.debounce_task_id.wrapping_add(1);
        self.document_find.last_query_change = None;
        self.document_find.restart_pending = false;
        if let Some(cancel) = self.document_find.job_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    /// The single entry point for "the query, options, or target document
    /// just changed": classifies the current query text, and either starts
    /// debouncing or scans immediately (`debounce = false` for everything
    /// that is not a keystroke — opening the bar, toggling case, or a tab
    /// switch — since only typing needs coalescing).
    fn restart_document_find(&mut self, cx: &mut Context<Self>, debounce: bool) {
        self.document_find.synced_target = Some(self.document_find_target());
        self.cancel_document_find_work();
        match FindQuery::parse(&self.document_find.query_text) {
            Ok(_) => {
                self.document_find.result = DocumentFindResult::Pending;
                if debounce {
                    self.document_find.last_query_change = Some(Instant::now());
                    self.schedule_document_find_debounce(cx);
                } else {
                    self.start_document_find_scan(cx);
                }
            }
            Err(FindQueryError::Empty) => {
                self.document_find.result = DocumentFindResult::Empty;
            }
            Err(error) => {
                self.document_find.result = DocumentFindResult::Invalid(error);
            }
        }
        cx.notify();
    }

    fn schedule_document_find_debounce(&mut self, cx: &mut Context<Self>) {
        if self.document_find.debounce_active {
            return;
        }
        self.document_find.debounce_active = true;
        self.document_find.debounce_task_id = self.document_find.debounce_task_id.wrapping_add(1);
        let task_id = self.document_find.debounce_task_id;
        cx.spawn(async move |view, cx| {
            loop {
                let delay = view
                    .read_with(cx, |view, _| {
                        let state = &view.document_find;
                        (state.debounce_active && state.debounce_task_id == task_id).then(|| {
                            state.last_query_change.map_or(DOCUMENT_FIND_DEBOUNCE, |at| {
                                DOCUMENT_FIND_DEBOUNCE.saturating_sub(at.elapsed())
                            })
                        })
                    })
                    .unwrap_or(None);
                let Some(delay) = delay else {
                    break;
                };
                cx.background_executor().timer(delay).await;
                let keep_waiting = view
                    .update(cx, |view, cx| {
                        let state = &mut view.document_find;
                        if !state.debounce_active || state.debounce_task_id != task_id {
                            return false;
                        }
                        if state
                            .last_query_change
                            .is_some_and(|at| at.elapsed() < DOCUMENT_FIND_DEBOUNCE)
                        {
                            return true;
                        }
                        state.debounce_active = false;
                        state.last_query_change = None;
                        view.start_document_find_scan(cx);
                        false
                    })
                    .unwrap_or(false);
                if !keep_waiting {
                    break;
                }
            }
        })
        .detach();
    }

    fn document_find_target(&self) -> DocumentFindTarget {
        let session = self.sessions.active();
        DocumentFindTarget {
            session: session.id(),
            generation: session.generation(),
            revision: session.revision(),
        }
    }

    /// Starts a scan for the current query against the active document, or
    /// — if a previous scan's task has not returned yet, including one that
    /// was already cancelled — marks that the latest request should be
    /// (re)started once it does. Never spawns a second background task while
    /// one is outstanding, so this view's find bar never has more than one
    /// running job plus one coalesced pending request.
    fn start_document_find_scan(&mut self, cx: &mut Context<Self>) {
        if self.document_find.job_active {
            self.document_find.restart_pending = true;
            return;
        }
        let Ok(query) = FindQuery::parse(&self.document_find.query_text) else {
            // The query became empty/invalid since this was scheduled;
            // `restart_document_find` already set the matching `result`.
            return;
        };
        let options = FindOptions {
            case_sensitive: self.document_find.case_sensitive,
        };
        let target = self.document_find_target();
        let query_generation = self.document_find.query_generation;
        // Cloning shares the underlying rope's persistent tree; the scan
        // itself (window-bounded, never a whole-document `String`) runs on
        // the background executor below, not here on the UI thread.
        let buffer = self.editor().document().clone();
        let cancel = Arc::new(AtomicBool::new(false));
        self.document_find.job_active = true;
        self.document_find.job_cancel = Some(cancel.clone());
        cx.spawn(async move |view, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    find_scan(&buffer, &query, options, || cancel.load(Ordering::Relaxed))
                })
                .await;
            let _ = view.update(cx, |view, cx| {
                view.finish_document_find_scan(target, query_generation, outcome, cx);
            });
        })
        .detach();
    }

    fn finish_document_find_scan(
        &mut self,
        target: DocumentFindTarget,
        query_generation: u64,
        outcome: FindScan,
        cx: &mut Context<Self>,
    ) {
        self.document_find.job_active = false;
        self.document_find.job_cancel = None;
        if query_generation == self.document_find.query_generation
            && target == self.document_find_target()
            && let FindScan::Completed(results) = outcome
        {
            self.document_find.result = DocumentFindResult::Ready {
                count: results.matches.len(),
                truncated: results.truncated,
            };
        }
        if self.document_find.restart_pending {
            self.document_find.restart_pending = false;
            self.start_document_find_scan(cx);
        }
        cx.notify();
    }

    fn document_find_status_text(&self) -> String {
        match &self.document_find.result {
            DocumentFindResult::Empty | DocumentFindResult::Invalid(FindQueryError::Empty) => {
                String::new()
            }
            DocumentFindResult::Invalid(FindQueryError::MultiLine) => {
                "複数行は検索できません".to_owned()
            }
            DocumentFindResult::Invalid(FindQueryError::TooLong { .. }) => {
                "検索語が長すぎます".to_owned()
            }
            DocumentFindResult::Pending => "検索中…".to_owned(),
            DocumentFindResult::Ready { count: 0, .. } => "一致なし".to_owned(),
            DocumentFindResult::Ready {
                count,
                truncated: true,
            } => format!("{count}件以上"),
            DocumentFindResult::Ready {
                count,
                truncated: false,
            } => format!("{count}件"),
        }
    }

    pub(super) fn document_find_bar(&self, cx: &mut Context<Self>) -> Option<gpui::Stateful<gpui::Div>> {
        if !self.document_find.open {
            return None;
        }
        let input = self
            .document_find
            .input
            .as_ref()
            .expect("the find bar's input is initialized before it is opened");
        let search_input = Input::new(input)
            .id("document-find-input")
            .h(px(DOCUMENT_FIND_BAR_HEIGHT))
            .appearance(false)
            .bordered(false)
            .focus_bordered(false);
        let search_box = div()
            .id("document-find-query")
            .h(px(DOCUMENT_FIND_BAR_HEIGHT))
            .flex_1()
            .min_w(px(0.0))
            .px(px(4.0))
            .rounded_sm()
            .border_1()
            .border_color(rgb(self.theme.sidebar_foreground))
            .bg(rgb(self.theme.code_background))
            .child(search_input);
        let status = div()
            .id("document-find-status")
            .debug_selector(|| "document-find-status".to_owned())
            .h(px(DOCUMENT_FIND_BAR_HEIGHT))
            .px(px(6.0))
            .flex_none()
            .flex()
            .items_center()
            .truncate()
            .child(self.document_find_status_text());
        let case_toggle = div()
            .id("document-find-case")
            .debug_selector(|| "document-find-case".to_owned())
            .h(px(DOCUMENT_FIND_BAR_HEIGHT))
            .px(px(6.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded_sm()
            .cursor_pointer()
            .bg(rgb(self.theme.code_background))
            .child(if self.document_find.case_sensitive {
                "Aa✓"
            } else {
                "Aa"
            })
            .on_click(cx.listener(|view, _, _, cx| view.toggle_document_find_case(cx)));
        let close = div()
            .id("document-find-close")
            .h(px(DOCUMENT_FIND_BAR_HEIGHT))
            .px(px(6.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded_sm()
            .cursor_pointer()
            .bg(rgb(self.theme.code_background))
            .child("閉じる")
            .on_click(cx.listener(|view, _, window, cx| {
                view.leave_document_find(window, cx);
            }));
        Some(
            // Absolutely positioned over the top of the editor viewport
            // (directly under the tab bar) instead of a flex sibling that
            // would push it down: `self.viewport_height` and the
            // mouse-to-row math in `on_editor_mouse_down`/`on_row_mouse_down`
            // are derived from the header/footer heights alone and must not
            // shift just because this bar is open (Issue #413).
            div()
                .id("document-find-bar")
                .debug_selector(|| "document-find-bar".to_owned())
                .absolute()
                .top(px(0.0))
                .left(px(0.0))
                .right(px(0.0))
                .h(px(DOCUMENT_FIND_BAR_HEIGHT + SIDEBAR_FILTER_GAP * 2.0))
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py(px(SIDEBAR_FILTER_GAP))
                .bg(rgb(self.theme.header_background))
                .text_color(rgb(self.theme.header_foreground))
                .child(search_box)
                .child(status)
                .child(case_toggle)
                .child(close),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn secondary_f_opens_the_bar_and_focuses_its_input_without_touching_the_document(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("one\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        // Focus the editor's "HaneEditor" key context the way a real click
        // does, so secondary-f is dispatched the same way a user would hit
        // it, not just called as a direct method.
        let point = cx
            .debug_bounds("row-0-0")
            .expect("first row painted")
            .center();
        cx.simulate_mouse_down(point, MouseButton::Left, gpui::Modifiers::none());
        cx.simulate_mouse_up(point, MouseButton::Left, gpui::Modifiers::none());
        cx.run_until_parked();

        cx.simulate_keystrokes("secondary-f");
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert!(view.document_find.open);
            assert!(view.document_find_input_is_focused());
            // Opening the bar must never touch the document it floats over.
            assert_eq!(view.editor().document().full_text(), "one\n");
        });
    }

    #[gpui::test]
    fn opening_seeds_the_query_from_a_non_empty_single_line_selection(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) =
            cx.add_window_view(|_, cx| EditorView::new("needle and thread\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection {
                    anchor: SourceOffset(0),
                    active: SourceOffset(6),
                })
                .unwrap();
            cx.notify();
        });
        cx.run_until_parked();

        cx.update(|window, app| {
            view.update(app, |view, cx| view.open_document_find(window, cx));
        });
        cx.run_until_parked();

        view.read_with(cx, |view, app| {
            let value = view
                .document_find
                .input
                .as_ref()
                .expect("find input is initialized by open_document_find")
                .read(app)
                .value()
                .to_string();
            assert_eq!(value, "needle");
            // Seeding only reads the selection; it must never change it or
            // the document's text.
            assert_eq!(
                view.editor().selection(),
                Selection {
                    anchor: SourceOffset(0),
                    active: SourceOffset(6),
                }
            );
            assert_eq!(view.editor().document().full_text(), "needle and thread\n");
        });
    }

    #[gpui::test]
    fn opening_does_not_seed_the_query_from_a_multi_line_selection(cx: &mut gpui::TestAppContext) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("one\ntwo\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        view.update(cx, |view, cx| {
            // "one\ntwo", spanning the line break.
            view.editor_mut()
                .set_selection(Selection {
                    anchor: SourceOffset(0),
                    active: SourceOffset(7),
                })
                .unwrap();
            cx.notify();
        });
        cx.run_until_parked();

        cx.update(|window, app| {
            view.update(app, |view, cx| view.open_document_find(window, cx));
        });
        cx.run_until_parked();

        view.read_with(cx, |view, app| {
            let value = view
                .document_find
                .input
                .as_ref()
                .expect("find input is initialized by open_document_find")
                .read(app)
                .value()
                .to_string();
            assert_eq!(value, "");
        });
    }

    #[gpui::test]
    fn reinvoking_while_open_selects_the_current_query_instead_of_reseeding(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) =
            cx.add_window_view(|_, cx| EditorView::new("needle and thread\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection {
                    anchor: SourceOffset(0),
                    active: SourceOffset(6),
                })
                .unwrap();
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(|window, app| {
            view.update(app, |view, cx| view.open_document_find(window, cx));
        });
        cx.run_until_parked();
        view.read_with(cx, |view, app| {
            let value = view.document_find.input.as_ref().unwrap().read(app).value().to_string();
            assert_eq!(value, "needle");
        });

        // A different single-line selection exists now, but re-invoking the
        // shortcut while the bar is already open must select the existing
        // query text rather than reseed it from this new selection.
        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection {
                    anchor: SourceOffset(11),
                    active: SourceOffset(17),
                })
                .unwrap();
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(|window, app| {
            view.update(app, |view, cx| view.open_document_find(window, cx));
        });
        cx.run_until_parked();

        view.read_with(cx, |view, app| {
            let value = view.document_find.input.as_ref().unwrap().read(app).value().to_string();
            assert_eq!(value, "needle");
        });
    }

    #[gpui::test]
    fn escape_from_the_find_input_clears_focus_so_backspace_reaches_the_editor(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("one\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        cx.update(|window, app| {
            view.update(app, |view, cx| view.open_document_find(window, cx));
        });
        cx.run_until_parked();
        assert!(view.read_with(cx, |view, _| view.document_find_input_is_focused()));

        // Escape must hide the bar, clear `input_focused` (not just drop
        // `open`), and move real GPUI keyboard focus back to the editor, or
        // Backspace keeps being swallowed by the lingering flag in
        // `actions.rs`/reaches a now-unmounted input.
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(!view.document_find.open);
            assert!(!view.document_find_input_is_focused());
        });

        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset(3)))
                .unwrap();
            cx.notify();
        });
        cx.run_until_parked();

        cx.simulate_keystrokes("backspace");
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().document().full_text(), "on\n");
        });
    }

    #[gpui::test]
    fn backspace_while_the_find_input_is_focused_does_not_edit_the_document(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("one\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset(3)))
                .unwrap();
            cx.notify();
        });
        cx.run_until_parked();

        cx.update(|window, app| {
            view.update(app, |view, cx| view.open_document_find(window, cx));
        });
        cx.run_until_parked();
        assert!(view.read_with(cx, |view, _| view.document_find_input_is_focused()));

        // While the find bar's own input holds focus, Backspace (and every
        // other document-editing key guarded in `actions.rs`) must never
        // reach the document underneath it.
        cx.simulate_keystrokes("backspace");
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().document().full_text(), "one\n");
        });
    }

    #[gpui::test]
    fn opening_the_find_bar_clears_content_search_input_focus(cx: &mut gpui::TestAppContext) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("one\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        cx.update(|window, app| {
            view.update(app, |view, cx| view.open_content_search(window, cx));
        });
        cx.run_until_parked();
        assert!(view.read_with(cx, |view, _| view.content_search_input_is_focused()));

        // Opening the document-find bar is a separate shortcut/field/focus
        // flag from the Work-folder content search in the sidebar, so the
        // two can coexist, but only one should ever claim keyboard focus.
        cx.update(|window, app| {
            view.update(app, |view, cx| view.open_document_find(window, cx));
        });
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert!(!view.content_search_input_is_focused());
            assert!(view.document_find_input_is_focused());
        });
    }

    #[gpui::test]
    fn an_empty_query_reports_no_result_without_scanning(cx: &mut gpui::TestAppContext) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("aa aa aa\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        cx.update(|window, app| {
            view.update(app, |view, cx| view.open_document_find(window, cx));
        });
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert_eq!(view.document_find.result, DocumentFindResult::Empty);
            assert!(!view.document_find.job_active);
        });
    }

    #[gpui::test]
    fn a_multi_line_query_is_reported_as_invalid_instead_of_a_zero_count(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("aa\nbb\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        view.update(cx, |view, cx| {
            view.document_find.query_text = "a\nb".to_owned();
            view.restart_document_find(cx, false);
        });
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert_eq!(
                view.document_find.result,
                DocumentFindResult::Invalid(FindQueryError::MultiLine)
            );
        });
    }

    #[gpui::test]
    fn an_oversized_query_is_reported_as_invalid(cx: &mut gpui::TestAppContext) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("aa\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        view.update(cx, |view, cx| {
            view.document_find.query_text = "a".repeat(hane_editor::MAX_QUERY_BYTES + 1);
            view.restart_document_find(cx, false);
        });
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert!(matches!(
                view.document_find.result,
                DocumentFindResult::Invalid(FindQueryError::TooLong { .. })
            ));
        });
    }

    #[gpui::test]
    fn a_valid_query_scans_the_active_in_memory_buffer_and_reports_the_exact_count(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) =
            cx.add_window_view(|_, cx| EditorView::new("aa aa aa\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        // An untitled, never-saved document: the scan must find the match in
        // the live draft buffer, not on disk (there is nothing on disk).
        view.update(cx, |view, cx| {
            view.document_find.query_text = "aa".to_owned();
            view.restart_document_find(cx, false);
        });
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert_eq!(
                view.document_find.result,
                DocumentFindResult::Ready {
                    count: 3,
                    truncated: false,
                }
            );
        });
    }

    #[gpui::test]
    fn case_insensitive_is_the_default_and_the_toggle_rescans(cx: &mut gpui::TestAppContext) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) =
            cx.add_window_view(|_, cx| EditorView::new("Needle needle NEEDLE\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        view.update(cx, |view, cx| {
            assert!(!view.document_find.case_sensitive);
            view.document_find.query_text = "needle".to_owned();
            view.restart_document_find(cx, false);
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(
                view.document_find.result,
                DocumentFindResult::Ready {
                    count: 3,
                    truncated: false,
                }
            );
        });

        view.update(cx, |view, cx| view.toggle_document_find_case(cx));
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(view.document_find.case_sensitive);
            assert_eq!(
                view.document_find.result,
                DocumentFindResult::Ready {
                    count: 1,
                    truncated: false,
                }
            );
        });
    }

    #[gpui::test]
    fn rapid_query_changes_coalesce_into_a_single_scan_for_the_final_text(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) =
            cx.add_window_view(|_, cx| EditorView::new("cat cab car\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        // Three keystrokes in a row, each re-debouncing before the previous
        // one's delay has had any (real or simulated) time to elapse.
        view.update(cx, |view, cx| {
            view.document_find_query_changed("c".to_owned(), cx);
            view.document_find_query_changed("ca".to_owned(), cx);
            view.document_find_query_changed("car".to_owned(), cx);
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert!(!view.document_find.job_active);
            assert_eq!(view.document_find.result, DocumentFindResult::Pending);
        });

        // The debounce timer, like the other wall-clock debounces in this
        // view (e.g. `settle_debounce` in `view.rs`), runs on GPUI's real
        // timer rather than the deterministic test dispatcher, so it needs
        // `advance_clock` rather than just `run_until_parked` to fire.
        //
        // Firing the timer is not enough on its own, though:
        // `schedule_document_find_debounce`'s post-wake check re-reads
        // `last_query_change.elapsed()` against `std::time::Instant::now()`,
        // which `advance_clock` (a virtual clock private to the test
        // dispatcher) never moves. Without real wall-clock time also having
        // passed, that check still reads as "not enough time yet", so the
        // loop just re-arms another `DOCUMENT_FIND_DEBOUNCE`-long virtual
        // timer — one `advance_clock` call past the one above's reach — and
        // the scan never runs. A real sleep for at least the debounce
        // window, taken before the virtual clock is advanced, makes that
        // `elapsed()` check see genuine elapsed time by the time the
        // now-due virtual timer wakes the task inside `advance_clock`.
        std::thread::sleep(DOCUMENT_FIND_DEBOUNCE + Duration::from_millis(50));
        cx.executor()
            .advance_clock(DOCUMENT_FIND_DEBOUNCE + Duration::from_millis(50));
        cx.run_until_parked();

        // Only the final, coalesced query ("car") is ever searched.
        view.read_with(cx, |view, _| {
            assert_eq!(
                view.document_find.result,
                DocumentFindResult::Ready {
                    count: 1,
                    truncated: false,
                }
            );
        });
    }

    #[gpui::test]
    fn a_request_that_arrives_while_a_job_is_outstanding_does_not_spawn_a_second_one(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) =
            cx.add_window_view(|_, cx| EditorView::new("apple apricot banana\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        view.update(cx, |view, cx| {
            view.document_find.query_text = "ap".to_owned();
            view.start_document_find_scan(cx);
            assert!(view.document_find.job_active);

            // A second request arrives before the first job's background
            // task has had a chance to run (the executor has not been
            // parked yet). It must only mark a restart as pending, never
            // spawn a second concurrent scan.
            view.document_find.query_text = "banana".to_owned();
            view.start_document_find_scan(cx);
            assert!(view.document_find.job_active);
            assert!(view.document_find.restart_pending);
        });

        cx.run_until_parked();

        // The coalesced, latest request ("banana") is the one whose result
        // is applied, and the job slot is free again afterward.
        view.read_with(cx, |view, _| {
            assert!(!view.document_find.job_active);
            assert!(!view.document_find.restart_pending);
            assert_eq!(
                view.document_find.result,
                DocumentFindResult::Ready {
                    count: 1,
                    truncated: false,
                }
            );
        });
    }

    #[gpui::test]
    fn a_result_for_a_different_session_is_not_applied(cx: &mut gpui::TestAppContext) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("needle\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        view.update(cx, |view, cx| {
            view.document_find.query_text = "needle".to_owned();
            view.restart_document_find(cx, false);
            let real_target = view.document_find_target();
            let other_session = SessionId(real_target.session.0.wrapping_add(1));
            let stale_target = DocumentFindTarget {
                session: other_session,
                generation: real_target.generation,
                revision: real_target.revision,
            };
            let query_generation = view.document_find.query_generation;
            view.finish_document_find_scan(
                stale_target,
                query_generation,
                FindScan::Completed(hane_editor::FindResults {
                    matches: vec![SourceRange::new(0, 6)],
                    truncated: false,
                }),
                cx,
            );
        });

        view.read_with(cx, |view, _| {
            // The forged result for another session must be rejected; the
            // real in-flight scan (still pending) is untouched.
            assert_eq!(view.document_find.result, DocumentFindResult::Pending);
        });
    }

    #[gpui::test]
    fn a_result_for_a_superseded_query_generation_is_not_applied(cx: &mut gpui::TestAppContext) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) = cx.add_window_view(|_, cx| EditorView::new("needle\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        view.update(cx, |view, cx| {
            view.document_find.query_text = "needle".to_owned();
            view.start_document_find_scan(cx);
            assert!(view.document_find.job_active);

            // A newer query supersedes the still-outstanding "needle" job
            // before it has run at all.
            view.document_find.query_text = "other".to_owned();
            view.restart_document_find(cx, false);
            assert!(view.document_find.restart_pending);
        });
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            // The late "needle" result must not win over a correctly
            // computed, current result for "other" (zero occurrences in
            // this document).
            assert_eq!(
                view.document_find.result,
                DocumentFindResult::Ready {
                    count: 0,
                    truncated: false,
                }
            );
        });
    }

    #[gpui::test]
    fn a_result_computed_before_an_edit_is_not_applied_after_the_revision_moved(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) =
            cx.add_window_view(|_, cx| EditorView::new("needle and thread\n", "Untitled", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        cx.update(|window, app| {
            view.update(app, |view, cx| {
                // The auto-rescan under test only applies while the bar is
                // open; opened through the normal `open_document_find` entry
                // point so the find input is initialized the same way the
                // real shortcut initializes it.
                view.open_document_find(window, cx);
                view.document_find.query_text = "needle".to_owned();
                view.restart_document_find(cx, false);
                assert!(view.document_find.job_active);
                assert_eq!(view.document_find.result, DocumentFindResult::Pending);

                // The document itself is edited (through the same view-level
                // path as real typing, which routes through `after_input`)
                // while the scan's background task has not run yet, moving the
                // revision the in-flight scan was started against out from
                // under it.
                view.editor_mut()
                    .set_selection(Selection::caret(SourceOffset(0)))
                    .unwrap();
                view.insert_text("needle ", cx);
            });
        });
        cx.run_until_parked();

        // The now-stale job is rejected rather than overwriting `result`
        // with a count computed against the pre-edit text; the edit itself
        // debounces a fresh scan the same way a keystroke in the query
        // field would, so the bar is still `Pending` right after the edit.
        view.read_with(cx, |view, _| {
            assert!(!view.document_find.job_active);
            assert_eq!(view.document_find.result, DocumentFindResult::Pending);
        });

        // See `rapid_query_changes_coalesce_into_a_single_scan_for_the_final_text`
        // for why both a real sleep and a virtual clock advance are needed
        // to make the debounce fire.
        std::thread::sleep(DOCUMENT_FIND_DEBOUNCE + Duration::from_millis(50));
        cx.executor()
            .advance_clock(DOCUMENT_FIND_DEBOUNCE + Duration::from_millis(50));
        cx.run_until_parked();

        // The bar rescans the current (post-edit) buffer on its own, with
        // no further explicit request, and its count reflects the
        // occurrence the edit introduced.
        view.read_with(cx, |view, _| {
            assert_eq!(
                view.document_find.result,
                DocumentFindResult::Ready {
                    count: 2,
                    truncated: false,
                }
            );
        });

        // Undo moves the revision again while the bar stays open; the
        // count must track it back down, not keep showing the edited
        // document's count.
        view.update(cx, |view, cx| {
            view.dispatch(EditorCommand::Undo, cx);
        });
        std::thread::sleep(DOCUMENT_FIND_DEBOUNCE + Duration::from_millis(50));
        cx.executor()
            .advance_clock(DOCUMENT_FIND_DEBOUNCE + Duration::from_millis(50));
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert_eq!(
                view.document_find.result,
                DocumentFindResult::Ready {
                    count: 1,
                    truncated: false,
                }
            );
        });
    }

    #[gpui::test]
    fn switching_the_active_tab_rescans_for_the_newly_active_document(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(crate::actions::register_key_bindings);
        let (view, cx) =
            cx.add_window_view(|_, cx| EditorView::new("needle needle\n", "First", cx));
        cx.simulate_resize(gpui::size(px(960.0), px(760.0)));
        cx.run_until_parked();

        let (first, second) = view.update(cx, |view, cx| {
            let first = view.sessions.active_id();
            let second = view.sessions.open_untitled("needle\n", "Second");
            view.on_document_replaced();
            assert!(view.sessions.activate(first));
            view.on_document_replaced();
            cx.notify();
            (first, second)
        });
        cx.run_until_parked();

        cx.update(|window, app| {
            view.update(app, |view, cx| {
                assert_eq!(view.sessions.active_id(), first);
                view.open_document_find(window, cx);
                view.document_find.query_text = "needle".to_owned();
                view.restart_document_find(cx, false);
            });
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(
                view.document_find.result,
                DocumentFindResult::Ready {
                    count: 2,
                    truncated: false,
                }
            );
        });

        // Switching to the other open document must not keep showing a
        // count computed against the document that is no longer active.
        view.update(cx, |view, cx| {
            assert!(view.activate_session(second, cx));
        });
        cx.run_until_parked();

        view.read_with(cx, |view, _| {
            assert_eq!(view.sessions.active_id(), second);
            assert_eq!(
                view.document_find.result,
                DocumentFindResult::Ready {
                    count: 1,
                    truncated: false,
                }
            );
        });
    }
}
