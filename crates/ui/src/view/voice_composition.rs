//! Voice composition (P1): recognized speech drawn as unconfirmed segments in
//! the body, committed one segment at a time.
//!
//! The segments live in the editor (`Editor::voice`), never in the document.
//! This file is the view's side: it splices the segments into the presented
//! lines so they lay out, wrap and paint with the surrounding text, lists them
//! in a strip below the body, and routes the keys (`Enter` confirms the shown
//! sentence, `Esc` discards the segment, as with an IME) when a segment has
//! the operation focus. A segment shows one sentence: the raw text, replaced
//! in place by the polished text once that arrives.
//!
//! Nothing here calls [`EditorView::after_input`] for speech that only
//! arrives, is polished or is discarded: that path starts the autosave, draft
//! and title timers, and those must run only for confirmed edits.

use super::*;
use hane_editor::{
    EndStatus, PolishMode, PolishResult, RawStatus, VoiceApply,
    VoiceEnvelope, VoiceError, VoiceEvent,
};
use hane_presentation::{VirtualInsert, splice_virtual_text};
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hash, Hasher};

/// The fixed script the debug command feeds in.
const SCRIPT_LAST_SEQ: u64 = 4;

fn script_events() -> Vec<VoiceEvent> {
    let raw = |seq: u64, text: &str, polish: PolishMode| VoiceEvent::SegmentRaw {
        seq,
        status: RawStatus::Ok {
            text: text.to_owned(),
            polish,
        },
    };
    vec![
        VoiceEvent::SessionStart,
        raw(1, "えーと、今日はいい天気ですね。", PolishMode::Pending),
        raw(2, "つぎのしょうです", PolishMode::Pending),
        VoiceEvent::SegmentPolished {
            seq: 2,
            result: PolishResult::Ok("次の章です。".to_owned()),
        },
        VoiceEvent::SegmentPolished {
            seq: 1,
            result: PolishResult::Ok("今日はいい天気ですね。".to_owned()),
        },
        raw(3, "Hello, voice composition.", PolishMode::Off),
        VoiceEvent::SegmentRaw {
            seq: 4,
            status: RawStatus::Silent,
        },
        VoiceEvent::SessionEnd {
            status: EndStatus::Completed,
            last_seq: SCRIPT_LAST_SEQ,
        },
    ]
}

/// A random 128-bit session id as 32 lowercase hex digits.
fn new_session_id() -> String {
    let state = RandomState::new();
    let part = |salt: u64| {
        let mut hasher = state.build_hasher();
        salt.hash(&mut hasher);
        std::process::id().hash(&mut hasher);
        hasher.finish()
    };
    format!("{:016x}{:016x}", part(1), part(2))
}

struct ScriptRun {
    session_id: String,
    next_event_no: u64,
    cursor: usize,
}

#[derive(Default)]
pub(super) struct VoiceViewState {
    /// The document the session belongs to: session id and generation.
    owner: Option<(SessionId, u64)>,
    /// The segment `Enter` / `Esc` act on. `None` leaves both keys to the body.
    pub(super) focus: Option<u64>,
    /// The voice stamp each cached block was presented with.
    stamps: HashMap<BlockId, u64>,
    script: Option<ScriptRun>,
    /// A short message for the strip, e.g. why an operation did nothing.
    status: Option<String>,
}

impl VoiceViewState {
    /// Forgets the presentation stamps of the blocks of a replaced document.
    pub(super) fn stamps_reset(&mut self) {
        self.stamps.clear();
    }

    pub(super) fn reset(&mut self) {
        self.focus = None;
        self.stamps.clear();
        self.script = None;
        self.status = None;
    }
}

/// What one pending segment looks like to the view.
struct SegmentView {
    seq: u64,
    offset: usize,
    text: String,
    /// The sentence the segment currently shows (without its separator).
    shown: String,
}

impl EditorView {
    pub fn voice_composition_enabled(&self) -> bool {
        self.settings.voice_composition
    }

    /// Turns voice composition on or off. Turning it on switches the whisparrot
    /// link off (the two are chosen explicitly and never run together); turning
    /// it off drops every unconfirmed segment.
    pub(crate) fn set_voice_composition(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.settings.voice_composition == enabled {
            return;
        }
        self.settings.voice_composition = enabled;
        if enabled {
            if self.settings.whisparrot_link {
                self.set_whisparrot_link(false, cx);
            }
        } else {
            self.voice_cancel_everywhere();
        }
        self.store_settings();
        cx.notify();
    }

    /// Drops the session of the document that owns it and clears the view's
    /// share of the state.
    fn voice_cancel_everywhere(&mut self) {
        if let Some((id, _)) = self.voice.owner.take()
            && let Some(session) = self.sessions.get_mut(id)
        {
            session.editor_mut().voice_cancel();
        }
        self.sessions.active_mut().editor_mut().voice_cancel();
        self.voice.reset();
    }

    /// A document replaced the one on screen (tab switch, reload, folder
    /// change): an unconfirmed segment must not follow into another document.
    pub(super) fn voice_on_document_replaced(&mut self) {
        let Some((id, generation)) = self.voice.owner else {
            return;
        };
        let current = (
            self.sessions.active_id(),
            self.sessions.active().generation(),
        );
        if (id, generation) != current {
            self.voice_cancel_everywhere();
        }
    }

    fn voice_sync_active(&mut self) {
        self.sessions.active_mut().editor_mut().voice_sync();
    }

    fn voice_segments(&self) -> Vec<SegmentView> {
        self.sessions
            .active()
            .editor()
            .voice()
            .map(|voice| {
                voice
                    .segments()
                    .iter()
                    .map(|segment| SegmentView {
                        seq: segment.seq,
                        offset: segment.offset(),
                        text: segment.display_text(),
                        shown: segment.shown().to_owned(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Starts a session at the caret of the active document.
    pub fn voice_begin_session(&mut self, cx: &mut Context<Self>) -> Option<String> {
        if !self.settings.voice_composition || !self.body_editable() {
            return None;
        }
        let id = new_session_id();
        match self.sessions.active_mut().editor_mut().voice_begin(id.clone()) {
            Ok(()) => {
                let generation = self.sessions.active().generation();
                self.voice.owner = Some((self.sessions.active_id(), generation));
                self.voice.focus = None;
                self.voice.status = None;
                cx.notify();
                Some(id)
            }
            Err(_) => {
                // A finished session keeps its unconfirmed segments until they
                // are confirmed or discarded; say so and point at the first.
                let finished = self
                    .sessions
                    .active()
                    .editor()
                    .voice()
                    .is_some_and(|voice| !voice.is_recording());
                if finished {
                    self.voice.status = Some(
                        "前の音声入力の未確定分が残っています。確定または破棄してから開始してください"
                            .to_owned(),
                    );
                    let mut ordered = self.voice_segments();
                    ordered.sort_by_key(|segment| (segment.offset, segment.seq));
                    if let Some(first) = ordered.first() {
                        self.voice.focus = Some(first.seq);
                    }
                    self.voice_after_display_change(cx);
                } else {
                    self.voice.status = Some("音声入力はすでに進行中です".to_owned());
                    cx.notify();
                }
                None
            }
        }
    }

    /// The internal receiving end: one engine event for the active document.
    /// Display only; nothing here touches saving.
    pub fn voice_apply_event(
        &mut self,
        envelope: VoiceEnvelope,
        cx: &mut Context<Self>,
    ) -> Result<VoiceApply, VoiceError> {
        let result = self.sessions.active_mut().editor_mut().voice_apply(envelope);
        self.voice_after_display_change(cx);
        result
    }

    /// Repaints after speech arrived, was polished or was dropped. The body
    /// text did not change, so this is not an edit.
    fn voice_after_display_change(&mut self, cx: &mut Context<Self>) {
        if self.sessions.active().editor().voice().is_none() {
            self.voice.focus = None;
            self.voice.stamps.clear();
        } else if let Some(focus) = self.voice.focus
            && !self.voice_segments().iter().any(|s| s.seq == focus)
        {
            self.voice.focus = None;
        }
        self.resync_heights();
        cx.notify();
    }

    fn voice_after_commit(&mut self, next_focus: Option<u64>, cx: &mut Context<Self>) {
        // A commit changed the document: the ordinary after-edit work (save
        // timers, parse, find) runs for it, and clears the focus.
        self.after_input(cx);
        self.voice.focus = next_focus;
        self.voice_after_display_change(cx);
    }

    /// The segment after `seq` in drawing order, or the first.
    fn voice_next_after(&self, seq: Option<u64>) -> Option<u64> {
        let mut ordered = self.voice_segments();
        ordered.sort_by_key(|segment| (segment.offset, segment.seq));
        let position = seq.and_then(|seq| ordered.iter().position(|segment| segment.seq == seq));
        let next = position.map_or(0, |index| index + 1);
        ordered.get(next).map(|segment| segment.seq)
    }

    /// Moves the operation focus to the next segment; past the last one it
    /// returns the keys to the body.
    pub fn voice_focus_next(&mut self, cx: &mut Context<Self>) {
        self.voice_sync_active();
        self.voice.focus = self.voice_next_after(self.voice.focus);
        cx.notify();
    }

    pub fn voice_set_focus(&mut self, seq: u64, cx: &mut Context<Self>) {
        self.voice_sync_active();
        if self.voice_segments().iter().any(|segment| segment.seq == seq) {
            self.voice.focus = Some(seq);
            cx.notify();
        }
    }

    pub fn voice_focused(&self) -> Option<u64> {
        self.voice.focus
    }

    /// Commits the focused segment with the sentence it shows.
    pub fn voice_commit_focused(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(seq) = self.voice.focus else {
            return false;
        };
        self.voice_commit(seq, cx)
    }

    pub fn voice_commit(&mut self, seq: u64, cx: &mut Context<Self>) -> bool {
        if !self.body_editable() {
            return false;
        }
        let next = self.voice_next_after(Some(seq));
        let result = self.sessions.active_mut().editor_mut().voice_commit(seq);
        match result {
            Ok(()) => {
                self.voice_after_commit(next.filter(|next| *next != seq), cx);
                true
            }
            Err(_) => false,
        }
    }

    pub fn voice_discard(&mut self, seq: u64, cx: &mut Context<Self>) {
        let next = self.voice_next_after(Some(seq));
        if self.sessions.active_mut().editor_mut().voice_discard(seq).is_ok() {
            self.voice.focus = next.filter(|next| *next != seq);
            self.voice_after_display_change(cx);
        }
    }

    /// Review: every segment in order, each with the sentence it shows.
    pub fn voice_commit_all(&mut self, cx: &mut Context<Self>) {
        if !self.body_editable() {
            return;
        }
        if self.sessions.active_mut().editor_mut().voice_commit_all().is_ok() {
            self.voice_after_commit(None, cx);
        }
    }

    pub fn voice_discard_all(&mut self, cx: &mut Context<Self>) {
        if self.sessions.active_mut().editor_mut().voice_discard_all().is_ok() {
            self.voice.focus = None;
            self.voice_after_display_change(cx);
        }
    }

    /// `Enter` with a focused segment: confirm the sentence it shows. Returns
    /// whether the key was taken; without a focused segment it belongs to the
    /// body.
    pub(crate) fn voice_handle_enter(&mut self, cx: &mut Context<Self>) -> bool {
        if self.voice.focus.is_none() {
            return false;
        }
        self.voice_commit_focused(cx);
        true
    }

    /// `Esc` with a focused segment: discard it.
    pub(crate) fn voice_handle_escape(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(seq) = self.voice.focus else {
            return false;
        };
        self.voice_discard(seq, cx);
        true
    }

    /// The debug command: sends the next event of the fixed script, starting a
    /// session first when none is running.
    pub fn voice_debug_step(&mut self, cx: &mut Context<Self>) {
        if !self.settings.voice_composition {
            return;
        }
        if self.voice.script.is_none() {
            let Some(session_id) = self.voice_begin_session(cx) else {
                return;
            };
            self.voice.script = Some(ScriptRun {
                session_id,
                next_event_no: 1,
                cursor: 0,
            });
        }
        let events = script_events();
        let Some(script) = self.voice.script.as_mut() else {
            return;
        };
        let Some(event) = events.get(script.cursor).cloned() else {
            self.voice.script = None;
            return;
        };
        let envelope = VoiceEnvelope {
            session_id: script.session_id.clone(),
            event_no: script.next_event_no,
            event,
        };
        script.next_event_no += 1;
        script.cursor += 1;
        if script.cursor >= events.len() {
            self.voice.script = None;
        }
        let _ = self.voice_apply_event(envelope, cx);
    }

    /// The debug command, all remaining steps at once.
    pub fn voice_debug_run(&mut self, cx: &mut Context<Self>) {
        for _ in 0..=script_events().len() {
            self.voice_debug_step(cx);
            if self.voice.script.is_none() {
                break;
            }
        }
    }

    /// The debug command: ends the session as cancelled.
    pub fn voice_debug_cancel(&mut self, cx: &mut Context<Self>) {
        self.voice_cancel_everywhere();
        self.voice_after_display_change(cx);
    }

    /// Hash of every segment that falls in `range`, 0 when none does. A cached
    /// presentation of the block is reusable only while this is unchanged.
    pub(super) fn voice_block_stamp(&mut self, range: SourceRange) -> u64 {
        self.voice_sync_active();
        let doc_len = self.sessions.active().editor().document().len_bytes().0;
        let focus = self.voice.focus;
        let Some(voice) = self.sessions.active().editor().voice() else {
            return 0;
        };
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        let mut any = false;
        for segment in voice.segments() {
            if segment_in_block(segment.offset(), range, doc_len) {
                any = true;
                (
                    segment.seq,
                    segment.offset(),
                    segment.display_text(),
                    focus == Some(segment.seq),
                )
                    .hash(&mut hasher);
            }
        }
        if any { hasher.finish() | 1 } else { 0 }
    }

    pub(super) fn voice_stamp_matches(&self, id: BlockId, current: u64) -> bool {
        self.voice.stamps.get(&id).copied().unwrap_or(0) == current
    }

    pub(super) fn voice_record_stamp(&mut self, id: BlockId, stamp: u64) {
        if stamp == 0 {
            self.voice.stamps.remove(&id);
        } else {
            self.voice.stamps.insert(id, stamp);
        }
    }

    /// Draws the segments that fall in this block into its presented lines.
    pub(super) fn voice_inject(&mut self, range: SourceRange, visual: &mut VisualBlock) {
        let doc_len = self.sessions.active().editor().document().len_bytes().0;
        let focus = self.voice.focus;
        let mut segments: Vec<_> = self
            .voice_segments()
            .into_iter()
            .filter(|segment| segment_in_block(segment.offset, range, doc_len))
            .collect();
        if segments.is_empty() {
            return;
        }
        segments.sort_by_key(|segment| (segment.offset, segment.seq));
        let bounds: Vec<(usize, usize)> = visual
            .lines
            .iter()
            .map(|line| (line.source_range.start.0, line.source_range.end.0))
            .collect();
        for (index, line) in visual.lines.iter_mut().enumerate() {
            let inserts: Vec<VirtualInsert<'_>> = segments
                .iter()
                .filter(|segment| owning_line(&bounds, segment.offset) == Some(index))
                .map(|segment| VirtualInsert {
                    id: segment.seq,
                    source: SourceOffset(segment.offset),
                    text: &segment.text,
                    emphasized: focus == Some(segment.seq),
                })
                .collect();
            if !inserts.is_empty() {
                splice_virtual_text(line, &inserts);
            }
        }
    }
}

/// The line a caret at `offset` is drawn on: the line that starts at or before
/// it and ends after it, else the line that ends exactly there (a last line
/// without a terminator, or a range that leaves the terminator out).
fn owning_line(bounds: &[(usize, usize)], offset: usize) -> Option<usize> {
    bounds
        .iter()
        .position(|&(start, end)| start <= offset && offset < end)
        .or_else(|| bounds.iter().rposition(|&(start, end)| start <= offset && offset == end))
}

fn segment_in_block(offset: usize, range: SourceRange, doc_len: usize) -> bool {
    offset >= range.start.0 && (offset < range.end.0 || (offset == range.end.0 && offset == doc_len))
}

impl EditorView {
    /// The strip below the body: every unconfirmed segment with the sentence
    /// it shows, and the operations. It is the reference display for segments
    /// the body cannot draw inline.
    pub(super) fn voice_strip_element(&mut self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if !self.settings.voice_composition {
            return None;
        }
        self.voice_sync_active();
        let editor = self.sessions.active().editor();
        let notice = editor
            .voice_notice()
            .map(str::to_owned)
            .or_else(|| self.voice.status.clone());
        let (recording, failed) = editor
            .voice()
            .map(|voice| (voice.is_recording(), voice.failed_segments()))
            .unwrap_or_default();
        let mut segments = self.voice_segments();
        let active = self.sessions.active().editor().voice().is_some();
        if !active && notice.is_none() {
            return None;
        }
        segments.sort_by_key(|segment| (segment.offset, segment.seq));
        let theme = self.theme;
        let focus = self.voice.focus;
        let view = cx.entity();

        let state_label = if !active {
            "音声入力"
        } else if recording {
            "音声入力（受信中）"
        } else {
            "音声入力（確認中）"
        };
        let mut header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(div().font_weight(gpui::FontWeight::BOLD).child(state_label))
            .when(failed > 0, |el| {
                el.child(div().child(format!("認識できなかった区間 {failed} 件")))
            })
            .children(notice.map(|notice| div().text_color(rgb(theme.quote_foreground)).child(notice)));
        if !segments.is_empty() {
            let all = view.clone();
            let none = view.clone();
            header = header
                .child(
                    Button::new("voice-commit-all")
                        .label("すべて確定")
                        .small()
                        .ghost()
                        .on_click(move |_, _window, app| {
                            all.update(app, |view, cx| view.voice_commit_all(cx));
                        }),
                )
                .child(
                    Button::new("voice-discard-all")
                        .label("すべて破棄")
                        .small()
                        .ghost()
                        .on_click(move |_, _window, app| {
                            none.update(app, |view, cx| view.voice_discard_all(cx));
                        }),
                );
        }

        let rows = segments.iter().take(8).map(|segment| {
            let seq = segment.seq;
            let focused = focus == Some(seq);
            let label = format!("#{seq}  {}", segment.shown);
            let pick = view.clone();
            let chip = div()
                .id(("voice-segment", seq))
                .debug_selector(move || format!("voice-segment-{seq}"))
                .px_2()
                .rounded_sm()
                .cursor_pointer()
                .when(focused, |el| el.bg(rgb(theme.selection_background)))
                .child(label)
                .on_click(move |_, _window, app| {
                    pick.update(app, |view, cx| view.voice_set_focus(seq, cx));
                });
            let confirm = view.clone();
            let drop = view.clone();
            let row = div()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(chip)
                        .child(
                            Button::new(("voice-confirm", seq))
                                .label("確定")
                                .small()
                                .ghost()
                                .on_click(move |_, _window, app| {
                                    confirm.update(app, |view, cx| {
                                        view.voice_commit(seq, cx);
                                    });
                                }),
                        )
                        .child(
                            Button::new(("voice-drop", seq))
                                .label("区間を破棄")
                                .small()
                                .ghost()
                                .on_click(move |_, _window, app| {
                                    drop.update(app, |view, cx| view.voice_discard(seq, cx));
                                }),
                        ),
                );
            row
        });
        Some(
            div()
                .id("voice-strip")
                .debug_selector(|| "voice-strip".to_owned())
                .flex()
                .flex_col()
                .gap_1()
                .px_3()
                .py_1()
                .border_t_1()
                .border_color(rgb(theme.tab_bar_border))
                .bg(rgb(theme.header_background))
                .text_color(rgb(theme.header_foreground))
                .child(header)
                .children(rows)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::EntityInputHandler;
    use hane_session::DraftId;

    type TestView = gpui::Entity<EditorView>;
    type TestCx = gpui::VisualTestContext;

    const SESSION: &str = "0123456789abcdef0123456789abcdef";

    fn settle(cx: &mut TestCx) {
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
    }

    fn open_view(
        cx: &mut gpui::TestAppContext,
        sessions: SessionSet,
        enabled: bool,
    ) -> (TestView, &mut TestCx) {
        let (view, cx) = cx.add_window_view(move |_, cx| {
            let mut view = EditorView::from_sessions(
                sessions,
                Arc::new(OsFileService),
                StateStores::memory(),
                cx,
            );
            view.settings.voice_composition = enabled;
            view
        });
        cx.simulate_resize(gpui::size(px(800.0), px(600.0)));
        settle(cx);
        (view, cx)
    }

    fn untitled<'a>(
        cx: &'a mut gpui::TestAppContext,
        source: &str,
        caret: usize,
    ) -> (TestView, &'a mut TestCx) {
        let (view, cx) = open_view(cx, SessionSet::with_untitled(source, "Untitled"), true);
        view.update(cx, |view, _| {
            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset(caret)))
                .unwrap();
            view.editor_mut().voice_begin(SESSION).unwrap();
        });
        settle(cx);
        (view, cx)
    }

    struct Feed(u64);

    impl Feed {
        fn send(&mut self, view: &TestView, cx: &mut TestCx, event: VoiceEvent) {
            let envelope = VoiceEnvelope {
                session_id: SESSION.to_owned(),
                event_no: self.0,
                event,
            };
            self.0 += 1;
            view.update(cx, |view, cx| {
                view.voice_apply_event(envelope, cx).unwrap();
            });
            settle(cx);
        }

        fn start(&mut self, view: &TestView, cx: &mut TestCx) {
            self.send(view, cx, VoiceEvent::SessionStart);
        }

        fn raw(&mut self, view: &TestView, cx: &mut TestCx, seq: u64, text: &str) {
            self.send(
                view,
                cx,
                VoiceEvent::SegmentRaw {
                    seq,
                    status: RawStatus::Ok {
                        text: text.to_owned(),
                        polish: PolishMode::Pending,
                    },
                },
            );
        }

        fn polished(&mut self, view: &TestView, cx: &mut TestCx, seq: u64, text: &str) {
            self.send(
                view,
                cx,
                VoiceEvent::SegmentPolished {
                    seq,
                    result: PolishResult::Ok(text.to_owned()),
                },
            );
        }
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

    /// The presented text of every cached block line, as the renderer reads it.
    fn visual_lines(view: &TestView, cx: &mut TestCx) -> Vec<String> {
        view.read_with(cx, |view, _| {
            let mut blocks: Vec<_> = view.block_cache.values().collect();
            blocks.sort_by_key(|block| block.source_range.start);
            blocks
                .iter()
                .flat_map(|block| block.lines.iter().map(|line| line.visual_text.clone()))
                .collect()
        })
    }

    fn replace_in(view: &TestView, cx: &mut TestCx, range: Option<std::ops::Range<usize>>, text: &str) {
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.replace_text_in_range(range, text, window, cx);
            });
        });
        settle(cx);
    }

    fn select(view: &TestView, cx: &mut TestCx, anchor: usize, active: usize) {
        view.update(cx, |view, _| {
            view.editor_mut()
                .set_selection(Selection {
                    anchor: SourceOffset(anchor),
                    active: SourceOffset(active),
                })
                .unwrap();
        });
    }

    fn undo(view: &TestView, cx: &mut TestCx) {
        view.update(cx, |view, cx| view.dispatch(EditorCommand::Undo, cx));
        settle(cx);
    }

    fn redo(view: &TestView, cx: &mut TestCx) {
        view.update(cx, |view, cx| view.dispatch(EditorCommand::Redo, cx));
        settle(cx);
    }

    fn pending_seqs(view: &TestView, cx: &mut TestCx) -> Vec<u64> {
        view.read_with(cx, |view, _| {
            view.voice_segments().iter().map(|segment| segment.seq).collect()
        })
    }

    #[gpui::test]
    fn speech_is_drawn_inside_the_line_and_stays_out_of_the_document(cx: &mut gpui::TestAppContext) {
        let (view, cx) = untitled(cx, "前後の文", "前".len());
        let before = doc_state(&view, cx);
        let mut feed = Feed(1);
        feed.start(&view, cx);
        feed.raw(&view, cx, 1, "音声");
        assert_eq!(visual_lines(&view, cx), ["前音声後の文"]);
        // Underlined like a composition: a marked-text run covers the piece.
        view.read_with(cx, |view, _| {
            let line = &view.block_cache.values().next().unwrap().lines[0];
            assert!(line.style_runs.iter().any(|run| run.kind == StyleKind::MarkedText
                && run.visual_range == hane_presentation::VisualRange::new(3, 9)));
        });
        assert_eq!(doc_state(&view, cx), before);
        assert!(!before.2, "a document with only speech on it is not dirty");
    }

    #[gpui::test]
    fn the_polished_text_replaces_the_shown_raw_text_in_place(cx: &mut gpui::TestAppContext) {
        let (view, cx) = untitled(cx, "", 0);
        let mut feed = Feed(1);
        feed.start(&view, cx);
        feed.raw(&view, cx, 1, "つぎのしょうです");
        // Even an empty document shows the segment in its (only) line, and the
        // strip lists it.
        assert_eq!(visual_lines(&view, cx), ["つぎのしょうです"]);
        assert!(cx.debug_bounds("voice-strip").is_some());
        feed.polished(&view, cx, 1, "次の章です。");
        view.read_with(cx, |view, _| {
            assert_eq!(view.voice_segments()[0].text, "次の章です。");
            assert_eq!(view.voice_segments()[0].shown, "次の章です。");
        });
        // One sentence only: the raw text is gone from the line.
        assert_eq!(visual_lines(&view, cx), ["次の章です。"]);
        assert_eq!(text_of(&view, cx), "");
    }

    #[gpui::test]
    fn the_caret_and_text_after_the_segment_follow_the_wider_line(cx: &mut gpui::TestAppContext) {
        let (view, cx) = untitled(cx, "あいうえお", "あい".len());
        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset("あいうえ".len())))
                .unwrap();
            cx.notify();
        });
        settle(cx);
        let plain_x = view.read_with(cx, |view, _| view.caret_geometry().unwrap().x);
        let mut feed = Feed(1);
        feed.start(&view, cx);
        feed.raw(&view, cx, 1, "音声");
        let shifted_x = view.read_with(cx, |view, _| view.caret_geometry().unwrap().x);
        assert!(
            shifted_x > plain_x + 10.0,
            "the caret after the segment moves right: {plain_x} -> {shifted_x}"
        );
        // A caret in front of the segment does not move.
        view.update(cx, |view, cx| {
            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset("あい".len())))
                .unwrap();
            cx.notify();
        });
        settle(cx);
        let front_x = view.read_with(cx, |view, _| view.caret_geometry().unwrap().x);
        assert!(front_x < plain_x, "{front_x} < {plain_x}");
    }

    #[gpui::test]
    fn two_segments_commit_out_of_order_with_one_undo_each(cx: &mut gpui::TestAppContext) {
        let (view, cx) = untitled(cx, "", 0);
        let mut feed = Feed(1);
        feed.start(&view, cx);
        feed.raw(&view, cx, 1, "一つ目");
        feed.raw(&view, cx, 2, "二つ目");
        feed.polished(&view, cx, 2, "二つ目。");
        feed.polished(&view, cx, 1, "一つ目。");
        // Focus the second segment, then Enter confirms the sentence it shows.
        view.update(cx, |view, cx| {
            view.voice_set_focus(2, cx);
            assert!(view.voice_handle_enter(cx));
        });
        settle(cx);
        assert_eq!(text_of(&view, cx), "二つ目。");
        assert_eq!(pending_seqs(&view, cx), [1]);
        feed.raw(&view, cx, 3, "三つ目");
        feed.polished(&view, cx, 3, "三つ目。");
        // Esc on the first one discards it; nothing is written.
        view.update(cx, |view, cx| {
            view.voice_set_focus(1, cx);
            assert!(view.voice_handle_escape(cx));
        });
        settle(cx);
        assert_eq!(text_of(&view, cx), "二つ目。");
        assert_eq!(pending_seqs(&view, cx), [3]);
        view.update(cx, |view, cx| {
            view.voice_set_focus(3, cx);
            assert!(view.voice_handle_enter(cx));
        });
        settle(cx);
        assert_eq!(text_of(&view, cx), "二つ目。三つ目。");
        assert!(view.read_with(cx, |view, _| view.sessions.active().is_dirty()));
        undo(&view, cx);
        assert_eq!(text_of(&view, cx), "二つ目。");
        undo(&view, cx);
        assert_eq!(text_of(&view, cx), "");
        // Unconfirmed text did not come back with the Undo.
        assert!(pending_seqs(&view, cx).is_empty());
        redo(&view, cx);
        redo(&view, cx);
        assert_eq!(text_of(&view, cx), "二つ目。三つ目。");
        assert!(pending_seqs(&view, cx).is_empty());
    }

    #[gpui::test]
    fn a_polished_text_after_confirmation_is_dropped(cx: &mut gpui::TestAppContext) {
        let (view, cx) = untitled(cx, "", 0);
        let mut feed = Feed(1);
        feed.start(&view, cx);
        feed.raw(&view, cx, 1, "あ");
        view.update(cx, |view, cx| {
            view.voice_set_focus(1, cx);
            assert!(view.voice_handle_enter(cx));
        });
        feed.polished(&view, cx, 1, "亜");
        assert_eq!(text_of(&view, cx), "あ");
        assert!(pending_seqs(&view, cx).is_empty());
    }

    #[gpui::test]
    fn enter_and_escape_belong_to_the_body_unless_a_segment_has_the_focus(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = untitled(cx, "本文", "本文".len());
        let mut feed = Feed(1);
        feed.start(&view, cx);
        feed.raw(&view, cx, 1, "音声");
        view.update(cx, |view, cx| {
            assert!(!view.voice_handle_enter(cx));
            assert!(!view.voice_handle_escape(cx));
        });
        assert_eq!(text_of(&view, cx), "本文");
        // Typing hands the keys back to the body again.
        view.update(cx, |view, cx| view.voice_set_focus(1, cx));
        replace_in(&view, cx, None, "X");
        view.read_with(cx, |view, _| assert_eq!(view.voice_focused(), None));
        assert_eq!(pending_seqs(&view, cx), [1]);
    }

    #[gpui::test]
    fn editing_inside_a_segment_confirms_the_shown_text_then_applies_the_edit(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = untitled(cx, "前😀\r\n後", "前".len());
        let mut feed = Feed(1);
        feed.start(&view, cx);
        feed.raw(&view, cx, 1, "音声");
        feed.polished(&view, cx, 1, "整文");
        // Replace a selection that spans the segment's position.
        select(&view, cx, 0, "前😀".len());
        replace_in(&view, cx, None, "Z");
        assert_eq!(text_of(&view, cx), "Z\r\n後");
        assert!(pending_seqs(&view, cx).is_empty());
        undo(&view, cx);
        assert_eq!(text_of(&view, cx), "前整文😀\r\n後");
        undo(&view, cx);
        assert_eq!(text_of(&view, cx), "前😀\r\n後");
    }

    #[gpui::test]
    fn an_edit_outside_the_segment_leaves_it_and_follows_it(cx: &mut gpui::TestAppContext) {
        let (view, cx) = untitled(cx, "頭と尻", "頭と".len());
        let mut feed = Feed(1);
        feed.start(&view, cx);
        feed.raw(&view, cx, 1, "音声");
        select(&view, cx, 0, "頭".len());
        replace_in(&view, cx, None, "🙂🙂");
        assert_eq!(text_of(&view, cx), "🙂🙂と尻");
        assert_eq!(pending_seqs(&view, cx), [1]);
        assert_eq!(visual_lines(&view, cx), ["🙂🙂と音声尻"]);
        undo(&view, cx);
        assert_eq!(text_of(&view, cx), "頭と尻");
        assert_eq!(visual_lines(&view, cx), ["頭と音声尻"]);
    }

    #[gpui::test]
    fn speech_that_arrives_during_a_japanese_composition_waits_for_its_end(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = untitled(cx, "abc", "abc".len());
        let mut feed = Feed(1);
        feed.start(&view, cx);
        feed.raw(&view, cx, 1, "音声");
        // A composition in the same line, before the segment.
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                view.replace_and_mark_text_in_range(Some(0..1), "か", None, window, cx);
            });
        });
        settle(cx);
        feed.raw(&view, cx, 2, "続き");
        assert_eq!(pending_seqs(&view, cx), [1], "the new segment is held back");
        view.read_with(cx, |view, _| {
            assert_eq!(view.editor().ime().unwrap().marked_text, "か");
            assert_eq!(view.editor().voice().unwrap().held_count(), 1);
        });
        // A commit during the composition waits as well.
        view.update(cx, |view, cx| {
            assert!(view.voice_commit(1, cx));
        });
        assert_eq!(text_of(&view, cx), "かbc");
        cx.update(|window, app| {
            view.update(app, |view, cx| view.unmark_text(window, cx));
        });
        settle(cx);
        assert_eq!(text_of(&view, cx), "かbc音声");
        assert_eq!(pending_seqs(&view, cx), [2]);
    }

    #[gpui::test]
    fn a_composition_that_starts_on_a_segment_confirms_it_first(cx: &mut gpui::TestAppContext) {
        let (view, cx) = untitled(cx, "abcd", "ab".len());
        let mut feed = Feed(1);
        feed.start(&view, cx);
        feed.raw(&view, cx, 1, "音声");
        cx.update(|window, app| {
            view.update(app, |view, cx| {
                // The OS range covers "bc", across the segment's position.
                view.replace_and_mark_text_in_range(Some(1..3), "か", None, window, cx);
            });
        });
        settle(cx);
        assert!(pending_seqs(&view, cx).is_empty());
        assert_eq!(text_of(&view, cx), "aかd");
    }

    #[gpui::test]
    fn only_confirmed_text_reaches_the_file_on_autosave_and_manual_save(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = std::env::temp_dir().join(format!("hane-voice-save-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("note.md");
        std::fs::write(&path, "確定A\n").unwrap();
        let loaded = OsFileService.load(&path).unwrap();
        let (view, cx) = open_view(cx, SessionSet::with_loaded(loaded), true);
        view.update(cx, |view, _| {
            view.editor_mut()
                .set_selection(Selection::caret(SourceOffset("確定A".len())))
                .unwrap();
            view.editor_mut().voice_begin(SESSION).unwrap();
        });
        let mut feed = Feed(1);
        feed.start(&view, cx);
        feed.raw(&view, cx, 1, "未確定の音声");
        feed.polished(&view, cx, 1, "未確定の整文");
        // A confirmed edit elsewhere (C) is what autosave writes.
        select(&view, cx, "確定A\n".len(), "確定A\n".len());
        replace_in(&view, cx, None, "手編集C");
        cx.executor().advance_clock(Duration::from_millis(1_500));
        cx.run_until_parked();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert_eq!(saved, "確定A\n手編集C");
        assert!(!saved.contains("未確定"));
        // The editor still holds the unconfirmed segment.
        assert_eq!(pending_seqs(&view, cx), [1]);
        // Confirm B, then a manual save writes it.
        view.update(cx, |view, cx| {
            assert!(view.voice_commit(1, cx));
            view.save_or_prompt(cx);
        });
        cx.run_until_parked();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("未確定の整文"));
        assert!(!saved.contains("未確定の音声"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[gpui::test]
    fn drafts_hold_only_confirmed_text(cx: &mut gpui::TestAppContext) {
        let root = std::env::temp_dir().join(format!("hane-voice-draft-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let work_folder = hane_session::OsWorkFolderScanner.scan(&root).unwrap();
        let (view, cx) = untitled(cx, "確定B", "確定B".len());
        let draft_id = DraftId::generate();
        view.update(cx, |view, _| {
            view.work_folder = Some(work_folder);
            let id = view.sessions.active_id();
            view.work_folder_drafts.insert(
                id,
                WorkFolderDraft {
                    draft_id,
                    target_directory: root.clone(),
                },
            );
        });
        let mut feed = Feed(1);
        feed.start(&view, cx);
        feed.raw(&view, cx, 1, "未確定の音声");
        view.update(cx, |view, _| view.flush_pending_drafts());
        let recovered = view.read_with(cx, |view, _| view.draft_store.recover(&root).unwrap());
        assert_eq!(recovered.drafts.len(), 1);
        assert_eq!(recovered.drafts[0].text, "確定B");
        // The update-restart flush reads the same text.
        view.read_with(cx, |view, _| {
            assert_eq!(view.flush_pending_drafts_checked(), Ok(1));
        });
        let recovered = view.read_with(cx, |view, _| view.draft_store.recover(&root).unwrap());
        assert_eq!(recovered.drafts[0].text, "確定B");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[gpui::test]
    fn arriving_discarding_and_polishing_start_no_save_timers_and_no_edit(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = untitled(cx, "確定", "確定".len());
        let before = doc_state(&view, cx);
        let mut feed = Feed(1);
        feed.start(&view, cx);
        feed.raw(&view, cx, 1, "あ");
        feed.raw(&view, cx, 2, "い");
        feed.polished(&view, cx, 1, "亜");
        view.update(cx, |view, cx| view.voice_discard(1, cx));
        view.update(cx, |view, cx| view.voice_discard_all(cx));
        settle(cx);
        assert_eq!(doc_state(&view, cx), before);
        assert!(!view.read_with(cx, |view, _| view.sessions.active().is_dirty()));
    }

    #[gpui::test]
    fn the_debug_script_walks_through_raw_polish_and_review(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, SessionSet::with_untitled("", "Untitled"), true);
        for _ in 0..3 {
            view.update(cx, |view, cx| view.voice_debug_step(cx));
        }
        settle(cx);
        assert_eq!(pending_seqs(&view, cx), [1, 2]);
        view.update(cx, |view, cx| view.voice_debug_run(cx));
        settle(cx);
        assert_eq!(pending_seqs(&view, cx), [1, 2, 3]);
        view.read_with(cx, |view, _| {
            assert!(!view.editor().voice().unwrap().is_recording());
        });
        view.update(cx, |view, cx| view.voice_commit_all(cx));
        settle(cx);
        assert_eq!(
            text_of(&view, cx),
            "今日はいい天気ですね。次の章です。Hello, voice composition."
        );
        view.read_with(cx, |view, _| assert!(view.editor().voice().is_none()));
        undo(&view, cx);
        assert_eq!(text_of(&view, cx), "今日はいい天気ですね。次の章です。");
    }

    #[gpui::test]
    fn a_new_run_is_refused_with_the_reason_and_focus_while_leftovers_remain(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = open_view(cx, SessionSet::with_untitled("", "Untitled"), true);
        view.update(cx, |view, cx| view.voice_debug_run(cx));
        settle(cx);
        assert_eq!(pending_seqs(&view, cx), [1, 2, 3]);
        view.update(cx, |view, cx| view.voice_debug_step(cx));
        settle(cx);
        view.read_with(cx, |view, _| {
            assert_eq!(view.voice_focused(), Some(1));
            assert_eq!(
                view.voice.status.as_deref(),
                Some("前の音声入力の未確定分が残っています。確定または破棄してから開始してください")
            );
        });
        assert_eq!(pending_seqs(&view, cx), [1, 2, 3]);
        // Once cleared, the next run starts.
        view.update(cx, |view, cx| view.voice_discard_all(cx));
        settle(cx);
        view.update(cx, |view, cx| view.voice_debug_step(cx));
        view.read_with(cx, |view, _| {
            assert!(view.editor().voice().unwrap().is_recording());
        });
    }

    #[gpui::test]
    fn a_recording_session_keeps_the_in_progress_message(cx: &mut gpui::TestAppContext) {
        let (view, cx) = open_view(cx, SessionSet::with_untitled("", "Untitled"), true);
        view.update(cx, |view, cx| view.voice_debug_step(cx));
        view.update(cx, |view, cx| {
            assert!(view.voice_begin_session(cx).is_none());
        });
        view.read_with(cx, |view, _| {
            assert_eq!(view.voice.status.as_deref(), Some("音声入力はすでに進行中です"));
            assert_eq!(view.voice_focused(), None);
        });
    }

    #[gpui::test]
    fn the_setting_is_off_by_default_and_turning_it_on_switches_the_link_off(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = open_view(cx, SessionSet::with_untitled("", "Untitled"), false);
        view.update(cx, |view, cx| {
            // Off: the debug command does nothing.
            view.voice_debug_step(cx);
            assert!(view.editor().voice().is_none());
            view.settings.whisparrot_link = true;
            view.set_voice_composition(true, cx);
            assert!(view.voice_composition_enabled());
            assert!(!view.settings.whisparrot_link);
            view.voice_debug_step(cx);
            assert!(view.editor().voice().is_some());
            view.set_voice_composition(false, cx);
            assert!(view.editor().voice().is_none());
        });
    }

    #[gpui::test]
    fn switching_documents_cancels_the_unconfirmed_segments(cx: &mut gpui::TestAppContext) {
        let (view, cx) = untitled(cx, "一枚目", "一枚目".len());
        let mut feed = Feed(1);
        feed.start(&view, cx);
        feed.raw(&view, cx, 1, "音声");
        view.update(cx, |view, cx| {
            view.voice.owner = Some((view.sessions.active_id(), view.sessions.active().generation()));
            view.sessions.open_untitled("二枚目", "Untitled");
            view.on_document_replaced();
            cx.notify();
        });
        settle(cx);
        view.update(cx, |view, _| {
            assert!(view.editor().voice().is_none());
        });
    }
}
